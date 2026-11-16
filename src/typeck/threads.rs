//! The `thread` module: threads, channels and atomics. What goes to another thread is
//! moved there, and holds no reference nor `shared` value: no data is reachable from two
//! threads except through `atomic` values lent inside a `scope`.

use super::Checker;
use std::collections::HashMap;

use crate::ast::{Builtin, Expr, ExprKind, Stmt};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::Type;

impl Checker<'_> {
    /// `spawn(f, args...)`: `f` is a function, not a capturing lambda; the arguments are
    /// moved into the thread, and `thread[R]` gives `f`'s result when joined.
    pub(super) fn check_spawn(&mut self, expr: &mut Expr) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Call { args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        let Some((function, rest)) = args.split_first_mut() else {
            return Err(Diagnostic::error(span, "`spawn` takes a function, then its arguments"));
        };
        let mut found = self.check_expr(function, None)?;
        if let ExprKind::FnRef(c_name) = &function.kind {
            found = self.own_spawned_params(c_name).unwrap_or(found);
        }
        let Type::Fn(params, ret) = found else {
            return Err(Diagnostic::error(
                function.span,
                format!("`spawn` runs a function, found `{found}`"),
            )
            .with_hint("a lambda that captures cannot outlive what it borrows; use a `scope`"));
        };
        if params.len() != rest.len() {
            return Err(Diagnostic::error(
                span,
                format!("the function takes {} value(s), {} given", params.len(), rest.len()),
            ));
        }
        for (arg, param) in rest.iter_mut().zip(&params) {
            self.check_sendable(param, arg.span)?;
            self.check_value(arg, param)?;
        }
        self.check_sendable(&ret, span)?;
        let all = std::mem::take(args);
        expr.kind = ExprKind::Builtin { op: Builtin::Spawn, args: all };
        Ok(Type::Thread(ret))
    }

    /// A `managed` function run by `spawn` keeps its parameters: their inferred modes
    /// become `owned`. Returns the function's type if it changed.
    fn own_spawned_params(&mut self, c_name: &str) -> Option<Type> {
        let index = self.tables.fns.iter().position(|sig| sig.c_name == c_name)?;
        let inferred = self.tables.inferred.get(&index)?.clone();
        let mut changed = false;
        for (position, ty) in inferred {
            let param = &mut self.tables.fns[index].params[position];
            if param.ty != ty {
                param.ty = ty;
                changed = true;
            }
        }
        self.modes_forced |= changed;
        let sig = &self.tables.fns[index];
        Some(Type::Fn(sig.params.iter().map(|p| p.ty.clone()).collect(), Box::new(sig.ret.clone())))
    }

    /// `channel[T]()`: a `(sender[T], receiver[T])` pair; `T` may also come from the
    /// expected type.
    pub(super) fn check_channel(&mut self, expr: &mut Expr, expected: Option<&Type>) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Call { type_args, args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        if !args.is_empty() {
            return Err(Diagnostic::error(span, "`channel[T]()` takes no value"));
        }
        let item = match (type_args.as_slice(), expected) {
            ([item], _) => {
                let item = item.clone();
                self.resolve(&item)?
            }
            ([], Some(Type::Tuple(ends))) if matches!(ends.first(), Some(Type::Sender(_))) => {
                let Some(Type::Sender(item)) = ends.first() else { unreachable!("matched above") };
                item.as_ref().clone()
            }
            _ => return Err(Diagnostic::error(span, "write the type of the messages: `channel[str]()`")),
        };
        self.check_sendable(&item, span)?;
        expr.kind = ExprKind::Builtin { op: Builtin::Channel, args: Vec::new() };
        Ok(Type::Tuple(vec![Type::Sender(Box::new(item.clone())), Type::Receiver(Box::new(item))]))
    }

    /// `tx.send(x)` moves `x` into the channel; `rx.receive()` waits for a message, and
    /// gives `None` once every sender is gone and nothing is left.
    pub(super) fn check_channel_method(
        &mut self,
        receiver: Expr,
        name: &str,
        args: &mut [Expr],
        span: Span,
    ) -> Result<(ExprKind, Type)> {
        match (receiver.ty().clone(), name, args) {
            (Type::Sender(item), "send", [value]) => {
                self.check_value(value, &item)?;
                let value = std::mem::replace(value, Expr::new(ExprKind::None, span));
                Ok((ExprKind::Builtin { op: Builtin::ChanSend, args: vec![receiver, value] }, Type::Void))
            }
            (Type::Receiver(item), "receive", []) => Ok((
                ExprKind::Builtin { op: Builtin::ChanReceive, args: vec![receiver] },
                Type::Optional(item),
            )),
            (ty, _, _) => {
                Err(Diagnostic::error(span, format!("`{ty}` has no method `{name}` taking these values"))
                    .with_hint("a sender has `send(x)` and `copy()`, a receiver `receive()`"))
            }
        }
    }

    /// `mutex(x)`: `x` moves in, and is reached inside `with m as x:` only.
    pub(super) fn check_mutex(&mut self, expr: &mut Expr, expected: Option<&Type>) -> Result<Type> {
        let span = expr.span;
        let hint = match expected {
            Some(Type::Mutex(item)) => Some(item.as_ref().clone()),
            _ => None,
        };
        let ExprKind::Call { args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        let [value] = args.as_mut_slice() else {
            return Err(Diagnostic::error(span, "`mutex` takes its first value: `mutex(0)`"));
        };
        let item = self.check_expr(value, hint.as_ref())?;
        if self.tables.types.contains_shared(&item) || matches!(item, Type::Ref(..) | Type::Void) {
            return Err(Diagnostic::error(value.span, format!("a `mutex` cannot hold a `{item}`")));
        }
        self.consume(value)?;
        let args = std::mem::take(args);
        expr.kind = ExprKind::Builtin { op: Builtin::MutexNew, args };
        Ok(Type::Mutex(Box::new(item)))
    }

    /// `with m as x:` locks the mutex `m`; `x` is its value, to read and change, until the
    /// block is left.
    fn check_locked(
        &mut self,
        value: &mut Expr,
        item: Type,
        name: &str,
        body: &mut [Stmt],
        span: Span,
    ) -> Result<()> {
        let Some(place) = self.place_of(value) else {
            return Err(Diagnostic::error(value.span, "`with` locks a mutex kept in a variable"));
        };
        if self.locked.contains(&place.text) {
            return Err(Diagnostic::error(
                value.span,
                format!("`{}` is already locked by the `with` around", place.text),
            )
            .with_hint("locking it again would wait forever"));
        }
        self.locked.push(place.text);
        self.push_scope();
        let var = self.new_var(Type::Ref(Box::new(item), true), false, span);
        let result = self.declare(name, var).and_then(|()| self.check_block(body));
        self.pop_scope();
        self.locked.pop();
        result
    }

    /// `atomic(x)`: an integer or a `bool`, changed one operation at a time.
    pub(super) fn check_atomic(&mut self, expr: &mut Expr, expected: Option<&Type>) -> Result<Type> {
        let span = expr.span;
        let hint = match expected {
            Some(Type::Atomic(item)) => Some(item.as_ref().clone()),
            _ => None,
        };
        let ExprKind::Call { args, .. } = &mut expr.kind else { unreachable!("called on a call") };
        let [value] = args.as_mut_slice() else {
            return Err(Diagnostic::error(span, "`atomic` takes its first value: `atomic(0)`"));
        };
        let item = self.check_expr(value, hint.as_ref())?;
        if item.as_int().is_none() && item != Type::Bool {
            return Err(Diagnostic::error(
                value.span,
                format!("`atomic` holds an integer or a `bool`, not `{item}`"),
            ));
        }
        let args = std::mem::take(args);
        expr.kind = ExprKind::Builtin { op: Builtin::AtomicNew, args };
        Ok(Type::Atomic(Box::new(item)))
    }

    /// The operations of an `atomic[T]`, through any borrow of it: each is one step that
    /// no other thread can split.
    pub(super) fn check_atomic_method(
        &mut self,
        receiver: Expr,
        item: &Type,
        name: &str,
        args: &mut [Expr],
        span: Span,
    ) -> Result<(ExprKind, Type)> {
        if self.place_of(&receiver).is_none() {
            return Err(Diagnostic::error(
                receiver.span,
                "an atomic is used where it lives: store it in a variable first",
            ));
        }
        let integer = item.as_int().is_some();
        let (op, params, ret) = match name {
            "load" => (Builtin::AtomicLoad, 0, item.clone()),
            "store" => (Builtin::AtomicStore, 1, Type::Void),
            "add" if integer => (Builtin::AtomicAdd, 1, item.clone()),
            "sub" if integer => (Builtin::AtomicSub, 1, item.clone()),
            "exchange" => (Builtin::AtomicExchange, 1, item.clone()),
            "compare_exchange" => (Builtin::AtomicCompareExchange, 2, Type::Bool),
            _ => {
                return Err(Diagnostic::error(span, format!("`atomic[{item}]` has no method `{name}`"))
                    .with_hint("atomics have `load`, `store`, `exchange`, `compare_exchange`, and `add`, `sub` for integers"));
            }
        };
        if args.len() != params {
            return Err(Diagnostic::error(
                span,
                format!("`{name}` takes {params} value(s), {} given", args.len()),
            ));
        }
        let mut all = vec![receiver];
        for arg in args.iter_mut() {
            self.check_value(arg, item)?;
            all.push(std::mem::replace(arg, Expr::new(ExprKind::None, span)));
        }
        Ok((ExprKind::Builtin { op, args: all }, ret))
    }

    /// A value given to another thread: no reference, no `shared` value.
    pub(super) fn check_sendable(&self, ty: &Type, span: Span) -> Result<()> {
        if self.tables.types.contains_shared(ty) {
            return Err(Diagnostic::error(
                span,
                format!("`{ty}` holds a `shared` value, which stays in one thread"),
            )
            .with_hint("send the value itself, or a copy of it"));
        }
        if holds_reference(ty) {
            return Err(Diagnostic::error(
                span,
                format!("`{ty}` is a reference, which cannot go to another thread"),
            )
            .with_hint("move the value, or lend it inside a `scope`"));
        }
        Ok(())
    }
}

fn holds_reference(ty: &Type) -> bool {
    match ty {
        Type::Ref(..) => true,
        Type::Tuple(items) => items.iter().any(holds_reference),
        Type::Optional(item) | Type::Array(item, _) | Type::Vector(item) | Type::Heap(item) => {
            holds_reference(item)
        }
        _ => false,
    }
}

/// An open `with scope()` block: the variables its threads borrow, until its end.
pub(super) struct Lending {
    /// How many variable scopes were open around the block: only their variables can be lent.
    outside: usize,
    /// Variable id → (lent with `&mut`, where).
    lent: HashMap<usize, (bool, Span)>,
}

impl Checker<'_> {
    /// `with scope() as s:` — the block's threads may borrow what lives outside the block;
    /// they are all waited for at its end.
    pub(super) fn check_with(
        &mut self,
        value: &mut Expr,
        name: &str,
        body: &mut [Stmt],
        span: Span,
    ) -> Result<()> {
        let ty = self.check_expr(value, None)?;
        if let Type::Mutex(item) = ty.deref_ref() {
            return self.check_locked(value, item.as_ref().clone(), name, body, span);
        }
        if ty != Type::Scope {
            return Err(Diagnostic::error(
                value.span,
                format!("`with` takes a `scope()` or a `mutex`, found `{ty}`"),
            )
            .with_hint("from thread import scope; with scope() as s:"));
        }
        self.lending.push(Lending { outside: self.scopes.len(), lent: HashMap::new() });
        self.push_scope();
        let var = self.new_var(Type::Scope, false, span);
        var.used.set(true);
        let result = self.declare(name, var).and_then(|()| self.check_block(body));
        self.pop_scope();
        self.lending.pop();
        result
    }

    /// How a variable is lent to a thread of an open scope: `Some(true)` with `&mut`.
    pub(super) fn lent(&self, var: usize) -> Option<bool> {
        self.lending.iter().find_map(|l| l.lent.get(&var)).map(|(mutable, _)| *mutable)
    }

    pub(super) fn lent_span(&self, var: usize) -> Span {
        self.lending.iter().find_map(|l| l.lent.get(&var)).map(|(_, span)| *span).expect("lent")
    }

    /// `s.spawn(f, args...)`: like `spawn`, but `f` may be a lambda that captures, and the
    /// arguments may be references, to variables declared outside the `with` block.
    pub(super) fn check_scoped_spawn(
        &mut self,
        receiver: Expr,
        name: &str,
        args: &mut [Expr],
        span: Span,
    ) -> Result<(ExprKind, Type)> {
        if name != "spawn" {
            return Err(Diagnostic::error(span, format!("a scope has `spawn`, not `{name}`")));
        }
        if self.lending.is_empty() {
            return Err(Diagnostic::error(span, "a scope starts threads only inside its `with` block"));
        }
        let Some((function, rest)) = args.split_first_mut() else {
            return Err(Diagnostic::error(span, "`spawn` takes a function, then its arguments"));
        };
        let mut lends = Vec::new();
        let found = if matches!(function.kind, ExprKind::Lambda { .. }) {
            let found = self.check_lambda(function, true)?;
            function.ty = Some(found.clone());
            let ExprKind::Lambda { captures, .. } = &function.kind else { unreachable!("a lambda") };
            for capture in captures {
                let ty = capture.ty.clone().expect("typed capture");
                self.check_sendable(&ty, capture.span)?;
                let id = self.find_local(&capture.name).map(|v| v.id).expect("captured local");
                lends.push((id, capture.name.clone(), capture.mutable, capture.span));
            }
            found
        } else {
            self.check_expr(function, None)?
        };
        let (params, ret) = match found.deref_ref() {
            Type::Fn(params, ret) => (params.clone(), ret.as_ref().clone()),
            other => {
                return Err(Diagnostic::error(
                    function.span,
                    format!("`spawn` runs a function, found `{other}`"),
                ));
            }
        };
        if params.len() != rest.len() {
            return Err(Diagnostic::error(
                span,
                format!("the function takes {} value(s), {} given", params.len(), rest.len()),
            ));
        }
        for (arg, param) in rest.iter_mut().zip(&params) {
            if let Type::Ref(target, mutable) = param {
                self.check_sendable(target, arg.span)?;
                self.check_arg(arg, param)?;
                if let ExprKind::AddrOf { inner, .. } = &arg.kind
                    && let Some(place) = self.place_of(inner)
                {
                    lends.push((place.var, place.text, *mutable, arg.span));
                }
            } else {
                self.check_sendable(param, arg.span)?;
                self.check_value(arg, param)?;
            }
        }
        self.check_sendable(&ret, span)?;
        for (id, shown, mutable, at) in lends {
            self.lend(id, &shown, mutable, at)?;
        }
        let mut all = vec![receiver];
        all.extend(args.iter_mut().map(|a| std::mem::replace(a, Expr::new(ExprKind::None, span))));
        Ok((ExprKind::Builtin { op: Builtin::ScopedSpawn, args: all }, Type::Thread(Box::new(ret))))
    }

    /// Lends a variable to a thread of the innermost scope, until the end of its block.
    fn lend(&mut self, id: usize, name: &str, mutable: bool, span: Span) -> Result<()> {
        let outside = self.lending.last().expect("inside a scope").outside;
        let declared_inside = self.scopes[outside..].iter().any(|scope| scope.values().any(|v| v.id == id));
        if declared_inside {
            return Err(Diagnostic::error(
                span,
                format!("`{name}` is declared inside the `with` block, and goes away before its threads end"),
            )
            .with_hint("declare it before `with scope() as s:`"));
        }
        if let Some(earlier) = self.lent(id)
            && (earlier || mutable)
        {
            return Err(Diagnostic::error(span, format!("`{name}` is already lent to another thread"))
                .with_hint("a value lent with `&mut` goes to one thread only; `&` may go to several"));
        }
        self.lending.last_mut().expect("inside a scope").lent.insert(id, (mutable, span));
        Ok(())
    }
}
