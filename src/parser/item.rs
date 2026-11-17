//! Top-level declarations, imports and the `comptime` forms shared with statements.

use super::Parser;
use crate::ast::{
    ComptimeAssert, ComptimeConst, ComptimeIf, Decorator, Expr, ExprKind, ExternFn, FnDecl, GlobalDecl,
    Import, ImportKind, Item, Level, ParsedFile,
};
use crate::diag::{Diagnostic, Result, Span};
use crate::lexer::{Kw, Sym, Tok};

impl Parser {
    pub(super) fn parse_file(&mut self) -> Result<ParsedFile> {
        let mut file = ParsedFile { imports: Vec::new(), items: Vec::new() };
        while !self.at(&Tok::Eof) {
            if matches!(self.peek(), Tok::Kw(Kw::Import | Kw::From)) {
                file.imports.push(self.parse_import()?);
            } else {
                file.items.push(self.parse_item()?);
            }
        }
        Ok(file)
    }

    /// A declaration, with the decorators written above it.
    fn parse_item(&mut self) -> Result<Item> {
        let doc = self.doc();
        let mut decorators = Vec::new();
        while self.at(&Tok::Sym(Sym::At)) {
            decorators.push(self.parse_decorator()?);
        }
        let mut item = self.parse_bare_item()?;
        match &mut item {
            Item::Struct(s) => s.doc = doc,
            Item::Enum(e) => e.doc = doc,
            _ => {}
        }
        match &mut item {
            Item::Fn(f) => f.decorators = decorators,
            Item::Struct(s) => s.decorators = decorators,
            // `@symbol("sqrt")` binds an `extern fn` of another name to that C function.
            Item::Extern(e) if decorators.len() == 1 && decorators[0].name == "symbol" => {
                let [Expr { kind: ExprKind::Str(symbol), .. }] = &decorators[0].args[..] else {
                    return Err(Diagnostic::error(decorators[0].span, "write `@symbol(\"name\")`"));
                };
                e.symbol.clone_from(symbol);
            }
            _ if !decorators.is_empty() => {
                return Err(Diagnostic::error(
                    decorators[0].span,
                    "only functions and structs take decorators, and an `extern fn` takes `@symbol(...)`",
                ));
            }
            _ => {}
        }
        Ok(item)
    }

    /// `@name` or `@name(args)`, on its own line.
    fn parse_decorator(&mut self) -> Result<Decorator> {
        let span = self.span();
        self.expect_sym(Sym::At)?;
        let name = self.expect_ident()?;
        let args = if self.at(&Tok::Sym(Sym::LParen)) { self.parse_args()? } else { Vec::new() };
        self.expect_newline()?;
        Ok(Decorator { name, args, span })
    }

    fn parse_bare_item(&mut self) -> Result<Item> {
        let private = self.eat_kw(Kw::Priv);
        if !private && self.at_extend() {
            return self.parse_extend().map(Item::Extend);
        }
        match self.peek() {
            Tok::Kw(Kw::Extern) => self.parse_extern(private).map(Item::Extern),
            Tok::Kw(Kw::Trait) => self.parse_trait(private).map(Item::Trait),
            Tok::Kw(Kw::Manual | Kw::Managed) => self.parse_leveled(private),
            Tok::Kw(Kw::Comptime) => self.parse_comptime_item(private),
            Tok::Kw(Kw::Const) => self.parse_global(private).map(Item::Global),
            _ => Err(self.unexpected("a declaration")),
        }
    }

    /// `manual` or `managed`, followed by `fn`, `struct` or `enum`.
    fn parse_leveled(&mut self, private: bool) -> Result<Item> {
        let span = self.span();
        let level = if self.advance() == Tok::Kw(Kw::Manual) { Level::Manual } else { Level::Managed };
        match self.advance() {
            Tok::Kw(Kw::Fn) => self.parse_fn_rest(level, private, span).map(Item::Fn),
            Tok::Kw(Kw::Struct) => self.parse_struct(level, private, span).map(Item::Struct),
            Tok::Kw(Kw::Enum) => self.parse_enum(level, private, span).map(Item::Enum),
            other => Err(Diagnostic::error(
                span,
                format!("expected `fn`, `struct` or `enum`, found {}", other.describe()),
            )),
        }
    }

    /// A function after `fn`: name, parameters, return type and body.
    pub(super) fn parse_fn_rest(&mut self, level: Level, private: bool, span: Span) -> Result<FnDecl> {
        let name = self.expect_ident()?;
        let captures = if self.at_captures() { self.parse_captures()? } else { Vec::new() };
        let generics = self.parse_generics()?;
        let (params, variadic) = self.parse_params()?;
        if variadic {
            return Err(Diagnostic::error(span, "only `extern` functions can be variadic"));
        }
        let ret = self.parse_return_type()?;
        let raises = self.parse_raises()?;
        let body = self.parse_block()?;
        Ok(FnDecl {
            level,
            private,
            name,
            generics,
            captures,
            receiver: None,
            params,
            ret,
            raises,
            body,
            decorators: Vec::new(),
            span,
        })
    }

    fn parse_extern(&mut self, private: bool) -> Result<ExternFn> {
        let span = self.span();
        self.expect_kw(Kw::Extern)?;
        self.expect_kw(Kw::Fn)?;
        let name = self.expect_ident()?;
        let (params, variadic) = self.parse_params()?;
        if variadic && params.is_empty() {
            return Err(Diagnostic::error(span, "a variadic function needs at least one fixed parameter"));
        }
        let ret = self.parse_return_type()?;
        self.expect_newline()?;
        Ok(ExternFn { private, symbol: name.clone(), name, params, variadic, ret, span })
    }

    fn parse_comptime_item(&mut self, private: bool) -> Result<Item> {
        let span = self.span();
        self.expect_kw(Kw::Comptime)?;
        let item = match self.peek() {
            Tok::Kw(Kw::If) => Item::ComptimeIf(self.parse_comptime_if(Self::parse_item_block)?),
            Tok::Kw(Kw::Fn) => {
                self.advance();
                return self.parse_fn_rest(Level::Manual, private, span).map(Item::ComptimeFn);
            }
            Tok::Ident(name) if name == "assert" => Item::ComptimeAssert(self.parse_comptime_assert(span)?),
            _ => return self.parse_comptime_const(private, span).map(Item::Comptime),
        };
        if private {
            return Err(Diagnostic::error(span, "`priv` only applies to declarations"));
        }
        Ok(item)
    }

    fn parse_item_block(&mut self) -> Result<Vec<Item>> {
        self.expect_sym(Sym::Colon)?;
        self.expect_newline()?;
        self.expect(Tok::Indent)?;
        let mut items = Vec::new();
        while !self.eat(&Tok::Dedent) {
            items.push(self.parse_item()?);
        }
        Ok(items)
    }

    /// `if cond: body` with `elif`/`else`, after `comptime`; `body` parses items or statements.
    pub(super) fn parse_comptime_if<T>(
        &mut self,
        body: fn(&mut Self) -> Result<Vec<T>>,
    ) -> Result<ComptimeIf<T>> {
        self.expect_kw(Kw::If)?;
        let mut branches = vec![(self.parse_expr()?, body(self)?)];
        while self.eat_kw(Kw::Elif) {
            branches.push((self.parse_expr()?, body(self)?));
        }
        let otherwise = if self.eat_kw(Kw::Else) { Some(body(self)?) } else { None };
        Ok(ComptimeIf { branches, otherwise })
    }

    /// `assert(cond[, "message"])`, after `comptime`.
    pub(super) fn parse_comptime_assert(&mut self, span: Span) -> Result<ComptimeAssert> {
        self.advance();
        self.expect_sym(Sym::LParen)?;
        let cond = self.parse_expr()?;
        let message = if self.eat_sym(Sym::Comma) { Some(self.expect_str()?) } else { None };
        self.expect_sym(Sym::RParen)?;
        self.expect_newline()?;
        Ok(ComptimeAssert { cond, message, span })
    }

    /// `const NAME: T = value` at module level; the checker accepts it in the standard library.
    fn parse_global(&mut self, private: bool) -> Result<GlobalDecl> {
        let span = self.span();
        self.advance();
        let name = self.expect_ident()?;
        self.expect_sym(Sym::Colon)?;
        let ty = self.parse_type()?;
        self.expect_sym(Sym::Assign)?;
        let value = self.parse_expr()?;
        self.expect_newline()?;
        Ok(GlobalDecl { private, name, ty, value, span })
    }

    /// `NAME[: T] = value`, after `comptime`.
    pub(super) fn parse_comptime_const(&mut self, private: bool, span: Span) -> Result<ComptimeConst> {
        let name = self.expect_ident()?;
        let ty = self.parse_optional_type()?;
        self.expect_sym(Sym::Assign)?;
        let value = self.parse_expr()?;
        self.expect_newline()?;
        Ok(ComptimeConst { private, name, ty, value, span })
    }

    fn parse_import(&mut self) -> Result<Import> {
        let span = self.span();
        if self.eat_kw(Kw::Import) {
            let module = self.parse_module_path()?;
            let alias = self.parse_alias()?;
            self.expect_newline()?;
            return Ok(Import { module, kind: ImportKind::Module { alias }, span });
        }
        self.expect_kw(Kw::From)?;
        let module = self.parse_module_path()?;
        self.expect_kw(Kw::Import)?;
        let mut names = vec![self.parse_imported_name()?];
        while self.eat_sym(Sym::Comma) {
            names.push(self.parse_imported_name()?);
        }
        self.expect_newline()?;
        Ok(Import { module, kind: ImportKind::Names(names), span })
    }

    fn parse_imported_name(&mut self) -> Result<(String, Option<String>)> {
        if self.at(&Tok::Sym(Sym::Star)) {
            return Err(Diagnostic::error(self.span(), "`import *` is not allowed")
                .with_hint("import each name explicitly"));
        }
        Ok((self.expect_ident()?, self.parse_alias()?))
    }

    fn parse_module_path(&mut self) -> Result<Vec<String>> {
        let mut path = vec![self.expect_ident()?];
        while self.eat_sym(Sym::Dot) {
            path.push(self.expect_ident()?);
        }
        Ok(path)
    }

    fn parse_alias(&mut self) -> Result<Option<String>> {
        if self.eat_kw(Kw::As) { self.expect_ident().map(Some) } else { Ok(None) }
    }
}
