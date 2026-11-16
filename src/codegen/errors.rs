//! Errors as return values: a fallible function returns a result struct, `check` tests it,
//! and an error goes to the innermost `try` by `goto`, or back to the caller.

use super::Emitter;
use super::expr::c_ident;
use crate::ast::{Catch, Expr, Handler, Stmt};
use crate::types::Type;

#[derive(Clone)]
pub struct TryFrame {
    /// The variable holding the caught error, and the label of the `except` clauses.
    slot: String,
    label: String,
    errors: Vec<Type>,
    /// Scopes open outside the `try`; the ones inside are dropped before jumping.
    depth: usize,
}

impl Emitter<'_> {
    /// Sends the error held in `source` (typed by `errors`) to the innermost `try`, or
    /// returns it from the function.
    fn propagate(&mut self, source: &str, errors: &[Type]) -> String {
        let (target, targets, depth) = match self.tries.last() {
            Some(frame) => (frame.slot.clone(), frame.errors.clone(), frame.depth),
            None => ("hsy_out".to_string(), self.fn_raises.clone(), 0),
        };
        let mut cases = String::new();
        for (i, error) in errors.iter().enumerate() {
            if let Some(j) = targets.iter().position(|t| t == error) {
                cases += &format!(
                    "case {}: {target}.err = {}; {target}.as.e{j} = {source}.as.e{i}; break; ",
                    i + 1,
                    j + 1
                );
            }
        }
        let route = format!("switch ({source}.err) {{ {cases}default: break; }} ");
        let drops = self.drops_from(depth);
        if let Some(frame) = self.tries.last() {
            format!("{route}{drops}goto {};", frame.label)
        } else {
            let result = self.types.result_type(&self.fn_ret.clone(), &targets);
            format!("{{ {result} hsy_out = {{0}}; {route}{drops}return hsy_out; }}")
        }
    }

    /// `check f(...)`: the call's value, or a jump with its error.
    pub(super) fn emit_check(&mut self, call: &Expr, errors: &[Type]) -> String {
        let result = self.types.result_type(call.ty(), errors);
        let code = self.emit_expr(call);
        let temp = self.temp("call");
        let on_error = self.propagate(&temp, errors);
        let value = if *call.ty() == Type::Void { String::new() } else { format!(" {temp}.value;") };
        format!("({{ {result} {temp} = {code}; if ({temp}.err) {{ {on_error} }}{value} }})")
    }

    pub(super) fn emit_raise(&mut self, value: &Expr) {
        let ty = value.ty().clone();
        if let Type::Errors(types) = &ty {
            // `raise e` after `except e:`: `e` already has the shape of an error result.
            let result = self.types.c_type(&ty);
            let code = self.emit_expr(value);
            let on_error = self.propagate("hsy_raised", types);
            return self.line(&format!("{{ {result} hsy_raised = {code}; {on_error} }}"));
        }
        let result = self.types.result_type(&Type::Void, std::slice::from_ref(&ty));
        let code = self.emit_expr(value);
        let on_error = self.propagate("hsy_raised", &[ty]);
        self.line(&format!("{{ {result} hsy_raised = {{.err = 1, .as.e0 = {code}}}; {on_error} }}"));
    }

    /// What `return` gives back: the value itself, or wrapped in the result struct.
    pub(super) fn wrap_return(&mut self, value: Option<&str>) -> String {
        if self.fn_raises.is_empty() {
            return value.unwrap_or_default().to_string();
        }
        let result = self.types.result_type(&self.fn_ret.clone(), &self.fn_raises.clone());
        match value {
            Some(value) => format!("(({result}){{.err = 0, .value = {value}}})"),
            None => format!("(({result}){{0}})"),
        }
    }

    pub(super) fn emit_try(&mut self, body: &[Stmt], handlers: &[Handler], errors: &[Type]) {
        let result = self.types.result_type(&Type::Void, errors);
        let slot = self.temp("caught");
        let label = self.temp("catch");
        let done = self.temp("done");
        self.line("{");
        self.indent += 1;
        self.line(&format!("{result} {slot} = {{0}};"));
        let depth = self.scopes.len();
        self.tries.push(TryFrame {
            slot: slot.clone(),
            label: label.clone(),
            errors: errors.to_vec(),
            depth,
        });
        self.line("{");
        self.emit_body(body);
        self.line("}");
        self.tries.pop();
        self.line(&format!("goto {done};"));
        self.line(&format!("{label}:;"));
        for (i, handler) in handlers.iter().enumerate() {
            let test = self.catch_test(&handler.catch, errors, &slot);
            self.line(&format!("{}if ({test}) {{", if i == 0 { "" } else { "} else " }));
            self.emit_handler(handler, errors, &slot);
        }
        let targets = self.tries.last().map_or(self.fn_raises.clone(), |frame| frame.errors.clone());
        if errors.iter().any(|e| targets.contains(e)) {
            let rest = self.propagate(&slot, errors);
            self.line(&format!("}} else {{ {rest} }}"));
        } else {
            self.line("}");
        }
        self.line(&format!("{done}:;"));
        self.indent -= 1;
        self.line("}");
    }

    fn catch_test(&mut self, catch: &Catch, errors: &[Type], slot: &str) -> String {
        match catch {
            Catch::All { error: None, several, .. } => {
                let tests: Vec<String> = several.iter().map(|i| format!("{slot}.err == {}", i + 1)).collect();
                format!("({})", tests.join(" || "))
            }
            Catch::Type { error, .. } | Catch::All { error, .. } => {
                format!("{slot}.err == {}", error.expect("resolved") + 1)
            }
            Catch::Variant { pattern, error } => {
                let index = error.expect("resolved");
                let value = format!("{slot}.as.e{index}");
                format!(
                    "{slot}.err == {} && {}",
                    index + 1,
                    self.pattern_test(pattern, &errors[index], &value)
                )
            }
        }
    }

    /// The clause owns the caught error: its bindings take parts of it, the rest is dropped.
    fn emit_handler(&mut self, handler: &Handler, errors: &[Type], slot: &str) {
        if let Catch::All { name, error: None, several } = &handler.catch {
            return self.emit_union_handler(handler, name, several, errors, slot);
        }
        let (Catch::Type { error, .. } | Catch::Variant { error, .. } | Catch::All { error, .. }) =
            &handler.catch;
        let index = error.expect("resolved");
        let (ty, value) = (errors[index].clone(), format!("{slot}.as.e{index}"));
        self.indent += 1;
        self.open_scope(false);
        self.own(value.clone(), ty.clone());
        match &handler.catch {
            Catch::Type { name, .. } | Catch::All { name, .. } => {
                let c_type = self.types.c_type(&ty);
                self.line(&format!("{c_type} {} = {value};", c_ident(name)));
                self.take_part(&value, c_ident(name), ty);
            }
            Catch::Variant { pattern, .. } => self.emit_bindings(pattern, &ty, &value),
        }
        self.indent -= 1;
        self.emit_body(&handler.body);
        self.indent += 1;
        self.close_scope(super::stmt::ends_with_jump(&handler.body));
        self.indent -= 1;
    }

    /// `except e:` over several types: `e` takes the caught error, renumbered among them.
    fn emit_union_handler(
        &mut self,
        handler: &Handler,
        name: &str,
        several: &[usize],
        errors: &[Type],
        slot: &str,
    ) {
        let ty = Type::Errors(several.iter().map(|i| errors[*i].clone()).collect());
        let c_type = self.types.c_type(&ty);
        let name = c_ident(name);
        let cases: String = several
            .iter()
            .enumerate()
            .map(|(j, i)| {
                format!("case {}: {name}.err = {}; {name}.as.e{j} = {slot}.as.e{i}; break; ", i + 1, j + 1)
            })
            .collect();
        self.indent += 1;
        self.open_scope(false);
        self.line(&format!("{c_type} {name} = {{0}};"));
        self.line(&format!("switch ({slot}.err) {{ {cases}default: break; }}"));
        self.line(&format!("memset(&{slot}, 0, sizeof {slot});"));
        self.own(name, ty);
        self.indent -= 1;
        self.emit_body(&handler.body);
        self.indent += 1;
        self.close_scope(super::stmt::ends_with_jump(&handler.body));
        self.indent -= 1;
    }
}
