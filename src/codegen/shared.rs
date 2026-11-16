//! `shared[T]`: a block holding a reference count and the value, and the functions the
//! cycle collector needs for each `T`: visit the `shared` values it holds, release it.

use super::types::TypeRegistry;
use crate::types::Type;

impl TypeRegistry<'_> {
    /// The C struct of the block behind a `shared[T]`.
    pub(super) fn shared_block(&mut self, inner: &Type) -> String {
        self.uses_shared = true;
        self.uses_alloc = true;
        let key = format!("block {inner:?}");
        if let Some(name) = self.made.get(&key) {
            return name.clone();
        }
        let name = format!("hsy_shared_{}", self.made.len());
        self.made.insert(key, name.clone());
        self.forwards += &format!("typedef struct {name} {name};\n");
        let value = self.c_type(inner);
        self.blocks += &format!("struct {name} {{\n    hsy_rc rc;\n    {value} value;\n}};\n\n");
        name
    }

    /// The functions a new block of `shared[T]` points to: visit its `shared` children, and
    /// release what its value owns.
    pub(super) fn shared_hooks(&mut self, inner: &Type) -> (String, String) {
        let block = self.shared_block(inner);
        let key = format!("hooks {inner:?}");
        if let Some(names) = self.made.get(&key) {
            let (children, release) = names.split_once(' ').expect("two names");
            return (children.into(), release.into());
        }
        let children = match self.visit_fn(inner) {
            Some(visit) => {
                let name = format!("hsy_children_{}", self.made.len());
                self.functions += &format!(
                    "static void {name}(hsy_rc* s, void (*f)(hsy_rc*)) {{ {visit}(&(({block}*)s)->value, f); }}\n\n"
                );
                self.prototypes += &format!("static void {name}(hsy_rc* s, void (*f)(hsy_rc*));\n");
                name
            }
            None => "hsy_rc_no_children".into(),
        };
        let release = match self.drop_fn(inner) {
            Some(drop) => {
                let name = format!("hsy_release_{}", self.made.len());
                self.functions +=
                    &format!("static void {name}(hsy_rc* s) {{ {drop}(&(({block}*)s)->value); }}\n\n");
                self.prototypes += &format!("static void {name}(hsy_rc* s);\n");
                name
            }
            None => "hsy_rc_no_release".into(),
        };
        self.made.insert(key, format!("{children} {release}"));
        (children, release)
    }

    /// The function that calls `f` on each `shared` value directly held by a `ty`, or
    /// `None` when it holds none.
    fn visit_fn(&mut self, ty: &Type) -> Option<String> {
        if !self.table.contains_shared(ty) {
            return None;
        }
        let key = format!("visit {ty:?}");
        if let Some(name) = self.made.get(&key) {
            return Some(name.clone());
        }
        let c = self.c_type(ty);
        let name = format!("hsy_visit_{}", self.made.len());
        self.made.insert(key, name.clone());
        self.prototypes += &format!("static void {name}({c}* v, void (*f)(hsy_rc*));\n");
        let body = self.visit_body(ty);
        self.functions += &format!("static void {name}({c}* v, void (*f)(hsy_rc*)) {{\n{body}}}\n\n");
        Some(name)
    }

    fn visit_body(&mut self, ty: &Type) -> String {
        let mut out = String::new();
        let mut visit = |this: &mut Self, item: &Type, place: String| {
            if let Some(v) = this.visit_fn(item) {
                out += &format!("    {v}({place}, f);\n");
            }
        };
        match ty {
            Type::Shared(_) => return "    if (*v) f(&(*v)->rc);\n".into(),
            Type::Vector(item) => {
                let v = self.visit_fn(item).expect("holds shared values");
                return format!("    for (size_t i = 0; i < v->len; i++) {v}(&v->data[i], f);\n");
            }
            Type::Array(item, len) => {
                let v = self.visit_fn(item).expect("holds shared values");
                return format!("    for (size_t i = 0; i < {len}; i++) {v}(&v->items[i], f);\n");
            }
            Type::Heap(inner) => {
                let v = self.visit_fn(inner).expect("holds shared values");
                return format!("    if (*v) {v}(*v, f);\n");
            }
            Type::Optional(inner) => {
                let v = self.visit_fn(inner).expect("holds shared values");
                return format!("    if (v->has) {v}(&v->value, f);\n");
            }
            Type::Enum { named, .. } => {
                let mut cases = String::new();
                for (index, (variant, fields)) in self.variants(named.id).into_iter().enumerate() {
                    let calls: String = fields
                        .iter()
                        .filter_map(|(f, t)| {
                            self.visit_fn(t).map(|v| format!("{v}(&v->as.{variant}.{f}, f); "))
                        })
                        .collect();
                    if !calls.is_empty() {
                        cases += &format!("case {index}: {calls}break; ");
                    }
                }
                return format!("    switch (v->tag) {{ {cases}default: break; }}\n");
            }
            _ => {}
        }
        for (field, field_ty) in self.members(ty) {
            visit(self, &field_ty, format!("&v->{field}"));
        }
        out
    }
}
