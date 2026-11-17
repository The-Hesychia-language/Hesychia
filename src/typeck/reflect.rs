//! `comptime for f in fields(T):` runs its body once for each field of the struct `T`. In
//! the body, `f.name` is the field's name, and `field(x, f)` is that field of `x`.
//! `empty[T]()` is the value a `T` starts with.

use super::{Checker, Result};
use crate::ast::{Builtin, Expr, ExprKind, Stmt, StmtKind, TypeExpr};
use crate::diag::{Diagnostic, Span};
use crate::types::{Named, Type};

/// Calls `change` on every expression of `stmts`, the inner ones first.
fn each_expr(stmts: &mut [Stmt], change: &mut dyn FnMut(&mut Expr)) {
    for stmt in stmts {
        match &mut stmt.kind {
            StmtKind::Var { value, .. } | StmtKind::Expr(value) | StmtKind::Raise(value) => inside(value, change),
            StmtKind::Assign { target, value, .. } => {
                inside(target, change);
                inside(value, change);
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    inside(cond, change);
                    each_expr(body, change);
                }
                each_expr(otherwise.as_deref_mut().unwrap_or_default(), change);
            }
            StmtKind::While { cond: value, body }
            | StmtKind::For { iter: value, body, .. }
            | StmtKind::ParallelFor { iter: value, body, .. }
            | StmtKind::With { value, body, .. }
            | StmtKind::ComptimeFor { source: value, body, .. } => {
                inside(value, change);
                each_expr(body, change);
            }
            StmtKind::Return(value) => value.iter_mut().for_each(|v| inside(v, change)),
            StmtKind::ComptimeIf(cif) => {
                for (cond, body) in &mut cif.branches {
                    inside(cond, change);
                    each_expr(body, change);
                }
                each_expr(cif.otherwise.as_deref_mut().unwrap_or_default(), change);
            }
            StmtKind::Block(body) => each_expr(body, change),
            StmtKind::Match { subject, cases, .. } => {
                inside(subject, change);
                for case in cases {
                    case.guard.iter_mut().for_each(|g| inside(g, change));
                    each_expr(&mut case.body, change);
                }
            }
            StmtKind::Try { body, handlers, .. } => {
                each_expr(body, change);
                handlers.iter_mut().for_each(|h| each_expr(&mut h.body, change));
            }
            StmtKind::Select { cases, otherwise } => {
                for case in cases {
                    inside(&mut case.wait, change);
                    each_expr(&mut case.body, change);
                }
                each_expr(otherwise.as_deref_mut().unwrap_or_default(), change);
            }
            StmtKind::Fn { decl, .. } => each_expr(&mut decl.body, change),
            StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Pass
            | StmtKind::Comptime(_)
            | StmtKind::ComptimeAssert(_)
            | StmtKind::Asm { .. } => {}
        }
    }
}

fn inside(expr: &mut Expr, change: &mut dyn FnMut(&mut Expr)) {
    match &mut expr.kind {
        ExprKind::Unary(_, a)
        | ExprKind::Field { base: a, .. }
        | ExprKind::Convert(a)
        | ExprKind::Named { value: a, .. }
        | ExprKind::AddrOf { inner: a, .. }
        | ExprKind::Deref(a)
        | ExprKind::CopyOf(a)
        | ExprKind::Cast(a, _)
        | ExprKind::Wrap(a)
        | ExprKind::Unwrap(a)
        | ExprKind::Check { call: a, .. }
        | ExprKind::Formatted { value: a, .. }
        | ExprKind::Lambda { body: a, .. }
        | ExprKind::Move { value: a, .. } => inside(a, change),
        ExprKind::Binary(_, a, b) | ExprKind::Index { base: a, index: b } => {
            inside(a, change);
            inside(b, change);
        }
        ExprKind::Call { args, .. }
        | ExprKind::Tuple(args)
        | ExprKind::ArrayLit(args)
        | ExprKind::Construct(args)
        | ExprKind::Variant { args, .. }
        | ExprKind::Format(args)
        | ExprKind::Builtin { args, .. } => args.iter_mut().for_each(|a| inside(a, change)),
        ExprKind::Method { receiver, args, .. } | ExprKind::CallValue { callee: receiver, args } => {
            inside(receiver, change);
            args.iter_mut().for_each(|a| inside(a, change));
        }
        ExprKind::Range { start, end, step } => {
            inside(start, change);
            inside(end, change);
            step.iter_mut().for_each(|s| inside(s, change));
        }
        ExprKind::Slice { base, start, end } => {
            inside(base, change);
            start.iter_mut().chain(end).for_each(|s| inside(s, change));
        }
        ExprKind::Comprehension { value, iter, filter, .. } => {
            inside(value, change);
            inside(iter, change);
            filter.iter_mut().for_each(|f| inside(f, change));
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Name(_)
        | ExprKind::Global { .. }
        | ExprKind::None
        | ExprKind::FnRef(_) => {}
    }
    change(expr);
}

impl Checker<'_> {
    /// Replaces a `comptime for` with one copy of its body for each field.
    pub(super) fn expand_comptime_for(&mut self, stmt: &mut Stmt) -> Result<()> {
        let StmtKind::ComptimeFor { var, source, body } = std::mem::replace(&mut stmt.kind, StmtKind::Pass) else {
            unreachable!("called on a comptime for")
        };
        let names = self.reflected_fields(&source)?;
        let mut copies = Vec::new();
        for name in names {
            let mut copy = body.clone();
            let mut misuse = None;
            each_expr(&mut copy, &mut |expr| {
                if let Err(problem) = use_field(expr, &var, &name) {
                    misuse.get_or_insert(problem);
                }
            });
            if let Some(problem) = misuse {
                return Err(problem);
            }
            self.check_block(&mut copy)?;
            copies.push(Stmt { kind: StmtKind::Block(copy), span: stmt.span });
        }
        stmt.kind = StmtKind::Block(copies);
        Ok(())
    }

    /// The field names of the struct that `fields(T)` names.
    fn reflected_fields(&mut self, source: &Expr) -> Result<Vec<String>> {
        let wrong = || {
            Diagnostic::error(source.span, "a `comptime for` goes over the fields of a struct")
                .with_hint("write `comptime for f in fields(T):`")
        };
        let ExprKind::Call { callee, args, .. } = &source.kind else { return Err(wrong()) };
        let [Expr { kind: ExprKind::Name(name), span, .. }] = args.as_slice() else { return Err(wrong()) };
        if callee != "fields" {
            return Err(wrong());
        }
        let ty = self.resolve(&TypeExpr::Named { name: name.clone(), args: Vec::new(), span: *span })?;
        let Type::Struct(Named { id, .. }) = &ty else {
            return Err(Diagnostic::error(*span, format!("`{ty}` is not a struct: it has no fields to go over")));
        };
        let def = &self.tables.types.structs[*id];
        if def.module != self.module && def.fields.iter().any(|f| f.private) {
            return Err(Diagnostic::error(
                source.span,
                format!("`{}` has private fields: only its module can go over them", def.name),
            ));
        }
        Ok(def.fields.iter().map(|f| f.name.clone()).collect())
    }

    /// `empty[T]()`: the value a `T` starts with.
    pub(super) fn check_empty(&mut self, expr: &mut Expr) -> Result<Type> {
        let ExprKind::Call { type_args, .. } = &expr.kind else { unreachable!("called on a call") };
        let ty = self.resolve(&type_args[0])?;
        // In a generic body checked for any `T`, the value waits for the type.
        if self.tables.has_param(&ty) {
            return Ok(ty);
        }
        let Some(value) = self.starting_value(&ty, expr.span) else {
            return Err(Diagnostic::error(expr.span, format!("a `{ty}` has no empty value")).with_hint(
                "numbers, `bool`, `str`, `vector`, `Optional`, and structs and tuples of them have one",
            ));
        };
        *expr = value;
        Ok(ty)
    }

    /// `0`, `False`, `""`, `[]`, `None`; a struct, tuple or array of them; the first case of
    /// an enum when it has no fields.
    fn starting_value(&self, ty: &Type, span: Span) -> Option<Expr> {
        let all = |types: &mut dyn Iterator<Item = &Type>| -> Option<Vec<Expr>> {
            types.map(|t| self.starting_value(t, span)).collect()
        };
        let kind = match ty {
            Type::Vector(_) => ExprKind::Builtin { op: Builtin::VecNew, args: Vec::new() },
            Type::Optional(_) => ExprKind::None,
            Type::Str => ExprKind::Str(String::new()),
            Type::Int(_) => ExprKind::Int(0),
            Type::Float(_) => ExprKind::Float(0.0),
            Type::Bool => ExprKind::Bool(false),
            Type::Struct(named) => {
                let def = &self.tables.types.structs[named.id];
                // A struct that releases something by itself is not made from nothing.
                if def.drop_method().is_some() {
                    return None;
                }
                ExprKind::Construct(all(&mut def.fields.iter().map(|f| &f.ty))?)
            }
            Type::Tuple(items) => ExprKind::Tuple(all(&mut items.iter())?),
            Type::Enum { named, .. } => {
                let first = self.tables.types.enums[named.id].variants.first()?;
                if !first.fields.is_empty() {
                    return None;
                }
                ExprKind::Variant { index: 0, args: Vec::new() }
            }
            _ => return None,
        };
        let mut value = Expr::new(kind, span);
        value.ty = Some(ty.clone());
        Some(value)
    }
}

/// In one copy of the body: `f.name` becomes the field's name, `field(x, f)` that field of `x`.
fn use_field(expr: &mut Expr, var: &str, field: &str) -> Result<()> {
    let is_var = |e: &Expr| matches!(&e.kind, ExprKind::Name(n) if n == var);
    match &mut expr.kind {
        ExprKind::Field { base, name } if is_var(base) => {
            if name != "name" {
                return Err(Diagnostic::error(expr.span, format!("a field has a `name`, not a `{name}`")));
            }
            expr.kind = ExprKind::Str(field.to_string());
        }
        ExprKind::Call { callee, args, .. } if callee == "field" && args.len() == 2 && is_var(&args[1]) => {
            let base = args.swap_remove(0);
            expr.kind = ExprKind::Field { base: Box::new(base), name: field.to_string() };
        }
        _ => {}
    }
    Ok(())
}
