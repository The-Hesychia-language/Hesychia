//! Lambdas: anonymous functions with one expression as body. Without captures, a lambda
//! becomes a plain C function and its value a function pointer. A lambda that captures,
//! by reference only, is a borrowed function: it is passed directly to a `&fn(...)`
//! parameter and lives no longer than the call.

use std::mem;

use super::Checker;
use crate::ast::{Expr, ExprKind};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::Type;

/// The parts of the checker's state that belong to the enclosing function.
struct Saved {
    ret: Type,
    raises: Vec<Type>,
    fn_name: String,
    tries: Vec<Vec<Type>>,
    loop_depth: usize,
}

impl Checker<'_> {
    /// `borrowed`: the lambda is an argument for a `&fn(...)` parameter, where it may capture.
    pub(super) fn check_lambda(&mut self, expr: &mut Expr, borrowed: bool) -> Result<Type> {
        let ExprKind::Lambda { captures, params, ret, body, c_name } = &mut expr.kind else {
            unreachable!("called on a lambda")
        };
        let mut types = Vec::new();
        for p in params.iter() {
            let ty = self.resolve(&p.ty)?;
            super::check_binding_mode(p, &ty)?;
            if p.default.is_some() {
                return Err(Diagnostic::error(p.span, "lambda parameters take no default value"));
            }
            types.push(ty);
        }
        let ret = match ret {
            Some(texpr) => self.resolve(texpr)?,
            None => Type::Void,
        };
        if matches!(ret, Type::Ref(..)) {
            return Err(Diagnostic::error(expr.span, "a lambda cannot return a reference"));
        }
        if !captures.is_empty() && !borrowed {
            return Err(Diagnostic::error(
                expr.span,
                "a lambda that captures can only be passed directly as an argument",
            )
            .with_hint("the parameter must be a borrowed function: `f: &fn(...)`"));
        }
        let captured = self.check_captures(captures)?;
        let saved = Saved {
            ret: mem::replace(&mut self.ret, ret.clone()),
            raises: mem::take(&mut self.raises),
            fn_name: mem::replace(&mut self.fn_name, "lambda".into()),
            tries: mem::take(&mut self.tries),
            loop_depth: mem::take(&mut self.loop_depth),
        };
        self.lambda_bounds.push(self.scopes.len());
        self.push_scope();
        let names = captured.into_iter().chain(params.iter().map(|p| p.name.clone()).zip(types.clone()));
        let result = self.check_lambda_body(names, body, &ret, expr.span);
        self.pop_scope();
        self.lambda_bounds.pop();
        self.ret = saved.ret;
        self.raises = saved.raises;
        self.fn_name = saved.fn_name;
        self.tries = saved.tries;
        self.loop_depth = saved.loop_depth;
        result?;
        self.lambdas += 1;
        *c_name = format!("hsy_m{}_lambda_{}", self.module, self.lambdas);
        let ty = Type::Fn(types, Box::new(ret));
        Ok(if borrowed { Type::Ref(Box::new(ty), false) } else { ty })
    }

    /// Each capture names a variable of the enclosing function, seen inside as a reference.
    pub(super) fn check_captures(
        &mut self,
        captures: &mut [crate::ast::Capture],
    ) -> Result<Vec<(String, Type)>> {
        let mut seen = Vec::new();
        for capture in captures.iter_mut() {
            let name = capture.name.clone();
            if seen.iter().any(|(n, _): &(String, Type)| *n == name) {
                return Err(Diagnostic::error(capture.span, format!("`{name}` is captured twice")));
            }
            self.check_capture(&name, capture.span)?;
            let var = self
                .local(&name)
                .ok_or_else(|| Diagnostic::error(capture.span, format!("unknown variable `{name}`")))?;
            let (id, ty, var_mutable) = (var.id, var.ty.clone(), var.mutable);
            self.check_not_moved(id, &name, capture.span)?;
            let (target, writable) = match ty {
                Type::Ref(inner, m) => {
                    capture.through_ref = true;
                    (*inner, m)
                }
                other => (other, var_mutable),
            };
            if capture.mutable && !writable {
                return Err(Diagnostic::error(capture.span, format!("cannot capture `{name}` with `&mut`"))
                    .with_hint("its variable must be `mut`, or reached through `&mut`"));
            }
            if capture.mutable {
                self.forget_narrowing(id);
            }
            capture.ty = Some(target.clone());
            seen.push((name, Type::Ref(Box::new(target), capture.mutable)));
        }
        Ok(seen)
    }

    fn check_lambda_body(
        &mut self,
        params: impl Iterator<Item = (String, Type)>,
        body: &mut Expr,
        ret: &Type,
        span: Span,
    ) -> Result<()> {
        for (name, ty) in params {
            let var = self.new_var(ty, false, span);
            self.declare(&name, var)?;
        }
        if *ret == Type::Void {
            return self.check_expr(body, None).map(|_| ());
        }
        self.returning = true;
        let result = self.check_value(body, ret);
        self.returning = false;
        result
    }

    /// Inside a lambda, a variable of the enclosing function cannot be used yet.
    pub(super) fn check_capture(&self, name: &str, span: Span) -> Result<()> {
        let Some(&bound) = self.lambda_bounds.last() else { return Ok(()) };
        let Some(depth) = self.scopes.iter().rposition(|scope| scope.contains_key(name)) else {
            return Ok(());
        };
        let constant = self.scopes[depth][name].constant.is_some();
        if depth < bound && !constant {
            return Err(Diagnostic::error(
                span,
                format!("this lambda uses `{name}`, a variable from outside"),
            )
            .with_hint(format!("capture it: `[&{name}]`, or pass it as a parameter")));
        }
        Ok(())
    }
}
