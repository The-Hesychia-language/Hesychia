//! Token definitions.

use crate::diag::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(u128),
    Float(f64),
    Str(String),
    Char(char),
    /// `f"..."`, split into text and embedded expressions.
    Format(Vec<FormatPart>),
    Kw(Kw),
    Sym(Sym),
    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    /// The comment lines right above the token's line, when it starts that line: what
    /// documents a struct, a field or an enum case.
    pub doc: Option<String>,
}

macro_rules! keywords {
    ($($variant:ident => $text:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Kw { $($variant),* }

        impl Kw {
            pub fn from_word(word: &str) -> Option<Kw> {
                match word { $($text => Some(Kw::$variant),)* _ => None }
            }

            pub fn text(self) -> &'static str {
                match self { $(Kw::$variant => $text),* }
            }
        }
    };
}

keywords! {
    Managed => "managed", Manual => "manual", Fn => "fn", Method => "method",
    Struct => "struct", Enum => "enum", Trait => "trait", Type => "type",
    Extern => "extern", Priv => "priv", Const => "const", Mut => "mut",
    Owned => "owned", SelfValue => "self", If => "if", Elif => "elif", Else => "else",
    While => "while", For => "for", In => "in", Break => "break",
    Continue => "continue", Return => "return", Match => "match", Case => "case",
    Pass => "pass", With => "with", As => "as", Raise => "raise", Raises => "raises",
    Try => "try", Except => "except", Check => "check", Lambda => "lambda",
    And => "and", Or => "or", Not => "not", Is => "is", None => "None",
    True => "True", False => "False", Import => "import", From => "from",
    Comptime => "comptime", Asm => "asm",
}

macro_rules! symbols {
    ($($variant:ident => $text:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Sym { $($variant),* }

        impl Sym {
            /// Every symbol, longest first, so that `**=` wins over `**` and `*`.
            pub fn all_longest_first() -> Vec<Sym> {
                let mut all = vec![$(Sym::$variant),*];
                all.sort_by_key(|sym| std::cmp::Reverse(sym.text().len()));
                all
            }

            pub fn text(self) -> &'static str {
                match self { $(Sym::$variant => $text),* }
            }
        }
    };
}

symbols! {
    Plus => "+", Minus => "-", Star => "*", Slash => "/", SlashSlash => "//",
    Percent => "%", StarStar => "**", EqEq => "==", NotEq => "!=", Lt => "<",
    Le => "<=", Gt => ">", Ge => ">=", Amp => "&", Pipe => "|", Caret => "^",
    Tilde => "~", Shl => "<<", Shr => ">>", Assign => "=", PlusEq => "+=",
    MinusEq => "-=", StarEq => "*=", SlashEq => "/=", SlashSlashEq => "//=",
    PercentEq => "%=", StarStarEq => "**=", AmpEq => "&=", PipeEq => "|=",
    CaretEq => "^=", ShlEq => "<<=", ShrEq => ">>=", Colon => ":", Arrow => "->",
    Comma => ",", Dot => ".", Ellipsis => "...", At => "@", LParen => "(",
    RParen => ")", LBracket => "[", RBracket => "]", LBrace => "{", RBrace => "}",
}

impl Tok {
    /// Human-readable form used in error messages.
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(name) => format!("`{name}`"),
            Tok::Int(_) | Tok::Float(_) => "a number".into(),
            Tok::Str(_) | Tok::Format(_) => "a string".into(),
            Tok::Char(_) => "a character".into(),
            Tok::Kw(kw) => format!("`{}`", kw.text()),
            Tok::Sym(sym) => format!("`{}`", sym.text()),
            Tok::Newline => "end of line".into(),
            Tok::Indent => "an indented block".into(),
            Tok::Dedent => "end of block".into(),
            Tok::Eof => "end of file".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormatPart {
    Text(String),
    /// The source of an embedded expression, where it starts, and its format after `:`.
    Expr(String, Span, Option<String>),
}
