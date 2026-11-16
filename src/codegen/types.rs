//! C names and definitions for Hesychia types.
//!
//! Every aggregate gets a typedef'd struct. Forward declarations come first, so pointers
//! can name a type anywhere; full definitions follow in dependency order.

use std::collections::HashMap;

use crate::types::{FloatTy, Type, TypeTable};

pub struct TypeRegistry<'a> {
    pub table: &'a TypeTable,
    /// C name of each aggregate type already named, by its Hesychia spelling.
    names: HashMap<String, String>,
    defined: HashMap<String, bool>,
    /// How many definitions are being written, one inside another.
    defining: usize,
    /// Vectors whose functions wait for every type being defined to be complete: a
    /// recursive item, `vector[Tree]` inside `Tree`, is not complete yet.
    waiting_vectors: Vec<(String, Type)>,
    pub(super) forwards: String,
    pub(super) definitions: String,
    /// Prototypes and bodies of the drop, copy and vector functions.
    pub(super) prototypes: String,
    pub(super) functions: String,
    /// Names of the drop and copy functions already made, by type spelling.
    pub(super) made: HashMap<String, String>,
    pub uses_alloc: bool,
    pub uses_str: bool,
    pub uses_shared: bool,
    pub uses_threads: bool,
    pub uses_channels: bool,
    pub uses_scope: bool,
    pub uses_parallel: bool,
    pub uses_select: bool,
    /// Blocks of `shared[T]` values, defined after every other type: they hold a `T`.
    pub(super) blocks: String,
    /// The C name of the `@panic_handler` function.
    pub panic_handler: Option<String>,
}

impl<'a> TypeRegistry<'a> {
    pub fn new(table: &'a TypeTable, panic_handler: Option<String>) -> Self {
        Self {
            table,
            names: HashMap::new(),
            defined: HashMap::new(),
            defining: 0,
            waiting_vectors: Vec::new(),
            forwards: String::new(),
            definitions: String::new(),
            prototypes: String::new(),
            functions: String::new(),
            made: HashMap::new(),
            uses_alloc: false,
            uses_str: false,
            uses_shared: false,
            uses_threads: false,
            uses_channels: false,
            uses_scope: false,
            uses_parallel: false,
            uses_select: false,
            blocks: String::new(),
            panic_handler,
        }
    }

    /// The C type for a value of `ty`, with its definition emitted.
    pub fn c_type(&mut self, ty: &Type) -> String {
        let name = self.name(ty);
        if is_aggregate(ty) && !self.defined.insert(name.clone(), true).unwrap_or(false) {
            self.defining += 1;
            self.define(ty, &name);
            self.defining -= 1;
            if self.defining == 0 {
                while let Some((vector, item)) = self.waiting_vectors.pop() {
                    self.defining += 1;
                    self.vector_functions(&vector, &item);
                    self.defining -= 1;
                }
            }
        }
        name
    }

    /// The C spelling of `ty`; aggregates are only declared, enough for pointers.
    fn name(&mut self, ty: &Type) -> String {
        match ty {
            Type::Int(t) => t.c_name().into(),
            Type::Float(FloatTy::F32) => "float".into(),
            Type::Float(FloatTy::F64) => "double".into(),
            Type::Bool => "bool".into(),
            Type::Char => "uint32_t".into(),
            Type::Void => "void".into(),
            Type::Str => {
                if !self.uses_str {
                    self.uses_str = true;
                    self.uses_alloc = true;
                    self.forwards += "typedef struct hsy_str hsy_str;\n";
                    self.definitions +=
                        "struct hsy_str {\n    char* data;\n    size_t len;\n    size_t cap;\n};\n\n";
                }
                "hsy_str".into()
            }
            Type::Heap(inner) => {
                self.uses_alloc = true;
                format!("{}*", self.name(inner))
            }
            Type::Shared(inner) => format!("{}*", self.shared_block(inner)),
            Type::Simd(item, lanes) => self.simd(item, *lanes),
            Type::Thread(result) => format!("{}*", self.thread_block(result)),
            Type::Atomic(item) => self.c_type(item),
            Type::Locked => {
                self.uses_threads = true;
                "pthread_mutex_t*".into()
            }
            Type::Scope => {
                self.uses_threads = true;
                self.uses_scope = true;
                self.uses_alloc = true;
                "hsy_scope".into()
            }
            Type::Sender(_) | Type::Receiver(_) => {
                if !self.uses_channels {
                    self.forwards += "typedef struct hsy_chan hsy_chan;\n";
                }
                self.uses_channels = true;
                self.uses_threads = true;
                self.uses_alloc = true;
                "hsy_chan*".into()
            }
            Type::Ref(inner, _) if matches!(**inner, Type::Fn(..)) => self.closure(inner).0,
            Type::Ptr(inner) | Type::Ref(inner, _) => format!("{}*", self.name(inner)),
            Type::Fn(params, ret) => self.fn_pointer(params, ret),
            Type::Errors(types) => self.result_type(&Type::Void, types),
            // `Optional[ptr[T]]` is a plain pointer, `NULL` meaning `None`.
            Type::Optional(inner) if is_pointer(inner) => self.name(inner),
            _ => self.aggregate_name(ty),
        }
    }

    /// Whether the code written so far uses the type of this C name.
    pub fn is_defined(&self, c_name: &str) -> bool {
        self.defined.contains_key(c_name)
    }

    fn aggregate_name(&mut self, ty: &Type) -> String {
        // By identity: two modules may each have a struct of the same name.
        let key = format!("{ty:?}");
        if let Some(name) = self.names.get(&key) {
            return name.clone();
        }
        let name = match ty {
            // A struct made from a template is named after it and its id: `Pair[i32]` has no C spelling.
            Type::Struct(named) if named.name.contains('[') => {
                let base = named.name.split('[').next().unwrap_or_default();
                format!("hsy_m{}_{base}__{}", self.table.structs[named.id].module, named.id)
            }
            Type::Struct(named) => {
                format!("hsy_m{}_{}", self.table.structs[named.id].module, named.name)
            }
            Type::Enum { named, .. } if named.name.contains('[') => {
                let base = named.name.split('[').next().unwrap_or_default();
                format!("hsy_m{}_{base}__e{}", self.table.enums[named.id].module, named.id)
            }
            Type::Enum { named, .. } => {
                format!("hsy_m{}_{}", self.table.enums[named.id].module, named.name)
            }
            Type::Tuple(_) => format!("hsy_tuple_{}", self.names.len()),
            Type::Optional(_) => format!("hsy_optional_{}", self.names.len()),
            Type::Vector(_) => format!("hsy_vector_{}", self.names.len()),
            Type::Array(..) => format!("hsy_array_{}", self.names.len()),
            Type::Mutex(_) => format!("hsy_mutex_{}", self.names.len()),
            other => unreachable!("`{other}` has no C representation yet"),
        };
        self.forwards += &format!("typedef struct {name} {name};\n");
        self.names.insert(key, name.clone());
        self.defined.insert(name.clone(), false);
        name
    }

    /// A borrowed function, `&fn(A) -> R`: code that takes an environment first, and the
    /// environment. Returns the struct's name and the thunk that lets plain functions in.
    pub fn closure(&mut self, function: &Type) -> (String, String) {
        let Type::Fn(params, ret) = function else { unreachable!("closures wrap functions") };
        let key = format!("&{function}");
        let plain = self.fn_pointer(params, ret);
        if let Some(name) = self.names.get(&key) {
            let name = name.clone();
            return (name.clone(), format!("{name}_thunk"));
        }
        let name = format!("hsy_closure_{}", self.names.len());
        self.names.insert(key, name.clone());
        let ret_c = self.c_type(ret);
        let params_c: Vec<String> = params.iter().map(|p| self.c_type(p)).collect();
        let mut code_params = vec!["void*".to_string()];
        code_params.extend(params_c.iter().cloned());
        self.definitions += &format!(
            "typedef struct {name} {{ {ret_c} (*code)({}); void* env; }} {name};\n\n",
            code_params.join(", ")
        );
        let args: Vec<String> = (0..params.len()).map(|i| format!("a{i}")).collect();
        let typed: Vec<String> = params_c.iter().zip(&args).map(|(t, a)| format!("{t} {a}")).collect();
        let mut thunk_params = vec!["void* env".to_string()];
        thunk_params.extend(typed);
        let call = format!("(*({plain}*)env)({})", args.join(", "));
        let body = if **ret == Type::Void { format!("{call};") } else { format!("return {call};") };
        self.functions +=
            &format!("HSY_UNUSED static {ret_c} {name}_thunk({}) {{ {body} }}\n\n", thunk_params.join(", "));
        let thunk = format!("{name}_thunk");
        (name, thunk)
    }

    /// A typedef for a pointer to functions of this shape.
    fn fn_pointer(&mut self, params: &[Type], ret: &Type) -> String {
        let key = Type::Fn(params.to_vec(), Box::new(ret.clone())).to_string();
        if let Some(name) = self.names.get(&key) {
            return name.clone();
        }
        let ret_c = self.c_type(ret);
        let params_c: Vec<String> = params.iter().map(|p| self.c_type(p)).collect();
        let params_c = if params_c.is_empty() { "void".to_string() } else { params_c.join(", ") };
        let name = format!("hsy_fn_{}", self.names.len());
        self.definitions += &format!("typedef {ret_c} (*{name})({params_c});\n\n");
        self.names.insert(key, name.clone());
        name
    }

    /// Emits the struct body; member types are defined first.
    fn define(&mut self, ty: &Type, name: &str) {
        let members: Vec<(String, String)> = match ty {
            Type::Struct(named) => {
                let def = &self.table.structs[named.id];
                let live = def.drop_method().is_some();
                let fields: Vec<_> = def.fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect();
                let mut members: Vec<_> =
                    fields.into_iter().map(|(n, t)| (self.c_type(&t), super::expr::c_ident(&n))).collect();
                if live {
                    members.push(("bool".into(), "hsy_live".into()));
                }
                members
            }
            Type::Tuple(items) => {
                items.iter().enumerate().map(|(i, t)| (self.c_type(t), format!("f{i}"))).collect()
            }
            Type::Array(item, len) => vec![(self.c_type(item), format!("items[{len}]"))],
            Type::Optional(inner) => {
                vec![("bool".into(), "has".into()), (self.c_type(inner), "value".into())]
            }
            Type::Mutex(item) => {
                self.uses_threads = true;
                vec![("pthread_mutex_t".into(), "lock".into()), (self.c_type(item), "value".into())]
            }
            Type::Enum { named, .. } => self.enum_members(named.id),
            Type::Vector(item) => {
                self.uses_alloc = true;
                let item_c = self.name(item);
                vec![
                    (format!("{item_c}*"), "data".into()),
                    ("size_t".into(), "len".into()),
                    ("size_t".into(), "cap".into()),
                ]
            }
            _ => unreachable!("only aggregates are defined"),
        };
        if let Type::Vector(item) = ty {
            self.waiting_vectors.push((name.to_string(), item.as_ref().clone()));
        }
        let mut body: String = members.iter().map(|(t, n)| format!("    {t} {n};\n")).collect();
        if body.is_empty() {
            body = "    char hsy_empty;\n".into();
        }
        let attributes = match ty {
            Type::Struct(named) => {
                let def = &self.table.structs[named.id];
                let packed = if def.packed { " __attribute__((packed))" } else { "" };
                let align = def.align.map_or(String::new(), |n| format!(" __attribute__((aligned({n})))"));
                format!("{packed}{align}")
            }
            _ => String::new(),
        };
        self.definitions += &format!("struct{attributes} {name} {{\n{body}}};\n\n");
    }

    /// `uint32_t tag;` then a union with one struct per variant that has fields.
    fn enum_members(&mut self, id: usize) -> Vec<(String, String)> {
        let variants: Vec<_> = self.table.enums[id]
            .variants
            .iter()
            .filter(|v| !v.fields.is_empty())
            .map(|v| {
                (v.name.clone(), v.fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect::<Vec<_>>())
            })
            .collect();
        let mut members = vec![("uint32_t".to_string(), "tag".to_string())];
        if variants.is_empty() {
            return members;
        }
        let mut union = String::from("union {\n");
        for (name, fields) in variants {
            let fields: String = fields
                .iter()
                .map(|(n, t)| format!("{} {}; ", self.c_type(t), super::expr::c_ident(n)))
                .collect();
            union += &format!("        struct {{ {fields}}} {};\n", super::expr::c_ident(&name));
        }
        members.push((union + "    }", "as".into()));
        members
    }

    /// The struct a fallible function returns: `err` is 0 on success, or 1 + the index
    /// of the error type, whose value is in `as.e<index>`.
    pub fn result_type(&mut self, ret: &Type, errors: &[Type]) -> String {
        let key = format!("{ret:?} raises {errors:?}");
        if let Some(name) = self.names.get(&key) {
            return name.clone();
        }
        let name = format!("hsy_result_{}", self.names.len());
        self.forwards += &format!("typedef struct {name} {name};\n");
        self.names.insert(key, name.clone());
        let mut body = String::from("    uint32_t err;\n");
        if *ret != Type::Void {
            body += &format!("    {} value;\n", self.c_type(ret));
        }
        let members: String =
            errors.iter().enumerate().map(|(i, e)| format!("{} e{i}; ", self.c_type(e))).collect();
        body += &format!("    union {{ {members}}} as;\n");
        self.definitions += &format!("struct {name} {{\n{body}}};\n\n");
        name
    }

    pub fn variant_fields(&self, id: usize, index: usize) -> Vec<(String, Type)> {
        self.table.enums[id].variants[index].fields.iter().map(|f| (f.name.clone(), f.ty.clone())).collect()
    }

    pub fn variant_name(&self, id: usize, index: usize) -> String {
        self.table.enums[id].variants[index].name.clone()
    }

    pub fn render(&self) -> String {
        let threads = match (self.uses_threads, self.uses_scope) {
            (_, true) => super::runtime::SCOPE_HEADER,
            (true, false) => "#include <pthread.h>\n\n",
            _ => "",
        };
        let header =
            format!("{threads}{}", if self.uses_shared { super::runtime::SHARED_HEADER } else { "" });
        let mut out = format!("{header}{}\n{}{}", self.forwards, self.definitions, self.blocks);
        if let Some(handler) = &self.panic_handler {
            out += &format!(
                "#define HSY_PANIC_HANDLER {handler}\nHSY_UNUSED static void {handler}(hsy_str* message);\n\n"
            );
        }
        if self.uses_alloc {
            out += super::runtime::ALLOC;
        }
        if self.uses_shared {
            out += super::runtime::SHARED;
        }
        if self.uses_channels {
            out += super::runtime::CHANNEL;
        }
        if self.uses_scope {
            out += super::runtime::SCOPE;
        }
        if self.uses_parallel {
            out += super::runtime::PARALLEL;
        }
        if self.uses_select {
            out += super::runtime::SELECT;
        }
        if self.uses_str {
            out += super::runtime::STR;
        }
        out
    }

    /// Drop, copy, equality and vector functions; they may call user functions, so they
    /// come after their prototypes.
    pub fn render_functions(&self) -> String {
        format!("{}\n{}", self.prototypes, self.functions)
    }

    fn vector_functions(&mut self, v: &str, item: &Type) {
        let item_c = self.c_type(item);
        let drop = self.drop_fn(item);
        let optional_ty = Type::Optional(Box::new(item.clone()));
        let optional = self.c_type(&optional_ty);
        let (some, none) = if is_pointer(item) {
            ("x".to_string(), "NULL".to_string())
        } else {
            (format!("(({optional}){{.has = true, .value = x}})"), format!("(({optional}){{.has = false}})"))
        };
        self.functions += &super::runtime::vector(v, &item_c, drop.as_deref(), &optional, &some, &none);
    }

    /// The function that releases a value of `ty`, or `None` when nothing needs releasing.
    pub fn drop_fn(&mut self, ty: &Type) -> Option<String> {
        if !self.table.needs_drop(ty) {
            return None;
        }
        if *ty == Type::Str {
            self.c_type(ty);
            return Some("hsy_str_drop".into());
        }
        let key = format!("drop {ty:?}");
        if let Some(name) = self.made.get(&key) {
            return Some(name.clone());
        }
        let c = self.c_type(ty);
        let name = format!("hsy_drop_{}", self.made.len());
        self.made.insert(key, name.clone());
        self.prototypes += &format!("static void {name}({c}* v);\n");
        let body = self.drop_body(ty);
        self.functions += &format!("static void {name}({c}* v) {{\n{body}}}\n\n");
        Some(name)
    }

    fn drop_body(&mut self, ty: &Type) -> String {
        let mut out = String::new();
        let mut line = |text: String| out += &format!("    {text}\n");
        match ty {
            Type::Vector(item) => {
                if let Some(d) = self.drop_fn(item) {
                    line(format!("for (size_t i = 0; i < v->len; i++) {d}(&v->data[i]);"));
                }
                line("free(v->data);".into());
            }
            Type::Shared(_) => line("if (*v) hsy_rc_dec(&(*v)->rc);".into()),
            Type::Scope => line("hsy_scope_end(v);".into()),
            Type::Locked => line("pthread_mutex_unlock(*v);".into()),
            Type::Mutex(item) => {
                line("pthread_mutex_destroy(&v->lock);".into());
                if let Some(d) = self.drop_fn(item) {
                    line(format!("{d}(&v->value);"));
                }
            }
            Type::Sender(_) => line("if (*v) hsy_chan_drop_sender(*v);".into()),
            Type::Receiver(_) => line("if (*v) hsy_chan_drop_receiver(*v);".into()),
            Type::Thread(result) => {
                // A thread not joined is waited for when its handle goes away.
                let release = self.drop_fn(result).map_or(String::new(), |d| format!("{d}(&(*v)->result); "));
                line(format!(
                    "if (*v && !(*v)->scoped) {{ pthread_join((*v)->id, NULL); {release}free(*v); }}"
                ));
            }
            Type::Heap(inner) => {
                let inner_drop = self.drop_fn(inner).map_or(String::new(), |d| format!("{d}(*v); "));
                line(format!("if (*v) {{ {inner_drop}free(*v); }}"));
            }
            Type::Array(item, len) => {
                let d = self.drop_fn(item).expect("arrays that need dropping hold such items");
                line(format!("for (size_t i = 0; i < {len}; i++) {d}(&v->items[i]);"));
            }
            Type::Optional(inner) => {
                let d = self.drop_fn(inner).expect("optional that needs dropping");
                line(format!("if (v->has) {d}(&v->value);"));
            }
            Type::Errors(types) => {
                let cases: String = types
                    .iter()
                    .enumerate()
                    .filter_map(|(i, t)| {
                        self.drop_fn(t).map(|d| format!("case {}: {d}(&v->as.e{i}); break; ", i + 1))
                    })
                    .collect();
                line(format!("switch (v->err) {{ {cases}default: break; }}"));
            }
            Type::Enum { named, .. } => {
                line("switch (v->tag) {".into());
                for (index, (variant, fields)) in self.variants(named.id).into_iter().enumerate() {
                    let drops: String = fields
                        .iter()
                        .filter_map(|(f, t)| self.drop_fn(t).map(|d| format!("{d}(&v->as.{variant}.{f}); ")))
                        .collect();
                    if !drops.is_empty() {
                        line(format!("case {index}: {drops}break;"));
                    }
                }
                line("default: break;".into());
                line("}".into());
            }
            _ => {
                // A moved-from value is zeroed, live flag included: its `Drop` code does not run.
                if let Type::Struct(named) = ty
                    && let Some(user) = self.table.structs[named.id].drop_method()
                {
                    line(format!("if (v->hsy_live) {user}(v);"));
                }
                for (field, field_ty) in self.members(ty) {
                    if let Some(d) = self.drop_fn(&field_ty) {
                        line(format!("{d}(&v->{field});"));
                    }
                }
            }
        }
        out
    }

    /// The function that duplicates a value of `ty` that owns resources.
    pub fn copy_fn(&mut self, ty: &Type) -> String {
        if *ty == Type::Str {
            self.c_type(ty);
            return "hsy_str_copy".into();
        }
        let key = format!("copy {ty:?}");
        if let Some(name) = self.made.get(&key) {
            return name.clone();
        }
        let c = self.c_type(ty);
        let name = format!("hsy_copy_{}", self.made.len());
        self.made.insert(key, name.clone());
        self.prototypes += &format!("static {c} {name}({c} const* v);\n");
        let body = self.copy_body(ty);
        self.functions +=
            &format!("static {c} {name}({c} const* v) {{\n    {c} r = *v;\n{body}    return r;\n}}\n\n");
        name
    }

    /// `typedef float hsy_simd_N __attribute__((vector_size(16)));`
    fn simd(&mut self, item: &Type, lanes: u64) -> String {
        let key = format!("simd {item} {lanes}");
        if let Some(name) = self.made.get(&key) {
            return name.clone();
        }
        let name = format!("hsy_simd_{}", self.made.len());
        self.made.insert(key, name.clone());
        let size = self.table.layout(item).expect("numbers have a size").0 * lanes;
        let c = self.c_type(item);
        self.forwards += &format!("typedef {c} {name} __attribute__((vector_size({size})));\n");
        name
    }

    /// Statements that replace, in `r`, every owning part with a copy.
    fn copy_body(&mut self, ty: &Type) -> String {
        let mut out = String::new();
        let mut line = |text: String| out += &format!("    {text}\n");
        let copy = |this: &mut Self, place: &str, ty: &Type| {
            if this.table.needs_drop(ty) {
                format!("{}(&v->{place})", this.copy_fn(ty))
            } else {
                format!("v->{place}")
            }
        };
        match ty {
            // Another owner of the same value.
            Type::Shared(_) => line("if (r) hsy_rc_inc(&r->rc);".into()),
            // One more sender on the same channel.
            Type::Sender(_) => line("if (r) hsy_chan_add_sender(r);".into()),
            Type::Vector(item) => {
                let item_c = self.c_type(item);
                line("r.cap = v->len;".into());
                line(format!("r.data = hsy_alloc(v->len * sizeof({item_c}));"));
                let value = copy(self, "data[i]", item);
                line(format!("for (size_t i = 0; i < v->len; i++) r.data[i] = {value};"));
            }
            Type::Heap(inner) => {
                let value = if self.table.needs_drop(inner) {
                    format!("{}(*v)", self.copy_fn(inner))
                } else {
                    "**v".into()
                };
                line(format!("if (*v) {{ r = hsy_alloc(sizeof **v); *r = {value}; }}"));
            }
            Type::Array(item, len) => {
                let value = copy(self, "items[i]", item);
                line(format!("for (size_t i = 0; i < {len}; i++) r.items[i] = {value};"));
            }
            Type::Optional(inner) => {
                let value = copy(self, "value", inner);
                line(format!("if (v->has) r.value = {value};"));
            }
            Type::Enum { named, .. } => {
                line("switch (v->tag) {".into());
                for (index, (variant, fields)) in self.variants(named.id).into_iter().enumerate() {
                    let copies: String = fields
                        .iter()
                        .filter(|(_, t)| self.table.needs_drop(t))
                        .map(|(f, t)| {
                            format!("r.as.{variant}.{f} = {}(&v->as.{variant}.{f}); ", self.copy_fn(t))
                        })
                        .collect();
                    if !copies.is_empty() {
                        line(format!("case {index}: {copies}break;"));
                    }
                }
                line("default: break;".into());
                line("}".into());
            }
            _ => {
                for (field, field_ty) in self.members(ty) {
                    if self.table.needs_drop(&field_ty) {
                        let value = copy(self, &field, &field_ty);
                        line(format!("r.{field} = {value};"));
                    }
                }
            }
        }
        out
    }

    /// The function that compares two values of an aggregate type, part by part, or with
    /// the struct's `Equal` method.
    pub fn eq_fn(&mut self, ty: &Type) -> String {
        if let Type::Struct(named) = ty
            && let Some(user) = self.table.structs[named.id].operators.get("equal")
        {
            return user.clone();
        }
        let key = format!("eq {ty:?}");
        if let Some(name) = self.made.get(&key) {
            return name.clone();
        }
        let c = self.c_type(ty);
        let name = format!("hsy_eq_{}", self.made.len());
        self.made.insert(key, name.clone());
        self.prototypes += &format!("static bool {name}({c}* a, {c}* b);\n");
        let body = self.eq_body(ty);
        self.functions += &format!("static bool {name}({c}* a, {c}* b) {{\n{body}}}\n\n");
        name
    }

    /// `a` and `b` are pointers to the values to compare.
    fn eq_body(&mut self, ty: &Type) -> String {
        let mut out = String::new();
        let mut line = |text: String| out += &format!("    {text}\n");
        match ty {
            Type::Vector(item) => {
                line("if (a->len != b->len) return false;".into());
                let test = self.part_eq(item, "a->data[i]", "b->data[i]");
                line(format!("for (size_t i = 0; i < a->len; i++) if (!{test}) return false;"));
            }
            Type::Array(item, len) => {
                let test = self.part_eq(item, "a->items[i]", "b->items[i]");
                line(format!("for (size_t i = 0; i < {len}; i++) if (!{test}) return false;"));
            }
            Type::Heap(inner) => {
                let test = self.part_eq(inner, "(**a)", "(**b)");
                line("if (!*a || !*b) return *a == *b;".into());
                line(format!("return {test};"));
                return out;
            }
            Type::Optional(inner) => {
                line("if (a->has != b->has) return false;".into());
                let test = self.part_eq(inner, "a->value", "b->value");
                line(format!("if (a->has && !{test}) return false;"));
            }
            Type::Enum { named, .. } => {
                line("if (a->tag != b->tag) return false;".into());
                line("switch (a->tag) {".into());
                for (index, (variant, fields)) in self.variants(named.id).into_iter().enumerate() {
                    let tests: Vec<String> = fields
                        .iter()
                        .map(|(f, t)| {
                            self.part_eq(t, &format!("a->as.{variant}.{f}"), &format!("b->as.{variant}.{f}"))
                        })
                        .collect();
                    if !tests.is_empty() {
                        line(format!("case {index}: return {};", tests.join(" && ")));
                    }
                }
                line("default: break;".into());
                line("}".into());
            }
            _ => {
                for (field, field_ty) in self.members(ty) {
                    let test = self.part_eq(&field_ty, &format!("a->{field}"), &format!("b->{field}"));
                    line(format!("if (!{test}) return false;"));
                }
            }
        }
        line("return true;".into());
        out
    }

    /// A C test that two places of type `ty` hold equal values.
    fn part_eq(&mut self, ty: &Type, a: &str, b: &str) -> String {
        match ty {
            Type::Str => {
                self.c_type(ty);
                format!("hsy_str_eq(&{a}, &{b})")
            }
            Type::Enum { plain: true, .. } => format!("({a}.tag == {b}.tag)"),
            Type::Optional(inner) if is_pointer(inner) => format!("({a} == {b})"),
            _ if is_aggregate(ty) || matches!(ty, Type::Heap(_) | Type::Enum { .. }) => {
                format!("{}(&{a}, &{b})", self.eq_fn(ty))
            }
            _ => format!("({a} == {b})"),
        }
    }

    /// The C member names and types of a struct or tuple.
    pub(super) fn members(&self, ty: &Type) -> Vec<(String, Type)> {
        match ty {
            Type::Struct(named) => self.table.structs[named.id]
                .fields
                .iter()
                .map(|f| (super::expr::c_ident(&f.name), f.ty.clone()))
                .collect(),
            Type::Tuple(items) => {
                items.iter().enumerate().map(|(i, t)| (format!("f{i}"), t.clone())).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Each variant's C name and its fields' C names and types.
    pub(super) fn variants(&self, id: usize) -> Vec<(String, Vec<(String, Type)>)> {
        self.table.enums[id]
            .variants
            .iter()
            .map(|v| {
                let fields = v.fields.iter().map(|f| (super::expr::c_ident(&f.name), f.ty.clone())).collect();
                (super::expr::c_ident(&v.name), fields)
            })
            .collect()
    }
}

pub fn is_aggregate(ty: &Type) -> bool {
    match ty {
        Type::Optional(inner) => !is_pointer(inner),
        _ => matches!(
            ty,
            Type::Struct(_)
                | Type::Enum { .. }
                | Type::Tuple(_)
                | Type::Array(..)
                | Type::Vector(_)
                | Type::Mutex(_)
        ),
    }
}

pub fn is_pointer(ty: &Type) -> bool {
    matches!(ty, Type::Ptr(_))
}
