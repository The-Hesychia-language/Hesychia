//! `@derive(Equal, Order, Hash, Show)` on a `manual struct` or `manual enum`: the trait methods
//! are generated from the fields (or variants), so structural equality, ordering, hashing and
//! display need not be hand-written. The methods are produced as source and parsed, so they go
//! through the same checks as any hand-written method — a field whose type lacks the trait is
//! reported there.

use crate::ast::{Decorator, EnumDecl, ExprKind, FieldDecl, FnDecl, Item, Level, Program, StructDecl, VariantDecl};
use crate::diag::{Diagnostic, Result, SourceMap, Span};
use crate::{lexer, parser};

/// The traits `@derive` understands, and the method each one generates.
const DERIVABLE: &[(&str, &str)] = &[("Equal", "equal"), ("Order", "compare"), ("Hash", "hash"), ("Show", "show")];
// An FNV-1a-style fold, with the same constants as `str.hash` so the combiners stay consistent.
const SEED: &str = "14695981039346656037";
const PRIME: &str = "1099511628211";

pub fn expand(program: &mut Program, sources: &mut SourceMap) -> Result<()> {
    for module in &mut program.modules {
        for item in &mut module.items {
            match item {
                Item::Struct(s) => expand_struct(s, sources)?,
                Item::Enum(e) => expand_enum(e, sources)?,
                _ => {}
            }
        }
    }
    Ok(())
}

/// The traits named across a type's `@derive(...)` decorators, checked to be derivable. Other
/// decorators (such as `@align`) are left untouched for their own handlers.
fn wanted(decorators: &[Decorator]) -> Result<Vec<(String, Span)>> {
    let mut out = Vec::new();
    for d in decorators.iter().filter(|d| d.name == "derive") {
        for arg in &d.args {
            let ExprKind::Name(name) = &arg.kind else {
                return Err(Diagnostic::error(arg.span, "write `@derive(Equal, Hash, ...)` with trait names"));
            };
            if !DERIVABLE.iter().any(|(t, _)| t == name) {
                return Err(Diagnostic::error(
                    arg.span,
                    format!("`{name}` cannot be derived; derivable traits are Equal, Order, Hash and Show"),
                ));
            }
            out.push((name.clone(), arg.span));
        }
    }
    Ok(out)
}

/// Checks the type is one `@derive` supports, and that it is not already given the trait by hand.
fn guard(level: Level, generics: bool, traits: &[(String, Span)], name: &str, want: &[(String, Span)]) -> Result<()> {
    if want.is_empty() {
        return Ok(());
    }
    let first = want[0].1;
    if level != Level::Manual {
        return Err(Diagnostic::error(first, "only `manual` structs and enums can derive traits"));
    }
    if generics {
        return Err(Diagnostic::error(first, "`@derive` on a generic type is not supported yet"));
    }
    for (trait_name, span) in want {
        if traits.iter().any(|(t, _)| t == trait_name) {
            return Err(Diagnostic::error(*span, format!("`{name}` already implements `{trait_name}`")));
        }
    }
    Ok(())
}

fn expand_struct(s: &mut StructDecl, sources: &mut SourceMap) -> Result<()> {
    let want = wanted(&s.decorators)?;
    guard(s.level, !s.generics.is_empty(), &s.traits, &s.name, &want)?;
    let mut body = String::new();
    for (trait_name, span) in &want {
        body += &match trait_name.as_str() {
            "Equal" => struct_equal(&s.name, &s.fields),
            "Order" => struct_order(&s.name, &s.fields),
            "Hash" => struct_hash(&s.fields),
            _ => struct_show(&s.name, &s.fields),
        };
        s.traits.push((trait_name.clone(), *span));
    }
    s.methods.extend(parse_methods(&s.name, body, sources)?);
    s.decorators.retain(|d| d.name != "derive");
    Ok(())
}

fn expand_enum(e: &mut EnumDecl, sources: &mut SourceMap) -> Result<()> {
    let want = wanted(&e.decorators)?;
    guard(e.level, !e.generics.is_empty(), &e.traits, &e.name, &want)?;
    let mut body = String::new();
    for (trait_name, span) in &want {
        body += &match trait_name.as_str() {
            "Equal" => enum_equal(&e.name, &e.variants),
            "Order" => enum_order(&e.name, &e.variants),
            "Hash" => enum_hash(&e.name, &e.variants),
            _ => enum_show(&e.name, &e.variants),
        };
        e.traits.push((trait_name.clone(), *span));
    }
    e.methods.extend(parse_methods(&e.name, body, sources)?);
    e.decorators.retain(|d| d.name != "derive");
    Ok(())
}

// ----- struct generators -----

fn struct_equal(name: &str, fields: &[FieldDecl]) -> String {
    let cond = if fields.is_empty() {
        "True".to_string()
    } else {
        fields.iter().map(|f| format!("self.{0} == other.{0}", f.name)).collect::<Vec<_>>().join(" and ")
    };
    format!("    method equal(&self, other: &{name}) -> bool:\n        return {cond}\n")
}

fn struct_order(name: &str, fields: &[FieldDecl]) -> String {
    let mut body = String::new();
    for f in fields {
        body += &format!(
            "        if self.{0} < other.{0}:\n            return -1\n        if self.{0} > other.{0}:\n            return 1\n",
            f.name
        );
    }
    body += "        return 0\n";
    format!("    method compare(&self, other: &{name}) -> i32:\n{body}")
}

fn struct_hash(fields: &[FieldDecl]) -> String {
    let mut body = format!("        mut h: u64 = {SEED}\n");
    for f in fields {
        body += &format!("        h = (h ^ self.{}.hash()).wrapping_mul({PRIME})\n", f.name);
    }
    body += "        return h\n";
    format!("    method hash(&self) -> u64:\n{body}")
}

fn struct_show(name: &str, fields: &[FieldDecl]) -> String {
    let text = if fields.is_empty() {
        name.to_string()
    } else {
        let inside = fields.iter().map(|f| format!("{0}={{self.{0}}}", f.name)).collect::<Vec<_>>().join(", ");
        format!("{name}({inside})")
    };
    format!("    method show(&self) -> str:\n        return f\"{text}\"\n")
}

// ----- enum generators -----

/// `Name.Variant` or `Name.Variant(b0, b1)`, binding each field to `prefix` + its name.
fn bound(name: &str, v: &VariantDecl, prefix: &str) -> String {
    if v.fields.is_empty() {
        return format!("{name}.{}", v.name);
    }
    let binds = v.fields.iter().map(|f| format!("{prefix}{}", f.name)).collect::<Vec<_>>().join(", ");
    format!("{name}.{}({binds})", v.name)
}

/// `Name.Variant` or `Name.Variant(_, _)`: matches the variant, ignoring any fields.
fn ignored(name: &str, v: &VariantDecl) -> String {
    if v.fields.is_empty() {
        return format!("{name}.{}", v.name);
    }
    let holes = vec!["_"; v.fields.len()].join(", ");
    format!("{name}.{}({holes})", v.name)
}

fn enum_equal(name: &str, variants: &[VariantDecl]) -> String {
    let mut arms = String::new();
    for v in variants {
        let inner = if v.fields.is_empty() {
            "                        return True\n".to_string()
        } else {
            let cond = v.fields.iter().map(|f| format!("{0} == o_{0}", f.name)).collect::<Vec<_>>().join(" and ");
            format!("                        return {cond}\n")
        };
        arms += &format!(
            "            case {}:\n                match other:\n                    case {}:\n{inner}                    case _:\n                        return False\n",
            bound(name, v, ""),
            bound(name, v, "o_"),
        );
    }
    format!("    method equal(&self, other: &{name}) -> bool:\n        match self:\n{arms}")
}

fn enum_hash(name: &str, variants: &[VariantDecl]) -> String {
    let mut arms = String::new();
    for (i, v) in variants.iter().enumerate() {
        // Fields are bound with an `f_` prefix so they never clash with the `acc` accumulator.
        let mut b = format!("                mut acc: u64 = {SEED}\n                acc = (acc ^ u64({i})).wrapping_mul({PRIME})\n");
        for f in &v.fields {
            b += &format!("                acc = (acc ^ f_{0}.hash()).wrapping_mul({PRIME})\n", f.name);
        }
        b += "                return acc\n";
        arms += &format!("            case {}:\n{b}", bound(name, v, "f_"));
    }
    format!("    method hash(&self) -> u64:\n        match self:\n{arms}")
}

fn enum_show(name: &str, variants: &[VariantDecl]) -> String {
    let mut arms = String::new();
    for v in variants {
        let text = if v.fields.is_empty() {
            format!("\"{name}.{}\"", v.name)
        } else {
            let inside = v.fields.iter().map(|f| format!("{0}={{{0}}}", f.name)).collect::<Vec<_>>().join(", ");
            format!("f\"{name}.{}({inside})\"", v.name)
        };
        arms += &format!("            case {}:\n                return {text}\n", bound(name, v, ""));
    }
    format!("    method show(&self) -> str:\n        match self:\n{arms}")
}

/// Orders by variant first (declaration order), then by fields within the same variant.
fn enum_order(name: &str, variants: &[VariantDecl]) -> String {
    let mut arms = String::new();
    for (i, vi) in variants.iter().enumerate() {
        let mut inner = String::new();
        for (j, vj) in variants.iter().enumerate() {
            let body = if i < j {
                "                        return -1\n".to_string()
            } else if i > j {
                "                        return 1\n".to_string()
            } else {
                let mut cmp = String::new();
                for f in &vi.fields {
                    cmp += &format!(
                        "                        if {0} < o_{0}:\n                            return -1\n                        if {0} > o_{0}:\n                            return 1\n",
                        f.name
                    );
                }
                cmp += "                        return 0\n";
                cmp
            };
            // The same variant binds both sides; the others are ignored on the `other` side.
            let other_pat = if i == j { bound(name, vj, "o_") } else { ignored(name, vj) };
            inner += &format!("                    case {other_pat}:\n{body}");
        }
        arms += &format!("            case {}:\n                match other:\n{inner}", bound(name, vi, ""));
    }
    format!("    method compare(&self, other: &{name}) -> i32:\n        match self:\n{arms}")
}

/// Wraps the generated method bodies in a throwaway struct, parses it, and hands back its methods.
fn parse_methods(name: &str, body: String, sources: &mut SourceMap) -> Result<Vec<FnDecl>> {
    let source = format!("manual struct __derived_{name}:\n    __tag: i32\n{body}");
    let file = sources.add(format!("<derived {name}>"), source);
    let parsed = parser::parse(lexer::lex(sources.source(file), file)?)?;
    for item in parsed.items {
        if let Item::Struct(s) = item {
            return Ok(s.methods);
        }
    }
    unreachable!("the generated source is a struct")
}
