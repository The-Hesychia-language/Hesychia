//! The `managed` level: what the compiler fills in where `manual` asks the programmer.
//!
//! Variables are declared by their first assignment, their type widened to every value
//! they receive in the function, and numbers converted, checked, where a type is fixed.

use std::collections::HashMap;

use super::Checker;
use super::resolve::Symbol;
use crate::ast::{Expr, ExprKind, Item, Program, Stmt, StmtKind};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{FloatTy, IntTy, Type};

/// The type holding every value of both, if there is one: `i32` and `i64` give `i64`,
/// `u8` and `i16` give `i16`; `u64` and `i64` have none, nor large integers and floats.
pub(super) fn widen(a: &Type, b: &Type) -> Option<Type> {
    match (a, b) {
        _ if a == b => Some(a.clone()),
        // A length (`size`) meets other integers as an `i64`, checked when converted.
        (Type::Int(IntTy::Size), Type::Int(other)) | (Type::Int(other), Type::Int(IntTy::Size))
            if other.signed() =>
        {
            Some(Type::Int(IntTy::I64))
        }
        (Type::Int(x), Type::Int(y)) => {
            if x.contains(*y) {
                Some(a.clone())
            } else if y.contains(*x) {
                Some(b.clone())
            } else {
                // Signed and unsigned of the same size: the next signed size holds both.
                [IntTy::I16, IntTy::I32, IntTy::I64]
                    .into_iter()
                    .find(|t| t.contains(*x) && t.contains(*y))
                    .map(Type::Int)
            }
        }
        (Type::Float(_), Type::Float(_)) => Some(Type::Float(FloatTy::F64)),
        // An `f64` holds every integer up to 2^53 exactly.
        (Type::Int(i), Type::Float(_)) | (Type::Float(_), Type::Int(i)) if i.bits() <= 32 => {
            Some(Type::Float(FloatTy::F64))
        }
        _ => None,
    }
}

impl Checker<'_> {
    /// In `managed`, a number of another type where `ty` is expected is converted, with a
    /// check when the conversion can fail, and a warning saying so. Returns whether it was.
    pub(super) fn coerce(&mut self, value: &mut Expr, found: &Type, ty: &Type) -> bool {
        if !self.managed || found == ty || !found.is_numeric() || !ty.is_numeric() {
            return false;
        }
        let lossless = widen(found, ty).as_ref() == Some(ty) || is_length(found, ty);
        if !lossless {
            self.warnings.push(
                Diagnostic::warning(value.span, format!("this value is `{found}`; it is converted to `{ty}`, and stops the program if it does not fit"))
                    .with_hint(format!("consider making the destination `{found}`")),
            );
        }
        convert(value, ty);
        true
    }

    /// `a + b` with `a: i32` and `b: i64` in `managed`: both sides become the wider type.
    pub(super) fn coerce_pair(
        left: &mut Expr,
        right: &mut Expr,
        lt: &Type,
        rt: &Type,
        span: Span,
    ) -> Result<Type> {
        let wide = widen(lt, rt).ok_or_else(|| {
            Diagnostic::error(span, format!("no type holds both `{lt}` and `{rt}`"))
                .with_hint("convert one side explicitly")
        })?;
        if *lt != wide {
            convert(left, &wide);
        }
        if *rt != wide {
            convert(right, &wide);
        }
        Ok(wide)
    }

    /// A variable declared without a type takes the type of its value, or the wider type
    /// found for it in an earlier pass over the function.
    pub(super) fn declare_inferred(&mut self, name: &str, value: &mut Expr, span: Span) -> Result<()> {
        let key = (span.line, span.col);
        let ty = if let Some(ty) = self.widened.get(&key).cloned() {
            self.check_value(value, &ty)?;
            ty
        } else {
            let ty = self.check_expr(value, None)?;
            self.consume(value)?;
            ty
        };
        let large = self.tables.types.layout(&ty).is_some_and(|(size, _)| size > self.target.heap_threshold);
        let ty = if large { allocate(value, ty) } else { ty };
        let mut var = self.new_var(ty, true, span);
        var.inferred = Some(key);
        var.boxed = large;
        self.declare(name, var)
    }

    /// `a = v` or `a += v` on a variable declared without a type, with `v` of a wider type:
    /// the variable is widened for the whole function, which is checked again.
    pub(super) fn widen_variable(
        &mut self,
        key: (u32, u32),
        current: &Type,
        found: &Type,
        span: Span,
    ) -> Result<bool> {
        if !self.managed || !current.is_numeric() || !found.is_numeric() {
            return Ok(false);
        }
        let wide = widen(current, found).ok_or_else(|| {
            Diagnostic::error(span, format!("no type holds both `{current}` and `{found}`"))
                .with_hint("give the variable a type, and convert explicitly")
        })?;
        if wide == *current {
            return Ok(false);
        }
        self.widened.insert(key, wide);
        self.recheck = true;
        Ok(true)
    }
}

/// `size` to `i64`: a length, which fits in practice; converted with a check, but quietly.
fn is_length(found: &Type, ty: &Type) -> bool {
    *found == Type::Int(IntTy::Size) && *ty == Type::Int(IntTy::I64)
}

fn convert(value: &mut Expr, ty: &Type) {
    let span = value.span;
    let inner = std::mem::replace(value, Expr::new(ExprKind::None, span));
    *value = Expr { kind: ExprKind::Convert(Box::new(inner)), span, ty: Some(ty.clone()) };
}

/// How a function uses a parameter, weakest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Mode {
    Read,
    Mut,
    Owned,
}

impl Mode {
    fn of(ty: &Type) -> Mode {
        match ty {
            Type::Ref(_, false) => Mode::Read,
            Type::Ref(_, true) => Mode::Mut,
            _ => Mode::Owned,
        }
    }

    fn apply(self, ty: &Type) -> Type {
        match self {
            Mode::Read => Type::Ref(Box::new(ty.clone()), false),
            Mode::Mut => Type::Ref(Box::new(ty.clone()), true),
            Mode::Owned => ty.clone(),
        }
    }
}

/// The parameters of the function being probed, taken by value so that every use is
/// allowed, and the strongest use seen of each.
#[derive(Default)]
pub(super) struct Probe {
    params: Vec<(usize, Type)>,
    /// Variable id → (parameter position, strongest use, where it is kept if it is).
    pub vars: HashMap<usize, (usize, Mode, Option<Span>)>,
}

impl Probe {
    pub fn declared_type(&self, position: usize) -> Option<Type> {
        self.params.iter().find(|(p, _)| *p == position).map(|(_, ty)| ty.clone())
    }

    pub fn watch(&mut self, position: usize, var: usize) {
        if self.params.iter().any(|(p, _)| *p == position) {
            self.vars.insert(var, (position, Mode::Read, None));
        }
    }

    pub fn note(&mut self, var: usize, mode: Mode, span: Option<Span>) {
        if let Some((_, seen, kept)) = self.vars.get_mut(&var) {
            if mode == Mode::Owned && kept.is_none() {
                *kept = span;
            }
            *seen = (*seen).max(mode);
        }
    }
}

impl Checker<'_> {
    /// Infers the mode of each `managed` parameter written without one: `owned` if the
    /// body keeps it, `&mut` if it changes it, `&` otherwise. Calls between functions
    /// make modes depend on each other, so the bodies are probed until none changes.
    pub(super) fn infer_modes(&mut self, program: &Program) {
        let mut changed = true;
        while changed {
            changed = false;
            for (module, items) in program.modules.iter().map(|m| &m.items).enumerate() {
                self.module = module;
                for item in items {
                    match item {
                        Item::Fn(f) => {
                            let Some(Symbol::Fn(index)) = self.tables.lookup(module, &f.name) else {
                                continue;
                            };
                            changed |= self.probe_fn(f, index);
                        }
                        Item::Struct(s) if s.generics.is_empty() => {
                            let Some(Symbol::Struct(id)) = self.tables.lookup(module, &s.name) else {
                                continue;
                            };
                            self.self_type =
                                Some(Type::Struct(crate::types::Named { id, name: s.name.clone() }));
                            for method in &s.methods {
                                let index = self.tables.methods[&(id, method.name.clone())];
                                changed |= self.probe_fn(method, index);
                            }
                            self.self_type = None;
                        }
                        _ => {}
                    }
                }
            }
            changed |= std::mem::take(&mut self.modes_forced);
        }
    }

    /// Checks a copy of the body with the parameters owned, and strengthens their modes
    /// to what it needs. Errors are left for the real check. Returns whether a mode changed.
    fn probe_fn(&mut self, f: &crate::ast::FnDecl, index: usize) -> bool {
        if !self.tables.managed.contains(&index) {
            return false;
        }
        let params = self.tables.inferred.get(&index).cloned().unwrap_or_default();
        let mut f = f.clone();
        let (warnings, lambdas) = (self.warnings.len(), self.lambdas);
        let raises = self.tables.fns[index].raises.clone();
        self.probe.params = params;
        let result = self.check_body(&mut f, index);
        self.warnings.truncate(warnings);
        self.lambdas = lambdas;
        let probe = std::mem::take(&mut self.probe);
        if result.is_err() {
            return false;
        }
        let mut changed = self.tables.fns[index].raises != raises;
        for (position, used, kept) in probe.vars.into_values() {
            if let Some(span) = kept {
                self.tables.kept_at.insert((index, position), span);
            }
            let param = &mut self.tables.fns[index].params[position];
            let ty = probe.params.iter().find(|(p, _)| *p == position).map(|(_, t)| t).expect("probed");
            // Changing what a `shared` value holds does not change the handle itself.
            let used = if matches!(ty, Type::Shared(_)) && used == Mode::Mut { Mode::Read } else { used };
            let mode = used.max(Mode::of(&param.ty));
            if mode != Mode::of(&param.ty) {
                param.ty = mode.apply(ty);
                changed = true;
            }
        }
        changed
    }
}

impl Checker<'_> {
    /// `[value for var in iter if filter]` becomes a hidden vector filled by a `for` loop:
    /// `result.push(value)` for each item that passes. The value is first checked on a copy,
    /// to learn the type of the vector.
    pub(super) fn check_comprehension(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        if !self.managed {
            return Err(Diagnostic::error(span, "list comprehensions are only for `managed` code")
                .with_hint("they allocate a vector without showing it; write the loop"));
        }
        let ExprKind::Comprehension { value, var, names, iter, filter, built } = &mut expr.kind else {
            unreachable!("called on a comprehension")
        };
        let item = self.comprehension_item(value, var, names, iter, span)?;
        self.comprehensions += 1;
        let result = format!("hsy_comprehension{}", self.comprehensions);
        let ty = Type::Vector(Box::new(item));
        let target = Expr::new(ExprKind::Name(result.clone()), span);
        let push = ExprKind::Method {
            receiver: Box::new(target),
            name: "push".into(),
            type_args: Vec::new(),
            args: vec![*value.clone()],
        };
        let mut body = vec![Stmt { kind: StmtKind::Expr(Expr::new(push, span)), span }];
        if let Some(filter) = filter.take() {
            body =
                vec![Stmt { kind: StmtKind::If { branches: vec![(*filter, body)], otherwise: None }, span }];
        }
        if !names.is_empty() {
            body.insert(0, take_apart(names, var, span));
        }
        let iter = std::mem::replace(iter.as_mut(), Expr::new(ExprKind::None, span));
        let mut fill = Stmt { kind: StmtKind::For { var: var.clone(), iter, body }, span };
        self.push_scope();
        let vector = self.new_var(ty.clone(), true, span);
        vector.used.set(true);
        self.declare(&result, vector)?;
        let checked = self.check_stmt(&mut fill);
        self.pop_scope();
        checked?;
        *built = Some((result, Box::new(fill)));
        Ok(ty)
    }

    /// The type of each value of a comprehension, from a copy of it checked with the loop
    /// variable in scope; what this check records is undone.
    fn comprehension_item(
        &mut self,
        value: &Expr,
        var: &str,
        names: &[String],
        iter: &Expr,
        span: Span,
    ) -> Result<Type> {
        let (warnings, lambdas, moved) = (self.warnings.len(), self.lambdas, self.moved.clone());
        let mut iter = iter.clone();
        let mut value = value.clone();
        let result = self.check_iterable(&mut iter, span).and_then(|(element, _)| {
            self.push_scope();
            let item = self.new_var(element, false, value.span);
            item.used.set(true);
            let mut declared = self.declare(var, item);
            if !names.is_empty() {
                declared = declared.and_then(|()| self.check_stmt(&mut take_apart(names, var, span)));
            }
            let ty = declared.and_then(|()| self.check_expr(&mut value, None));
            self.pop_scope();
            ty
        });
        self.warnings.truncate(warnings);
        self.lambdas = lambdas;
        self.moved = moved;
        result
    }
}

impl Checker<'_> {
    /// A `T` given where a `heap[T]` (or an `Optional[heap[T]]`) is expected is allocated,
    /// as `allocate(value)`. Returns the type of the value as it now stands.
    pub(super) fn allocate_for(&mut self, value: &mut Expr, found: Type, ty: &Type) -> Result<Type> {
        let target = match ty {
            Type::Optional(inner) => inner.as_ref(),
            other => other,
        };
        let Type::Heap(inner) = target else { return Ok(found) };
        if **inner != found {
            return Ok(found);
        }
        self.consume(value)?;
        let span = value.span;
        let held = std::mem::replace(value, Expr::new(ExprKind::None, span));
        let heap = Type::Heap(Box::new(found));
        let kind = ExprKind::Builtin { op: crate::ast::Builtin::Allocate, args: vec![held] };
        *value = Expr { kind, span, ty: Some(heap.clone()) };
        Ok(heap)
    }
}

/// A `heap[T]` read where a `T` is expected, in `managed`: the value it points to.
pub(super) fn read_heap(value: &mut Expr, found: Type, ty: &Type) -> Type {
    match found {
        Type::Heap(inner) | Type::Shared(inner) if *inner == *ty => {
            let span = value.span;
            let pointer = std::mem::replace(value, Expr::new(ExprKind::None, span));
            *value = Expr { kind: ExprKind::Deref(Box::new(pointer)), span, ty: Some(ty.clone()) };
            *inner
        }
        found => found,
    }
}

/// A value too large for the stack goes to the heap: `allocate(value)`.
fn allocate(value: &mut Expr, ty: Type) -> Type {
    let span = value.span;
    let held = std::mem::replace(value, Expr::new(ExprKind::None, span));
    let heap = Type::Heap(Box::new(ty));
    let kind = ExprKind::Builtin { op: crate::ast::Builtin::Allocate, args: vec![held] };
    *value = Expr { kind, span, ty: Some(heap.clone()) };
    heap
}

/// `a, b = item`, the first statement of a comprehension over pairs.
fn take_apart(names: &[String], var: &str, span: Span) -> Stmt {
    let value = Expr::new(ExprKind::Name(var.to_string()), span);
    let kind = StmtKind::Var {
        mutable: false,
        target: crate::ast::VarTarget::Tuple(names.to_vec()),
        ty: None,
        value,
    };
    Stmt { kind, span }
}
