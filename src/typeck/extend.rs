//! Methods the standard library adds to `str` and `vector` with `extend`: `s.split(",")`
//! calls the function `str__split(&s, ",")` of the module that extends `str`.

use super::Checker;
use super::resolve::Symbol;
use crate::ast::{Expr, ExprKind, Receiver};
use crate::diag::{Diagnostic, Result};
use crate::types::Type;

impl Checker<'_> {
    /// The function behind `x.name(...)` when the standard library adds `name` to `x`'s
    /// type: its index and its name.
    pub(super) fn extension(&self, ty: &Type, name: &str) -> Option<(usize, String)> {
        let key = match ty {
            Type::Ref(inner, _) => return self.extension(inner, name),
            Type::Str => "str",
            Type::Vector(_) => "vector",
            _ => return None,
        };
        let (module, function) = self.tables.extensions.get(&(key.to_string(), name.to_string()))?;
        match self.tables.lookup(*module, function) {
            Some(Symbol::Fn(index)) => Some((index, function.clone())),
            _ => None,
        }
    }

    /// `x.name(args)` as `type__name(x, args)`, `x` given as the function's `self` asks.
    pub(super) fn check_extension_call(
        &mut self,
        expr: &mut Expr,
        function: (usize, String),
    ) -> Result<Type> {
        let (index, callee) = function;
        let span = expr.span;
        let ExprKind::Method { receiver, name, args, .. } = &mut expr.kind else {
            unreachable!("called on a method")
        };
        let receiver = std::mem::replace(receiver.as_mut(), Expr::new(ExprKind::None, span));
        let (name, mut args) = (name.clone(), std::mem::take(args));
        let wanted = match &self.tables.fns[index].params[0].ty {
            Type::Ref(_, false) => Receiver::Shared,
            Type::Ref(_, true) => Receiver::Mut,
            _ => Receiver::Owned,
        };
        let self_arg = match (receiver.ty(), wanted) {
            // Already a reference: passed on as it is.
            (Type::Ref(_, mutable), Receiver::Shared | Receiver::Mut) => {
                if wanted == Receiver::Mut && !mutable {
                    return Err(Diagnostic::error(
                        receiver.span,
                        format!("`{name}` changes the value behind a `&` reference"),
                    )
                    .with_hint("take it as `&mut`"));
                }
                receiver
            }
            (Type::Ref(..), _) => {
                return Err(Diagnostic::error(
                    receiver.span,
                    format!("`{name}` takes its value, which a reference cannot give"),
                ));
            }
            _ => self.self_argument(receiver, wanted, &name)?,
        };
        args.insert(0, self_arg);
        expr.kind = ExprKind::Call { callee, type_args: Vec::new(), args, target: None };
        self.check_fn_call(expr, index)
    }
}
