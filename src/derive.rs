//! `@derive(Equal, Order, Hash, Show)` on a `manual struct` or `manual enum`: the trait methods
//! are generated from the fields (or variants), so structural equality, ordering, hashing and
//! display need not be hand-written. The methods are produced as source and parsed, so they go
//! through the same checks as any hand-written method — a field whose type lacks the trait is
//! reported there.

use crate::ast::{Decorator, EnumDecl, ExprKind, FieldDecl, FnDecl, Item, Level, Program, StructDecl};
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

fn expand_enum(e: &EnumDecl, _sources: &mut SourceMap) -> Result<()> {
    // Enums carry no methods yet, so they cannot implement a trait at all; deriving one needs that
    // foundation first. Reject it clearly rather than silently doing nothing.
    let want = wanted(&e.decorators)?;
    if let Some((_, span)) = want.first() {
        return Err(Diagnostic::error(*span, "`@derive` on an enum is not supported yet (enums cannot carry methods)"));
    }
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
