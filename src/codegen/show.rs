//! Writing values in f-strings, as Python writes them: `Point(x=1, y=2)`, `Color.Red`,
//! `[1, 2]`, `(1, 'a')`, `None`. Inside such a value, text is quoted: `Person(name='Ann')`.

use super::types::TypeRegistry;
use crate::types::{FloatTy, Type};

impl TypeRegistry<'_> {
    /// The function that appends a written `ty` to a `str`: `void name(hsy_str* out, T* v)`.
    pub(super) fn show_fn(&mut self, ty: &Type) -> String {
        let key = format!("show {ty:?}");
        if let Some(name) = self.made.get(&key) {
            return name.clone();
        }
        self.c_type(&Type::Str);
        let value = self.c_type(ty);
        let name = format!("hsy_show_{}", self.made.len());
        self.made.insert(key, name.clone());
        self.prototypes += &format!("static void {name}(hsy_str* out, {value}* v);\n");
        let body = self.show_body(ty);
        self.functions += &format!("static void {name}(hsy_str* out, {value}* v) {{\n{body}}}\n\n");
        name
    }

    fn show_body(&mut self, ty: &Type) -> String {
        let mut out = String::new();
        let text =
            |out: &mut String, s: &str| *out += &format!("    hsy_str_append(out, \"{s}\", {});\n", s.len());
        match ty {
            Type::Int(t) if t.signed() => out += "    hsy_str_push_int(out, (__int128)*v);\n",
            Type::Int(_) => out += "    hsy_str_push_uint(out, (unsigned __int128)*v);\n",
            Type::Float(FloatTy::F32) => out += "    hsy_str_push_float32(out, *v);\n",
            Type::Float(_) => out += "    hsy_str_push_float(out, *v);\n",
            Type::Bool => out += "    hsy_str_push_bool(out, *v);\n",
            Type::Char => {
                out += "    hsy_str_append(out, \"'\", 1); hsy_str_push_char(out, *v); hsy_str_append(out, \"'\", 1);\n"
            }
            Type::Str => {
                out += "    hsy_str_append(out, \"'\", 1); hsy_str_push_str(out, v); hsy_str_append(out, \"'\", 1);\n"
            }
            Type::Heap(inner) => out += &format!("    {}(out, *v);\n", self.show_fn(inner)),
            Type::Shared(inner) => out += &format!("    {}(out, &(*v)->value);\n", self.show_fn(inner)),
            Type::Optional(inner) => {
                let show = self.show_fn(inner);
                out += &format!(
                    "    if (v->has) {show}(out, &v->value); else hsy_str_append(out, \"None\", 4);\n"
                );
            }
            Type::Vector(item) | Type::Array(item, _) => {
                let show = self.show_fn(item);
                let (len, items) = match ty {
                    Type::Array(_, n) => (n.to_string(), "items"),
                    _ => ("v->len".to_string(), "data"),
                };
                text(&mut out, "[");
                out += &format!(
                    "    for (size_t i = 0; i < {len}; i++) {{ if (i) hsy_str_append(out, \", \", 2); {show}(out, &v->{items}[i]); }}\n"
                );
                text(&mut out, "]");
            }
            Type::Tuple(items) => {
                text(&mut out, "(");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        text(&mut out, ", ");
                    }
                    out += &format!("    {}(out, &v->f{i});\n", self.show_fn(item));
                }
                if items.len() == 1 {
                    text(&mut out, ",");
                }
                text(&mut out, ")");
            }
            // A struct with `Show` writes itself.
            Type::Struct(named) if self.table.structs[named.id].operators.contains_key("show") => {
                let show = &self.table.structs[named.id].operators["show"];
                out += &format!(
                    "    hsy_str t = {show}(v);\n    hsy_str_push_str(out, &t);\n    hsy_str_drop(&t);\n"
                );
            }
            Type::Struct(named) => {
                let table = self.table;
                let def = &table.structs[named.id];
                let (name, fields) = (shown_name(&def.name), &def.fields);
                text(&mut out, &format!("{name}("));
                for (i, field) in fields.iter().enumerate() {
                    let separator = if i > 0 { ", " } else { "" };
                    text(&mut out, &format!("{separator}{}=", field.name));
                    let show = self.show_fn(&field.ty);
                    out += &format!("    {show}(out, &v->{});\n", super::expr::c_ident(&field.name));
                }
                text(&mut out, ")");
            }
            Type::Enum { named, .. } => {
                let table = self.table;
                let def = &table.enums[named.id];
                let name = shown_name(&def.name);
                let variants: Vec<_> = def.variants.iter().map(|v| (&v.name, &v.fields)).collect();
                out += "    switch (v->tag) {\n";
                for (index, (variant, fields)) in variants.iter().enumerate() {
                    let mut case = String::new();
                    text(&mut case, &format!("{name}.{variant}"));
                    if !fields.is_empty() {
                        text(&mut case, "(");
                        for (i, field) in fields.iter().enumerate() {
                            let separator = if i > 0 { ", " } else { "" };
                            text(&mut case, &format!("{separator}{}=", field.name));
                            let show = self.show_fn(&field.ty);
                            let part = format!(
                                "v->as.{}.{}",
                                super::expr::c_ident(variant),
                                super::expr::c_ident(&field.name)
                            );
                            case += &format!("    {show}(out, &{part});\n");
                        }
                        text(&mut case, ")");
                    }
                    out += &format!("    case {index}: {{\n{case}    break; }}\n");
                }
                out += "    default: break;\n    }\n";
            }
            _ => unreachable!("only writable types are shown"),
        }
        out
    }
}

/// `Pair[i32]` is written `Pair`, as its constructor is.
fn shown_name(name: &str) -> &str {
    name.split('[').next().unwrap_or(name)
}
