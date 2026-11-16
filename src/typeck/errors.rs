//! Errors: `raises` in signatures, `raise`, `check` on fallible calls, and `try` / `except`.
//!
//! Each error type raised in a function must be caught by an enclosing `try` or declared
//! in the function's `raises`.

use std::collections::HashSet;

use super::Checker;
use super::resolve::Symbol;
use super::stmt::always_returns;
use crate::ast::{Catch, Expr, ExprKind, Handler, Pattern, Stmt};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::Type;

impl Checker<'_> {
    /// `check f(...)`: the call must be to a function that can raise.
    pub(super) fn check_check(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Check { call, errors } = &mut expr.kind else { unreachable!("called on a check") };
        self.checking = Some(&raw const **call);
        let result = self.check_expr(call, None);
        self.checking = None;
        let ty = result?;
        let raised = self.checked_raises.take().ok_or_else(|| {
            Diagnostic::error(span, "`check` applies to a call of a function that can raise")
        })?;
        self.reach(&raised, span)?;
        *errors = raised;
        Ok(ty)
    }

    /// A `managed` call to a fallible function, checked: it becomes `check f(...)`.
    pub(super) fn implicit_check(&mut self, expr: &mut Expr, ty: Type) -> Result<Type> {
        let key = std::ptr::from_ref::<Expr>(expr);
        let Some(at) = self.unchecked.iter().position(|(e, _)| *e == key) else { return Ok(ty) };
        let (_, errors) = self.unchecked.remove(at);
        let span = expr.span;
        let call =
            Expr { kind: std::mem::replace(&mut expr.kind, ExprKind::None), span, ty: Some(ty.clone()) };
        self.reach(&errors, span)?;
        expr.kind = ExprKind::Check { call: Box::new(call), errors };
        Ok(ty)
    }

    /// Called for every call: a fallible callee needs `check`, and only it.
    pub(super) fn note_call(&mut self, expr: &Expr, callee: &str, raises: &[Type]) -> Result<()> {
        let checked = self.checking == Some(std::ptr::from_ref::<Expr>(expr));
        if checked {
            self.checking = None;
        }
        match (checked, raises.is_empty()) {
            // `managed` checks for the programmer.
            (false, false) if self.managed => {
                self.unchecked.push((std::ptr::from_ref::<Expr>(expr), raises.to_vec()));
                Ok(())
            }
            (false, false) => {
                Err(Diagnostic::error(expr.span, format!("`{callee}` can raise; mark the call with `check`"))
                    .with_hint(format!("write `check {callee}(...)`")))
            }
            (true, true) => {
                Err(Diagnostic::error(expr.span, format!("`{callee}` cannot raise; remove `check`")))
            }
            (true, false) => {
                self.checked_raises = Some(raises.to_vec());
                Ok(())
            }
            (false, true) => Ok(()),
        }
    }

    /// Errors raised at `span` go to the innermost `try`, or must be declared by the function.
    pub(super) fn reach(&mut self, errors: &[Type], span: Span) -> Result<()> {
        if let Some(caught) = self.tries.last_mut() {
            for error in errors {
                if !caught.contains(error) {
                    caught.push(error.clone());
                }
            }
            return Ok(());
        }
        if self.infer_raises && !self.in_parallel() {
            for error in errors {
                if !self.raises.contains(error) {
                    self.raises.push(error.clone());
                }
            }
            return Ok(());
        }
        match errors.iter().find(|e| !self.raises.contains(e)) {
            Some(error) if self.in_parallel() => Err(Diagnostic::error(
                span,
                format!("`{error}` cannot leave a `parallel for`: its items run apart"),
            )
            .with_hint("catch it with `try` / `except` inside the loop")),
            Some(error) => Err(Diagnostic::error(
                span,
                format!("`{error}` can be raised here, but `{}` does not declare it", self.fn_name),
            )
            .with_hint(format!("add `raises {error}` to the signature, or catch it with `try` / `except`"))),
            None => Ok(()),
        }
    }

    pub(super) fn check_raise(&mut self, value: &mut Expr, span: Span) -> Result<()> {
        let ty = self.check_expr(value, None)?;
        let raised = match &ty {
            Type::Struct(_) | Type::Enum { .. } => vec![ty.clone()],
            // `raise e` after `except e:` sends each error on with its own type.
            Type::Errors(types) => types.clone(),
            _ => {
                return Err(Diagnostic::error(
                    value.span,
                    format!("`{ty}` cannot be raised; errors are structs or enums"),
                ));
            }
        };
        self.consume(value)?;
        self.reach(&raised, span)
    }

    pub(super) fn check_try(
        &mut self,
        body: &mut [Stmt],
        handlers: &mut [Handler],
        errors: &mut Vec<Type>,
        span: Span,
    ) -> Result<()> {
        self.tries.push(Vec::new());
        let result = self.check_block(body);
        let reached = self.tries.pop().expect("pushed above");
        result?;
        if reached.is_empty() {
            return Err(Diagnostic::error(span, "nothing in this `try` can raise"));
        }
        let after_body = self.moved.clone();
        let mut merged = after_body.clone();
        let mut coverage =
            Coverage { whole: vec![false; reached.len()], variants: vec![HashSet::new(); reached.len()] };
        for handler in handlers.iter_mut() {
            self.moved.clone_from(&after_body);
            self.push_scope();
            let bindings = self.check_catch(&mut handler.catch, &reached, &mut coverage, handler.span)?;
            for (name, ty, span) in bindings {
                let var = self.new_var(ty, false, span);
                self.declare(&name, var)?;
            }
            self.check_block(&mut handler.body)?;
            self.pop_scope();
            if !always_returns(&handler.body) {
                merged.extend(self.moved.drain());
            }
        }
        self.moved = merged;
        let uncaught: Vec<Type> = reached
            .iter()
            .zip(&coverage.whole)
            .filter(|(_, covered)| !**covered)
            .map(|(t, _)| t.clone())
            .collect();
        *errors = reached;
        self.reach(&uncaught, span)
    }

    /// Resolves one `except` clause, records what it covers and returns its bindings.
    fn check_catch(
        &mut self,
        catch: &mut Catch,
        reached: &[Type],
        coverage: &mut Coverage,
        span: Span,
    ) -> Result<Vec<(String, Type, Span)>> {
        match catch {
            Catch::Type { path, name, error } => {
                let index = caught_type(path, reached, span)?;
                coverage.cover(index, &mut self.warnings, span);
                *error = Some(index);
                Ok(vec![(name.clone(), reached[index].clone(), span)])
            }
            Catch::Variant { pattern, error } => {
                let Pattern::Variant { path, .. } = pattern else {
                    return Err(Diagnostic::error(span, "expected `Type.Variant` after `except`"));
                };
                let type_path = path[..path.len() - 1].to_vec();
                let index = caught_type(&type_path, reached, span)?;
                let ty = reached[index].clone();
                let bindings = self.check_pattern(pattern, &ty, true)?;
                let Pattern::Variant { variant: Some(variant), .. } = pattern else {
                    unreachable!("resolved")
                };
                let Type::Enum { named, .. } = &ty else { unreachable!("variants belong to enums") };
                let count = self.tables.types.enums[named.id].variants.len();
                coverage.variants[index].insert(*variant);
                if coverage.variants[index].len() == count {
                    coverage.cover(index, &mut self.warnings, span);
                }
                *error = Some(index);
                Ok(bindings)
            }
            Catch::All { name, error, several } => {
                if matches!(self.global(name), Some(Symbol::Struct(_) | Symbol::Enum(_))) {
                    return Err(Diagnostic::error(
                        span,
                        format!("name the caught error: `except {name} as e:`"),
                    ));
                }
                let remaining: Vec<usize> = (0..reached.len()).filter(|i| !coverage.whole[*i]).collect();
                match remaining.as_slice() {
                    [] => Err(Diagnostic::error(span, "every error is already caught above")),
                    [index] => {
                        coverage.cover(*index, &mut self.warnings, span);
                        *error = Some(*index);
                        Ok(vec![(name.clone(), reached[*index].clone(), span)])
                    }
                    _ => {
                        let types = remaining.iter().map(|i| reached[*i].clone()).collect();
                        for &i in &remaining {
                            coverage.cover(i, &mut self.warnings, span);
                        }
                        *several = remaining;
                        Ok(vec![(name.clone(), Type::Errors(types), span)])
                    }
                }
            }
        }
    }
}

/// The position, among the errors that reach this `try`, of the type named by `path`.
pub(super) fn caught_type(path: &[String], reached: &[Type], span: Span) -> Result<usize> {
    let name = path.last().expect("a path has a name");
    let shown = path.join(".");
    reached
        .iter()
        .position(|ty| {
            matches!(ty, Type::Struct(n) | Type::Enum { named: n, .. } if n.name.split('[').next() == Some(name.as_str()))
        })
        .ok_or_else(|| Diagnostic::error(span, format!("`{shown}` is never raised in this `try`")))
}

/// What the `except` clauses of one `try` have caught so far.
struct Coverage {
    whole: Vec<bool>,
    variants: Vec<HashSet<usize>>,
}

impl Coverage {
    fn cover(&mut self, index: usize, warnings: &mut Vec<Diagnostic>, span: Span) {
        if self.whole[index] {
            warnings.push(
                Diagnostic::warning(span, "this clause can never run")
                    .with_hint("an earlier clause catches these errors"),
            );
        }
        self.whole[index] = true;
    }
}
