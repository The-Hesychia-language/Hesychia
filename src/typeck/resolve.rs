//! Module-level resolution: `comptime` items, names, imports, types and signatures.
//!
//! 1. Each module's `comptime` items are expanded and its names registered.
//! 2. Namespaces are built from those names and the imports.
//! 3. Types, fields, variants and signatures are resolved through the namespaces.

use std::collections::HashMap;

use crate::ast::ExprKind;
use crate::ast::{
    Builtin, ComptimeAssert, ComptimeConst, ComptimeIf, EnumDecl, Expr, ExtendDecl, FnDecl, Import,
    ImportKind, Item, Level, Module, Param, Program, Receiver, StructDecl, TypeExpr, fn_c_name,
    method_c_name,
};
use crate::comptime::{self, Target, Value};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{EnumDef, FieldDef, IntTy, Named, StructDef, Type, TypeTable, VariantDef};

use super::comptime_fn::{ComptimeFn, ModuleEnv};
use super::scalar_type;
use super::templates::Kind;

/// A compile-time value parameter, `[N: size]`: its name and integer type.
pub(super) type ValueParam = (String, Type);

/// A type parameter and the traits it must implement.
pub(super) type TypeParam = (String, Vec<usize>);

pub(super) struct FnSig {
    pub params: Vec<ParamSig>,
    pub ret: Type,
    /// The error types of `raises`, in declaration order.
    pub raises: Vec<Type>,
    /// Type parameters and the traits each must implement.
    pub generics: Vec<(String, Vec<usize>)>,
    /// Which of the parameters are compile-time values, such as `[N: size]`, with their type.
    pub values: Vec<(String, Type)>,
    pub variadic: bool,
    pub c_name: String,
    pub receiver: Option<Receiver>,
}

#[derive(Clone)]
pub(super) struct ParamSig {
    pub name: String,
    pub ty: Type,
    pub default: Option<Value>,
}

pub(super) struct Constant {
    pub ty: Type,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Symbol {
    Fn(usize),
    Const(usize),
    Module(usize),
    Struct(usize),
    Enum(usize),
    Trait(usize),
    /// A struct with type parameters, by index in `Tables::templates`.
    GenericStruct(usize),
    /// An enum with type parameters, by index in `Tables::templates`.
    GenericEnum(usize),
    /// A function of the standard library, implemented by the runtime.
    Builtin(Builtin),
    /// A module constant, by index in `Tables::globals`.
    Global(usize),
    /// A `comptime fn`, by index in `Tables::comptime_fns`.
    ComptimeFn(usize),
}

pub(super) struct Export {
    pub symbol: Symbol,
    private: bool,
    span: Span,
}

/// Everything known about the program's top-level names.
#[derive(Default)]
pub(super) struct Tables {
    pub fns: Vec<FnSig>,
    /// The standard library's module constants, such as `io.stdout`.
    pub globals: Vec<Global>,
    /// Methods the standard library adds to a built-in type: (`str` or `vector`, method)
    /// to the module and name of the function behind it.
    pub extensions: HashMap<(String, String), (usize, String)>,
    pub consts: Vec<Constant>,
    pub types: TypeTable,
    pub traits: Vec<super::traits::TraitDef>,
    pub templates: super::templates::Templates,
    /// The `managed` functions, by signature index; `manual` code cannot call them.
    pub managed: std::collections::HashSet<usize>,
    /// Parameters of `managed` functions whose mode is inferred, by signature index:
    /// their positions, and their type before borrowing.
    pub inferred: HashMap<usize, Vec<(usize, Type)>>,
    /// Where a `managed` function keeps a parameter inferred `owned`: (function, position).
    pub kept_at: HashMap<(usize, usize), Span>,
    /// The `@panic_handler` function, by signature index.
    pub panic_handler: Option<usize>,
    /// The traits each struct implements, by struct id.
    pub impls: HashMap<usize, Vec<usize>>,
    /// Methods of each struct, by struct id then name.
    pub methods: HashMap<(usize, String), usize>,
    /// The traits each enum implements, by enum id (a separate id space from structs).
    pub enum_impls: HashMap<usize, Vec<usize>>,
    /// Methods of each enum, by enum id then name.
    pub enum_methods: HashMap<(usize, String), usize>,
    pub exports: Vec<HashMap<String, Export>>,
    namespaces: Vec<HashMap<String, Symbol>>,
    pub module_paths: Vec<String>,
    pub comptime_fns: Vec<super::comptime_fn::ComptimeFn>,
    /// The calls of `Type::Computed` types, by index.
    pub computed: std::cell::RefCell<Vec<super::comptime_fn::ComputedCall>>,
    /// The machine the program is built for, which `comptime` code can ask about.
    pub target: Option<Target>,
}

impl Tables {
    /// A name as seen from inside `module`: its own items and its imports.
    pub fn lookup(&self, module: usize, name: &str) -> Option<Symbol> {
        self.symbol(module, name)
    }

    /// A name as seen from inside `module`, before its imports are known: its own items.
    pub fn symbol(&self, module: usize, name: &str) -> Option<Symbol> {
        match self.namespaces.get(module) {
            Some(namespace) => namespace.get(name).copied(),
            None => self.exports.get(module)?.get(name).map(|e| e.symbol),
        }
    }

    /// A name reached through another module, which must not be private.
    pub fn export(&self, module: usize, name: &str, span: Span) -> Result<Symbol> {
        let path = &self.module_paths[module];
        let export = self.exports[module]
            .get(name)
            .ok_or_else(|| Diagnostic::error(span, format!("module `{path}` has no item `{name}`")))?;
        if export.private {
            return Err(Diagnostic::error(span, format!("`{name}` is private to module `{path}`")));
        }
        Ok(export.symbol)
    }

    /// Turns a written type into a semantic type, as seen from `module`.
    /// Resolves with extra names in scope, such as `Self` or type parameters.
    pub fn resolve_in(&self, module: usize, texpr: &TypeExpr, extra: &[(String, Type)]) -> Result<Type> {
        let resolve = |t: &TypeExpr| self.resolve_in(module, t, extra);
        let (name, args, span) = match texpr {
            TypeExpr::Ref { mutable, inner, .. } => {
                return Ok(Type::Ref(Box::new(resolve(inner)?), *mutable));
            }
            TypeExpr::Tuple(items, _) => {
                return items.iter().map(resolve).collect::<Result<_>>().map(Type::Tuple);
            }
            TypeExpr::Fn { params, ret, .. } => {
                let params = params.iter().map(resolve).collect::<Result<_>>()?;
                let ret = ret.as_deref().map_or(Ok(Type::Void), resolve)?;
                return Ok(Type::Fn(params, Box::new(ret)));
            }
            TypeExpr::Size(_, span) => {
                return Err(Diagnostic::error(*span, "expected a type, found a number"));
            }
            TypeExpr::Call { name, args, span } => {
                let args = args.iter().map(|a| self.comptime_arg(module, a, extra)).collect::<Result<_>>()?;
                let target = self.target.as_ref().expect("set when the tables are built");
                return self.computed_type(target, module, name, args, *span);
            }
            TypeExpr::Named { name, args, span } => (name.as_str(), args.as_slice(), *span),
        };
        if let Some(ty) = scalar_type(texpr) {
            return Ok(ty);
        }
        if let (Some((_, ty)), []) = (extra.iter().find(|(n, _)| n == name), args) {
            if let Type::Family(family) = ty {
                return Err(Diagnostic::error(
                    span,
                    format!("`{name}` is `{family}`, which needs its type arguments here"),
                ));
            }
            return Ok(ty.clone());
        }
        match self.lookup(module, name) {
            Some(Symbol::GenericStruct(template)) => {
                let args = args.iter().map(resolve).collect::<Result<Vec<_>>>()?;
                return self.templates.find(Kind::Struct, template, args, span);
            }
            Some(Symbol::GenericEnum(template)) => {
                let args = args.iter().map(resolve).collect::<Result<Vec<_>>>()?;
                return self.templates.find(Kind::Enum, template, args, span);
            }
            _ => {}
        }
        match (name, args) {
            ("Self", []) => Err(Diagnostic::error(span, "`Self` is only known inside a struct or a trait")),
            ("str", []) => Ok(Type::Str),
            // A thread whose function returns nothing.
            ("thread", []) => Ok(Type::Thread(Box::new(Type::Void))),
            ("array", [item, size]) => {
                let item = Box::new(resolve(item)?);
                self.array_type(module, item, size, extra, span)
            }
            ("SIMD", [item, TypeExpr::Size(lanes, _)]) => {
                let item = resolve(item)?;
                if !item.is_numeric() || !lanes.is_power_of_two() {
                    return Err(Diagnostic::error(
                        span,
                        "`SIMD[T, N]` takes a number type and a power of two",
                    )
                    .with_hint("for example `SIMD[f32, 4]`"));
                }
                Ok(Type::Simd(Box::new(item), *lanes))
            }
            (_, [item]) if wrapper(name, Type::Void).is_some() => {
                let ty = wrapper(name, resolve(item)?).expect("a wrapper type");
                if let Type::Atomic(item) = &ty
                    && item.as_int().is_none()
                    && **item != Type::Bool
                {
                    return Err(Diagnostic::error(span, "`atomic[T]` holds an integer or a `bool`"));
                }
                Ok(ty)
            }
            (_, []) => self.user_type(module, name, span),
            _ => Err(Diagnostic::error(span, format!("unknown type `{name}`"))),
        }
    }

    /// A type argument that may be a generic type without its arguments: the `dict` of
    /// `load[dict]`.
    pub fn resolve_arg(&self, module: usize, texpr: &TypeExpr, extra: &[(String, Type)]) -> Result<Type> {
        if let TypeExpr::Named { name, args, .. } = texpr
            && args.is_empty()
        {
            if let Some((_, ty)) = extra.iter().find(|(n, _)| n == name) {
                return Ok(ty.clone());
            }
            let generic =
                matches!(self.lookup(module, name), Some(Symbol::GenericStruct(_) | Symbol::GenericEnum(_)));
            if generic || wrapper(name, Type::Void).is_some() {
                return Ok(Type::Family(name.clone()));
            }
        }
        self.resolve_in(module, texpr, extra)
    }

    /// An argument of a `comptime fn` written in a type: `row(R)`, `pad(4)`.
    fn comptime_arg(&self, module: usize, texpr: &TypeExpr, extra: &[(String, Type)]) -> Result<Value> {
        match texpr {
            TypeExpr::Size(n, _) => Ok(Value::Int(i128::from(*n))),
            _ => self.resolve_arg(module, texpr, extra).map(Value::Type),
        }
    }

    /// `array[T, 4]`, `array[T, N]` with `N` a compile-time parameter, or with a `comptime` constant.
    fn array_type(
        &self,
        module: usize,
        item: Box<Type>,
        size: &TypeExpr,
        extra: &[(String, Type)],
        span: Span,
    ) -> Result<Type> {
        let len = match size {
            TypeExpr::Size(len, _) => return Ok(Type::Array(item, *len)),
            TypeExpr::Named { name, args, .. } if args.is_empty() => name,
            _ => {
                return Err(Diagnostic::error(
                    span,
                    "the size of an array is a number or a compile-time name",
                ));
            }
        };
        match extra.iter().find(|(n, _)| n == len).map(|(_, t)| t) {
            Some(Type::Const(value)) => Ok(Type::Array(item, *value)),
            Some(Type::Param(_)) => Ok(Type::ArrayParam(item, len.clone())),
            _ => match self.lookup(module, len) {
                Some(Symbol::Const(i)) => match self.consts[i].value {
                    crate::comptime::Value::Int(value) if value >= 0 => Ok(Type::Array(item, value as u64)),
                    _ => Err(Diagnostic::error(span, format!("`{len}` is not a valid size"))),
                },
                _ => Err(Diagnostic::error(span, format!("unknown size `{len}`"))),
            },
        }
    }

    fn user_type(&self, module: usize, name: &str, span: Span) -> Result<Type> {
        let named = |id| Named { id, name: name.to_string() };
        match self.lookup(module, name) {
            Some(Symbol::Struct(id)) => Ok(Type::Struct(named(id))),
            Some(Symbol::Enum(id)) => Ok(Type::Enum { named: named(id), plain: self.types.enums[id].plain }),
            _ => Err(Diagnostic::error(span, format!("unknown type `{name}`"))),
        }
    }
}

/// Runs the three resolution phases and checks `main`.
pub(super) fn build(program: &mut Program, target: &Target) -> Result<Tables> {
    let mut tables =
        Tables { traits: super::traits::builtin_traits(), target: Some(target.clone()), ..Tables::default() };
    for (index, module) in program.modules.iter_mut().enumerate() {
        tables.module_paths.push(module.name.clone());
        tables.exports.push(stdlib_exports(&module.name));
        let items = std::mem::take(&mut module.items);
        let stdlib = module.stdlib;
        module.items = Collector { tables: &mut tables, module: index, target, stdlib }.flatten(items)?;
        refuse_managed_generics(&module.items)?;
    }
    let indices: HashMap<&str, usize> =
        program.modules.iter().enumerate().map(|(i, m)| (m.name.as_str(), i)).collect();
    for (index, module) in program.modules.iter().enumerate() {
        let namespace = build_namespace(&tables, index, &module.imports, &indices)?;
        tables.namespaces.push(namespace);
    }
    super::traits::sign_traits(&mut tables, program)?;
    super::traits::attach_traits(&mut tables, program)?;
    let mut externs = HashMap::new();
    for (index, module) in program.modules.iter().enumerate() {
        Signer {
            tables: &mut tables,
            module: index,
            target,
            externs: &mut externs,
            self_type: None,
            type_params: Vec::new(),
        }
        .sign(module)?;
    }
    super::traits::check_impls(&mut tables, program)?;
    check_recursion(&tables.types, program)?;
    check_main(&tables)?;
    Ok(tables)
}

/// `extend vector[T]:` `method m(&self, x: i32)` is the function
/// `vector__m[T](self: &vector[T], x: i32)`, private to its module.
fn extension_fn(decl: &ExtendDecl, method: &FnDecl) -> Result<FnDecl> {
    let span = method.span;
    let args = decl
        .generics
        .iter()
        .map(|g| TypeExpr::Named { name: g.name.clone(), args: Vec::new(), span })
        .collect();
    let ty = TypeExpr::Named { name: decl.ty.clone(), args, span };
    let (owned, ty) = match method.receiver {
        Some(Receiver::Shared) => (false, TypeExpr::Ref { mutable: false, inner: Box::new(ty), span }),
        Some(Receiver::Mut) => (false, TypeExpr::Ref { mutable: true, inner: Box::new(ty), span }),
        Some(Receiver::Owned) => (true, ty),
        _ => return Err(Diagnostic::error(span, "a method added by `extend` takes `self`")),
    };
    let mut f = method.clone();
    f.name = format!("{}__{}", decl.ty, method.name);
    f.private = true;
    f.receiver = None;
    f.generics = decl.generics.iter().cloned().chain(method.generics.iter().cloned()).collect();
    f.params.insert(0, Param { owned, name: "self".into(), ty, default: None, span });
    Ok(f)
}

/// `managed` code uses generic types but declares none: type parameters are for `manual`.
fn refuse_managed_generics(items: &[Item]) -> Result<()> {
    let refuse = |kind: &str, span: Span| {
        Err(Diagnostic::error(
            span,
            format!(
                "a `managed` {kind} cannot have type parameters: write it in `manual`, and `managed` code can use it"
            ),
        ))
    };
    for item in items {
        match item {
            Item::Fn(f) if f.level == Level::Managed && !f.generics.is_empty() => {
                return refuse("function", f.span);
            }
            Item::Struct(s) if s.level == Level::Managed => {
                if !s.generics.is_empty() {
                    return refuse("struct", s.span);
                }
                if let Some(method) = s.methods.iter().find(|m| !m.generics.is_empty()) {
                    return refuse("method", method.span);
                }
            }
            Item::Enum(e) if e.level == Level::Managed && !e.generics.is_empty() => {
                return refuse("enum", e.span);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Phase 1: expands `comptime` items and registers every name of one module.
/// A module constant: made before `main`, then only read.
pub(super) struct Global {
    pub module: usize,
    pub name: String,
    pub ty: Type,
}

struct Collector<'a> {
    tables: &'a mut Tables,
    module: usize,
    target: &'a Target,
    stdlib: bool,
}

impl Collector<'_> {
    fn flatten(&mut self, items: Vec<Item>) -> Result<Vec<Item>> {
        let mut kept = Vec::new();
        for item in items {
            match item {
                Item::Comptime(c) if matches!(c.value.kind, ExprKind::ArrayLit(_)) => {
                    let item = Item::Global(self.table(c)?);
                    self.register(&item)?;
                    kept.push(item);
                }
                Item::Comptime(c) => self.add_const(&c)?,
                Item::ComptimeFn(decl) => {
                    let (name, private, span) = (decl.name.clone(), decl.private, decl.span);
                    self.tables.comptime_fns.push(ComptimeFn { module: self.module, decl });
                    let symbol = Symbol::ComptimeFn(self.tables.comptime_fns.len() - 1);
                    self.export(&name, symbol, private, span)?;
                }
                Item::ComptimeAssert(a) => check_assert(&a, |e| self.eval(e))?,
                Item::ComptimeIf(cif) => {
                    let branch = select_branch(cif, |e| self.eval(e))?;
                    kept.extend(self.flatten(branch)?);
                }
                Item::Global(decl) if !self.stdlib => {
                    return Err(Diagnostic::error(
                        decl.span,
                        "module constants are declared with `comptime`",
                    )
                    .with_hint("write `comptime NAME: T = ...`"));
                }
                Item::Extend(decl) => {
                    if !self.stdlib {
                        return Err(Diagnostic::error(
                            decl.span,
                            "only the standard library can `extend` a built-in type",
                        )
                        .with_hint("write a function that takes the value"));
                    }
                    for method in &decl.methods {
                        let item = Item::Fn(extension_fn(&decl, method)?);
                        self.register(&item)?;
                        let Item::Fn(f) = &item else { unreachable!("built above") };
                        let key = (decl.ty.clone(), method.name.clone());
                        self.tables.extensions.insert(key, (self.module, f.name.clone()));
                        kept.push(item);
                    }
                }
                item => {
                    self.register(&item)?;
                    kept.push(item);
                }
            }
        }
        Ok(kept)
    }

    fn register(&mut self, item: &Item) -> Result<()> {
        match item {
            Item::Fn(f) => {
                let symbol = Symbol::Fn(self.placeholder_fn());
                self.export(&f.name, symbol, f.private, f.span)
            }
            Item::Extern(e) => {
                let symbol = Symbol::Fn(self.placeholder_fn());
                self.export(&e.name, symbol, e.private, e.span)
            }
            Item::Struct(s) => self.register_struct(s),
            Item::Trait(t) => {
                let def = super::traits::TraitDef {
                    name: t.name.clone(),
                    module: Some(self.module),
                    methods: Vec::new(),
                };
                self.tables.traits.push(def);
                let symbol = Symbol::Trait(self.tables.traits.len() - 1);
                self.export(&t.name, symbol, t.private, t.span)
            }
            Item::Enum(e) if !e.generics.is_empty() => {
                let index = self.tables.templates.register_enum(e, self.module)?;
                self.export(&e.name, Symbol::GenericEnum(index), e.private, e.span)
            }
            Item::Enum(e) => {
                let plain = e.variants.iter().all(|v| v.fields.is_empty());
                // The names are known from here on, for `comptime` constants; the fields come
                // with the other types, and until then only say whether a variant has any.
                let unknown = || FieldDef {
                    name: String::new(),
                    ty: Type::Void,
                    private: false,
                    default: None,
                    doc: String::new(),
                    decorators: Vec::new(),
                };
                let variants = e
                    .variants
                    .iter()
                    .map(|v| VariantDef {
                        name: v.name.clone(),
                        fields: v.fields.iter().map(|_| unknown()).collect(),
                        doc: v.doc.clone(),
                    })
                    .collect();
                let doc = e.doc.clone();
                let def = EnumDef { name: e.name.clone(), module: self.module, doc, plain, variants, operators: HashMap::new() };
                self.tables.types.enums.push(def);
                let id = self.tables.types.enums.len() - 1;
                for method in &e.methods {
                    let index = self.placeholder_fn();
                    if self.tables.enum_methods.insert((id, method.name.clone()), index).is_some() {
                        return Err(Diagnostic::error(
                            method.span,
                            format!("method `{}` is defined twice", method.name),
                        ));
                    }
                }
                self.export(&e.name, Symbol::Enum(id), e.private, e.span)
            }
            Item::Global(g) => {
                self.tables.globals.push(Global {
                    module: self.module,
                    name: g.name.clone(),
                    ty: Type::Void,
                });
                let symbol = Symbol::Global(self.tables.globals.len() - 1);
                self.export(&g.name, symbol, g.private, g.span)
            }
            Item::Comptime(_)
            | Item::ComptimeFn(_)
            | Item::ComptimeIf(_)
            | Item::ComptimeAssert(_)
            | Item::Extend(_) => {
                unreachable!("handled by flatten")
            }
        }
    }

    fn register_struct(&mut self, s: &StructDecl) -> Result<()> {
        if !s.generics.is_empty() {
            let index = self.tables.templates.register(s, self.module);
            return self.export(&s.name, Symbol::GenericStruct(index), s.private, s.span);
        }
        let (packed, align) = struct_layout(s)?;
        let def = StructDef {
            name: s.name.clone(),
            module: self.module,
            doc: s.doc.clone(),
            fields: Vec::new(),
            operators: HashMap::new(),
            packed,
            align,
        };
        self.tables.types.structs.push(def);
        let id = self.tables.types.structs.len() - 1;
        for method in &s.methods {
            let index = self.placeholder_fn();
            if self.tables.methods.insert((id, method.name.clone()), index).is_some() {
                return Err(Diagnostic::error(
                    method.span,
                    format!("method `{}` is defined twice", method.name),
                ));
            }
        }
        self.export(&s.name, Symbol::Struct(id), s.private, s.span)
    }

    /// Reserves a signature slot, filled in during phase 3.
    fn placeholder_fn(&mut self) -> usize {
        let sig = FnSig {
            params: Vec::new(),
            ret: Type::Void,
            raises: Vec::new(),
            generics: Vec::new(),
            values: Vec::new(),
            variadic: false,
            c_name: String::new(),
            receiver: None,
        };
        self.tables.fns.push(sig);
        self.tables.fns.len() - 1
    }

    /// `comptime PRIMES: array[i32, 4] = [2, 3, 5, 7]`: a table, kept as a module constant
    /// that code reads where it is. Its items are values known when compiling.
    fn table(&self, c: ComptimeConst) -> Result<crate::ast::GlobalDecl> {
        let is_array = |ty: &TypeExpr| matches!(ty, TypeExpr::Named { name, .. } if name == "array");
        let Some(ty) = c.ty.filter(is_array) else {
            return Err(Diagnostic::error(c.span, format!("the table `{}` needs its type", c.name))
                .with_hint(format!("write `comptime {}: array[i32, 3] = [...]`", c.name)));
        };
        self.table_items(&c.value)?;
        Ok(crate::ast::GlobalDecl { private: c.private, name: c.name, ty, value: c.value, span: c.span })
    }

    fn table_items(&self, value: &Expr) -> Result<()> {
        let ExprKind::ArrayLit(items) = &value.kind else {
            return self.eval(value).map(|_| ()).map_err(|_| {
                Diagnostic::error(value.span, "the items of a table are values known when compiling")
                    .with_hint("numbers, texts, `comptime` constants, and what is computed from them")
            });
        };
        items.iter().try_for_each(|item| self.table_items(item))
    }

    fn add_const(&mut self, c: &ComptimeConst) -> Result<()> {
        let constant = evaluate_const(c, |e| self.eval(e))?;
        self.tables.consts.push(constant);
        let symbol = Symbol::Const(self.tables.consts.len() - 1);
        self.export(&c.name, symbol, c.private, c.span)
    }

    fn export(&mut self, name: &str, symbol: Symbol, private: bool, span: Span) -> Result<()> {
        let exports = &mut self.tables.exports[self.module];
        if exports.insert(name.to_string(), Export { symbol, private, span }).is_some() {
            return Err(Diagnostic::error(span, format!("`{name}` is defined twice in this module")));
        }
        Ok(())
    }

    /// Module-level `comptime` code sees what is declared above it in the same module.
    fn eval(&self, expr: &Expr) -> Result<Value> {
        let env = ModuleEnv {
            tables: self.tables,
            module: self.module,
            target: self.target,
            extra: &[],
            local: &|_| None,
        };
        comptime::eval(expr, self.target, &env)
    }
}

/// Phase 3: resolves the types of fields, variants and signatures of one module.
struct Signer<'a> {
    tables: &'a mut Tables,
    module: usize,
    target: &'a Target,
    externs: &'a mut HashMap<String, usize>,
    /// The struct whose fields and methods are being signed, for `Self`.
    self_type: Option<Type>,
    /// The type parameters of the function being signed.
    type_params: Vec<(String, Type)>,
}

impl Signer<'_> {
    fn sign(&mut self, module: &Module) -> Result<()> {
        for item in &module.items {
            match item {
                Item::Fn(f) => {
                    if let Some(capture) = f.captures.first() {
                        return Err(Diagnostic::error(
                            capture.span,
                            "only a function defined inside another one can capture",
                        ));
                    }
                    let index = self.fn_index(&f.name);
                    let (generics, values) = self.generic_params(f)?;
                    if f.level == Level::Managed {
                        self.tables.managed.insert(index);
                    }
                    self.type_params =
                        generics.iter().map(|(n, _)| (n.clone(), Type::Param(n.clone()))).collect();
                    let sig = self.signature(f, fn_c_name(self.module, &f.name), None);
                    self.type_params.clear();
                    let mut sig = FnSig { generics, values, ..sig? };
                    self.fn_decorators(f, index, &sig)?;
                    if f.level == Level::Managed {
                        self.borrow_by_default(index, &mut sig, f);
                    }
                    self.tables.fns[index] = sig;
                }
                Item::Extern(e) => {
                    let index = self.fn_index(&e.name);
                    let params = self.params(&e.params, Level::Manual)?;
                    let ret = self.return_type(e.ret.as_ref())?;
                    let c_name = format!("hsy_extern_{}", e.name);
                    let sig = FnSig {
                        params,
                        ret,
                        raises: Vec::new(),
                        generics: Vec::new(),
                        values: Vec::new(),
                        variadic: e.variadic,
                        c_name,
                        receiver: None,
                    };
                    self.check_extern(&e.name, index, &sig, e.span)?;
                    self.tables.fns[index] = sig;
                }
                Item::Struct(s) => self.sign_struct(s)?,
                Item::Trait(_) => {}
                Item::Enum(e) => self.sign_enum(e)?,
                Item::Global(g) => {
                    let ty = self.ty(&g.ty)?;
                    let Some(Symbol::Global(index)) = self.tables.lookup(self.module, &g.name) else {
                        unreachable!("registered above")
                    };
                    self.tables.globals[index].ty = ty;
                }
                Item::Comptime(_)
                | Item::ComptimeFn(_)
                | Item::ComptimeIf(_)
                | Item::ComptimeAssert(_)
                | Item::Extend(_) => {}
            }
        }
        Ok(())
    }

    fn ty(&mut self, texpr: &TypeExpr) -> Result<Type> {
        let mut extra: Vec<(String, Type)> =
            self.self_type.iter().map(|t| ("Self".to_string(), t.clone())).collect();
        extra.extend(self.type_params.iter().cloned());
        self.tables.resolve_mut(self.module, texpr, &extra)
    }

    fn fn_index(&self, name: &str) -> usize {
        match self.tables.exports[self.module][name].symbol {
            Symbol::Fn(index) => index,
            _ => unreachable!("registered as a function"),
        }
    }

    fn sign_struct(&mut self, s: &StructDecl) -> Result<()> {
        let Symbol::Struct(id) = self.tables.exports[self.module][&s.name].symbol else { return Ok(()) };
        self.self_type = Some(Type::Struct(Named { id, name: s.name.clone() }));
        let mut fields = Vec::new();
        for field in &s.fields {
            if fields.iter().any(|f: &FieldDef| f.name == field.name) {
                return Err(Diagnostic::error(
                    field.span,
                    format!("field `{}` is defined twice", field.name),
                ));
            }
            let mut ty = self.field_type(&field.ty)?;
            if s.level == Level::Managed {
                ty = boxed_self(ty, id);
            }
            let default = self.field_default(field, &ty)?;
            let doc = field.doc.clone();
            let decorators = field.decorators.clone();
            fields.push(FieldDef { name: field.name.clone(), ty, private: field.private, default, doc, decorators });
        }
        self.tables.types.structs[id].fields = fields;
        let owner = Type::Struct(Named { id, name: s.name.clone() });
        self.sign_methods(&s.methods, &s.name, &owner, id, false)?;
        self.self_type = None;
        Ok(())
    }

    /// Signs the methods of a struct or enum; `is_enum` picks which method table to read the
    /// reserved slots from, since structs and enums have separate id spaces.
    fn sign_methods(&mut self, methods: &[FnDecl], name: &str, owner: &Type, id: usize, is_enum: bool) -> Result<()> {
        for method in methods {
            let index = if is_enum {
                self.tables.enum_methods[&(id, method.name.clone())]
            } else {
                self.tables.methods[&(id, method.name.clone())]
            };
            let c_name = method_c_name(self.module, name, &method.name);
            let generics = self.generics(method)?;
            self.type_params = generics.iter().map(|(n, _)| (n.clone(), Type::Param(n.clone()))).collect();
            let sig = self.signature(method, c_name, Some(owner));
            self.type_params.clear();
            if !generics.is_empty() {
                let entry = super::templates::GenericMethod {
                    module: self.module,
                    decl: method.clone(),
                    bindings: Vec::new(),
                    owner: owner.clone(),
                };
                self.tables.templates.generic_methods.insert(index, entry);
            }
            let mut sig = FnSig { generics, values: Vec::new(), ..sig? };
            if method.level == Level::Managed {
                self.tables.managed.insert(index);
                self.borrow_by_default(index, &mut sig, method);
            }
            self.tables.fns[index] = sig;
        }
        Ok(())
    }

    fn sign_enum(&mut self, e: &EnumDecl) -> Result<()> {
        let Symbol::Enum(id) = self.tables.exports[self.module][&e.name].symbol else { return Ok(()) };
        let mut variants = Vec::new();
        for variant in &e.variants {
            if e.level == Level::Managed && !variant.fields.is_empty() {
                return Err(Diagnostic::error(variant.span, "`managed` enums cannot carry data")
                    .with_hint("declare a `manual enum` for variants with fields"));
            }
            let mut fields = Vec::new();
            for f in &variant.fields {
                let ty = self.field_type(&f.ty)?;
                let default = self.field_default(f, &ty)?;
                fields.push(FieldDef { name: f.name.clone(), ty, private: false, default, doc: f.doc.clone(), decorators: Vec::new() });
            }
            variants.push(VariantDef { name: variant.name.clone(), fields, doc: variant.doc.clone() });
        }
        self.tables.types.enums[id].variants = variants;
        let owner = Type::Enum { named: Named { id, name: e.name.clone() }, plain: e.variants.iter().all(|v| v.fields.is_empty()) };
        self.self_type = Some(owner.clone());
        self.sign_methods(&e.methods, &e.name, &owner, id, true)?;
        self.self_type = None;
        Ok(())
    }

    fn field_type(&mut self, texpr: &TypeExpr) -> Result<Type> {
        let ty = self.ty(texpr)?;
        if matches!(ty, Type::Ref(..)) {
            return Err(Diagnostic::error(texpr.span(), "a field cannot hold a reference")
                .with_hint("store the value itself, or an index into a collection"));
        }
        Ok(ty)
    }

    /// The signature of a function or method; `owner` is the struct of a method.
    fn signature(&mut self, f: &FnDecl, c_name: String, owner: Option<&Type>) -> Result<FnSig> {
        let mut params = Vec::new();
        if let (Some(receiver), Some(owner)) = (f.receiver, owner) {
            let ty = match receiver {
                Receiver::Shared => Type::Ref(Box::new(owner.clone()), false),
                Receiver::Mut => Type::Ref(Box::new(owner.clone()), true),
                Receiver::Owned => owner.clone(),
                // A `managed` `self` starts as `&self`; its mode is inferred like a parameter's.
                Receiver::Plain if f.level == Level::Managed => Type::Ref(Box::new(owner.clone()), false),
                Receiver::Plain => {
                    return Err(Diagnostic::error(
                        f.span,
                        "a `manual` method takes `&self`, `&mut self` or `owned self`",
                    ));
                }
            };
            params.push(ParamSig { name: "self".into(), ty, default: None });
        }
        params.extend(self.params(&f.params, f.level)?);
        let ret = self.return_type(f.ret.as_ref())?;
        let raises = self.error_types(&f.raises)?;
        Ok(FnSig {
            params,
            ret,
            raises,
            generics: Vec::new(),
            values: Vec::new(),
            variadic: false,
            c_name,
            receiver: f.receiver,
        })
    }

    /// The type parameters of `f`, each with the traits it must implement.
    fn generics(&mut self, f: &FnDecl) -> Result<Vec<(String, Vec<usize>)>> {
        Ok(self.generic_params(f)?.0)
    }

    /// The parameters in `[...]`: type parameters with their bounds, and among them the
    /// compile-time values, such as `[N: size]`, with their integer type.
    fn generic_params(&mut self, f: &FnDecl) -> Result<(Vec<TypeParam>, Vec<ValueParam>)> {
        let (mut generics, mut values) = (Vec::new(), Vec::new());
        for param in &f.generics {
            if generics.iter().any(|(n, _): &(String, Vec<usize>)| *n == param.name) {
                return Err(Diagnostic::error(param.span, format!("`{}` is declared twice", param.name)));
            }
            let mut bounds = Vec::new();
            for (name, span) in &param.bounds {
                match (self.tables.lookup(self.module, name), Type::scalar(name)) {
                    (Some(Symbol::Trait(id)), _) => bounds.push(id),
                    (_, Some(ty)) if ty.as_int().is_some() && param.bounds.len() == 1 => {
                        values.push((param.name.clone(), ty))
                    }
                    (_, Some(_)) => {
                        return Err(Diagnostic::error(
                            *span,
                            "a compile-time parameter is an integer: `[N: size]`",
                        ));
                    }
                    _ => return Err(Diagnostic::error(*span, format!("unknown trait `{name}`"))),
                }
            }
            generics.push((param.name.clone(), bounds));
        }
        Ok((generics, values))
    }

    /// The types of `raises`: structs or enums, each named once.
    fn error_types(&mut self, texprs: &[TypeExpr]) -> Result<Vec<Type>> {
        let mut types = Vec::new();
        for texpr in texprs {
            let ty = self.ty(texpr)?;
            if !matches!(ty, Type::Struct(_) | Type::Enum { .. }) {
                return Err(Diagnostic::error(
                    texpr.span(),
                    format!("`{ty}` cannot be raised; errors are structs or enums"),
                ));
            }
            if types.contains(&ty) {
                return Err(Diagnostic::error(texpr.span(), format!("`{ty}` is listed twice")));
            }
            types.push(ty);
        }
        Ok(types)
    }

    /// `@panic_handler`: one `manual fn name(message: &str)`, called before the program
    /// stops on a fault or a `panic`.
    fn fn_decorators(&mut self, f: &crate::ast::FnDecl, index: usize, sig: &FnSig) -> Result<()> {
        for d in &f.decorators {
            if d.name == "test" {
                continue;
            }
            if d.name != "panic_handler" {
                return Err(Diagnostic::error(
                    d.span,
                    format!("unknown decorator `@{}` on a function", d.name),
                )
                .with_hint("functions take `@panic_handler` and `@test`"));
            }
            let expected = [Type::Ref(Box::new(Type::Str), false)];
            let fits = f.level == Level::Manual
                && sig.generics.is_empty()
                && sig.params.iter().map(|p| &p.ty).eq(expected.iter())
                && sig.ret == Type::Void
                && sig.raises.is_empty()
                && d.args.is_empty();
            if !fits {
                return Err(Diagnostic::error(d.span, "a panic handler is `manual fn name(message: &str)`")
                    .with_hint("it takes the message, returns nothing and raises nothing"));
            }
            if self.tables.panic_handler.replace(index).is_some() {
                return Err(Diagnostic::error(d.span, "only one function can be the `@panic_handler`"));
            }
        }
        Ok(())
    }

    /// A `managed` parameter written without a mode starts as `&`; the body may make it
    /// `&mut` or `owned` (see `managed::infer_modes`).
    fn borrow_by_default(&mut self, index: usize, sig: &mut FnSig, f: &crate::ast::FnDecl) {
        let offset = sig.params.len() - f.params.len();
        let mut inferred = Vec::new();
        if f.receiver == Some(Receiver::Plain)
            && let Type::Ref(owner, _) = &sig.params[0].ty
        {
            inferred.push((0, owner.as_ref().clone()));
        }
        for (i, p) in f.params.iter().enumerate() {
            let param = &mut sig.params[offset + i];
            if p.owned || param.ty.is_copy() || matches!(param.ty, Type::Ref(..)) {
                continue;
            }
            inferred.push((offset + i, param.ty.clone()));
            param.ty = Type::Ref(Box::new(param.ty.clone()), false);
        }
        if !inferred.is_empty() {
            self.tables.inferred.insert(index, inferred);
        }
    }

    fn params(&mut self, params: &[Param], level: Level) -> Result<Vec<ParamSig>> {
        params.iter().map(|p| self.param(p, level)).collect()
    }

    /// `count: i64 = 10`: the value of a field that is not given, known when compiling.
    fn field_default(&mut self, field: &crate::ast::FieldDecl, ty: &Type) -> Result<Option<Value>> {
        let Some(expr) = &field.default else { return Ok(None) };
        let value = self.eval(expr)?;
        comptime::check_fits(&value, ty, expr.span)?;
        Ok(Some(value))
    }

    fn param(&mut self, p: &Param, level: Level) -> Result<ParamSig> {
        let ty = self.ty(&p.ty)?;
        if level == Level::Manual {
            super::check_binding_mode(p, &ty)?;
        }
        let default = match &p.default {
            Some(expr) => {
                let value = self.eval(expr)?;
                // For a type parameter, the call checks the value once the type is known, and
                // so for a module constant.
                if !matches!(ty, Type::Param(_)) && !matches!(value, Value::Global { .. }) {
                    comptime::check_fits(&value, &ty, expr.span)?;
                }
                Some(value)
            }
            None => None,
        };
        Ok(ParamSig { name: p.name.clone(), ty, default })
    }

    fn return_type(&mut self, texpr: Option<&TypeExpr>) -> Result<Type> {
        let Some(texpr) = texpr else { return Ok(Type::Void) };
        let ty = self.ty(texpr)?;
        if matches!(ty, Type::Ref(..)) {
            return Err(Diagnostic::error(texpr.span(), "functions cannot return references")
                .with_hint("return the value itself"));
        }
        Ok(ty)
    }

    fn check_extern(&mut self, name: &str, index: usize, sig: &FnSig, span: Span) -> Result<()> {
        if let Some(&previous) = self.externs.get(name) {
            let other = &self.tables.fns[previous];
            let same_params = other.params.iter().map(|p| &p.ty).eq(sig.params.iter().map(|p| &p.ty));
            if !same_params || other.ret != sig.ret || other.variadic != sig.variadic {
                return Err(Diagnostic::error(
                    span,
                    format!("`{name}` is declared with another signature elsewhere"),
                ));
            }
        }
        self.externs.insert(name.to_string(), index);
        Ok(())
    }

    /// Defaults are compile-time values, evaluated where the function is declared, or module
    /// constants of the standard library, such as `to: &File = stdout`.
    fn eval(&self, expr: &Expr) -> Result<Value> {
        let env = ModuleEnv {
            tables: self.tables,
            module: self.module,
            target: self.target,
            extra: &[],
            local: &|_| None,
        };
        comptime::eval(expr, self.target, &env)
    }
}

/// `@packed` and `@align(N)`, for `manual` structs.
fn struct_layout(s: &StructDecl) -> Result<(bool, Option<u64>)> {
    let (mut packed, mut align) = (false, None);
    for d in &s.decorators {
        if s.level == Level::Managed {
            return Err(Diagnostic::error(d.span, "only `manual` structs choose their layout"));
        }
        match (d.name.as_str(), d.args.as_slice()) {
            ("packed", []) => packed = true,
            ("align", [Expr { kind: crate::ast::ExprKind::Int(n), .. }])
                if *n > 0 && (*n as u64).is_power_of_two() =>
            {
                align = Some(*n as u64);
            }
            ("align", _) => {
                return Err(Diagnostic::error(
                    d.span,
                    "`@align(N)` takes a power of two, such as `@align(16)`",
                ));
            }
            _ => {
                return Err(Diagnostic::error(
                    d.span,
                    format!("unknown decorator `@{}` on a struct", d.name),
                )
                .with_hint("structs take `@packed` and `@align(N)`"));
            }
        }
    }
    Ok((packed, align))
}

/// A `managed` field holding its own struct, directly or through `Optional`, tuples or
/// arrays, holds it on the heap: `next: Optional[Node]` is `Optional[heap[Node]]`.
fn boxed_self(ty: Type, id: usize) -> Type {
    match ty {
        Type::Struct(named) if named.id == id => Type::Heap(Box::new(Type::Struct(named))),
        Type::Optional(item) => Type::Optional(Box::new(boxed_self(*item, id))),
        Type::Array(item, n) => Type::Array(Box::new(boxed_self(*item, id)), n),
        Type::Tuple(items) => Type::Tuple(items.into_iter().map(|t| boxed_self(t, id)).collect()),
        other => other,
    }
}

/// A struct cannot contain itself by value: its size would be infinite.
fn check_recursion(types: &TypeTable, program: &Program) -> Result<()> {
    for (id, def) in types.structs.iter().enumerate() {
        let ty = Type::Struct(Named { id, name: def.name.clone() });
        if contains_struct(types, &ty, id, true, &mut Vec::new()) {
            let span = struct_span(program, &def.name, def.module);
            return Err(Diagnostic::error(span, format!("`{}` contains itself", def.name))
                .with_hint("store the inner value on the heap with `heap[T]`"));
        }
    }
    Ok(())
}

/// Whether `ty` holds `target` by value. `seen` keeps the structs already looked into, so that
/// another struct holding itself is not followed forever: it is reported on its own.
fn contains_struct(types: &TypeTable, ty: &Type, target: usize, top: bool, seen: &mut Vec<usize>) -> bool {
    let inside = |t: &Type, seen: &mut Vec<usize>| contains_struct(types, t, target, false, seen);
    match ty {
        Type::Struct(named) if named.id == target && !top => true,
        Type::Struct(named) => {
            if seen.contains(&named.id) {
                return false;
            }
            seen.push(named.id);
            types.structs[named.id].fields.iter().any(|f| inside(&f.ty, seen))
        }
        Type::Tuple(items) => items.iter().any(|t| inside(t, seen)),
        Type::Array(item, _) | Type::Optional(item) => inside(item, seen),
        Type::Enum { named, .. } => {
            types.enums[named.id].variants.iter().any(|v| v.fields.iter().any(|f| inside(&f.ty, seen)))
        }
        _ => false,
    }
}

fn struct_span(program: &Program, name: &str, module: usize) -> Span {
    program.modules[module]
        .items
        .iter()
        .find_map(|item| match item {
            Item::Struct(s) if s.name == name => Some(s.span),
            _ => None,
        })
        .unwrap_or(Span::new(0, 1, 1))
}

/// Evaluates a `comptime` constant and checks it against its declared type.
pub(super) fn evaluate_const(c: &ComptimeConst, eval: impl Fn(&Expr) -> Result<Value>) -> Result<Constant> {
    let texpr = c.ty.as_ref().ok_or_else(|| {
        Diagnostic::error(c.span, format!("`{}` needs an explicit type", c.name))
            .with_hint(format!("write `comptime {}: i32 = ...`", c.name))
    })?;
    let Some(ty) = scalar_type(texpr) else {
        // Or an enum, for a variant without fields: `comptime MODE: Mode = Mode.Safe`.
        return match (eval(&c.value)?, texpr) {
            (Value::Variant { ty, index }, TypeExpr::Named { name, args, .. })
                if args.is_empty() && ty.to_string() == *name =>
            {
                Ok(Constant { ty: ty.clone(), value: Value::Variant { ty, index } })
            }
            _ => Err(Diagnostic::error(
                texpr.span(),
                "a `comptime` constant has a scalar type, or is a variant without fields of its enum",
            )),
        };
    };
    let value = eval(&c.value)?;
    comptime::check_fits(&value, &ty, c.value.span)?;
    Ok(Constant { ty, value })
}

/// Keeps the body of the first branch whose condition holds.
pub(super) fn select_branch<T>(
    cif: ComptimeIf<T>,
    mut eval: impl FnMut(&Expr) -> Result<Value>,
) -> Result<Vec<T>> {
    for (cond, body) in cif.branches {
        match eval(&cond)? {
            Value::Bool(true) => return Ok(body),
            Value::Bool(false) => {}
            _ => return Err(Diagnostic::error(cond.span, "a `comptime if` condition must be a `bool`")),
        }
    }
    Ok(cif.otherwise.unwrap_or_default())
}

pub(super) fn check_assert(a: &ComptimeAssert, eval: impl Fn(&Expr) -> Result<Value>) -> Result<()> {
    match eval(&a.cond)? {
        Value::Bool(true) => Ok(()),
        Value::Bool(false) => Err(Diagnostic::error(
            a.span,
            a.message.clone().unwrap_or_else(|| "compile-time assertion failed".into()),
        )),
        _ => Err(Diagnostic::error(a.cond.span, "`comptime assert` needs a `bool` condition")),
    }
}

/// The items the compiler provides for a standard module.
fn stdlib_exports(module: &str) -> HashMap<String, Export> {
    let items: &[(&str, Builtin)] = match module {
        "io" => &[("print", Builtin::Print)],
        "thread" => &[
            ("spawn", Builtin::Spawn),
            ("channel", Builtin::Channel),
            ("atomic", Builtin::AtomicNew),
            ("mutex", Builtin::MutexNew),
            ("scope", Builtin::ScopeNew),
        ],
        _ => &[],
    };
    let span = Span::new(0, 1, 1);
    items
        .iter()
        .map(|(name, op)| (name.to_string(), Export { symbol: Symbol::Builtin(*op), private: false, span }))
        .collect()
}

fn build_namespace(
    tables: &Tables,
    module: usize,
    imports: &[Import],
    indices: &HashMap<&str, usize>,
) -> Result<HashMap<String, Symbol>> {
    let mut namespace: HashMap<String, Symbol> =
        tables.exports[module].iter().map(|(name, export)| (name.clone(), export.symbol)).collect();
    // Built-in traits are visible everywhere, unless a module defines or imports the same name.
    let mut prelude: Vec<(String, Symbol)> = Vec::new();
    for (id, def) in tables.traits.iter().enumerate().filter(|(_, t)| t.module.is_none()) {
        prelude.push((def.name.clone(), Symbol::Trait(id)));
    }
    // So are the public names of the standard prelude.
    if let Some(&source) = indices.get(crate::stdlib::PRELUDE) {
        let public = tables.exports[source].iter().filter(|(_, export)| !export.private);
        prelude.extend(public.map(|(name, export)| (name.clone(), export.symbol)));
    }
    for import in imports {
        let source = indices[import.module.join(".").as_str()];
        match &import.kind {
            ImportKind::Module { alias } => {
                let name = alias.clone().unwrap_or_else(|| import.module.last().cloned().unwrap_or_default());
                bind(&mut namespace, name, Symbol::Module(source), import.span)?;
            }
            ImportKind::Names(names) => {
                for (name, alias) in names {
                    let symbol = tables.export(source, name, import.span)?;
                    bind(&mut namespace, alias.clone().unwrap_or_else(|| name.clone()), symbol, import.span)?;
                }
            }
        }
    }
    for (name, symbol) in prelude {
        namespace.entry(name).or_insert(symbol);
    }
    Ok(namespace)
}

fn bind(namespace: &mut HashMap<String, Symbol>, name: String, symbol: Symbol, span: Span) -> Result<()> {
    if namespace.contains_key(&name) {
        return Err(Diagnostic::error(span, format!("`{name}` is already defined in this module"))
            .with_hint("rename the import with `as`"));
    }
    namespace.insert(name, symbol);
    Ok(())
}

fn check_main(tables: &Tables) -> Result<()> {
    let Some(main) = tables.exports[0].get("main") else {
        return Err(Diagnostic::error(Span::new(0, 1, 1), "the program has no `main` function"));
    };
    let Symbol::Fn(index) = main.symbol else {
        return Err(Diagnostic::error(main.span, "`main` must be a function"));
    };
    let sig = &tables.fns[index];
    if !sig.params.is_empty() || !matches!(sig.ret, Type::Void | Type::Int(IntTy::I32)) {
        return Err(Diagnostic::error(main.span, "`main` takes no parameters and returns nothing or `i32`"));
    }
    if !sig.raises.is_empty() && !tables.managed.contains(&index) {
        return Err(Diagnostic::error(main.span, "`main` cannot raise")
            .with_hint("catch its errors with `try` / `except`"));
    }
    Ok(())
}

/// The built-in types that wrap one other type: `vector[T]`, `heap[T]`, `thread[T]`...
pub(super) fn wrapper(name: &str, item: Type) -> Option<Type> {
    let item = Box::new(item);
    Some(match name {
        "ptr" => Type::Ptr(item),
        "Optional" => Type::Optional(item),
        "vector" => Type::Vector(item),
        "heap" => Type::Heap(item),
        "shared" => Type::Shared(item),
        "thread" => Type::Thread(item),
        "sender" => Type::Sender(item),
        "receiver" => Type::Receiver(item),
        "atomic" => Type::Atomic(item),
        "mutex" => Type::Mutex(item),
        _ => return None,
    })
}
