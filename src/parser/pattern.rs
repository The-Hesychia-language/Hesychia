//! `match` statements and their patterns.

use super::Parser;
use crate::ast::{Case, Catch, Expr, ExprKind, Handler, Pattern, StmtKind};
use crate::diag::{Diagnostic, Result};
use crate::lexer::{Kw, Sym, Tok};

impl Parser {
    /// `match subject:` followed by an indented list of `case pattern: body`.
    pub(super) fn parse_match(&mut self) -> Result<StmtKind> {
        self.advance();
        let subject = self.parse_expr()?;
        self.expect_sym(Sym::Colon)?;
        self.expect(Tok::Newline)?;
        self.expect(Tok::Indent)?;
        let mut cases = Vec::new();
        while !self.eat(&Tok::Dedent) {
            let span = self.span();
            self.expect_kw(Kw::Case)?;
            let pattern = self.parse_pattern()?;
            let guard = if self.eat(&Tok::Kw(Kw::If)) { Some(self.parse_expr()?) } else { None };
            let body = self.parse_block()?;
            cases.push(Case { pattern, guard, body, span });
        }
        Ok(StmtKind::Match { subject, cases, borrowed: false })
    }

    fn parse_pattern(&mut self) -> Result<Pattern> {
        let span = self.span();
        let first = self.parse_single_pattern()?;
        if !self.at(&Tok::Sym(Sym::Pipe)) {
            return Ok(first);
        }
        let mut options = vec![first];
        while self.eat_sym(Sym::Pipe) {
            options.push(self.parse_single_pattern()?);
        }
        Ok(Pattern::Or(options, span))
    }

    fn parse_single_pattern(&mut self) -> Result<Pattern> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Kw(Kw::None) => {
                self.advance();
                Ok(Pattern::None(span))
            }
            Tok::Ident(name) if name == "_" => {
                self.advance();
                Ok(Pattern::Wildcard(span))
            }
            Tok::Ident(_) => self.parse_path_pattern(),
            Tok::Int(_)
            | Tok::Float(_)
            | Tok::Char(_)
            | Tok::Kw(Kw::True | Kw::False)
            | Tok::Sym(Sym::Minus) => Ok(Pattern::Literal(self.parse_literal_pattern()?)),
            other => Err(Diagnostic::error(span, format!("expected a pattern, found {}", other.describe()))),
        }
    }

    /// `name`, `Type.Variant` or `Type.Variant(a, b)`.
    fn parse_path_pattern(&mut self) -> Result<Pattern> {
        let span = self.span();
        let mut path = vec![self.expect_ident()?];
        while self.eat_sym(Sym::Dot) {
            path.push(self.expect_ident()?);
        }
        let bindings = if self.eat_sym(Sym::LParen) {
            let mut names = Vec::new();
            while !self.eat_sym(Sym::RParen) {
                names.push(self.expect_ident()?);
                if !self.eat_sym(Sym::Comma) {
                    self.expect_sym(Sym::RParen)?;
                    break;
                }
            }
            Some(names)
        } else {
            None
        };
        if bindings.is_none() && self.eat_kw(Kw::As) {
            let name = Some(self.expect_ident()?);
            return Ok(Pattern::ErrorType { path, name, index: None, span });
        }
        if path.len() == 1 && bindings.is_none() {
            return Ok(Pattern::Bind(path.remove(0), span));
        }
        Ok(Pattern::Variant { path, bindings, variant: None, span })
    }

    fn parse_literal_pattern(&mut self) -> Result<Expr> {
        let span = self.span();
        let negative = self.eat_sym(Sym::Minus);
        let kind = match self.advance() {
            Tok::Int(value) => {
                let value = i128::try_from(value)
                    .map_err(|_| Diagnostic::error(span, "integer literal is too large"))?;
                ExprKind::Int(if negative { -value } else { value })
            }
            Tok::Float(value) => ExprKind::Float(if negative { -value } else { value }),
            Tok::Char(c) if !negative => ExprKind::Char(c),
            Tok::Kw(Kw::True) if !negative => ExprKind::Bool(true),
            Tok::Kw(Kw::False) if !negative => ExprKind::Bool(false),
            other => {
                return Err(Diagnostic::error(
                    span,
                    format!("expected a literal, found {}", other.describe()),
                ));
            }
        };
        Ok(Expr::new(kind, span))
    }

    /// `try:` followed by one or more `except ...:` clauses.
    pub(super) fn parse_try(&mut self) -> Result<StmtKind> {
        self.advance();
        let body = self.parse_block()?;
        let mut handlers = Vec::new();
        while self.at(&Tok::Kw(Kw::Except)) {
            let span = self.span();
            self.advance();
            let catch = self.parse_catch()?;
            let body = self.parse_block()?;
            handlers.push(Handler { catch, body, span });
        }
        if handlers.is_empty() {
            return Err(Diagnostic::error(self.span(), "a `try` needs at least one `except` clause"));
        }
        Ok(StmtKind::Try { body, handlers, errors: Vec::new() })
    }

    /// `T as e`, `T.Variant`, `T.Variant(a, b)` or `e`.
    fn parse_catch(&mut self) -> Result<Catch> {
        let start = self.pos;
        let mut path = vec![self.expect_ident()?];
        while self.eat_sym(Sym::Dot) {
            path.push(self.expect_ident()?);
        }
        if self.eat_kw(Kw::As) {
            let name = self.expect_ident()?;
            return Ok(Catch::Type { path, name, error: None });
        }
        if path.len() == 1 && !self.at(&Tok::Sym(Sym::LParen)) {
            return Ok(Catch::All { name: path.remove(0), error: None, several: Vec::new() });
        }
        self.pos = start;
        let pattern = self.parse_path_pattern()?;
        Ok(Catch::Variant { pattern, error: None })
    }
}
