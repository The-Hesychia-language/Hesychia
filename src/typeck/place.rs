//! Places: the expressions that name storage, and can be borrowed, assigned or moved from.

use super::Checker;
use crate::ast::{Expr, ExprKind};
use crate::types::Type;

/// The `var` of a module constant's place, which no variable has.
pub(super) const GLOBAL: usize = usize::MAX;

/// The variable a place belongs to, and how it is reached.
pub(super) struct Place {
    pub var: usize,
    /// The place can be written: a `mut` variable, or a target reached through `&mut`.
    pub mutable: bool,
    /// The place is the variable itself, not one of its parts nor a target behind a reference.
    pub whole: bool,
    /// The source text of the place, for messages: `p.pos.x`.
    pub text: String,
    /// Loop nesting of the variable's declaration.
    pub depth: usize,
}

impl Checker<'_> {
    /// The place a checked expression names, if any.
    pub(super) fn place_of(&self, expr: &Expr) -> Option<Place> {
        match &expr.kind {
            // A module constant can be read and lent, never changed nor moved.
            ExprKind::Global { name, .. } => {
                Some(Place { var: GLOBAL, mutable: false, whole: true, text: name.clone(), depth: 0 })
            }
            ExprKind::Name(name) => {
                let var = self.find_local(name)?;
                Some(Place {
                    var: var.id,
                    mutable: var.mutable,
                    whole: true,
                    text: name.clone(),
                    depth: var.depth,
                })
            }
            ExprKind::Deref(inner)
                if matches!(inner.ty, Some(crate::types::Type::Heap(_) | crate::types::Type::Shared(_))) =>
            {
                let base = self.place_of(inner)?;
                // Every owner of a `shared` value may change it.
                let shared = matches!(inner.ty, Some(crate::types::Type::Shared(_)));
                Some(Place { whole: false, mutable: base.mutable || shared, ..base })
            }
            ExprKind::Deref(inner) => {
                let ExprKind::Name(name) = &inner.kind else { return None };
                let var = self.find_local(name)?;
                let mutable = matches!(var.ty, crate::types::Type::Ref(_, true));
                Some(Place { var: var.id, mutable, whole: false, text: name.clone(), depth: var.depth })
            }
            // The value of a narrowed `Optional` stands for the whole variable.
            ExprKind::Unwrap(inner) => self.place_of(inner),
            ExprKind::Field { base, name } => {
                let base = self.place_of(base)?;
                Some(Place { whole: false, text: format!("{}.{name}", base.text), ..base })
            }
            // `c[k]` through `Index`, `c.index(&k)[0]`: a part of `c`, as writable as `c` is.
            ExprKind::Index { base, .. } if let Some(container) = indexed(base) => {
                let base = self.place_of(container)?;
                Some(Place { whole: false, text: format!("{}[...]", base.text), ..base })
            }
            // What a pointer points to can always be written, unchecked.
            ExprKind::Index { base, .. } if matches!(base.ty, Some(crate::types::Type::Ptr(_))) => {
                let base = self.place_of(base)?;
                Some(Place { whole: false, mutable: true, text: format!("{}[...]", base.text), ..base })
            }
            ExprKind::Index { base, .. } => {
                let base = self.place_of(base)?;
                Some(Place { whole: false, text: format!("{}[...]", base.text), ..base })
            }
            _ => None,
        }
    }
}

/// The container of `c.index(&k)`, the call that `c[k]` becomes for a struct with `Index`.
fn indexed(base: &Expr) -> Option<&Expr> {
    let ExprKind::Call { callee, args, .. } = &base.kind else { return None };
    match (callee.as_str(), args.first().map(|a| &a.kind)) {
        ("index", Some(ExprKind::AddrOf { inner, .. })) if matches!(inner.ty, Some(Type::Struct(_))) => {
            Some(inner)
        }
        _ => None,
    }
}
