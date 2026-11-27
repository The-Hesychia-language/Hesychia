//! Releasing owned values: variables are dropped when their block ends, and a moved
//! variable is left zeroed, which every drop function treats as empty.

use super::Emitter;
use super::expr::{c_ident, c_string};
use crate::ast::{Builtin, Expr, ExprKind};
use crate::types::Type;

pub struct Scope {
    vars: Vec<(String, Type)>,
    is_loop: bool,
}

impl Emitter<'_> {
    pub(super) fn open_scope(&mut self, is_loop: bool) {
        self.scopes.push(Scope { vars: Vec::new(), is_loop });
    }

    /// Ends a block; its variables are dropped unless the block already jumped away.
    pub(super) fn close_scope(&mut self, jumped: bool) {
        let scope = self.scopes.pop().expect("a scope is open");
        if !jumped {
            self.drop_vars(&scope.vars);
        }
    }

    /// Records a variable to drop when its block ends, if it owns anything.
    pub(super) fn own(&mut self, c_name: String, ty: Type) {
        if self.types.table.needs_drop(&ty) {
            self.scopes.last_mut().expect("a scope is open").vars.push((c_name, ty));
        }
    }

    pub(super) fn has_owned(&self) -> bool {
        self.scopes.iter().any(|s| !s.vars.is_empty())
    }

    /// Drops before `return` (every block) or `break`/`continue` (up to the loop body).
    pub(super) fn drop_for_jump(&mut self, to_loop: bool) {
        let depth = if to_loop { self.scopes.iter().rposition(|s| s.is_loop).unwrap_or(0) } else { 0 };
        let code = self.drops_from(depth);
        if !code.is_empty() {
            self.line(code.trim_end());
        }
    }

    /// Drop calls for the variables of every scope from `depth` inward, innermost first.
    pub(super) fn drops_from(&mut self, depth: usize) -> String {
        let vars: Vec<(String, Type)> =
            self.scopes[depth..].iter().flat_map(|s| s.vars.iter().cloned()).collect();
        self.drops_code(&vars)
    }

    fn drops_code(&mut self, vars: &[(String, Type)]) -> String {
        // Threads are waited for last: one may be waiting for a sender of this block to go.
        fn waits(ty: &Type) -> bool {
            match ty {
                Type::Thread(_) | Type::Scope => true,
                Type::Vector(item) | Type::Optional(item) | Type::Array(item, _) => waits(item),
                Type::Tuple(items) => items.iter().any(waits),
                _ => false,
            }
        }
        let ordered = vars
            .iter()
            .rev()
            .filter(|(_, ty)| !waits(ty))
            .chain(vars.iter().rev().filter(|(_, ty)| waits(ty)));
        let mut code = String::new();
        for (name, ty) in ordered {
            let drop = self.types.drop_fn(ty).expect("owned variables need dropping");
            code += &format!("{drop}(&{name}); ");
        }
        code
    }

    fn drop_vars(&mut self, vars: &[(String, Type)]) {
        let code = self.drops_code(vars);
        if !code.is_empty() {
            self.line(code.trim_end());
        }
    }

    /// Releases a value that was computed but not kept, such as the result of a call statement.
    pub(super) fn drop_temporary(&mut self, ty: &Type, code: &str) -> Option<String> {
        let drop = self.types.drop_fn(ty)?;
        let c_type = self.types.c_type(ty);
        let temp = self.temp("discard");
        Some(format!("{{ {c_type} {temp} = {code}; {drop}(&{temp}); }}"))
    }

    pub(super) fn emit_move(&mut self, value: &Expr, root: &str) -> String {
        let c_type = self.types.c_type(value.ty());
        let code = self.emit_expr(value);
        let temp = self.temp("moved");
        let root = c_ident(root);
        format!("({{ {c_type} {temp} = {code}; memset(&{root}, 0, sizeof {root}); {temp}; }})")
    }

    pub(super) fn emit_copy(&mut self, value: &Expr) -> String {
        let code = self.emit_expr(value);
        if !self.types.table.needs_drop(value.ty()) {
            return code;
        }
        let copy = self.types.copy_fn(value.ty());
        let c_type = self.types.c_type(value.ty());
        let temp = self.temp("source");
        format!("({{ {c_type} {temp} = {code}; {copy}(&{temp}); }})")
    }

    /// An operand that is only read: a place is used where it is, a temporary is stored
    /// and released afterwards. Returns (setup, pointer, cleanup).
    pub(super) fn read_operand(&mut self, value: &Expr) -> (String, String, String) {
        if let ExprKind::AddrOf { .. } = &value.kind {
            return (String::new(), self.emit_expr(value), String::new());
        }
        let code = self.emit_expr(value);
        if super::stmt::is_place(value) || matches!(value.kind, ExprKind::Str(_)) {
            return (String::new(), format!("(&{code})"), String::new());
        }
        let c_type = self.types.c_type(value.ty());
        let temp = self.temp("operand");
        let cleanup = self.types.drop_fn(value.ty()).map_or(String::new(), |d| format!("{d}(&{temp}); "));
        (format!("{c_type} {temp} = {code}; "), format!("(&{temp})"), cleanup)
    }

    fn read_str(&mut self, value: &Expr) -> (String, String, String) {
        self.read_operand(value)
    }

    /// `call(&left, &right)` on two read-only operands, giving a `result`.
    pub(super) fn emit_operand_call(
        &mut self,
        call: &str,
        left: &Expr,
        right: &Expr,
        result: &str,
    ) -> String {
        self.emit_str_binary(call, left, right, result)
    }

    /// `a + b` and `a == b` on `str` values.
    pub(super) fn emit_str_binary(&mut self, call: &str, left: &Expr, right: &Expr, result: &str) -> String {
        let (setup_a, a, clean_a) = self.read_str(left);
        let (setup_b, b, clean_b) = self.read_str(right);
        let temp = self.temp("result");
        format!("({{ {setup_a}{setup_b}{result} {temp} = {call}({a}, {b}); {clean_a}{clean_b}{temp}; }})")
    }

    /// `s += t` appends in place.
    pub(super) fn emit_str_append(&mut self, target: &str, value: &Expr) -> String {
        let (setup, pointer, cleanup) = self.read_str(value);
        format!("{{ {setup}hsy_str_push_str(&{target}, {pointer}); {cleanup}}}")
    }

    /// `f"..."`: each part is appended to a new string.
    pub(super) fn emit_format(&mut self, parts: &[Expr]) -> String {
        self.types.c_type(&Type::Str);
        let out = self.temp("format");
        let mut code = format!("({{ hsy_str {out} = {{0}}; ");
        for part in parts {
            code += &match (&part.kind, part.ty()) {
                (ExprKind::Str(text), _) => {
                    format!("hsy_str_append(&{out}, {}, {}); ", c_string(text), text.len())
                }
                (_, Type::Str) => {
                    let (setup, pointer, cleanup) = self.read_str(part);
                    format!("{setup}hsy_str_push_str(&{out}, {pointer}); {cleanup}")
                }
                (_, ty) if !ty.is_numeric() && !matches!(ty, Type::Bool | Type::Char) => {
                    self.emit_shown(part, &out)
                }
                (_, ty) => {
                    let value = self.emit_expr(part);
                    match ty {
                        Type::Int(t) if t.signed() => {
                            format!("hsy_str_push_int(&{out}, (__int128)({value})); ")
                        }
                        Type::Int(_) => format!("hsy_str_push_uint(&{out}, (unsigned __int128)({value})); "),
                        Type::Float(crate::types::FloatTy::F32) => {
                            format!("hsy_str_push_float32(&{out}, {value}); ")
                        }
                        Type::Float(_) => format!("hsy_str_push_float(&{out}, (double)({value})); "),
                        Type::Bool => format!("hsy_str_push_bool(&{out}, {value}); "),
                        _ => format!("hsy_str_push_char(&{out}, {value}); "),
                    }
                }
            };
        }
        code + &format!("{out}; }})")
    }

    /// A struct, an enum, a list... written into `out`; a temporary is released afterwards.
    fn emit_shown(&mut self, part: &Expr, out: &str) -> String {
        let ty = part.ty().deref_ref().clone();
        let show = self.types.show_fn(&ty);
        let value = self.emit_expr(part);
        if matches!(part.ty(), Type::Ref(..)) {
            return format!("{show}(&{out}, {value}); ");
        }
        if super::stmt::is_place(part) {
            return format!("{show}(&{out}, &{value}); ");
        }
        let (c_type, temp) = (self.types.c_type(&ty), self.temp("shown"));
        let drop = self.types.drop_fn(&ty).map_or(String::new(), |d| format!("{d}(&{temp}); "));
        format!("{c_type} {temp} = {value}; {show}(&{out}, &{temp}); {drop}")
    }

    /// A built-in operation, by family.
    pub(super) fn emit_builtin(&mut self, op: Builtin, args: &[Expr], ty: &Type) -> String {
        if let Some(code) = self.emit_text_builtin(op, args) {
            return code;
        }
        let values: Vec<String> = args.iter().map(|arg| self.emit_expr(arg)).collect();
        if let Some(code) = self.emit_thread_builtin(op, args, &values, ty) {
            return code;
        }
        if let Some(code) = self.emit_low_level_builtin(op, args, &values, ty) {
            return code;
        }
        match op {
            Builtin::Allocate => {
                let c_type = self.types.c_type(args[0].ty());
                let temp = self.temp("heap");
                format!(
                    "({{ {c_type}* {temp} = hsy_alloc(sizeof({c_type})); *{temp} = {}; {temp}; }})",
                    values[0]
                )
            }
            Builtin::Deallocate => {
                let c_type = self.types.c_type(ty);
                let (heap, value) = (self.temp("heap"), self.temp("value"));
                format!(
                    "({{ {c_type}* {heap} = {}; {c_type} {value} = *{heap}; free({heap}); {value}; }})",
                    values[0]
                )
            }
            Builtin::SharedNew => self.emit_shared_new(args[0].ty(), &values[0]),
            Builtin::StrToC => format!("hsy_str_c({})", values[0]),
            Builtin::StrFromC => format!("hsy_str_from_c({})", values[0]),
            Builtin::Hash => format!("hsy_hash64((uint64_t)*({}))", values[0]),
            Builtin::StrByte => format!("hsy_str_byte({}, {})", values[0], values[1]),
            Builtin::WrappingAdd | Builtin::WrappingSub | Builtin::WrappingMul => {
                let ty = args[1].ty();
                let c_type = self.types.c_type(ty);
                // Unsigned arithmetic wraps in C; 64 bits keep small types from promoting to `int`.
                let wide = if matches!(ty, Type::Int(crate::types::IntTy::I128 | crate::types::IntTy::U128)) {
                    "unsigned __int128"
                } else {
                    "uint64_t"
                };
                let op = match op {
                    Builtin::WrappingAdd => "+",
                    Builtin::WrappingSub => "-",
                    _ => "*",
                };
                format!("(({c_type})(({wide})*({}) {op} ({wide})({})))", values[0], values[1])
            }
            Builtin::StrFromBytes => {
                let optional = self.types.c_type(&Type::Optional(Box::new(Type::Str)));
                let ok = self.temp("utf8");
                let text =
                    format!("hsy_str_from_bytes((const char*)({0})->data, ({0})->len, &{ok})", values[0]);
                format!("({{ bool {ok}; hsy_str t = {text}; ({optional}){{{ok}, t}}; }})")
            }
            Builtin::VecNew => format!("(({}){{0}})", self.types.c_type(ty)),
            Builtin::VecLen => format!("({}->len)", values[0]),
            _ => self.emit_vector_method(op, &args[0], &values),
        }
    }

    /// `print`, `panic`, `len` and `byte_len` of a `str`, and `to_c()` of a literal: they read
    /// the text where it is, a literal's at compile time.
    fn emit_text_builtin(&mut self, op: Builtin, args: &[Expr]) -> Option<String> {
        match (op, args.first().map(|a| &a.kind)) {
            (Builtin::StrLen, Some(ExprKind::Str(text))) => {
                Some(format!("((size_t){})", text.chars().count()))
            }
            (Builtin::StrBytes, Some(ExprKind::Str(text))) => Some(format!("((size_t){})", text.len())),
            (Builtin::StrToC, Some(ExprKind::Str(text))) => Some(format!("((uint8_t*){})", c_string(text))),
            (Builtin::Print, _) => {
                let (setup_text, text, clean_text) = self.read_str(&args[0]);
                let (setup_end, end, clean_end) = self.read_str(&args[1]);
                let to = self.emit_expr(&args[2]);
                Some(format!(
                    "({{ {setup_text}{setup_end}hsy_print({text}, {end}, ({to})->handle, ({to})->connection); {clean_text}{clean_end}}})"
                ))
            }
            (Builtin::Panic | Builtin::StrLen | Builtin::StrBytes, _) => {
                let (setup, pointer, cleanup) = self.read_str(&args[0]);
                let (call, result) = match op {
                    Builtin::Panic => ("hsy_panic_text", None),
                    Builtin::StrLen => ("hsy_str_chars", Some("size_t")),
                    _ => ("hsy_str_bytes", Some("size_t")),
                };
                let Some(result) = result else {
                    return Some(format!("({{ {setup}{call}({pointer}); {cleanup}}})"));
                };
                let temp = self.temp("count");
                Some(format!("({{ {setup}{result} {temp} = {call}({pointer}); {cleanup}{temp}; }})"))
            }
            _ => None,
        }
    }

    /// `shared(x)`: a block with a count of one, and the hooks of the cycle collector.
    fn emit_shared_new(&mut self, inner: &Type, value: &str) -> String {
        let block = self.types.shared_block(inner);
        let (children, release) = self.types.shared_hooks(inner);
        let temp = self.temp("shared");
        format!(
            "({{ {block}* {temp} = hsy_alloc(sizeof *{temp}); {temp}->rc = (hsy_rc){{1, 0, 0, {children}, {release}}}; \
             {temp}->value = {value}; {temp}; }})"
        )
    }

    /// `v.push(x)` and the other methods of a vector, which take it by pointer.
    fn emit_vector_method(&mut self, op: Builtin, receiver: &Expr, values: &[String]) -> String {
        let Type::Ref(vector, _) = receiver.ty() else { unreachable!("a vector method borrows its vector") };
        let vector = self.types.c_type(vector);
        let name = match op {
            Builtin::VecPush => "push",
            Builtin::VecPop => "pop",
            Builtin::VecInsert => "insert",
            Builtin::VecRemove => "remove",
            Builtin::VecClear => "clear",
            Builtin::VecSwap => "swap",
            _ => unreachable!("every other built-in has its own emission"),
        };
        format!("{vector}_{name}({})", values.join(", "))
    }

    /// Pointers, volatile accesses and SIMD vectors.
    fn emit_low_level_builtin(
        &mut self,
        op: Builtin,
        args: &[Expr],
        values: &[String],
        ty: &Type,
    ) -> Option<String> {
        Some(match op {
            Builtin::PtrOf => values[0].clone(),
            Builtin::PtrCast => format!("(({})({}))", self.types.c_type(ty), values[0]),
            Builtin::VolatileRead => format!("(*(volatile {}*)({}))", self.types.c_type(ty), values[0]),
            Builtin::VolatileWrite => {
                let target = self.types.c_type(args[1].ty());
                format!("((void)(*(volatile {target}*)({}) = {}))", values[0], values[1])
            }
            // One value for all lanes: the scalar spreads over a zero vector.
            Builtin::SimdNew if values.len() == 1 => {
                let Type::Simd(item, _) = ty else { unreachable!("SIMD type") };
                format!("((({}){{0}}) + ({})({}))", self.types.c_type(ty), self.types.c_type(item), values[0])
            }
            Builtin::SimdNew => format!("(({}){{{}}})", self.types.c_type(ty), values.join(", ")),
            _ => return None,
        })
    }

    /// `[a, b]` building a vector: pushed one by one.
    pub(super) fn emit_vector_literal(&mut self, ty: &Type, items: &[Expr]) -> String {
        let c_type = self.types.c_type(ty);
        let temp = self.temp("vector");
        let mut code = format!("({{ {c_type} {temp} = {{0}}; ");
        for item in items {
            let value = self.emit_expr(item);
            code += &format!("{c_type}_push(&{temp}, {value}); ");
        }
        code + &format!("{temp}; }})")
    }

    /// The bytes of `embed_bytes(...)`: a fresh vector copied from data in the binary.
    pub(super) fn emit_bytes(&mut self, ty: &Type, bytes: &[u8]) -> String {
        let c_type = self.types.c_type(ty);
        if bytes.is_empty() {
            return format!("(({c_type}){{0}})");
        }
        let (data, temp) = (self.temp("bytes"), self.temp("vector"));
        let listed: Vec<String> = bytes.iter().map(u8::to_string).collect();
        format!(
            "({{ static const uint8_t {data}[] = {{{}}}; {c_type} {temp} = {{0}}; {c_type}_reserve(&{temp}, {n}); memcpy({temp}.data, {data}, {n}); {temp}.len = {n}; {temp}; }})",
            listed.join(","),
            n = bytes.len(),
        )
    }

    /// `v[i]` on a vector, checked against its current length.
    pub(super) fn emit_vector_index(&mut self, base: &Expr, index: &Expr) -> String {
        let (vector, position) = (self.emit_expr(base), self.emit_expr(index));
        let check = self.helpers.call(super::helpers::Helper::Index);
        format!("{vector}.data[{check}((__int128)({position}), {vector}.len)]")
    }
}
