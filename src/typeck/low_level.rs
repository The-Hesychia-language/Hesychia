//! `manual`'s low level: conversions that are not `T(x)`, pointers, volatile accesses,
//! SIMD vectors and `asm`.

use super::Checker;
use super::expr::is_literal;
use crate::ast::{AsmKind, AsmOperand, BinOp, Builtin, CastKind, Expr, ExprKind, TypeExpr};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{IntTy, Type};

impl Checker<'_> {
    /// `T.checked(x)`, `T.truncate(x)` and `T.reinterpret(x)` between numbers.
    pub(super) fn check_cast(&mut self, expr: &mut Expr, target: Type) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Method { name, args, .. } = &mut expr.kind else { unreachable!("called on a method") };
        let kind = match name.as_str() {
            "checked" => CastKind::Checked,
            "truncate" => CastKind::Truncate,
            "reinterpret" => CastKind::Reinterpret,
            _ => {
                return Err(Diagnostic::error(span, format!("`{target}` has no function `{name}`"))
                    .with_hint(
                        "conversions are `T(x)`, `T.checked(x)`, `T.truncate(x)` and `T.reinterpret(x)`",
                    ));
            }
        };
        let [arg] = std::mem::take(args).try_into().map_err(|_: Vec<Expr>| {
            Diagnostic::error(span, format!("`{target}.{name}(...)` takes exactly one value"))
        })?;
        let mut arg = Box::new(arg);
        let source = self.check_expr(&mut arg, None)?;
        if !source.is_numeric() || !target.is_numeric() {
            return Err(Diagnostic::error(span, format!("cannot convert `{source}` to `{target}`")));
        }
        if kind == CastKind::Reinterpret
            && self.tables.types.layout(&source) != self.tables.types.layout(&target)
        {
            return Err(Diagnostic::error(
                span,
                format!("`{source}` and `{target}` do not have the same size"),
            )
            .with_hint("`reinterpret` keeps the bits, so both types must have the same size"));
        }
        expr.kind = ExprKind::Cast(arg, kind);
        Ok(if kind == CastKind::Checked { Type::Optional(Box::new(target)) } else { target })
    }

    /// `p + n` and `p - n` move a pointer by `n` items; `p2 - p1` counts the items between.
    pub(super) fn check_pointer_arithmetic(
        &mut self,
        op: BinOp,
        pointer: Type,
        right: &mut Expr,
    ) -> Result<Type> {
        let span = right.span;
        if self.managed {
            return Err(Diagnostic::error(span, "pointer arithmetic is only for `manual` code"));
        }
        let offset = Type::Int(IntTy::Offset);
        let hint = if is_literal(right) { Some(&offset) } else { None };
        let found = self.check_expr(right, hint)?;
        match (&found, op) {
            (Type::Ptr(_), BinOp::Sub) if found == pointer => Ok(offset),
            (Type::Int(_), BinOp::Add | BinOp::Sub) => Ok(pointer),
            _ => Err(Diagnostic::error(span, format!("`{pointer}` moves by an integer, found `{found}`"))
                .with_hint("`p + n` and `p - n` move by `n` items; `p2 - p1` counts the items between")),
        }
    }

    pub(super) fn manual_only(&self, what: &str, span: Span) -> Result<()> {
        if self.managed {
            return Err(Diagnostic::error(span, format!("{what} is only for `manual` code")));
        }
        Ok(())
    }

    /// `p.volatile_read()` and `p.volatile_write(x)`: never removed nor reordered.
    pub(super) fn check_volatile(
        &mut self,
        receiver: Expr,
        name: &str,
        args: &mut [Expr],
        span: Span,
    ) -> Result<(ExprKind, Type)> {
        self.manual_only(&format!("`{name}`"), span)?;
        let Type::Ptr(target) = receiver.ty().clone() else { unreachable!("called on a pointer") };
        match (name, args) {
            ("volatile_read", []) => {
                Ok((ExprKind::Builtin { op: Builtin::VolatileRead, args: vec![receiver] }, *target))
            }
            ("volatile_write", [value]) => {
                self.check_value(value, &target)?;
                let value = std::mem::replace(value, Expr::new(ExprKind::None, span));
                Ok((
                    ExprKind::Builtin { op: Builtin::VolatileWrite, args: vec![receiver, value] },
                    Type::Void,
                ))
            }
            _ => Err(Diagnostic::error(
                span,
                format!("`{}` has no method `{name}` taking these values", receiver.ty()),
            )
            .with_hint("pointers have `volatile_read()` and `volatile_write(x)`")),
        }
    }

    /// `asm`, in `manual` only: inputs are numbers or pointers; outputs are places of such types.
    pub(super) fn check_asm(&mut self, operands: &mut [AsmOperand], span: Span) -> Result<()> {
        if self.managed {
            return Err(Diagnostic::error(span, "`asm` is only for `manual` code"));
        }
        for operand in operands {
            let Some(value) = &mut operand.value else { continue };
            let ty = self.check_expr(value, None)?;
            if !(ty.is_numeric() || matches!(ty, Type::Bool | Type::Ptr(_))) {
                return Err(Diagnostic::error(
                    value.span,
                    format!("a register holds a number or a pointer, not `{ty}`"),
                ));
            }
            if operand.kind == AsmKind::In {
                continue;
            }
            let place = self.place_of(value).ok_or_else(|| {
                Diagnostic::error(operand.span, "an `out` operand is a variable, field or item")
            })?;
            if !place.mutable {
                return Err(Diagnostic::error(
                    value.span,
                    format!("`asm` writes `{}`, which is not mutable", place.text),
                )
                .with_hint("declare it with `mut`"));
            }
            self.forget_narrowing(place.var);
        }
        Ok(())
    }

    /// `ptr(&x)` and `ptr(&mut x)`: the address of a place, unchecked from then on.
    pub(super) fn check_address(&mut self, arg: &mut Expr, span: Span) -> Result<(Builtin, Type)> {
        self.manual_only("`ptr(&x)`", span)?;
        let ExprKind::AddrOf { mutable, inner } = &mut arg.kind else {
            return Err(Diagnostic::error(arg.span, "`ptr` takes the address of a place: `ptr(&x)`"));
        };
        let mutable = *mutable;
        let ty = self.check_expr(inner, None)?;
        let place = self.place_of(inner).ok_or_else(|| {
            Diagnostic::error(inner.span, "only variables, fields and items have an address")
        })?;
        if mutable && !place.mutable {
            return Err(Diagnostic::error(inner.span, format!("cannot take `&mut {}`", place.text)));
        }
        if mutable {
            self.forget_narrowing(place.var);
        }
        arg.ty = Some(Type::Ref(Box::new(ty.clone()), mutable));
        Ok((Builtin::PtrOf, Type::Ptr(Box::new(ty))))
    }

    /// `ptr[U](p)`: the same address, seen as pointing to a `U`.
    pub(super) fn check_pointer_cast(
        &mut self,
        target: &TypeExpr,
        arg: &mut Expr,
        span: Span,
    ) -> Result<(Builtin, Type)> {
        self.manual_only("`ptr[T](p)`", span)?;
        let target = self.resolve(target)?;
        let found = self.check_expr(arg, None)?;
        if !matches!(found, Type::Ptr(_)) {
            return Err(Diagnostic::error(
                arg.span,
                format!("`ptr[{target}](...)` converts a pointer, found `{found}`"),
            ));
        }
        Ok((Builtin::PtrCast, Type::Ptr(Box::new(target))))
    }

    /// `SIMD[T, N](a, b, ...)`: one value per lane, or one for all lanes.
    pub(super) fn check_simd_new(
        &mut self,
        type_args: &[TypeExpr],
        args: &mut [Expr],
        span: Span,
    ) -> Result<(Builtin, Type)> {
        let texpr = TypeExpr::Named { name: "SIMD".into(), args: type_args.to_vec(), span };
        let ty = self.resolve(&texpr)?;
        let Type::Simd(item, lanes) = &ty else { unreachable!("resolved as SIMD") };
        if args.len() != 1 && args.len() as u64 != *lanes {
            return Err(Diagnostic::error(
                span,
                format!("`{ty}(...)` takes one value for every lane, or one for all"),
            ));
        }
        for arg in args.iter_mut() {
            self.check_value(arg, item)?;
        }
        Ok((Builtin::SimdNew, ty))
    }
}
