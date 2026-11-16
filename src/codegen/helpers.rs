//! Checked runtime helpers, emitted only when the program uses them.

use std::collections::BTreeSet;

use crate::types::{FloatTy, IntTy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Helper {
    Add(IntTy),
    Sub(IntTy),
    Mul(IntTy),
    Div(IntTy),
    Mod(IntTy),
    Neg(IntTy),
    Pow(IntTy),
    RangeNext(IntTy),
    Shl(IntTy),
    Shr(IntTy),
    IntToInt(IntTy, IntTy),
    FloatToInt(FloatTy, IntTy),
    Index,
}

#[derive(Default)]
pub struct Helpers {
    used: BTreeSet<Helper>,
}

impl Helpers {
    /// Registers a helper and returns the C function name to call.
    pub fn call(&mut self, helper: Helper) -> String {
        self.used.insert(helper);
        name(helper)
    }

    pub fn render(&self) -> String {
        if self.used.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for helper in &self.used {
            out += &definition(*helper);
        }
        out
    }
}

fn name(helper: Helper) -> String {
    match helper {
        Helper::Add(t) => format!("hsy_add_{}", t.name()),
        Helper::Sub(t) => format!("hsy_sub_{}", t.name()),
        Helper::Mul(t) => format!("hsy_mul_{}", t.name()),
        Helper::Div(t) => format!("hsy_div_{}", t.name()),
        Helper::Mod(t) => format!("hsy_mod_{}", t.name()),
        Helper::Neg(t) => format!("hsy_neg_{}", t.name()),
        Helper::Pow(t) => format!("hsy_pow_{}", t.name()),
        Helper::RangeNext(t) => format!("hsy_range_next_{}", t.name()),
        Helper::Shl(t) => format!("hsy_shl_{}", t.name()),
        Helper::Shr(t) => format!("hsy_shr_{}", t.name()),
        Helper::IntToInt(from, to) => format!("hsy_conv_{}_{}", from.name(), to.name()),
        Helper::FloatToInt(from, to) => format!("hsy_conv_{}_{}", from.name(), to.name()),
        Helper::Index => "hsy_index".into(),
    }
}

fn definition(helper: Helper) -> String {
    let fname = name(helper);
    match helper {
        Helper::Add(t) => overflow_op(&fname, t, "add"),
        Helper::Sub(t) => overflow_op(&fname, t, "sub"),
        Helper::Mul(t) => overflow_op(&fname, t, "mul"),
        Helper::Div(t) => division(&fname, t),
        Helper::Mod(t) => modulo(&fname, t),
        Helper::Neg(t) => negation(&fname, t),
        Helper::Pow(t) => power(&fname, t),
        Helper::RangeNext(t) => range_next(&fname, t),
        Helper::Shl(t) => shift(&fname, t, &format!("({})(({})a << n)", t.c_name(), t.unsigned().c_name())),
        Helper::Shr(t) => shift(&fname, t, &format!("({})(a >> n)", t.c_name())),
        Helper::IntToInt(from, to) => int_to_int(&fname, from, to),
        Helper::FloatToInt(from, to) => float_to_int(&fname, from, to),
        Helper::Index => function(
            &fname,
            "size_t",
            "__int128 i, size_t len",
            "if (i < 0 || i >= (__int128)len) hsy_panic();\n    return (size_t)i;",
        ),
    }
}

fn function(name: &str, ret: &str, params: &str, body: &str) -> String {
    format!("static inline {ret} {name}({params}) {{\n    {body}\n}}\n\n")
}

fn overflow_op(name: &str, t: IntTy, builtin: &str) -> String {
    let c = t.c_name();
    let body = format!("{c} r;\n    if (__builtin_{builtin}_overflow(a, b, &r)) hsy_panic();\n    return r;");
    function(name, c, &format!("{c} a, {c} b"), &body)
}

fn negation(name: &str, t: IntTy) -> String {
    let c = t.c_name();
    let body = format!("{c} r;\n    if (__builtin_sub_overflow(({c})0, a, &r)) hsy_panic();\n    return r;");
    function(name, c, &format!("{c} a"), &body)
}

/// Exponentiation by squaring; a negative exponent stops the program.
fn power(name: &str, t: IntTy) -> String {
    let c = t.c_name();
    let negative_check = if t.signed() { "if (exp < 0) hsy_panic();\n    " } else { "" };
    let body = format!(
        "{negative_check}{c} result = 1;\n    while (exp > 0) {{\n        \
         if ((exp & 1) && __builtin_mul_overflow(result, base, &result)) hsy_panic();\n        \
         exp >>= 1;\n        \
         if (exp > 0 && __builtin_mul_overflow(base, base, &base)) hsy_panic();\n    }}\n    return result;"
    );
    function(name, c, &format!("{c} base, {c} exp"), &body)
}

/// The next value of a `range` loop; overflowing means the loop is over.
fn range_next(name: &str, t: IntTy) -> String {
    let c = t.c_name();
    let body = format!("{c} r;\n    if (__builtin_add_overflow(i, step, &r)) return end;\n    return r;");
    function(name, c, &format!("{c} i, {c} step, {c} end"), &body)
}

/// The builtin computes `x + 0` exactly and reports whether it fits in the target type.
fn int_to_int(name: &str, from: IntTy, to: IntTy) -> String {
    let c = to.c_name();
    let body = format!("{c} r;\n    if (__builtin_add_overflow(x, 0, &r)) hsy_panic();\n    return r;");
    function(name, c, &format!("{} x", from.c_name()), &body)
}

/// Truncating division; dividing the minimum signed value by -1 overflows.
fn division(name: &str, t: IntTy) -> String {
    let c = t.c_name();
    let mut body = String::from("if (b == 0) hsy_panic();\n");
    if t.signed() {
        body += &format!(
            "    if (b == -1) {{\n        {c} r;\n        if (__builtin_mul_overflow(a, b, &r)) hsy_panic();\n        return r;\n    }}\n"
        );
    }
    body += "    return a / b;";
    function(name, c, &format!("{c} a, {c} b"), &body)
}

fn modulo(name: &str, t: IntTy) -> String {
    let c = t.c_name();
    let mut body = String::from("if (b == 0) hsy_panic();\n");
    if t.signed() {
        body += "    if (b == -1) return 0;\n";
    }
    body += "    return a % b;";
    function(name, c, &format!("{c} a, {c} b"), &body)
}

/// Shifting by the width of the type or more stops the program.
fn shift(name: &str, t: IntTy, result: &str) -> String {
    let body = format!("if (n >= {}) hsy_panic();\n    return {result};", t.bits());
    function(name, t.c_name(), &format!("{} a, unsigned __int128 n", t.c_name()), &body)
}

/// Stops the program when the truncated value does not fit, including NaN.
fn float_to_int(name: &str, from: FloatTy, to: IntTy) -> String {
    let bits = to.bits() as i32;
    let (low, high) = if to.signed() {
        let low = -(2f64.powi(bits - 1));
        // Below 64 bits, `low - 1` is exact, so values that truncate to `low` are accepted.
        let low_check = if bits <= 32 { format!("x > {:?}", low - 1.0) } else { format!("x >= {low:?}") };
        (low_check, 2f64.powi(bits - 1))
    } else {
        ("x > -1.0".to_string(), 2f64.powi(bits))
    };
    let body = format!("if (!({low} && x < {high:?})) hsy_panic();\n    return ({})x;", to.c_name());
    function(name, to.c_name(), &format!("{} x", from.c_name()), &body)
}
