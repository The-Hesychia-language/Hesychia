//! Enums, `match` and `Optional`: variant values, patterns, exhaustiveness and `None` checks.

use std::collections::HashSet;

use super::call::order_args;
use super::stmt::always_returns;
use super::{Checker, expect_type};
use crate::ast::{BinOp, Case, Expr, ExprKind, Pattern};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{Named, Type};

impl Checker<'_> {
    pub(super) fn enum_type(&self, id: usize) -> Type {
        let def = &self.tables.types.enums[id];
        let plain = def.variants.iter().all(|v| v.fields.is_empty());
        Type::Enum { named: Named { id, name: def.name.clone() }, plain }
    }

    /// `Shape.Circle(1.0)` or `Color.Red`: one value per field of the variant.
    pub(super) fn check_variant(
        &mut self,
        expr: &mut Expr,
        id: usize,
        name: &str,
        args: Vec<Expr>,
    ) -> Result<Type> {
        let span = expr.span;
        let def = &self.tables.types.enums[id];
        let index = def
            .variants
            .iter()
            .position(|v| v.name == name)
            .ok_or_else(|| Diagnostic::error(span, format!("`{}` has no variant `{name}`", def.name)))?;
        let shown = format!("{}.{name}", def.name);
        let fields: Vec<_> =
            def.variants[index].fields.iter().map(|f| (f.name.clone(), f.ty.clone(), None)).collect();
        let mut values = order_args(&shown, &fields, args, span)?;
        for (value, (_, ty, _)) in values.iter_mut().zip(&fields) {
            self.check_value(value, ty)?;
        }
        expr.kind = ExprKind::Variant { index, args: values };
        Ok(self.enum_type(id))
    }

    /// `x is None` and `x is not None`.
    pub(super) fn check_none_test(&mut self, left: &mut Expr, right: &mut Expr, span: Span) -> Result<Type> {
        if !matches!(right.kind, ExprKind::None) {
            return Err(Diagnostic::error(right.span, "`is` only compares with `None`")
                .with_hint("compare values with `==`"));
        }
        let ty = self.check_expr(left, None)?;
        if !matches!(ty, Type::Optional(_)) {
            return Err(Diagnostic::error(span, format!("a `{ty}` value is never `None`")));
        }
        right.ty = Some(ty);
        Ok(Type::Bool)
    }

    /// The `Optional` variables known to hold a value when `cond` evaluates to `holds`:
    /// `x is not None and ...` when true, `x is None or ...` when false.
    pub(super) fn narrowing(&self, cond: &Expr, holds: bool) -> Vec<(usize, String)> {
        match &cond.kind {
            ExprKind::Binary(BinOp::And, left, right) if holds => {
                [left, right].iter().flat_map(|side| self.narrowing(side, true)).collect()
            }
            ExprKind::Binary(BinOp::Or, left, right) if !holds => {
                [left, right].iter().flat_map(|side| self.narrowing(side, false)).collect()
            }
            ExprKind::Binary(op @ (BinOp::Is | BinOp::IsNot), left, right)
                if (*op == BinOp::IsNot) == holds =>
            {
                if !matches!(right.kind, ExprKind::None) {
                    return Vec::new();
                }
                self.place_of(left)
                    .filter(|place| !place.text.contains('['))
                    .map(|place| (place.var, place.text))
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// After a write or a `&mut` borrow, nothing is known about the variable's `Optional`s.
    /// Records that a variable is changed: what it was narrowed to no longer holds.
    pub(super) fn forget_narrowing(&mut self, var: usize) {
        self.narrowed.retain(|(id, _)| *id != var);
        self.probe.note(var, super::managed::Mode::Mut, None);
        if let Some(at) = self.copied.remove(&var) {
            self.warnings.push(
                Diagnostic::warning(at, "this copy does not see the changes made afterwards to the original")
                    .with_hint("to keep one value with several owners, create it with `shared(...)`"),
            );
        }
        if self.lent(var).is_some() && self.lend_error.is_none() {
            self.lend_error = Some(lent_error(self, var));
        }
        for (id, changed) in &mut self.iterating {
            *changed |= *id == var;
        }
    }

    /// `match x:` takes `x` apart, moving out what its bindings own. `match &x:`, or a
    /// match on a reference, only reads it: those bindings are `&` references into it.
    pub(super) fn check_match(
        &mut self,
        subject: &mut Expr,
        cases: &mut [Case],
        borrowed: &mut bool,
        span: Span,
    ) -> Result<()> {
        let explicit = matches!(subject.kind, ExprKind::AddrOf { mutable: false, .. });
        if let ExprKind::AddrOf { inner, .. } = &mut subject.kind
            && explicit
        {
            let inner = std::mem::replace(inner.as_mut(), Expr::new(ExprKind::None, span));
            *subject = inner;
        }
        let ty = self.check_expr(subject, None)?;
        if explicit && self.place_of(subject).is_none() {
            return Err(Diagnostic::error(
                subject.span,
                "only a variable, field or item can be matched by `&`",
            ));
        }
        let through_ref = matches!(&subject.kind, ExprKind::Deref(r) if matches!(r.ty, Some(Type::Ref(..))));
        *borrowed = explicit || through_ref;
        let mut bindings = cases
            .iter_mut()
            .map(|case| self.check_pattern(&mut case.pattern, &ty, true))
            .collect::<Result<Vec<_>>>()?;
        if *borrowed {
            for (_, ty, _) in bindings.iter_mut().flatten().filter(|(_, ty, _)| !ty.is_copy()) {
                *ty = Type::Ref(Box::new(ty.clone()), false);
            }
        } else if bindings.iter().flatten().any(|(_, ty, _)| !ty.is_copy()) {
            self.consume(subject)?;
        }
        self.check_exhaustive(cases, &ty, span)?;
        let start = self.moved.clone();
        let mut merged = start.clone();
        for (case, names) in cases.iter_mut().zip(bindings) {
            self.moved.clone_from(&start);
            self.push_scope();
            for (name, ty, span) in names {
                let var = self.new_var(ty, false, span);
                self.declare(&name, var)?;
            }
            self.check_block(&mut case.body)?;
            self.pop_scope();
            if !always_returns(&case.body) {
                merged.extend(self.moved.drain());
            }
        }
        self.moved = merged;
        Ok(())
    }

    /// Checks a pattern against the subject type and returns the names it binds.
    pub(super) fn check_pattern(
        &mut self,
        pattern: &mut Pattern,
        ty: &Type,
        may_bind: bool,
    ) -> Result<Vec<(String, Type, Span)>> {
        let span = pattern.span();
        let no_bind = || Diagnostic::error(span, "the options of a `|` pattern cannot bind names");
        if let Type::Errors(types) = ty {
            return self.check_error_pattern(pattern, types, may_bind);
        }
        match pattern {
            Pattern::Wildcard(_) => Ok(Vec::new()),
            Pattern::Bind(name, _) if may_bind => Ok(vec![(name.clone(), ty.clone(), span)]),
            Pattern::Bind(..) => Err(no_bind()),
            Pattern::None(_) if matches!(ty, Type::Optional(_)) => Ok(Vec::new()),
            Pattern::None(_) => Err(Diagnostic::error(span, format!("a `{ty}` value is never `None`"))),
            Pattern::ErrorType { .. } => Err(Diagnostic::error(
                span,
                format!("`as` names the type of a caught error, but this is a `{ty}`"),
            )),
            Pattern::Literal(value) => {
                if !matches!(ty, Type::Int(_) | Type::Float(_) | Type::Bool | Type::Char) {
                    return Err(Diagnostic::error(
                        span,
                        format!("a `{ty}` value cannot be matched against a literal"),
                    ));
                }
                let found = self.check_expr(value, Some(ty))?;
                expect_type(&found, ty, span)?;
                Ok(Vec::new())
            }
            Pattern::Or(options, _) => {
                for option in options {
                    self.check_pattern(option, ty, false)?;
                }
                Ok(Vec::new())
            }
            Pattern::Variant { path, bindings, variant, .. } => {
                let found = self.variant_of(path, ty, span)?;
                *variant = Some(found);
                let Type::Enum { named, .. } = ty else { unreachable!("checked by variant_of") };
                let fields = &self.tables.types.enums[named.id].variants[found].fields;
                let Some(names) = bindings else { return Ok(Vec::new()) };
                if !may_bind && !names.is_empty() {
                    return Err(no_bind());
                }
                if names.len() != fields.len() {
                    let message = format!(
                        "`{}` has {} field(s), {} name(s) given",
                        path.join("."),
                        fields.len(),
                        names.len()
                    );
                    return Err(Diagnostic::error(span, message));
                }
                Ok(names
                    .iter()
                    .zip(fields)
                    .filter(|(n, _)| *n != "_")
                    .map(|(n, f)| (n.clone(), f.ty.clone(), span))
                    .collect())
            }
        }
    }

    /// A case on a caught error names one of its types: `IoError`, or `IoError as e`.
    fn check_error_pattern(
        &mut self,
        pattern: &mut Pattern,
        types: &[Type],
        may_bind: bool,
    ) -> Result<Vec<(String, Type, Span)>> {
        let span = pattern.span();
        // `case IoError:` reads as a name or a path: here it is a type.
        let path = match pattern {
            Pattern::Wildcard(_) => return Ok(Vec::new()),
            Pattern::Bind(name, _) => vec![name.clone()],
            Pattern::Variant { path, bindings: None, .. } | Pattern::ErrorType { path, .. } => path.clone(),
            Pattern::Or(options, _) => {
                for option in options {
                    self.check_error_pattern(option, types, false)?;
                }
                return Ok(Vec::new());
            }
            _ => {
                return Err(Diagnostic::error(
                    span,
                    "a caught error is matched by its type: `case IoError as e:`",
                ));
            }
        };
        let index = super::errors::caught_type(&path, types, span)?;
        let name = match pattern {
            Pattern::ErrorType { name, .. } => name.clone(),
            _ => None,
        };
        if name.is_some() && !may_bind {
            return Err(Diagnostic::error(span, "the options of a `|` pattern cannot bind names"));
        }
        *pattern = Pattern::ErrorType { path, name: name.clone(), index: Some(index), span };
        Ok(name.map(|n| (n, types[index].clone(), span)).into_iter().collect())
    }

    /// The variant index named by `Type.Variant` (or `module.Type.Variant`) for the subject's enum.
    fn variant_of(&self, path: &[String], ty: &Type, span: Span) -> Result<usize> {
        let Type::Enum { named, .. } = ty else {
            return Err(Diagnostic::error(span, format!("a `{ty}` value has no variants")));
        };
        let def = &self.tables.types.enums[named.id];
        let [.., type_name, variant] = path else {
            return Err(Diagnostic::error(span, format!("write the variant as `{}.Variant`", def.name)));
        };
        if type_name.as_str() != def.name.split('[').next().unwrap_or_default() {
            return Err(Diagnostic::error(
                span,
                format!("expected a variant of `{}`, found `{type_name}`", def.name),
            ));
        }
        def.variants
            .iter()
            .position(|v| &v.name == variant)
            .ok_or_else(|| Diagnostic::error(span, format!("`{}` has no variant `{variant}`", def.name)))
    }

    /// Every value of the subject must be matched; cases after a catch-all are reported.
    fn check_exhaustive(&mut self, cases: &[Case], ty: &Type, span: Span) -> Result<()> {
        if let Some(position) =
            cases.iter().position(|c| matches!(c.pattern, Pattern::Wildcard(_) | Pattern::Bind(..)))
        {
            for case in &cases[position + 1..] {
                self.warnings.push(
                    Diagnostic::warning(case.span, "this case can never match")
                        .with_hint("an earlier case already matches every value"),
                );
            }
            return Ok(());
        }
        let mut covered = HashSet::new();
        for case in cases {
            collect_covered(&case.pattern, &mut covered);
        }
        let missing: Vec<String> = match ty {
            Type::Errors(types) => types
                .iter()
                .enumerate()
                .filter(|(i, _)| !covered.contains(&Covered::Variant(*i)))
                .map(|(_, t)| format!("`{t}`"))
                .collect(),
            Type::Enum { named, .. } => {
                let def = &self.tables.types.enums[named.id];
                def.variants
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !covered.contains(&Covered::Variant(*i)))
                    .map(|(_, v)| format!("`{}.{}`", def.name, v.name))
                    .collect()
            }
            Type::Bool => [(true, "`True`"), (false, "`False`")]
                .into_iter()
                .filter(|(b, _)| !covered.contains(&Covered::Bool(*b)))
                .map(|(_, text)| text.to_string())
                .collect(),
            _ => vec!["every other value".into()],
        };
        if missing.is_empty() {
            return Ok(());
        }
        Err(Diagnostic::error(span, format!("this `match` does not cover {}", missing.join(", ")))
            .with_hint("add the missing cases, or `case _:`"))
    }
}

#[derive(PartialEq, Eq, Hash)]
enum Covered {
    Variant(usize),
    Bool(bool),
}

fn collect_covered(pattern: &Pattern, out: &mut HashSet<Covered>) {
    match pattern {
        Pattern::Variant { variant: Some(index), .. } | Pattern::ErrorType { index: Some(index), .. } => {
            out.insert(Covered::Variant(*index));
        }
        Pattern::Literal(Expr { kind: ExprKind::Bool(value), .. }) => {
            out.insert(Covered::Bool(*value));
        }
        Pattern::Or(options, _) => options.iter().for_each(|p| collect_covered(p, out)),
        _ => {}
    }
}

/// The error for a change to a value lent to a thread.
fn lent_error(checker: &Checker<'_>, var: usize) -> Diagnostic {
    let span = checker.lent_span(var);
    Diagnostic::error(
        span,
        "this value is lent to a thread until the end of the `scope`, and cannot change meanwhile",
    )
    .with_hint("change it before the threads start, or after the `with` block")
}
