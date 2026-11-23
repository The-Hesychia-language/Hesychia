//! Expression checking.

use super::{Checker, expect_type};
use crate::ast::{BinOp, Expr, ExprKind, UnOp};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{FloatTy, IntTy, Type};

const INT_DIVISION_WARNING: &str = "integer division truncates the decimal part";
const INT_DIVISION_HINT: &str = "use floats to keep it, or `//` to truncate explicitly";

impl Checker<'_> {
    /// Checks `expr`, records its type, and returns it. `expected` guides literals.
    pub(super) fn check_expr(&mut self, expr: &mut Expr, expected: Option<&Type>) -> Result<Type> {
        fold_negative_literal(expr);
        let ty = self.infer(expr, expected)?;
        expr.ty = Some(ty.clone());
        Ok(ty)
    }

    fn infer(&mut self, expr: &mut Expr, expected: Option<&Type>) -> Result<Type> {
        let span = expr.span;
        if matches!(expr.kind, ExprKind::Formatted { .. }) {
            return self.check_formatted(expr);
        }
        match &mut expr.kind {
            ExprKind::Formatted { .. } => unreachable!("checked above"),
            ExprKind::Int(value) => match literal_hint(expected) {
                // In `managed`, a literal too large for `i32` is an `i64`.
                None if self.managed && !IntTy::I32.fits(*value) => {
                    int_literal(*value, Some(&Type::Int(IntTy::I64)), span)
                }
                hint => int_literal(*value, hint, span),
            },
            ExprKind::Float(_) => Ok(match literal_hint(expected) {
                Some(ty @ Type::Float(_)) => ty.clone(),
                _ => Type::Float(FloatTy::F64),
            }),
            ExprKind::Bool(_) => Ok(Type::Bool),
            ExprKind::Char(_) => Ok(Type::Char),
            ExprKind::Str(_) => Ok(Type::Str),
            ExprKind::Name(_) => self.check_name(expr),
            ExprKind::Unary(op, operand) => self.check_unary(*op, operand, expected, span),
            ExprKind::Binary(BinOp::In | BinOp::NotIn, ..) => self.check_in(expr),
            ExprKind::Binary(op, left, right) => self.check_binary(*op, left, right, expected, span),
            ExprKind::Slice { .. } => self.check_slice(expr),
            // Made by the checker for a module constant, then checked again inside a copy.
            ExprKind::Global { module, name } => {
                let found = self.tables.globals.iter().find(|g| g.module == *module && g.name == *name);
                Ok(found.expect("a known constant").ty.clone())
            }
            ExprKind::Call { .. } => {
                self.expected_struct = expected.cloned();
                let ty = self.check_call(expr)?;
                self.implicit_check(expr, ty)
            }
            ExprKind::Method { .. } => {
                self.expected_struct = expected.cloned();
                let ty = self.check_method(expr)?;
                self.implicit_check(expr, ty)
            }
            ExprKind::Field { .. } => self.check_field(expr, expected),
            ExprKind::Index { .. } => self.check_index(expr),
            ExprKind::Tuple(items) => self.check_tuple(items, expected),
            ExprKind::ArrayLit(items) => self.check_array(items, expected, false, span),
            ExprKind::Format(parts) => self.check_format(parts),
            ExprKind::Check { .. } => self.check_check(expr),
            ExprKind::Comprehension { .. } => self.check_comprehension(expr),
            ExprKind::Lambda { .. } => self.check_lambda(expr, false),
            ExprKind::None => match expected {
                Some(ty @ Type::Optional(_)) => Ok(ty.clone()),
                _ => Err(Diagnostic::error(span, "`None` needs an `Optional` type here")
                    .with_hint("declare the variable as `Optional[T]`")),
            },
            ExprKind::Named { name, .. } => {
                Err(Diagnostic::error(span, format!("`{name}=` can only name an argument of a call")))
            }
            ExprKind::AddrOf { .. } => {
                Err(Diagnostic::error(span, "a reference can only be passed directly to a function")
                    .with_hint("references cannot be stored in variables"))
            }
            // Built by the checker itself, already typed.
            ExprKind::Range { .. }
            | ExprKind::Convert(_)
            | ExprKind::Deref(_)
            | ExprKind::Construct(_)
            | ExprKind::CopyOf(_)
            | ExprKind::Cast(..)
            | ExprKind::Variant { .. }
            | ExprKind::Wrap(_)
            | ExprKind::Unwrap(_)
            | ExprKind::Move { .. }
            | ExprKind::FnRef(_)
            | ExprKind::CallValue { .. }
            | ExprKind::Builtin { .. } => Ok(expr.ty.clone().expect("built with a type")),
        }
    }

    fn check_tuple(&mut self, items: &mut [Expr], expected: Option<&Type>) -> Result<Type> {
        let hints = match expected {
            Some(Type::Tuple(types)) if types.len() == items.len() => types.clone(),
            _ => Vec::new(),
        };
        let mut types = Vec::new();
        for (i, item) in items.iter_mut().enumerate() {
            let ty = self.check_expr(item, hints.get(i))?;
            self.consume(item)?;
            types.push(ty);
        }
        Ok(Type::Tuple(types))
    }

    /// `[a, b, c]`: all items share the first one's type, or the expected array's.
    /// `[a, b]`: an array, or a vector in `managed`, when one is expected, or with `vector`.
    pub(super) fn check_array(
        &mut self,
        items: &mut [Expr],
        expected: Option<&Type>,
        vector: bool,
        span: Span,
    ) -> Result<Type> {
        let mut item_ty = match expected {
            Some(Type::Array(item, len)) => {
                if *len != items.len() as u64 {
                    return Err(Diagnostic::error(
                        span,
                        format!("expected {len} items, found {}", items.len()),
                    ));
                }
                Some(item.as_ref().clone())
            }
            Some(Type::Vector(item)) => Some(item.as_ref().clone()),
            // In a generic body the length is not known yet; each specialization checks it.
            Some(Type::ArrayParam(item, _)) => Some(item.as_ref().clone()),
            _ => None,
        };
        // In `managed`, a list is a vector unless an array is asked for, as in Python.
        let vector = vector
            || matches!(expected, Some(Type::Vector(_)))
            || (self.managed && !matches!(expected, Some(Type::Array(..) | Type::ArrayParam(..))));
        for item in items.iter_mut() {
            let ty = self.check_expr(item, item_ty.as_ref())?;
            match &item_ty {
                Some(expected) => expect_type(&ty, expected, item.span)?,
                None => item_ty = Some(ty),
            }
            self.consume(item)?;
        }
        let item_ty =
            item_ty.ok_or_else(|| Diagnostic::error(span, "the type of an empty array must be written"))?;
        if vector {
            return Ok(Type::Vector(Box::new(item_ty)));
        }
        if let Some(Type::ArrayParam(_, len)) = expected {
            return Ok(Type::ArrayParam(Box::new(item_ty), len.clone()));
        }
        Ok(Type::Array(Box::new(item_ty), items.len() as u64))
    }

    fn check_unary(
        &mut self,
        op: UnOp,
        operand: &mut Expr,
        expected: Option<&Type>,
        span: Span,
    ) -> Result<Type> {
        let ty = self.check_expr(operand, expected)?;
        let valid = match op {
            UnOp::Neg => matches!(ty, Type::Float(_)) || ty.as_int().is_some_and(IntTy::signed),
            UnOp::Not => ty == Type::Bool,
            UnOp::BitNot => ty.as_int().is_some(),
        };
        if !valid {
            return Err(Diagnostic::error(span, format!("this operator cannot be applied to `{ty}`")));
        }
        Ok(ty)
    }

    fn check_binary(
        &mut self,
        op: BinOp,
        left: &mut Expr,
        right: &mut Expr,
        expected: Option<&Type>,
        span: Span,
    ) -> Result<Type> {
        match op {
            BinOp::And | BinOp::Or => {
                // `x is not None and x > 0`: the right side sees `x` narrowed.
                let proven = self.narrowing(left, op == BinOp::And);
                let ty = self.check_expr(left, Some(&Type::Bool))?;
                expect_type(&ty, &Type::Bool, left.span)?;
                let outer = self.narrowed.clone();
                self.narrowed.extend(proven);
                let result = self.check_expr(right, Some(&Type::Bool));
                self.narrowed = outer;
                expect_type(&result?, &Type::Bool, right.span)?;
                Ok(Type::Bool)
            }
            BinOp::Shl | BinOp::Shr => self.check_shift(left, right, expected),
            BinOp::Is | BinOp::IsNot => self.check_none_test(left, right, span),
            _ if op.is_comparison() => {
                let ty = self.check_pair(left, right, None, None)?;
                match &ty {
                    Type::Param(name) => self.check_param_operator(op, name, span)?,
                    Type::Struct(_) | Type::Enum { .. } if !matches!(op, BinOp::Eq | BinOp::Ne) => {
                        if !self.has_operator(&ty, "compare") {
                            return Err(Diagnostic::error(span, format!("`{ty}` does not implement `Order`, needed by `{}`", op.symbol()))
                                .with_hint(format!("write `(Order)` on the type and define `method compare(&self, other: &{ty}) -> i32`")));
                        }
                    }
                    _ => check_comparable(op, &ty, span)?,
                }
                Ok(Type::Bool)
            }
            _ => {
                let ty = self.check_pair(left, right, expected, Some(op))?;
                if matches!(ty, Type::Ptr(_) | Type::Int(IntTy::Offset))
                    && matches!(left.ty, Some(Type::Ptr(_)))
                {
                    return Ok(ty);
                }
                self.check_arithmetic(op, &ty, span)?;
                Ok(ty)
            }
        }
    }

    /// `target op= value`, where the target type is already known.
    pub(super) fn check_compound(
        &mut self,
        op: BinOp,
        target: &Type,
        value: &mut Expr,
        span: Span,
    ) -> Result<Type> {
        let found = if matches!(op, BinOp::Shl | BinOp::Shr) {
            self.check_shift_amount(value)?;
            target.clone()
        } else {
            let found = self.check_expr(value, Some(target))?;
            if let Some(key) = self.assigning
                && self.widen_variable(key, target, &found, value.span)?
            {
                return Ok(target.clone());
            }
            if !self.coerce(value, &found, target) {
                expect_type(&found, target, value.span)?;
            }
            self.check_arithmetic(op, target, span)?;
            target.clone()
        };
        Ok(found)
    }

    /// Checks two operands that must share a type; a literal adopts the other side's type.
    fn check_pair(
        &mut self,
        left: &mut Expr,
        right: &mut Expr,
        expected: Option<&Type>,
        arithmetic: Option<BinOp>,
    ) -> Result<Type> {
        let (lt, rt) = if is_literal(left) && !is_literal(right) {
            let rt = self.check_expr(right, expected)?;
            (self.check_literal_beside(left, &rt)?, rt)
        } else {
            let lt = self.check_expr(left, expected)?;
            if let (Type::Ptr(_), Some(op)) = (&lt, arithmetic) {
                return self.check_pointer_arithmetic(op, lt, right);
            }
            let rt = if is_literal(right) {
                self.check_literal_beside(right, &lt)?
            } else {
                self.check_expr(right, Some(&lt))?
            };
            (lt.clone(), rt)
        };
        if lt != rt && self.managed && lt.is_numeric() && rt.is_numeric() {
            return Self::coerce_pair(left, right, &lt, &rt, right.span);
        }
        if lt != rt {
            let error = Diagnostic::error(right.span, format!("mismatched types `{lt}` and `{rt}`"));
            let hint = if matches!(lt, Type::Optional(_)) || matches!(rt, Type::Optional(_)) {
                "check the value first with `if x is not None:`".to_string()
            } else {
                format!("convert one side explicitly with `{lt}(...)` or `{rt}(...)`")
            };
            return Err(error.with_hint(hint));
        }
        Ok(lt)
    }

    /// A literal takes the type of the other operand; in `managed`, a literal too large for
    /// it keeps its own type, and the two are widened together.
    fn check_literal_beside(&mut self, literal: &mut Expr, other: &Type) -> Result<Type> {
        match self.check_expr(literal, Some(other)) {
            Err(_) if self.managed => self.check_expr(literal, None),
            result => result,
        }
    }

    fn check_arithmetic(&mut self, op: BinOp, ty: &Type, span: Span) -> Result<()> {
        let symbol = op.symbol();
        if let Type::Param(name) = ty {
            return self.check_param_operator(op, name, span);
        }
        if let Type::Struct(named) = ty {
            let def = &self.tables.types.structs[named.id];
            let (trait_name, method) = operator_trait(op)
                .ok_or_else(|| Diagnostic::error(span, format!("`{symbol}` does not apply to `{ty}`")))?;
            if !def.operators.contains_key(method) {
                return Err(Diagnostic::error(span, format!("`{ty}` does not implement `{trait_name}`, needed by `{symbol}`"))
                    .with_hint(format!("write `struct {}({trait_name})` and define `method {method}(&self, other: &{ty}) -> {ty}`", def.name)));
            }
            return Ok(());
        }
        if let Type::Simd(item, _) = ty {
            return match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => Ok(()),
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor if item.as_int().is_some() => Ok(()),
                _ => Err(Diagnostic::error(span, format!("`{symbol}` does not apply to `{ty}`"))
                    .with_hint("SIMD lanes take `+`, `-`, `*`, `/`, and `&`, `|`, `^` for integers")),
            };
        }
        match op {
            BinOp::Add if *ty == Type::Str => {}
            BinOp::Mod | BinOp::FloorDiv | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
                if ty.as_int().is_none() =>
            {
                return Err(Diagnostic::error(
                    span,
                    format!("`{symbol}` needs integer operands, found `{ty}`"),
                ));
            }
            _ if !ty.is_numeric() => {
                return Err(Diagnostic::error(
                    span,
                    format!("`{symbol}` needs numeric operands, found `{ty}`"),
                ));
            }
            BinOp::Div if ty.as_int().is_some() => {
                self.warnings
                    .push(Diagnostic::warning(span, INT_DIVISION_WARNING).with_hint(INT_DIVISION_HINT));
            }
            _ => {}
        }
        Ok(())
    }

    fn check_shift(&mut self, left: &mut Expr, right: &mut Expr, expected: Option<&Type>) -> Result<Type> {
        let ty = self.check_expr(left, expected)?;
        if ty.as_int().is_none() {
            return Err(Diagnostic::error(left.span, format!("shifts need an integer, found `{ty}`")));
        }
        self.check_shift_amount(right)?;
        Ok(ty)
    }

    fn check_shift_amount(&mut self, amount: &mut Expr) -> Result<()> {
        let ty = self.check_expr(amount, None)?;
        if ty.as_int().is_none() {
            return Err(Diagnostic::error(
                amount.span,
                format!("the shift amount must be an integer, found `{ty}`"),
            ));
        }
        Ok(())
    }
}

pub(super) fn int_literal(value: i128, expected: Option<&Type>, span: Span) -> Result<Type> {
    let ty = match expected {
        Some(Type::Float(f)) => return Ok(Type::Float(*f)),
        Some(Type::Int(ty)) => *ty,
        _ => IntTy::I32,
    };
    if !ty.fits(value) {
        return Err(Diagnostic::error(span, format!("the value {value} does not fit in `{}`", ty.name()))
            .with_hint("use a wider type such as `i64`"));
    }
    Ok(Type::Int(ty))
}

fn check_comparable(op: BinOp, ty: &Type, span: Span) -> Result<()> {
    let equality = matches!(op, BinOp::Eq | BinOp::Ne);
    let valid = match ty {
        Type::Int(_) | Type::Float(_) | Type::Char | Type::Str => true,
        Type::Bool | Type::Ptr(_) | Type::Enum { .. } | Type::Struct(_) => equality,
        Type::Tuple(_) | Type::Array(..) | Type::Vector(_) | Type::Optional(_) | Type::Heap(_) => equality,
        _ => false,
    };
    if !valid {
        return Err(Diagnostic::error(span, format!("`{}` cannot compare `{ty}` values", op.symbol())));
    }
    Ok(())
}

/// The built-in trait, and its method, behind an arithmetic operator.
pub(super) fn operator_trait(op: BinOp) -> Option<(&'static str, &'static str)> {
    Some(match op {
        BinOp::Add => ("Add", "add"),
        BinOp::Sub => ("Sub", "sub"),
        BinOp::Mul => ("Mul", "mul"),
        BinOp::Div => ("Div", "div"),
        BinOp::Mod => ("Mod", "mod"),
        _ => return None,
    })
}

/// Literals for an `Optional[T]` take the type `T`.
fn literal_hint(expected: Option<&Type>) -> Option<&Type> {
    match expected {
        Some(Type::Optional(inner)) => Some(inner),
        other => other,
    }
}

pub(super) fn is_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_) | ExprKind::Float(_) => true,
        ExprKind::Unary(UnOp::Neg, inner) => is_literal(inner),
        _ => false,
    }
}

/// Turns `-5` into a single literal so that `i8 = -128` type-checks.
fn fold_negative_literal(expr: &mut Expr) {
    let ExprKind::Unary(UnOp::Neg, inner) = &expr.kind else {
        return;
    };
    let folded = match inner.kind {
        ExprKind::Int(value) => ExprKind::Int(-value),
        ExprKind::Float(value) => ExprKind::Float(-value),
        _ => return,
    };
    expr.kind = folded;
}
