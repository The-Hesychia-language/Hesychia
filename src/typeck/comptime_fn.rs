//! `comptime fn`: functions the compiler runs on values and types known at compile time.
//!
//! A call runs the body statement by statement, with its locals as compile-time values.
//! A call whose type arguments are not known yet, such as `row(R)` in a generic signature,
//! becomes a `Type::Computed`, run again once they are.

use std::cell::{Cell, RefCell};

use super::resolve::{Symbol, Tables, wrapper};
use super::templates::Kind;
use crate::ast::{Expr, ExprKind, FnDecl, Stmt, StmtKind, TypeExpr, VarTarget};
use crate::comptime::{self, Env, Target, Value};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::Type;

/// Statements one call may run, its inner calls included, before the compiler gives up.
const STEPS: u64 = 1_000_000;

/// Calls inside calls, before the compiler gives up rather than run out of stack.
const DEPTH: usize = 200;

pub(super) struct ComptimeFn {
    pub module: usize,
    pub decl: FnDecl,
}

/// A `comptime fn` written in the program, or one every module has, such as `item`.
#[derive(Clone, PartialEq)]
pub(super) enum Callee {
    Fn(usize),
    Builtin(String),
}

/// A call to a `comptime fn` in a type, waiting for its type parameters.
#[derive(Clone)]
pub(super) struct ComputedCall {
    pub callee: Callee,
    pub args: Vec<Value>,
}

enum Flow {
    Next,
    Break,
    Continue,
    Return(Value),
}

impl Tables {
    /// Runs the `comptime fn` `func` on `args`.
    pub fn comptime_call(&self, target: &Target, func: usize, args: Vec<Value>, span: Span) -> Result<Value> {
        let steps = Cell::new(0);
        self.run(target, func, args, Outer { steps: &steps, origin: span, depth: 0 }, span)
    }

    fn run(&self, target: &Target, func: usize, args: Vec<Value>, outer: Outer, span: Span) -> Result<Value> {
        let ComptimeFn { module, decl } = &self.comptime_fns[func];
        if outer.depth > DEPTH {
            return Err(Diagnostic::error(outer.origin, "this `comptime fn` calls itself too deeply")
                .with_hint("a recursion probably never ends"));
        }
        if args.len() != decl.params.len() {
            return Err(Diagnostic::error(
                span,
                format!("`{}` takes {} argument(s)", decl.name, decl.params.len()),
            ));
        }
        let frame = Frame {
            tables: self,
            target,
            module: *module,
            locals: RefCell::new(Vec::new()),
            steps: outer.steps,
            origin: outer.origin,
            depth: outer.depth,
        };
        for (param, value) in decl.params.iter().zip(args) {
            frame.check_kind(&value, &param.ty, span)?;
            frame.locals.borrow_mut().push((param.name.clone(), value));
        }
        let value = match frame.block(&decl.body)? {
            Flow::Return(value) => value,
            _ => {
                return Err(Diagnostic::error(decl.span, format!("`{}` ended without returning", decl.name)));
            }
        };
        if let Some(ret) = &decl.ret {
            frame.check_kind(&value, ret, span)?;
        }
        Ok(value)
    }

    /// The type `name(args)` gives, or a `Type::Computed` while an argument has a type parameter.
    pub fn computed_type(
        &self,
        target: &Target,
        module: usize,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Type> {
        let callee = match self.comptime_fn(module, name) {
            Some(func) => Callee::Fn(func),
            None => Callee::Builtin(name.to_string()),
        };
        self.type_call(target, callee, args, span)
    }

    /// The type `callee` gives for `args`, or a `Type::Computed` for later.
    pub fn type_call(&self, target: &Target, callee: Callee, args: Vec<Value>, span: Span) -> Result<Type> {
        let name = match &callee {
            Callee::Fn(func) => self.comptime_fns[*func].decl.name.clone(),
            Callee::Builtin(name) => name.clone(),
        };
        let waiting = args.iter().any(|a| matches!(a, Value::Type(t) if self.has_param(t)));
        // A built-in one may already know: `item(vector[T])` is `T`.
        if waiting
            && let Callee::Builtin(name) = &callee
            && let Ok(Value::Type(ty)) = builtin_call(self, name, args.clone(), span)
        {
            return Ok(ty);
        }
        if waiting {
            let shown = format!("{name}({})", args.iter().map(shown).collect::<Vec<_>>().join(", "));
            let mut calls = self.computed.borrow_mut();
            let call = calls.iter().position(|c| c.callee == callee && c.args == args).unwrap_or_else(|| {
                calls.push(ComputedCall { callee, args });
                calls.len() - 1
            });
            return Ok(Type::Computed { call, shown });
        }
        let value = match callee {
            Callee::Fn(func) => self.comptime_call(target, func, args, span)?,
            Callee::Builtin(name) => builtin_call(self, &name, args, span)?,
        };
        match value {
            Value::Type(ty) => Ok(ty),
            _ => Err(Diagnostic::error(span, format!("`{name}` gives a value, not a type"))),
        }
    }

    /// A name in compile-time code, as seen from `module` with `extra` type parameters.
    pub fn comptime_name(
        &self,
        module: usize,
        name: &str,
        extra: &[(String, Type)],
        span: Span,
    ) -> Result<Value> {
        if let Some((_, ty)) = extra.iter().find(|(n, _)| n == name) {
            return Ok(match ty {
                Type::Const(value) => Value::Int(i128::from(*value)),
                ty => Value::Type(ty.clone()),
            });
        }
        let texpr = TypeExpr::Named { name: name.to_string(), args: Vec::new(), span };
        match self.symbol(module, name) {
            Some(Symbol::Const(i)) => Ok(self.consts[i].value.clone()),
            Some(Symbol::Global(i)) => {
                let global = &self.globals[i];
                Ok(Value::Global { module: global.module, name: global.name.clone() })
            }
            Some(Symbol::GenericStruct(_) | Symbol::GenericEnum(_)) => {
                Ok(Value::Type(Type::Family(name.into())))
            }
            Some(Symbol::Struct(_) | Symbol::Enum(_)) => {
                self.resolve_in(module, &texpr, extra).map(Value::Type)
            }
            _ => match name.split_once('.') {
                Some((owner, variant)) => self.variant_value(module, owner, variant, span),
                None if wrapper(name, Type::Void).is_some() => Ok(Value::Type(Type::Family(name.into()))),
                None => match self.resolve_in(module, &texpr, extra) {
                    Ok(ty) => Ok(Value::Type(ty)),
                    Err(_) => {
                        Err(Diagnostic::error(span, format!("`{name}` is not a compile-time constant")))
                    }
                },
            },
        }
    }

    /// A variant without fields, such as `Side.Start`, as a compile-time value.
    pub fn variant_value(&self, module: usize, owner: &str, variant: &str, span: Span) -> Result<Value> {
        let shown = format!("{owner}.{variant}");
        let Some(Symbol::Enum(id)) = self.symbol(module, owner) else {
            return Err(Diagnostic::error(span, format!("`{shown}` is not a compile-time constant")));
        };
        let def = &self.types.enums[id];
        let index = def
            .variants
            .iter()
            .position(|v| v.name == variant)
            .ok_or_else(|| Diagnostic::error(span, format!("`{owner}` has no variant `{variant}`")))?;
        if !def.variants[index].fields.is_empty() {
            return Err(Diagnostic::error(
                span,
                format!("`{shown}` has fields, it is not a compile-time constant"),
            ));
        }
        let ty = Type::Enum { named: crate::types::Named { id, name: def.name.clone() }, plain: def.plain };
        Ok(Value::Variant { ty, index })
    }

    /// Whether `ty` was made from the generic type `family`: `dict[str, i32]` from `dict`.
    pub fn is_family(&self, ty: &Type, family: &str) -> bool {
        let template = |kind, id| self.templates.origin(kind, id).map(|(t, _)| t);
        match ty {
            Type::Struct(named) => {
                template(Kind::Struct, named.id).is_some_and(|t| self.templates.list[t].decl.name == family)
            }
            Type::Enum { named, .. } => {
                template(Kind::Enum, named.id).is_some_and(|t| self.templates.enums[t].decl.name == family)
            }
            _ => item(ty).is_some_and(|inner| wrapper(family, inner.clone()).as_ref() == Some(ty)),
        }
    }

    /// The `comptime fn` behind `name`, or a built-in one.
    pub fn comptime_fn(&self, module: usize, name: &str) -> Option<usize> {
        match self.symbol(module, name) {
            Some(Symbol::ComptimeFn(func)) => Some(func),
            _ => None,
        }
    }
}

/// The item type of a type that holds others: `vector[T]`, `Optional[T]`, `array[T, N]`...
fn item(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Ptr(inner)
        | Type::Optional(inner)
        | Type::Vector(inner)
        | Type::Heap(inner)
        | Type::Shared(inner)
        | Type::Thread(inner)
        | Type::Sender(inner)
        | Type::Receiver(inner)
        | Type::Atomic(inner)
        | Type::Array(inner, _)
        | Type::Simd(inner, _) => Some(inner),
        _ => None,
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::Type(ty) => ty.to_string(),
        Value::Int(v) => v.to_string(),
        Value::Float(v) => v.to_string(),
        Value::Bool(v) => if *v { "True" } else { "False" }.into(),
        Value::Str(v) => format!("{v:?}"),
        Value::Global { name, .. } => name.clone(),
        Value::Variant { ty, .. } => ty.to_string(),
        Value::None => "None".into(),
    }
}

/// What a call shares with the calls around it. `origin` is where the outermost call is
/// written, where a `panic` is reported.
#[derive(Clone, Copy)]
struct Outer<'a> {
    steps: &'a Cell<u64>,
    origin: Span,
    depth: usize,
}

/// One running call: its locals, innermost last.
struct Frame<'a> {
    tables: &'a Tables,
    target: &'a Target,
    module: usize,
    locals: RefCell<Vec<(String, Value)>>,
    steps: &'a Cell<u64>,
    origin: Span,
    depth: usize,
}

impl Frame<'_> {
    fn eval(&self, expr: &Expr) -> Result<Value> {
        comptime::eval(expr, self.target, self)
    }

    fn block(&self, body: &[Stmt]) -> Result<Flow> {
        let depth = self.locals.borrow().len();
        let flow = self.stmts(body);
        self.locals.borrow_mut().truncate(depth);
        flow
    }

    fn stmts(&self, body: &[Stmt]) -> Result<Flow> {
        for stmt in body {
            self.steps.set(self.steps.get() + 1);
            if self.steps.get() > STEPS {
                return Err(Diagnostic::error(stmt.span, "this `comptime fn` runs too long")
                    .with_hint("a loop or a recursion probably never ends"));
            }
            match self.stmt(stmt)? {
                Flow::Next => {}
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Next)
    }

    fn stmt(&self, stmt: &Stmt) -> Result<Flow> {
        match &stmt.kind {
            StmtKind::Var { target: VarTarget::Name(name), ty, value, .. } => {
                let value = self.eval(value)?;
                if let Some(ty) = ty {
                    self.check_kind(&value, ty, stmt.span)?;
                }
                self.locals.borrow_mut().push((name.clone(), value));
            }
            StmtKind::Assign { target, op, value } => {
                let ExprKind::Name(name) = &target.kind else {
                    return Err(Diagnostic::error(
                        target.span,
                        "a `comptime fn` only assigns to its variables",
                    ));
                };
                let mut value = self.eval(value)?;
                if let Some(op) = op {
                    value = comptime::binary(*op, self.name(name, target.span)?, value, self, stmt.span)?;
                }
                let mut locals = self.locals.borrow_mut();
                let slot = locals.iter_mut().rev().find(|(n, _)| n == name).ok_or_else(|| {
                    Diagnostic::error(target.span, format!("unknown variable `{name}`"))
                        .with_hint(format!("declare it with `mut {name} = ...`"))
                })?;
                slot.1 = value;
            }
            StmtKind::Expr(expr) => {
                self.eval(expr)?;
            }
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    if self.condition(cond)? {
                        return self.block(body);
                    }
                }
                if let Some(body) = otherwise {
                    return self.block(body);
                }
            }
            StmtKind::While { cond, body } => {
                while self.condition(cond)? {
                    match self.block(body)? {
                        Flow::Break => break,
                        Flow::Return(value) => return Ok(Flow::Return(value)),
                        Flow::Next | Flow::Continue => {}
                    }
                }
            }
            StmtKind::For { var, iter, body } => return self.for_range(var, iter, body),
            StmtKind::Return(Some(value)) => return Ok(Flow::Return(self.eval(value)?)),
            StmtKind::Break => return Ok(Flow::Break),
            StmtKind::Continue => return Ok(Flow::Continue),
            StmtKind::Pass => {}
            StmtKind::Block(body) => return self.block(body),
            _ => {
                return Err(Diagnostic::error(stmt.span, "a `comptime fn` cannot do this")
                    .with_hint("it can declare variables, assign, call `comptime fn`s, use `if`, `while`, `for` over `range` and `return`"));
            }
        }
        Ok(Flow::Next)
    }

    /// `for i in range(...)`, the only loop over values a `comptime fn` has.
    fn for_range(&self, var: &str, iter: &Expr, body: &[Stmt]) -> Result<Flow> {
        let ExprKind::Call { callee, args, .. } = &iter.kind else {
            return Err(Diagnostic::error(iter.span, "a `comptime fn` loops over `range(...)` only"));
        };
        if callee != "range" || args.is_empty() || args.len() > 3 {
            return Err(Diagnostic::error(iter.span, "a `comptime fn` loops over `range(...)` only"));
        }
        let ints = args
            .iter()
            .map(|a| match self.eval(a)? {
                Value::Int(v) => Ok(v),
                _ => Err(Diagnostic::error(a.span, "`range` takes integers")),
            })
            .collect::<Result<Vec<_>>>()?;
        let (start, end, step) = match ints.as_slice() {
            [end] => (0, *end, 1),
            [start, end] => (*start, *end, 1),
            [start, end, step] => (*start, *end, *step),
            _ => unreachable!("checked above"),
        };
        if step == 0 {
            return Err(Diagnostic::error(iter.span, "the step of `range` is 0"));
        }
        let mut i = start;
        while (step > 0 && i < end) || (step < 0 && i > end) {
            self.locals.borrow_mut().push((var.to_string(), Value::Int(i)));
            let flow = self.block(body);
            self.locals.borrow_mut().pop();
            match flow? {
                Flow::Break => break,
                Flow::Return(value) => return Ok(Flow::Return(value)),
                Flow::Next | Flow::Continue => {}
            }
            i += step;
        }
        Ok(Flow::Next)
    }

    fn condition(&self, cond: &Expr) -> Result<bool> {
        match self.eval(cond)? {
            Value::Bool(v) => Ok(v),
            _ => Err(Diagnostic::error(cond.span, "a condition is `True` or `False`")),
        }
    }

    /// Checks a parameter, a variable or the result against its written type.
    fn check_kind(&self, value: &Value, texpr: &TypeExpr, span: Span) -> Result<()> {
        if matches!(texpr, TypeExpr::Named { name, args, .. } if name == "type" && args.is_empty()) {
            return match value {
                Value::Type(_) => Ok(()),
                _ => Err(Diagnostic::error(span, "expected a type")),
            };
        }
        if let Value::Type(ty) = value {
            return Err(Diagnostic::error(span, format!("expected a value, found the type `{ty}`")));
        }
        let ty = self.tables.resolve_in(self.module, texpr, &[])?;
        comptime::check_fits(value, &ty, span)
    }
}

impl Env for Frame<'_> {
    fn name(&self, name: &str, span: Span) -> Result<Value> {
        if let Some((_, value)) = self.locals.borrow().iter().rev().find(|(n, _)| n == name) {
            return Ok(value.clone());
        }
        self.tables.comptime_name(self.module, name, &[], span)
    }

    fn layout(&self, texpr: &TypeExpr) -> Result<(u64, u64)> {
        let ty = self.type_of(texpr)?;
        self.tables
            .types
            .layout(&ty)
            .ok_or_else(|| Diagnostic::error(texpr.span(), "this type has no fixed size"))
    }

    /// Locals holding types are type names here: `vector[T]` with `T` a parameter.
    fn type_of(&self, texpr: &TypeExpr) -> Result<Type> {
        let extra: Vec<(String, Type)> = self
            .locals
            .borrow()
            .iter()
            .filter_map(|(n, v)| match v {
                Value::Type(ty) => Some((n.clone(), ty.clone())),
                _ => None,
            })
            .collect();
        self.tables.resolve_in(self.module, texpr, &extra)
    }

    fn call(&self, callee: &str, args: Vec<Value>, span: Span) -> Result<Value> {
        if let Some(func) = self.tables.comptime_fn(self.module, callee) {
            let outer = Outer { steps: self.steps, origin: self.origin, depth: self.depth + 1 };
            return self.tables.run(self.target, func, args, outer, span);
        }
        let span = if callee == "panic" { self.origin } else { span };
        builtin_call(self.tables, callee, args, span)
    }

    fn is_family(&self, ty: &Type, family: &str) -> bool {
        self.tables.is_family(ty, family)
    }
}

/// The `comptime fn`s every module has: `item(T)`, the type of what `T` holds, and `panic`.
pub(super) fn builtin_call(tables: &Tables, callee: &str, args: Vec<Value>, span: Span) -> Result<Value> {
    // A `dict[K, V]` or a `set[T]` holds what its last type argument names.
    let held = |ty: &Type| match ty {
        Type::Struct(named) => tables
            .templates
            .origin(super::templates::Kind::Struct, named.id)
            .and_then(|(_, args)| args.last().cloned()),
        ty => item(ty).cloned(),
    };
    match (callee, args.as_slice()) {
        ("item", [Value::Type(ty)]) => held(ty)
            .map(Value::Type)
            .ok_or_else(|| Diagnostic::error(span, format!("`{ty}` holds no items"))),
        ("item", _) => Err(Diagnostic::error(span, "`item` takes one type")),
        ("is_struct", [Value::Type(ty)]) => Ok(Value::Bool(matches!(ty, Type::Struct(_)))),
        ("is_enum", [Value::Type(ty)]) => Ok(Value::Bool(matches!(ty, Type::Enum { .. }))),
        // A `comptime fn` that panics stops the compilation with its message.
        ("panic", [Value::Str(message)]) => Err(Diagnostic::error(span, message.clone())),
        _ => Err(Diagnostic::error(span, format!("`{callee}` cannot be called at compile time"))),
    }
}

/// Compile-time code outside a `comptime fn`: a constant, a default, a `comptime if`.
pub(super) struct ModuleEnv<'a> {
    pub tables: &'a Tables,
    pub module: usize,
    pub target: &'a Target,
    /// Type parameters in scope, by name.
    pub extra: &'a [(String, Type)],
    /// Names known only where the code is, such as the `comptime` constants of a body.
    pub local: &'a dyn Fn(&str) -> Option<Value>,
}

impl Env for ModuleEnv<'_> {
    fn name(&self, name: &str, span: Span) -> Result<Value> {
        if let Some(value) = (self.local)(name) {
            return Ok(value);
        }
        self.tables.comptime_name(self.module, name, self.extra, span)
    }

    fn layout(&self, texpr: &TypeExpr) -> Result<(u64, u64)> {
        let ty = self.type_of(texpr)?;
        self.tables
            .types
            .layout(&ty)
            .ok_or_else(|| Diagnostic::error(texpr.span(), "this type has no fixed size"))
    }

    fn type_of(&self, texpr: &TypeExpr) -> Result<Type> {
        self.tables.resolve_in(self.module, texpr, self.extra)
    }

    fn call(&self, callee: &str, args: Vec<Value>, span: Span) -> Result<Value> {
        match self.tables.comptime_fn(self.module, callee) {
            Some(func) => self.tables.comptime_call(self.target, func, args, span),
            None => builtin_call(self.tables, callee, args, span),
        }
    }

    fn is_family(&self, ty: &Type, family: &str) -> bool {
        self.tables.is_family(ty, family)
    }
}

/// Whether a generic function is checked for each set of types it is called with, rather
/// than once for all: its body asks about its type parameters in a `comptime if`, or its
/// signature has a type a `comptime fn` gives.
pub(super) fn checked_per_call(f: &FnDecl, params: &[(String, Vec<usize>)]) -> bool {
    let computed = f.params.iter().map(|p| &p.ty).chain(&f.ret).any(type_call);
    computed || depends_on_params(&f.body, params)
}

fn type_call(texpr: &TypeExpr) -> bool {
    match texpr {
        TypeExpr::Call { .. } => true,
        TypeExpr::Named { args, .. } => args.iter().any(type_call),
        TypeExpr::Ref { inner, .. } => type_call(inner),
        TypeExpr::Tuple(items, _) => items.iter().any(type_call),
        TypeExpr::Fn { params, ret, .. } => {
            params.iter().any(type_call) || ret.as_deref().is_some_and(type_call)
        }
        TypeExpr::Size(..) => false,
    }
}

/// Whether a body holds a `comptime if` whose condition names one of `params`.
fn depends_on_params(body: &[Stmt], params: &[(String, Vec<usize>)]) -> bool {
    let named = |expr: &Expr| params.iter().any(|(name, _)| names(expr, name));
    body.iter().any(|stmt| match &stmt.kind {
        StmtKind::ComptimeIf(cif) => {
            cif.branches.iter().any(|(cond, body)| named(cond) || depends_on_params(body, params))
                || cif.otherwise.as_deref().is_some_and(|body| depends_on_params(body, params))
        }
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| depends_on_params(body, params))
                || otherwise.as_deref().is_some_and(|body| depends_on_params(body, params))
        }
        StmtKind::ComptimeFor { source, body, .. } => named(source) || depends_on_params(body, params),
        StmtKind::While { body, .. } | StmtKind::For { body, .. } | StmtKind::Block(body) => {
            depends_on_params(body, params)
        }
        StmtKind::With { body, .. } => depends_on_params(body, params),
        StmtKind::Try { body, handlers, .. } => {
            depends_on_params(body, params) || handlers.iter().any(|h| depends_on_params(&h.body, params))
        }
        StmtKind::Match { cases, .. } => cases.iter().any(|c| depends_on_params(&c.body, params)),
        _ => false,
    })
}

/// Whether `expr` mentions the name `name`.
fn names(expr: &Expr, name: &str) -> bool {
    match &expr.kind {
        ExprKind::Name(n) => n == name,
        ExprKind::Unary(_, e) | ExprKind::AddrOf { inner: e, .. } | ExprKind::Field { base: e, .. } => {
            names(e, name)
        }
        ExprKind::Binary(_, a, b) | ExprKind::Index { base: a, index: b } => names(a, name) || names(b, name),
        ExprKind::Call { args, type_args, .. } => {
            args.iter().any(|a| names(a, name)) || type_args.iter().any(|t| type_names(t, name))
        }
        ExprKind::Tuple(items) => items.iter().any(|e| names(e, name)),
        _ => false,
    }
}

fn type_names(texpr: &TypeExpr, name: &str) -> bool {
    match texpr {
        TypeExpr::Named { name: n, args, .. } => n == name || args.iter().any(|t| type_names(t, name)),
        TypeExpr::Ref { inner, .. } => type_names(inner, name),
        TypeExpr::Tuple(items, _) => items.iter().any(|t| type_names(t, name)),
        TypeExpr::Fn { params, ret, .. } => {
            params.iter().any(|t| type_names(t, name)) || ret.as_deref().is_some_and(|t| type_names(t, name))
        }
        TypeExpr::Call { args, .. } => args.iter().any(|t| type_names(t, name)),
        TypeExpr::Size(..) => false,
    }
}
