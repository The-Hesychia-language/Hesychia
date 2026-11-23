//! Compile-time evaluation for `comptime` constants, `comptime if` and `comptime assert`.

use crate::ast::{BinOp, Expr, ExprKind, TypeExpr, UnOp};
use crate::diag::{Diagnostic, Result, Span};
use crate::platform::Platform;
use crate::types::Type;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    /// A module constant of the standard library, as a parameter's default: `to=stdout`. Only
    /// a default may be one; it is lent to the call.
    Global {
        module: usize,
        name: String,
    },
    /// A variant without fields of an enum: `Side.Start`.
    Variant {
        ty: Type,
        index: usize,
    },
    /// A type, in `comptime` code: `R == dict`, `return vector[str]`.
    Type(Type),
    /// `None`, as the default of an `Optional` parameter.
    None,
}

/// What `target.*` reports: the machine the program is built for.
#[derive(Clone)]
pub struct Target {
    pub platform: Platform,
    pub debug: bool,
    /// `managed` locals larger than this, in bytes, live on the heap.
    pub heap_threshold: u64,
}

/// The default of `[settings] heap_threshold`: 64 KiB.
pub const HEAP_THRESHOLD: u64 = 64 * 1024;

impl Target {
    fn property(&self, name: &str, span: Span) -> Result<Value> {
        // Every supported target is 64-bit and little-endian.
        Ok(match name {
            "os" => Value::Str(self.platform.os.clone()),
            "arch" => Value::Str(self.platform.arch.clone()),
            "env" => Value::Str(self.platform.env.clone()),
            "pointer_bits" => Value::Int(64),
            "endian" => Value::Str("little".into()),
            "debug" => Value::Bool(self.debug),
            _ => return Err(Diagnostic::error(span, format!("`target` has no property `{name}`"))),
        })
    }
}

/// What compile-time code can see where it runs.
pub trait Env {
    /// A name: a constant, a type, a variant written `Side.Start`, or a local of a
    /// `comptime fn`.
    fn name(&self, name: &str, span: Span) -> Result<Value>;
    /// Size and alignment of a written type.
    fn layout(&self, texpr: &TypeExpr) -> Result<(u64, u64)>;
    /// A type written as an expression, such as `dict[str, str]`.
    fn type_of(&self, texpr: &TypeExpr) -> Result<Type> {
        Err(Diagnostic::error(texpr.span(), "types are not known here"))
    }
    /// A call to a `comptime fn`.
    fn call(&self, callee: &str, _args: Vec<Value>, span: Span) -> Result<Value> {
        Err(Diagnostic::error(span, format!("`{callee}` cannot be called at compile time")))
    }
    /// Whether `ty` was made from the generic type `family`, as `R == dict` asks.
    fn is_family(&self, _ty: &Type, _family: &str) -> bool {
        false
    }
}

pub fn eval(expr: &Expr, target: &Target, env: &dyn Env) -> Result<Value> {
    let span = expr.span;
    match &expr.kind {
        ExprKind::Int(v) => Ok(Value::Int(*v)),
        ExprKind::Float(v) => Ok(Value::Float(*v)),
        ExprKind::Bool(v) => Ok(Value::Bool(*v)),
        ExprKind::Str(v) => Ok(Value::Str(v.clone())),
        ExprKind::None => Ok(Value::None),
        ExprKind::Name(name) => env.name(name, span),
        ExprKind::Field { base, name } => match &base.kind {
            ExprKind::Name(b) if b == "target" => target.property(name, span),
            // `Side.Start`: the environment knows the enums.
            ExprKind::Name(b) => env.name(&format!("{b}.{name}"), span),
            _ => Err(not_compile_time(span)),
        },
        ExprKind::Index { .. } => {
            let texpr = type_expr(expr).ok_or_else(|| not_compile_time(span))?;
            env.type_of(&texpr).map(Value::Type)
        }
        ExprKind::Unary(op, operand) => unary(*op, eval(operand, target, env)?, span),
        ExprKind::Binary(op, left, right) => {
            let (left, right) = (eval(left, target, env)?, eval(right, target, env)?);
            binary(*op, left, right, env, span)
        }
        ExprKind::Call { callee, type_args, args, .. } if args.is_empty() && type_args.len() == 1 => {
            layout_query(callee, env.layout(&type_args[0])?, span)
        }
        ExprKind::Call { callee, type_args, args, .. } if type_args.is_empty() => {
            let args = args.iter().map(|a| eval(a, target, env)).collect::<Result<_>>()?;
            env.call(callee, args, span)
        }
        _ => Err(not_compile_time(span)),
    }
}

/// An expression read as a type: `R`, `dict[str, str]`, `&str`, `(i32, bool)`.
pub fn type_expr(expr: &Expr) -> Option<TypeExpr> {
    let span = expr.span;
    Some(match &expr.kind {
        ExprKind::Name(name) => TypeExpr::Named { name: name.clone(), args: Vec::new(), span },
        ExprKind::Index { base, index } => {
            let ExprKind::Name(name) = &base.kind else { return None };
            let args = match &index.kind {
                ExprKind::Tuple(items) => items.iter().map(type_expr).collect::<Option<_>>()?,
                _ => vec![type_expr(index)?],
            };
            TypeExpr::Named { name: name.clone(), args, span }
        }
        ExprKind::AddrOf { mutable, inner } => {
            TypeExpr::Ref { mutable: *mutable, inner: Box::new(type_expr(inner)?), span }
        }
        ExprKind::Tuple(items) => TypeExpr::Tuple(items.iter().map(type_expr).collect::<Option<_>>()?, span),
        ExprKind::Int(n) => TypeExpr::Size(u64::try_from(*n).ok()?, span),
        ExprKind::Call { callee, type_args, args, .. } if type_args.is_empty() => TypeExpr::Call {
            name: callee.clone(),
            args: args.iter().map(type_expr).collect::<Option<_>>()?,
            span,
        },
        _ => return None,
    })
}

fn not_compile_time(span: Span) -> Diagnostic {
    Diagnostic::error(span, "this expression cannot be evaluated at compile time")
}

/// `size_of[T]()` and `align_of[T]()`, given the layout of `T`.
pub fn layout_query(callee: &str, (size, align): (u64, u64), span: Span) -> Result<Value> {
    match callee {
        "size_of" => Ok(Value::Int(size.into())),
        "align_of" => Ok(Value::Int(align.into())),
        _ => Err(Diagnostic::error(span, "this expression cannot be evaluated at compile time")),
    }
}

/// Checks that a value can be stored in `ty`.
pub fn check_fits(value: &Value, ty: &Type, span: Span) -> Result<()> {
    let fits = match (value, ty) {
        (Value::Int(v), Type::Int(t)) => t.fits(*v),
        (Value::Int(_) | Value::Float(_), Type::Float(_)) | (Value::Bool(_), Type::Bool) => true,
        (Value::Variant { ty: of, .. }, _) => of == ty,
        (Value::None, Type::Optional(_)) => true,
        // A text default for a `&str` parameter is lent for the call.
        (Value::Str(_), Type::Ref(inner, false)) => **inner == Type::Str,
        _ => false,
    };
    if !fits {
        return Err(Diagnostic::error(span, format!("this value does not fit in `{ty}`")));
    }
    Ok(())
}

fn unary(op: UnOp, value: Value, span: Span) -> Result<Value> {
    match (op, value) {
        (UnOp::Neg, Value::Int(v)) => v.checked_neg().map(Value::Int).ok_or_else(|| overflow(span)),
        (UnOp::Neg, Value::Float(v)) => Ok(Value::Float(-v)),
        (UnOp::Not, Value::Bool(v)) => Ok(Value::Bool(!v)),
        _ => Err(Diagnostic::error(span, "this operator cannot be used here at compile time")),
    }
}

pub fn binary(op: BinOp, left: Value, right: Value, env: &dyn Env, span: Span) -> Result<Value> {
    match (left, right) {
        (Value::Type(a), Value::Type(b)) => {
            let same = match (&a, &b) {
                (Type::Family(x), Type::Family(y)) => x == y,
                (ty, Type::Family(family)) | (Type::Family(family), ty) => env.is_family(ty, family),
                _ => a == b,
            };
            match op {
                BinOp::Eq => Ok(Value::Bool(same)),
                BinOp::Ne => Ok(Value::Bool(!same)),
                _ => Err(unsupported(op, span)),
            }
        }
        (Value::Int(a), Value::Int(b)) => int_binary(op, a, b, span),
        (Value::Float(a), Value::Float(b)) => float_binary(op, a, b, span),
        (Value::Bool(a), Value::Bool(b)) => match op {
            BinOp::And => Ok(Value::Bool(a && b)),
            BinOp::Or => Ok(Value::Bool(a || b)),
            BinOp::Eq => Ok(Value::Bool(a == b)),
            BinOp::Ne => Ok(Value::Bool(a != b)),
            _ => Err(unsupported(op, span)),
        },
        (Value::Variant { ty: a, index: i }, Value::Variant { ty: b, index: j }) if a == b => match op {
            BinOp::Eq => Ok(Value::Bool(i == j)),
            BinOp::Ne => Ok(Value::Bool(i != j)),
            _ => Err(unsupported(op, span)),
        },
        (Value::Str(a), Value::Str(b)) => match op {
            BinOp::Eq => Ok(Value::Bool(a == b)),
            BinOp::Ne => Ok(Value::Bool(a != b)),
            _ => Err(unsupported(op, span)),
        },
        _ => Err(Diagnostic::error(span, format!("`{}` needs two values of the same kind", op.symbol()))),
    }
}

fn int_binary(op: BinOp, a: i128, b: i128, span: Span) -> Result<Value> {
    if op.is_comparison() {
        return Ok(Value::Bool(compare(op, a.cmp(&b))));
    }
    let result = match op {
        BinOp::Add => a.checked_add(b),
        BinOp::Sub => a.checked_sub(b),
        BinOp::Mul => a.checked_mul(b),
        BinOp::Div | BinOp::FloorDiv | BinOp::Mod if b == 0 => {
            return Err(Diagnostic::error(span, "division by zero"));
        }
        BinOp::Div | BinOp::FloorDiv => a.checked_div(b),
        BinOp::Mod => a.checked_rem(b),
        BinOp::Pow => u32::try_from(b).ok().and_then(|exp| a.checked_pow(exp)),
        // `checked_shl` only rejects a shift wider than the type; it does not notice when the
        // value itself overflows (it keeps the low 128 bits). `r >> n == a` holds only when no
        // bit was lost, so a left shift that overflows `i128` is reported like any other
        // overflow instead of folding to a silently wrong value.
        BinOp::Shl => u32::try_from(b).ok().and_then(|n| a.checked_shl(n).filter(|r| r >> n == a)),
        BinOp::Shr => u32::try_from(b).ok().and_then(|n| a.checked_shr(n)),
        BinOp::BitAnd => Some(a & b),
        BinOp::BitOr => Some(a | b),
        BinOp::BitXor => Some(a ^ b),
        _ => return Err(unsupported(op, span)),
    };
    result.map(Value::Int).ok_or_else(|| overflow(span))
}

fn float_binary(op: BinOp, a: f64, b: f64, span: Span) -> Result<Value> {
    if op.is_comparison() {
        let ordering = a.partial_cmp(&b).ok_or_else(|| Diagnostic::error(span, "comparison with NaN"))?;
        return Ok(Value::Bool(compare(op, ordering)));
    }
    Ok(Value::Float(match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::Pow => a.powf(b),
        _ => return Err(unsupported(op, span)),
    }))
}

fn compare(op: BinOp, ordering: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match op {
        BinOp::Eq => ordering == Equal,
        BinOp::Ne => ordering != Equal,
        BinOp::Lt => ordering == Less,
        BinOp::Le => ordering != Greater,
        BinOp::Gt => ordering == Greater,
        _ => ordering != Less,
    }
}

fn overflow(span: Span) -> Diagnostic {
    Diagnostic::error(span, "this compile-time computation overflows")
}

fn unsupported(op: BinOp, span: Span) -> Diagnostic {
    Diagnostic::error(span, format!("`{}` cannot be used on these values", op.symbol()))
}
