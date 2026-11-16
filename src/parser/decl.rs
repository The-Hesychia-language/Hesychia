//! Struct and enum declarations, with their fields, variants and methods.

use super::Parser;
use crate::ast::{
    EnumDecl, ExtendDecl, FieldDecl, FnDecl, Level, Param, Receiver, StructDecl, TraitDecl, TraitMethod,
    VariantDecl,
};
use crate::diag::{Diagnostic, Result, Span};
use crate::lexer::{Kw, Sym, Tok};

impl Parser {
    /// A struct after `struct`: an indented list of fields and methods.
    pub(super) fn parse_struct(&mut self, level: Level, private: bool, span: Span) -> Result<StructDecl> {
        let name = self.expect_ident()?;
        let generics = self.parse_generics()?;
        let traits = self.parse_trait_list()?;
        if level == Level::Managed && !traits.is_empty() {
            return Err(Diagnostic::error(span, "`managed` structs do not implement traits")
                .with_hint("declare a `manual struct` to implement traits"));
        }
        let (mut fields, mut methods) = (Vec::new(), Vec::new());
        self.open_body()?;
        while !self.eat(&Tok::Dedent) {
            if self.at(&Tok::Kw(Kw::Method)) || self.at_private_method() {
                methods.push(self.parse_method(level)?);
            } else {
                fields.push(self.parse_field()?);
                self.expect_newline()?;
            }
        }
        Ok(StructDecl {
            level,
            private,
            name,
            generics,
            traits,
            fields,
            methods,
            decorators: Vec::new(),
            span,
        })
    }

    /// `(TraitA, TraitB)` after a struct's name.
    fn parse_trait_list(&mut self) -> Result<Vec<(String, Span)>> {
        let mut traits = Vec::new();
        if self.eat_sym(Sym::LParen) {
            while !self.eat_sym(Sym::RParen) {
                if !traits.is_empty() {
                    self.expect_sym(Sym::Comma)?;
                }
                let span = self.span();
                traits.push((self.expect_ident()?, span));
            }
        }
        Ok(traits)
    }

    /// `extend` is a word, not a keyword, so that `v.extend(w)` stays a method name.
    pub(super) fn at_extend(&self) -> bool {
        matches!(self.peek(), Tok::Ident(word) if word == "extend")
            && matches!(self.tokens.get(self.pos + 1).map(|t| &t.tok), Some(Tok::Ident(_)))
    }

    /// `extend vector[T: Order]:` followed by `manual` methods.
    pub(super) fn parse_extend(&mut self) -> Result<ExtendDecl> {
        let span = self.span();
        self.advance();
        let ty = self.expect_ident()?;
        let generics = self.parse_generics()?;
        self.open_body()?;
        let mut methods = Vec::new();
        while !self.eat(&Tok::Dedent) {
            methods.push(self.parse_method(Level::Manual)?);
        }
        Ok(ExtendDecl { ty, generics, methods, span })
    }

    /// `trait Name:` followed by method signatures, each with or without a default body.
    pub(super) fn parse_trait(&mut self, private: bool) -> Result<TraitDecl> {
        let span = self.span();
        self.expect_kw(Kw::Trait)?;
        let name = self.expect_ident()?;
        self.open_body()?;
        let mut methods = Vec::new();
        while !self.eat(&Tok::Dedent) {
            let (decl, default) = self.parse_method_header(Level::Manual)?;
            methods.push(TraitMethod { decl, default });
        }
        Ok(TraitDecl { private, name, methods, span })
    }

    /// An enum after `enum`: one variant per line, with optional fields.
    pub(super) fn parse_enum(&mut self, level: Level, private: bool, span: Span) -> Result<EnumDecl> {
        let name = self.expect_ident()?;
        let generics = self.parse_generics()?;
        let mut variants = Vec::new();
        self.open_body()?;
        while !self.eat(&Tok::Dedent) {
            let span = self.span();
            let name = self.expect_ident()?;
            let mut fields = Vec::new();
            if self.eat_sym(Sym::LParen) {
                while !self.eat_sym(Sym::RParen) {
                    if !fields.is_empty() {
                        self.expect_sym(Sym::Comma)?;
                    }
                    fields.push(self.parse_field()?);
                }
            }
            self.expect_newline()?;
            variants.push(VariantDecl { name, fields, span });
        }
        Ok(EnumDecl { level, private, name, generics, variants, span })
    }

    fn open_body(&mut self) -> Result<()> {
        self.expect_sym(Sym::Colon)?;
        self.expect_newline()?;
        self.expect(Tok::Indent)
    }

    fn at_private_method(&self) -> bool {
        self.at(&Tok::Kw(Kw::Priv))
            && matches!(self.tokens.get(self.pos + 1).map(|t| &t.tok), Some(Tok::Kw(Kw::Method)))
    }

    fn parse_field(&mut self) -> Result<FieldDecl> {
        let span = self.span();
        let private = self.eat_kw(Kw::Priv);
        let name = self.expect_ident()?;
        self.expect_sym(Sym::Colon)?;
        let ty = self.parse_type()?;
        Ok(FieldDecl { private, name, ty, span })
    }

    /// `method name(self-or-params) [-> T]: body`; the method takes the struct's level.
    fn parse_method(&mut self, level: Level) -> Result<FnDecl> {
        let span = self.span();
        let private = self.eat_kw(Kw::Priv);
        self.expect_kw(Kw::Method)?;
        let name = self.expect_ident()?;
        let generics = self.parse_generics()?;
        let receiver = self.parse_receiver()?;
        let params = self.parse_params_after_receiver(receiver.is_some())?;
        let ret = self.parse_return_type()?;
        let raises = self.parse_raises()?;
        let body = self.parse_block()?;
        let captures = Vec::new();
        let decorators = Vec::new();
        Ok(FnDecl {
            level,
            private,
            name,
            generics,
            captures,
            receiver,
            params,
            ret,
            raises,
            body,
            decorators,
            span,
        })
    }

    /// A trait method: its signature, and its body when it has a default one.
    fn parse_method_header(&mut self, level: Level) -> Result<(FnDecl, bool)> {
        let span = self.span();
        self.expect_kw(Kw::Method)?;
        let name = self.expect_ident()?;
        let receiver = self.parse_receiver()?;
        let params = self.parse_params_after_receiver(receiver.is_some())?;
        let ret = self.parse_return_type()?;
        let raises = self.parse_raises()?;
        let default = !self.at(&Tok::Newline);
        let body = if default { self.parse_block()? } else { self.expect_newline().map(|()| Vec::new())? };
        let decl = FnDecl {
            level,
            private: false,
            name,
            generics: Vec::new(),
            captures: Vec::new(),
            receiver,
            params,
            ret,
            raises,
            body,
            decorators: Vec::new(),
            span,
        };
        Ok((decl, default))
    }

    /// Reads `(` and an optional `self`, `&self`, `&mut self` or `owned self`.
    fn parse_receiver(&mut self) -> Result<Option<Receiver>> {
        self.expect_sym(Sym::LParen)?;
        let at = |offset: usize| self.tokens.get(self.pos + offset).map(|t| &t.tok);
        let self_at = |offset: usize| at(offset) == Some(&Tok::Kw(Kw::SelfValue));
        let (receiver, length) = match (self.peek(), at(1)) {
            (Tok::Kw(Kw::SelfValue), _) => (Receiver::Plain, 1),
            (Tok::Kw(Kw::Owned), _) if self_at(1) => (Receiver::Owned, 2),
            (Tok::Sym(Sym::Amp), _) if self_at(1) => (Receiver::Shared, 2),
            (Tok::Sym(Sym::Amp), Some(Tok::Kw(Kw::Mut))) if self_at(2) => (Receiver::Mut, 3),
            _ => return Ok(None),
        };
        for _ in 0..length {
            self.advance();
        }
        Ok(Some(receiver))
    }

    /// The parameters after the receiver, up to and including `)`.
    fn parse_params_after_receiver(&mut self, has_receiver: bool) -> Result<Vec<Param>> {
        let mut params = Vec::new();
        while !self.eat_sym(Sym::RParen) {
            if has_receiver || !params.is_empty() {
                self.expect_sym(Sym::Comma)?;
            }
            params.push(self.parse_param()?);
        }
        Ok(params)
    }
}
