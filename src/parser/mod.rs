//! Recursive-descent parser: tokens to syntax tree.

mod decl;
mod expr;
mod item;
mod pattern;
mod stmt;

use crate::ast::{GenericParam, Param, ParsedFile, TypeExpr};
use crate::diag::{Diagnostic, Result, Span};
use crate::lexer::{Kw, Sym, Tok, Token};

pub fn parse(tokens: Vec<Token>) -> Result<ParsedFile> {
    Parser { tokens, pos: 0 }.parse_file()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    /// Parses `(a: T, b: U)`, with an optional trailing `...`.
    fn parse_params(&mut self) -> Result<(Vec<Param>, bool)> {
        self.expect_sym(Sym::LParen)?;
        let mut params = Vec::new();
        let mut variadic = false;
        while !self.eat_sym(Sym::RParen) {
            if !params.is_empty() || variadic {
                self.expect_sym(Sym::Comma)?;
            }
            if self.eat_sym(Sym::Ellipsis) {
                variadic = true;
                self.expect_sym(Sym::RParen)?;
                break;
            }
            params.push(self.parse_param()?);
        }
        Ok((params, variadic))
    }

    fn parse_param(&mut self) -> Result<Param> {
        let span = self.span();
        let owned = self.eat_kw(Kw::Owned);
        let name = self.expect_ident()?;
        self.expect_sym(Sym::Colon)?;
        let ty = self.parse_type()?;
        let default = if self.eat_sym(Sym::Assign) { Some(self.parse_expr()?) } else { None };
        Ok(Param { owned, name, ty, default, span })
    }

    fn parse_return_type(&mut self) -> Result<Option<TypeExpr>> {
        if self.eat_sym(Sym::Arrow) { self.parse_type().map(Some) } else { Ok(None) }
    }

    /// `[T: A + B, U]` after a function's name.
    fn parse_generics(&mut self) -> Result<Vec<GenericParam>> {
        let mut generics = Vec::new();
        if !self.eat_sym(Sym::LBracket) {
            return Ok(generics);
        }
        while !self.eat_sym(Sym::RBracket) {
            if !generics.is_empty() {
                self.expect_sym(Sym::Comma)?;
            }
            let span = self.span();
            let name = self.expect_ident()?;
            let mut bounds = Vec::new();
            if self.eat_sym(Sym::Colon) {
                loop {
                    let at = self.span();
                    bounds.push((self.expect_ident()?, at));
                    if !self.eat_sym(Sym::Plus) {
                        break;
                    }
                }
            }
            let default = if self.eat_sym(Sym::Assign) { Some(self.parse_type()?) } else { None };
            generics.push(GenericParam { name, bounds, default, span });
        }
        Ok(generics)
    }

    /// `raises A | B` after a signature.
    fn parse_raises(&mut self) -> Result<Vec<TypeExpr>> {
        let mut types = Vec::new();
        if self.eat_kw(Kw::Raises) {
            types.push(self.parse_type()?);
            while self.eat_sym(Sym::Pipe) {
                types.push(self.parse_type()?);
            }
        }
        Ok(types)
    }

    fn parse_optional_type(&mut self) -> Result<Option<TypeExpr>> {
        if self.eat_sym(Sym::Colon) { self.parse_type().map(Some) } else { Ok(None) }
    }

    /// Parses a type such as `i32`, `ptr[u8]`, `&mut Point` or `(i32, f64)`.
    fn parse_type(&mut self) -> Result<TypeExpr> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Sym(Sym::Amp) => {
                self.advance();
                let mutable = self.eat_kw(Kw::Mut);
                Ok(TypeExpr::Ref { mutable, inner: Box::new(self.parse_type()?), span })
            }
            Tok::Sym(Sym::LParen) => {
                self.advance();
                let mut items = vec![self.parse_type()?];
                while self.eat_sym(Sym::Comma) {
                    items.push(self.parse_type()?);
                }
                self.expect_sym(Sym::RParen)?;
                Ok(TypeExpr::Tuple(items, span))
            }
            Tok::Kw(Kw::Fn) => {
                self.advance();
                self.expect_sym(Sym::LParen)?;
                let mut params = Vec::new();
                while !self.eat_sym(Sym::RParen) {
                    if !params.is_empty() {
                        self.expect_sym(Sym::Comma)?;
                    }
                    params.push(self.parse_type()?);
                }
                let ret = if self.eat_sym(Sym::Arrow) { Some(Box::new(self.parse_type()?)) } else { None };
                Ok(TypeExpr::Fn { params, ret, span })
            }
            // The type of types, for the parameters of a `comptime fn`.
            Tok::Kw(Kw::Type) => {
                self.advance();
                Ok(TypeExpr::Named { name: "type".into(), args: Vec::new(), span })
            }
            Tok::Int(n) => {
                self.advance();
                let n = u64::try_from(n).map_err(|_| Diagnostic::error(span, "this size is too large"))?;
                Ok(TypeExpr::Size(n, span))
            }
            _ => {
                let name = self.expect_ident()?;
                if self.eat_sym(Sym::LParen) {
                    let mut args = Vec::new();
                    while !self.eat_sym(Sym::RParen) {
                        if !args.is_empty() {
                            self.expect_sym(Sym::Comma)?;
                        }
                        args.push(self.parse_type()?);
                    }
                    return Ok(TypeExpr::Call { name, args, span });
                }
                let args =
                    if self.at(&Tok::Sym(Sym::LBracket)) { self.parse_type_args()? } else { Vec::new() };
                Ok(TypeExpr::Named { name, args, span })
            }
        }
    }

    /// Parses `[T, U]`.
    fn parse_type_args(&mut self) -> Result<Vec<TypeExpr>> {
        self.expect_sym(Sym::LBracket)?;
        let mut args = vec![self.parse_type()?];
        while !self.eat_sym(Sym::RBracket) {
            self.expect_sym(Sym::Comma)?;
            args.push(self.parse_type()?);
        }
        Ok(args)
    }

    // --- token helpers ---

    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn at(&self, tok: &Tok) -> bool {
        self.peek() == tok
    }

    fn advance(&mut self) -> Tok {
        let tok = self.tokens[self.pos].tok.clone();
        if tok != Tok::Eof {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        let found = self.at(tok);
        if found {
            self.advance();
        }
        found
    }

    fn eat_sym(&mut self, sym: Sym) -> bool {
        self.eat(&Tok::Sym(sym))
    }

    fn eat_kw(&mut self, kw: Kw) -> bool {
        self.eat(&Tok::Kw(kw))
    }

    fn expect(&mut self, tok: Tok) -> Result<()> {
        if self.eat(&tok) { Ok(()) } else { Err(self.unexpected(&tok.describe())) }
    }

    fn expect_sym(&mut self, sym: Sym) -> Result<()> {
        self.expect(Tok::Sym(sym))
    }

    fn expect_kw(&mut self, kw: Kw) -> Result<()> {
        self.expect(Tok::Kw(kw))
    }

    fn expect_newline(&mut self) -> Result<()> {
        self.expect(Tok::Newline)
    }

    fn expect_ident(&mut self) -> Result<String> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                self.advance();
                Ok(name)
            }
            _ => Err(self.unexpected("a name")),
        }
    }

    fn expect_str(&mut self) -> Result<String> {
        match self.peek().clone() {
            Tok::Str(text) => {
                self.advance();
                Ok(text)
            }
            _ => Err(self.unexpected("a string")),
        }
    }

    fn unexpected(&self, expected: &str) -> Diagnostic {
        Diagnostic::error(self.span(), format!("expected {expected}, found {}", self.peek().describe()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ImportKind, Item, StmtKind};
    use crate::lexer::lex;

    fn parse_source(source: &str) -> Result<ParsedFile> {
        parse(lex(source, 0)?)
    }

    #[test]
    fn parses_function_with_body() {
        let file = parse_source("manual fn main():\n    mut x: i32 = 1 + 2 * 3\n    x += 1\n").unwrap();
        let Item::Fn(f) = &file.items[0] else { panic!("expected a function") };
        assert_eq!(f.name, "main");
        assert!(matches!(f.body[0].kind, StmtKind::Var { mutable: true, .. }));
        assert!(matches!(f.body[1].kind, StmtKind::Assign { op: Some(_), .. }));
    }

    #[test]
    fn parses_variadic_extern() {
        let file = parse_source("extern fn printf(fmt: ptr[u8], ...) -> i32\n").unwrap();
        let Item::Extern(e) = &file.items[0] else { panic!("expected an extern") };
        assert!(e.variadic);
        assert!(
            matches!(&e.params[0].ty, TypeExpr::Named { name, args, .. } if name == "ptr" && args.len() == 1)
        );
    }

    #[test]
    fn parses_imports() {
        let file = parse_source("import net.http as web\nfrom util import a, b as c\n").unwrap();
        assert_eq!(file.imports[0].module, ["net", "http"]);
        let ImportKind::Names(names) = &file.imports[1].kind else { panic!("expected names") };
        assert_eq!(names[1], ("b".to_string(), Some("c".to_string())));
    }

    #[test]
    fn parses_comptime_items() {
        let source = "comptime N: size = 4\ncomptime if target.os == \"linux\":\n    extern fn f() -> i32\n";
        let file = parse_source(source).unwrap();
        assert!(matches!(file.items[0], Item::Comptime(_)));
        assert!(matches!(file.items[1], Item::ComptimeIf(_)));
    }

    #[test]
    fn rejects_chained_comparisons() {
        assert!(parse_source("manual fn main():\n    const b: bool = 1 < 2 < 3\n").is_err());
    }
}
