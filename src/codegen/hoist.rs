//! A fallible call inside a larger expression may leave it early, by `goto` or `return`.
//! What the expression computed before it would then be lost: an f-string being built, the
//! result of an earlier call. So, before such a statement, every call and owning value
//! computed ahead of its last fallible call is moved into a variable of the block, in the
//! order the expression evaluates them; leaving early releases those variables like any other.

use super::Emitter;
use crate::ast::{BinOp, Expr, ExprKind, Stmt, StmtKind};
use crate::types::Type;

impl Emitter<'_> {
    /// The statement with its early values taken out, once they are declared; `None` when it
    /// needs nothing of the kind.
    pub(super) fn hoist(&mut self, stmt: &Stmt) -> Option<Stmt> {
        let mut stmt = stmt.clone();
        let root = match &mut stmt.kind {
            StmtKind::Expr(e) | StmtKind::Return(Some(e)) | StmtKind::Raise(e) => e,
            StmtKind::Var { value, .. } | StmtKind::Assign { value, .. } => value,
            StmtKind::If { branches, .. } => &mut branches.first_mut()?.0,
            StmtKind::Match { subject, .. } => subject,
            StmtKind::For { iter, .. } => iter,
            _ => return None,
        };
        self.hoist_expr(root).then_some(stmt)
    }

    /// Declares the early values of `root` in the current block, and takes them out of it.
    /// Returns whether there were any.
    pub(super) fn hoist_expr(&mut self, root: &mut Expr) -> bool {
        let total = checks(root);
        if total == 0 || !ordered(root, &mut 0, total) {
            return false;
        }
        let mut walk = Walk { total, seen: 0, next: self.temps, hoisted: Vec::new() };
        walk.children(root);
        self.temps = walk.next;
        let any = !walk.hoisted.is_empty();
        for (name, value) in walk.hoisted {
            let ty = value.ty().clone();
            let code = self.emit_expr(&value);
            let (c_type, name) = (self.types.c_type(&ty), super::expr::c_ident(&name));
            self.line(&format!("{c_type} {name} = {code};"));
            if self.types.table.needs_drop(&ty) {
                self.own(name, ty);
            }
        }
        any
    }

    /// `while cond:` whose condition needs early values: they are declared at the start of
    /// each turn, and released with it, whichever way it ends.
    pub(super) fn emit_hoisted_while(&mut self, cond: &Expr, body: &[Stmt]) -> bool {
        let mut cond = cond.clone();
        let total = checks(&cond);
        if total == 0 || !ordered(&cond, &mut 0, total) || !needs_hoisting(&cond, total) {
            return false;
        }
        self.line("while (1) {");
        self.indent += 1;
        self.open_scope(true);
        self.hoist_expr(&mut cond);
        let test = self.emit_condition(&cond);
        self.line(&format!("if (!({test})) {{"));
        self.drop_for_jump(true);
        self.line("    break;");
        self.line("}");
        self.indent -= 1;
        self.emit_block(body, false);
        self.indent += 1;
        self.close_scope(super::stmt::ends_with_jump(body));
        self.indent -= 1;
        self.line("}");
        true
    }

    /// `a and b` (or `or`) whose right side needs early values: `b` is computed in a block of
    /// its own, run only when it has to be.
    pub(super) fn emit_hoisted_logic(&mut self, op: BinOp, left: &Expr, right: &Expr) -> Option<String> {
        let total = checks(right);
        if total == 0 || !ordered(right, &mut 0, total) || !needs_hoisting(right, total) {
            return None;
        }
        let mut right = right.clone();
        let (out, indent) = (std::mem::take(&mut self.out), self.indent);
        self.indent = 0;
        let result = self.temp("logic");
        let left = self.emit_expr(left);
        self.line(&format!("bool {result} = {left};"));
        let test = if op == BinOp::And { result.clone() } else { format!("!{result}") };
        self.line(&format!("if ({test}) {{"));
        self.indent += 1;
        self.open_scope(false);
        self.hoist_expr(&mut right);
        let value = self.emit_expr(&right);
        self.line(&format!("{result} = {value};"));
        self.close_scope(false);
        self.indent -= 1;
        self.line("}");
        let body = std::mem::replace(&mut self.out, out);
        self.indent = indent;
        // Kept on separate lines: the body may carry `#line` directives.
        Some(format!("({{\n{body}{result}; }})"))
    }
}

/// Whether taking out early values would take out anything: a value computed before a
/// fallible call, or a fallible call that is not the whole expression.
fn needs_hoisting(expr: &Expr, total: usize) -> bool {
    let mut walk = Walk { total, seen: 0, next: 0, hoisted: Vec::new() };
    walk.children(&mut expr.clone());
    !walk.hoisted.is_empty()
}

/// The points where an expression may fail: its fallible calls, nested ones included, and
/// each `and` / `or` whose right side may fail, counted once.
fn checks(expr: &Expr) -> usize {
    usize::from(fails_here(expr)) + evaluated(expr).into_iter().map(checks).sum::<usize>()
}

/// A fallible call, or an `and` / `or` whose right side holds one.
fn fails_here(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Check { .. } => true,
        ExprKind::Binary(BinOp::And | BinOp::Or, _, right) => checks(right) > 0,
        _ => false,
    }
}

/// Whether the order of evaluation can be kept: no part evaluated only sometimes (the right
/// side of `and` / `or`) may come before the last fallible call.
fn ordered(expr: &Expr, seen: &mut usize, total: usize) -> bool {
    if let ExprKind::Binary(BinOp::And | BinOp::Or, left, right) = &expr.kind {
        let fine = ordered(left, seen, total);
        if fails_here(expr) {
            *seen += 1;
            return fine;
        }
        return fine && (*seen == total || !calls(right));
    }
    let fine = evaluated(expr).into_iter().all(|e| ordered(e, seen, total));
    *seen += usize::from(fails_here(expr));
    fine
}

/// Whether an expression calls anything, where it may or may not be evaluated included.
fn calls(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { .. }
        | ExprKind::CallValue { .. }
        | ExprKind::Check { .. }
        | ExprKind::Builtin { .. } => true,
        ExprKind::Binary(_, left, right) => calls(left) || calls(right),
        _ => evaluated(expr).into_iter().any(calls),
    }
}

/// The parts of an expression evaluated every time it is, in order. A lambda's body and a
/// comprehension's loop are not among them.
fn evaluated(expr: &Expr) -> Vec<&Expr> {
    match &expr.kind {
        ExprKind::Unary(_, e)
        | ExprKind::Convert(e)
        | ExprKind::Field { base: e, .. }
        | ExprKind::Named { value: e, .. }
        | ExprKind::AddrOf { inner: e, .. }
        | ExprKind::Deref(e)
        | ExprKind::CopyOf(e)
        | ExprKind::Cast(e, _)
        | ExprKind::Wrap(e)
        | ExprKind::Unwrap(e)
        | ExprKind::Move { value: e, .. } => vec![e],
        ExprKind::Binary(BinOp::And | BinOp::Or, left, _) => vec![left],
        ExprKind::Binary(_, a, b) | ExprKind::Index { base: a, index: b } => vec![a, b],
        ExprKind::Range { start, end, step } => [start, end].into_iter().chain(step).map(|e| &**e).collect(),
        ExprKind::Call { args, .. }
        | ExprKind::Tuple(args)
        | ExprKind::ArrayLit(args)
        | ExprKind::Construct(args)
        | ExprKind::Variant { args, .. }
        | ExprKind::Format(args)
        | ExprKind::Builtin { args, .. } => args.iter().collect(),
        ExprKind::CallValue { callee, args } => std::iter::once(&**callee).chain(args).collect(),
        // The call itself is the check: only its arguments come first.
        ExprKind::Check { call, .. } => evaluated(call),
        _ => Vec::new(),
    }
}

/// The same parts, to replace them.
fn evaluated_mut(expr: &mut Expr) -> Vec<&mut Expr> {
    match &mut expr.kind {
        ExprKind::Unary(_, e)
        | ExprKind::Convert(e)
        | ExprKind::Field { base: e, .. }
        | ExprKind::Named { value: e, .. }
        | ExprKind::AddrOf { inner: e, .. }
        | ExprKind::Deref(e)
        | ExprKind::CopyOf(e)
        | ExprKind::Cast(e, _)
        | ExprKind::Wrap(e)
        | ExprKind::Unwrap(e)
        | ExprKind::Move { value: e, .. } => vec![e],
        ExprKind::Binary(BinOp::And | BinOp::Or, left, _) => vec![left],
        ExprKind::Binary(_, a, b) | ExprKind::Index { base: a, index: b } => vec![a, b],
        ExprKind::Range { start, end, step } => {
            [start, end].into_iter().chain(step.iter_mut()).map(|e| &mut **e).collect()
        }
        ExprKind::Call { args, .. }
        | ExprKind::Tuple(args)
        | ExprKind::ArrayLit(args)
        | ExprKind::Construct(args)
        | ExprKind::Variant { args, .. }
        | ExprKind::Format(args)
        | ExprKind::Builtin { args, .. } => args.iter_mut().collect(),
        ExprKind::CallValue { callee, args } => {
            std::iter::once(&mut **callee).chain(args.iter_mut()).collect()
        }
        ExprKind::Check { call, .. } => evaluated_mut(call),
        _ => Vec::new(),
    }
}

struct Walk {
    total: usize,
    seen: usize,
    /// Numbers the variables, after the emitter's own temporaries.
    next: usize,
    hoisted: Vec<(String, Expr)>,
}

impl Walk {
    fn children(&mut self, expr: &mut Expr) {
        let borrowed = matches!(expr.kind, ExprKind::AddrOf { .. });
        for child in evaluated_mut(expr) {
            self.visit(child, borrowed);
        }
    }

    /// Takes out `expr` if it is computed before a fallible call that follows it, or is
    /// itself one; a borrowed value stays where it is, in its variable.
    fn visit(&mut self, expr: &mut Expr, borrowed: bool) {
        self.children(expr);
        let check = fails_here(expr);
        self.seen += usize::from(check);
        let ahead = self.seen < self.total;
        if !(check || ahead && produces(expr)) || matches!(expr.ty(), Type::Void | Type::Ref(..)) {
            return;
        }
        self.next += 1;
        let name = format!("hsy_early{}", self.next);
        let ty = expr.ty().clone();
        let mut taken = Expr { kind: ExprKind::Name(name.clone()), span: expr.span, ty: Some(ty.clone()) };
        std::mem::swap(expr, &mut taken);
        if !borrowed && !ty.is_copy() {
            let place = expr.clone();
            expr.kind = ExprKind::Move { value: Box::new(place), root: name.clone() };
        }
        self.hoisted.push((name, taken));
    }
}

/// A value computed by a call, or owning what it builds.
fn produces(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Call { .. }
            | ExprKind::CallValue { .. }
            | ExprKind::Format(_)
            | ExprKind::Builtin { .. }
            | ExprKind::Construct(_)
            | ExprKind::Tuple(_)
            | ExprKind::ArrayLit(_)
            | ExprKind::Variant { .. }
            | ExprKind::CopyOf(_)
    ) || matches!(&expr.kind, ExprKind::Binary(BinOp::Add, _, _) if *expr.ty() == Type::Str)
}
