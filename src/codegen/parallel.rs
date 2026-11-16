//! `parallel for`, whose worker the runtime runs over slices of the items on every core,
//! and `select`, which waits on several channels at once.

use super::Emitter;
use super::expr::c_ident;
use crate::ast::{Builtin, Expr, ExprKind, FnDecl, SelectCase, Stmt};
use crate::types::Type;

impl Emitter<'_> {
    pub(super) fn emit_parallel_for(&mut self, iter: &Expr, worker: &FnDecl, c_name: &str) {
        self.types.uses_threads = true;
        self.types.uses_parallel = true;
        self.line("{");
        self.indent += 1;
        self.emit_nested_fn(worker, c_name);
        let item = self.signatures[c_name].params[0].clone();
        let function = Type::Fn(vec![item.clone()], Box::new(Type::Void));
        let closure = !worker.captures.is_empty();
        let value_ty = if closure { Type::Ref(Box::new(function), false) } else { function };
        let value_c = self.types.c_type(&value_ty);
        let site = self.temp("parallel");
        let (frame, run) = (format!("{site}_frame"), format!("{site}_run"));
        let call = |arg: &str| {
            if closure { format!("c->f.code(c->f.env, {arg});") } else { format!("c->f({arg});") }
        };
        let f = c_ident(&worker.name);
        let (fields, item_of, setup, count) = match &iter.kind {
            ExprKind::Range { start, end, step } => {
                let t = iter.ty().as_int().expect("range over integers").c_name();
                let (start, end) = (self.emit_expr(start), self.emit_expr(end));
                let step = step.as_ref().map_or_else(|| "1".to_string(), |s| self.emit_expr(s));
                let fields = format!("{t} start; {t} step;");
                let item = format!("({t})((__int128)c->start + (__int128)k * c->step)");
                let setup = format!(
                    "const {t} hsy_start = {start}; const {t} hsy_end = {end}; const {t} hsy_step = {step}; \
                     if (hsy_step == 0) hsy_panic(); {frame} hsy_frame = {{{f}, hsy_start, hsy_step}};"
                );
                let count = "hsy_range_count((__int128)hsy_start, (__int128)hsy_end, (__int128)hsy_step)";
                (fields, item, setup, count.to_string())
            }
            _ => {
                let vector = self.types.c_type(iter.ty());
                let items = self.emit_expr(iter);
                let item =
                    if matches!(item, Type::Ref(..)) { "&c->items->data[k]" } else { "c->items->data[k]" };
                let setup = format!("{frame} hsy_frame = {{{f}, {items}}};");
                (format!("{vector} items;"), item.to_string(), setup, "hsy_frame.items->len".to_string())
            }
        };
        let body = call(&item_of);
        self.types.forwards += &format!("typedef struct {frame} {frame};\n");
        self.lambdas += &format!(
            "struct {frame} {{ {value_c} f; {fields} }};\n\n\
             static void {run}(void* p, size_t lo, size_t hi) {{\n    {frame}* c = p;\n    \
             for (size_t k = lo; k < hi; k++) {body}\n}}\n\n"
        );
        self.line(&setup);
        self.line(&format!("hsy_parallel({count}, {run}, &hsy_frame);"));
        self.indent -= 1;
        self.line("}");
    }

    /// The receivers are read once; `hsy_select` gives the case that fired, which takes its
    /// message as an `Optional`, released with the case's block.
    pub(super) fn emit_select(&mut self, cases: &[SelectCase], otherwise: Option<&[Stmt]>) {
        self.types.uses_threads = true;
        self.types.uses_select = true;
        self.line("{");
        self.indent += 1;
        let (chans, outs, got) = (self.temp("chans"), self.temp("outs"), self.temp("got"));
        let mut receivers = Vec::new();
        let mut messages = Vec::new();
        let mut limit = if otherwise.is_some() { "0".to_string() } else { "-1".to_string() };
        let mut timed = None;
        for case in cases {
            let ExprKind::Builtin { op, args } = &case.wait.kind else { unreachable!("checked") };
            match op {
                Builtin::ChanReceive => {
                    let Type::Optional(item) = case.wait.ty() else {
                        unreachable!("receive gives an Optional")
                    };
                    let item_c = self.types.c_type(item);
                    let message = self.temp("message");
                    let value = self.emit_expr(&args[0]);
                    self.line(&format!("{item_c} {message}; memset(&{message}, 0, sizeof {message});"));
                    receivers.push(value);
                    messages.push(message);
                }
                Builtin::SelectSleep => {
                    let (duration, unit) = (self.emit_expr(&args[0]), self.emit_expr(&args[1]));
                    limit = format!("hsy_select_limit({duration}, {unit})");
                    timed = Some(case);
                }
                _ => unreachable!("checked"),
            }
        }
        let count = receivers.len();
        self.line(&format!("hsy_chan* {chans}[{count}] = {{{}}};", receivers.join(", ")));
        let addresses: Vec<String> = messages.iter().map(|m| format!("&{m}")).collect();
        self.line(&format!("void* {outs}[{count}] = {{{}}};", addresses.join(", ")));
        self.line(&format!("bool {got} = false;"));
        let which = self.temp("which");
        self.line(&format!("size_t {which} = hsy_select({chans}, {outs}, {count}, {limit}, &{got});"));
        let receiving = cases
            .iter()
            .filter(|c| matches!(c.wait.kind, ExprKind::Builtin { op: Builtin::ChanReceive, .. }));
        for (index, case) in receiving.enumerate() {
            let ty = case.wait.ty().clone();
            let optional = self.types.c_type(&ty);
            let name = case.name.as_ref().map_or_else(|| self.temp("unused"), |n| c_ident(n));
            let keyword = if index == 0 { "if" } else { "else if" };
            self.line(&format!("{keyword} ({which} == {index}) {{"));
            self.indent += 1;
            self.open_scope(false);
            self.line(&format!(
                "HSY_UNUSED {optional} {name} = {{.has = {got}, .value = {}}};",
                messages[index]
            ));
            self.own(name, ty);
            for stmt in &case.body {
                self.emit_stmt(stmt);
            }
            self.close_scope(super::stmt::ends_with_jump(&case.body));
            self.indent -= 1;
            self.line("}");
        }
        if let Some(body) = timed.map(|c| c.body.as_slice()).or(otherwise) {
            self.line("else {");
            self.emit_block(body, false);
            self.line("}");
        }
        self.indent -= 1;
        self.line("}");
    }
}
