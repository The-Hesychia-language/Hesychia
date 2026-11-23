//! The operators a struct gets from the built-in traits: `c[k]` (`Index`), `c[k] = v`
//! (`IndexSet`), `x in c` (`Contains`), `c[a:b]` (`Slice`) and `len(c)` (`Len`). Each one
//! is turned into a call of the struct's method.

use super::Checker;
use crate::ast::{BinOp, Expr, ExprKind, UnOp};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{IntTy, Type};

impl Checker<'_> {
    /// Whether values of `ty` have the operator method `method`, from a built-in trait.
    pub(super) fn has_operator(&self, ty: &Type, method: &str) -> bool {
        let ty = match ty {
            Type::Ref(inner, _) => inner.as_ref(),
            ty => ty,
        };
        match ty {
            Type::Struct(named) => self.tables.types.structs[named.id].operators.contains_key(method),
            Type::Enum { named, .. } => self.tables.types.enums[named.id].operators.contains_key(method),
            // `v[i]` and `len(v)` are the compiler's; `in` and slices come from the prelude.
            Type::Str | Type::Vector(_) => {
                matches!(method, "contains" | "slice") && self.extension(ty, method).is_some()
            }
            _ => false,
        }
    }

    /// The type of `expr`, learned on a copy so that `expr` can still be rewritten.
    fn type_of_copy(&mut self, expr: &Expr) -> Result<Type> {
        let mut copy = expr.clone();
        self.check_expr(&mut copy, None)
    }

    /// `receiver.name(args)`, checked.
    fn call_operator(&mut self, receiver: Expr, name: &str, args: Vec<Expr>, span: Span) -> Result<Expr> {
        let kind =
            ExprKind::Method { receiver: Box::new(receiver), name: name.into(), type_args: Vec::new(), args };
        let mut call = Expr::new(kind, span);
        self.check_expr(&mut call, None)?;
        Ok(call)
    }

    /// A key passed to `&K`: `managed` borrows by itself, `manual` is given `&key`.
    fn as_key(&self, key: Expr) -> Expr {
        if self.managed {
            return key;
        }
        let span = key.span;
        Expr::new(ExprKind::AddrOf { mutable: false, inner: Box::new(key) }, span)
    }

    /// `c[k]` on a struct with `Index`: `c.index(&k)[0]`, the place `index` points to.
    pub(super) fn check_struct_index(&mut self, expr: &mut Expr, base: Expr, index: Expr) -> Result<Type> {
        let call = self.call_operator(base, "index", vec![self.as_key(index)], expr.span)?;
        let Type::Ptr(item) = call.ty().clone() else { unreachable!("`Index` returns a pointer") };
        let mut zero = Expr::new(ExprKind::Int(0), expr.span);
        zero.ty = Some(Type::Int(IntTy::Size));
        expr.kind = ExprKind::Index { base: Box::new(call), index: Box::new(zero) };
        Ok(*item)
    }

    /// `c[k] = v` on a struct with `IndexSet`: `c.set_index(k, v)`. `None` otherwise.
    pub(super) fn index_set(&mut self, target: &Expr, value: &Expr) -> Result<Option<Expr>> {
        let ExprKind::Index { base, index } = &target.kind else { return Ok(None) };
        let ty = self.type_of_copy(base)?;
        if !self.has_operator(&ty, "set_index") {
            return Ok(None);
        }
        let args = vec![index.as_ref().clone(), value.clone()];
        self.call_operator(base.as_ref().clone(), "set_index", args, target.span).map(Some)
    }

    /// `x in c` is `c.contains(&x)`; `x not in c` is its negation.
    pub(super) fn check_in(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Binary(op, item, container) = &expr.kind else { unreachable!("called on `in`") };
        let ty = self.type_of_copy(container)?;
        if !self.has_operator(&ty, "contains") {
            return Err(Diagnostic::error(span, format!("`in` does not apply to `{ty}`"))
                .with_hint("a struct gets `in` from the trait `Contains`"));
        }
        let negate = *op == BinOp::NotIn;
        let args = vec![self.as_key(item.as_ref().clone())];
        let call = self.call_operator(container.as_ref().clone(), "contains", args, span)?;
        expr.kind = if negate { ExprKind::Unary(UnOp::Not, Box::new(call)) } else { call.kind };
        Ok(Type::Bool)
    }

    /// `c[a:b]` is `c.slice(a, b)`; a start left out is 0, an end left out is the largest
    /// `size`, which `slice` clamps to the length.
    pub(super) fn check_slice(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Slice { base, start, end } = &expr.kind else { unreachable!("called on a slice") };
        let ty = self.type_of_copy(base)?;
        if !self.has_operator(&ty, "slice") {
            return Err(Diagnostic::error(span, format!("`{ty}` cannot be sliced"))
                .with_hint("a struct gets `c[a:b]` from the trait `Slice`"));
        }
        let bound = |value: &Option<Box<Expr>>, default: i128| {
            value.as_deref().cloned().unwrap_or_else(|| Expr::new(ExprKind::Int(default), span))
        };
        let args = vec![bound(start, 0), bound(end, i128::from(u64::MAX))];
        let call = self.call_operator(base.as_ref().clone(), "slice", args, span)?;
        let ty = call.ty().clone();
        *expr = call;
        Ok(ty)
    }

    /// `len(c)` on a struct with `Len`: `c.len()`.
    pub(super) fn struct_len(&mut self, arg: Expr, span: Span) -> Result<Expr> {
        self.call_operator(arg, "len", Vec::new(), span)
    }
}

impl Checker<'_> {
    /// A function of the prelude, private ones included.
    pub(super) fn prelude_fn(&self, name: &str) -> Option<usize> {
        let module = self.tables.module_paths.iter().position(|path| path == crate::stdlib::PRELUDE)?;
        match self.tables.lookup(module, name) {
            Some(super::resolve::Symbol::Fn(index)) => Some(index),
            _ => None,
        }
    }

    /// `i64(s)` on a `str`: the prelude's `parse_i64(s)`, which raises `ParseError`.
    pub(super) fn check_parse(&mut self, expr: &mut Expr, text: Expr, target: &Type) -> Result<Type> {
        let callee = format!("parse_{target}");
        let index = self
            .prelude_fn(&callee)
            .ok_or_else(|| Diagnostic::error(expr.span, format!("cannot read a `{target}` from a `str`")))?;
        let args = vec![self.as_key(text)];
        // Errors name the call as written; the C name comes from the function itself.
        let callee = target.to_string();
        expr.kind = ExprKind::Call { callee, type_args: Vec::new(), args, target: None };
        self.check_fn_call(expr, index)
    }

    /// `str(x)` writes `x` as `f"{x}"` does.
    pub(super) fn check_to_str(&mut self, expr: &mut Expr) -> Result<Type> {
        let ExprKind::Call { args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        expr.kind = ExprKind::Format(std::mem::take(args));
        self.check_expr(expr, None)
    }

    /// `{x:>8.2f}`: the format is checked against the type of `x`, then the prelude's
    /// `formatted` writes it.
    pub(super) fn check_formatted(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Formatted { value, spec } = &mut expr.kind else { unreachable!("called on a format") };
        let (value, spec) =
            (std::mem::replace(value.as_mut(), Expr::new(ExprKind::None, span)), spec.clone());
        let found = self.probe(&mut value.clone(), &Type::Void)?;
        let found = found.deref_ref();
        let (integer, float) = (matches!(found, Type::Int(_)), matches!(found, Type::Float(_)));
        let refuse = |message: String| Err(Diagnostic::error(span, message));
        match spec.kind {
            Some(kind @ ('d' | 'x' | 'X' | 'b' | 'o')) if !integer => {
                return refuse(format!("the format `{kind}` writes an integer, not a `{found}`"));
            }
            Some(kind @ ('f' | 'e' | '%')) if !integer && !float => {
                return refuse(format!("the format `{kind}` writes a number, not a `{found}`"));
            }
            _ => {}
        }
        let as_float = float || matches!(spec.kind, Some('f' | 'e' | '%'));
        if spec.precision.is_some() && !as_float {
            return refuse(format!("a number of digits after `.` is for floats, not a `{found}`"));
        }
        if (spec.plus || spec.zero || spec.comma) && !integer && !float {
            return refuse(format!("`+`, `0` and `,` are for numbers, not a `{found}`"));
        }
        if spec.comma && matches!(spec.kind, Some('x' | 'X' | 'b' | 'o' | 'e')) {
            return refuse("`,` groups the digits of `d`, `f` and `%` only".into());
        }
        let index = self.prelude_fn("formatted").expect("the prelude has `formatted`");
        let literal = |kind| Expr::new(kind, span);
        let args = vec![
            self.as_key(value),
            literal(ExprKind::Char(spec.fill)),
            literal(ExprKind::Char(spec.align.unwrap_or(' '))),
            literal(ExprKind::Bool(spec.plus)),
            literal(ExprKind::Bool(spec.zero)),
            literal(ExprKind::Int(i128::from(spec.width))),
            literal(ExprKind::Bool(spec.comma)),
            literal(ExprKind::Int(spec.precision.map_or(-1, i128::from))),
            literal(ExprKind::Char(spec.kind.unwrap_or(' '))),
        ];
        expr.kind = ExprKind::Call { callee: "formatted".into(), type_args: Vec::new(), args, target: None };
        let outer = std::mem::replace(&mut self.lends_temporaries, true);
        let result = self.check_generic_call(expr, index);
        self.lends_temporaries = outer;
        result
    }
}
