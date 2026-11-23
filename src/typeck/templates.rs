//! Generic structs and enums: `struct Pair[T]` is a template. Each set of type arguments used makes a
//! struct of its own, with its fields and method signatures written for those types.
//!
//! Resolving a type never changes the tables, so a struct not made yet is reported as
//! missing; `Tables::resolve_mut` makes it and resolves again.

use std::cell::RefCell;
use std::collections::HashMap;

use super::comptime_fn::ComputedCall;
use super::resolve::{FnSig, ParamSig, Tables};
use crate::ast::{EnumDecl, FnDecl, Receiver, StructDecl, method_c_name};
use crate::comptime::Value;
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{EnumDef, FieldDef, Named, StructDef, Type, VariantDef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Struct,
    Enum,
}

pub(super) struct EnumTemplate {
    pub decl: EnumDecl,
    pub module: usize,
    pub params: Vec<String>,
}

pub(super) struct Template {
    pub decl: StructDecl,
    pub module: usize,
    pub params: Vec<String>,
}

/// A generic method: specialized per call like a generic function, with the bindings of
/// its struct's own type parameters, if any, and the struct it belongs to.
#[derive(Clone)]
pub(super) struct GenericMethod {
    pub module: usize,
    pub decl: FnDecl,
    pub bindings: Vec<(String, Type)>,
    pub owner: Type,
}

/// A method of a struct made from a template, waiting to have its body checked.
pub(super) struct MethodInstance {
    pub module: usize,
    pub decl: FnDecl,
    pub sig: usize,
    pub bindings: Vec<(String, Type)>,
    pub owner: Type,
}

/// A template, its type arguments, and where the type was needed.
type Missing = (Kind, usize, Vec<Type>, Span);

#[derive(Default)]
pub(super) struct Templates {
    pub list: Vec<Template>,
    pub enums: Vec<EnumTemplate>,
    /// The struct or enum made for each template and set of type arguments.
    made: HashMap<(Kind, usize, Vec<Type>), usize>,
    /// The type a resolution needed and did not find.
    missing: RefCell<Option<Missing>>,
    /// Methods of concrete structs, by signature, checked only once something calls them.
    pub unused: HashMap<usize, MethodInstance>,
    pub pending: Vec<MethodInstance>,
    /// Generic methods, by the index of their generic signature.
    pub generic_methods: HashMap<usize, GenericMethod>,
}

impl Templates {
    /// A call to `sig`: if it is a method made from a template, its body is now needed.
    pub fn called(&mut self, sig: usize) {
        if let Some(method) = self.unused.remove(&sig) {
            self.pending.push(method);
        }
    }
}

impl Templates {
    pub fn register(&mut self, decl: &StructDecl, module: usize) -> usize {
        let params = decl.generics.iter().map(|g| g.name.clone()).collect();
        self.list.push(Template { decl: decl.clone(), module, params });
        self.list.len() - 1
    }

    pub fn register_enum(&mut self, decl: &EnumDecl, module: usize) -> Result<usize> {
        let params = decl.generics.iter().map(|g| g.name.clone()).collect();
        self.enums.push(EnumTemplate { decl: decl.clone(), module, params });
        Ok(self.enums.len() - 1)
    }

    /// The struct for `template[args]`, made now if needed.
    pub fn struct_for(tables: &mut Tables, template: usize, args: Vec<Type>, span: Span) -> Result<usize> {
        match tables.templates.made.get(&(Kind::Struct, template, args.clone())) {
            Some(&id) => Ok(id),
            None => tables.instantiate(template, args, span),
        }
    }

    /// The enum for `template[args]`, made now if needed.
    pub fn enum_for(tables: &mut Tables, template: usize, args: Vec<Type>, span: Span) -> Result<usize> {
        match tables.templates.made.get(&(Kind::Enum, template, args.clone())) {
            Some(&id) => Ok(id),
            None => tables.instantiate_enum(template, args, span),
        }
    }

    /// The template and type arguments a struct or enum was made from.
    pub fn origin(&self, kind: Kind, id: usize) -> Option<(usize, &Vec<Type>)> {
        self.made
            .iter()
            .find(|((k, _, _), made)| *k == kind && **made == id)
            .map(|((_, t, args), _)| (*t, args))
    }

    /// The name and parameter count of a template.
    fn template_shape(&self, kind: Kind, template: usize) -> (&str, usize) {
        match kind {
            Kind::Struct => (&self.list[template].decl.name, self.list[template].params.len()),
            Kind::Enum => (&self.enums[template].decl.name, self.enums[template].params.len()),
        }
    }

    pub fn enum_type(&self, template: usize, id: usize, args: &[Type]) -> Type {
        let decl = &self.enums[template].decl;
        let plain = decl.variants.iter().all(|v| v.fields.is_empty());
        Type::Enum { named: Named { id, name: display(&decl.name, args) }, plain }
    }

    /// The struct for `template[args]`, if it was made already.
    pub fn find(&self, kind: Kind, template: usize, args: Vec<Type>, span: Span) -> Result<Type> {
        let (name, count) = self.template_shape(kind, template);
        if args.len() != count {
            return Err(Diagnostic::error(span, format!("`{name}` takes {count} type argument(s)")));
        }
        if let Some(&id) = self.made.get(&(kind, template, args.clone())) {
            return Ok(match kind {
                Kind::Struct => Type::Struct(Named { id, name: display(name, &args) }),
                Kind::Enum => self.enum_type(template, id, &args),
            });
        }
        let shown = display(name, &args);
        *self.missing.borrow_mut() = Some((kind, template, args, span));
        Err(Diagnostic::error(span, format!("`{shown}` is not known here yet")))
    }
}

fn display(name: &str, args: &[Type]) -> String {
    let args: Vec<String> = args.iter().map(std::string::ToString::to_string).collect();
    format!("{name}[{}]", args.join(", "))
}

impl Tables {
    /// Resolves a type, making the structs it needs from their templates.
    pub fn resolve_mut(
        &mut self,
        module: usize,
        texpr: &crate::ast::TypeExpr,
        extra: &[(String, Type)],
    ) -> Result<Type> {
        loop {
            let result = self.resolve_in(module, texpr, extra);
            if !self.make_missing(result.is_err())? {
                return result;
            }
        }
    }

    /// After a failed resolution, makes the struct or enum it found missing; false when
    /// nothing was, and the failure stands.
    pub fn make_missing(&mut self, failed: bool) -> Result<bool> {
        let missing = self.templates.missing.borrow_mut().take();
        match missing {
            Some((Kind::Struct, template, args, span)) if failed => {
                self.instantiate(template, args, span)?;
            }
            Some((Kind::Enum, template, args, span)) if failed => {
                self.instantiate_enum(template, args, span)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Makes `template[args]`: a struct with its fields and method signatures. Methods of
    /// concrete structs are queued to have their bodies checked.
    pub fn instantiate(&mut self, template: usize, args: Vec<Type>, span: Span) -> Result<usize> {
        let Template { decl, module, params } = &self.templates.list[template];
        let (decl, module, params) = (decl.clone(), *module, params.clone());
        let name = display(&decl.name, &args);
        let def = StructDef {
            name: name.clone(),
            module,
            doc: decl.doc.clone(),
            fields: Vec::new(),
            operators: HashMap::new(),
            packed: false,
            align: None,
        };
        self.types.structs.push(def);
        let id = self.types.structs.len() - 1;
        self.templates.made.insert((Kind::Struct, template, args.clone()), id);
        let owner = Type::Struct(Named { id, name: name.clone() });
        let bindings: Vec<(String, Type)> = params.iter().cloned().zip(args.iter().cloned()).collect();
        let mut extra = bindings.clone();
        extra.push(("Self".into(), owner.clone()));
        let mut fields = Vec::new();
        for field in &decl.fields {
            let ty = self.resolve_mut(module, &field.ty, &extra)?;
            if matches!(ty, Type::Ref(..)) {
                return Err(Diagnostic::error(field.span, "a field cannot hold a reference"));
            }
            fields.push(FieldDef {
                name: field.name.clone(),
                ty,
                private: field.private,
                default: None,
                doc: field.doc.clone(),
            });
        }
        self.types.structs[id].fields = fields;
        let concrete = !args.iter().any(|t| self.has_param(t));
        if concrete {
            self.check_bounds(&decl.generics, &decl.name, module, &args, span)?;
        }
        let traits = self.trait_ids(&decl.traits, module)?;
        let mut methods = decl.methods.clone();
        for &trait_id in &traits {
            for method in &self.traits[trait_id].methods {
                if let Some(default) = &method.default
                    && !methods.iter().any(|m| m.name == method.name)
                {
                    methods.push(default.clone());
                }
            }
        }
        let c_owner = format!("{}__{id}", decl.name);
        for method in &methods {
            let c_name = method_c_name(module, &c_owner, &method.name);
            let sig = self.method_signature(module, method, &extra, &owner, c_name)?;
            self.fns.push(sig);
            let index = self.fns.len() - 1;
            self.methods.insert((id, method.name.clone()), index);
            if !method.generics.is_empty() {
                let entry = GenericMethod {
                    module,
                    decl: method.clone(),
                    bindings: bindings.clone(),
                    owner: owner.clone(),
                };
                self.templates.generic_methods.insert(index, entry);
            } else if concrete {
                let decl = FnDecl { name: format!("{c_owner}_{}", method.name), ..method.clone() };
                let pending = MethodInstance {
                    module,
                    decl,
                    sig: index,
                    bindings: bindings.clone(),
                    owner: owner.clone(),
                };
                self.templates.unused.insert(index, pending);
            }
        }
        self.impl_traits(id, &owner, traits, module, &c_owner, span)?;
        Ok(id)
    }

    /// The traits named in `struct Name[T](A, B)`.
    fn trait_ids(&self, traits: &[(String, Span)], module: usize) -> Result<Vec<usize>> {
        traits
            .iter()
            .map(|(name, span)| match self.lookup(module, name) {
                Some(super::resolve::Symbol::Trait(id)) => Ok(id),
                _ => Err(Diagnostic::error(*span, format!("unknown trait `{name}`"))),
            })
            .collect()
    }

    /// Each type argument must implement the traits its parameter is bounded by.
    fn check_bounds(
        &self,
        generics: &[crate::ast::GenericParam],
        owner: &str,
        module: usize,
        args: &[Type],
        span: Span,
    ) -> Result<()> {
        for (param, arg) in generics.iter().zip(args) {
            for trait_id in self.trait_ids(&param.bounds, module)? {
                if !self.satisfies(arg, trait_id) {
                    let trait_name = &self.traits[trait_id].name;
                    return Err(Diagnostic::error(
                        span,
                        format!(
                            "`{arg}` does not implement `{trait_name}`, required by `{}` of `{}`",
                            param.name, owner
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Records the traits of a struct made from a template and checks its methods against
    /// them. Methods of built-in traits run without an explicit call, so they are needed now.
    fn impl_traits(
        &mut self,
        id: usize,
        owner: &Type,
        traits: Vec<usize>,
        module: usize,
        c_owner: &str,
        span: Span,
    ) -> Result<()> {
        let concrete = !self.has_param(owner);
        for &trait_id in &traits {
            let def = &self.traits[trait_id];
            for method in &def.methods {
                let Some(&index) = self.methods.get(&(id, method.name.clone())) else {
                    return Err(Diagnostic::error(
                        span,
                        format!("`{owner}` implements `{}` but has no method `{}`", def.name, method.name),
                    )
                    .with_hint(format!("define `{}`", super::traits::shown(method, owner))));
                };
                if !super::traits::matches(&self.fns[index], method, owner) {
                    return Err(Diagnostic::error(
                        span,
                        format!("`{owner}.{}` does not match the trait `{}`", method.name, def.name),
                    )
                    .with_hint(format!("expected `{}`", super::traits::shown(method, owner))));
                }
            }
            if def.module.is_none() {
                let names: Vec<String> = def.methods.iter().map(|m| m.name.clone()).collect();
                for name in names {
                    let index = self.methods[&(id, name.clone())];
                    if concrete {
                        self.templates.called(index);
                    }
                    let c_name = method_c_name(module, c_owner, &name);
                    self.types.structs[id].operators.insert(name, c_name);
                }
            }
        }
        self.impls.insert(id, traits);
        Ok(())
    }

    /// Whether a concrete type meets a trait, as a type argument.
    pub fn satisfies(&self, ty: &Type, trait_id: usize) -> bool {
        let def = &self.traits[trait_id];
        if let Type::Struct(named) = ty
            && self.impls.get(&named.id).is_some_and(|t| t.contains(&trait_id))
        {
            return true;
        }
        if def.module.is_some() {
            return false;
        }
        match def.name.as_str() {
            "Equal" => !matches!(ty, Type::Ref(..) | Type::Void),
            "Add" => ty.is_numeric() || *ty == Type::Str,
            "Sub" | "Mul" | "Div" => ty.is_numeric(),
            "Mod" => ty.as_int().is_some(),
            "Order" => ty.is_numeric() || matches!(ty, Type::Char | Type::Str),
            "Hash" => ty.as_int().is_some() || matches!(ty, Type::Bool | Type::Char | Type::Str),
            _ => false,
        }
    }

    /// Makes `template[args]` for an enum: its variants with their fields' types.
    pub fn instantiate_enum(&mut self, template: usize, args: Vec<Type>, span: Span) -> Result<usize> {
        let EnumTemplate { decl, module, params } = &self.templates.enums[template];
        let (decl, module, params) = (decl.clone(), *module, params.clone());
        let name = display(&decl.name, &args);
        let plain = decl.variants.iter().all(|v| v.fields.is_empty());
        self.types.enums.push(EnumDef { name, module, doc: decl.doc.clone(), plain, variants: Vec::new() });
        let id = self.types.enums.len() - 1;
        self.templates.made.insert((Kind::Enum, template, args.clone()), id);
        let mut extra: Vec<(String, Type)> = params.into_iter().zip(args.iter().cloned()).collect();
        extra.push(("Self".into(), self.templates.enum_type(template, id, &args)));
        let mut variants = Vec::new();
        for variant in &decl.variants {
            let mut fields = Vec::new();
            for field in &variant.fields {
                let ty = self.resolve_mut(module, &field.ty, &extra)?;
                if matches!(ty, Type::Ref(..)) {
                    return Err(Diagnostic::error(field.span, "a field cannot hold a reference"));
                }
                let doc = field.doc.clone();
                fields.push(FieldDef { name: field.name.clone(), ty, private: false, default: None, doc });
            }
            variants.push(VariantDef { name: variant.name.clone(), fields, doc: variant.doc.clone() });
        }
        self.types.enums[id].variants = variants;
        if !args.iter().any(|t| self.has_param(t)) {
            self.check_bounds(&decl.generics, &decl.name, module, &args, span)?;
        }
        Ok(id)
    }

    fn method_signature(
        &mut self,
        module: usize,
        method: &FnDecl,
        extra: &[(String, Type)],
        owner: &Type,
        c_name: String,
    ) -> Result<FnSig> {
        let mut generics = Vec::new();
        let mut extra = extra.to_vec();
        for param in &method.generics {
            generics.push((param.name.clone(), self.trait_ids(&param.bounds, module)?));
            extra.push((param.name.clone(), Type::Param(param.name.clone())));
        }
        let extra = extra.as_slice();
        let mut params = Vec::new();
        if let Some(receiver) = method.receiver {
            let ty = match receiver {
                Receiver::Shared => Type::Ref(Box::new(owner.clone()), false),
                Receiver::Mut => Type::Ref(Box::new(owner.clone()), true),
                Receiver::Owned => owner.clone(),
                Receiver::Plain => {
                    return Err(Diagnostic::error(
                        method.span,
                        "a `manual` method takes `&self`, `&mut self` or `owned self`",
                    ));
                }
            };
            params.push(ParamSig { name: "self".into(), ty, default: None });
        }
        for p in &method.params {
            let ty = self.resolve_mut(module, &p.ty, extra)?;
            super::check_binding_mode(p, &ty)?;
            if p.default.is_some() {
                return Err(Diagnostic::error(
                    p.span,
                    "methods of generic structs take no default values yet",
                ));
            }
            params.push(ParamSig { name: p.name.clone(), ty, default: None });
        }
        let ret = match &method.ret {
            Some(texpr) => self.resolve_mut(module, texpr, extra)?,
            None => Type::Void,
        };
        if matches!(ret, Type::Ref(..)) {
            return Err(Diagnostic::error(method.span, "functions cannot return references"));
        }
        let raises =
            method.raises.iter().map(|t| self.resolve_mut(module, t, extra)).collect::<Result<_>>()?;
        Ok(FnSig {
            params,
            ret,
            raises,
            generics,
            values: Vec::new(),
            variadic: false,
            c_name,
            receiver: method.receiver,
        })
    }

    /// Replaces type parameters, also inside the arguments of structs made from templates.
    pub fn substitute(&mut self, ty: &Type, bindings: &[(String, Type)], span: Span) -> Result<Type> {
        let sub = |this: &mut Self, t: &Type| this.substitute(t, bindings, span).map(Box::new);
        Ok(match ty {
            Type::Struct(named) => match self.templates.origin(Kind::Struct, named.id) {
                Some((template, args)) => {
                    let args = args.clone();
                    let args = args
                        .iter()
                        .map(|a| self.substitute(a, bindings, span))
                        .collect::<Result<Vec<_>>>()?;
                    let name = display(&self.templates.list[template].decl.name, &args);
                    let id = Templates::struct_for(self, template, args, span)?;
                    Type::Struct(Named { id, name })
                }
                None => ty.clone(),
            },
            Type::Enum { named, .. } => match self.templates.origin(Kind::Enum, named.id) {
                Some((template, args)) => {
                    let args = args.clone();
                    let args = args
                        .iter()
                        .map(|a| self.substitute(a, bindings, span))
                        .collect::<Result<Vec<_>>>()?;
                    let id = Templates::enum_for(self, template, args.clone(), span)?;
                    self.templates.enum_type(template, id, &args)
                }
                None => ty.clone(),
            },
            Type::Ref(inner, m) => Type::Ref(sub(self, inner)?, *m),
            Type::Ptr(inner) => Type::Ptr(sub(self, inner)?),
            Type::Optional(inner) => Type::Optional(sub(self, inner)?),
            Type::Vector(inner) => Type::Vector(sub(self, inner)?),
            Type::Heap(inner) => Type::Heap(sub(self, inner)?),
            Type::Shared(inner) => Type::Shared(sub(self, inner)?),
            Type::Simd(item, n) => Type::Simd(sub(self, item)?, *n),
            Type::Thread(item) => Type::Thread(sub(self, item)?),
            Type::Sender(item) => Type::Sender(sub(self, item)?),
            Type::Receiver(item) => Type::Receiver(sub(self, item)?),
            Type::Atomic(item) => Type::Atomic(sub(self, item)?),
            Type::Array(inner, n) => Type::Array(sub(self, inner)?, *n),
            Type::Tuple(items) => {
                Type::Tuple(items.iter().map(|t| self.substitute(t, bindings, span)).collect::<Result<_>>()?)
            }
            Type::Fn(params, ret) => {
                let params =
                    params.iter().map(|t| self.substitute(t, bindings, span)).collect::<Result<_>>()?;
                Type::Fn(params, sub(self, ret)?)
            }
            Type::Computed { call, .. } => {
                let ComputedCall { callee, args } = self.computed.borrow()[*call].clone();
                let mut values = Vec::new();
                for arg in args {
                    values.push(match arg {
                        Value::Type(t) => Value::Type(self.substitute(&t, bindings, span)?),
                        value => value,
                    });
                }
                let target = self.target.clone().expect("set when the tables are built");
                loop {
                    let result = self.type_call(&target, callee.clone(), values.clone(), span);
                    if !self.make_missing(result.is_err())? {
                        break result?;
                    }
                }
            }
            other => other.substitute(bindings),
        })
    }

    /// Whether a type parameter appears in the type, including in struct arguments.
    pub fn has_param(&self, ty: &Type) -> bool {
        match ty {
            Type::Struct(named) => self
                .templates
                .origin(Kind::Struct, named.id)
                .is_some_and(|(_, args)| args.iter().any(|a| self.has_param(a))),
            Type::Enum { named, .. } => self
                .templates
                .origin(Kind::Enum, named.id)
                .is_some_and(|(_, args)| args.iter().any(|a| self.has_param(a))),
            Type::Ref(inner, _)
            | Type::Ptr(inner)
            | Type::Optional(inner)
            | Type::Vector(inner)
            | Type::Heap(inner)
            | Type::Shared(inner)
            | Type::Simd(inner, _)
            | Type::Thread(inner)
            | Type::Sender(inner)
            | Type::Receiver(inner)
            | Type::Atomic(inner) => self.has_param(inner),
            Type::Array(inner, _) => self.has_param(inner),
            Type::ArrayParam(..) => true,
            Type::Tuple(items) => items.iter().any(|t| self.has_param(t)),
            Type::Fn(params, ret) => params.iter().any(|t| self.has_param(t)) || self.has_param(ret),
            Type::Param(_) | Type::Computed { .. } => true,
            _ => false,
        }
    }
}
