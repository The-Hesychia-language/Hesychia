//! Traits: their method signatures, the built-in ones, and checking that each struct
//! implements the traits it names.

use super::resolve::{FnSig, Symbol, Tables};
use crate::ast::{FnDecl, Item, Program, Receiver, TraitMethod, method_c_name};
use crate::diag::{Diagnostic, Result};
use crate::types::{IntTy, Named, Type};

pub(super) struct TraitDef {
    pub name: String,
    /// `None` for the built-in traits, known in every module.
    pub module: Option<usize>,
    pub methods: Vec<TraitMethodSig>,
}

/// A trait method, with `Self` written as `Type::Param("Self")`.
pub(super) struct TraitMethodSig {
    pub name: String,
    pub receiver: Option<Receiver>,
    pub params: Vec<Type>,
    pub ret: Type,
    pub raises: Vec<Type>,
    /// The default body, copied into structs that do not define the method.
    pub default: Option<FnDecl>,
}

/// `Equal` replaces the generated `==`; the arithmetic traits enable `+`, `-`, `*`, `/`, `%`;
/// `Drop` runs code before a value is released.
pub(super) fn builtin_traits() -> Vec<TraitDef> {
    let this = || Type::Param("Self".into());
    let other = || vec![Type::Ref(Box::new(this()), false)];
    let method = |name: &str, receiver, params, ret| TraitMethodSig {
        name: name.into(),
        receiver: Some(receiver),
        params,
        ret,
        raises: Vec::new(),
        default: None,
    };
    let operator = |trait_name: &str, name: &str| TraitDef {
        name: trait_name.into(),
        module: None,
        methods: vec![method(name, Receiver::Shared, other(), this())],
    };
    vec![
        TraitDef {
            name: "Equal".into(),
            module: None,
            methods: vec![method("equal", Receiver::Shared, other(), Type::Bool)],
        },
        operator("Add", "add"),
        operator("Sub", "sub"),
        operator("Mul", "mul"),
        operator("Div", "div"),
        operator("Mod", "mod"),
        TraitDef {
            name: "Drop".into(),
            module: None,
            methods: vec![method("drop", Receiver::Mut, vec![], Type::Void)],
        },
        // `_` stands for any type: the struct's method decides.
        TraitDef {
            name: "Index".into(),
            module: None,
            methods: vec![method("index", Receiver::Shared, vec![any_ref()], Type::Ptr(Box::new(any())))],
        },
        TraitDef {
            name: "IndexSet".into(),
            module: None,
            methods: vec![method("set_index", Receiver::Mut, vec![any(), any()], Type::Void)],
        },
        TraitDef {
            name: "Contains".into(),
            module: None,
            methods: vec![method("contains", Receiver::Shared, vec![any_ref()], Type::Bool)],
        },
        TraitDef {
            name: "Slice".into(),
            module: None,
            methods: vec![method("slice", Receiver::Shared, vec![size(), size()], this())],
        },
        TraitDef {
            name: "Show".into(),
            module: None,
            methods: vec![method("show", Receiver::Shared, vec![], Type::Str)],
        },
        TraitDef {
            name: "Order".into(),
            module: None,
            methods: vec![method("compare", Receiver::Shared, other(), Type::Int(IntTy::I32))],
        },
        TraitDef {
            name: "Len".into(),
            module: None,
            methods: vec![method("len", Receiver::Shared, vec![], size())],
        },
        TraitDef {
            name: "Hash".into(),
            module: None,
            methods: vec![method("hash", Receiver::Shared, vec![], Type::Int(IntTy::U64))],
        },
    ]
}

/// The type a built-in trait leaves to the struct's method.
fn any() -> Type {
    Type::Param("_".into())
}

fn any_ref() -> Type {
    Type::Ref(Box::new(any()), false)
}

fn size() -> Type {
    Type::Int(IntTy::Size)
}

/// `found` has the shape `wanted`, where `_` matches any type.
fn fits(found: &Type, wanted: &Type) -> bool {
    match (found, wanted) {
        (_, Type::Param(name)) if name == "_" => true,
        (Type::Ref(f, fm), Type::Ref(w, wm)) => fm == wm && fits(f, w),
        (Type::Ptr(f), Type::Ptr(w)) => fits(f, w),
        _ => found == wanted,
    }
}

/// Resolves the method signatures of every user trait.
pub(super) fn sign_traits(tables: &mut Tables, program: &Program) -> Result<()> {
    let extra = [("Self".to_string(), Type::Param("Self".into()))];
    for (module, items) in program.modules.iter().map(|m| &m.items).enumerate() {
        for item in items {
            let Item::Trait(decl) = item else { continue };
            let Some(Symbol::Trait(id)) = tables.lookup(module, &decl.name) else {
                unreachable!("registered trait")
            };
            let mut methods = Vec::new();
            for TraitMethod { decl: m, default } in &decl.methods {
                let resolve = |t| tables.resolve_in(module, t, &extra);
                let params = m.params.iter().map(|p| resolve(&p.ty)).collect::<Result<_>>()?;
                let ret = m.ret.as_ref().map_or(Ok(Type::Void), resolve)?;
                let raises = m.raises.iter().map(resolve).collect::<Result<_>>()?;
                if m.receiver == Some(Receiver::Plain) {
                    return Err(Diagnostic::error(
                        m.span,
                        "a trait method takes `&self`, `&mut self` or `owned self`",
                    ));
                }
                let default = default.then(|| m.clone());
                methods.push(TraitMethodSig {
                    name: m.name.clone(),
                    receiver: m.receiver,
                    params,
                    ret,
                    raises,
                    default,
                });
            }
            tables.traits[id].methods = methods;
        }
    }
    Ok(())
}

/// Records which traits each struct names, and copies in the default methods it lacks.
pub(super) fn attach_traits(tables: &mut Tables, program: &mut Program) -> Result<()> {
    for (module, items) in program.modules.iter_mut().map(|m| &mut m.items).enumerate() {
        for item in items.iter_mut() {
            let Item::Struct(s) = item else { continue };
            let Some(Symbol::Struct(id)) = tables.lookup(module, &s.name) else {
                if !s.generics.is_empty() {
                    continue;
                }
                unreachable!("registered struct")
            };
            let mut traits = Vec::new();
            for (name, span) in &s.traits {
                let Some(Symbol::Trait(trait_id)) = tables.lookup(module, name) else {
                    return Err(Diagnostic::error(*span, format!("unknown trait `{name}`")));
                };
                if traits.contains(&trait_id) {
                    return Err(Diagnostic::error(*span, format!("`{name}` is named twice")));
                }
                traits.push(trait_id);
                for method in &tables.traits[trait_id].methods {
                    let Some(default) = &method.default else { continue };
                    if s.methods.iter().any(|m| m.name == method.name) {
                        continue;
                    }
                    s.methods.push(default.clone());
                    tables.fns.push(placeholder());
                    tables.methods.insert((id, method.name.clone()), tables.fns.len() - 1);
                }
            }
            tables.impls.insert(id, traits);
        }
    }
    Ok(())
}

fn placeholder() -> FnSig {
    FnSig {
        params: Vec::new(),
        ret: Type::Void,
        raises: Vec::new(),
        generics: Vec::new(),
        values: Vec::new(),
        variadic: false,
        c_name: String::new(),
        receiver: None,
    }
}

/// Every method a struct's traits ask for must exist with the same signature.
pub(super) fn check_impls(tables: &mut Tables, program: &Program) -> Result<()> {
    for (module, items) in program.modules.iter().map(|m| &m.items).enumerate() {
        for item in items {
            let Item::Struct(s) = item else { continue };
            let Some(Symbol::Struct(id)) = tables.lookup(module, &s.name) else {
                if !s.generics.is_empty() {
                    continue;
                }
                unreachable!("registered struct")
            };
            let owner = Type::Struct(Named { id, name: s.name.clone() });
            for &trait_id in &tables.impls[&id].clone() {
                let def = &tables.traits[trait_id];
                for method in &def.methods {
                    let Some(&index) = tables.methods.get(&(id, method.name.clone())) else {
                        return Err(Diagnostic::error(
                            s.span,
                            format!(
                                "`{}` implements `{}` but has no method `{}`",
                                s.name, def.name, method.name
                            ),
                        )
                        .with_hint(format!("define `{}`", shown(method, &owner))));
                    };
                    let sig = &tables.fns[index];
                    if !matches(sig, method, &owner) {
                        let span =
                            s.methods.iter().find(|m| m.name == method.name).map_or(s.span, |m| m.span);
                        return Err(Diagnostic::error(
                            span,
                            format!("`{}.{}` does not match the trait `{}`", s.name, method.name, def.name),
                        )
                        .with_hint(format!("expected `{}`", shown(method, &owner))));
                    }
                }
                if def.module.is_none() {
                    let names: Vec<String> = def.methods.iter().map(|m| m.name.clone()).collect();
                    for name in names {
                        let c_name = method_c_name(module, &s.name, &name);
                        tables.types.structs[id].operators.insert(name, c_name);
                    }
                }
            }
        }
    }
    Ok(())
}

pub(super) fn matches(sig: &FnSig, method: &TraitMethodSig, owner: &Type) -> bool {
    let skip = usize::from(sig.receiver.is_some());
    let params: Vec<Type> = method.params.iter().map(|t| replace_self(t, owner)).collect();
    let raises: Vec<Type> = method.raises.iter().map(|t| replace_self(t, owner)).collect();
    sig.receiver == method.receiver
        && sig.params.len() == skip + params.len()
        && sig.params[skip..].iter().zip(&params).all(|(p, wanted)| fits(&p.ty, wanted))
        && fits(&sig.ret, &replace_self(&method.ret, owner))
        && sig.raises == raises
}

/// Writes `Self` as the implementing struct.
pub(super) fn replace_self(ty: &Type, owner: &Type) -> Type {
    ty.substitute(&[("Self".to_string(), owner.clone())])
}

/// The method as the struct should write it.
pub(super) fn shown(method: &TraitMethodSig, owner: &Type) -> String {
    let mut parts: Vec<String> = Vec::new();
    match method.receiver {
        Some(Receiver::Shared) => parts.push("&self".into()),
        Some(Receiver::Mut) => parts.push("&mut self".into()),
        Some(Receiver::Owned) => parts.push("owned self".into()),
        Some(Receiver::Plain) | None => {}
    }
    parts.extend(method.params.iter().enumerate().map(|(i, t)| format!("p{i}: {}", replace_self(t, owner))));
    let ret = replace_self(&method.ret, owner);
    let ret = if ret == Type::Void { String::new() } else { format!(" -> {ret}") };
    format!("method {}({}){ret}", method.name, parts.join(", "))
}
