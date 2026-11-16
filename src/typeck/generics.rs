//! Generic functions: each call infers or reads the type arguments, checks the bounds, and
//! asks for a specialized copy of the function, checked again with the concrete types.
//!
//! The generic body itself is checked once with its parameters as `Type::Param`: only the
//! operations its bounds allow are accepted there.

use std::collections::HashMap;

use super::Checker;
use super::call::order_args;
use super::resolve::{FnSig, ParamSig};
use super::templates::Kind;
use crate::ast::{BinOp, Expr, ExprKind, FnDecl, Receiver, fn_c_name};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::Type;

/// A specialized copy of a generic function, waiting to be checked.
pub(super) struct Instance {
    pub module: usize,
    pub decl: FnDecl,
    pub sig: usize,
    pub bindings: Vec<(String, Type)>,
    /// The struct of a generic method, for `Self`.
    pub owner: Option<Type>,
    /// The compile-time value parameters, now bound to values in `bindings`.
    pub values: Vec<(String, Type)>,
}

/// Generic declarations and the copies made of them.
#[derive(Default)]
pub(super) struct Generics {
    /// The unchecked declaration of each generic function, with its module.
    pub decls: HashMap<usize, (usize, FnDecl)>,
    /// The signature of each specialization, by function and type arguments.
    pub instances: HashMap<(usize, Vec<Type>), usize>,
    pub pending: Vec<Instance>,
}

impl Checker<'_> {
    /// A call to a generic function becomes a call to its specialization.
    pub(super) fn check_generic_call(&mut self, expr: &mut Expr, index: usize) -> Result<Type> {
        let span = expr.span;
        let sig = &self.tables.fns[index];
        let generics = sig.generics.clone();
        let values = sig.values.clone();
        let params: Vec<_> =
            sig.params.iter().map(|p| (p.name.clone(), p.ty.clone(), p.default.clone())).collect();
        let ExprKind::Call { callee, type_args, args, .. } = &mut expr.kind else {
            unreachable!("called on a call")
        };
        let callee = callee.clone();
        let mut bound: Vec<Option<Type>> = vec![None; generics.len()];
        if !type_args.is_empty() {
            if type_args.len() != generics.len() {
                return Err(Diagnostic::error(
                    span,
                    format!("`{callee}` takes {} type argument(s)", generics.len()),
                ));
            }
            for ((slot, texpr), (name, _)) in bound.iter_mut().zip(std::mem::take(type_args)).zip(&generics) {
                *slot = Some(match values.iter().find(|(n, _)| n == name) {
                    Some((_, ty)) => self.compile_time_arg(&texpr, ty)?,
                    None => self.resolve_arg(&texpr)?,
                });
            }
        }
        let probes = order_args(&callee, &params, args.clone(), span)?;
        let names: Vec<String> = generics.iter().map(|(n, _)| n.clone()).collect();
        for (mut probe, (_, ty, _)) in probes.into_iter().zip(&params) {
            if self.tables.has_param(ty) {
                // An argument that cannot be typed alone, such as `None`, binds nothing;
                // its real check comes later.
                if let Ok(found) = self.probe(&mut probe, ty) {
                    unify(ty, &found, &names, &mut bound, &self.tables.templates);
                }
            }
        }
        let mut bindings = Vec::new();
        for ((name, bounds), ty) in generics.iter().zip(bound) {
            let ty = ty.ok_or_else(|| {
                Diagnostic::error(span, format!("cannot infer `{name}` in this call to `{callee}`"))
                    .with_hint(format!("write it: `{callee}[i32](...)`"))
            })?;
            for &trait_id in bounds {
                if !self.satisfies(&ty, trait_id) {
                    let trait_name = &self.tables.traits[trait_id].name;
                    return Err(Diagnostic::error(
                        span,
                        format!(
                            "`{ty}` does not implement `{trait_name}`, required by `{name}` of `{callee}`"
                        ),
                    ));
                }
            }
            bindings.push((name.clone(), ty));
        }
        let instance = self.instance(index, &bindings, span)?;
        self.check_fn_call(expr, instance)
    }

    /// `Pair(1, 2)` or `Pair[i32](1, 2)`: the type arguments, read or inferred from the
    /// fields, pick the struct made from the template.
    pub(super) fn check_generic_construct(
        &mut self,
        expr: &mut Expr,
        template: usize,
        expected: Option<Type>,
    ) -> Result<Type> {
        use super::templates::Templates;
        let span = expr.span;
        let names = self.tables.templates.list[template].params.clone();
        let ExprKind::Call { callee, type_args, args, .. } = &mut expr.kind else {
            unreachable!("called on a call")
        };
        let callee = callee.clone();
        let mut bound: Vec<Option<Type>> = vec![None; names.len()];
        let hint = match expected {
            Some(Type::Struct(named)) => self
                .tables
                .templates
                .origin(Kind::Struct, named.id)
                .filter(|(t, _)| *t == template)
                .map(|(_, a)| a.clone()),
            _ => None,
        };
        if !type_args.is_empty() {
            if type_args.len() != names.len() {
                return Err(Diagnostic::error(
                    span,
                    format!("`{callee}` takes {} type argument(s)", names.len()),
                ));
            }
            for (slot, texpr) in bound.iter_mut().zip(std::mem::take(type_args)) {
                *slot = Some(self.resolve(&texpr)?);
            }
        } else if let Some(args) = hint {
            bound = args.into_iter().map(Some).collect();
        } else {
            let params = names.iter().map(|n| Type::Param(n.clone())).collect();
            let generic = Templates::struct_for(&mut self.tables, template, params, span)?;
            let fields: Vec<_> = self.tables.types.structs[generic]
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.clone(), None))
                .collect();
            let probes = order_args(&callee, &fields, args.clone(), span)?;
            for (mut probe, (_, ty, _)) in probes.into_iter().zip(&fields) {
                // An argument that cannot be typed alone, such as `None`, binds nothing;
                // its real check comes later.
                if let Ok(found) = self.probe(&mut probe, ty) {
                    unify(ty, &found, &names, &mut bound, &self.tables.templates);
                }
            }
        }
        let mut types = Vec::new();
        for (name, ty) in names.iter().zip(bound) {
            types.push(ty.ok_or_else(|| {
                Diagnostic::error(span, format!("cannot infer `{name}` in `{callee}(...)`"))
                    .with_hint(format!("write it: `{callee}[i32](...)`"))
            })?);
        }
        let id = Templates::struct_for(&mut self.tables, template, types, span)?;
        self.check_construct(expr, id)
    }

    /// `Pair.make(1, 2)`: a static method of a generic struct, whose type arguments come
    /// from the expected type or from the arguments.
    pub(super) fn check_generic_static(
        &mut self,
        expr: &mut Expr,
        template: usize,
        expected: Option<Type>,
    ) -> Result<Type> {
        use super::templates::Templates;
        let span = expr.span;
        let names = self.tables.templates.list[template].params.clone();
        let type_name = self.tables.templates.list[template].decl.name.clone();
        let ExprKind::Method { name, args, .. } = &expr.kind else { unreachable!("called on a method") };
        let (method, args) = (name.clone(), args.clone());
        let hint = match &expected {
            Some(Type::Struct(named)) => self
                .tables
                .templates
                .origin(Kind::Struct, named.id)
                .filter(|(t, _)| *t == template)
                .map(|(_, a)| a.clone()),
            _ => None,
        };
        let types = if let Some(types) = hint {
            types
        } else {
            let params = names.iter().map(|n| Type::Param(n.clone())).collect();
            let generic = Templates::struct_for(&mut self.tables, template, params, span)?;
            let index =
                self.tables.methods.get(&(generic, method.clone())).copied().ok_or_else(|| {
                    Diagnostic::error(span, format!("`{type_name}` has no method `{method}`"))
                })?;
            let params: Vec<_> =
                self.tables.fns[index].params.iter().map(|p| (p.name.clone(), p.ty.clone(), None)).collect();
            let probes = order_args(&method, &params, args, span)?;
            let mut bound: Vec<Option<Type>> = vec![None; names.len()];
            for (mut probe, (_, ty, _)) in probes.into_iter().zip(&params) {
                if let Ok(found) = self.probe(&mut probe, ty) {
                    unify(ty, &found, &names, &mut bound, &self.tables.templates);
                }
            }
            let mut types = Vec::new();
            for (name, ty) in names.iter().zip(bound) {
                types.push(ty.ok_or_else(|| {
                    Diagnostic::error(span, format!("cannot infer `{name}` in `{type_name}.{method}(...)`"))
                        .with_hint(format!("give the variable its type: `{type_name}[i32]`"))
                })?);
            }
            types
        };
        let id = Templates::struct_for(&mut self.tables, template, types, span)?;
        self.check_static_call(expr, id)
    }

    /// `Option.Some(1)` or `Option.Nothing`: the type arguments come from the expected
    /// type, or from the variant's values.
    pub(super) fn check_generic_variant(
        &mut self,
        expr: &mut Expr,
        template: usize,
        variant: &str,
        args: Vec<Expr>,
        expected: Option<Type>,
    ) -> Result<Type> {
        use super::templates::Templates;
        let span = expr.span;
        let decl_name = self.tables.templates.enums[template].decl.name.clone();
        let names = self.tables.templates.enums[template].params.clone();
        let hint = match expected {
            Some(Type::Enum { named, .. }) => self
                .tables
                .templates
                .origin(Kind::Enum, named.id)
                .filter(|(t, _)| *t == template)
                .map(|(_, a)| a.clone()),
            _ => None,
        };
        let types = if let Some(types) = hint {
            types
        } else {
            let params = names.iter().map(|n| Type::Param(n.clone())).collect();
            let generic = Templates::enum_for(&mut self.tables, template, params, span)?;
            let def = &self.tables.types.enums[generic];
            let fields: Vec<_> = def
                .variants
                .iter()
                .find(|v| v.name == variant)
                .ok_or_else(|| Diagnostic::error(span, format!("`{decl_name}` has no variant `{variant}`")))?
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.clone(), None))
                .collect();
            let shown = format!("{decl_name}.{variant}");
            let probes = order_args(&shown, &fields, args.clone(), span)?;
            let mut bound: Vec<Option<Type>> = vec![None; names.len()];
            for (mut probe, (_, ty, _)) in probes.into_iter().zip(&fields) {
                if let Ok(found) = self.probe(&mut probe, ty) {
                    unify(ty, &found, &names, &mut bound, &self.tables.templates);
                }
            }
            let mut types = Vec::new();
            for (name, ty) in names.iter().zip(bound) {
                types.push(ty.ok_or_else(|| {
                    Diagnostic::error(span, format!("cannot infer `{name}` in `{shown}`"))
                        .with_hint(format!("give the variable its type: `{decl_name}[i32]`"))
                })?);
            }
            types
        };
        let id = Templates::enum_for(&mut self.tables, template, types, span)?;
        self.check_variant(expr, id, variant, args)
    }

    /// A value for `[N: size]`: a number, or a `comptime` constant, that fits its type.
    fn compile_time_arg(&self, texpr: &crate::ast::TypeExpr, ty: &Type) -> Result<Type> {
        use crate::ast::TypeExpr;
        let span = texpr.span();
        let value = match texpr {
            TypeExpr::Size(value, _) => i128::from(*value),
            TypeExpr::Named { name, args, .. } if args.is_empty() => match self.const_value(name, span)? {
                crate::comptime::Value::Int(value) => value,
                _ => return Err(Diagnostic::error(span, format!("`{name}` is not an integer"))),
            },
            _ => return Err(Diagnostic::error(span, "expected a compile-time integer")),
        };
        match ty.as_int() {
            Some(int) if value >= 0 && int.fits(value) => Ok(Type::Const(value as u64)),
            _ => Err(Diagnostic::error(span, format!("{value} does not fit in `{ty}`"))),
        }
    }

    /// The type an argument would have, found on a copy so the real one is checked once.
    pub(super) fn probe(&mut self, arg: &mut Expr, param: &Type) -> Result<Type> {
        // Already checked, such as a receiver the checker borrowed itself.
        if let Some(ty) = &arg.ty {
            return Ok(ty.clone());
        }
        let warnings = self.warnings.len();
        // The copy moves nothing: the real argument will.
        let moved = self.moved.clone();
        let found = self.probe_type(arg, param);
        self.moved = moved;
        self.warnings.truncate(warnings);
        found
    }

    fn probe_type(&mut self, arg: &mut Expr, param: &Type) -> Result<Type> {
        let found = match (&mut arg.kind, param) {
            (ExprKind::Lambda { .. }, Type::Ref(target, _)) if matches!(**target, Type::Fn(..)) => {
                self.check_lambda(arg, true)?
            }
            (ExprKind::AddrOf { mutable, inner }, Type::Ref(target, _)) => {
                let mutable = *mutable;
                // `&box` lends what the `heap` holds, unless a `heap` is asked for.
                let found = match self.check_expr(inner, None)? {
                    Type::Heap(held) if !matches!(**target, Type::Heap(_)) => *held,
                    found => found,
                };
                Type::Ref(Box::new(found), mutable)
            }
            _ => self.check_expr(arg, None)?,
        };
        Ok(found)
    }

    /// The signature of the specialization of `index` for `bindings`, created on first use.
    fn instance(&mut self, index: usize, bindings: &[(String, Type)], span: Span) -> Result<usize> {
        let types: Vec<Type> = bindings.iter().map(|(_, t)| t.clone()).collect();
        if let Some(&sig) = self.generics.instances.get(&(index, types.clone())) {
            return Ok(sig);
        }
        let concrete = !types.iter().any(|t| self.tables.has_param(t));
        let (module, decl, base, owner) = if let Some((module, decl)) = self.generics.decls.get(&index) {
            (*module, decl.clone(), Vec::new(), None)
        } else {
            let method = self.tables.templates.generic_methods[&index].clone();
            (method.module, method.decl, method.bindings, Some(method.owner))
        };
        let mut full = base;
        full.extend(bindings.iter().cloned());
        let name = format!("{}__{}", decl.name, self.generics.instances.len());
        let generic = &self.tables.fns[index];
        let (params, ret, raises) = (generic.params.clone(), generic.ret.clone(), generic.raises.clone());
        let (receiver, values) = (generic.receiver, generic.values.clone());
        let mut sub = |t: &Type| self.tables.substitute(t, bindings, span);
        let params = params
            .iter()
            .map(|p| Ok(ParamSig { name: p.name.clone(), ty: sub(&p.ty)?, default: p.default.clone() }))
            .collect::<Result<_>>()?;
        let ret = sub(&ret)?;
        let raises = raises.iter().map(&mut sub).collect::<Result<_>>()?;
        let sig = FnSig {
            params,
            ret,
            raises,
            generics: Vec::new(),
            values: Vec::new(),
            variadic: false,
            c_name: if concrete { fn_c_name(module, &name) } else { String::new() },
            receiver,
        };
        self.tables.fns.push(sig);
        let sig = self.tables.fns.len() - 1;
        self.generics.instances.insert((index, types), sig);
        if concrete {
            let decl = FnDecl { name, generics: Vec::new(), ..decl };
            self.generics.pending.push(Instance { module, decl, sig, bindings: full, owner, values });
        }
        Ok(sig)
    }

    /// Whether `ty` meets the trait `trait_id`, as a type argument.
    pub(super) fn satisfies(&self, ty: &Type, trait_id: usize) -> bool {
        if let Type::Param(name) = ty {
            return self.bounds.get(name).is_some_and(|b| b.contains(&trait_id));
        }
        self.tables.satisfies(ty, trait_id)
    }

    /// The built-in trait with this name.
    fn builtin_trait(&self, name: &str) -> usize {
        self.tables.traits.iter().position(|t| t.module.is_none() && t.name == name).expect("built-in trait")
    }

    /// In a generic body, an operator on `T` needs the matching bound.
    pub(super) fn check_param_operator(&self, op: BinOp, name: &str, span: Span) -> Result<()> {
        let trait_name = match op {
            BinOp::Eq | BinOp::Ne => "Equal",
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => "Order",
            _ => match super::expr::operator_trait(op) {
                Some((trait_name, _)) => trait_name,
                None => {
                    return Err(Diagnostic::error(
                        span,
                        format!("`{}` does not apply to `{name}`", op.symbol()),
                    ));
                }
            },
        };
        if self.satisfies(&Type::Param(name.to_string()), self.builtin_trait(trait_name)) {
            return Ok(());
        }
        Err(Diagnostic::error(span, format!("`{}` on `{name}` needs the bound `{trait_name}`", op.symbol()))
            .with_hint(format!("write `[{name}: {trait_name}]`")))
    }

    /// `x.m(...)` on a value of a type parameter: `m` comes from one of its bounds.
    pub(super) fn check_param_method(&mut self, expr: &mut Expr, param: &str) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Method { receiver, name, args, .. } = &mut expr.kind else {
            unreachable!("called on a method")
        };
        let this = Type::Param(param.to_string());
        let bounds = self.bounds.get(param).cloned().unwrap_or_default();
        let found = bounds
            .iter()
            .flat_map(|&t| self.tables.traits[t].methods.iter())
            .find(|m| m.name == *name)
            .map(|m| (m.receiver, m.params.clone(), m.ret.clone()))
            .ok_or_else(|| Diagnostic::error(span, format!("no bound of `{param}` has a method `{name}`")))?;
        let (method_receiver, params, ret) = found;
        let self_binding = [("Self".to_string(), this.clone())];
        if args.len() != params.len() {
            return Err(Diagnostic::error(span, format!("`{name}` takes {} value(s)", params.len())));
        }
        let receiver = std::mem::replace(receiver.as_mut(), Expr::new(ExprKind::None, span));
        let mutable = method_receiver == Some(Receiver::Mut);
        let mut all = vec![if let Some(Receiver::Owned) = method_receiver {
            let mut receiver = receiver;
            self.consume(&mut receiver)?;
            receiver
        } else {
            self.place_of(&receiver).ok_or_else(|| {
                Diagnostic::error(
                    receiver.span,
                    format!("`{name}` borrows its value; store it in a variable first"),
                )
            })?;
            super::builtin::borrow(receiver, mutable)
        }];
        for (arg, param_ty) in args.iter_mut().zip(&params) {
            self.check_arg(arg, &param_ty.substitute(&self_binding))?;
            all.push(std::mem::replace(arg, Expr::new(ExprKind::None, arg.span)));
        }
        let callee = std::mem::take(name);
        expr.kind = ExprKind::Call { callee, type_args: Vec::new(), args: all, target: Some(String::new()) };
        Ok(ret.substitute(&self_binding))
    }
}

/// Binds the type parameters of `pattern` by matching it against `found`.
pub(super) fn unify(
    pattern: &Type,
    found: &Type,
    names: &[String],
    bound: &mut [Option<Type>],
    templates: &super::templates::Templates,
) {
    let unify = |p: &Type, f: &Type, bound: &mut [Option<Type>]| unify(p, f, names, bound, templates);
    match (pattern, found) {
        // `Pair[T]` against `Pair[i32]`: the same template, so the arguments match.
        (Type::Struct(p), Type::Struct(f)) => {
            if let (Some((tp, pargs)), Some((tf, fargs))) =
                (templates.origin(Kind::Struct, p.id), templates.origin(Kind::Struct, f.id))
                && tp == tf
            {
                pargs.iter().zip(fargs).for_each(|(p, f)| unify(p, f, bound));
            }
        }
        (Type::Enum { named: p, .. }, Type::Enum { named: f, .. }) => {
            if let (Some((tp, pargs)), Some((tf, fargs))) =
                (templates.origin(Kind::Enum, p.id), templates.origin(Kind::Enum, f.id))
                && tp == tf
            {
                pargs.iter().zip(fargs).for_each(|(p, f)| unify(p, f, bound));
            }
        }
        (Type::Param(name), _) => {
            if let Some(i) = names.iter().position(|n| n == name)
                && bound[i].is_none()
            {
                bound[i] = Some(found.clone());
            }
        }
        (Type::Ref(p, _), Type::Ref(f, _))
        | (Type::Ptr(p), Type::Ptr(f))
        | (Type::Optional(p), Type::Optional(f))
        | (Type::Vector(p), Type::Vector(f))
        | (Type::Heap(p), Type::Heap(f))
        | (Type::Shared(p), Type::Shared(f))
        | (Type::Array(p, _), Type::Array(f, _)) => unify(p, f, bound),
        (Type::Tuple(ps), Type::Tuple(fs)) => ps.iter().zip(fs).for_each(|(p, f)| unify(p, f, bound)),
        // A borrowed parameter given a plain value: `managed` borrows it for the call.
        (Type::Ref(p, _), f) => unify(p, f, bound),
        // `array[T, N]` against `array[i32, 4]`: `N` is 4.
        (Type::ArrayParam(p, len), Type::Array(f, value)) => {
            unify(p, f, bound);
            if let Some(i) = names.iter().position(|n| n == len)
                && bound[i].is_none()
            {
                bound[i] = Some(Type::Const(*value));
            }
        }
        (Type::Fn(ps, pr), Type::Fn(fs, fr)) => {
            ps.iter().zip(fs).for_each(|(p, f)| unify(p, f, bound));
            unify(pr, fr, bound);
        }
        _ => {}
    }
}
