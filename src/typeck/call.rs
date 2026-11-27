//! Names, calls, methods, fields and ranges: everything resolved through a namespace.

use super::expr::is_literal;
use super::resolve::Symbol;
use super::{Checker, expect_type};
use crate::ast::{Expr, ExprKind, Receiver};
use crate::comptime::{self, Value};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{IntTy, Named, Type};

impl Checker<'_> {
    pub(super) fn check_name(&mut self, expr: &mut Expr) -> Result<Type> {
        let ExprKind::Name(name) = &expr.kind else { unreachable!("called on a name") };
        self.check_capture(name, expr.span)?;
        if let Some(var) = self.local(name) {
            let (id, ty, constant, boxed) = (var.id, var.ty.clone(), var.constant.clone(), var.boxed);
            if let Some(value) = constant {
                expr.kind = literal(value);
                return Ok(ty);
            }
            self.check_not_moved(id, name, expr.span)?;
            if self.lent(id) == Some(true) {
                return Err(Diagnostic::error(
                    expr.span,
                    format!("`{name}` is lent to a thread with `&mut` until the end of the `scope`"),
                )
                .with_hint("use it after the `with` block"));
            }
            if let Type::Optional(inner) = &ty
                && self.narrowed.contains(&(id, name.clone()))
            {
                let mut optional = Expr::new(std::mem::replace(&mut expr.kind, ExprKind::None), expr.span);
                optional.ty = Some(ty.clone());
                expr.kind = ExprKind::Unwrap(Box::new(optional));
                return Ok(inner.as_ref().clone());
            }
            if matches!(&ty, Type::Ref(inner, _) if matches!(**inner, Type::Fn(..))) {
                return Err(Diagnostic::error(
                    expr.span,
                    format!("`{name}` captures variables; it can only be called or passed as `&fn(...)`"),
                ));
            }
            // A reference reads as its target, and a large variable as its value on the heap;
            // the place stays reachable through `Deref`.
            if let Type::Ref(inner, _) | Type::Heap(inner) = &ty
                && (boxed || matches!(ty, Type::Ref(..)))
            {
                let mut reference =
                    Expr::new(std::mem::replace(&mut expr.kind, ExprKind::Tuple(Vec::new())), expr.span);
                reference.ty = Some(ty.clone());
                expr.kind = ExprKind::Deref(Box::new(reference));
                return Ok(inner.as_ref().clone());
            }
            return Ok(ty);
        }
        match self.global(name) {
            Some(Symbol::Global(i)) => Ok(self.global_value(expr, i)),
            Some(Symbol::Const(i)) => {
                let constant = &self.tables.consts[i];
                let ty = constant.ty.clone();
                expr.kind = literal(constant.value.clone());
                Ok(ty)
            }
            Some(Symbol::Fn(index)) => self.check_fn_value(expr, index),
            Some(Symbol::Builtin(_) | Symbol::ComptimeFn(_)) => Err(Diagnostic::error(
                expr.span,
                format!("`{name}` is a function; call it with `{name}(...)`"),
            )),
            Some(Symbol::Module(_)) => Err(Diagnostic::error(expr.span, format!("`{name}` is a module"))),
            Some(
                Symbol::Struct(_)
                | Symbol::Enum(_)
                | Symbol::Trait(_)
                | Symbol::GenericStruct(_)
                | Symbol::GenericEnum(_),
            ) => Err(Diagnostic::error(expr.span, format!("`{name}` is a type"))),
            None => match self.value_param(name) {
                Some((ty, value)) => {
                    expr.kind = ExprKind::Int(value);
                    Ok(ty)
                }
                None if name == "_" => {
                    Err(Diagnostic::error(expr.span, "`_` discards a value: it cannot be read")
                        .with_hint("give the value a name"))
                }
                None => Err(Diagnostic::error(expr.span, format!("unknown variable `{name}`"))),
            },
        }
    }

    pub(super) fn check_call(&mut self, expr: &mut Expr) -> Result<Type> {
        let expected = self.expected_struct.take();
        if let ExprKind::Call { callee, .. } = &expr.kind
            && let Some(Some(_)) = self.find_local(callee).map(|v| v.ty.deref_fn())
        {
            return self.check_value_call(expr);
        }
        if let Some(ty) = self.check_builtin_call(expr, expected.as_ref())? {
            return Ok(ty);
        }
        let span = expr.span;
        let ExprKind::Call { callee, type_args, args, .. } = &expr.kind else {
            unreachable!("called on a call")
        };
        if let Some(target) = Type::scalar(callee) {
            return self.check_conversion(expr, target);
        }
        if callee == "str" && args.len() == 1 && self.global("str").is_none() {
            return self.check_to_str(expr);
        }
        match callee.as_str() {
            "size_of" | "align_of" if type_args.len() == 1 && args.is_empty() => {
                let ty = self.resolve(&type_args[0])?;
                let layout = self
                    .tables
                    .types
                    .layout(&ty)
                    .ok_or_else(|| Diagnostic::error(span, format!("`{ty}` has no fixed size")))?;
                let Value::Int(value) = comptime::layout_query(callee, layout, span)? else { unreachable!() };
                expr.kind = ExprKind::Int(value);
                return Ok(Type::Int(IntTy::Size));
            }
            "empty" if type_args.len() == 1 && args.len() <= 1 && self.global("empty").is_none() => {
                return self.check_empty(expr);
            }
            "doc" if type_args.is_empty() && args.len() == 1 && self.global("doc").is_none() => {
                return self.check_doc(expr);
            }
            "embed" | "embed_bytes" | "source" | "program_source" | "program_files"
                if type_args.is_empty() && self.global(callee).is_none() =>
            {
                return self.check_embedded(expr, expected.as_ref());
            }
            "range" => return Err(Diagnostic::error(span, "`range` can only be used in a `for` loop")),
            _ if !type_args.is_empty() && !self.is_generic_fn(callee) => {
                return Err(Diagnostic::error(span, format!("`{callee}` does not take type arguments")));
            }
            _ => {}
        }
        match self.global(callee) {
            Some(Symbol::Fn(index)) => self.check_fn_call(expr, index),
            Some(Symbol::Struct(id)) => self.check_construct(expr, id),
            Some(Symbol::GenericStruct(template)) => self.check_generic_construct(expr, template, expected),
            Some(Symbol::ComptimeFn(_)) => self.check_comptime_call(expr),
            Some(_) => Err(Diagnostic::error(span, format!("`{callee}` is not a function"))),
            None if callee == "print" => Err(Diagnostic::error(span, "unknown function `print`")
                .with_hint("import it with `from io import print`")),
            None => Err(Diagnostic::error(span, format!("unknown function `{callee}`"))),
        }
    }

    /// A call to a `comptime fn` in running code: the compiler runs it, and its value takes
    /// its place.
    fn check_comptime_call(&mut self, expr: &mut Expr) -> Result<Type> {
        match self.eval_mut(expr)? {
            Value::Type(ty) => {
                Err(Diagnostic::error(expr.span, format!("this gives the type `{ty}`, not a value")))
            }
            value => {
                expr.kind = literal(value);
                self.check_expr(expr, None)
            }
        }
    }

    /// Checks the arguments of a call to the function `index` and records its C name.
    /// `args` may already hold a checked receiver, as the first argument of a method.
    pub(super) fn check_fn_call(&mut self, expr: &mut Expr, index: usize) -> Result<Type> {
        // `sys_read` and the other system calls of the standard library are `manual` code.
        if self.managed
            && let ExprKind::Call { callee, .. } = &expr.kind
            && callee.starts_with("sys_")
            && self.tables.fns[index].c_name.contains("_sys_")
            && self.stdlib_fn(index)
        {
            return Err(Diagnostic::error(expr.span, format!("`{callee}` is for `manual` code"))
                .with_hint("move the call into a `manual` function"));
        }
        if !self.tables.fns[index].generics.is_empty() {
            return self.check_generic_call(expr, index);
        }
        self.tables.templates.called(index);
        if !self.managed && self.tables.managed.contains(&index) {
            let ExprKind::Call { callee, .. } = &expr.kind else { unreachable!("called on a call") };
            return Err(Diagnostic::error(
                expr.span,
                format!("`manual` code cannot call `{callee}`, a `managed` function"),
            )
            .with_hint("make the caller `managed`, or the callee `manual`"));
        }
        let span = expr.span;
        let raises = self.tables.fns[index].raises.clone();
        let shown = match &expr.kind {
            ExprKind::Call { callee, .. } => callee.clone(),
            _ => unreachable!("called on a call"),
        };
        self.note_call(expr, &shown, &raises)?;
        let ExprKind::Call { callee, args, target, .. } = &mut expr.kind else {
            unreachable!("called on a call")
        };
        let sig = &self.tables.fns[index];
        let ret = sig.ret.clone();
        let variadic = sig.variadic;
        *target = Some(sig.c_name.clone());
        let skip =
            usize::from(sig.receiver.is_some() && sig.params.first().is_some_and(|p| p.name == "self"));
        let skip = skip.min(args.len());
        let params: Vec<_> =
            sig.params[skip..].iter().map(|p| (p.name.clone(), p.ty.clone(), p.default.clone())).collect();
        let given = args.split_off(skip);
        let (fixed, extra) = if variadic && given.len() > params.len() {
            let mut given = given;
            let extra = given.split_off(params.len());
            (given, extra)
        } else {
            (given, Vec::new())
        };
        let mut ordered = order_args(callee, &params, fixed, span)?;
        let callee = callee.clone();
        for (position, (arg, (_, ty, _))) in ordered.iter_mut().zip(&params).enumerate() {
            let moved = self.place_of(arg).filter(|p| p.whole).map(|p| p.var);
            self.check_arg(arg, ty)?;
            // An argument a `managed` function keeps: said if the value is used again.
            if let (Some(var), Some(&kept)) = (moved, self.tables.kept_at.get(&(index, position + skip))) {
                self.moved_into.insert(var, (callee.clone(), kept));
            }
        }
        args.append(&mut ordered);
        for mut arg in extra {
            self.check_variadic_arg(&mut arg)?;
            args.push(arg);
        }
        self.check_call_conflicts(args)?;
        Ok(ret)
    }

    /// `Point(1, 2)` or `Point(x=1, y=2)`: one value per field, in declaration order.
    pub(super) fn check_construct(&mut self, expr: &mut Expr, id: usize) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Call { callee, args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        let def = &self.tables.types.structs[id];
        // `S()` on a struct whose fields are all private: each field starts empty.
        if args.is_empty() && !def.fields.is_empty() && def.fields.iter().all(|f| f.private) {
            let empty: Option<Vec<Expr>> = def.fields.iter().map(|f| self.empty_value(&f.ty, span)).collect();
            if let Some(values) = empty {
                let ty = Type::Struct(Named { id, name: def.name.clone() });
                expr.kind = ExprKind::Construct(values);
                return Ok(ty);
            }
        }
        if def.module != self.module && def.fields.iter().any(|f| f.private) {
            return Err(Diagnostic::error(
                span,
                format!("`{}` has private fields and can only be built in its module", def.name),
            ));
        }
        let ty = Type::Struct(Named { id, name: def.name.clone() });
        let fields: Vec<_> = def.fields.iter().map(|f| (f.name.clone(), f.ty.clone(), f.default.clone())).collect();
        let mut values = order_args(callee, &fields, std::mem::take(args), span)?;
        for (value, (_, field_ty, _)) in values.iter_mut().zip(&fields) {
            self.check_value(value, field_ty)?;
        }
        expr.kind = ExprKind::Construct(values);
        Ok(ty)
    }

    /// The value an empty field starts with: `[]`, `""`, `None`, zero, `False`, or an empty
    /// struct whose fields are all private, such as a `dict`.
    fn empty_value(&self, ty: &Type, span: Span) -> Option<Expr> {
        let kind = match ty {
            Type::Vector(_) => ExprKind::Builtin { op: crate::ast::Builtin::VecNew, args: Vec::new() },
            Type::Optional(_) => ExprKind::None,
            Type::Str => ExprKind::Str(String::new()),
            Type::Int(_) => ExprKind::Int(0),
            Type::Float(_) => ExprKind::Float(0.0),
            Type::Bool => ExprKind::Bool(false),
            Type::Struct(named) => {
                let fields = &self.tables.types.structs[named.id].fields;
                if fields.is_empty() || !fields.iter().all(|f| f.private) {
                    return None;
                }
                let values = fields.iter().map(|f| self.empty_value(&f.ty, span)).collect::<Option<_>>()?;
                ExprKind::Construct(values)
            }
            _ => return None,
        };
        let mut value = Expr::new(kind, span);
        value.ty = Some(ty.clone());
        Some(value)
    }

    /// An argument for a parameter of type `ty`; references are taken with `&x` or `&mut x`.
    pub(super) fn check_arg(&mut self, arg: &mut Expr, ty: &Type) -> Result<()> {
        // A borrow the checker made itself, such as a method's receiver, is checked already.
        if let (Some(found), ExprKind::AddrOf { .. }) = (&arg.ty, &arg.kind) {
            return super::expect_type(found, ty, arg.span);
        }
        let Type::Ref(target, mutable) = ty else { return self.check_value(arg, ty) };
        if let Type::Fn(..) = target.as_ref()
            && !mutable
        {
            return self.check_fn_arg(arg, ty);
        }
        if **target == Type::Str && !mutable && matches!(arg.kind, ExprKind::Str(_) | ExprKind::Format(_)) {
            self.check_expr(arg, None)?;
            let value = std::mem::replace(arg, Expr::new(ExprKind::None, arg.span));
            *arg = super::builtin::borrow(value, false);
            return Ok(());
        }
        if let ExprKind::Name(name) = &arg.kind
            && self.check_capture(name, arg.span).is_ok()
            && let Some(var) = self.local(name)
            && let Type::Ref(held, held_mut) = &var.ty
        {
            // Passing a reference on: same target, at most the same rights.
            if held == target && (*held_mut || !mutable) {
                arg.ty = Some(var.ty.clone());
                let id = var.id;
                if *mutable {
                    self.forget_narrowing(id);
                }
                return Ok(());
            }
        }
        // `managed` borrows for the callee.
        if self.managed && !matches!(arg.kind, ExprKind::AddrOf { .. }) {
            let span = arg.span;
            let inner = std::mem::replace(arg, Expr::new(ExprKind::None, span));
            *arg = Expr::new(ExprKind::AddrOf { mutable: *mutable, inner: Box::new(inner) }, span);
        }
        let ExprKind::AddrOf { mutable: borrowed_mut, inner } = &mut arg.kind else {
            let found = self.check_expr(arg, Some(target))?;
            let prefix = if *mutable { "&mut " } else { "&" };
            return Err(Diagnostic::error(arg.span, format!("expected `{ty}`, found `{found}`"))
                .with_hint(format!("borrow it with `{prefix}...`")));
        };
        let borrowed_mut = *borrowed_mut;
        let found = self.check_expr(inner, Some(target))?;
        // `&box` lends what the `heap` holds where that is expected.
        let found = super::managed::read_heap(inner, found, target);
        expect_type(&found, target, inner.span)?;
        let literal = super::expr::is_literal(inner)
            || matches!(inner.kind, ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Char(_));
        if !borrowed_mut && literal {
            arg.ty = Some(Type::Ref(Box::new(found), false));
            return Ok(());
        }
        // `managed` lends a temporary value for the call, then releases it; `manual` lends one
        // only when there is nothing to release, as `&(a + b)`.
        if (self.managed || self.lends_temporaries || found.is_copy())
            && !borrowed_mut
            && self.place_of(inner).is_none()
        {
            arg.ty = Some(Type::Ref(Box::new(found), false));
            return Ok(());
        }
        let place = self.place_of(inner).ok_or_else(|| {
            Diagnostic::error(inner.span, "only variables, fields, items and literals can be borrowed")
        })?;
        if borrowed_mut && !place.mutable && !self.loop_needs_mut(place.var) {
            return Err(Diagnostic::error(inner.span, format!("cannot borrow `{}` with `&mut`", place.text))
                .with_hint("its variable must be `mut`, or reached through `&mut`"));
        }
        if borrowed_mut {
            self.forget_narrowing(place.var);
        }
        if *mutable && !borrowed_mut {
            return Err(Diagnostic::error(arg.span, format!("expected `{ty}`, found `&{found}`"))
                .with_hint(format!("borrow it with `&mut {}`", place.text)));
        }
        arg.ty = Some(Type::Ref(Box::new(found), borrowed_mut));
        Ok(())
    }

    pub(super) fn check_variadic_arg(&mut self, arg: &mut Expr) -> Result<()> {
        // `None` is C's NULL, as at the end of `gtk_dialog_new_with_buttons(..., None)`.
        if matches!(arg.kind, ExprKind::None) {
            let null = Type::Optional(Box::new(Type::Ptr(Box::new(Type::Int(IntTy::U8)))));
            return self.check_value(arg, &null);
        }
        let ty = self.check_expr(arg, None)?;
        // A plain function is a C function pointer, as libcurl's CURLOPT_WRITEFUNCTION takes.
        if !(ty.is_numeric() || matches!(ty, Type::Bool | Type::Char | Type::Ptr(_) | Type::Fn(..)))
            && !matches!(&ty, Type::Optional(inner) if matches!(**inner, Type::Ptr(_)))
        {
            return Err(Diagnostic::error(
                arg.span,
                format!("a `{ty}` value cannot be passed to a variadic C function"),
            )
            .with_hint("pass string literals with `\"...\".to_c()`"));
        }
        Ok(())
    }

    /// Rewrites `T(x)` into a conversion and checks it.
    fn check_conversion(&mut self, expr: &mut Expr, target: Type) -> Result<Type> {
        let ExprKind::Call { args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        let mut args = std::mem::take(args);
        // `i64(text, base=16)`: the base a text is read in.
        let base = match args.last().map(|a| &a.kind) {
            Some(ExprKind::Named { name, .. }) if name == "base" && args.len() == 2 => args.pop(),
            _ => None,
        };
        let [arg] = args.try_into().map_err(|_: Vec<Expr>| {
            Diagnostic::error(expr.span, format!("`{target}(...)` takes exactly one value"))
        })?;
        let original = arg.clone();
        let mut arg = Box::new(arg);
        let literal_hint = is_literal(&arg).then_some(&target);
        let source = self.check_expr(&mut arg, literal_hint)?;
        let text = match &source {
            Type::Ref(inner, _) => inner.as_ref() == &Type::Str,
            ty => *ty == Type::Str,
        };
        if let Some(base) = &base
            && !(text && matches!(target, Type::Int(_)))
        {
            return Err(Diagnostic::error(base.span, "`base=` reads a text as a whole number"));
        }
        if text && target.is_numeric() {
            return self.check_parse(expr, original, base, &target);
        }
        if !source.is_numeric() || !target.is_numeric() {
            return Err(Diagnostic::error(expr.span, format!("cannot convert `{source}` to `{target}`")));
        }
        expr.kind = ExprKind::Convert(arg);
        Ok(target)
    }

    /// `module.f(...)`, `Type.f(...)`, `x.copy()` and `x.f(...)` all become plain calls.
    pub(super) fn check_method(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Method { receiver, .. } = &mut expr.kind else { unreachable!("called on a method") };
        if let Some(module) = self.module_alias(receiver) {
            return self.check_module_call(expr, module);
        }
        if let ExprKind::Method { type_args, .. } = &expr.kind
            && !type_args.is_empty()
        {
            return Err(Diagnostic::error(span, "only functions take type arguments")
                .with_hint("methods take their types from their arguments"));
        }
        let ExprKind::Method { receiver, name, args, .. } = &mut expr.kind else {
            unreachable!("checked above")
        };
        if let Some(symbol) = self.type_path(receiver)? {
            return self.check_type_call(expr, symbol);
        }
        // `i32.checked(x)`, or `T.checked(x)` where `T` is bound to a number type.
        if let ExprKind::Name(type_name) = &receiver.kind
            && self.find_local(type_name).is_none()
            && let Some(target) = Type::scalar(type_name).or_else(|| {
                let bound = self.type_params.iter().find(|(name, _)| name == type_name);
                bound.map(|(_, ty)| ty.clone()).filter(|ty| ty.is_numeric())
            })
        {
            return self.check_cast(expr, target);
        }
        if name == "copy" && args.is_empty() {
            return self.check_explicit_copy(expr);
        }
        if matches!(&receiver.kind, ExprKind::Name(n) if n == "str" && self.find_local(n).is_none()) {
            if name == "from_any_bytes" {
                return self.check_from_any_bytes(expr);
            }
            let (kind, ty) = self.check_str_static(name, args, span)?;
            expr.kind = kind;
            return Ok(ty);
        }
        let ty = self.check_expr(receiver, None)?;
        let ty = deref_heap(receiver, ty);
        if let Type::Param(param) = &ty {
            return self.check_param_method(expr, &param.clone());
        }
        if let Type::Struct(named) = &ty {
            let id = named.id;
            return self.check_bound_call(expr, id, false);
        }
        if let Type::Enum { named, .. } = &ty {
            let id = named.id;
            return self.check_bound_call(expr, id, true);
        }
        if !super::builtin::has_builtin_method(&ty, name)
            && let Some(function) = self.extension(&ty, name)
        {
            return self.check_extension_call(expr, function);
        }
        let ExprKind::Method { receiver, name, args, .. } = &mut expr.kind else {
            unreachable!("called on a method")
        };
        let receiver = std::mem::replace(receiver.as_mut(), Expr::new(ExprKind::None, span));
        let (kind, ty) = self.check_builtin_method(receiver, &name.clone(), args, span)?;
        expr.kind = kind;
        Ok(ty)
    }

    /// `module.f(...)`: a plain call of what the module exports.
    fn check_module_call(&mut self, expr: &mut Expr, module: usize) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Method { name, type_args, args, .. } = &mut expr.kind else {
            unreachable!("called on a method")
        };
        let symbol = self.tables.export(module, name, span)?;
        let (callee, type_args, args) =
            (std::mem::take(name), std::mem::take(type_args), std::mem::take(args));
        let generic = !type_args.is_empty();
        expr.kind = ExprKind::Call { callee, type_args, args, target: None };
        match symbol {
            Symbol::Fn(index) if generic && self.tables.fns[index].generics.is_empty() => {
                Err(Diagnostic::error(span, "this function does not take type arguments"))
            }
            Symbol::Fn(index) if generic => self.check_generic_call(expr, index),
            Symbol::Fn(index) => self.check_fn_call(expr, index),
            Symbol::Builtin(op) => {
                let expected = self.expected_struct.take();
                self.check_library_call(expr, op, expected.as_ref())
            }
            Symbol::Struct(id) => self.check_construct(expr, id),
            _ => Err(Diagnostic::error(span, "only functions and types can be called")),
        }
    }

    /// `Type.f(...)` and `Enum.Variant(...)`, generic or not.
    fn check_type_call(&mut self, expr: &mut Expr, symbol: Symbol) -> Result<Type> {
        let ExprKind::Method { name, args, .. } = &mut expr.kind else { unreachable!("called on a method") };
        match symbol {
            Symbol::Struct(id) => self.check_static_call(expr, id),
            Symbol::Enum(id) => {
                let (variant, args) = (std::mem::take(name), std::mem::take(args));
                self.check_variant(expr, id, &variant, args)
            }
            Symbol::GenericEnum(template) => {
                let (variant, args) = (std::mem::take(name), std::mem::take(args));
                let expected = self.expected_struct.take();
                self.check_generic_variant(expr, template, &variant, args, expected)
            }
            Symbol::GenericStruct(template) => {
                let expected = self.expected_struct.take();
                self.check_generic_static(expr, template, expected)
            }
            _ => unreachable!("type_path gives types"),
        }
    }

    /// `x.copy()`: a full copy, refused for what owns a resource (a `sender` excepted:
    /// its copy is one more sender on the same channel).
    fn check_explicit_copy(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Method { receiver, .. } = &mut expr.kind else { unreachable!("called on a method") };
        let mut receiver = Box::new(std::mem::replace(receiver.as_mut(), Expr::new(ExprKind::None, span)));
        let ty = self.check_expr(&mut receiver, None)?;
        if matches!(ty, Type::Errors(_)) {
            return Err(Diagnostic::error(span, "a caught error cannot be copied")
                .with_hint("raise it again with `raise e`"));
        }
        if self.tables.types.has_drop_impl(&ty) && !matches!(ty, Type::Sender(_)) {
            return Err(Diagnostic::error(span, format!("`{ty}` implements `Drop` and cannot be copied"))
                .with_hint("move it, or borrow it with `&`"));
        }
        expr.kind = ExprKind::CopyOf(receiver);
        Ok(ty)
    }

    /// `Type.name(...)` on a method without `self`.
    pub(super) fn check_static_call(&mut self, expr: &mut Expr, id: usize) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Method { name, args, .. } = &mut expr.kind else { unreachable!("called on a method") };
        let index = self.method(id, name, span, false)?;
        if self.tables.fns[index].receiver.is_some() {
            return Err(Diagnostic::error(span, format!("`{name}` takes `self`; call it on a value")));
        }
        let (callee, args) = (std::mem::take(name), std::mem::take(args));
        expr.kind = ExprKind::Call { callee, type_args: Vec::new(), args, target: None };
        self.check_fn_call(expr, index)
    }

    /// A function named without being called: its C name, typed `fn(A) -> R`.
    fn check_fn_value(&mut self, expr: &mut Expr, index: usize) -> Result<Type> {
        let sig = &self.tables.fns[index];
        let ExprKind::Name(name) = &expr.kind else { unreachable!("called on a name") };
        if !sig.generics.is_empty() || !sig.raises.is_empty() || sig.variadic {
            return Err(Diagnostic::error(expr.span, format!("`{name}` cannot be used as a value yet"))
                .with_hint("only functions without type parameters, `raises` or `...` can"));
        }
        let ty = Type::Fn(sig.params.iter().map(|p| p.ty.clone()).collect(), Box::new(sig.ret.clone()));
        expr.kind = ExprKind::FnRef(sig.c_name.clone());
        Ok(ty)
    }

    /// An argument for a `&fn(...)` parameter: a lambda, which may capture, or any function.
    fn check_fn_arg(&mut self, arg: &mut Expr, ty: &Type) -> Result<()> {
        let Type::Ref(target, _) = ty else { unreachable!("a borrowed function") };
        if matches!(arg.kind, ExprKind::Lambda { .. }) {
            let found = self.check_lambda(arg, true)?;
            arg.ty = Some(found.clone());
            return expect_type(&found, ty, arg.span);
        }
        if let ExprKind::Name(name) = &arg.kind
            && self.find_local(name).is_some_and(|v| v.ty == *ty)
        {
            let name = name.clone();
            *arg = self.function_value(&name, arg.span)?;
            let ExprKind::Deref(reference) = std::mem::replace(&mut arg.kind, ExprKind::None) else {
                unreachable!("borrowed functions read through `Deref`")
            };
            *arg = *reference;
            return Ok(());
        }
        let found = self.check_expr(arg, Some(target))?;
        expect_type(&found, target, arg.span)?;
        let value = std::mem::replace(arg, Expr::new(ExprKind::None, arg.span));
        *arg = super::builtin::borrow(value, false);
        Ok(())
    }

    /// A variable holding a function, read only to call it or pass it on. A borrowed
    /// function is read through `Deref`, typed as the function.
    fn function_value(&mut self, name: &str, span: Span) -> Result<Expr> {
        self.check_capture(name, span)?;
        let var = self.local(name).expect("checked as a local");
        let (id, ty) = (var.id, var.ty.clone());
        self.check_not_moved(id, name, span)?;
        let mut value = Expr::new(ExprKind::Name(name.to_string()), span);
        value.ty = Some(ty.clone());
        if let Type::Ref(inner, _) = ty {
            let ty = Some(*inner);
            value = Expr { kind: ExprKind::Deref(Box::new(value)), span, ty };
        }
        Ok(value)
    }

    /// `f(x)` where `f` is a variable holding a function.
    fn check_value_call(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Call { callee, args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        let function = self.function_value(callee, span)?;
        let Type::Fn(params, ret) = function.ty().clone() else { unreachable!("checked as a function") };
        if args.len() != params.len() {
            return Err(Diagnostic::error(span, format!("`{callee}` takes {} argument(s)", params.len())));
        }
        for (arg, param) in args.iter_mut().zip(&params) {
            if matches!(arg.kind, ExprKind::Named { .. }) {
                return Err(Diagnostic::error(arg.span, "a function value takes no named arguments"));
            }
            self.check_arg(arg, param)?;
        }
        self.check_call_conflicts(args)?;
        let args = std::mem::take(args);
        expr.kind = ExprKind::CallValue { callee: Box::new(function), args };
        Ok(*ret)
    }

    fn is_generic_fn(&self, name: &str) -> bool {
        match self.global(name) {
            Some(Symbol::Fn(i)) => !self.tables.fns[i].generics.is_empty(),
            Some(Symbol::GenericStruct(_)) => true,
            _ => false,
        }
    }

    /// `x.name(...)`: the checked receiver becomes the first argument, borrowed as the method asks.
    fn check_bound_call(&mut self, expr: &mut Expr, id: usize, is_enum: bool) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Method { receiver, name, .. } = &mut expr.kind else {
            unreachable!("called on a method")
        };
        let index = self.method(id, name, span, is_enum)?;
        let type_name = if is_enum {
            self.tables.types.enums[id].name.clone()
        } else {
            self.tables.types.structs[id].name.clone()
        };
        if !is_enum && name == "drop" && self.tables.types.structs[id].drop_method().is_some() {
            return Err(Diagnostic::error(
                span,
                format!("`drop` runs by itself when a `{type_name}` goes away"),
            )
            .with_hint("let the value go out of scope instead"));
        }
        let receiver_kind = self.tables.fns[index].receiver.ok_or_else(|| {
            Diagnostic::error(span, format!("`{name}` has no `self`; call it as `{type_name}.{name}(...)`"))
        })?;
        // A `managed` `self` takes the mode inferred for it.
        let receiver_kind = match (receiver_kind, &self.tables.fns[index].params[0].ty) {
            (Receiver::Plain, Type::Ref(_, false)) => Receiver::Shared,
            (Receiver::Plain, Type::Ref(_, true)) => Receiver::Mut,
            (kind, _) => kind,
        };
        let receiver = std::mem::replace(receiver.as_mut(), Expr::new(ExprKind::Tuple(Vec::new()), span));
        let self_arg = self.self_argument(receiver, receiver_kind, &name.clone())?;
        let ExprKind::Method { name, args, .. } = &mut expr.kind else { unreachable!("called on a method") };
        let mut all = vec![self_arg];
        all.append(args);
        let callee = std::mem::take(name);
        expr.kind = ExprKind::Call { callee, type_args: Vec::new(), args: all, target: None };
        self.check_fn_call(expr, index)
    }

    /// The receiver of `x.name(...)` as the method's first argument: moved, or borrowed.
    pub(super) fn self_argument(&mut self, mut receiver: Expr, kind: Receiver, name: &str) -> Result<Expr> {
        Ok(match kind {
            Receiver::Owned | Receiver::Plain => {
                self.consume(&mut receiver)?;
                receiver
            }
            // A temporary, `make().total()`, is lent for the call and released after it.
            Receiver::Shared if self.place_of(&receiver).is_none() => super::builtin::borrow(receiver, false),
            Receiver::Shared | Receiver::Mut => {
                let mutable = kind == Receiver::Mut;
                let place = self.place_of(&receiver).ok_or_else(|| {
                    Diagnostic::error(
                        receiver.span,
                        format!("`{name}` borrows its value; store it in a variable first"),
                    )
                })?;
                if mutable {
                    self.forget_narrowing(place.var);
                }
                if mutable && !place.mutable && !self.loop_needs_mut(place.var) {
                    if let Some(refused) = self.outside_write(&place.text, receiver.span) {
                        return Err(refused);
                    }
                    return Err(Diagnostic::error(
                        receiver.span,
                        format!("`{name}` changes `{}`, which is not mutable", place.text),
                    )
                    .with_hint("declare the variable with `mut`"));
                }
                let (ty, span) = (Type::Ref(Box::new(receiver.ty().clone()), mutable), receiver.span);
                let mut borrow = Expr::new(ExprKind::AddrOf { mutable, inner: Box::new(receiver) }, span);
                borrow.ty = Some(ty);
                borrow
            }
        })
    }

    fn method(&self, id: usize, name: &str, span: Span, is_enum: bool) -> Result<usize> {
        let (table, type_name) = if is_enum {
            (&self.tables.enum_methods, &self.tables.types.enums[id].name)
        } else {
            (&self.tables.methods, &self.tables.types.structs[id].name)
        };
        table
            .get(&(id, name.to_string()))
            .copied()
            .ok_or_else(|| Diagnostic::error(span, format!("`{type_name}` has no method `{name}`")))
    }

    /// The type a receiver names, as `Point` or `geometry.Point`, unless a variable hides it.
    pub(super) fn type_path(&self, expr: &Expr) -> Result<Option<Symbol>> {
        let symbol = match &expr.kind {
            ExprKind::Name(name) if self.find_local(name).is_none() => self.global(name),
            ExprKind::Field { base, name } => match self.module_alias(base) {
                Some(module) => Some(self.tables.export(module, name, expr.span)?),
                None => None,
            },
            _ => None,
        };
        Ok(symbol.filter(|s| {
            matches!(
                s,
                Symbol::Struct(_) | Symbol::Enum(_) | Symbol::GenericEnum(_) | Symbol::GenericStruct(_)
            )
        }))
    }

    /// Whether a function belongs to the standard library.
    fn stdlib_fn(&self, index: usize) -> bool {
        let c_name = &self.tables.fns[index].c_name;
        self.tables.module_paths.iter().enumerate().any(|(module, path)| {
            crate::stdlib::source(path).is_some() && c_name.starts_with(&format!("hsy_m{module}_"))
        })
    }

    /// `io.stdout`: the module constant itself, which code reads in place.
    fn global_value(&self, expr: &mut Expr, index: usize) -> Type {
        let global = &self.tables.globals[index];
        expr.kind = ExprKind::Global { module: global.module, name: global.name.clone() };
        global.ty.clone()
    }

    /// `target.property` and `module.CONSTANT` are replaced by their values.
    pub(super) fn check_field(&mut self, expr: &mut Expr, expected: Option<&Type>) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Field { base, name } = &expr.kind else { unreachable!("called on a field") };
        if let Some(module) = self.module_alias(base) {
            let symbol = self.tables.export(module, name, span)?;
            if let Symbol::Global(index) = symbol {
                return Ok(self.global_value(expr, index));
            }
            let Symbol::Const(index) = symbol else {
                return Err(Diagnostic::error(span, format!("`{name}` is not a constant")));
            };
            let constant = &self.tables.consts[index];
            let ty = constant.ty.clone();
            expr.kind = literal(constant.value.clone());
            return Ok(ty);
        }
        match self.type_path(base)? {
            Some(Symbol::Enum(id)) => {
                let variant = name.clone();
                return self.check_variant(expr, id, &variant, Vec::new());
            }
            Some(Symbol::GenericEnum(template)) => {
                let variant = name.clone();
                let expected = expected.cloned();
                return self.check_generic_variant(expr, template, &variant, Vec::new(), expected);
            }
            _ => {}
        }
        if matches!(&base.kind, ExprKind::Name(b) if b == "target" && self.find_local(b).is_none()) {
            expr.kind = literal(self.eval(expr)?);
            return self.check_expr(expr, expected);
        }
        self.check_struct_field(expr)
    }

    fn check_struct_field(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Field { base, name } = &mut expr.kind else { unreachable!("called on a field") };
        let ty = self.check_expr(base, None)?;
        let ty = deref_heap(base, ty);
        // `as.Case.field`, which `field(x, f)` makes in a `comptime for` over the cases of an
        // enum: a field of the case that `x` is.
        if let (Type::Enum { named, .. }, Some((case, field))) =
            (&ty, name.strip_prefix("as.").and_then(|rest| rest.split_once('.')))
        {
            let variants = &self.tables.types.enums[named.id].variants;
            let found = variants.iter().find(|v| v.name == case).and_then(|v| v.fields.iter().find(|f| f.name == field));
            if let Some(found) = found {
                return Ok(found.ty.clone());
            }
        }
        let Type::Struct(named) = &ty else {
            return Err(Diagnostic::error(span, format!("`{ty}` has no field `{name}`")));
        };
        let def = &self.tables.types.structs[named.id];
        let field = def
            .fields
            .iter()
            .find(|f| &f.name == name)
            .ok_or_else(|| Diagnostic::error(span, format!("`{}` has no field `{name}`", def.name)))?;
        if field.private && def.module != self.module {
            return Err(Diagnostic::error(span, format!("field `{name}` of `{}` is private", def.name)));
        }
        let ty = field.ty.clone();
        if let Type::Optional(inner) = &ty
            && let Some(place) = self.place_of(expr)
            && self.narrowed.contains(&(place.var, place.text))
        {
            let inner = inner.as_ref().clone();
            let mut optional = Expr::new(std::mem::replace(&mut expr.kind, ExprKind::None), span);
            optional.ty = Some(ty);
            expr.kind = ExprKind::Unwrap(Box::new(optional));
            return Ok(inner);
        }
        Ok(ty)
    }

    /// `a[i]`: any integer index, checked against the length when the program runs.
    pub(super) fn check_index(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Index { base, index } = &mut expr.kind else { unreachable!("called on an index") };
        let original = base.as_ref().clone();
        let ty = self.check_expr(base, None)?;
        if self.has_operator(&ty, "index") {
            let index = index.as_ref().clone();
            return self.check_struct_index(expr, original, index);
        }
        let ExprKind::Index { base, index } = &mut expr.kind else { unreachable!("called on an index") };
        let ty = deref_heap(base, ty);
        if let Type::Vector(item) | Type::ArrayParam(item, _) | Type::Ptr(item) = &ty {
            if matches!(ty, Type::Ptr(_)) && self.managed {
                return Err(Diagnostic::error(span, "reading through a pointer is only for `manual` code"));
            }
            let item = item.as_ref().clone();
            let index_ty = self.check_expr(index, None)?;
            if index_ty.as_int().is_none() {
                return Err(Diagnostic::error(
                    index.span,
                    format!("an index must be an integer, found `{index_ty}`"),
                ));
            }
            return Ok(item);
        }
        if let Type::Tuple(items) = &ty {
            // `pair[0]`: the position is known when compiling, and so is the item's type.
            let ExprKind::Int(position) = index.kind else {
                return Err(Diagnostic::error(index.span, "a tuple is indexed by a number written here: `pair[0]`"));
            };
            let Some(item) = usize::try_from(position).ok().and_then(|i| items.get(i)) else {
                return Err(Diagnostic::error(
                    index.span,
                    format!("index {position} is out of bounds for `{ty}`"),
                ));
            };
            let item = item.clone();
            let base = std::mem::replace(base.as_mut(), Expr::new(ExprKind::None, span));
            expr.kind = ExprKind::Field { base: Box::new(base), name: format!("f{position}") };
            return Ok(item);
        }
        let (Type::Array(item, len) | Type::Simd(item, len)) = &ty else {
            return Err(Diagnostic::error(span, format!("`{ty}` cannot be indexed")));
        };
        let index_ty = self.check_expr(index, None)?;
        if index_ty.as_int().is_none() {
            return Err(Diagnostic::error(
                index.span,
                format!("an index must be an integer, found `{index_ty}`"),
            ));
        }
        if let ExprKind::Int(value) = index.kind
            && (value < 0 || value >= i128::from(*len))
        {
            return Err(Diagnostic::error(index.span, format!("index {value} is out of bounds for `{ty}`")));
        }
        Ok(item.as_ref().clone())
    }

    /// The module a name refers to, unless a local variable hides it.
    pub(super) fn module_alias(&self, expr: &Expr) -> Option<usize> {
        let ExprKind::Name(name) = &expr.kind else { return None };
        if self.find_local(name).is_some() {
            return None;
        }
        match self.global(name) {
            Some(Symbol::Module(module)) => Some(module),
            _ => None,
        }
    }

    /// Checks `range(...)` in a `for` loop and returns the element type.
    /// What `for` iterates: a `range`, or a vector read through `&` (`managed` borrows by
    /// itself). Returns the type of each item, and the variable of a borrowed vector.
    /// What `for` iterates, and how: a `range`; `&v`, items read; `&mut v`, items changed
    /// through `&mut`; or, in `manual`, `v` itself, its items moved into the loop. `managed`
    /// borrows by itself, with `&mut` when the body changes the items. Returns the type of each
    /// item, and the variable of a borrowed vector.
    pub(super) fn check_iterable(&mut self, iter: &mut Expr, at: Span) -> Result<(Type, Option<usize>)> {
        if matches!(&iter.kind, ExprKind::Call { callee, .. } if callee == "range") {
            return Ok((self.check_range(iter)?, None));
        }
        // `for w in ["a", "b"]`: the loop owns a vector of the items.
        if let ExprKind::ArrayLit(items) = &mut iter.kind {
            let found = self.check_array(items, None, true, iter.span)?;
            iter.ty = Some(found.clone());
            return self.iterate_owned(iter, found);
        }
        let (vector, mutable) = match &mut iter.kind {
            ExprKind::AddrOf { inner, mutable } => {
                let found = self.check_expr(inner, None)?;
                iter.ty = Some(Type::Ref(Box::new(found.clone()), *mutable));
                (found, *mutable)
            }
            _ => match self.check_expr(iter, None)? {
                // A reference held by a variable is iterated as it is.
                found if matches!(&iter.kind, ExprKind::Deref(r) if matches!(r.ty, Some(Type::Ref(..)))) => {
                    let ExprKind::Deref(reference) = std::mem::replace(&mut iter.kind, ExprKind::None) else {
                        unreachable!("matched above")
                    };
                    *iter = *reference;
                    let mutable = matches!(iter.ty(), Type::Ref(_, true));
                    (found, mutable)
                }
                // A temporary, `for w in s.split()`: the loop owns it and its items.
                found if self.place_of(iter).is_none() => return self.iterate_owned(iter, found),
                found if self.managed => {
                    let mutable = self.mut_loops.contains(&(at.line, at.col));
                    let span = iter.span;
                    let inner = std::mem::replace(iter, Expr::new(ExprKind::None, span));
                    *iter = super::builtin::borrow(inner, mutable);
                    (found, mutable)
                }
                found => return self.iterate_owned(iter, found),
            },
        };
        let Type::Vector(item) = &vector else { return Err(not_iterable(iter.span)) };
        let root = match &iter.kind {
            ExprKind::AddrOf { inner, .. } => inner.as_ref(),
            _ => &*iter,
        };
        let place = self.place_of(root).ok_or_else(|| {
            Diagnostic::error(iter.span, "only a vector in a variable, field or item can be iterated")
        })?;
        if mutable && matches!(iter.kind, ExprKind::AddrOf { .. }) {
            if !place.mutable {
                return Err(Diagnostic::error(
                    iter.span,
                    format!("cannot change the items of `{}`", place.text),
                )
                .with_hint("its variable must be `mut`, or reached through `&mut`"));
            }
            self.forget_narrowing(place.var);
        }
        let element = match (mutable, item.is_copy()) {
            (true, _) => Type::Ref(item.clone(), true),
            (false, true) => item.as_ref().clone(),
            (false, false) => Type::Ref(item.clone(), false),
        };
        Ok((element, Some(place.var)))
    }

    /// `for x in v` in `manual`: `v` is moved into the loop, and each item into `x`.
    fn iterate_owned(&mut self, iter: &mut Expr, found: Type) -> Result<(Type, Option<usize>)> {
        let Type::Vector(item) = found else {
            return Err(not_iterable(iter.span).with_hint("iterate a vector, or a `range`"));
        };
        self.consume(iter)?;
        Ok((*item, None))
    }

    /// `managed`: a loop variable is changed, so its loop iterates with `&mut`, which the
    /// next check of the function does.
    pub(super) fn loop_needs_mut(&mut self, var: usize) -> bool {
        let Some(&key) = self.loop_vars.get(&var).filter(|_| self.managed) else { return false };
        self.mut_loops.insert(key);
        self.recheck = true;
        true
    }

    /// The loop changes the vector it iterates: `managed` iterates on a copy taken first.
    pub(super) fn iterate_on_copy(&mut self, iter: &mut Expr) -> Result<()> {
        if !self.managed {
            return Err(Diagnostic::error(iter.span, "this loop changes the vector it iterates")
                .with_hint("copy it into a variable first, or use a `range` loop"));
        }
        let vector = match iter.ty() {
            Type::Ref(target, _) => target.as_ref().clone(),
            other => other.clone(),
        };
        let span = iter.span;
        self.warnings.push(Diagnostic::note(span, "this vector is copied before the loop, which changes it"));
        let value = match std::mem::replace(iter, Expr::new(ExprKind::None, span)) {
            Expr { kind: ExprKind::AddrOf { inner, .. }, .. } => *inner,
            reference => Expr { kind: ExprKind::Deref(Box::new(reference)), span, ty: Some(vector.clone()) },
        };
        *iter = Expr { kind: ExprKind::CopyOf(Box::new(value)), span, ty: Some(vector) };
        Ok(())
    }

    pub(super) fn check_range(&mut self, iter: &mut Expr) -> Result<Type> {
        let span = iter.span;
        let ExprKind::Call { callee, args, .. } = &mut iter.kind else { return Err(not_iterable(span)) };
        if callee != "range" || args.is_empty() || args.len() > 3 {
            return Err(not_iterable(span));
        }
        let element = self.check_range_args(args)?;
        let mut args = std::mem::take(args).into_iter();
        let first = args.next().expect("at least one argument");
        let (start, end) = match args.next() {
            Some(end) => (first, end),
            None => (typed_zero(span, &element), first),
        };
        let step = args.next().map(Box::new);
        if let Some(step) = &step
            && matches!(step.kind, ExprKind::Int(0))
        {
            return Err(Diagnostic::error(step.span, "the step of a `range` cannot be zero"));
        }
        iter.kind = ExprKind::Range { start: Box::new(start), end: Box::new(end), step };
        iter.ty = Some(element.clone());
        Ok(element)
    }

    /// All arguments share one integer type; literals adopt it, `i32` by default.
    fn check_range_args(&mut self, args: &mut [Expr]) -> Result<Type> {
        let anchor = args.iter().position(|arg| !is_literal(arg));
        let element = match anchor {
            Some(i) => self.check_expr(&mut args[i], None)?,
            None => Type::Int(IntTy::I32),
        };
        if element.as_int().is_none() {
            return Err(Diagnostic::error(
                args[0].span,
                format!("`range` needs integers, found `{element}`"),
            ));
        }
        for (_, arg) in args.iter_mut().enumerate().filter(|(i, _)| Some(*i) != anchor) {
            let found = self.check_expr(arg, Some(&element))?;
            expect_type(&found, &element, arg.span)?;
        }
        Ok(element)
    }
}

/// Reads through a `heap[T]`: fields, items and methods of the value it points to.
pub(super) fn deref_heap(expr: &mut Expr, ty: Type) -> Type {
    let (Type::Heap(inner) | Type::Shared(inner)) = ty else { return ty };
    let pointer = std::mem::replace(expr, Expr::new(ExprKind::None, expr.span));
    let span = pointer.span;
    *expr = Expr { kind: ExprKind::Deref(Box::new(pointer)), span, ty: Some(inner.as_ref().clone()) };
    *inner
}

/// Puts positional then named arguments into parameter order, filling in defaults.
pub(super) fn order_args(
    callee: &str,
    params: &[(String, Type, Option<Value>)],
    args: Vec<Expr>,
    span: Span,
) -> Result<Vec<Expr>> {
    let mut slots: Vec<Option<Expr>> = (0..params.len()).map(|_| None).collect();
    let split = args.iter().position(|arg| matches!(arg.kind, ExprKind::Named { .. })).unwrap_or(args.len());
    let mut args = args.into_iter();
    for (slot, arg) in args.by_ref().take(split).enumerate() {
        if slot == params.len() {
            return Err(Diagnostic::error(
                arg.span,
                format!("`{callee}` takes {} argument(s)", params.len()),
            ));
        }
        slots[slot] = Some(arg);
    }
    for arg in args {
        let ExprKind::Named { name, value } = arg.kind else {
            return Err(Diagnostic::error(arg.span, "positional arguments must come before named ones"));
        };
        let slot = params.iter().position(|(p, ..)| *p == name).ok_or_else(|| {
            Diagnostic::error(arg.span, format!("`{callee}` has no parameter named `{name}`"))
        })?;
        if slots[slot].is_some() {
            return Err(Diagnostic::error(arg.span, format!("`{name}` is given twice")));
        }
        slots[slot] = Some(*value);
    }
    slots
        .into_iter()
        .zip(params)
        .map(|(slot, (name, ty, default))| match (slot, default) {
            (Some(arg), _) => Ok(arg),
            (None, Some(value)) => {
                let mut expr = Expr::new(literal(value.clone()), span);
                if let Value::Variant { ty, .. } = value {
                    expr.ty = Some(ty.clone());
                }
                let value = expr;
                if !matches!(ty, Type::Ref(..)) {
                    return Ok(value);
                }
                Ok(Expr::new(ExprKind::AddrOf { mutable: false, inner: Box::new(value) }, span))
            }
            (None, None) => Err(Diagnostic::error(span, format!("missing argument `{name}` for `{callee}`"))),
        })
        .collect()
}

fn typed_zero(span: Span, ty: &Type) -> Expr {
    let mut zero = Expr::new(ExprKind::Int(0), span);
    zero.ty = Some(ty.clone());
    zero
}

fn not_iterable(span: Span) -> Diagnostic {
    Diagnostic::error(span, "only `range(...)` can be iterated for now")
}

/// The literal form of a compile-time value.
pub(super) fn literal(value: Value) -> ExprKind {
    match value {
        Value::Int(v) => ExprKind::Int(v),
        Value::Float(v) => ExprKind::Float(v),
        Value::Bool(v) => ExprKind::Bool(v),
        Value::Str(text) => ExprKind::Str(text),
        Value::Global { module, name } => ExprKind::Global { module, name },
        Value::Variant { index, .. } => ExprKind::Variant { index, args: Vec::new() },
        Value::None => ExprKind::None,
        Value::Type(_) => unreachable!("a type never becomes a runtime value"),
    }
}
