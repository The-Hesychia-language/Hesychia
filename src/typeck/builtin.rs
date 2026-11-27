//! The runtime's own operations: `print`, `allocate`, f-strings, and the methods of
//! `str` and `vector`.

use super::Checker;
use super::resolve::Symbol;
use crate::ast::{Builtin, Expr, ExprKind};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{IntTy, Type};

/// A method of a built-in type: its operation, whether it changes the receiver,
/// its other parameters and its result.
struct MethodSig {
    op: Builtin,
    mutates: bool,
    params: Vec<Type>,
    ret: Type,
}

impl Checker<'_> {
    /// `len(x)`, `byte_len(s)`, `allocate(value)`, `vector[T]()` and imported standard functions such
    /// as `print`, or `None` for other calls.
    pub(super) fn check_builtin_call(
        &mut self,
        expr: &mut Expr,
        expected: Option<&Type>,
    ) -> Result<Option<Type>> {
        let span = expr.span;
        let ExprKind::Call { callee, type_args, args, .. } = &mut expr.kind else {
            unreachable!("called on a call")
        };
        match self.global(callee) {
            Some(Symbol::Builtin(op)) => return self.check_library_call(expr, op, expected).map(Some),
            Some(_) => return Ok(None),
            None => {}
        }
        let (op, ty) = match (callee.as_str(), type_args.as_slice(), args.len()) {
            ("len", [], 1) => {
                let (op, arg) = self.check_len_arg(&mut args[0])?;
                expr.kind = match op {
                    Some(op) => ExprKind::Builtin { op, args: vec![arg] },
                    None => arg.kind,
                };
                return Ok(Some(Type::Int(IntTy::Size)));
            }
            ("panic", [], 1) => {
                let arg = self.check_text(&mut args[0])?;
                expr.kind = ExprKind::Builtin { op: Builtin::Panic, args: vec![arg] };
                return Ok(Some(Type::Void));
            }
            ("byte_len", [], 1) => {
                let arg = self.check_text(&mut args[0])?;
                expr.kind = ExprKind::Builtin { op: Builtin::StrBytes, args: vec![arg] };
                return Ok(Some(Type::Int(IntTy::Size)));
            }
            ("allocate", [], 1) => {
                let hint = match expected {
                    Some(Type::Heap(inner)) => Some(inner.as_ref().clone()),
                    Some(Type::Optional(inner)) => match inner.as_ref() {
                        Type::Heap(inner) => Some(inner.as_ref().clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let ty = self.check_expr(&mut args[0], hint.as_ref())?;
                self.consume(&mut args[0])?;
                (Builtin::Allocate, Type::Heap(Box::new(ty)))
            }
            ("deallocate", [], 1) => {
                let hint = expected.map(|ty| Type::Heap(Box::new(ty.clone())));
                let found = self.check_expr(&mut args[0], hint.as_ref())?;
                let Type::Heap(inner) = found else {
                    return Err(Diagnostic::error(
                        args[0].span,
                        format!("expected a `heap` value, found `{found}`"),
                    ));
                };
                self.consume(&mut args[0])?;
                (Builtin::Deallocate, *inner)
            }
            ("ptr", [], 1) => self.check_address(&mut args[0], span)?,
            ("ptr", [target], 1) => {
                let target = target.clone();
                self.check_pointer_cast(&target, &mut args[0], span)?
            }
            ("SIMD", [_, _], _) => {
                let type_args = type_args.clone();
                self.check_simd_new(&type_args, args, span)?
            }
            ("shared", [], 1) => {
                if !self.managed {
                    return Err(Diagnostic::error(span, "`shared` is only for `managed` code")
                        .with_hint("give the value one owner, and borrow it"));
                }
                let hint = match expected {
                    Some(Type::Shared(inner)) => Some(inner.as_ref().clone()),
                    _ => None,
                };
                let ty = self.check_expr(&mut args[0], hint.as_ref())?;
                self.consume(&mut args[0])?;
                (Builtin::SharedNew, Type::Shared(Box::new(ty)))
            }
            ("vector", [item], 0) => (Builtin::VecNew, Type::Vector(Box::new(self.resolve(item)?))),
            ("len" | "byte_len" | "panic" | "allocate" | "deallocate" | "shared", _, _) => {
                return Err(Diagnostic::error(span, format!("`{callee}` takes exactly one value")));
            }
            _ => return Ok(None),
        };
        expr.kind = ExprKind::Builtin { op, args: std::mem::take(args) };
        Ok(Some(ty))
    }

    /// A call to a function of the standard library, such as `io.print(text)`.
    pub(super) fn check_library_call(
        &mut self,
        expr: &mut Expr,
        op: Builtin,
        expected: Option<&Type>,
    ) -> Result<Type> {
        match op {
            Builtin::Spawn => return self.check_spawn(expr),
            Builtin::Channel => return self.check_channel(expr, expected),
            Builtin::AtomicNew => return self.check_atomic(expr, expected),
            Builtin::MutexNew => return self.check_mutex(expr, expected),
            Builtin::Print => return self.check_print(expr),
            Builtin::ScopeNew => {
                let ExprKind::Call { args, .. } = &expr.kind else { unreachable!("called on a call") };
                if !args.is_empty() {
                    return Err(Diagnostic::error(expr.span, "`scope()` takes no value"));
                }
                expr.kind = ExprKind::Builtin { op: Builtin::ScopeNew, args: Vec::new() };
                return Ok(Type::Scope);
            }
            _ => {}
        }
        let span = expr.span;
        let ExprKind::Call { callee, args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        let [arg] = args.as_mut_slice() else {
            return Err(Diagnostic::error(span, format!("`{callee}` takes exactly one value")));
        };
        let arg = self.check_text(arg)?;
        expr.kind = ExprKind::Builtin { op, args: vec![arg] };
        Ok(Type::Void)
    }

    /// `print(text, end="\n", to=stdout)`: the text, what follows it, and an `io.File` to
    /// write to, lent for the call.
    fn check_print(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Call { args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        let mut args = std::mem::take(args);
        let (mut text, mut end, mut to) = (None, None, None);
        for arg in args.drain(..) {
            match arg.kind {
                ExprKind::Named { name, value } if name == "end" && end.is_none() => end = Some(*value),
                ExprKind::Named { name, value } if name == "to" && to.is_none() => to = Some(*value),
                ExprKind::Named { name, .. } => {
                    return Err(Diagnostic::error(arg.span, format!("`print` takes no `{name}=`"))
                        .with_hint("it takes the text, then `end=` and `to=`"));
                }
                _ if text.is_none() => text = Some(arg),
                _ => {
                    return Err(Diagnostic::error(
                        arg.span,
                        "`print` takes one text; join several with an f-string",
                    ));
                }
            }
        }
        let text = text.ok_or_else(|| Diagnostic::error(span, "`print` takes the text to write"))?;
        let text = self.check_text(&mut { text })?;
        let end = end.unwrap_or_else(|| Expr::new(ExprKind::Str("\n".into()), span));
        let end = self.check_text(&mut { end })?;
        let to = match to {
            Some(to) => to,
            None => self.standard_stream("stdout", span)?,
        };
        let to = self.check_stream(to)?;
        expr.kind = ExprKind::Builtin { op: Builtin::Print, args: vec![text, end, to] };
        Ok(Type::Void)
    }

    /// An `io.File` to write to: lent, as a module constant such as `stdout` or a variable.
    fn check_stream(&mut self, mut to: Expr) -> Result<Expr> {
        let ty = match &to.ty {
            Some(ty) => ty.clone(),
            None => self.check_expr(&mut to, None)?,
        };
        let file = match &ty {
            Type::Ref(inner, _) => inner.as_ref().clone(),
            ty => ty.clone(),
        };
        let is_file = matches!(&file, Type::Struct(named)
            if named.name == "File" && self.tables.module_paths[self.tables.types.structs[named.id].module] == "io");
        if !is_file {
            return Err(Diagnostic::error(to.span, format!("`to=` takes an `io.File`, found `{ty}`"))
                .with_hint("such as `stdout`, `stderr`, or a file from `fs.open`"));
        }
        if matches!(ty, Type::Ref(..)) {
            return Ok(to);
        }
        if self.place_of(&to).is_none() {
            return Err(Diagnostic::error(to.span, "`to=` takes a file kept in a variable"));
        }
        Ok(borrow(to, false))
    }

    /// `io.stdout` and the like, imported or not.
    fn standard_stream(&self, name: &str, span: crate::diag::Span) -> Result<Expr> {
        let global =
            self.tables.globals.iter().find(|g| g.name == name && self.tables.module_paths[g.module] == "io");
        let global = global.ok_or_else(|| Diagnostic::error(span, format!("`io.{name}` is missing")))?;
        let mut stream =
            Expr::new(ExprKind::Global { module: global.module, name: global.name.clone() }, span);
        stream.ty = Some(global.ty.clone());
        Ok(stream)
    }

    /// `len(s)` counts the characters of a `str`, `len(v)` the items of a `vector`.
    /// The operation for `len`, and its argument; no operation for a struct's `len()`, the
    /// call itself.
    fn check_len_arg(&mut self, arg: &mut Expr) -> Result<(Option<Builtin>, Expr)> {
        let original = arg.clone();
        let ty = self.check_expr(arg, None)?;
        if self.has_operator(&ty, "len") {
            return Ok((None, self.struct_len(original, arg.span)?));
        }
        let ty = super::call::deref_heap(arg, ty);
        match ty {
            Type::Str => Ok((Some(Builtin::StrLen), self.read_only(arg))),
            Type::Vector(_) => {
                self.place_of(arg).ok_or_else(|| {
                    Diagnostic::error(arg.span, "`len` borrows its value; store it in a variable first")
                })?;
                let value = std::mem::replace(arg, Expr::new(ExprKind::None, arg.span));
                Ok((Some(Builtin::VecLen), borrow(value, false)))
            }
            _ => Err(Diagnostic::error(arg.span, format!("a `{ty}` value has no length"))),
        }
    }

    /// A `str` operand that is only read: a place is borrowed, a temporary is kept as is.
    fn check_text(&mut self, arg: &mut Expr) -> Result<Expr> {
        let ty = self.check_expr(arg, Some(&Type::Str))?;
        if ty != Type::Str {
            return Err(Diagnostic::error(arg.span, format!("expected a `str`, found `{ty}`"))
                .with_hint("build one with an f-string: `f\"{value}\"`"));
        }
        Ok(self.read_only(arg))
    }

    /// A checked operand that is only read: a place is borrowed, a temporary is kept as is.
    fn read_only(&mut self, arg: &mut Expr) -> Expr {
        let arg = std::mem::replace(arg, Expr::new(ExprKind::None, arg.span));
        match self.place_of(&arg) {
            Some(_) => borrow(arg, false),
            None => arg,
        }
    }

    /// `f"total: {n}"`: every value must be a number, `bool`, `char` or `str`.
    pub(super) fn check_format(&mut self, parts: &mut [Expr]) -> Result<Type> {
        for part in parts {
            let ty = self.check_expr(part, None)?;
            if !self.tables.types.writable(ty.deref_ref()) {
                return Err(Diagnostic::error(
                    part.span,
                    format!("a `{ty}` value cannot be written in an f-string"),
                )
                .with_hint("numbers, text, structs, enums, tuples, lists and `Optional` values can"));
            }
        }
        Ok(Type::Str)
    }

    /// `str.from_c(p)`, the one static method of a built-in type.
    pub(super) fn check_str_static(
        &mut self,
        name: &str,
        args: &mut Vec<Expr>,
        span: Span,
    ) -> Result<(ExprKind, Type)> {
        let byte_ptr = Type::Ptr(Box::new(Type::Int(IntTy::U8)));
        if name == "from_bytes" && args.len() == 1 {
            let bytes = Type::Vector(Box::new(Type::Int(IntTy::U8)));
            // `&v` is written in `manual`, `managed` borrows `v` by itself.
            let mut arg = match args.remove(0) {
                Expr { kind: ExprKind::AddrOf { mutable: false, inner }, .. } => *inner,
                arg => arg,
            };
            let found = self.check_expr(&mut arg, None)?;
            let arg = match found {
                Type::Ref(inner, _) if *inner == bytes => arg,
                ty if ty == bytes && self.place_of(&arg).is_some() => borrow(arg, false),
                ty if ty == bytes => {
                    return Err(Diagnostic::error(
                        arg.span,
                        "`str.from_bytes` borrows its bytes, and these are a temporary value",
                    )
                    .with_hint("store them in a variable first, then pass the variable: `data = ...`, `str.from_bytes(data)`"));
                }
                ty => {
                    return Err(Diagnostic::error(
                        arg.span,
                        format!("expected a `vector[u8]`, found `{ty}`"),
                    )
                    .with_hint("`from_bytes` borrows a vector stored in a variable"));
                }
            };
            let ty = Type::Optional(Box::new(Type::Str));
            return Ok((ExprKind::Builtin { op: Builtin::StrFromBytes, args: vec![arg] }, ty));
        }
        if name != "from_c" || args.len() != 1 {
            return Err(Diagnostic::error(
                span,
                format!("`str` has no static method `{name}` taking these values"),
            ));
        }
        self.check_value(&mut args[0], &byte_ptr)?;
        Ok((ExprKind::Builtin { op: Builtin::StrFromC, args: std::mem::take(args) }, Type::Str))
    }

    /// `x.len()`, `v.push(x)`, `"...".to_c()` and the other methods of `str` and `vector`.
    pub(super) fn check_builtin_method(
        &mut self,
        receiver: Expr,
        name: &str,
        args: &mut [Expr],
        span: Span,
    ) -> Result<(ExprKind, Type)> {
        let ty = receiver.ty().clone();
        if matches!(ty, Type::Ptr(_)) {
            return self.check_volatile(receiver, name, args, span);
        }
        if ty == Type::Scope {
            return self.check_scoped_spawn(receiver, name, args, span);
        }
        if let Type::Atomic(item) = &ty {
            return self.check_atomic_method(receiver, item, name, args, span);
        }
        if matches!(ty, Type::Sender(_) | Type::Receiver(_)) {
            return self.check_channel_method(receiver, name, args, span);
        }
        if let Type::Thread(result) = &ty
            && name == "join"
            && args.is_empty()
        {
            let mut receiver = receiver;
            self.consume(&mut receiver)?;
            return Ok((
                ExprKind::Builtin { op: Builtin::ThreadJoin, args: vec![receiver] },
                result.as_ref().clone(),
            ));
        }
        let sig = method_sig(&ty, name)
            .ok_or_else(|| Diagnostic::error(span, format!("`{ty}` has no method `{name}`")))?;
        if args.len() != sig.params.len() {
            return Err(Diagnostic::error(
                span,
                format!("`{name}` takes {} value(s), {} given", sig.params.len(), args.len()),
            ));
        }
        let receiver = match (&receiver.kind, sig.op) {
            // A literal's characters live in the program itself.
            (ExprKind::Str(_), Builtin::StrToC | Builtin::StrLen) => receiver,
            (ExprKind::Str(_), Builtin::StrByte) => borrow(receiver, false),
            // A number owns nothing: a temporary one, `(a ^ b).wrapping_mul(c)`, is lent too.
            _ if !sig.mutates && receiver.ty().is_numeric() && self.place_of(&receiver).is_none() => {
                borrow(receiver, false)
            }
            _ => {
                let place = self.place_of(&receiver).ok_or_else(|| {
                    Diagnostic::error(
                        receiver.span,
                        format!("`{name}` borrows its value; store it in a variable first"),
                    )
                })?;
                if sig.mutates && !place.mutable && !self.loop_needs_mut(place.var) {
                    if let Some(refused) = self.outside_write(&place.text, receiver.span) {
                        return Err(refused);
                    }
                    return Err(Diagnostic::error(
                        receiver.span,
                        format!("`{name}` changes `{}`, which is not mutable", place.text),
                    )
                    .with_hint("declare the variable with `mut`"));
                }
                if sig.mutates {
                    self.forget_narrowing(place.var);
                }
                borrow(receiver, sig.mutates)
            }
        };
        let mut all = vec![receiver];
        for (arg, param) in args.iter_mut().zip(&sig.params) {
            self.check_value(arg, param)?;
            all.push(std::mem::replace(arg, Expr::new(ExprKind::None, arg.span)));
        }
        Ok((ExprKind::Builtin { op: sig.op, args: all }, sig.ret))
    }
}

/// Whether the compiler itself gives `ty` the method `name`.
pub(super) fn has_builtin_method(ty: &Type, name: &str) -> bool {
    method_sig(ty, name).is_some()
}

fn method_sig(ty: &Type, name: &str) -> Option<MethodSig> {
    let size = Type::Int(IntTy::Size);
    let sig = |op, mutates, params, ret| Some(MethodSig { op, mutates, params, ret });
    match (ty, name) {
        (Type::Int(_), "wrapping_add" | "wrapping_sub" | "wrapping_mul") => {
            let op = match name {
                "wrapping_add" => Builtin::WrappingAdd,
                "wrapping_sub" => Builtin::WrappingSub,
                _ => Builtin::WrappingMul,
            };
            sig(op, false, vec![ty.clone()], ty.clone())
        }
        // `str.hash` is written in Hesychia (`extend str` in the prelude); only the scalar
        // hashes stay builtins, since they just mix the value's bits.
        (Type::Int(_) | Type::Bool | Type::Char, "hash") => {
            sig(Builtin::Hash, false, vec![], Type::Int(IntTy::U64))
        }
        (Type::Str, "byte") => sig(Builtin::StrByte, false, vec![size.clone()], Type::Int(IntTy::U8)),
        (Type::Str, "to_c") => sig(Builtin::StrToC, false, vec![], Type::Ptr(Box::new(Type::Int(IntTy::U8)))),
        (Type::Vector(item), _) => {
            let item = item.as_ref().clone();
            match name {
                "push" => sig(Builtin::VecPush, true, vec![item], Type::Void),
                "pop" => sig(Builtin::VecPop, true, vec![], Type::Optional(Box::new(item))),
                "insert" => sig(Builtin::VecInsert, true, vec![size, item], Type::Void),
                "remove" => sig(Builtin::VecRemove, true, vec![size], item),
                "clear" => sig(Builtin::VecClear, true, vec![], Type::Void),
                "swap" => sig(Builtin::VecSwap, true, vec![size.clone(), size], Type::Void),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `&value` or `&mut value`, already typed.
pub(super) fn borrow(value: Expr, mutable: bool) -> Expr {
    let ty = Type::Ref(Box::new(value.ty().clone()), mutable);
    let span = value.span;
    Expr { kind: ExprKind::AddrOf { mutable, inner: Box::new(value) }, span, ty: Some(ty) }
}
