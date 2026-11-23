//! Functions defined inside another one. They follow the rules of lambdas: without
//! captures, a plain function; with captures, by reference, a borrowed function that can
//! only be called or passed as `&fn(...)` while its block lasts.

use std::mem;

use super::Checker;
use super::resolve::{FnSig, ParamSig};
use super::stmt::always_returns;
use crate::ast::{FnDecl, Level};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::Type;

impl Checker<'_> {
    pub(super) fn check_nested_fn(
        &mut self,
        decl: &mut FnDecl,
        c_name: &mut String,
        span: Span,
    ) -> Result<()> {
        if decl.level == Level::Managed {
            return Err(Diagnostic::error(span, "`managed` functions are not supported yet"));
        }
        if !decl.generics.is_empty() || !decl.raises.is_empty() {
            return Err(Diagnostic::error(span, "a nested function cannot be generic or raise yet"));
        }
        let mut params = Vec::new();
        for p in &decl.params {
            let ty = self.resolve(&p.ty)?;
            super::check_binding_mode(p, &ty)?;
            if p.default.is_some() {
                return Err(Diagnostic::error(p.span, "a nested function takes no default values"));
            }
            params.push(ParamSig { name: p.name.clone(), ty, default: None });
        }
        let ret = match &decl.ret {
            Some(texpr) => self.resolve(texpr)?,
            None => Type::Void,
        };
        self.check_nested_with(decl, c_name, params, ret, span)
    }

    /// A nested function whose parameter and return types are known, such as the worker of
    /// a `parallel for`.
    pub(super) fn check_nested_with(
        &mut self,
        decl: &mut FnDecl,
        c_name: &mut String,
        params: Vec<ParamSig>,
        ret: Type,
        span: Span,
    ) -> Result<()> {
        let function = Type::Fn(params.iter().map(|p| p.ty.clone()).collect(), Box::new(ret.clone()));
        let value_ty = if decl.captures.is_empty() { function } else { Type::Ref(Box::new(function), false) };
        let captured = self.check_captures(&mut decl.captures)?;
        self.lambdas += 1;
        *c_name = format!("hsy_m{}_{}_nested_{}", self.module, decl.name, self.lambdas);

        let saved = (
            mem::replace(&mut self.ret, ret.clone()),
            mem::take(&mut self.raises),
            mem::replace(&mut self.fn_name, decl.name.clone()),
            mem::take(&mut self.tries),
            mem::take(&mut self.loop_depth),
            mem::take(&mut self.narrowed),
        );
        self.lambda_bounds.push(self.scopes.len());
        self.push_scope();
        let result = self.check_nested_body(decl, &captured, &params, &value_ty, span);
        self.pop_scope();
        self.lambda_bounds.pop();
        (self.ret, self.raises, self.fn_name, self.tries, self.loop_depth, self.narrowed) = saved;
        result?;
        if ret != Type::Void && !always_returns(&decl.body) {
            return Err(Diagnostic::error(
                span,
                format!("function `{}` can end without returning a value", decl.name),
            ));
        }
        let sig = FnSig {
            params,
            ret,
            raises: Vec::new(),
            generics: Vec::new(),
            values: Vec::new(),
            variadic: false,
            c_name: c_name.clone(),
            receiver: None,
        };
        self.tables.fns.push(sig);
        let var = self.new_var(value_ty, false, span);
        self.declare(&decl.name, var)
    }

    /// Captures, the function's own name (for recursion) and its parameters are in scope.
    fn check_nested_body(
        &mut self,
        decl: &mut FnDecl,
        captured: &[(String, Type)],
        params: &[ParamSig],
        value_ty: &Type,
        span: Span,
    ) -> Result<()> {
        for (name, ty) in captured {
            let var = self.new_var(ty.clone(), false, span);
            self.declare(name, var)?;
        }
        let itself = self.new_var(value_ty.clone(), false, span);
        itself.used.set(true);
        self.declare(&decl.name, itself)?;
        for param in params {
            let var = self.new_var(param.ty.clone(), false, span);
            self.declare(&param.name, var)?;
        }
        self.check_stmts(&mut decl.body)
    }
}
