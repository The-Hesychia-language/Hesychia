//! Files placed in the program when compiling: `embed("help.txt")` gives a file's text,
//! `embed_bytes("logo.png")` its bytes, and `source()` the text of the file it is written in.
//! `offsec.quine` reads the whole program through `program_source()` and `program_files()`,
//! which only the standard library calls.

use super::Checker;
use super::resolve::Tables;
use crate::ast::{Expr, ExprKind};
use crate::diag::{Diagnostic, Result, Span};
use crate::types::{IntTy, Type};

impl Checker<'_> {
    pub(super) fn check_embedded(&mut self, expr: &mut Expr, expected: Option<&Type>) -> Result<Type> {
        let span = expr.span;
        let ExprKind::Call { callee, args, .. } = &expr.kind else { unreachable!("called on a call") };
        let callee = callee.clone();
        let wants_path = matches!(callee.as_str(), "embed" | "embed_bytes");
        if wants_path != (args.len() == 1) {
            let shape = if wants_path { format!("`{callee}(\"path\")`") } else { format!("`{callee}()`") };
            return Err(Diagnostic::error(span, format!("this is written {shape}")));
        }
        let path = || match &args[0].kind {
            ExprKind::Str(path) => Ok(path.clone()),
            _ => Err(Diagnostic::error(args[0].span, format!("`{callee}` takes a path written as a text"))
                .with_hint(format!("`{callee}(\"data/help.txt\")`, from the folder of this file"))),
        };
        let (kind, ty) = match callee.as_str() {
            "embed" => (ExprKind::Str(self.tables.embedded_text(self.module, &path()?, span)?), Type::Str),
            "embed_bytes" => {
                let bytes = self.tables.embedded_file(self.module, &path()?, span)?;
                (ExprKind::Bytes(bytes), Type::Vector(Box::new(Type::Int(IntTy::U8))))
            }
            "source" => (ExprKind::Str(self.tables.file_text(self.module, span)?), Type::Str),
            _ if self.tables.module_files[self.module].is_some() => {
                let name = callee.strip_prefix("program_").unwrap_or(&callee);
                return Err(Diagnostic::error(span, format!("`{callee}` is for the standard library"))
                    .with_hint(format!("use `offsec.quine.{name}()`")));
            }
            "program_source" => (ExprKind::Str(self.tables.file_text(0, span)?), Type::Str),
            _ => {
                let text = |s: &String| Expr::new(ExprKind::Str(s.clone()), span);
                let (keys, values) =
                    self.tables.files.iter().map(|(path, body)| (text(path), text(body))).unzip();
                expr.kind = ExprKind::Collection { keys, values: Some(values), built: None };
                return self.check_collection(expr, expected);
            }
        };
        expr.kind = kind;
        Ok(ty)
    }
}

impl Tables {
    /// The text of the file `path` names, from the folder of `module`'s file.
    pub(super) fn embedded_text(&self, module: usize, path: &str, span: Span) -> Result<String> {
        String::from_utf8(self.embedded_file(module, path, span)?).map_err(|_| {
            Diagnostic::error(span, "this file is not UTF-8 text")
                .with_hint("read its bytes with `embed_bytes`")
        })
    }

    /// The bytes of the file `path` names, from the folder of `module`'s file.
    pub(super) fn embedded_file(&self, module: usize, path: &str, span: Span) -> Result<Vec<u8>> {
        let Some(file) = &self.module_files[module] else {
            return Err(Diagnostic::error(
                span,
                "`embed` reads files of the program, not of the standard library",
            ));
        };
        let full = file.parent().unwrap_or(std::path::Path::new("")).join(path);
        std::fs::read(&full).map_err(|e| {
            Diagnostic::error(span, format!("cannot read `{}`: {e}", full.display()))
                .with_hint("the path starts from the folder of this file")
        })
    }

    /// The text of `module`'s file, as the program was compiled.
    pub(super) fn file_text(&self, module: usize, span: Span) -> Result<String> {
        let key = self.module_keys[module].as_ref().ok_or_else(|| {
            Diagnostic::error(span, "`source()` gives the text of a file of the program")
                .with_hint("the standard library and Git dependencies are not copied")
        })?;
        let (_, text) = self.files.iter().find(|(path, _)| path == key).expect("every key has its file");
        Ok(text.clone())
    }
}
