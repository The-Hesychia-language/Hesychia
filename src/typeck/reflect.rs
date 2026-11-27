//! `comptime for f in fields(T):` runs its body once for each field of the struct `T`. In
//! the body, `f.name` is the field's name, `f.doc` the comment above it, `f.has_default`
//! whether it has a value of its own, `f.decorator("short")` the text of `@short("v")` above
//! it ("" without one), and `field(x, f)` is that field of `x`.
//!
//! `comptime for v in variants(T):` does the same for the cases of the enum `T`, with
//! `v.name` and `v.doc`; there `fields(v)` are the fields of the case, `empty[T](v)` is a
//! value of that case, and `field(x, f)` a field of `x`, which must be of that case.
//!
//! `empty[T]()` is the value a `T` starts with, and `doc(T)` the comment above `T`.

use super::call::literal;
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
        ExprKind::Collection { keys, values, .. } => {
            keys.iter_mut().chain(values.iter_mut().flatten()).for_each(|e| inside(e, change));
        }
        ExprKind::Int(_)
        | ExprKind::Bytes(_)
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

/// What a `comptime for` goes over: a field, or a case of an enum.
struct Part {
    name: String,
    doc: String,
    has_default: bool,
    /// The decorators above a field, by name, with their text.
    decorators: Vec<(String, String)>,
    /// For a field of an enum's case: the name of the case.
    case: Option<String>,
    /// For a case: the type as written, to name it in the copies of the body.
    of: Option<String>,
}

impl Checker<'_> {
    /// Replaces a `comptime for` with one copy of its body for each field or case.
    pub(super) fn expand_comptime_for(&mut self, stmt: &mut Stmt) -> Result<()> {
        let StmtKind::ComptimeFor { var, source, body } = std::mem::replace(&mut stmt.kind, StmtKind::Pass) else {
            unreachable!("called on a comptime for")
        };
        let mut copies = Vec::new();
        for part in self.reflected(&source)? {
            let mut copy = body.clone();
            let mut misuse = None;
            each_expr(&mut copy, &mut |expr| {
                if let Err(problem) = use_part(expr, &var, &part) {
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

    /// The type that the name `name` stands for, in `fields(T)` and the like.
    fn reflected_type(&mut self, name: &str, span: Span) -> Result<Type> {
        self.resolve(&TypeExpr::Named { name: name.to_string(), args: Vec::new(), span })
    }

    /// The fields of `fields(T)`, or the cases of `variants(T)`.
    fn reflected(&mut self, source: &Expr) -> Result<Vec<Part>> {
        let wrong = || {
            Diagnostic::error(source.span, "a `comptime for` goes over the fields of a struct or the cases of an enum")
                .with_hint("write `comptime for f in fields(T):` or `comptime for v in variants(T):`")
        };
        let ExprKind::Call { callee, args, .. } = &source.kind else { return Err(wrong()) };
        let (name, span, case) = match args.as_slice() {
            [Expr { kind: ExprKind::Name(name), span, .. }] => (name, *span, None),
            // `fields(v)`, as a copy of the body for the case `v` has it.
            [Expr { kind: ExprKind::Name(name), span, .. }, Expr { kind: ExprKind::Str(case), .. }] => {
                (name, *span, Some(case.clone()))
            }
            _ => return Err(wrong()),
        };
        let ty = self.reflected_type(name, span)?;
        let part = |f: &crate::types::FieldDef, case: &Option<String>| Part {
            name: f.name.clone(),
            doc: f.doc.clone(),
            has_default: f.default.is_some(),
            decorators: f.decorators.clone(),
            case: case.clone(),
            of: None,
        };
        match (callee.as_str(), &ty, &case) {
            ("fields", Type::Struct(Named { id, .. }), None) => {
                let def = &self.tables.types.structs[*id];
                if def.module != self.module && def.fields.iter().any(|f| f.private) {
                    return Err(Diagnostic::error(
                        source.span,
                        format!("`{}` has private fields: only its module can go over them", def.name),
                    ));
                }
                Ok(def.fields.iter().map(|f| part(f, &None)).collect())
            }
            ("fields", Type::Enum { named, .. }, Some(wanted)) => {
                let def = &self.tables.types.enums[named.id];
                let variant = def.variants.iter().find(|v| &v.name == wanted).ok_or_else(wrong)?;
                Ok(variant.fields.iter().map(|f| part(f, &case)).collect())
            }
            ("fields", _, _) => {
                Err(Diagnostic::error(span, format!("`{ty}` is not a struct: it has no fields to go over")))
            }
            ("variants", Type::Enum { named, .. }, None) => {
                let def = &self.tables.types.enums[named.id];
                let case = |v: &crate::types::VariantDef| Part {
                    name: v.name.clone(),
                    doc: v.doc.clone(),
                    has_default: false,
                    decorators: Vec::new(),
                    case: None,
                    of: Some(name.clone()),
                };
                Ok(def.variants.iter().map(case).collect())
            }
            ("variants", _, _) => {
                Err(Diagnostic::error(span, format!("`{ty}` is not an enum: it has no cases to go over")))
            }
            _ => Err(wrong()),
        }
    }

    /// `doc(T)`: the comment above the struct or the enum `T`.
    pub(super) fn check_doc(&mut self, expr: &mut Expr) -> Result<Type> {
        let ExprKind::Call { args, .. } = &expr.kind else { unreachable!("called on a call") };
        let [Expr { kind: ExprKind::Name(name), span, .. }] = args.as_slice() else {
            return Err(Diagnostic::error(expr.span, "`doc` takes a type: `doc(T)`"));
        };
        let doc = match self.reflected_type(&name.clone(), *span)? {
            Type::Struct(named) => self.tables.types.structs[named.id].doc.clone(),
            Type::Enum { named, .. } => self.tables.types.enums[named.id].doc.clone(),
            _ => String::new(),
        };
        expr.kind = ExprKind::Str(doc);
        Ok(Type::Str)
    }

    /// `empty[T]()`: the value a `T` starts with. `empty[T](v)`, in a `comptime for` over
    /// the cases of `T`: a value of the case `v`.
    pub(super) fn check_empty(&mut self, expr: &mut Expr) -> Result<Type> {
        let ExprKind::Call { type_args, args, .. } = &expr.kind else { unreachable!("called on a call") };
        let ty = self.resolve(&type_args[0])?;
        // In a generic body checked for any `T`, the value waits for the type.
        if self.tables.has_param(&ty) {
            return Ok(ty);
        }
        let value = match (args.as_slice(), &ty) {
            ([], _) => self.starting_value(&ty, expr.span),
            ([Expr { kind: ExprKind::Str(case), .. }], Type::Enum { named, .. }) => {
                let def = &self.tables.types.enums[named.id];
                def.variants.iter().position(|v| &v.name == case).and_then(|index| {
                    let fields = &self.tables.types.enums[named.id].variants[index].fields;
                    let args = fields.iter().map(|f| self.field_start(f, expr.span)).collect::<Option<_>>()?;
                    let mut value = Expr::new(ExprKind::Variant { index, args }, expr.span);
                    value.ty = Some(ty.clone());
                    Some(value)
                })
            }
            _ => None,
        };
        let Some(value) = value else {
            return Err(Diagnostic::error(expr.span, format!("a `{ty}` has no empty value")).with_hint(
                "numbers, `bool`, `str`, `vector`, `Optional`, and structs and tuples of them have one",
            ));
        };
        *expr = value;
        Ok(ty)
    }

    /// What a field starts with: its default, or the empty value of its type.
    fn field_start(&self, field: &crate::types::FieldDef, span: Span) -> Option<Expr> {
        let Some(default) = &field.default else { return self.starting_value(&field.ty, span) };
        let mut value = Expr::new(literal(default.clone()), span);
        value.ty = Some(field.ty.clone());
        // A default for an `Optional` field is the value it holds.
        if let Type::Optional(inner) = &field.ty
            && !matches!(value.kind, ExprKind::None)
        {
            value.ty = Some(inner.as_ref().clone());
            let mut wrapped = Expr::new(ExprKind::Wrap(Box::new(value)), span);
            wrapped.ty = Some(field.ty.clone());
            return Some(wrapped);
        }
        Some(value)
    }

    /// `0`, `False`, `""`, `[]`, `None`; a struct, tuple or array of them, a field with a
    /// default starting with it; the first case of an enum when it has no fields.
    fn starting_value(&self, ty: &Type, span: Span) -> Option<Expr> {
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
                ExprKind::Construct(def.fields.iter().map(|f| self.field_start(f, span)).collect::<Option<_>>()?)
            }
            Type::Tuple(items) => {
                ExprKind::Tuple(items.iter().map(|t| self.starting_value(t, span)).collect::<Option<_>>()?)
            }
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

/// In one copy of the body, what is said of the loop's variable becomes what it is for
/// this field or case.
fn use_part(expr: &mut Expr, var: &str, part: &Part) -> Result<()> {
    let is_var = |e: &Expr| matches!(&e.kind, ExprKind::Name(n) if n == var);
    match &mut expr.kind {
        ExprKind::Field { base, name } if is_var(base) => {
            expr.kind = match name.as_str() {
                "name" => ExprKind::Str(part.name.clone()),
                "doc" => ExprKind::Str(part.doc.clone()),
                "has_default" if part.of.is_none() => ExprKind::Bool(part.has_default),
                other => {
                    let known = if part.of.is_some() { "`name` and `doc`" } else { "`name`, `doc` and `has_default`" };
                    return Err(Diagnostic::error(expr.span, format!("`{var}` has {known}, not `{other}`")));
                }
            };
        }
        // `f.decorator("short")`: the text of `@short("v")` above the field, "" without one.
        ExprKind::Method { receiver, name, args, .. }
            if is_var(receiver) && name == "decorator" && part.of.is_none() =>
        {
            let [Expr { kind: ExprKind::Str(wanted), .. }] = args.as_slice() else {
                return Err(Diagnostic::error(expr.span, format!("write `{var}.decorator(\"name\")`")));
            };
            let text = part.decorators.iter().find(|(n, _)| n == wanted).map(|(_, t)| t.clone());
            expr.kind = ExprKind::Str(text.unwrap_or_default());
        }
        // `field(x, f)`: the field of a struct, or of the case of an enum that `x` is.
        ExprKind::Call { callee, args, .. }
            if callee == "field" && args.len() == 2 && is_var(&args[1]) && part.of.is_none() =>
        {
            let base = args.swap_remove(0);
            let name = match &part.case {
                Some(case) => format!("as.{case}.{}", part.name),
                None => part.name.clone(),
            };
            expr.kind = ExprKind::Field { base: Box::new(base), name };
        }
        // `fields(v)` and `empty[T](v)` name the case for the copies made later.
        ExprKind::Call { callee, args, .. } if callee == "fields" && args.len() == 1 && is_var(&args[0]) => {
            let Some(of) = &part.of else {
                return Err(Diagnostic::error(expr.span, format!("`{var}` is a field: it has no fields")));
            };
            let span = args[0].span;
            *args = vec![Expr::new(ExprKind::Name(of.clone()), span), Expr::new(ExprKind::Str(part.name.clone()), span)];
        }
        ExprKind::Call { callee, args, .. }
            if callee == "empty" && args.len() == 1 && is_var(&args[0]) && part.of.is_some() =>
        {
            args[0].kind = ExprKind::Str(part.name.clone());
        }
        _ => {}
    }
    Ok(())
}
