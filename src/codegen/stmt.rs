//! Statement emission.

use super::Emitter;
use super::expr::{c_ident, c_string};
use super::helpers::Helper;
use crate::ast::{BinOp, Case, Expr, ExprKind, Pattern, Stmt, StmtKind, UnOp, VarTarget};
use crate::types::Type;

impl Emitter<'_> {
    pub(super) fn emit_body(&mut self, stmts: &[Stmt]) {
        self.emit_block(stmts, false);
    }

    /// A block whose owned variables are dropped at its end; loop bodies are marked so
    /// that `break` and `continue` know what to drop.
    pub(super) fn emit_block(&mut self, stmts: &[Stmt], is_loop: bool) {
        self.indent += 1;
        self.open_scope(is_loop);
        for stmt in stmts {
            self.emit_stmt(stmt);
        }
        self.close_scope(ends_with_jump(stmts));
        self.indent -= 1;
    }

    pub(super) fn emit_stmt(&mut self, stmt: &Stmt) {
        if self.options.debug {
            let file = c_string(&self.options.file_names[stmt.span.file]);
            self.out += &format!("#line {} {file}\n", stmt.span.line);
        }
        if self.in_test {
            self.line(&format!("hsy_test_line = {};", stmt.span.line));
        }
        if let Some(hoisted) = self.hoist(stmt) {
            return self.emit_stmt_kind(&hoisted);
        }
        self.emit_stmt_kind(stmt);
    }

    fn emit_stmt_kind(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Asm { operands, lines } => self.emit_asm(operands, lines),
            StmtKind::With { value, name, body } if matches!(value.ty().deref_ref(), Type::Mutex(_)) => {
                self.emit_locked(value, name, body);
            }
            StmtKind::With { name, body, .. } => self.emit_with(name, body),
            StmtKind::ParallelFor { iter, worker, c_name, .. } => {
                self.emit_parallel_for(iter, worker.as_ref().expect("checked"), c_name);
            }
            StmtKind::Select { cases, otherwise } => self.emit_select(cases, otherwise.as_deref()),
            StmtKind::Var { target, value, .. } => self.emit_var(target, value),
            StmtKind::Assign { target, op, value } => self.emit_assign(target, *op, value),
            StmtKind::Expr(expr) => {
                let code = self.emit_expr(expr);
                // A statement expression whose value is not used (`check f()` alone): clang
                // warns of the value unless it is cast away.
                let discarded = if code.starts_with("({") && *expr.ty() != Type::Void {
                    format!("(void){code};")
                } else {
                    format!("{code};")
                };
                let code = self.drop_temporary(expr.ty(), &code).unwrap_or(discarded);
                self.line(&code);
            }
            StmtKind::If { branches, otherwise } => self.emit_if(branches, otherwise.as_deref()),
            StmtKind::While { cond, body } => {
                if self.emit_hoisted_while(cond, body) {
                    return;
                }
                let cond = self.emit_condition(cond);
                self.line(&format!("while ({cond}) {{"));
                self.emit_block(body, true);
                self.line("}");
            }
            StmtKind::For { var, iter, body } => self.emit_for(var, iter, body),
            StmtKind::Match { subject, cases, borrowed } => self.emit_match(subject, cases, *borrowed),
            StmtKind::Block(body) => {
                self.line("{");
                self.emit_body(body);
                self.line("}");
            }
            StmtKind::Return(Some(value)) => {
                let code = self.emit_expr(value);
                if !self.has_owned() {
                    let code = self.wrap_return(Some(&code));
                    return self.line(&format!("return {code};"));
                }
                let c_type = self.types.c_type(value.ty());
                self.line(&format!("{{ {c_type} hsy_result = {code};"));
                self.drop_for_jump(false);
                let result = self.wrap_return(Some("hsy_result"));
                self.line(&format!("return {result}; }}"));
            }
            StmtKind::Return(None) => {
                self.drop_for_jump(false);
                let result = self.wrap_return(None);
                self.line(&format!("return {result};").replace("return ;", "return;"));
            }
            StmtKind::Raise(value) => self.emit_raise(value),
            StmtKind::Fn { decl, c_name } => self.emit_nested_fn(decl, c_name),
            StmtKind::Try { body, handlers, errors } => self.emit_try(body, handlers, errors),
            StmtKind::Break => {
                self.drop_for_jump(true);
                self.line("break;");
            }
            StmtKind::Continue => {
                self.drop_for_jump(true);
                self.line("continue;");
            }
            StmtKind::Pass | StmtKind::Comptime(_) | StmtKind::ComptimeAssert(_) => {}
            StmtKind::ComptimeIf(_) | StmtKind::ComptimeFor { .. } => {
                unreachable!("expanded by the type checker")
            }
        }
    }

    /// A condition without the outer parentheses that `if (...)` already provides.
    pub(super) fn emit_condition(&mut self, cond: &Expr) -> String {
        let code = self.emit_expr(cond);
        let parenthesized = match &cond.kind {
            ExprKind::Binary(op, ..) => op.is_comparison() || matches!(op, BinOp::And | BinOp::Or),
            ExprKind::Unary(UnOp::Not, _) => true,
            _ => false,
        };
        // `({ ... })` is a statement expression: its parentheses belong to it.
        if parenthesized && !code.starts_with("({") { code[1..code.len() - 1].to_string() } else { code }
    }

    /// `for v in range(...)`: the bounds are evaluated once, and stepping never overflows.
    fn emit_for(&mut self, var: &str, iter: &Expr, body: &[Stmt]) {
        let ExprKind::Range { start, end, step } = &iter.kind else {
            return self.emit_vector_for(var, iter, body);
        };
        let t = iter.ty().as_int().expect("range over integers");
        let (c, v) = (t.c_name(), c_ident(var));
        let (end_name, step_name) = (self.temp("end"), self.temp("step"));
        let (start, end) = (self.emit_expr(start), self.emit_expr(end));
        let step_code = step.as_ref().map_or_else(|| format!("(({c})1)"), |s| self.emit_expr(s));
        let comparison = match step.as_ref().map(|s| &s.kind) {
            None => format!("{v} < {end_name}"),
            Some(ExprKind::Int(n)) if *n > 0 => format!("{v} < {end_name}"),
            Some(ExprKind::Int(_)) => format!("{v} > {end_name}"),
            Some(_) => format!("{step_name} > 0 ? {v} < {end_name} : {v} > {end_name}"),
        };
        let next = self.helpers.call(Helper::RangeNext(t));
        self.line("{");
        self.indent += 1;
        self.line(&format!("const {c} {end_name} = {end};"));
        self.line(&format!("const {c} {step_name} = {step_code};"));
        if step.as_ref().is_some_and(|s| !matches!(s.kind, ExprKind::Int(_))) {
            self.line(&format!("if ({step_name} == 0) hsy_panic();"));
        }
        self.line(&format!(
            "for ({c} {v} = {start}; {comparison}; {v} = {next}({v}, {step_name}, {end_name})) {{"
        ));
        self.emit_block(body, true);
        self.line("}");
        self.indent -= 1;
        self.line("}");
    }

    /// `for x in v` over a vector: through `&` (each item by value if it is copied
    /// implicitly), through `&mut`, or owning the vector, each item then moved into `x`. An
    /// owned vector, or a copy taken because the loop changes the original, is released after
    /// the loop, whichever way it is left.
    fn emit_vector_for(&mut self, var: &str, iter: &Expr, body: &[Stmt]) {
        let Type::Vector(item) = iter.ty().deref_ref().clone() else { unreachable!("checked as a vector") };
        let (items, index, v) = (self.temp("items"), self.temp("i"), c_ident(var));
        let item_c = self.types.c_type(&item);
        self.line("{");
        self.indent += 1;
        self.open_scope(false);
        let code = self.emit_expr(iter);
        let c_type = self.types.c_type(iter.ty());
        let owned = !matches!(iter.ty(), Type::Ref(..));
        if owned {
            let vector = self.temp("vector");
            self.line(&format!("{c_type} {vector} = {code};"));
            self.line(&format!("{c_type}* {items} = &{vector};"));
            self.own(vector, iter.ty().clone());
        } else {
            self.line(&format!("{c_type} {items} = {code};"));
        }
        let place = format!("{items}->data[{index}]");
        let binding = match iter.ty() {
            // A copy is read like the original; a vector of its own gives its items away.
            _ if matches!(iter.kind, ExprKind::CopyOf(_)) && !item.is_copy() => {
                format!("{item_c}* {v} = &{place};")
            }
            Type::Ref(_, false) if !item.is_copy() => format!("{item_c}* {v} = &{place};"),
            Type::Ref(_, true) => format!("{item_c}* {v} = &{place};"),
            _ => format!("{item_c} {v} = {place};"),
        };
        let moves_items = owned && !matches!(iter.kind, ExprKind::CopyOf(_));
        self.line(&format!("for (size_t {index} = 0; {index} < {items}->len; {index}++) {{"));
        self.indent += 1;
        self.open_scope(true);
        self.line(&binding);
        if moves_items {
            self.take_part(&place, v, item.as_ref().clone());
        }
        for stmt in body {
            self.emit_stmt(stmt);
        }
        self.close_scope(ends_with_jump(body));
        self.indent -= 1;
        self.line("}");
        self.close_scope(false);
        self.indent -= 1;
        self.line("}");
    }

    /// `[x * 2 for x in v]`: a statement expression that fills a fresh vector.
    pub(super) fn emit_comprehension(&mut self, result: &str, fill: &Stmt, ty: &Type) -> String {
        let (out, indent) = (std::mem::take(&mut self.out), self.indent);
        self.indent = 0;
        let c_type = self.types.c_type(ty);
        let name = c_ident(result);
        self.line(&format!("{c_type} {name} = {{0}};"));
        self.emit_stmt(fill);
        let body = std::mem::replace(&mut self.out, out);
        self.indent = indent;
        // Kept on separate lines: the body carries `#line` directives.
        format!("({{\n{body}{name}; }})")
    }

    /// Assigning an owning value releases the old one, after the new one is computed.
    fn emit_assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr) {
        let lhs = self.emit_expr(target);
        if *target.ty() == Type::Str && op == Some(BinOp::Add) {
            let code = self.emit_str_append(&lhs, value);
            return self.line(&code);
        }
        let rhs = match op {
            Some(op) => self.emit_binary(op, target, value),
            None => self.emit_expr(value),
        };
        match self.types.drop_fn(target.ty()) {
            Some(drop) => {
                let c_type = self.types.c_type(target.ty());
                self.line(&format!("{{ {c_type} hsy_new = {rhs}; {drop}(&{lhs}); {lhs} = hsy_new; }}"));
            }
            None => self.line(&format!("{lhs} = {rhs};")),
        }
    }

    /// Variables are never `const` in C: their address may be passed to a `&` parameter.
    fn emit_var(&mut self, target: &VarTarget, value: &Expr) {
        let ty = self.types.c_type(value.ty());
        let code = self.emit_expr(value);
        match target {
            VarTarget::Name(name) => {
                self.line(&format!("{ty} {} = {code};", c_ident(name)));
                self.own(c_ident(name), value.ty().clone());
            }
            VarTarget::Tuple(names) => {
                let Type::Tuple(items) = value.ty() else { unreachable!("checked as a tuple") };
                let whole = self.temp("tuple");
                self.line(&format!("{ty} {whole} = {code};"));
                for (i, (name, item)) in names.iter().zip(items).enumerate() {
                    let item = self.types.c_type(item);
                    self.line(&format!("{item} {} = {whole}.f{i};", c_ident(name)));
                    self.own(c_ident(name), items[i].clone());
                }
            }
        }
    }

    fn emit_if(&mut self, branches: &[(Expr, Vec<Stmt>)], otherwise: Option<&[Stmt]>) {
        for (i, (cond, body)) in branches.iter().enumerate() {
            let cond = self.emit_condition(cond);
            let opener = if i == 0 { format!("if ({cond}) {{") } else { format!("}} else if ({cond}) {{") };
            self.line(&opener);
            self.emit_body(body);
        }
        if let Some(body) = otherwise {
            self.line("} else {");
            self.emit_body(body);
        }
        self.line("}");
    }

    /// A `match` is an `if` chain on the subject, read through a pointer when it is a place.
    fn emit_match(&mut self, subject: &Expr, cases: &[Case], borrowed: bool) {
        self.borrowed_match.push(borrowed);
        let ty = self.types.c_type(subject.ty());
        let name = self.temp("match");
        let code = self.emit_expr(subject);
        self.line("{");
        self.indent += 1;
        // A moved subject is owned by the match: bindings take their parts, the rest is dropped.
        self.open_scope(false);
        let value = if is_place(subject) || borrowed {
            self.line(&format!("{ty}* {name} = &{code};"));
            format!("(*{name})")
        } else {
            self.line(&format!("{ty} {name} = {code};"));
            self.own(name.clone(), subject.ty().clone());
            name
        };
        if cases.iter().any(|c| c.guard.is_some()) {
            self.emit_guarded_cases(cases, subject.ty(), &value);
            self.close_scope(false);
            self.indent -= 1;
            self.line("}");
            self.borrowed_match.pop();
            return;
        }
        for (i, case) in cases.iter().enumerate() {
            // The match is exhaustive, so the last case needs no test.
            let catch_all =
                i + 1 == cases.len() || matches!(case.pattern, Pattern::Wildcard(_) | Pattern::Bind(..));
            let opener = match (i, catch_all) {
                (0, true) => "{".to_string(),
                (_, true) => "} else {".to_string(),
                (0, false) => format!("if ({}) {{", self.pattern_test(&case.pattern, subject.ty(), &value)),
                _ => format!("}} else if ({}) {{", self.pattern_test(&case.pattern, subject.ty(), &value)),
            };
            self.line(&opener);
            self.indent += 1;
            self.open_scope(false);
            self.emit_bindings(&case.pattern, subject.ty(), &value);
            self.indent -= 1;
            self.emit_body(&case.body);
            self.indent += 1;
            self.close_scope(ends_with_jump(&case.body));
            self.indent -= 1;
            if catch_all {
                break;
            }
        }
        if !cases.is_empty() {
            self.line("}");
        }
        self.close_scope(false);
        self.indent -= 1;
        self.line("}");
        self.borrowed_match.pop();
    }

    /// Cases of a `match` that has guards. A case whose guard fails leaves the others to be
    /// tried, so each case is an `if` of its own that ends by leaving the match. The guard
    /// reads what the pattern binds by address; the body takes it once the guard holds.
    fn emit_guarded_cases(&mut self, cases: &[Case], ty: &Type, value: &str) {
        let end = self.temp("matched");
        let mut leaves = false;
        // The cases without guard cover every value: the last of them needs no test.
        let last = cases.iter().rposition(|c| c.guard.is_none()).unwrap_or(cases.len());
        for (i, case) in cases.iter().enumerate().take(last + 1) {
            let test = if i == last { "1".to_string() } else { self.pattern_test(&case.pattern, ty, value) };
            self.line(&format!("if ({test}) {{"));
            self.indent += 1;
            if let Some(guard) = &case.guard {
                let held = self.temp("guard");
                self.line(&format!("bool {held};"));
                self.line("{");
                self.indent += 1;
                self.open_scope(false);
                self.borrowed_match.push(true);
                self.emit_bindings(&case.pattern, ty, value);
                self.borrowed_match.pop();
                let mut guard = guard.clone();
                self.hoist_expr(&mut guard);
                let code = self.emit_condition(&guard);
                self.line(&format!("{held} = {code};"));
                self.close_scope(false);
                self.indent -= 1;
                self.line("}");
                self.line(&format!("if ({held}) {{"));
                self.indent += 1;
            }
            self.open_scope(false);
            self.emit_bindings(&case.pattern, ty, value);
            self.indent -= 1;
            self.emit_body(&case.body);
            self.indent += 1;
            let jumped = ends_with_jump(&case.body);
            self.close_scope(jumped);
            if !jumped && i != last {
                self.line(&format!("goto {end};"));
                leaves = true;
            }
            if case.guard.is_some() {
                self.indent -= 1;
                self.line("}");
            }
            self.indent -= 1;
            self.line("}");
        }
        if leaves {
            self.line(&format!("{end}: ;"));
        }
    }

    pub(super) fn pattern_test(&mut self, pattern: &Pattern, ty: &Type, value: &str) -> String {
        match pattern {
            Pattern::Wildcard(_) | Pattern::Bind(..) => "1".into(),
            Pattern::None(_) => format!("!{}", Self::has_value(ty, value)),
            Pattern::Literal(literal) => format!("{value} == {}", self.emit_expr(literal)),
            Pattern::Variant { variant, .. } => {
                format!("{value}.tag == {}", variant.expect("resolved variant"))
            }
            Pattern::ErrorType { index, .. } => {
                format!("{value}.err == {}", index.expect("resolved type") + 1)
            }
            Pattern::Or(options, _) => {
                let tests: Vec<String> =
                    options.iter().map(|p| format!("({})", self.pattern_test(p, ty, value))).collect();
                tests.join(" || ")
            }
        }
    }

    pub(super) fn emit_bindings(&mut self, pattern: &Pattern, ty: &Type, value: &str) {
        match pattern {
            Pattern::Bind(name, _) => self.bind(name, value, ty.clone()),
            Pattern::ErrorType { name: Some(name), index: Some(index), .. } => {
                let Type::Errors(types) = ty else { unreachable!("type patterns match caught errors") };
                let c_type = self.types.c_type(&types[*index]);
                let part = format!("{value}.as.e{index}");
                self.line(&format!("{c_type} {} = {part};", c_ident(name)));
                self.take_part(&part, c_ident(name), types[*index].clone());
            }
            Pattern::Variant { bindings: Some(names), variant: Some(index), .. } => {
                let Type::Enum { named, .. } = ty else { unreachable!("variant patterns match enums") };
                let fields = self.types.variant_fields(named.id, *index);
                let variant = c_ident(&self.types.variant_name(named.id, *index));
                for (name, (field, field_ty)) in names.iter().zip(fields) {
                    if name != "_" {
                        let part = format!("{value}.as.{variant}.{}", c_ident(&field));
                        self.bind(name, &part, field_ty);
                    }
                }
            }
            _ => {}
        }
    }

    /// `name = part`. A borrowed match binds what owns data by address instead.
    fn bind(&mut self, name: &str, part: &str, ty: Type) {
        let c_type = self.types.c_type(&ty);
        if self.borrowed_match.last() == Some(&true) && !ty.is_copy() {
            self.line(&format!("{c_type}* {} = &{part};", c_ident(name)));
            return;
        }
        self.line(&format!("{c_type} {} = {part};", c_ident(name)));
        self.take_part(part, c_ident(name), ty);
    }

    /// A binding that owns resources takes them from the subject, which is left empty there.
    pub(super) fn take_part(&mut self, part: &str, binding: String, ty: Type) {
        if self.types.table.needs_drop(&ty) {
            self.line(&format!("memset(&{part}, 0, sizeof {part});"));
            self.own(binding, ty);
        }
    }
}

/// Whether a block ends by leaving it, so that nothing after its last statement runs.
pub(super) fn ends_with_jump(stmts: &[Stmt]) -> bool {
    matches!(stmts.last().map(|s| &s.kind), Some(StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue))
}

/// Whether an expression names storage, so that it can be read through a pointer.
pub(super) fn is_place(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Name(_) | ExprKind::Deref(_) | ExprKind::Global { .. } => true,
        // What a pointer points to, such as the item `c[k]` finds through `Index`.
        ExprKind::Index { base, .. } if matches!(base.ty(), crate::types::Type::Ptr(_)) => true,
        ExprKind::Field { base, .. } | ExprKind::Index { base, .. } | ExprKind::Unwrap(base) => {
            is_place(base)
        }
        _ => false,
    }
}

impl Emitter<'_> {
    /// Each operand gets a local register variable bound to its register, as gcc and clang
    /// define for `asm` operands; outputs are written back after the instructions.
    fn emit_asm(&mut self, operands: &[crate::ast::AsmOperand], lines: &[String]) {
        use crate::ast::AsmKind;
        self.line("{");
        self.indent += 1;
        let (mut outputs, mut inputs, mut clobbers, mut write_back) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for operand in operands {
            let register = super::expr::c_string(&operand.register);
            let Some(value) = &operand.value else {
                clobbers.push(register);
                continue;
            };
            let c_type = self.types.c_type(value.ty());
            let name = self.temp("asm");
            let code = self.emit_expr(value);
            let init = if operand.kind == AsmKind::Out { String::new() } else { format!(" = {code}") };
            self.line(&format!("register {c_type} {name} __asm__({register}){init};"));
            match operand.kind {
                AsmKind::In => inputs.push(format!("\"r\"({name})")),
                AsmKind::Out => outputs.push(format!("\"=r\"({name})")),
                _ => outputs.push(format!("\"+r\"({name})")),
            }
            if operand.kind != AsmKind::In {
                write_back.push(format!("{code} = {name};"));
            }
        }
        // `%` introduces operands in a template: written as `%%`.
        let text = lines.iter().map(|l| l.replace('%', "%%")).collect::<Vec<_>>().join("\n\t");
        let template = super::expr::c_string(&text);
        self.line(&format!(
            "__asm__ volatile ({template} : {} : {} : {});",
            outputs.join(", "),
            inputs.join(", "),
            clobbers.join(", ")
        ));
        for line in write_back {
            self.line(&line);
        }
        self.indent -= 1;
        self.line("}");
    }
}
