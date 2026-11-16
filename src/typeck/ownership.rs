//! Moves and borrows: a moved variable cannot be used again, and one call cannot
//! borrow a value mutably while using it another way.

use super::Checker;
use crate::ast::{Expr, ExprKind};
use crate::diag::{Diagnostic, Result, Span};

/// How one call argument uses its variable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Read,
    Borrow,
    BorrowMut,
    Move,
}

impl Checker<'_> {
    /// Records that a checked expression is used by value: non-copy places are moved.
    pub(super) fn consume(&mut self, expr: &mut Expr) -> Result<()> {
        if expr.ty().is_copy() {
            return Ok(());
        }
        let Some(place) = self.place_of(expr) else { return Ok(()) };
        if place.var == super::place::GLOBAL {
            return Err(Diagnostic::error(
                expr.span,
                format!("`{}` is a module constant: it cannot be moved", place.text),
            )
            .with_hint("lend it with `&`"));
        }
        if self.lent(place.var).is_some() {
            return Err(Diagnostic::error(
                expr.span,
                format!("`{}` is lent to a thread until the end of the `scope`", place.text),
            )
            .with_hint("move it before the threads start, or after the `with` block"));
        }
        // `managed` copies where `manual` would refuse the move, or where the value is used again.
        let again = self.copies.contains(&(expr.span.line, expr.span.col));
        let refused = !place.whole || (place.depth < self.loop_depth && !self.returning);
        if self.managed && (again || refused) && !self.tables.types.has_drop_impl(expr.ty()) {
            let why = if again {
                "it is used again later"
            } else if place.whole {
                "it is moved in a loop"
            } else {
                "it is part of another value"
            };
            // A `shared` value is not copied: one more owner holds it.
            if !matches!(expr.ty(), crate::types::Type::Shared(_)) {
                self.warnings
                    .push(Diagnostic::note(expr.span, format!("`{}` is copied here: {why}", place.text)));
                if again {
                    self.copied.insert(place.var, expr.span);
                }
            }
            copy(expr);
            return Ok(());
        }
        if !place.whole {
            return Err(Diagnostic::error(
                expr.span,
                format!("cannot move `{}` out of its owner", place.text),
            )
            .with_hint(format!("copy it with `{}.copy()`", place.text)));
        }
        let name = &place.text;
        let continues = self.leaves_to.unwrap_or(self.loop_depth);
        if place.depth < self.loop_depth.min(continues) && !self.returning && self.refilled != Some(place.var)
        {
            return Err(Diagnostic::error(
                expr.span,
                format!("`{name}` would be moved again on the next iteration"),
            )
            .with_hint(format!("move a copy with `{name}.copy()`")));
        }
        self.moved.insert(place.var, expr.span);
        self.probe.note(place.var, super::managed::Mode::Owned, Some(expr.span));
        if self.tables.types.needs_drop(expr.ty()) {
            let ty = expr.ty.clone();
            let value = std::mem::replace(expr, Expr::new(ExprKind::None, expr.span));
            *expr = Expr {
                kind: ExprKind::Move { value: Box::new(value), root: place.text },
                span: expr.span,
                ty,
            };
        }
        Ok(())
    }

    /// Fails if a variable read here was moved before.
    pub(super) fn check_not_moved(&mut self, id: usize, name: &str, span: Span) -> Result<()> {
        let Some(&moved) = self.moved.get(&id) else { return Ok(()) };
        let droppable = self.find_local(name).is_some_and(|v| self.tables.types.has_drop_impl(&v.ty));
        if self.managed && !droppable {
            // Copy at the move instead, and check the function again.
            self.copies.insert((moved.line, moved.col));
            self.recheck = true;
            return Ok(());
        }
        if let Some((callee, kept)) = self.moved_into.get(&id) {
            return Err(Diagnostic::error(span, format!("`{name}` was moved away on line {}", moved.line))
                .with_hint(format!(
                    "`{callee}` takes ownership of it: it keeps the value on line {} of its module",
                    kept.line
                )));
        }
        if self.managed {
            return Err(Diagnostic::error(span, format!("`{name}` was moved away on line {}", moved.line))
                .with_hint("it implements `Drop`, so it is never copied; use it before the move"));
        }
        Err(Diagnostic::error(span, format!("`{name}` was moved away on line {}", moved.line))
            .with_hint(format!("pass a copy there with `{name}.copy()`, or borrow it with `&{name}`")))
    }

    /// Within one call, a value borrowed mutably cannot be used by any other argument,
    /// and a moved value cannot also be borrowed.
    pub(super) fn check_call_conflicts(&self, args: &[Expr]) -> Result<()> {
        let mut uses: Vec<(usize, Use, &Expr)> = Vec::new();
        for arg in args {
            if let ExprKind::Lambda { captures, .. } = &arg.kind {
                for capture in captures {
                    let Some(var) = self.find_local(&capture.name) else { continue };
                    let kind = if capture.mutable { Use::BorrowMut } else { Use::Borrow };
                    if let Some(&(_, other, _)) = uses.iter().find(|(v, _, _)| *v == var.id)
                        && conflicts(kind, other)
                    {
                        return Err(Diagnostic::error(
                            capture.span,
                            format!("`{}` is already used by another argument", capture.name),
                        )
                        .with_hint(
                            "a value borrowed with `&mut` cannot be used elsewhere in the same call",
                        ));
                    }
                    uses.push((var.id, kind, arg));
                }
                continue;
            }
            let (inner, kind) = match &arg.kind {
                ExprKind::AddrOf { mutable, inner } => {
                    (inner.as_ref(), if *mutable { Use::BorrowMut } else { Use::Borrow })
                }
                _ if arg.ty().is_copy() => (arg, Use::Read),
                _ => (arg, Use::Move),
            };
            let Some(place) = self.place_of(inner) else { continue };
            for &(var, other, _) in &uses {
                if var == place.var && conflicts(kind, other) {
                    return Err(Diagnostic::error(
                        arg.span,
                        format!("`{}` is already used by another argument", place.text),
                    )
                    .with_hint("a value borrowed with `&mut` cannot be used elsewhere in the same call"));
                }
            }
            uses.push((place.var, kind, arg));
        }
        Ok(())
    }
}

fn conflicts(a: Use, b: Use) -> bool {
    matches!(
        (a, b),
        (Use::BorrowMut, _) | (_, Use::BorrowMut) | (Use::Move, Use::Borrow) | (Use::Borrow, Use::Move)
    )
}

/// Replaces a value by a copy of it.
fn copy(expr: &mut Expr) {
    let span = expr.span;
    let ty = expr.ty.clone();
    let value = std::mem::replace(expr, Expr::new(ExprKind::None, span));
    *expr = Expr { kind: ExprKind::CopyOf(Box::new(value)), span, ty };
}
