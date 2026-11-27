//! Expressions, by increasing precedence.

use super::Parser;
use crate::ast::{BinOp, Capture, Expr, ExprKind, TypeExpr, UnOp};
use crate::diag::{Diagnostic, Result, Span};
use crate::lexer::{FormatPart, Kw, Sym, Tok};

/// Binary operator levels between comparisons and unary operators, loosest first.
const BINARY_LEVELS: &[&[(Sym, BinOp)]] = &[
    &[(Sym::Pipe, BinOp::BitOr)],
    &[(Sym::Caret, BinOp::BitXor)],
    &[(Sym::Amp, BinOp::BitAnd)],
    &[(Sym::Shl, BinOp::Shl), (Sym::Shr, BinOp::Shr)],
    &[(Sym::Plus, BinOp::Add), (Sym::Minus, BinOp::Sub)],
    &[
        (Sym::Star, BinOp::Mul),
        (Sym::Slash, BinOp::Div),
        (Sym::SlashSlash, BinOp::FloorDiv),
        (Sym::Percent, BinOp::Mod),
    ],
];

const COMPARISONS: &[(Sym, BinOp)] = &[
    (Sym::EqEq, BinOp::Eq),
    (Sym::NotEq, BinOp::Ne),
    (Sym::Lt, BinOp::Lt),
    (Sym::Le, BinOp::Le),
    (Sym::Gt, BinOp::Gt),
    (Sym::Ge, BinOp::Ge),
];

impl Parser {
    pub(super) fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_logic(Kw::Or, BinOp::Or)
    }

    /// `or` binds looser than `and`, which binds looser than `not`.
    fn parse_logic(&mut self, kw: Kw, op: BinOp) -> Result<Expr> {
        let mut left = self.parse_logic_operand(kw)?;
        while self.at(&Tok::Kw(kw)) {
            let span = self.span();
            self.advance();
            let right = self.parse_logic_operand(kw)?;
            left = Expr::new(ExprKind::Binary(op, Box::new(left), Box::new(right)), span);
        }
        Ok(left)
    }

    fn parse_logic_operand(&mut self, kw: Kw) -> Result<Expr> {
        if kw == Kw::Or { self.parse_logic(Kw::And, BinOp::And) } else { self.parse_not() }
    }

    fn parse_not(&mut self) -> Result<Expr> {
        let span = self.span();
        if self.eat_kw(Kw::Not) {
            let operand = self.parse_not()?;
            return Ok(Expr::new(ExprKind::Unary(UnOp::Not, Box::new(operand)), span));
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        let left = self.parse_binary(0)?;
        let span = self.span();
        let not_in = self.at(&Tok::Kw(Kw::Not))
            && self.tokens.get(self.pos + 1).is_some_and(|t| t.tok == Tok::Kw(Kw::In));
        let op = if self.eat_kw(Kw::Is) {
            if self.eat_kw(Kw::Not) { BinOp::IsNot } else { BinOp::Is }
        } else if self.eat_kw(Kw::In) {
            BinOp::In
        } else if not_in {
            self.advance();
            self.advance();
            BinOp::NotIn
        } else {
            let Some(op) = self.match_op(COMPARISONS) else {
                return Ok(left);
            };
            self.advance();
            op
        };
        let right = self.parse_binary(0)?;
        if self.match_op(COMPARISONS).is_some() {
            return Err(Diagnostic::error(self.span(), "comparisons cannot be chained")
                .with_hint("write `a < b and b < c`"));
        }
        Ok(Expr::new(ExprKind::Binary(op, Box::new(left), Box::new(right)), span))
    }

    fn parse_binary(&mut self, level: usize) -> Result<Expr> {
        let Some(ops) = BINARY_LEVELS.get(level) else {
            return self.parse_unary();
        };
        let mut left = self.parse_binary(level + 1)?;
        while let Some(op) = self.match_op(ops) {
            let span = self.span();
            self.advance();
            let right = self.parse_binary(level + 1)?;
            left = Expr::new(ExprKind::Binary(op, Box::new(left), Box::new(right)), span);
        }
        Ok(left)
    }

    fn match_op(&self, ops: &[(Sym, BinOp)]) -> Option<BinOp> {
        let Tok::Sym(sym) = self.peek() else {
            return None;
        };
        ops.iter().find(|(s, _)| s == sym).map(|(_, op)| *op)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        let span = self.span();
        let op = match self.peek() {
            Tok::Sym(Sym::Minus) => UnOp::Neg,
            Tok::Sym(Sym::Tilde) => UnOp::BitNot,
            Tok::Sym(Sym::Amp) => return self.parse_addr_of(),
            Tok::Kw(Kw::Check) => {
                self.advance();
                let call = self.parse_unary()?;
                return Ok(Expr::new(ExprKind::Check { call: Box::new(call), errors: Vec::new() }, span));
            }
            _ => return self.parse_power(),
        };
        self.advance();
        let operand = self.parse_unary()?;
        Ok(Expr::new(ExprKind::Unary(op, Box::new(operand)), span))
    }

    /// `&x` or `&mut x`.
    fn parse_addr_of(&mut self) -> Result<Expr> {
        let span = self.span();
        self.advance();
        let mutable = self.eat_kw(Kw::Mut);
        let inner = self.parse_unary()?;
        Ok(Expr::new(ExprKind::AddrOf { mutable, inner: Box::new(inner) }, span))
    }

    /// `**` is right-associative and binds tighter than a unary minus on its left.
    fn parse_power(&mut self) -> Result<Expr> {
        let base = self.parse_postfix()?;
        if !self.at(&Tok::Sym(Sym::StarStar)) {
            return Ok(base);
        }
        let span = self.span();
        self.advance();
        let exponent = self.parse_unary()?;
        Ok(Expr::new(ExprKind::Binary(BinOp::Pow, Box::new(base), Box::new(exponent)), span))
    }

    /// `[i]`, or a slice `[a:b]` whose ends may be left out, after `[`.
    fn parse_index(&mut self, base: Expr, span: Span) -> Result<Expr> {
        let bound = |p: &mut Self| -> Result<Option<Box<Expr>>> {
            let at_end = p.at(&Tok::Sym(Sym::Colon)) || p.at(&Tok::Sym(Sym::RBracket));
            if at_end { Ok(None) } else { p.parse_expr().map(|e| Some(Box::new(e))) }
        };
        let start = bound(self)?;
        if !self.eat_sym(Sym::Colon) {
            let mut index = start.ok_or_else(|| self.unexpected("an index"))?;
            // `d[a, b]` indexes with a tuple; in `comptime` code, `dict[str, str]` is a type.
            if self.at(&Tok::Sym(Sym::Comma)) {
                let mut items = vec![*index];
                while self.eat_sym(Sym::Comma) {
                    items.push(self.parse_expr()?);
                }
                index = Box::new(Expr::new(ExprKind::Tuple(items), span));
            }
            self.expect_sym(Sym::RBracket)?;
            return Ok(Expr::new(ExprKind::Index { base: Box::new(base), index }, span));
        }
        let end = bound(self)?;
        self.expect_sym(Sym::RBracket)?;
        Ok(Expr::new(ExprKind::Slice { base: Box::new(base), start, end }, span))
    }

    fn parse_postfix(&mut self) -> Result<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            let span = self.span();
            if self.eat_sym(Sym::LBracket) {
                expr = self.parse_index(expr, span)?;
                continue;
            }
            if !self.eat_sym(Sym::Dot) {
                break;
            }
            let name = self.expect_ident()?;
            let type_args = self.parse_call_type_args();
            let kind = if self.at(&Tok::Sym(Sym::LParen)) {
                ExprKind::Method { receiver: Box::new(expr), name, type_args, args: self.parse_args()? }
            } else {
                ExprKind::Field { base: Box::new(expr), name }
            };
            expr = Expr::new(kind, span);
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let span = self.span();
        let kind = match self.advance() {
            Tok::Int(value) => ExprKind::Int(
                i128::try_from(value).map_err(|_| Diagnostic::error(span, "integer literal is too large"))?,
            ),
            Tok::Float(value) => ExprKind::Float(value),
            Tok::Str(text) => ExprKind::Str(text),
            Tok::Format(parts) => ExprKind::Format(parse_format(parts, span)?),
            Tok::Char(c) => ExprKind::Char(c),
            Tok::Kw(Kw::True) => ExprKind::Bool(true),
            Tok::Kw(Kw::False) => ExprKind::Bool(false),
            Tok::Ident(name) => self.parse_name_or_call(name)?,
            Tok::Kw(Kw::SelfValue) => ExprKind::Name("self".into()),
            Tok::Kw(Kw::None) => ExprKind::None,
            Tok::Kw(Kw::Lambda) => self.parse_lambda()?,
            Tok::Sym(Sym::LParen) => return self.parse_parenthesized(span),
            Tok::Sym(Sym::LBracket) => return self.parse_array_literal(span),
            Tok::Sym(Sym::LBrace) => return self.parse_collection_literal(span),
            other => {
                return Err(Diagnostic::error(
                    span,
                    format!("expected an expression, found {}", other.describe()),
                ));
            }
        };
        Ok(Expr::new(kind, span))
    }

    /// `f(x)`, `f[T](x)`, or a plain name.
    fn parse_name_or_call(&mut self, callee: String) -> Result<ExprKind> {
        let type_args = self.parse_call_type_args();
        if !self.at(&Tok::Sym(Sym::LParen)) {
            return Ok(ExprKind::Name(callee));
        }
        let args = self.parse_args()?;
        Ok(ExprKind::Call { callee, type_args, args, target: None })
    }

    /// Type arguments are only taken when a call follows, as in `size_of[T]()`.
    fn parse_call_type_args(&mut self) -> Vec<TypeExpr> {
        if !self.at(&Tok::Sym(Sym::LBracket)) {
            return Vec::new();
        }
        let start = self.pos;
        match self.parse_type_args() {
            Ok(args) if self.at(&Tok::Sym(Sym::LParen)) => args,
            _ => {
                self.pos = start;
                Vec::new()
            }
        }
    }

    /// `(x)` or a tuple `(a, b)`.
    fn parse_parenthesized(&mut self, span: Span) -> Result<Expr> {
        let first = self.parse_expr()?;
        if self.eat_sym(Sym::RParen) {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat_sym(Sym::Comma) {
            items.push(self.parse_expr()?);
        }
        self.expect_sym(Sym::RParen)?;
        Ok(Expr::new(ExprKind::Tuple(items), span))
    }

    /// `[a, b, c]`
    /// `lambda (x: i32, y: i32) -> i32: x + y`, after the `lambda` keyword.
    fn parse_lambda(&mut self) -> Result<ExprKind> {
        let captures = self.parse_captures()?;
        let (params, variadic) = self.parse_params()?;
        if variadic {
            return Err(Diagnostic::error(self.span(), "a lambda cannot take `...`"));
        }
        let ret = self.parse_return_type()?;
        self.expect_sym(Sym::Colon)?;
        let body = self.parse_expr()?;
        Ok(ExprKind::Lambda { captures, params, ret, body: Box::new(body), c_name: String::new() })
    }

    /// `[` followed by `&`: a capture list, not type parameters.
    pub(super) fn at_captures(&self) -> bool {
        self.at(&Tok::Sym(Sym::LBracket))
            && matches!(self.tokens.get(self.pos + 1).map(|t| &t.tok), Some(Tok::Sym(Sym::Amp)))
    }

    /// `[&a, &mut b]`: lambdas capture by reference only.
    pub(super) fn parse_captures(&mut self) -> Result<Vec<Capture>> {
        let mut captures = Vec::new();
        if !self.eat_sym(Sym::LBracket) {
            return Ok(captures);
        }
        while !self.eat_sym(Sym::RBracket) {
            if !captures.is_empty() {
                self.expect_sym(Sym::Comma)?;
            }
            let span = self.span();
            if !self.eat_sym(Sym::Amp) {
                return Err(Diagnostic::error(
                    span,
                    "lambdas capture by reference: write `&name` or `&mut name`",
                ));
            }
            let mutable = self.eat_kw(Kw::Mut);
            let name = self.expect_ident()?;
            captures.push(Capture { name, mutable, through_ref: false, ty: None, span });
        }
        Ok(captures)
    }

    fn parse_array_literal(&mut self, span: Span) -> Result<Expr> {
        let mut items = Vec::new();
        if !self.at(&Tok::Sym(Sym::RBracket)) {
            let first = self.parse_expr()?;
            if self.eat_kw(Kw::For) {
                return self.parse_comprehension(first, span);
            }
            items.push(first);
        }
        while !self.eat_sym(Sym::RBracket) {
            if !items.is_empty() {
                self.expect_sym(Sym::Comma)?;
            }
            items.push(self.parse_expr()?);
        }
        Ok(Expr::new(ExprKind::ArrayLit(items), span))
    }

    /// `{k: v, ...}` for a `dict`, `{a, b}` for a `set`, after `{`; `{}` is an empty `dict`.
    fn parse_collection_literal(&mut self, span: Span) -> Result<Expr> {
        let (mut keys, mut values) = (Vec::new(), Some(Vec::new()));
        while !self.eat_sym(Sym::RBrace) {
            if !keys.is_empty() {
                self.expect_sym(Sym::Comma)?;
                if self.eat_sym(Sym::RBrace) {
                    break;
                }
            }
            keys.push(self.parse_expr()?);
            if keys.len() == 1 && !self.at(&Tok::Sym(Sym::Colon)) {
                values = None;
            }
            if let Some(values) = &mut values {
                self.expect_sym(Sym::Colon)?;
                values.push(self.parse_expr()?);
            }
        }
        Ok(Expr::new(ExprKind::Collection { keys, values, built: None }, span))
    }

    /// `[value for var in iter if filter]`, after `for`.
    fn parse_comprehension(&mut self, value: Expr, span: Span) -> Result<Expr> {
        let mut names = vec![self.binding()?];
        while self.eat_sym(Sym::Comma) {
            names.push(self.binding()?);
        }
        let var = match names.as_slice() {
            [single] => single.clone(),
            _ => format!("{}{}", crate::ast::LOOP_ITEM, names.join("_")),
        };
        if names.len() == 1 {
            names.clear();
        }
        self.expect_kw(Kw::In)?;
        let iter = Box::new(self.parse_expr()?);
        let filter = if self.eat_kw(Kw::If) { Some(Box::new(self.parse_expr()?)) } else { None };
        self.expect_sym(Sym::RBracket)?;
        let kind = ExprKind::Comprehension { value: Box::new(value), var, names, iter, filter, built: None };
        Ok(Expr::new(kind, span))
    }

    pub(super) fn parse_args(&mut self) -> Result<Vec<Expr>> {
        self.expect_sym(Sym::LParen)?;
        let mut args = Vec::new();
        while !self.eat_sym(Sym::RParen) {
            if !args.is_empty() {
                self.expect_sym(Sym::Comma)?;
            }
            args.push(self.parse_arg()?);
        }
        Ok(args)
    }

    /// An argument, possibly named: `port=443`.
    fn parse_arg(&mut self) -> Result<Expr> {
        let span = self.span();
        if let (Tok::Ident(name), Some(Tok::Sym(Sym::Assign))) =
            (self.peek().clone(), self.tokens.get(self.pos + 1).map(|t| &t.tok))
        {
            self.advance();
            self.advance();
            let value = Box::new(self.parse_expr()?);
            return Ok(Expr::new(ExprKind::Named { name, value }, span));
        }
        self.parse_expr()
    }
}

/// Parses the expressions embedded in an f-string; their positions are counted from the `{`.
fn parse_format(parts: Vec<FormatPart>, span: Span) -> Result<Vec<Expr>> {
    let mut out = Vec::new();
    for part in parts {
        match part {
            FormatPart::Text(text) if text.is_empty() => {}
            FormatPart::Text(text) => out.push(Expr::new(ExprKind::Str(text), span)),
            FormatPart::Expr(source, at, spec) => {
                let mut tokens = crate::lexer::lex(&source, at.file)?;
                for token in &mut tokens {
                    token.span = Span::new(at.file, at.line, at.col + token.span.col);
                }
                let mut parser = Parser { tokens, pos: 0, discards: 0 };
                let expr = parser.parse_expr()?;
                if !matches!(parser.peek(), Tok::Newline | Tok::Eof) {
                    return Err(Diagnostic::error(parser.span(), "expected `}` after the expression"));
                }
                let expr = match spec {
                    Some(text) => {
                        let spec = crate::ast::FormatSpec::parse(&text)
                            .map_err(|message| Diagnostic::error(at, message))?;
                        let span = expr.span;
                        Expr::new(ExprKind::Formatted { value: Box::new(expr), spec }, span)
                    }
                    None => expr,
                };
                out.push(expr);
            }
        }
    }
    Ok(out)
}
