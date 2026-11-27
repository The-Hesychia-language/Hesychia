//! Finds the functions reachable from `main`, so that only those are emitted.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Expr, ExprKind, FnDecl, Item, Program, Stmt, StmtKind, fn_c_name, global_c_name, method_c_name,
};

/// C names of every function `main` can call, directly or not.
/// The functions reachable from `main` and from `roots`, which the generated code may call
/// on its own (operator and `Drop` methods).
pub fn reachable(program: &Program, mut roots: Vec<String>) -> HashSet<String> {
    roots.push(fn_c_name(0, "main"));
    reachable_from(program, roots)
}

/// The functions reachable from `roots` alone, `roots` included.
pub fn reachable_from(program: &Program, roots: Vec<String>) -> HashSet<String> {
    let bodies: HashMap<String, &FnDecl> = program
        .modules
        .iter()
        .enumerate()
        .flat_map(|(index, module)| {
            module.items.iter().flat_map(move |item| match item {
                Item::Fn(f) => vec![(fn_c_name(index, &f.name), f)],
                Item::Struct(s) if s.generics.is_empty() => {
                    s.methods.iter().map(|m| (method_c_name(index, &s.name, &m.name), m)).collect()
                }
                Item::Enum(e) if e.generics.is_empty() => {
                    e.methods.iter().map(|m| (method_c_name(index, &e.name, &m.name), m)).collect()
                }
                _ => Vec::new(),
            })
        })
        .collect();
    // A module constant is reached by name; its value may call functions in turn.
    let globals: HashMap<String, &Expr> = program
        .modules
        .iter()
        .enumerate()
        .flat_map(|(index, module)| {
            module.items.iter().filter_map(move |item| match item {
                Item::Global(g) => Some((global_c_name(index, &g.name), &g.value)),
                _ => None,
            })
        })
        .collect();
    let mut seen = HashSet::new();
    let mut pending = roots;
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(f) = bodies.get(&name) {
            calls_in_stmts(&f.body, &mut pending);
        }
        if let Some(value) = globals.get(&name) {
            calls_in(value, &mut pending);
        }
    }
    seen
}

fn calls_in_stmts(stmts: &[Stmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Var { value: e, .. } | StmtKind::Expr(e) | StmtKind::Return(Some(e)) => {
                calls_in(e, out)
            }
            StmtKind::Assign { target, value, .. } => [target, value].iter().for_each(|e| calls_in(e, out)),
            StmtKind::If { branches, otherwise } => {
                for (cond, body) in branches {
                    calls_in(cond, out);
                    calls_in_stmts(body, out);
                }
                calls_in_stmts(otherwise.as_deref().unwrap_or_default(), out);
            }
            StmtKind::While { cond: e, body } | StmtKind::For { iter: e, body, .. } => {
                calls_in(e, out);
                calls_in_stmts(body, out);
            }
            StmtKind::Block(body) => calls_in_stmts(body, out),
            StmtKind::With { value, body, .. } => {
                calls_in(value, out);
                calls_in_stmts(body, out);
            }
            StmtKind::Raise(value) => calls_in(value, out),
            StmtKind::Fn { decl, .. } => calls_in_stmts(&decl.body, out),
            StmtKind::ParallelFor { iter, worker, .. } => {
                calls_in(iter, out);
                calls_in_stmts(&worker.as_ref().expect("checked").body, out);
            }
            StmtKind::Select { cases, otherwise } => {
                for case in cases {
                    calls_in(&case.wait, out);
                    calls_in_stmts(&case.body, out);
                }
                calls_in_stmts(otherwise.as_deref().unwrap_or_default(), out);
            }
            StmtKind::Try { body, handlers, .. } => {
                calls_in_stmts(body, out);
                for h in handlers {
                    calls_in_stmts(&h.body, out);
                }
            }
            StmtKind::Match { subject, cases, .. } => {
                calls_in(subject, out);
                for case in cases {
                    if let Some(guard) = &case.guard {
                        calls_in(guard, out);
                    }
                    calls_in_stmts(&case.body, out);
                }
            }
            _ => {}
        }
    }
}

fn calls_in(expr: &Expr, out: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Global { module, name } => out.push(global_c_name(*module, name)),
        ExprKind::Call { args, target, .. } => {
            out.extend(target.clone());
            for arg in args {
                calls_in(arg, out);
            }
        }
        ExprKind::Unary(_, e)
        | ExprKind::Convert(e)
        | ExprKind::Field { base: e, .. }
        | ExprKind::AddrOf { inner: e, .. }
        | ExprKind::Deref(e)
        | ExprKind::CopyOf(e)
        | ExprKind::Cast(e, _)
        | ExprKind::Wrap(e)
        | ExprKind::Unwrap(e)
        | ExprKind::Move { value: e, .. }
        | ExprKind::Check { call: e, .. }
        | ExprKind::Named { value: e, .. } => calls_in(e, out),
        ExprKind::FnRef(c_name) => out.push(c_name.clone()),
        ExprKind::Lambda { body, .. } => calls_in(body, out),
        ExprKind::CallValue { callee, args } => {
            calls_in(callee, out);
            for arg in args {
                calls_in(arg, out);
            }
        }
        ExprKind::Binary(_, a, b) | ExprKind::Index { base: a, index: b } => {
            calls_in(a, out);
            calls_in(b, out);
        }
        ExprKind::Tuple(items)
        | ExprKind::ArrayLit(items)
        | ExprKind::Construct(items)
        | ExprKind::Variant { args: items, .. }
        | ExprKind::Format(items)
        | ExprKind::Builtin { args: items, .. } => {
            for item in items {
                calls_in(item, out);
            }
        }
        ExprKind::Method { receiver, args, .. } => {
            calls_in(receiver, out);
            for arg in args {
                calls_in(arg, out);
            }
        }
        ExprKind::Range { start, end, step } => {
            for e in [start, end].into_iter().chain(step) {
                calls_in(e, out);
            }
        }
        ExprKind::Comprehension { built: Some((_, fill)), .. }
        | ExprKind::Collection { built: Some((_, fill)), .. } => {
            calls_in_stmts(std::slice::from_ref(fill), out)
        }
        _ => {}
    }
}
