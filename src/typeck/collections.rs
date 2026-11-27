//! `{k: v, ...}` and `{a, b}`: a `dict` or a `set` built in place, as a comprehension is: a
//! hidden collection filled by `result[k] = v` or `result.add(a)` for each item.

use super::Checker;
use super::resolve::Symbol;
use super::templates::Kind;
use crate::ast::{Expr, ExprKind, Stmt, StmtKind};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::Type;

impl Checker<'_> {
    pub(super) fn check_collection(&mut self, expr: &mut Expr, expected: Option<&Type>) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Collection { keys, values, built } = &mut expr.kind else {
            unreachable!("called on a collection literal")
        };
        let name = if values.is_some() { "dict" } else { "set" };
        let Some(Symbol::GenericStruct(template)) = self.tables.lookup(self.module, name) else {
            return Err(Diagnostic::error(span, format!("`{name}` is not known here")));
        };
        let ty = match expected {
            Some(ty) if self.tables.templates.is_instance(Kind::Struct, template, ty) => ty.clone(),
            _ => {
                let (Some(key), first_value) = (keys.first(), values.as_ref().and_then(|v| v.first())) else {
                    return Err(Diagnostic::error(span, "the type of an empty `{}` cannot be inferred")
                        .with_hint("write `dict[K, V]()`, or give the variable a type"));
                };
                let mut args = vec![self.literal_item(key)?];
                if let Some(value) = first_value {
                    args.push(self.literal_item(value)?);
                }
                self.made_struct(template, args, span)?
            }
        };
        self.comprehensions += 1;
        let result = format!("hsy_collection{}", self.comprehensions);
        let fill = fill_statements(&result, std::mem::take(keys), values.take(), span);
        let mut fill = Stmt { kind: StmtKind::Block(fill), span };
        self.push_scope();
        let collection = self.new_var(ty.clone(), true, span);
        collection.used.set(true);
        self.declare(&result, collection)?;
        let checked = self.check_stmt(&mut fill);
        self.pop_scope();
        checked?;
        *built = Some((result, Box::new(fill)));
        Ok(ty)
    }

    /// `template[args]`, made if no code needed it before.
    fn made_struct(&mut self, template: usize, args: Vec<Type>, span: Span) -> Result<Type> {
        loop {
            let result = self.tables.templates.find(Kind::Struct, template, args.clone(), span);
            if !self.tables.make_missing(result.is_err())? {
                return result;
            }
        }
    }

    /// The type of an item, from a copy of it; what this check records is undone.
    fn literal_item(&mut self, item: &Expr) -> Result<Type> {
        let (warnings, lambdas, moved) = (self.warnings.len(), self.lambdas, self.moved.clone());
        let ty = self.check_expr(&mut item.clone(), None);
        self.warnings.truncate(warnings);
        self.lambdas = lambdas;
        self.moved = moved;
        ty
    }
}

/// `result[k] = v` for each entry of a `dict`, `result.add(a)` for each item of a `set`.
fn fill_statements(result: &str, keys: Vec<Expr>, values: Option<Vec<Expr>>, span: Span) -> Vec<Stmt> {
    let target = || Box::new(Expr::new(ExprKind::Name(result.to_string()), span));
    let stmt = |kind| Stmt { kind, span };
    match values {
        Some(values) => keys
            .into_iter()
            .zip(values)
            .map(|(key, value)| {
                let place = Expr::new(ExprKind::Index { base: target(), index: Box::new(key) }, span);
                stmt(StmtKind::Assign { target: place, op: None, value })
            })
            .collect(),
        None => keys
            .into_iter()
            .map(|item| {
                let add = ExprKind::Method {
                    receiver: target(),
                    name: "add".into(),
                    type_args: Vec::new(),
                    args: vec![item],
                };
                stmt(StmtKind::Expr(Expr::new(add, span)))
            })
            .collect(),
    }
}
