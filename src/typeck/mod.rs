//! Type checker: resolves names and types, enforces the rules of the spec, annotates expressions.

mod builtin;
mod call;
mod comptime_fn;
mod errors;
mod expr;
mod extend;
mod generics;
mod lambda;
mod low_level;
mod managed;
mod matching;
mod nested;
mod operators;
mod ownership;
mod parallel;
mod place;
mod reflect;
mod resolve;
mod stmt;
mod templates;
mod threads;
mod traits;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use crate::ast::{FnDecl, Item, Program, StructDecl, TypeExpr, method_c_name};
use crate::comptime::{Target, Value};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{Type, TypeTable};
use resolve::{Symbol, Tables};

/// What code generation needs from the checker.
pub struct Checked {
    pub warnings: Vec<Diagnostic>,
    /// The copies `managed` code inserted, for `--warn-copies`.
    pub copies: Vec<Diagnostic>,
    /// The C name of the `@panic_handler` function.
    pub panic_handler: Option<String>,
    pub types: TypeTable,
    /// Parameter and return types of every function, by C name.
    pub signatures: HashMap<String, Signature>,
}

/// Parameter types, return type and error types of a function.
pub struct Signature {
    pub params: Vec<Type>,
    pub ret: Type,
    pub raises: Vec<Type>,
}

/// Checks the program in place.
pub fn check(program: &mut Program, target: &Target) -> Result<Checked> {
    let tables = resolve::build(program, target)?;
    let mut checker = Checker::new(tables, target);
    checker.collect_generics(program);
    checker.infer_modes(program);
    for (index, module) in program.modules.iter_mut().enumerate() {
        checker.module = index;
        for item in &mut module.items {
            match item {
                Item::Fn(f) => checker.check_fn(f)?,
                Item::Struct(s) if s.generics.is_empty() => checker.check_methods(s)?,
                Item::Global(g) => checker.check_global(g)?,
                _ => {}
            }
        }
    }
    checker.check_instances(program)?;
    let signatures = checker
        .tables
        .fns
        .iter()
        .filter(|sig| sig.generics.is_empty() && !sig.c_name.is_empty())
        .map(|sig| {
            let params = sig.params.iter().map(|p| p.ty.clone()).collect();
            (sig.c_name.clone(), Signature { params, ret: sig.ret.clone(), raises: sig.raises.clone() })
        })
        .collect();
    // Each specialization of a generic body is checked again; its warnings are said once.
    let mut warnings: Vec<Diagnostic> = Vec::new();
    for warning in checker.warnings {
        let seen = warnings.iter().any(|w| {
            (w.span.file, w.span.line, w.span.col) == (warning.span.file, warning.span.line, warning.span.col)
                && w.message == warning.message
        });
        if !seen {
            warnings.push(warning);
        }
    }
    let (copies, warnings) = warnings.into_iter().partition(|w| w.severity == crate::diag::Severity::Note);
    let panic_handler = checker.tables.panic_handler.map(|i| checker.tables.fns[i].c_name.clone());
    Ok(Checked { warnings, copies, panic_handler, types: checker.tables.types, signatures })
}

/// The method of the built-in trait behind an arithmetic operator: `add` for `+`...
pub fn operator_method(op: crate::ast::BinOp) -> Option<&'static str> {
    expr::operator_trait(op).map(|(_, method)| method)
}

/// A scalar type written by name, such as `i32` or `bool`.
fn scalar_type(texpr: &TypeExpr) -> Option<Type> {
    match texpr {
        TypeExpr::Named { name, args, .. } if args.is_empty() => Type::scalar(name),
        _ => None,
    }
}

struct Var {
    id: usize,
    ty: Type,
    mutable: bool,
    /// Loop nesting where the variable was declared; moving it from deeper loops is refused.
    depth: usize,
    /// Set for `comptime` constants, whose uses become literals.
    constant: Option<Value>,
    /// Set for a `managed` variable declared without a type: where it was declared.
    inferred: Option<(u32, u32)>,
    /// A `managed` variable too large for the stack: its type is `heap[T]`, and it reads as a `T`.
    boxed: bool,
    used: Cell<bool>,
    span: Span,
}

struct Checker<'a> {
    tables: Tables,
    target: &'a Target,
    module: usize,
    scopes: Vec<HashMap<String, Var>>,
    next_var: usize,
    /// Variables whose value was moved away, with the place of the move.
    moved: HashMap<usize, Span>,
    /// `Optional` places known to hold a value here, after `x is not None`:
    /// the variable and the place's text, such as `item.parent`.
    narrowed: HashSet<(usize, String)>,
    ret: Type,
    /// The function being checked is `managed`.
    managed: bool,
    /// Types widened for `managed` variables, by declaration; set means check again.
    widened: HashMap<(u32, u32), Type>,
    recheck: bool,
    /// `managed` calls to fallible functions, which get an implicit `check`.
    unchecked: Vec<(*const crate::ast::Expr, Vec<Type>)>,
    /// The function's `raises` grows with what it lets escape (`managed`).
    infer_raises: bool,
    /// Variables copied because they are used again, with where: a later change to one
    /// does not reach its copy, which `managed` points out.
    copied: HashMap<usize, Span>,
    /// Open `with scope()` blocks, innermost last: what their threads borrow.
    lending: Vec<threads::Lending>,
    /// A lent value was changed in the statement being checked.
    lend_error: Option<Diagnostic>,
    /// `managed` loop variables, by variable id, with their loop's position.
    loop_vars: HashMap<usize, (u32, u32)>,
    /// `managed` loops whose body changes the items: they iterate with `&mut`.
    mut_loops: HashSet<(u32, u32)>,
    /// Vectors being iterated by `for`, innermost last, and whether the loop changes them.
    iterating: Vec<(usize, bool)>,
    /// Variables moved into a `managed` function that keeps them: its name, and where.
    moved_into: HashMap<usize, (String, Span)>,
    /// Moves that `managed` code turns into copies, by position: the value is used again.
    copies: HashSet<(u32, u32)>,
    /// The inferred variable a compound assignment is writing, for widening.
    assigning: Option<(u32, u32)>,
    /// While inferring parameter modes: the parameters probed, and how each is used.
    probe: managed::Probe,
    /// A function given to `spawn` had its parameters made `owned`: infer again.
    modes_forced: bool,
    /// Errors the current function declares, and its name for messages.
    raises: Vec<Type>,
    fn_name: String,
    /// The struct whose method is being checked, for `Self`.
    self_type: Option<Type>,
    /// Type parameters in scope: `Type::Param` in a generic body, concrete in a specialization.
    type_params: Vec<(String, Type)>,
    /// The traits each type parameter must implement, in a generic body.
    bounds: HashMap<String, Vec<usize>>,
    /// The compile-time value parameters in scope, with their integer type.
    values: Vec<(String, Type)>,
    generics: generics::Generics,
    /// The scope depth at which each open lambda starts, and the lambdas made so far.
    lambda_bounds: Vec<usize>,
    /// The variables from outside each `parallel for` being checked, which it only reads.
    parallel_reads: Vec<Vec<String>>,
    /// The type expected for the call being checked, so `Pair(1, 2)` can take its type
    /// arguments from where it goes.
    expected_struct: Option<Type>,
    lambdas: usize,
    /// Hidden vectors made for comprehensions, for unique names.
    comprehensions: usize,
    /// Error types reaching each open `try`, innermost last.
    tries: Vec<Vec<Type>>,
    /// The call that the `check` being checked applies to, and the errors it found there.
    checking: Option<*const crate::ast::Expr>,
    checked_raises: Option<Vec<Type>>,
    /// Checking a `return` value: moving out of a loop is fine there.
    returning: bool,
    /// The loop depth the code after the current statement leaves to, by `return` (0) or
    /// `break`: a value moved before it is not moved again by the loops it leaves.
    leaves_to: Option<usize>,
    /// The variable the current statement assigns anew, as `x = f(x)`: moving it out in a
    /// loop is fine, it is filled again before the next turn.
    refilled: Option<usize>,
    /// Set while the checker lends a value itself, as the one an f-string formats: a
    /// temporary is then kept for the call and released after, in `manual` too.
    lends_temporaries: bool,
    /// The mutexes the `with` blocks around the code hold, as written.
    locked: Vec<String>,
    loop_depth: usize,
    warnings: Vec<Diagnostic>,
}

impl<'a> Checker<'a> {
    fn new(tables: Tables, target: &'a Target) -> Self {
        Self {
            tables,
            target,
            module: 0,
            scopes: Vec::new(),
            next_var: 0,
            moved: HashMap::new(),
            narrowed: HashSet::new(),
            ret: Type::Void,
            returning: false,
            leaves_to: None,
            refilled: None,
            lends_temporaries: false,
            locked: Vec::new(),
            managed: false,
            widened: HashMap::new(),
            recheck: false,
            assigning: None,
            copies: HashSet::new(),
            moved_into: HashMap::new(),
            iterating: Vec::new(),
            loop_vars: HashMap::new(),
            mut_loops: HashSet::new(),
            lending: Vec::new(),
            lend_error: None,
            copied: HashMap::new(),
            unchecked: Vec::new(),
            infer_raises: false,
            probe: managed::Probe::default(),
            modes_forced: false,
            raises: Vec::new(),
            fn_name: String::new(),
            self_type: None,
            type_params: Vec::new(),
            bounds: HashMap::new(),
            values: Vec::new(),
            generics: generics::Generics::default(),
            lambda_bounds: Vec::new(),
            parallel_reads: Vec::new(),
            expected_struct: None,
            lambdas: 0,
            comprehensions: 0,
            tries: Vec::new(),
            checking: None,
            checked_raises: None,
            loop_depth: 0,
            warnings: Vec::new(),
        }
    }

    fn check_methods(&mut self, s: &mut StructDecl) -> Result<()> {
        let Some(Symbol::Struct(id)) = self.global(&s.name) else { unreachable!("registered struct") };
        self.self_type = Some(Type::Struct(crate::types::Named { id, name: s.name.clone() }));
        for method in &mut s.methods {
            let c_name = method_c_name(self.module, &s.name, &method.name);
            let index = self.tables.fns.iter().position(|sig| sig.c_name == c_name).expect("signed method");
            let generics = self.tables.fns[index].generics.clone();
            self.type_params = generics.iter().map(|(n, _)| (n.clone(), Type::Param(n.clone()))).collect();
            let deferred = comptime_fn::checked_per_call(method, &generics);
            self.bounds = generics.into_iter().collect();
            let result = if deferred { Ok(()) } else { self.check_body(method, index) };
            self.type_params.clear();
            self.bounds.clear();
            result?;
        }
        self.self_type = None;
        Ok(())
    }

    fn check_fn(&mut self, f: &mut FnDecl) -> Result<()> {
        let Some(Symbol::Fn(index)) = self.global(&f.name) else { unreachable!("registered function") };
        let generics = self.tables.fns[index].generics.clone();
        self.values = self.tables.fns[index].values.clone();
        self.type_params = generics.iter().map(|(n, _)| (n.clone(), Type::Param(n.clone()))).collect();
        let deferred = comptime_fn::checked_per_call(f, &generics);
        self.bounds = generics.into_iter().collect();
        let result = if deferred { Ok(()) } else { self.check_body(f, index) };
        self.type_params.clear();
        self.bounds.clear();
        self.values.clear();
        result
    }

    /// A module constant's value, checked as `manual` code against its type.
    fn check_global(&mut self, g: &mut crate::ast::GlobalDecl) -> Result<()> {
        let Some(Symbol::Global(index)) = self.global(&g.name) else { unreachable!("registered constant") };
        let ty = self.tables.globals[index].ty.clone();
        self.managed = false;
        self.check_value(&mut g.value, &ty)
    }

    /// Keeps an unchecked copy of each generic function, to specialize later.
    fn collect_generics(&mut self, program: &Program) {
        for (module, items) in program.modules.iter().map(|m| &m.items).enumerate() {
            for item in items {
                if let Item::Fn(f) = item
                    && !f.generics.is_empty()
                    && let Some(Symbol::Fn(index)) = self.tables.lookup(module, &f.name)
                {
                    self.generics.decls.insert(index, (module, f.clone()));
                }
            }
        }
    }

    /// Checks every specialization asked for, including those asked for by others, and adds
    /// them to their module.
    fn check_instances(&mut self, program: &mut Program) -> Result<()> {
        loop {
            if let Some(mut instance) = self.generics.pending.pop() {
                self.module = instance.module;
                self.type_params = instance.bindings;
                self.values = instance.values;
                self.self_type = instance.owner;
                self.check_body(&mut instance.decl, instance.sig)?;
                self.type_params.clear();
                self.values.clear();
                self.self_type = None;
                program.modules[instance.module].items.push(Item::Fn(instance.decl));
            } else if let Some(mut method) = self.tables.templates.pending.pop() {
                self.module = method.module;
                self.type_params = method.bindings;
                self.self_type = Some(method.owner);
                self.check_body(&mut method.decl, method.sig)?;
                self.type_params.clear();
                self.self_type = None;
                program.modules[method.module].items.push(Item::Fn(method.decl));
            } else {
                return Ok(());
            }
        }
    }

    /// Checks a body against the signature `index`, with its parameters in scope.
    /// Checks a body; a `managed` body is checked again while its variables get wider.
    fn check_body(&mut self, f: &mut FnDecl, index: usize) -> Result<()> {
        self.managed = f.level == crate::ast::Level::Managed;
        self.widened.clear();
        self.copies.clear();
        self.mut_loops.clear();
        self.infer_raises = self.managed;
        let original = self.managed.then(|| f.body.clone());
        let warnings = self.warnings.len();
        // Each check can only widen what the last one learned; past this many, it learns no more.
        let mut attempts = 0;
        loop {
            self.recheck = false;
            attempts += 1;
            let result = self.check_body_once(f, index);
            // Asked to check again: what failed this time may not fail next time.
            if result.is_err() && !(self.recheck && original.is_some() && attempts < 64) {
                return result;
            }
            match &original {
                Some(body) if self.recheck && attempts < 64 => {
                    f.body.clone_from(body);
                    self.warnings.truncate(warnings);
                }
                _ => break,
            }
        }
        if self.infer_raises {
            self.tables.fns[index].raises.clone_from(&self.raises);
        }
        Ok(())
    }

    fn check_body_once(&mut self, f: &mut FnDecl, index: usize) -> Result<()> {
        let sig = &self.tables.fns[index];
        self.ret = sig.ret.clone();
        self.raises = sig.raises.clone();
        self.fn_name.clone_from(&f.name);
        let params: Vec<(String, Type)> = sig.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
        self.moved.clear();
        self.narrowed.clear();
        self.copied.clear();
        self.moved_into.clear();
        self.probe.vars.clear();
        // A pass that fails leaves its scopes open; a `managed` function is then checked
        // again, and the functions after it must not see its variables.
        let depth = self.scopes.len();
        let result = self.check_scoped_body(f, params);
        self.scopes.truncate(depth);
        result?;
        if self.ret != Type::Void && !stmt::always_returns(&f.body) {
            return Err(Diagnostic::error(
                f.span,
                format!("function `{}` can end without returning a value", f.name),
            ));
        }
        Ok(())
    }

    /// The parameters, then the body, in a scope of their own.
    fn check_scoped_body(&mut self, f: &mut FnDecl, params: Vec<(String, Type)>) -> Result<()> {
        self.push_scope();
        for (position, (name, ty)) in params.into_iter().enumerate() {
            let ty = self.probe.declared_type(position).unwrap_or(ty);
            // Every `managed` variable can be changed, parameters included.
            let mutable = self.managed && !matches!(ty, Type::Ref(..));
            let var = self.new_var(ty, mutable, f.span);
            self.probe.watch(position, var.id);
            if name == "self" {
                var.used.set(true);
            }
            self.declare(&name, var)?;
        }
        self.check_stmts(&mut f.body)?;
        self.pop_scope();
        Ok(())
    }

    /// A type argument, which may be a generic type without its arguments: `load[dict]`.
    fn resolve_arg(&mut self, texpr: &TypeExpr) -> Result<Type> {
        loop {
            let result = self.tables.resolve_arg(self.module, texpr, &self.type_params);
            if !self.tables.make_missing(result.is_err())? {
                return result;
            }
        }
    }

    fn resolve(&mut self, texpr: &TypeExpr) -> Result<Type> {
        let mut extra: Vec<(String, Type)> =
            self.self_type.iter().map(|t| ("Self".to_string(), t.clone())).collect();
        extra.extend(self.type_params.iter().cloned());
        self.tables.resolve_mut(self.module, texpr, &extra)
    }

    fn new_var(&mut self, ty: Type, mutable: bool, span: Span) -> Var {
        self.next_var += 1;
        let depth = self.loop_depth;
        Var {
            id: self.next_var,
            ty,
            mutable,
            depth,
            constant: None,
            inferred: None,
            boxed: false,
            used: Cell::new(false),
            span,
        }
    }

    fn declare(&mut self, name: &str, var: Var) -> Result<()> {
        let span = var.span;
        let scope = self.scopes.last_mut().expect("a scope is open");
        if scope.insert(name.to_string(), var).is_some() {
            return Err(Diagnostic::error(span, format!("`{name}` is already defined in this block")));
        }
        Ok(())
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Closes a scope and warns about variables that were never read.
    fn pop_scope(&mut self) {
        let scope = self.scopes.pop().expect("a scope is open");
        // In a body checked for given types, a `comptime if` may leave a variable to the
        // other branches.
        if self.type_params.iter().any(|(_, ty)| !matches!(ty, Type::Param(_))) {
            return;
        }
        let mut unused: Vec<_> =
            scope.into_iter().filter(|(name, var)| !var.used.get() && !name.starts_with('_')).collect();
        unused.sort_by_key(|(_, var)| (var.span.line, var.span.col));
        for (name, var) in unused {
            self.warnings.push(
                Diagnostic::warning(var.span, format!("unused variable `{name}`"))
                    .with_hint(format!("remove it, or name it `_{name}` if that is intended")),
            );
        }
    }

    fn find_local(&self, name: &str) -> Option<&Var> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    /// Finds a local variable or constant and marks it as read.
    fn local(&self, name: &str) -> Option<&Var> {
        let var = self.find_local(name)?;
        var.used.set(true);
        Some(var)
    }

    fn global(&self, name: &str) -> Option<Symbol> {
        self.tables.lookup(self.module, name)
    }

    /// A name the module neither defines nor imports: free, or known only from the prelude,
    /// which a local variable may hide.
    fn free_name(&self, name: &str) -> bool {
        let Some(symbol) = self.global(name) else { return true };
        let prelude = self.tables.module_paths.iter().position(|path| path == crate::stdlib::PRELUDE);
        prelude.is_some_and(|module| {
            module != self.module && self.tables.exports[module].get(name).is_some_and(|e| e.symbol == symbol)
        })
    }

    /// Compile-time value of a name, for `comptime` code inside functions.
    fn const_value(&self, name: &str, span: Span) -> Result<Value> {
        if let Some(value) = self.local(name).and_then(|var| var.constant.clone()) {
            return Ok(value);
        }
        if let Some((_, value)) = self.value_param(name) {
            return Ok(Value::Int(value));
        }
        match self.global(name) {
            Some(Symbol::Const(i)) => Ok(self.tables.consts[i].value.clone()),
            _ => Err(Diagnostic::error(span, format!("`{name}` is not a compile-time constant"))),
        }
    }

    /// A compile-time parameter such as `[N: size]`: its type, and its value in a
    /// specialization (a placeholder of 1 while the generic body is checked).
    fn value_param(&self, name: &str) -> Option<(Type, i128)> {
        let (_, ty) = self.values.iter().find(|(n, _)| n == name)?;
        let value = match self.type_params.iter().find(|(n, _)| n == name) {
            Some((_, Type::Const(value))) => i128::from(*value),
            _ => 1,
        };
        Some((ty.clone(), value))
    }

    /// Evaluates, making the generic types the code names as it goes.
    fn eval_mut(&mut self, expr: &crate::ast::Expr) -> Result<Value> {
        loop {
            let result = self.eval(expr);
            if !self.tables.make_missing(result.is_err())? {
                return result;
            }
        }
    }

    fn eval(&self, expr: &crate::ast::Expr) -> Result<Value> {
        let local = |name: &str| match self.local(name).and_then(|var| var.constant.clone()) {
            Some(value) => Some(value),
            None => self.value_param(name).map(|(_, value)| Value::Int(value)),
        };
        let env = comptime_fn::ModuleEnv {
            tables: &self.tables,
            module: self.module,
            target: self.target,
            extra: &self.type_params,
            local: &local,
        };
        crate::comptime::eval(expr, self.target, &env)
    }
}

fn expect_type(found: &Type, expected: &Type, span: Span) -> Result<()> {
    if found == expected {
        return Ok(());
    }
    let mut diag = Diagnostic::error(span, format!("expected `{expected}`, found `{found}`"));
    if found.is_numeric() && expected.is_numeric() {
        diag = diag.with_hint(format!("convert explicitly with `{expected}(...)`"));
    }
    Err(diag)
}
