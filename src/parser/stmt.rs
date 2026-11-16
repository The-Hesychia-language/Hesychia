//! Statements and blocks.

use super::Parser;
use crate::ast::{AsmKind, AsmOperand, BinOp, Expr, ExprKind, Level, SelectCase, Stmt, StmtKind, VarTarget};
use crate::diag::{Diagnostic, Result};
use crate::lexer::{Kw, Sym, Tok};

impl Parser {
    /// Parses `:` followed by an indented block, or a single statement on the same line.
    pub(super) fn parse_block(&mut self) -> Result<Vec<Stmt>> {
        self.expect_sym(Sym::Colon)?;
        if !self.eat(&Tok::Newline) {
            return Ok(vec![self.parse_simple_stmt()?]);
        }
        self.expect(Tok::Indent)?;
        let mut body = Vec::new();
        while !self.eat(&Tok::Dedent) {
            body.push(self.parse_stmt()?);
        }
        Ok(body)
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        let span = self.span();
        let kind = match self.peek() {
            Tok::Kw(Kw::If) => self.parse_if()?,
            Tok::Kw(Kw::While) => self.parse_while()?,
            Tok::Kw(Kw::For) => self.parse_for()?,
            Tok::Kw(Kw::Match) => self.parse_match()?,
            Tok::Kw(Kw::Manual | Kw::Managed) => {
                let level =
                    if self.advance() == Tok::Kw(Kw::Manual) { Level::Manual } else { Level::Managed };
                self.expect_kw(Kw::Fn)?;
                let decl = self.parse_fn_rest(level, false, span)?;
                StmtKind::Fn { decl: Box::new(decl), c_name: String::new() }
            }
            Tok::Kw(Kw::Try) => self.parse_try()?,
            Tok::Kw(Kw::Asm) => self.parse_asm()?,
            Tok::Kw(Kw::With) => {
                self.advance();
                let value = self.parse_expr()?;
                self.expect_kw(Kw::As)?;
                let name = self.expect_ident()?;
                StmtKind::With { value, name, body: self.parse_block()? }
            }
            Tok::Kw(Kw::Comptime) => return self.parse_comptime_stmt(),
            // `parallel` and `select` are names elsewhere.
            Tok::Ident(name) if name == "parallel" && self.next_is(&Tok::Kw(Kw::For)) => {
                self.advance();
                let StmtKind::For { var, iter, body } = self.parse_for()? else { unreachable!("a for loop") };
                if var.starts_with(crate::ast::LOOP_ITEM) {
                    return Err(Diagnostic::error(span, "a `parallel for` takes one name"));
                }
                StmtKind::ParallelFor { var, iter, body, worker: None, c_name: String::new() }
            }
            Tok::Ident(name) if name == "select" && self.next_is(&Tok::Sym(Sym::Colon)) => {
                self.parse_select()?
            }
            _ => return self.parse_simple_stmt(),
        };
        Ok(Stmt { kind, span })
    }

    fn next_is(&self, tok: &Tok) -> bool {
        self.tokens.get(self.pos + 1).is_some_and(|t| t.tok == *tok)
    }

    /// `select:` then `case [name =] call:` blocks and an optional `else:`.
    fn parse_select(&mut self) -> Result<StmtKind> {
        self.advance();
        self.expect_sym(Sym::Colon)?;
        self.expect_newline()?;
        self.expect(Tok::Indent)?;
        let mut cases = Vec::new();
        let mut otherwise = None;
        while !self.eat(&Tok::Dedent) {
            let span = self.span();
            if self.eat_kw(Kw::Else) {
                otherwise = Some(self.parse_block()?);
                continue;
            }
            if otherwise.is_some() {
                return Err(Diagnostic::error(span, "`else` comes last in a `select`"));
            }
            self.expect_kw(Kw::Case)?;
            let first = self.parse_expr()?;
            let (name, wait) = if self.eat_sym(Sym::Assign) {
                let ExprKind::Name(name) = first.kind else {
                    return Err(Diagnostic::error(first.span, "a `case` takes the message into a name"));
                };
                (Some(name), self.parse_expr()?)
            } else {
                (None, first)
            };
            cases.push(SelectCase { name, wait, body: self.parse_block()?, span });
        }
        if cases.is_empty() {
            return Err(self.unexpected("a `case`"));
        }
        Ok(StmtKind::Select { cases, otherwise })
    }

    /// `asm(operands):` then one string per line of assembly.
    fn parse_asm(&mut self) -> Result<StmtKind> {
        self.expect_kw(Kw::Asm)?;
        self.expect_sym(Sym::LParen)?;
        let mut operands = Vec::new();
        while !self.eat_sym(Sym::RParen) {
            if !operands.is_empty() {
                self.expect_sym(Sym::Comma)?;
            }
            operands.push(self.parse_asm_operand()?);
        }
        self.expect_sym(Sym::Colon)?;
        self.expect_newline()?;
        self.expect(Tok::Indent)?;
        let mut lines = Vec::new();
        while !self.eat(&Tok::Dedent) {
            match self.advance() {
                Tok::Str(line) => lines.push(line),
                _ => {
                    return Err(Diagnostic::error(
                        self.span(),
                        "the body of `asm` is lines of text, one string each",
                    ));
                }
            }
            self.expect_newline()?;
        }
        Ok(StmtKind::Asm { operands, lines })
    }

    /// `in("reg") x`, `out("reg") y`, `inout("reg") z` or `clobber("reg")`.
    fn parse_asm_operand(&mut self) -> Result<AsmOperand> {
        let span = self.span();
        let kind = match self.advance() {
            Tok::Kw(Kw::In) => AsmKind::In,
            Tok::Ident(name) if name == "out" => AsmKind::Out,
            Tok::Ident(name) if name == "inout" => AsmKind::InOut,
            Tok::Ident(name) if name == "clobber" => AsmKind::Clobber,
            _ => {
                return Err(Diagnostic::error(span, "expected `in`, `out`, `inout` or `clobber`"));
            }
        };
        self.expect_sym(Sym::LParen)?;
        let Tok::Str(register) = self.advance() else {
            return Err(Diagnostic::error(span, "a register is named by a string: `in(\"eax\") x`"));
        };
        self.expect_sym(Sym::RParen)?;
        let value = if kind == AsmKind::Clobber { None } else { Some(self.parse_expr()?) };
        Ok(AsmOperand { kind, register, value, span })
    }

    /// A statement that fits on one line and ends with a newline.
    fn parse_simple_stmt(&mut self) -> Result<Stmt> {
        let span = self.span();
        let kind = match self.peek() {
            Tok::Kw(Kw::Mut | Kw::Const) => self.parse_var()?,
            Tok::Kw(Kw::Return) => self.parse_return()?,
            Tok::Kw(Kw::Break) => self.keyword_stmt(StmtKind::Break),
            Tok::Kw(Kw::Continue) => self.keyword_stmt(StmtKind::Continue),
            Tok::Kw(Kw::Pass) => self.keyword_stmt(StmtKind::Pass),
            Tok::Kw(Kw::Raise) => {
                self.advance();
                StmtKind::Raise(self.parse_expr()?)
            }
            _ => self.parse_expr_or_assign()?,
        };
        self.expect_newline()?;
        Ok(Stmt { kind, span })
    }

    fn keyword_stmt(&mut self, kind: StmtKind) -> StmtKind {
        self.advance();
        kind
    }

    fn parse_var(&mut self) -> Result<StmtKind> {
        let mutable = self.advance() == Tok::Kw(Kw::Mut);
        let target = self.parse_var_target()?;
        let ty = self.parse_optional_type()?;
        self.expect_sym(Sym::Assign)?;
        let value = self.parse_expr()?;
        Ok(StmtKind::Var { mutable, target, ty, value })
    }

    /// `name`, or `(a, b)` to take a tuple apart.
    fn parse_var_target(&mut self) -> Result<VarTarget> {
        if !self.eat_sym(Sym::LParen) {
            return self.expect_ident().map(VarTarget::Name);
        }
        let mut names = vec![self.expect_ident()?];
        while self.eat_sym(Sym::Comma) {
            names.push(self.expect_ident()?);
        }
        self.expect_sym(Sym::RParen)?;
        Ok(VarTarget::Tuple(names))
    }

    fn parse_return(&mut self) -> Result<StmtKind> {
        self.advance();
        let value = if self.at(&Tok::Newline) { None } else { Some(self.parse_expr()?) };
        Ok(StmtKind::Return(value))
    }

    fn parse_expr_or_assign(&mut self) -> Result<StmtKind> {
        let target = self.parse_expr()?;
        // `a, b = pair` takes a tuple apart into new variables (`managed`).
        if let crate::ast::ExprKind::Name(first) = &target.kind
            && self.at(&Tok::Sym(Sym::Comma))
        {
            let mut names = vec![first.clone()];
            while self.eat_sym(Sym::Comma) {
                names.push(self.expect_ident()?);
            }
            self.expect_sym(Sym::Assign)?;
            let value = self.parse_expr()?;
            return Ok(StmtKind::Var { mutable: true, target: VarTarget::Tuple(names), ty: None, value });
        }
        let Tok::Sym(sym) = *self.peek() else {
            return Ok(StmtKind::Expr(target));
        };
        let Some(op) = assignment_op(sym) else {
            return Ok(StmtKind::Expr(target));
        };
        self.advance();
        let value = self.parse_expr()?;
        Ok(StmtKind::Assign { target, op, value })
    }

    fn parse_if(&mut self) -> Result<StmtKind> {
        self.advance();
        let mut branches = vec![(self.parse_expr()?, self.parse_block()?)];
        while self.eat_kw(Kw::Elif) {
            branches.push((self.parse_expr()?, self.parse_block()?));
        }
        let otherwise = if self.eat_kw(Kw::Else) { Some(self.parse_block()?) } else { None };
        Ok(StmtKind::If { branches, otherwise })
    }

    /// `for x in it:`, or `for a, b in pairs:`, whose body starts by taking each item apart.
    fn parse_for(&mut self) -> Result<StmtKind> {
        let span = self.span();
        self.advance();
        let mut names = vec![self.expect_ident()?];
        while self.eat_sym(Sym::Comma) {
            names.push(self.expect_ident()?);
        }
        self.expect_kw(Kw::In)?;
        let iter = self.parse_expr()?;
        let mut body = self.parse_block()?;
        if names.len() == 1 {
            return Ok(StmtKind::For { var: names.remove(0), iter, body });
        }
        let var = format!("{}{}", crate::ast::LOOP_ITEM, names.join("_"));
        let value = Expr::new(ExprKind::Name(var.clone()), span);
        let kind = StmtKind::Var { mutable: false, target: VarTarget::Tuple(names), ty: None, value };
        body.insert(0, Stmt { kind, span });
        Ok(StmtKind::For { var, iter, body })
    }

    fn parse_comptime_stmt(&mut self) -> Result<Stmt> {
        let span = self.span();
        self.advance();
        let kind = match self.peek() {
            Tok::Kw(Kw::If) => StmtKind::ComptimeIf(self.parse_comptime_if(Self::parse_block)?),
            Tok::Ident(name) if name == "assert" => {
                StmtKind::ComptimeAssert(self.parse_comptime_assert(span)?)
            }
            _ => StmtKind::Comptime(self.parse_comptime_const(false, span)?),
        };
        Ok(Stmt { kind, span })
    }

    fn parse_while(&mut self) -> Result<StmtKind> {
        self.advance();
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(StmtKind::While { cond, body })
    }
}

/// Maps `=` to `None` and `+=`-style symbols to their operator.
fn assignment_op(sym: Sym) -> Option<Option<BinOp>> {
    let op = match sym {
        Sym::Assign => return Some(None),
        Sym::PlusEq => BinOp::Add,
        Sym::MinusEq => BinOp::Sub,
        Sym::StarEq => BinOp::Mul,
        Sym::SlashEq => BinOp::Div,
        Sym::SlashSlashEq => BinOp::FloorDiv,
        Sym::PercentEq => BinOp::Mod,
        Sym::StarStarEq => BinOp::Pow,
        Sym::AmpEq => BinOp::BitAnd,
        Sym::PipeEq => BinOp::BitOr,
        Sym::CaretEq => BinOp::BitXor,
        Sym::ShlEq => BinOp::Shl,
        Sym::ShrEq => BinOp::Shr,
        _ => return None,
    };
    Some(Some(op))
}
