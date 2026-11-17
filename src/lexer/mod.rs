//! Lexer: turns source text into tokens, including INDENT/DEDENT for blocks.

mod token;

pub use token::{FormatPart, Kw, Sym, Tok, Token};

use crate::diag::{Diagnostic, Result, Span};

pub fn lex(source: &str, file: usize) -> Result<Vec<Token>> {
    Lexer::new(source, file).run()
}

struct Lexer {
    file: usize,
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
    tokens: Vec<Token>,
    indents: Vec<usize>,
    indent_char: Option<char>,
    bracket_depth: usize,
    line_start: bool,
    symbols: Vec<Sym>,
    /// Comment lines not yet followed by code or a blank line.
    doc: Vec<String>,
    /// A token was read on the current line.
    line_has_token: bool,
}

impl Lexer {
    fn new(source: &str, file: usize) -> Self {
        Self {
            file,
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            tokens: Vec::new(),
            indents: vec![0],
            indent_char: None,
            bracket_depth: 0,
            line_start: true,
            symbols: Sym::all_longest_first(),
            doc: Vec::new(),
            line_has_token: false,
        }
    }

    fn run(mut self) -> Result<Vec<Token>> {
        loop {
            if self.line_start && self.bracket_depth == 0 {
                self.line_start = false;
                self.lex_indentation()?;
            }
            let Some(c) = self.peek() else { break };
            match c {
                '\n' => self.lex_newline(),
                ' ' | '\t' | '\r' => {
                    self.advance();
                }
                '#' => self.skip_comment(),
                _ => self.lex_token(c)?,
            }
        }
        Ok(self.finish())
    }

    fn lex_newline(&mut self) {
        let span = self.span();
        self.advance();
        self.line_has_token = false;
        if self.bracket_depth == 0 && self.needs_newline() {
            self.push(Tok::Newline, span);
        }
        self.line_start = true;
    }

    fn needs_newline(&self) -> bool {
        !matches!(self.tokens.last().map(|t| &t.tok), None | Some(Tok::Newline | Tok::Indent | Tok::Dedent))
    }

    /// A comment alone on its line may document what follows it.
    fn skip_comment(&mut self) {
        let mut text = String::new();
        while let Some(c) = self.peek().filter(|&c| c != '\n') {
            text.push(c);
            self.advance();
        }
        if !self.line_has_token && self.bracket_depth == 0 {
            let text = text.trim_start_matches('#');
            self.doc.push(text.strip_prefix(' ').unwrap_or(text).trim_end().to_string());
        }
    }

    /// Measures the indentation of a new line and emits INDENT/DEDENT tokens.
    fn lex_indentation(&mut self) -> Result<()> {
        let span = self.span();
        let mut width = 0;
        let mut used = None;
        while let Some(c @ (' ' | '\t')) = self.peek() {
            self.check_indent_char(c, used, span)?;
            used = Some(c);
            width += 1;
            self.advance();
        }
        if matches!(self.peek(), None | Some('\n' | '#' | '\r')) {
            if self.peek() != Some('#') {
                // A blank line parts a comment from the code below it.
                self.doc.clear();
            }
            return Ok(()); // blank or comment-only line: no block change
        }
        if let Some(c) = used {
            self.indent_char = Some(c);
        }
        self.apply_indent(width, span)
    }

    fn check_indent_char(&self, c: char, used: Option<char>, span: Span) -> Result<()> {
        let expected = used.or(self.indent_char);
        match expected {
            Some(e) if e != c => Err(Diagnostic::error(span, "mixed tabs and spaces in indentation")
                .with_hint("indent the whole file with either tabs or spaces")),
            _ => Ok(()),
        }
    }

    fn apply_indent(&mut self, width: usize, span: Span) -> Result<()> {
        let top = *self.indents.last().unwrap_or(&0);
        if width > top {
            self.indents.push(width);
            self.push(Tok::Indent, span);
            return Ok(());
        }
        while width < *self.indents.last().unwrap_or(&0) {
            self.indents.pop();
            self.push(Tok::Dedent, span);
        }
        if width != *self.indents.last().unwrap_or(&0) {
            return Err(Diagnostic::error(span, "this line does not match any outer indentation level"));
        }
        Ok(())
    }

    fn lex_token(&mut self, c: char) -> Result<()> {
        let span = self.span();
        let tok = if c == '"' {
            self.lex_string(span)?
        } else if c == '\'' {
            self.lex_char(span)?
        } else if c.is_ascii_digit() {
            self.lex_number(span)?
        } else if c.is_ascii_alphabetic() || c == '_' {
            self.lex_word(span)?
        } else {
            self.lex_symbol(span)?
        };
        self.push(tok, span);
        Ok(())
    }

    fn lex_word(&mut self, span: Span) -> Result<Tok> {
        let mut word = String::new();
        while let Some(c) = self.peek().filter(|c| c.is_ascii_alphanumeric() || *c == '_') {
            word.push(c);
            self.advance();
        }
        if self.peek() == Some('"') && word == "f" {
            return self.lex_format_string(span);
        }
        if self.peek() == Some('"') {
            return Err(Diagnostic::error(span, format!("string prefix `{word}` is not supported yet")));
        }
        if self.peek().is_some_and(char::is_alphabetic) {
            return Err(Diagnostic::error(self.span(), "identifiers must use ASCII letters"));
        }
        Ok(Kw::from_word(&word).map_or(Tok::Ident(word), Tok::Kw))
    }

    fn lex_number(&mut self, span: Span) -> Result<Tok> {
        let radix = match (self.peek(), self.peek_at(1)) {
            (Some('0'), Some('x')) => 16,
            (Some('0'), Some('b')) => 2,
            (Some('0'), Some('o')) => 8,
            _ => 10,
        };
        let tok = if radix == 10 { self.lex_decimal(span)? } else { self.lex_radix(radix, span)? };
        if self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(Diagnostic::error(self.span(), "invalid character in number literal"));
        }
        Ok(tok)
    }

    fn lex_radix(&mut self, radix: u32, span: Span) -> Result<Tok> {
        self.advance();
        self.advance();
        let digits = self.take_digits(|c| c.is_digit(radix));
        u128::from_str_radix(&digits, radix)
            .map(Tok::Int)
            .map_err(|_| Diagnostic::error(span, "invalid or too large integer literal"))
    }

    fn lex_decimal(&mut self, span: Span) -> Result<Tok> {
        let mut text = self.take_digits(|c| c.is_ascii_digit());
        let mut is_float = false;
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            self.advance();
            text.push('.');
            text += &self.take_digits(|c| c.is_ascii_digit());
            is_float = true;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            text += &self.lex_exponent(span)?;
            is_float = true;
        }
        if is_float {
            return text
                .parse()
                .map(Tok::Float)
                .map_err(|_| Diagnostic::error(span, "invalid float literal"));
        }
        text.parse().map(Tok::Int).map_err(|_| Diagnostic::error(span, "integer literal is too large"))
    }

    fn lex_exponent(&mut self, span: Span) -> Result<String> {
        let mut text = String::from("e");
        self.advance();
        if let Some(sign @ ('+' | '-')) = self.peek() {
            text.push(sign);
            self.advance();
        }
        let digits = self.take_digits(|c| c.is_ascii_digit());
        if digits.is_empty() {
            return Err(Diagnostic::error(span, "missing digits after the exponent"));
        }
        Ok(text + &digits)
    }

    /// Reads digits accepted by `accept`, skipping `_` separators.
    fn take_digits(&mut self, accept: impl Fn(char) -> bool) -> String {
        let mut digits = String::new();
        while let Some(c) = self.peek().filter(|&c| accept(c) || c == '_') {
            if c != '_' {
                digits.push(c);
            }
            self.advance();
        }
        digits
    }

    fn lex_string(&mut self, span: Span) -> Result<Tok> {
        self.advance();
        let mut text = String::new();
        loop {
            match self.peek() {
                None | Some('\n') => return Err(Diagnostic::error(span, "unterminated string")),
                Some('"') => break,
                Some('\\') => text.push(self.lex_escape()?),
                Some(c) => {
                    text.push(c);
                    self.advance();
                }
            }
        }
        self.advance();
        Ok(Tok::Str(text))
    }

    /// `f"text {expr} text"`: text pieces and the source of each embedded expression.
    fn lex_format_string(&mut self, span: Span) -> Result<Tok> {
        self.advance();
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            match self.peek() {
                None | Some('\n') => return Err(Diagnostic::error(span, "unterminated string")),
                Some('"') => break,
                Some('\\') => text.push(self.lex_escape()?),
                Some('{') if self.peek_at(1) == Some('{') => {
                    self.advance();
                    self.advance();
                    text.push('{');
                }
                Some('}') if self.peek_at(1) == Some('}') => {
                    self.advance();
                    self.advance();
                    text.push('}');
                }
                Some('{') => {
                    parts.push(FormatPart::Text(std::mem::take(&mut text)));
                    let at = self.span();
                    self.advance();
                    let (source, spec) = self.lex_embedded(span)?;
                    parts.push(FormatPart::Expr(source, at, spec));
                }
                Some('}') => return Err(Diagnostic::error(self.span(), "a single `}` must be written `}}`")),
                Some(c) => {
                    text.push(c);
                    self.advance();
                }
            }
        }
        self.advance();
        parts.push(FormatPart::Text(text));
        Ok(Tok::Format(parts))
    }

    /// The source of an embedded expression, up to its closing `}`, and the format written
    /// after a `:`, as in `{x:.2f}`.
    fn lex_embedded(&mut self, span: Span) -> Result<(String, Option<String>)> {
        let mut source = String::new();
        let mut depth = 0;
        loop {
            match self.advance() {
                None | Some('\n') => return Err(Diagnostic::error(span, "unterminated `{` in an f-string")),
                // A string inside the expression, as in `f"{name == "Ada"}"`.
                Some('"') => {
                    source.push('"');
                    loop {
                        match self.advance() {
                            None | Some('\n') => return Err(Diagnostic::error(span, "unterminated string")),
                            Some('"') => break,
                            Some('\\') => {
                                source.push('\\');
                                source.extend(self.advance());
                            }
                            Some(c) => source.push(c),
                        }
                    }
                    source.push('"');
                }
                Some('}') if depth == 0 => return Ok((source, None)),
                Some(':') if depth == 0 => {
                    let mut spec = String::new();
                    loop {
                        match self.advance() {
                            None | Some('\n') => {
                                return Err(Diagnostic::error(span, "unterminated `{` in an f-string"));
                            }
                            Some('}') => return Ok((source, Some(spec))),
                            Some(c) => spec.push(c),
                        }
                    }
                }
                Some(c) => {
                    match c {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' => depth -= 1,
                        _ => {}
                    }
                    source.push(c);
                }
            }
        }
    }

    fn lex_char(&mut self, span: Span) -> Result<Tok> {
        self.advance();
        let c = match self.peek() {
            Some('\\') => self.lex_escape()?,
            Some(c) if c != '\'' && c != '\n' => {
                self.advance();
                c
            }
            _ => return Err(Diagnostic::error(span, "empty character literal")),
        };
        if self.peek() != Some('\'') {
            return Err(Diagnostic::error(span, "character literal must contain exactly one character"));
        }
        self.advance();
        Ok(Tok::Char(c))
    }

    fn lex_escape(&mut self) -> Result<char> {
        let span = self.span();
        self.advance();
        let c = self.advance().ok_or_else(|| Diagnostic::error(span, "unterminated escape"))?;
        match c {
            'n' => Ok('\n'),
            't' => Ok('\t'),
            'r' => Ok('\r'),
            '0' => Ok('\0'),
            '\\' | '"' | '\'' => Ok(c),
            'x' => self.lex_hex_escape(2, span),
            'u' => self.lex_unicode_escape(span),
            _ => Err(Diagnostic::error(span, format!("unknown escape `\\{c}`"))),
        }
    }

    fn lex_hex_escape(&mut self, len: usize, span: Span) -> Result<char> {
        let digits: String = (0..len).filter_map(|_| self.advance()).collect();
        u32::from_str_radix(&digits, 16)
            .ok()
            .filter(|_| digits.len() == len)
            .and_then(char::from_u32)
            .ok_or_else(|| Diagnostic::error(span, "invalid `\\x` escape"))
    }

    fn lex_unicode_escape(&mut self, span: Span) -> Result<char> {
        let invalid = || Diagnostic::error(span, "invalid `\\u{...}` escape");
        if self.advance() != Some('{') {
            return Err(invalid());
        }
        let digits = self.take_digits(|c| c.is_ascii_hexdigit());
        if self.advance() != Some('}') {
            return Err(invalid());
        }
        u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32).ok_or_else(invalid)
    }

    fn lex_symbol(&mut self, span: Span) -> Result<Tok> {
        let sym =
            self.symbols.iter().copied().find(|sym| self.matches_text(sym.text())).ok_or_else(|| {
                Diagnostic::error(span, format!("unexpected character `{}`", self.chars[self.pos]))
            })?;
        for _ in 0..sym.text().len() {
            self.advance();
        }
        self.track_brackets(sym);
        Ok(Tok::Sym(sym))
    }

    fn matches_text(&self, text: &str) -> bool {
        text.chars().enumerate().all(|(i, c)| self.peek_at(i) == Some(c))
    }

    fn track_brackets(&mut self, sym: Sym) {
        match sym {
            Sym::LParen | Sym::LBracket | Sym::LBrace => self.bracket_depth += 1,
            Sym::RParen | Sym::RBracket | Sym::RBrace => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1)
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Token> {
        let span = self.span();
        if self.needs_newline() {
            self.push(Tok::Newline, span);
        }
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push(Tok::Dedent, span);
        }
        self.push(Tok::Eof, span);
        self.tokens
    }

    fn push(&mut self, tok: Tok, span: Span) {
        let mut doc = None;
        if !matches!(tok, Tok::Newline | Tok::Indent | Tok::Dedent) {
            if !self.doc.is_empty() {
                doc = Some(std::mem::take(&mut self.doc).join("\n"));
            }
            self.line_has_token = true;
        }
        self.tokens.push(Token { tok, span, doc });
    }

    fn span(&self) -> Span {
        Span::new(self.file, self.line, self.col)
    }

    fn peek(&self) -> Option<char> {
        self.peek_at(0)
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<Tok> {
        lex(source, 0).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn indentation_produces_blocks() {
        let toks = kinds("if x:\n    y\nz\n");
        assert_eq!(
            toks,
            vec![
                Tok::Kw(Kw::If),
                Tok::Ident("x".into()),
                Tok::Sym(Sym::Colon),
                Tok::Newline,
                Tok::Indent,
                Tok::Ident("y".into()),
                Tok::Newline,
                Tok::Dedent,
                Tok::Ident("z".into()),
                Tok::Newline,
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn numbers_and_symbols() {
        let toks = kinds("0xFF 1_000 2.5e3 a //= b\n");
        assert_eq!(toks[0], Tok::Int(255));
        assert_eq!(toks[1], Tok::Int(1000));
        assert_eq!(toks[2], Tok::Float(2500.0));
        assert_eq!(toks[4], Tok::Sym(Sym::SlashSlashEq));
    }

    #[test]
    fn mixed_indentation_is_rejected() {
        assert!(lex("if a:\n    b\nif c:\n\td\n", 0).is_err());
    }

    #[test]
    fn newlines_inside_brackets_are_ignored() {
        let toks = kinds("f(a,\n  b)\n");
        assert!(!toks[..toks.len() - 2].contains(&Tok::Newline));
    }
}
