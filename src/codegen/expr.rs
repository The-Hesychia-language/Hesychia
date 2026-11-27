//! Expression emission. Every compound expression is fully parenthesized.

use super::Emitter;
use super::helpers::Helper;
use super::types::is_pointer;
use crate::ast::{BinOp, CastKind, Expr, ExprKind, UnOp};
use crate::types::{FloatTy, IntTy, Type};

const C_KEYWORDS: &[&str] = &[
    "auto", "char", "default", "do", "double", "extern", "float", "goto", "inline", "int", "long",
    "register", "restrict", "short", "signed", "sizeof", "static", "switch", "typedef", "union", "unsigned",
    "void", "volatile", "bool", "true", "false", "NULL",
];

/// A C-safe spelling of a Hesychia identifier; `_t` names would clash with C types.
pub(super) fn c_ident(name: &str) -> String {
    let reserved = C_KEYWORDS.contains(&name) || name.starts_with("hsy_") || name.ends_with("_t");
    if reserved { format!("{name}_") } else { name.to_string() }
}

/// A C string literal holding the UTF-8 bytes of `text`.
pub(super) fn c_string(text: &str) -> String {
    let mut out = String::from("\"");
    for byte in text.bytes() {
        match byte {
            b'"' => out += "\\\"",
            b'\\' => out += "\\\\",
            b'\n' => out += "\\n",
            b'\t' => out += "\\t",
            b'\r' => out += "\\r",
            0x20..=0x7e => out.push(byte as char),
            _ => out += &format!("\\{byte:03o}"),
        }
    }
    out + "\""
}

impl Emitter<'_> {
    /// `base.name`. The field of a temporary value, as in `make().age`, is taken out of it
    /// (copied, when it owns memory) and the temporary is released.
    fn emit_field(&mut self, expr: &Expr, base: &Expr, name: &str) -> String {
        let base_code = self.emit_expr(base);
        let drop = if super::stmt::is_place(base) { None } else { self.types.drop_fn(base.ty()) };
        // `as.Case.field` reaches into an enum: each part is a C name of its own.
        let path = name.split('.').map(c_ident).collect::<Vec<_>>().join(".");
        let Some(drop) = drop else { return format!("{base_code}.{path}") };
        let (whole, part) = (self.types.c_type(base.ty()), self.types.c_type(expr.ty()));
        let (temp, field) = (self.temp("whole"), self.temp("part"));
        let taken = if self.types.table.needs_drop(expr.ty()) {
            format!("{}(&{temp}.{})", self.types.copy_fn(expr.ty()), c_ident(name))
        } else {
            format!("{temp}.{}", c_ident(name))
        };
        format!("({{ {whole} {temp} = {base_code}; {part} {field} = {taken}; {drop}(&{temp}); {field}; }})")
    }

    pub(super) fn emit_expr(&mut self, expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Formatted { .. } => unreachable!("made a call by the checker"),
            ExprKind::Int(value) => int_literal(*value, expr.ty()),
            ExprKind::Float(value) => float_literal(*value, expr.ty()),
            ExprKind::Bool(value) => value.to_string(),
            ExprKind::Char(c) => format!("((uint32_t){}u)", *c as u32),
            ExprKind::Str(text) => {
                self.types.c_type(&Type::Str);
                format!("((hsy_str){{(char*){}, {}, 0}})", c_string(text), text.len())
            }
            ExprKind::Bytes(bytes) => self.emit_bytes(expr.ty(), bytes),
            ExprKind::Format(parts) => self.emit_format(parts),
            ExprKind::FnRef(c_name) => c_name.clone(),
            ExprKind::Lambda { captures, params, body, c_name, .. } => match expr.ty() {
                Type::Ref(function, _) => self.emit_closure(captures, params, body, c_name, function),
                ty => self.emit_lambda(params, body, c_name, ty),
            },
            ExprKind::CallValue { callee, args } => {
                let args: Vec<String> = args.iter().map(|a| self.emit_expr(a)).collect();
                match &callee.kind {
                    // A borrowed function: its code gets its environment first.
                    ExprKind::Deref(closure) => {
                        let closure = self.emit_expr(closure);
                        let mut all = vec![format!("{closure}.env")];
                        all.extend(args);
                        format!("{closure}.code({})", all.join(", "))
                    }
                    _ => format!("{}({})", self.emit_expr(callee), args.join(", ")),
                }
            }
            ExprKind::Check { call, errors } => self.emit_check(call, errors),
            ExprKind::Move { value, root } => self.emit_move(value, root),
            ExprKind::Builtin { op, args } => self.emit_builtin(*op, args, expr.ty()),
            ExprKind::Name(name) => c_ident(name),
            ExprKind::Unary(op, operand) => self.emit_unary(*op, operand),
            ExprKind::Binary(op, left, right) => self.emit_binary(*op, left, right),
            ExprKind::Call { args, target, .. } => {
                self.emit_call(target.as_deref().expect("calls are resolved"), args, expr.ty())
            }
            ExprKind::Method { .. } => unreachable!("methods become calls in the type checker"),
            ExprKind::Convert(value) => self.emit_conversion(value, expr.ty()),
            ExprKind::Cast(value, kind) => self.emit_cast(value, *kind, expr.ty()),
            ExprKind::Field { base, name } => self.emit_field(expr, base, name),
            ExprKind::Index { base, index } if matches!(base.ty(), Type::Vector(_)) => {
                self.emit_vector_index(base, index)
            }
            ExprKind::Index { base, index } if matches!(base.ty(), Type::Ptr(_)) => {
                format!("({}[{}])", self.emit_expr(base), self.emit_expr(index))
            }
            ExprKind::Index { base, index } => self.emit_index(base, index),
            ExprKind::Slice { .. } => unreachable!("the checker turns a slice into a call"),
            ExprKind::Global { module, name } => crate::ast::global_c_name(*module, &c_ident(name)),
            // A plain function lent as `&fn(...)`: called through the thunk, which reads it back.
            ExprKind::AddrOf { inner, .. } if matches!(inner.ty(), Type::Fn(..)) => {
                let (closure, thunk) = self.types.closure(inner.ty());
                let pointer = self.types.c_type(inner.ty());
                format!("(({closure}){{{thunk}, &({pointer}){{{}}}}})", self.emit_expr(inner))
            }
            // Places, and compound literals such as a `str` literal, have an address already.
            ExprKind::AddrOf { inner, .. }
                if super::stmt::is_place(inner) || matches!(inner.kind, ExprKind::Str(_)) =>
            {
                format!("(&{})", self.emit_expr(inner))
            }
            ExprKind::AddrOf { inner, .. } => {
                // A one-item array takes any value as its item, a struct as well as a number.
                let c_type = self.types.c_type(inner.ty());
                format!("(&(({c_type}[1]){{{}}})[0])", self.emit_expr(inner))
            }
            ExprKind::Deref(inner) if matches!(inner.ty(), Type::Shared(_)) => {
                format!("({}->value)", self.emit_expr(inner))
            }
            ExprKind::Deref(inner) => format!("(*{})", self.emit_expr(inner)),
            ExprKind::CopyOf(inner) => self.emit_copy(inner),
            ExprKind::Comprehension { built: Some((result, fill)), .. }
            | ExprKind::Collection { built: Some((result, fill)), .. } => {
                self.emit_comprehension(result, fill, expr.ty())
            }
            ExprKind::Comprehension { .. } | ExprKind::Collection { .. } => {
                unreachable!("built by the checker")
            }
            ExprKind::Tuple(items) | ExprKind::Construct(items) => {
                self.emit_aggregate(expr.ty(), items, false)
            }
            ExprKind::ArrayLit(items) if matches!(expr.ty(), Type::Vector(_)) => {
                self.emit_vector_literal(expr.ty(), items)
            }
            ExprKind::ArrayLit(items) => self.emit_aggregate(expr.ty(), items, true),
            ExprKind::Variant { index, args } => self.emit_variant(expr.ty(), *index, args),
            ExprKind::None => self.emit_none(expr.ty()),
            ExprKind::Wrap(inner) => self.emit_wrap(expr.ty(), inner),
            ExprKind::Unwrap(inner) => {
                let code = self.emit_expr(inner);
                if is_pointer(expr.ty()) { code } else { format!("{code}.value") }
            }
            ExprKind::Range { .. } | ExprKind::Named { .. } => unreachable!("rewritten by the type checker"),
        }
    }

    /// Operators on structs call their trait methods; `==` on other aggregates compares
    /// them part by part.
    fn emit_composite_binary(&mut self, op: BinOp, left: &Expr, right: &Expr) -> Option<String> {
        let ty = left.ty().clone();
        let composite = match &ty {
            Type::Enum { plain, .. } => !plain,
            _ => super::types::is_aggregate(&ty) || matches!(ty, Type::Heap(_)),
        };
        if !composite {
            return None;
        }
        let negate = op == BinOp::Ne;
        let (call, result) = match (op, &ty) {
            (BinOp::Eq | BinOp::Ne, _) => (self.types.eq_fn(&ty), "bool".to_string()),
            // `<` and the like on a struct or enum with `Order`: the sign of `compare`.
            (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, Type::Struct(named)) => {
                let call = self.types.table.structs[named.id].operators["compare"].clone();
                let code = self.emit_operand_call(&call, left, right, "int32_t");
                return Some(format!("({code} {} 0)", op.symbol()));
            }
            (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, Type::Enum { named, .. }) => {
                let call = self.types.table.enums[named.id].operators["compare"].clone();
                let code = self.emit_operand_call(&call, left, right, "int32_t");
                return Some(format!("({code} {} 0)", op.symbol()));
            }
            (_, Type::Struct(named)) => {
                let method = crate::typeck::operator_method(op).expect("checked operator");
                (self.types.table.structs[named.id].operators[method].clone(), self.types.c_type(&ty))
            }
            _ => unreachable!("only structs have arithmetic operators"),
        };
        let code = self.emit_operand_call(&call, left, right, &result);
        Some(if negate { format!("(!{code})") } else { code })
    }

    /// A compound literal; arrays wrap their items in one more pair of braces.
    fn emit_aggregate(&mut self, ty: &Type, items: &[Expr], array: bool) -> String {
        let c_type = self.types.c_type(ty);
        let mut values: Vec<String> = items.iter().map(|item| self.emit_expr(item)).collect();
        if let Type::Struct(named) = ty
            && self.types.table.structs[named.id].drop_method().is_some()
        {
            values.push("true".into());
        }
        let values = if values.is_empty() { "0".to_string() } else { values.join(", ") };
        if array { format!("(({c_type}){{{{{values}}}}})") } else { format!("(({c_type}){{{values}}})") }
    }

    fn emit_variant(&mut self, ty: &Type, index: usize, args: &[Expr]) -> String {
        let c_type = self.types.c_type(ty);
        if args.is_empty() {
            return format!("(({c_type}){{.tag = {index}}})");
        }
        let Type::Enum { named, .. } = ty else { unreachable!("variants build enums") };
        let variant = c_ident(&self.types.variant_name(named.id, index));
        let values: Vec<String> = args.iter().map(|arg| self.emit_expr(arg)).collect();
        format!("(({c_type}){{.tag = {index}, .as.{variant} = {{{}}}}})", values.join(", "))
    }

    pub(super) fn emit_none(&mut self, ty: &Type) -> String {
        let Type::Optional(inner) = ty else { unreachable!("`None` is an Optional") };
        if is_pointer(inner) {
            return "NULL".into();
        }
        format!("(({}){{.has = false}})", self.types.c_type(ty))
    }

    fn emit_wrap(&mut self, ty: &Type, inner: &Expr) -> String {
        let code = self.emit_expr(inner);
        if is_pointer(inner.ty()) {
            return code;
        }
        format!("(({}){{.has = true, .value = {code}}})", self.types.c_type(ty))
    }

    /// Whether an `Optional` held in the C expression `code` has a value.
    pub(super) fn has_value(ty: &Type, code: &str) -> String {
        match ty {
            Type::Optional(inner) if is_pointer(inner) => format!("({code} != NULL)"),
            _ => format!("{code}.has"),
        }
    }

    /// `a[i]` with the index checked against the length.
    fn emit_index(&mut self, base: &Expr, index: &Expr) -> String {
        let (array, position) = (self.emit_expr(base), self.emit_expr(index));
        let check = self.helpers.call(Helper::Index);
        if let Type::Simd(_, lanes) = base.ty() {
            return format!("{array}[{check}((__int128)({position}), {lanes})]");
        }
        let Type::Array(_, len) = base.ty() else { unreachable!("only arrays are indexed") };
        format!("{array}.items[{check}((__int128)({position}), {len})]")
    }

    fn emit_unary(&mut self, op: UnOp, operand: &Expr) -> String {
        let value = self.emit_expr(operand);
        match (op, operand.ty().as_int()) {
            (UnOp::Neg, Some(t)) => format!("{}({value})", self.helpers.call(Helper::Neg(t))),
            (UnOp::Neg, None) => format!("(-{value})"),
            (UnOp::Not, _) => format!("(!{value})"),
            (UnOp::BitNot, _) => format!("(({})~{value})", self.types.c_type(operand.ty())),
        }
    }

    pub(super) fn emit_binary(&mut self, op: BinOp, left: &Expr, right: &Expr) -> String {
        if matches!(op, BinOp::Is | BinOp::IsNot) {
            // A temporary, `os.env("HOME") is None`, is released after the test.
            if self.types.table.needs_drop(left.ty()) && !super::stmt::is_place(left) {
                let (setup, pointer, cleanup) = self.read_operand(left);
                let has = Self::has_value(left.ty(), &format!("(*{pointer})"));
                let test = if op == BinOp::IsNot { has } else { format!("!{has}") };
                let temp = self.temp("has");
                return format!("({{ {setup}bool {temp} = {test}; {cleanup}{temp}; }})");
            }
            let has = Self::has_value(left.ty(), &self.emit_expr(left));
            return if op == BinOp::IsNot { format!("({has})") } else { format!("(!{has})") };
        }
        if *left.ty() == Type::Str {
            return match op {
                BinOp::Add => self.emit_str_binary("hsy_str_concat", left, right, "hsy_str"),
                BinOp::Eq => self.emit_str_binary("hsy_str_eq", left, right, "bool"),
                BinOp::Ne => format!("(!{})", self.emit_str_binary("hsy_str_eq", left, right, "bool")),
                _ => {
                    format!("({} {} 0)", self.emit_str_binary("hsy_str_cmp", left, right, "int"), op.symbol())
                }
            };
        }
        if let Some(code) = self.emit_composite_binary(op, left, right) {
            return code;
        }
        if matches!(op, BinOp::And | BinOp::Or)
            && let Some(code) = self.emit_hoisted_logic(op, left, right)
        {
            return code;
        }
        let (a, b) = (self.emit_expr(left), self.emit_expr(right));
        let symbol = op.symbol();
        if matches!(left.ty(), Type::Enum { .. }) {
            return format!("({a}.tag {symbol} {b}.tag)");
        }
        let Some(t) = left.ty().as_int() else {
            return match op {
                BinOp::And => format!("({a} && {b})"),
                BinOp::Or => format!("({a} || {b})"),
                BinOp::Pow => float_power(left.ty(), &a, &b),
                _ => format!("({a} {symbol} {b})"),
            };
        };
        let helper = match op {
            BinOp::Add => Helper::Add(t),
            BinOp::Sub => Helper::Sub(t),
            BinOp::Mul => Helper::Mul(t),
            BinOp::Div | BinOp::FloorDiv => Helper::Div(t),
            BinOp::Mod => Helper::Mod(t),
            BinOp::Pow => Helper::Pow(t),
            BinOp::Shl | BinOp::Shr => return self.emit_shift(op, t, &a, &b),
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                return format!("(({})({a} {symbol} {b}))", t.c_name());
            }
            _ => return format!("({a} {symbol} {b})"),
        };
        format!("{}({a}, {b})", self.helpers.call(helper))
    }

    fn emit_shift(&mut self, op: BinOp, t: IntTy, value: &str, amount: &str) -> String {
        // `<<` drops the bits that fall off the top in `manual`; in `managed` losing them is an
        // overflow and stops the program. `>>` never overflows, so it is the same at both levels.
        let helper = match op {
            BinOp::Shl if self.fn_level == crate::ast::Level::Managed => Helper::Shl(t),
            BinOp::Shl => Helper::ShlWrap(t),
            _ => Helper::Shr(t),
        };
        format!("{}({value}, (unsigned __int128)({amount}))", self.helpers.call(helper))
    }

    fn emit_conversion(&mut self, value: &Expr, target: &Type) -> String {
        let code = self.emit_expr(value);
        let helper = match (value.ty(), target) {
            (from, to) if from == to => return code,
            (Type::Int(from), Type::Int(to)) if to.contains(*from) => {
                return format!("(({}){code})", to.c_name());
            }
            (Type::Int(from), Type::Int(to)) => Helper::IntToInt(*from, *to),
            (Type::Float(from), Type::Int(to)) => Helper::FloatToInt(*from, *to),
            _ => return format!("(({}){code})", self.types.c_type(target)),
        };
        format!("{}({code})", self.helpers.call(helper))
    }
}

impl Emitter<'_> {
    /// A call. A temporary that owns resources and is lent to the callee lives in a variable
    /// for the call, and is released after it.
    fn emit_call(&mut self, function: &str, args: &[Expr], ty: &Type) -> String {
        let (mut setup, mut cleanup, mut values) = (String::new(), String::new(), Vec::new());
        for arg in args {
            match &arg.kind {
                ExprKind::AddrOf { inner, .. }
                    if !super::stmt::is_place(inner) && self.types.drop_fn(inner.ty()).is_some() =>
                {
                    let (c_type, drop) =
                        (self.types.c_type(inner.ty()), self.types.drop_fn(inner.ty()).expect("checked"));
                    let temp = self.temp("lent");
                    setup += &format!("{c_type} {temp} = {}; ", self.emit_expr(inner));
                    cleanup += &format!("{drop}(&{temp}); ");
                    values.push(format!("&{temp}"));
                }
                _ => values.push(self.emit_expr(arg)),
            }
        }
        let call = format!("{function}({})", values.join(", "));
        if setup.is_empty() {
            return call;
        }
        let fallible = self.signatures.get(function).is_some_and(|sig| !sig.raises.is_empty());
        if *ty == Type::Void && !fallible {
            return format!("({{ {setup}{call}; {cleanup}}})");
        }
        // `__typeof__`: a fallible callee gives its result struct, not `ty`.
        let result = self.temp("result");
        format!("({{ {setup}__typeof__({call}) {result} = {call}; {cleanup}{result}; }})")
    }

    /// `T.checked(x)`, `T.truncate(x)`, `T.reinterpret(x)`.
    fn emit_cast(&mut self, value: &Expr, kind: CastKind, ty: &Type) -> String {
        let code = self.emit_expr(value);
        let from = self.types.c_type(value.ty());
        let target = match (kind, ty) {
            (CastKind::Checked, Type::Optional(inner)) => inner.as_ref().clone(),
            _ => ty.clone(),
        };
        let to = self.types.c_type(&target);
        let v = self.temp("cast");
        match (kind, value.ty(), &target) {
            (CastKind::Reinterpret, ..) => {
                format!("({{ {from} {v} = {code}; {to} r; memcpy(&r, &{v}, sizeof r); r; }})")
            }
            (CastKind::Truncate, Type::Float(_), Type::Int(t)) => {
                let b = IntBounds::of(*t);
                format!(
                    "({{ double {v} = {code}; {v} != {v} ? ({to})0 : {v} <= {} ? {} : {v} >= {} ? {} : ({to}){v}; }})",
                    b.low, b.min, b.high, b.max
                )
            }
            // Through the unsigned type of the target's width: the low bits, in two's complement.
            (CastKind::Truncate, Type::Int(_), Type::Int(t)) if t.signed() => {
                format!("(({to})({})({code}))", t.unsigned().c_name())
            }
            (CastKind::Truncate, ..) => format!("(({to})({code}))"),
            (CastKind::Checked, _, _) => {
                let optional = self.types.c_type(ty);
                let fits = match (value.ty(), &target) {
                    (Type::Int(_), Type::Int(_)) => format!("!__builtin_add_overflow({v}, 0, &r)"),
                    (Type::Float(_), Type::Int(t)) => {
                        let b = IntBounds::of(*t);
                        format!(
                            "({v} == {v} && {v} > {} - 1.0 && {v} < {} && ((r = ({to}){v}), 1))",
                            b.low, b.high
                        )
                    }
                    _ => format!("((r = ({to}){v}), 1)"),
                };
                format!(
                    "({{ {from} {v} = {code}; {to} r = 0; bool ok = {fits}; ({optional}){{.has = ok, .value = r}}; }})"
                )
            }
        }
    }
}

/// The range of an integer type, for converting floats: `low` and `high` as C doubles
/// (`high` excluded), `min` and `max` as values of the type.
struct IntBounds {
    low: String,
    high: String,
    min: String,
    max: String,
}

impl IntBounds {
    fn of(t: IntTy) -> Self {
        let (c, bits) = (t.c_name(), t.bits());
        let unsigned = t.unsigned().c_name();
        let all_ones = format!("(({unsigned})~({unsigned})0)");
        if t.signed() {
            let max = format!("(({c})({all_ones} >> 1))");
            Self {
                low: format!("-0x1p{}", bits - 1),
                high: format!("0x1p{}", bits - 1),
                min: format!("(-{max} - 1)"),
                max,
            }
        } else {
            Self { low: "0.0".into(), high: format!("0x1p{bits}"), min: format!("(({c})0)"), max: all_ones }
        }
    }
}

fn float_power(ty: &Type, base: &str, exponent: &str) -> String {
    let builtin = if *ty == Type::Float(FloatTy::F32) { "__builtin_powf" } else { "__builtin_pow" };
    format!("{builtin}({base}, {exponent})")
}

/// Plain `i32` literals need no cast; other types are cast so variadic calls get the right width.
fn int_literal(value: i128, ty: &Type) -> String {
    match ty {
        Type::Int(IntTy::I32) => value.to_string(),
        Type::Int(t) => format!("(({}){})", t.c_name(), c_int_constant(value)),
        _ => float_literal(value as f64, ty),
    }
}

/// C spells infinities and NaN with built-ins; `math.inf` and `math.nan` are constants.
fn float_literal(value: f64, ty: &Type) -> String {
    let f32 = *ty == Type::Float(FloatTy::F32);
    let suffix = if f32 { "f" } else { "" };
    if value.is_nan() {
        return format!("__builtin_nan{suffix}(\"\")");
    }
    if value.is_infinite() {
        let sign = if value < 0.0 { "-" } else { "" };
        return format!("({sign}__builtin_inf{suffix}())");
    }
    format!("{value:?}{suffix}")
}

/// Spells an integer constant that C parses with the right value.
fn c_int_constant(value: i128) -> String {
    if value == i128::from(i64::MIN) {
        "(-9223372036854775807LL - 1)".into()
    } else if i64::try_from(value).is_ok() {
        format!("{value}LL")
    } else if u64::try_from(value).is_ok() {
        format!("{value}ULL")
    } else {
        let bits = value as u128;
        format!("(((unsigned __int128){}ULL << 64) | {}ULL)", bits >> 64, bits as u64)
    }
}
