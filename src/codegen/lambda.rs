//! Lambdas as C functions. A plain lambda is a function; a lambda passed as `&fn(...)` takes
//! an environment first, which points to the variables it captures in the caller's frame.

use super::Emitter;
use super::expr::c_ident;
use crate::ast::{Capture, Expr, FnDecl, Param};
use crate::types::Type;

impl Emitter<'_> {
    /// A lambda lent to a `&fn(...)` parameter: its function, and a closure value pointing
    /// to an environment made of the addresses of its captures.
    pub(super) fn emit_closure(
        &mut self,
        captures: &[Capture],
        params: &[Param],
        body: &Expr,
        c_name: &str,
        function: &Type,
    ) -> String {
        let prologue = self.environment(captures, c_name);
        self.emit_lambda_with(params, body, c_name, function, Some(&prologue));
        self.closure_value(captures, c_name, function)
    }

    /// Defines the environment struct of `c_name` and returns the code that reads the
    /// captures back from `hsy_env`, as local pointers.
    fn environment(&mut self, captures: &[Capture], c_name: &str) -> String {
        let env = format!("{c_name}_env");
        let mut fields = String::new();
        let mut prologue = String::from("(void)hsy_env; ");
        for capture in captures {
            let ty = self.types.c_type(capture.ty.as_ref().expect("typed capture"));
            let name = c_ident(&capture.name);
            fields += &format!("{ty}* {name}; ");
            prologue += &format!("{ty}* {name} = ((struct {env}*)hsy_env)->{name}; ");
        }
        if !captures.is_empty() {
            self.lambdas += &format!("struct {env} {{ {fields}}};\n\n");
        }
        prologue
    }

    /// The borrowed function: the code, and the addresses of the captures in this frame.
    fn closure_value(&mut self, captures: &[Capture], c_name: &str, function: &Type) -> String {
        let (closure, _) = self.types.closure(function);
        if captures.is_empty() {
            return format!("(({closure}){{{c_name}, NULL}})");
        }
        let addresses: Vec<String> = captures
            .iter()
            .map(|c| if c.through_ref { c_ident(&c.name) } else { format!("&{}", c_ident(&c.name)) })
            .collect();
        format!("(({closure}){{{c_name}, &(struct {c_name}_env){{{}}}}})", addresses.join(", "))
    }

    /// A nested function becomes a C function of its own; its name is a local variable
    /// holding it, as a function pointer or, when it captures, as a borrowed function.
    pub(super) fn emit_nested_fn(&mut self, decl: &FnDecl, c_name: &str) {
        let sig = &self.signatures[c_name];
        let (params, ret) = (sig.params.clone(), sig.ret.clone());
        let function = Type::Fn(params.clone(), Box::new(ret.clone()));
        let captures = !decl.captures.is_empty();
        let value_ty = if captures { Type::Ref(Box::new(function.clone()), false) } else { function.clone() };
        let value_c = self.types.c_type(&value_ty);
        let name = c_ident(&decl.name);
        let prologue = if captures { Some(self.environment(&decl.captures, c_name)) } else { None };
        let outer = (
            std::mem::take(&mut self.out),
            std::mem::take(&mut self.indent),
            std::mem::take(&mut self.scopes),
            std::mem::replace(&mut self.fn_ret, ret.clone()),
            std::mem::take(&mut self.fn_raises),
            std::mem::take(&mut self.tries),
        );
        let names: Vec<&str> = decl.params.iter().map(|p| p.name.as_str()).collect();
        let mut params_c = self.c_params(&params, &names, false);
        let itself = match &prologue {
            Some(_) => format!("{{{c_name}, hsy_env}}"),
            None => c_name.to_string(),
        };
        if prologue.is_some() {
            params_c = if params_c == "void" {
                "void* hsy_env".into()
            } else {
                format!("void* hsy_env, {params_c}")
            };
        }
        let ret_c = self.types.c_type(&ret);
        self.line(&format!("HSY_UNUSED static {ret_c} {c_name}({params_c}) {{"));
        self.indent += 1;
        if let Some(prologue) = &prologue {
            self.line(prologue.trim_end());
        }
        self.line(&format!("HSY_UNUSED {value_c} {name} = {itself};"));
        // The item of a `parallel for` may go unread.
        let unread: String = names.iter().map(|n| format!("(void){}; ", c_ident(n))).collect();
        if !unread.is_empty() {
            self.line(unread.trim_end());
        }
        self.open_scope(false);
        for (param, ty) in names.iter().zip(&params) {
            self.own(c_ident(param), ty.clone());
        }
        self.indent -= 1;
        self.emit_body(&decl.body);
        self.indent += 1;
        let jumped = super::stmt::ends_with_jump(&decl.body);
        self.close_scope(jumped);
        if !jumped && ret != Type::Void {
            self.line("__builtin_unreachable();");
        }
        self.indent -= 1;
        self.line("}\n");
        let function_c = std::mem::take(&mut self.out);
        self.lambdas += &function_c;
        (self.out, self.indent, self.scopes, self.fn_ret, self.fn_raises, self.tries) = outer;
        let value =
            if captures { self.closure_value(&decl.captures, c_name, &function) } else { c_name.to_string() };
        self.line(&format!("HSY_UNUSED {value_c} {name} = {value};"));
    }

    /// Writes a lambda as a C function of its own, and gives its name as the value.
    pub(super) fn emit_lambda(&mut self, params: &[Param], body: &Expr, c_name: &str, ty: &Type) -> String {
        self.emit_lambda_with(params, body, c_name, ty, None);
        c_name.to_string()
    }

    /// The C function of a lambda; with `env`, it first takes its environment and runs `env`.
    fn emit_lambda_with(
        &mut self,
        params: &[Param],
        body: &Expr,
        c_name: &str,
        ty: &Type,
        env: Option<&str>,
    ) {
        let Type::Fn(types, ret) = ty else { unreachable!("lambdas are functions") };
        let outer = (
            std::mem::take(&mut self.scopes),
            std::mem::replace(&mut self.fn_ret, ret.as_ref().clone()),
            std::mem::take(&mut self.fn_raises),
            std::mem::take(&mut self.tries),
        );
        self.open_scope(false);
        let names: Vec<String> = params.iter().map(|p| c_ident(&p.name)).collect();
        for (name, ty) in names.iter().zip(types) {
            self.own(name.clone(), ty.clone());
        }
        let code = self.emit_expr(body);
        let drops = self.drops_from(0);
        let ret_c = self.types.c_type(ret);
        let inner = match ret.as_ref() {
            Type::Void => {
                let code = self.drop_temporary(body.ty(), &code).unwrap_or(format!("{code};"));
                format!("{code} {drops}")
            }
            _ => format!("{ret_c} hsy_result = {code}; {drops}return hsy_result;"),
        };
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        let mut params_c = self.c_params(types, &names, false);
        let mut inner = inner;
        if let Some(prologue) = env {
            params_c = if params_c == "void" {
                "void* hsy_env".into()
            } else {
                format!("void* hsy_env, {params_c}")
            };
            inner = format!("{prologue}{inner}");
        }
        self.lambdas += &format!("HSY_UNUSED static {ret_c} {c_name}({params_c}) {{\n    {inner}\n}}\n\n");
        (self.scopes, self.fn_ret, self.fn_raises, self.tries) = outer;
    }
}
