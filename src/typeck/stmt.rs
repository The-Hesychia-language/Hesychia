//! Statement checking.

use super::{Checker, expect_type, resolve};
use crate::ast::{BinOp, Builtin, Expr, ExprKind, Stmt, StmtKind, TypeExpr, VarTarget};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::Type;

impl Checker<'_> {
    pub(super) fn check_block(&mut self, stmts: &mut [Stmt]) -> Result<()> {
        let narrowed = self.narrowed.clone();
        self.push_scope();
        let result = self.check_stmts(stmts);
        self.pop_scope();
        self.narrowed = narrowed;
        result
    }

    /// Statements in the scope already open: a function's body shares its parameters' scope.
    pub(super) fn check_stmts(&mut self, stmts: &mut [Stmt]) -> Result<()> {
        for i in 0..stmts.len() {
            let (stmt, rest) = stmts[i..].split_first_mut().expect("in range");
            // A `continue` among them comes back for another turn; a `break` leaves one loop only.
            let (continues, breaks) = jumps(rest);
            let ends = always_returns(rest) || matches!(rest.last().map(|s| &s.kind), Some(StmtKind::Break));
            let leaves = match (ends && !continues, breaks) {
                (false, _) => None,
                (true, true) => Some(self.loop_depth.saturating_sub(1)),
                (true, false) => Some(0),
            };
            let outer = self.leaves_to;
            self.leaves_to = match (outer, leaves) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let result = self.check_stmt(stmt);
            self.leaves_to = outer;
            result?;
            // A change to a value lent to a thread, noticed while checking the statement.
            self.lend_error.take().map_or(Ok(()), Err)?;
        }
        Ok(())
    }

    pub(super) fn check_stmt(&mut self, stmt: &mut Stmt) -> Result<()> {
        if matches!(stmt.kind, StmtKind::ComptimeIf(_)) {
            return self.expand_comptime_if(stmt);
        }
        let span = stmt.span;
        if let StmtKind::Assign { target, op: None, value } = &stmt.kind
            && let Some(call) = self.index_set(target, value)?
        {
            stmt.kind = StmtKind::Expr(call);
            return Ok(());
        }
        // `x = 5` in `managed` declares `x` the first time: it becomes `mut x = 5`.
        if let StmtKind::Assign { target: Expr { kind: ExprKind::Name(name), span: at, .. }, op: None, value } =
            &mut stmt.kind
            && self.managed
            && self.find_local(name).is_none()
            && self.free_name(name)
        {
            let (name, at) = (name.clone(), *at);
            let value = std::mem::replace(value, Expr::new(ExprKind::None, at));
            stmt.kind =
                StmtKind::Var { mutable: true, target: VarTarget::Name(name.clone()), ty: None, value };
            let StmtKind::Var { value, .. } = &mut stmt.kind else { unreachable!("just built") };
            return self.declare_inferred(&name, value, at);
        }
        match &mut stmt.kind {
            StmtKind::Var { mutable, target, ty, value } => {
                self.check_var(*mutable, target, ty.as_ref(), value, span)
            }
            StmtKind::Assign { target, op, value } => self.check_assign(target, *op, value),
            StmtKind::Expr(expr) => self.check_expr(expr, None).map(drop),
            StmtKind::If { branches, otherwise } => self.check_if(branches, otherwise.as_deref_mut()),
            StmtKind::While { cond, body } => {
                self.check_condition(cond)?;
                self.check_loop_body(body)
            }
            StmtKind::For { var, iter, body } => self.check_for(var, iter, body, span),
            StmtKind::Return(value) => self.check_return(value.as_mut(), span),
            StmtKind::Break | StmtKind::Continue => self.check_in_loop(span),
            StmtKind::Pass => Ok(()),
            StmtKind::Comptime(c) => {
                let constant = resolve::evaluate_const(c, |e| self.eval(e))?;
                let mut var = self.new_var(constant.ty, false, c.span);
                var.constant = Some(constant.value);
                self.declare(&c.name, var)
            }
            StmtKind::ComptimeAssert(a) => resolve::check_assert(a, |e| self.eval(e)),
            StmtKind::Block(body) => self.check_block(body),
            StmtKind::Match { subject, cases, borrowed } => self.check_match(subject, cases, borrowed, span),
            StmtKind::Raise(value) => self.check_raise(value, span),
            StmtKind::Fn { decl, c_name } => self.check_nested_fn(decl, c_name, span),
            StmtKind::Try { body, handlers, errors } => self.check_try(body, handlers, errors, span),
            StmtKind::Asm { operands, .. } => self.check_asm(operands, span),
            StmtKind::With { value, name, body } => self.check_with(value, name, body, span),
            StmtKind::ParallelFor { var, iter, body, worker, c_name } => {
                self.check_parallel_for(var, iter, body, worker, c_name, span)
            }
            StmtKind::Select { cases, otherwise } => self.check_select(cases, otherwise.as_deref_mut()),
            StmtKind::ComptimeIf(_) => unreachable!("expanded above"),
        }
    }

    /// Replaces a `comptime if` with its selected branch; the other branches are never checked.
    fn expand_comptime_if(&mut self, stmt: &mut Stmt) -> Result<()> {
        let StmtKind::ComptimeIf(cif) = std::mem::replace(&mut stmt.kind, StmtKind::Pass) else {
            unreachable!("called on a comptime if")
        };
        let mut body = resolve::select_branch(cif, |e| self.eval_mut(e))?;
        self.check_block(&mut body)?;
        stmt.kind = StmtKind::Block(body);
        Ok(())
    }

    /// Each branch starts from the same moves; a move in a branch that falls through
    /// counts afterwards. `x is not None` narrows `x` in its branch, `x is None` in the
    /// following ones, and after the `if` when every branch leaves.
    fn check_if(&mut self, branches: &mut [(Expr, Vec<Stmt>)], otherwise: Option<&mut [Stmt]>) -> Result<()> {
        let (start, outer) = (self.moved.clone(), self.narrowed.clone());
        let mut merged = start.clone();
        let mut carried = Vec::new();
        let mut leaves = true;
        let has_else = otherwise.is_some();
        let mut check_body =
            |this: &mut Self, body: &mut [Stmt], extra: Vec<(usize, String)>| -> Result<()> {
                this.moved.clone_from(&start);
                this.narrowed.extend(extra);
                this.check_block(body)?;
                if !always_returns(body) {
                    merged.extend(this.moved.drain());
                    leaves = false;
                }
                Ok(())
            };
        for (cond, body) in branches.iter_mut() {
            let (proven, refuted) = (self.narrowing(cond, true), self.narrowing(cond, false));
            self.check_condition(cond)?;
            let narrowed_here = self.narrowed.clone();
            check_body(self, body, proven)?;
            self.narrowed = narrowed_here;
            self.narrowed.extend(refuted.iter().cloned());
            carried.extend(refuted);
        }
        if let Some(body) = otherwise {
            check_body(self, body, Vec::new())?;
        }
        self.moved = merged;
        self.narrowed = outer;
        if leaves && !has_else {
            self.narrowed.extend(carried);
        }
        Ok(())
    }

    fn check_var(
        &mut self,
        mutable: bool,
        target: &VarTarget,
        ty: Option<&TypeExpr>,
        value: &mut Expr,
        span: Span,
    ) -> Result<()> {
        let keyword = if mutable { "mut" } else { "const" };
        let ty = match ty {
            // `a, b = pair` in `managed`, and the item of `for a, b in`: the types come
            // from the value.
            None if self.managed
                || matches!(&value.kind, ExprKind::Name(n) if n.starts_with(crate::ast::LOOP_ITEM)) =>
            {
                let found = self.check_expr(value, None)?;
                self.consume(value)?;
                return self.declare_target(target, found, mutable, span);
            }
            ty => ty,
        };
        let ty = ty.ok_or_else(|| {
            let shown = match target {
                VarTarget::Name(name) => name.clone(),
                VarTarget::Tuple(names) => format!("({})", names.join(", ")),
            };
            Diagnostic::error(span, format!("`{shown}` needs an explicit type in a `manual` function"))
                .with_hint(format!("write `{keyword} {shown}: <type> = ...`"))
        })?;
        let ty = self.resolve(ty)?;
        self.check_value(value, &ty)?;
        self.declare_target(target, ty, mutable, span)
    }

    /// Declares a name, or the names a tuple is taken apart into.
    fn declare_target(&mut self, target: &VarTarget, ty: Type, mutable: bool, span: Span) -> Result<()> {
        match target {
            VarTarget::Name(name) => {
                let var = self.new_var(ty, mutable, span);
                self.declare(name, var)
            }
            VarTarget::Tuple(names) => {
                let Type::Tuple(items) = &ty else {
                    return Err(Diagnostic::error(
                        span,
                        format!("cannot take a `{ty}` apart into {} names", names.len()),
                    ));
                };
                if items.len() != names.len() {
                    return Err(Diagnostic::error(
                        span,
                        format!("this tuple has {} items, {} names given", items.len(), names.len()),
                    ));
                }
                for (name, item) in names.iter().zip(items) {
                    let var = self.new_var(item.clone(), mutable, span);
                    self.declare(name, var)?;
                }
                Ok(())
            }
        }
    }

    fn check_assign(&mut self, target: &mut Expr, op: Option<BinOp>, value: &mut Expr) -> Result<()> {
        let inferred = match &target.kind {
            ExprKind::Name(name) => self.find_local(name).and_then(|v| v.inferred),
            _ => None,
        };
        let ty = match &target.kind {
            ExprKind::Name(name) => self.assign_to_variable(target, name.clone(), op.is_some())?,
            _ => self.assign_to_part(target)?,
        };
        let refilled = match (&target.kind, op) {
            (ExprKind::Name(name), None) => self.find_local(name).map(|v| v.id),
            _ => None,
        };
        let outer = std::mem::replace(&mut self.refilled, refilled);
        let result = self.check_assigned(target, op, value, inferred, ty);
        self.refilled = outer;
        // Moved out by its own new value, the variable is whole again.
        if let Some(id) = refilled {
            self.moved.remove(&id);
        }
        result
    }

    fn check_assigned(
        &mut self,
        target: &mut Expr,
        op: Option<BinOp>,
        value: &mut Expr,
        inferred: Option<(u32, u32)>,
        ty: Type,
    ) -> Result<()> {
        match (op, inferred) {
            (Some(op), _) => {
                self.assigning = inferred;
                let result = self.check_compound(op, &ty, value, target.span);
                self.assigning = None;
                let found = result?;
                if self.recheck {
                    return Ok(());
                }
                expect_type(&found, &ty, value.span)
            }
            // A wider value widens an inferred variable instead of being converted.
            (None, Some(key)) => {
                let found = self.check_expr(value, Some(&ty))?;
                if self.widen_variable(key, &ty, &found, value.span)? {
                    return Ok(());
                }
                if !self.coerce(value, &found, &ty) {
                    expect_type(&found, &ty, value.span)?;
                }
                self.consume(value)
            }
            (None, None) => self.check_value(value, &ty),
        }
    }

    /// `x = ...` reinitializes `x`; through a `&mut` reference it writes the target.
    fn assign_to_variable(&mut self, target: &mut Expr, name: String, reads: bool) -> Result<Type> {
        let var = self
            .find_local(&name)
            .ok_or_else(|| Diagnostic::error(target.span, format!("unknown variable `{name}`")))?;
        let (id, ty, mutable, boxed) = (var.id, var.ty.clone(), var.mutable, var.boxed);
        if reads {
            var.used.set(true);
            self.check_not_moved(id, &name, target.span)?;
        }
        let var = self.find_local(&name).expect("found above");
        // A large variable is written where it lives, on the heap.
        if let (Type::Heap(inner), true) = (&ty, boxed) {
            var.used.set(true);
            let mut name_expr = Expr::new(ExprKind::Name(name), target.span);
            name_expr.ty = Some(ty.clone());
            target.kind = ExprKind::Deref(Box::new(name_expr));
            target.ty = Some(inner.as_ref().clone());
            self.forget_narrowing(id);
            return Ok(inner.as_ref().clone());
        }
        if let Type::Ref(inner, through) = &ty {
            if !through && !self.loop_needs_mut(id) {
                if let Some(refused) = self.outside_write(&name, target.span) {
                    return Err(refused);
                }
                return Err(Diagnostic::error(
                    target.span,
                    format!("cannot write through `{name}`, a `&` reference"),
                )
                .with_hint("take the parameter as `&mut`"));
            }
            self.find_local(&name).expect("found above").used.set(true);
            let mut name_expr = Expr::new(ExprKind::Name(name), target.span);
            name_expr.ty = Some(ty.clone());
            target.kind = ExprKind::Deref(Box::new(name_expr));
            target.ty = Some(inner.as_ref().clone());
            return Ok(inner.as_ref().clone());
        }
        if !mutable && !self.loop_needs_mut(id) {
            return Err(Diagnostic::error(target.span, format!("cannot assign twice to `{name}`"))
                .with_hint(format!("declare it with `mut {name}` to make it changeable")));
        }
        self.moved.remove(&id);
        // A new value: the copies of the old one are not meant to follow it.
        self.copied.remove(&id);
        self.forget_narrowing(id);
        target.ty = Some(ty.clone());
        Ok(ty)
    }

    /// `p.x = ...` and `a[i] = ...` need a place reached through mutable storage.
    fn assign_to_part(&mut self, target: &mut Expr) -> Result<Type> {
        let ty = self.check_expr(target, None)?;
        let place = self.place_of(target).ok_or_else(|| {
            Diagnostic::error(target.span, "only variables, fields and items can be assigned")
        })?;
        if !place.mutable && !self.loop_needs_mut(place.var) {
            if let Some(refused) = self.outside_write(&place.text, target.span) {
                return Err(refused);
            }
            return Err(Diagnostic::error(target.span, format!("cannot assign to `{}`", place.text))
                .with_hint("its variable must be `mut`, or reached through `&mut`"));
        }
        self.forget_narrowing(place.var);
        Ok(ty)
    }

    /// Checks a value that is stored or passed on: non-copy places are moved, and a
    /// `T` given where an `Optional[T]` is expected is wrapped.
    pub(super) fn check_value(&mut self, value: &mut Expr, ty: &Type) -> Result<()> {
        let found = self.check_expr(value, Some(ty))?;
        // `managed` places on the heap what a field keeps there.
        let found = if self.managed {
            let found = super::managed::read_heap(value, found, ty);
            self.allocate_for(value, found, ty)?
        } else {
            found
        };
        // A value converted as `managed` converts, then wrapped; or the error names `T`.
        if let Type::Optional(inner) = ty
            && **inner != found
            && !matches!(found, Type::Optional(_))
            && found != Type::Void
        {
            if !self.coerce(value, &found, inner) {
                expect_type(&found, inner, value.span)?;
            }
            return self.check_value_wrapped(value, ty);
        }
        if let Type::Optional(inner) = ty
            && **inner == found
        {
            return self.check_value_wrapped(value, ty);
        }
        if !self.coerce(value, &found, ty) {
            expect_type(&found, ty, value.span)?;
        }
        self.consume(value)
    }

    /// Moves the checked `value` into an `Optional` of type `ty`.
    fn check_value_wrapped(&mut self, value: &mut Expr, ty: &Type) -> Result<()> {
        self.consume(value)?;
        let span = value.span;
        let inner = std::mem::replace(value, Expr::new(ExprKind::None, span));
        let mut wrapped = Expr::new(ExprKind::Wrap(Box::new(inner)), span);
        wrapped.ty = Some(ty.clone());
        *value = wrapped;
        Ok(())
    }

    pub(super) fn check_for(
        &mut self,
        var: &str,
        iter: &mut Expr,
        body: &mut [Stmt],
        span: Span,
    ) -> Result<()> {
        let (element, iterated) = self.check_iterable(iter, span)?;
        self.push_scope();
        let mut var_decl = self.new_var(element, false, span);
        // Each iteration has its own item, which the body may move.
        var_decl.depth += 1;
        if self.managed && iterated.is_some() {
            self.loop_vars.insert(var_decl.id, (span.line, span.col));
        }
        self.declare(var, var_decl)?;
        self.iterating.extend(iterated.map(|id| (id, false)));
        let result = self.check_loop_body(body);
        let changed = iterated.is_some() && self.iterating.pop().is_some_and(|(_, changed)| changed);
        self.pop_scope();
        result?;
        if changed && matches!(iter.ty(), Type::Ref(_, true)) {
            return Err(Diagnostic::error(iter.span, "this loop changes the vector whose items it changes")
                .with_hint("use a `range` loop and `v[i]`"));
        }
        if changed {
            self.iterate_on_copy(iter)?;
        }
        Ok(())
    }

    fn check_loop_body(&mut self, body: &mut [Stmt]) -> Result<()> {
        self.loop_depth += 1;
        // What follows the loop says nothing of its next turn.
        let leaves = self.leaves_to.take();
        let result = self.check_block(body);
        self.leaves_to = leaves;
        self.loop_depth -= 1;
        result
    }

    pub(super) fn check_condition(&mut self, cond: &mut Expr) -> Result<()> {
        let found = self.check_expr(cond, Some(&Type::Bool))?;
        expect_type(&found, &Type::Bool, cond.span)
    }

    fn check_return(&mut self, value: Option<&mut Expr>, span: Span) -> Result<()> {
        let expected = self.ret.clone();
        match value {
            Some(value) if expected != Type::Void => {
                self.returning = true;
                let result = self.check_value(value, &expected);
                self.returning = false;
                result
            }
            Some(value) => Err(Diagnostic::error(value.span, "this function does not return a value")),
            None if expected != Type::Void => {
                Err(Diagnostic::error(span, format!("expected a `{expected}` value to return")))
            }
            None => Ok(()),
        }
    }

    fn check_in_loop(&self, span: Span) -> Result<()> {
        if self.loop_depth == 0 {
            return Err(Diagnostic::error(span, "`break` and `continue` can only be used inside a loop"));
        }
        Ok(())
    }
}

/// Whether every path through `stmts` ends with `return`.
pub(super) fn always_returns(stmts: &[Stmt]) -> bool {
    match stmts.last().map(|stmt| &stmt.kind) {
        Some(StmtKind::Return(_)) => true,
        Some(StmtKind::Block(body) | StmtKind::With { body, .. }) => always_returns(body),
        // One case runs, or the `else`.
        Some(StmtKind::Select { cases, otherwise }) => {
            cases.iter().all(|c| always_returns(&c.body)) && otherwise.as_deref().is_none_or(always_returns)
        }
        Some(StmtKind::Match { cases, .. }) => cases.iter().all(|case| always_returns(&case.body)),
        Some(StmtKind::Raise(_)) => true,
        Some(StmtKind::Expr(Expr { kind: ExprKind::Builtin { op: Builtin::Panic, .. }, .. })) => true,
        // `while True:` without a `break` is only left by `return`.
        Some(StmtKind::While { cond: Expr { kind: ExprKind::Bool(true), .. }, body }) => !jumps(body).1,
        Some(StmtKind::Try { body, handlers, .. }) => {
            always_returns(body) && handlers.iter().all(|h| always_returns(&h.body))
        }
        Some(StmtKind::If { branches, otherwise: Some(otherwise) }) => {
            branches.iter().all(|(_, body)| always_returns(body)) && always_returns(otherwise)
        }
        _ => false,
    }
}

/// Whether the statements hold a `continue`, and a `break`, at any depth.
fn jumps(stmts: &[Stmt]) -> (bool, bool) {
    let mut found = (false, false);
    let mut add = |body: &[Stmt]| {
        let (continues, breaks) = jumps(body);
        found = (found.0 || continues, found.1 || breaks);
    };
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::If { branches, otherwise } => {
                branches.iter().for_each(|(_, body)| add(body));
                add(otherwise.as_deref().unwrap_or_default());
            }
            StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::Block(body)
            | StmtKind::With { body, .. } => add(body),
            StmtKind::Match { cases, .. } => cases.iter().for_each(|c| add(&c.body)),
            StmtKind::Try { body, handlers, .. } => {
                add(body);
                handlers.iter().for_each(|h| add(&h.body));
            }
            StmtKind::Select { cases, otherwise } => {
                cases.iter().for_each(|c| add(&c.body));
                add(otherwise.as_deref().unwrap_or_default());
            }
            StmtKind::ComptimeIf(cif) => {
                cif.branches.iter().for_each(|(_, body)| add(body));
                add(cif.otherwise.as_deref().unwrap_or_default());
            }
            _ => {}
        }
    }
    let direct = |kind: fn(&StmtKind) -> bool| stmts.iter().any(|s| kind(&s.kind));
    (
        found.0 || direct(|k| matches!(k, StmtKind::Continue)),
        found.1 || direct(|k| matches!(k, StmtKind::Break)),
    )
}
