//! `parallel for` and `select`.
//!
//! A `parallel for` becomes a nested function, its worker, that borrows with `&` every
//! variable from outside it names and takes one item; the runtime calls it on every core.
//! Reading outside is free; writing goes through the item, or an `atomic`.

use std::collections::BTreeSet;

use super::Checker;
use super::resolve::{ParamSig, Symbol};
use super::stmt::always_returns;
use crate::ast::{
    Builtin, Capture, Expr, ExprKind, FnDecl, Level, Param, SelectCase, Stmt, StmtKind, TypeExpr,
};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{IntTy, Type};

impl Checker<'_> {
    pub(super) fn check_parallel_for(
        &mut self,
        var: &str,
        iter: &mut Expr,
        body: &mut Vec<Stmt>,
        worker: &mut Option<Box<FnDecl>>,
        c_name: &mut String,
        span: Span,
    ) -> Result<()> {
        leaves(body, false)?;
        let range = matches!(&iter.kind, ExprKind::Call { callee, .. } if callee == "range");
        // `managed` lends the vector, with `&mut` when the body changes its item.
        if self.managed && !range && !matches!(iter.kind, ExprKind::AddrOf { .. }) {
            let inner = std::mem::replace(iter, Expr::new(ExprKind::None, iter.span));
            let mutable = assigns(body, var);
            *iter = Expr::new(ExprKind::AddrOf { mutable, inner: Box::new(inner) }, iter.span);
        }
        if !range && !matches!(iter.kind, ExprKind::AddrOf { .. }) {
            return Err(Diagnostic::error(
                iter.span,
                "a `parallel for` goes over `range(...)`, `&v` or `&mut v`",
            ));
        }
        let (element, iterated) = self.check_iterable(iter, span)?;
        if !range && iterated.is_none() {
            return Err(Diagnostic::error(iter.span, "a `parallel for` goes over a vector in a variable"));
        }
        let mut names = BTreeSet::new();
        stmts_names(body, &mut names);
        names.remove(var);
        let mut declared = BTreeSet::new();
        stmts_declared(body, &mut declared);
        let lent = match &iter.kind {
            ExprKind::AddrOf { mutable: true, inner } => root_name(inner),
            _ => None,
        };
        let mut captures = Vec::new();
        for name in names.difference(&declared).cloned() {
            let Some(found) = self.local(&name) else { continue };
            if found.constant.is_some() {
                continue;
            }
            if lent.as_deref() == Some(name.as_str()) {
                return Err(Diagnostic::error(span, format!("`{name}` is lent item by item to this loop"))
                    .with_hint(format!("change each item through `{var}`")));
            }
            if self.tables.types.contains_shared(found.ty.deref_ref()) {
                return Err(Diagnostic::error(
                    span,
                    format!("`{name}` holds a `shared` value, which stays on one thread"),
                ));
            }
            captures.push(Capture { name, mutable: false, through_ref: false, ty: None, span });
        }
        // Only the name of the item is read from the declaration; its type comes with `params`.
        let item = Param {
            owned: false,
            name: var.to_string(),
            ty: TypeExpr::Named { name: "_".into(), args: Vec::new(), span },
            default: None,
            span,
        };
        let mut decl = FnDecl {
            level: Level::Manual,
            private: false,
            name: format!("hsy_parallel_{}", self.lambdas + 1),
            generics: Vec::new(),
            captures,
            receiver: None,
            params: vec![item],
            ret: None,
            raises: Vec::new(),
            body: std::mem::take(body),
            decorators: Vec::new(),
            span,
        };
        let params = vec![ParamSig { name: var.to_string(), ty: element, default: None }];
        self.parallel_reads.push(decl.captures.iter().map(|c| c.name.clone()).collect());
        let result = self.check_nested_with(&mut decl, c_name, params, Type::Void, span);
        self.parallel_reads.pop();
        result?;
        if let Some(found) = self.local(&decl.name) {
            found.used.set(true);
        }
        *worker = Some(Box::new(decl));
        Ok(())
    }

    /// Writing to `place`, refused when it starts from a variable a `parallel for` only reads.
    pub(super) fn outside_write(&self, place: &str, span: Span) -> Option<Diagnostic> {
        let root = place.split(['.', '[']).next()?;
        self.parallel_reads.last()?.iter().any(|n| n == root).then(|| {
            Diagnostic::error(span, format!("a `parallel for` only reads `{root}`, which is outside it"))
                .with_hint("change the item, or count through an `atomic`")
        })
    }

    /// Whether the code being checked is the body of a `parallel for`.
    pub(super) fn in_parallel(&self) -> bool {
        self.fn_name.starts_with("hsy_parallel_")
    }

    /// Each case waits for a message on a receiver, or for a time; its body runs with the
    /// message, an `Optional` that is `None` once every sender is gone.
    pub(super) fn check_select(
        &mut self,
        cases: &mut [SelectCase],
        otherwise: Option<&mut [Stmt]>,
    ) -> Result<()> {
        let mut sleeps = 0;
        let mut found = Vec::new();
        for case in cases.iter_mut() {
            let message = if self.is_sleep(&case.wait) {
                sleeps += 1;
                if case.name.is_some() {
                    return Err(Diagnostic::error(case.span, "waiting a time gives no message"));
                }
                self.check_sleep(&mut case.wait)?;
                None
            } else {
                let ty = self.check_expr(&mut case.wait, None)?;
                if !matches!(case.wait.kind, ExprKind::Builtin { op: Builtin::ChanReceive, .. }) {
                    return Err(Diagnostic::error(
                        case.wait.span,
                        "a `case` of `select` waits on `rx.receive()` or `time.sleep(...)`",
                    ));
                }
                Some(ty)
            };
            found.push(message);
        }
        if sleeps > 1 {
            return Err(Diagnostic::error(cases[0].span, "a `select` has one time limit at most"));
        }
        if sleeps == found.len() {
            return Err(Diagnostic::error(cases[0].span, "a `select` waits on at least one receiver"));
        }
        if sleeps == 1 && otherwise.is_some() {
            return Err(Diagnostic::error(cases[0].span, "a `select` with `else` does not wait")
                .with_hint("keep the time limit or the `else`, not both"));
        }
        // As the branches of an `if`: each starts from the same moves.
        let start = self.moved.clone();
        let mut merged = start.clone();
        for (case, message) in cases.iter_mut().zip(found) {
            self.moved.clone_from(&start);
            self.push_scope();
            let declared = match case.name.clone().zip(message) {
                Some((name, ty)) => {
                    let var = self.new_var(ty, self.managed, case.span);
                    self.declare(&name, var)
                }
                None => Ok(()),
            };
            let result = declared.and_then(|()| self.check_block(&mut case.body));
            self.pop_scope();
            result?;
            if !always_returns(&case.body) {
                merged.extend(self.moved.drain());
            }
        }
        if let Some(body) = otherwise {
            self.moved.clone_from(&start);
            self.check_block(body)?;
            if !always_returns(body) {
                merged.extend(self.moved.drain());
            }
        }
        self.moved = merged;
        Ok(())
    }

    /// Whether `expr` is a call to `time.sleep`.
    fn is_sleep(&self, expr: &Expr) -> bool {
        let symbol = match &expr.kind {
            ExprKind::Method { receiver, name, .. } if name == "sleep" => match self.module_alias(receiver) {
                Some(module) => self.tables.export(module, name, expr.span).ok(),
                None => None,
            },
            ExprKind::Call { callee, .. } if callee == "sleep" && self.find_local(callee).is_none() => {
                self.global(callee)
            }
            _ => None,
        };
        let Some(Symbol::Fn(index)) = symbol else { return false };
        let Some(time) = self.tables.module_paths.iter().position(|p| p == "time") else { return false };
        matches!(self.tables.lookup(time, "sleep"), Some(Symbol::Fn(i)) if i == index)
    }

    /// `time.sleep(duration, unit=...)` as a time limit: the duration and the unit, checked
    /// as `sleep` checks them.
    fn check_sleep(&mut self, wait: &mut Expr) -> Result<()> {
        let span = wait.span;
        let args = match &mut wait.kind {
            ExprKind::Method { args, .. } | ExprKind::Call { args, .. } => std::mem::take(args),
            _ => unreachable!("checked to be a call"),
        };
        let mut duration = None;
        let mut unit = None;
        for (position, arg) in args.into_iter().enumerate() {
            match arg.kind {
                ExprKind::Named { name, value } if name == "unit" => unit = Some(*value),
                ExprKind::Named { name, value } if name == "duration" => duration = Some(*value),
                ExprKind::Named { name, .. } => {
                    return Err(Diagnostic::error(
                        arg.span,
                        format!("`sleep` has no parameter named `{name}`"),
                    ));
                }
                _ if position == 0 => duration = Some(arg),
                _ if position == 1 => unit = Some(arg),
                _ => return Err(Diagnostic::error(arg.span, "`sleep` takes a duration and a unit")),
            }
        }
        let mut duration =
            duration.ok_or_else(|| Diagnostic::error(span, "missing argument `duration` for `sleep`"))?;
        // The unit's length in nanoseconds; a second, the default unit of `sleep`.
        let mut unit = match unit {
            Some(unit) => {
                let (receiver, name) = (Box::new(unit), "nanoseconds".to_string());
                Expr::new(ExprKind::Method { receiver, name, type_args: Vec::new(), args: Vec::new() }, span)
            }
            None => Expr::new(ExprKind::Int(1_000_000_000), span),
        };
        self.check_value(&mut duration, &Type::Float(crate::types::FloatTy::F64))?;
        self.check_value(&mut unit, &Type::Int(IntTy::I64))?;
        wait.kind = ExprKind::Builtin { op: Builtin::SelectSleep, args: vec![duration, unit] };
        wait.ty = Some(Type::Void);
        Ok(())
    }
}

/// Refuses what would leave a `parallel for` early; a `continue` ends its item's turn, as a
/// `return` of the worker.
fn leaves(body: &mut [Stmt], in_loop: bool) -> Result<()> {
    for stmt in body {
        let span = stmt.span;
        match &mut stmt.kind {
            StmtKind::Return(_) => {
                return Err(Diagnostic::error(span, "a `parallel for` cannot `return`: its items run apart"));
            }
            StmtKind::Break if !in_loop => {
                return Err(Diagnostic::error(span, "a `parallel for` cannot `break`: its items run apart"));
            }
            StmtKind::Continue if !in_loop => stmt.kind = StmtKind::Return(None),
            StmtKind::If { branches, otherwise } => {
                for (_, body) in branches {
                    leaves(body, in_loop)?;
                }
                if let Some(body) = otherwise {
                    leaves(body, in_loop)?;
                }
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } => leaves(body, true)?,
            StmtKind::Block(body) | StmtKind::With { body, .. } => leaves(body, in_loop)?,
            StmtKind::Match { cases, .. } => {
                for case in cases {
                    leaves(&mut case.body, in_loop)?;
                }
            }
            StmtKind::Try { body, handlers, .. } => {
                leaves(body, in_loop)?;
                for handler in handlers {
                    leaves(&mut handler.body, in_loop)?;
                }
            }
            StmtKind::ComptimeIf(cif) => {
                for (_, body) in &mut cif.branches {
                    leaves(body, in_loop)?;
                }
                if let Some(body) = &mut cif.otherwise {
                    leaves(body, in_loop)?;
                }
            }
            StmtKind::Select { cases, otherwise } => {
                for case in cases {
                    leaves(&mut case.body, in_loop)?;
                }
                if let Some(body) = otherwise {
                    leaves(body, in_loop)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether the body assigns to `var` or to a part of it.
fn assigns(body: &[Stmt], var: &str) -> bool {
    let mut targets = BTreeSet::new();
    for stmt in body {
        assigned(stmt, &mut targets);
    }
    targets.contains(var)
}

fn assigned(stmt: &Stmt, out: &mut BTreeSet<String>) {
    let each = |body: &[Stmt], out: &mut BTreeSet<String>| body.iter().for_each(|s| assigned(s, out));
    match &stmt.kind {
        StmtKind::Assign { target, .. } => out.extend(root_name(target)),
        StmtKind::If { branches, otherwise } => {
            branches.iter().for_each(|(_, b)| each(b, out));
            each(otherwise.as_deref().unwrap_or_default(), out);
        }
        StmtKind::While { body, .. }
        | StmtKind::For { body, .. }
        | StmtKind::Block(body)
        | StmtKind::With { body, .. }
        | StmtKind::Try { body, .. } => each(body, out),
        StmtKind::Match { cases, .. } => cases.iter().for_each(|c| each(&c.body, out)),
        _ => {}
    }
}

/// The variable a place starts from: `v` for `v[i].x`.
fn root_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name.clone()),
        ExprKind::Field { base, .. } | ExprKind::Index { base, .. } | ExprKind::Deref(base) => {
            root_name(base)
        }
        _ => None,
    }
}

/// The names the statements declare, at any depth: their own variables, not the outside's.
fn stmts_declared(body: &[Stmt], out: &mut BTreeSet<String>) {
    for stmt in body {
        match &stmt.kind {
            StmtKind::Var { target: crate::ast::VarTarget::Name(name), .. } => {
                out.insert(name.clone());
            }
            StmtKind::Var { target: crate::ast::VarTarget::Tuple(names), .. } => {
                out.extend(names.iter().cloned())
            }
            StmtKind::For { var, body, .. } | StmtKind::ParallelFor { var, body, .. } => {
                out.insert(var.clone());
                stmts_declared(body, out);
            }
            StmtKind::If { branches, otherwise } => {
                branches.iter().for_each(|(_, b)| stmts_declared(b, out));
                stmts_declared(otherwise.as_deref().unwrap_or_default(), out);
            }
            StmtKind::While { body, .. } | StmtKind::Block(body) | StmtKind::With { body, .. } => {
                stmts_declared(body, out);
            }
            StmtKind::Try { body, handlers, .. } => {
                stmts_declared(body, out);
                handlers.iter().for_each(|h| stmts_declared(&h.body, out));
            }
            StmtKind::Match { cases, .. } => cases.iter().for_each(|c| stmts_declared(&c.body, out)),
            StmtKind::Select { cases, otherwise } => {
                for case in cases {
                    out.extend(case.name.clone());
                    stmts_declared(&case.body, out);
                }
                stmts_declared(otherwise.as_deref().unwrap_or_default(), out);
            }
            _ => {}
        }
    }
}

/// Every name the statements use, declared inside or not.
fn stmts_names(body: &[Stmt], out: &mut BTreeSet<String>) {
    for stmt in body {
        stmt_names(stmt, out);
    }
}

fn stmt_names(stmt: &Stmt, out: &mut BTreeSet<String>) {
    match &stmt.kind {
        StmtKind::Var { value, .. } | StmtKind::Expr(value) | StmtKind::Raise(value) => names(value, out),
        StmtKind::Return(value) => value.iter().for_each(|v| names(v, out)),
        StmtKind::Assign { target, value, .. } => {
            names(target, out);
            names(value, out);
        }
        StmtKind::If { branches, otherwise } => {
            for (cond, body) in branches {
                names(cond, out);
                stmts_names(body, out);
            }
            stmts_names(otherwise.as_deref().unwrap_or_default(), out);
        }
        StmtKind::While { cond: e, body }
        | StmtKind::For { iter: e, body, .. }
        | StmtKind::With { value: e, body, .. } => {
            names(e, out);
            stmts_names(body, out);
        }
        StmtKind::ParallelFor { iter, body, .. } => {
            names(iter, out);
            stmts_names(body, out);
        }
        StmtKind::Block(body) => stmts_names(body, out),
        StmtKind::Match { subject, cases, .. } => {
            names(subject, out);
            cases.iter().for_each(|c| stmts_names(&c.body, out));
        }
        StmtKind::Try { body, handlers, .. } => {
            stmts_names(body, out);
            handlers.iter().for_each(|h| stmts_names(&h.body, out));
        }
        StmtKind::Fn { decl, .. } => out.extend(decl.captures.iter().map(|c| c.name.clone())),
        StmtKind::Asm { operands, .. } => {
            operands.iter().filter_map(|o| o.value.as_ref()).for_each(|v| names(v, out))
        }
        StmtKind::ComptimeIf(cif) => {
            cif.branches.iter().for_each(|(_, b)| stmts_names(b, out));
            stmts_names(cif.otherwise.as_deref().unwrap_or_default(), out);
        }
        StmtKind::ComptimeFor { body, .. } => stmts_names(body, out),
        StmtKind::Select { cases, otherwise } => {
            for case in cases {
                names(&case.wait, out);
                stmts_names(&case.body, out);
            }
            stmts_names(otherwise.as_deref().unwrap_or_default(), out);
        }
        StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Pass
        | StmtKind::Comptime(_)
        | StmtKind::ComptimeAssert(_) => {}
    }
}

fn names(expr: &Expr, out: &mut BTreeSet<String>) {
    let all = |items: &[Expr], out: &mut BTreeSet<String>| items.iter().for_each(|e| names(e, out));
    match &expr.kind {
        ExprKind::Name(name) => {
            out.insert(name.clone());
        }
        ExprKind::Call { callee, args, .. } => {
            // A local function value may be called by name.
            out.insert(callee.clone());
            all(args, out);
        }
        ExprKind::Method { receiver, args, .. } => {
            names(receiver, out);
            all(args, out);
        }
        ExprKind::Unary(_, e)
        | ExprKind::Field { base: e, .. }
        | ExprKind::Convert(e)
        | ExprKind::Named { value: e, .. }
        | ExprKind::AddrOf { inner: e, .. }
        | ExprKind::Deref(e)
        | ExprKind::CopyOf(e)
        | ExprKind::Cast(e, _)
        | ExprKind::Wrap(e)
        | ExprKind::Unwrap(e)
        | ExprKind::Check { call: e, .. }
        | ExprKind::Formatted { value: e, .. }
        | ExprKind::Move { value: e, .. } => names(e, out),
        ExprKind::Binary(_, a, b) | ExprKind::Index { base: a, index: b } => {
            names(a, out);
            names(b, out);
        }
        ExprKind::Range { start, end, step } => {
            names(start, out);
            names(end, out);
            step.iter().for_each(|s| names(s, out));
        }
        ExprKind::Slice { base, start, end } => {
            names(base, out);
            start.iter().chain(end).for_each(|e| names(e, out));
        }
        ExprKind::Tuple(items)
        | ExprKind::ArrayLit(items)
        | ExprKind::Construct(items)
        | ExprKind::Format(items)
        | ExprKind::Variant { args: items, .. }
        | ExprKind::Builtin { args: items, .. } => all(items, out),
        ExprKind::Comprehension { value, iter, filter, .. } => {
            names(value, out);
            names(iter, out);
            filter.iter().for_each(|f| names(f, out));
        }
        ExprKind::Collection { keys, values, .. } => {
            all(keys, out);
            values.iter().for_each(|v| all(v, out));
        }
        ExprKind::Lambda { captures, .. } => out.extend(captures.iter().map(|c| c.name.clone())),
        ExprKind::CallValue { callee, args } => {
            names(callee, out);
            all(args, out);
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Str(_)
        | ExprKind::Char(_)
        | ExprKind::Global { .. }
        | ExprKind::None
        | ExprKind::FnRef(_) => {}
    }
}
