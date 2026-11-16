//! Syntax tree produced by the parser and annotated by the type checker.

use crate::diag::Span;
use crate::types::Type;

pub struct Program {
    /// The module holding `main` comes first.
    pub modules: Vec<Module>,
}

pub struct Module {
    /// Dotted name, such as `net.http` for `net/http.hsy`.
    pub name: String,
    /// Part of the standard library, which may `extend` the built-in types.
    pub stdlib: bool,
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

/// The parsed content of one file, before it becomes a module.
pub struct ParsedFile {
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

pub struct Import {
    pub module: Vec<String>,
    pub kind: ImportKind,
    pub span: Span,
}

pub enum ImportKind {
    /// `import a.b [as c]`
    Module { alias: Option<String> },
    /// `from a.b import x [as y], ...`
    Names(Vec<(String, Option<String>)>),
}

pub enum Item {
    Fn(FnDecl),
    /// `extend str:` or `extend vector[T]:`, standard library only.
    Extend(ExtendDecl),
    /// `const stdout: File = ...` at module level, standard library only: a value made
    /// before `main` runs, which code reads but never changes nor moves.
    Global(GlobalDecl),
    Trait(TraitDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Extern(ExternFn),
    Comptime(ComptimeConst),
    /// `comptime fn`: run by the compiler, on values and types known at compile time.
    ComptimeFn(FnDecl),
    ComptimeIf(ComptimeIf<Item>),
    ComptimeAssert(ComptimeAssert),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Managed,
    Manual,
}

#[derive(Clone)]
pub struct FnDecl {
    pub level: Level,
    pub private: bool,
    pub name: String,
    /// `[T: Bound + Other, U]`; specialized for each set of types it is called with.
    pub generics: Vec<GenericParam>,
    /// `[&a, &mut b]`, for a function defined inside another one.
    pub captures: Vec<Capture>,
    /// Set for methods that take `self`; `None` for functions and static methods.
    pub receiver: Option<Receiver>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    /// The error types of `raises A | B`.
    pub raises: Vec<TypeExpr>,
    pub body: Vec<Stmt>,
    /// `@panic_handler` and the like, written above the function.
    pub decorators: Vec<Decorator>,
    pub span: Span,
}

/// One operand of `asm`: the register it binds and the value or place it binds to.
#[derive(Clone)]
pub struct AsmOperand {
    pub kind: AsmKind,
    pub register: String,
    /// `None` for `clobber`.
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AsmKind {
    In,
    Out,
    InOut,
    Clobber,
}

/// `@name` or `@name(args)` above a declaration.
#[derive(Clone)]
pub struct Decorator {
    pub name: String,
    pub args: Vec<Expr>,
    pub span: Span,
}

pub struct ExternFn {
    pub private: bool,
    pub name: String,
    /// The C symbol: the name, or the one `@symbol("...")` gives.
    pub symbol: String,
    pub params: Vec<Param>,
    pub variadic: bool,
    pub ret: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver {
    /// `self` in a `managed` struct.
    Plain,
    /// `&self`
    Shared,
    /// `&mut self`
    Mut,
    /// `owned self`
    Owned,
}

#[derive(Clone)]
pub struct Param {
    pub owned: bool,
    pub name: String,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone)]
pub struct StructDecl {
    pub level: Level,
    pub private: bool,
    pub name: String,
    /// `[T, U]`: each set of types used makes its own struct.
    pub generics: Vec<GenericParam>,
    /// The traits named in `struct Name(TraitA, TraitB)`.
    pub traits: Vec<(String, Span)>,
    pub fields: Vec<FieldDecl>,
    pub methods: Vec<FnDecl>,
    /// `@packed`, `@align(N)`.
    pub decorators: Vec<Decorator>,
    pub span: Span,
}

/// Methods added to a built-in type: `extend vector[T: Order]:` then `method`s.
#[derive(Clone)]
pub struct ExtendDecl {
    /// `str` or `vector`.
    pub ty: String,
    /// The type's parameters, such as the `T` of `vector[T]`, with their bounds.
    pub generics: Vec<GenericParam>,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Clone)]
pub struct GenericParam {
    pub name: String,
    pub bounds: Vec<(String, Span)>,
    pub span: Span,
}

/// `&name` or `&mut name` in a lambda's capture list. `through_ref` is set by the checker
/// when the captured variable is itself a reference, passed on as it is.
#[derive(Clone)]
pub struct Capture {
    pub name: String,
    pub mutable: bool,
    pub through_ref: bool,
    /// The type of the captured value, set by the checker.
    pub ty: Option<Type>,
    pub span: Span,
}

/// `trait Name:` and its method signatures; `default` marks a method with a body.
pub struct TraitDecl {
    pub private: bool,
    pub name: String,
    pub methods: Vec<TraitMethod>,
    pub span: Span,
}

#[derive(Clone)]
pub struct TraitMethod {
    pub decl: FnDecl,
    pub default: bool,
}

#[derive(Clone)]
pub struct FieldDecl {
    pub private: bool,
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone)]
pub struct EnumDecl {
    pub level: Level,
    pub private: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<VariantDecl>,
    pub span: Span,
}

#[derive(Clone)]
pub struct VariantDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
    pub span: Span,
}

/// `comptime NAME: T = value`, at module level or in a function.
#[derive(Clone)]
pub struct GlobalDecl {
    pub private: bool,
    pub name: String,
    pub ty: TypeExpr,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone)]
pub struct ComptimeConst {
    pub private: bool,
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
    pub span: Span,
}

/// `comptime if`: only the selected branch is kept and checked.
#[derive(Clone)]
pub struct ComptimeIf<T> {
    pub branches: Vec<(Expr, Vec<T>)>,
    pub otherwise: Option<Vec<T>>,
}

#[derive(Clone)]
pub struct ComptimeAssert {
    pub cond: Expr,
    pub message: Option<String>,
    pub span: Span,
}

/// A type as written in the source.
#[derive(Clone)]
pub enum TypeExpr {
    /// `i32`, `Point`, `ptr[u8]`, `array[u8, 4]`
    Named { name: String, args: Vec<TypeExpr>, span: Span },
    /// `&T` or `&mut T`
    Ref { mutable: bool, inner: Box<TypeExpr>, span: Span },
    /// `(A, B)`
    Tuple(Vec<TypeExpr>, Span),
    /// A number among type arguments, such as the `4` of `array[u8, 4]`.
    Size(u64, Span),
    /// `fn(A, B) -> R`
    Fn { params: Vec<TypeExpr>, ret: Option<Box<TypeExpr>>, span: Span },
    /// `row(R)`: the type a `comptime fn` gives for these arguments.
    Call { name: String, args: Vec<TypeExpr>, span: Span },
}

impl TypeExpr {
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Named { span, .. }
            | TypeExpr::Ref { span, .. }
            | TypeExpr::Fn { span, .. }
            | TypeExpr::Call { span, .. } => *span,
            TypeExpr::Tuple(_, span) | TypeExpr::Size(_, span) => *span,
        }
    }
}

/// The start of the hidden name of a `for a, b in ...` loop's item.
pub const LOOP_ITEM: &str = "hsy_item_";

#[derive(Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone)]
pub enum StmtKind {
    Var {
        mutable: bool,
        target: VarTarget,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Assign {
        target: Expr,
        op: Option<BinOp>,
        value: Expr,
    },
    Expr(Expr),
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        otherwise: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Pass,
    Comptime(ComptimeConst),
    ComptimeIf(ComptimeIf<Stmt>),
    ComptimeAssert(ComptimeAssert),
    /// The branch selected by a `comptime if`, after checking.
    Block(Vec<Stmt>),
    Match {
        subject: Expr,
        cases: Vec<Case>,
        /// Set by the checker for `match &x:` or a reference: the subject is only read, and
        /// bindings that own data are `&` references into it.
        borrowed: bool,
    },
    Raise(Expr),
    /// A function defined inside another one; `c_name` is set by the checker.
    Fn {
        decl: Box<FnDecl>,
        c_name: String,
    },
    /// `with scope() as s:` and its block; the block ends by waiting for its threads.
    With {
        value: Expr,
        name: String,
        body: Vec<Stmt>,
    },
    /// `asm(in("eax") x, out("edx") y, clobber("ecx")):` and lines of Intel assembly.
    Asm {
        operands: Vec<AsmOperand>,
        lines: Vec<String>,
    },
    Try {
        body: Vec<Stmt>,
        handlers: Vec<Handler>,
        /// Every error type that can reach the handlers, in order; set by the checker.
        errors: Vec<Type>,
    },
    /// `parallel for x in ...:`. The checker moves the body into `worker`, a nested function
    /// that borrows what it uses and takes the item, run on every core.
    ParallelFor {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
        worker: Option<Box<FnDecl>>,
        c_name: String,
    },
    /// `select:` and its cases: the first message among several channels, a time limit, or
    /// with `else`, no waiting.
    Select {
        cases: Vec<SelectCase>,
        otherwise: Option<Vec<Stmt>>,
    },
}

/// `case msg = rx.receive():` or `case time.sleep(2):`. `wait` is that call; the checker
/// makes it a `ChanReceive` or a `SelectSleep`.
#[derive(Clone)]
pub struct SelectCase {
    pub name: Option<String>,
    pub wait: Expr,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone)]
pub struct Handler {
    pub catch: Catch,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone)]
pub enum Catch {
    /// `except ParseError as e:`; `error` is the index of the type in the try's errors.
    Type { path: Vec<String>, name: String, error: Option<usize> },
    /// `except ParseError.Empty:` or `except ParseError.InvalidChar(pos):`.
    Variant { pattern: Pattern, error: Option<usize> },
    /// `except e:`, every error not caught above: one type (`error`), or several
    /// (`several`, indices in the try's errors), held together in a hidden enum.
    All { name: String, error: Option<usize>, several: Vec<usize> },
}

#[derive(Clone)]
pub struct Case {
    pub pattern: Pattern,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone)]
pub enum Pattern {
    /// `_`
    Wildcard(Span),
    /// `name`: matches anything and binds it.
    Bind(String, Span),
    /// `1`, `-3`, `True`, `'a'`; typed by the checker.
    Literal(Expr),
    /// `Shape.Circle(r)` or `Shape.Empty`; `variant` is set by the checker.
    Variant {
        path: Vec<String>,
        bindings: Option<Vec<String>>,
        variant: Option<usize>,
        span: Span,
    },
    None(Span),
    /// `a | b`
    Or(Vec<Pattern>, Span),
    /// `IoError as e` (or `IoError`) on a caught error: its type; `index` is set by the
    /// checker, the type's position among those caught.
    ErrorType {
        path: Vec<String>,
        name: Option<String>,
        index: Option<usize>,
        span: Span,
    },
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard(span) | Pattern::Bind(_, span) | Pattern::None(span) => *span,
            Pattern::Variant { span, .. } | Pattern::Or(_, span) | Pattern::ErrorType { span, .. } => *span,
            Pattern::Literal(expr) => expr.span,
        }
    }
}

/// What a declaration binds: one name, or the parts of a tuple.
#[derive(Clone)]
pub enum VarTarget {
    Name(String),
    Tuple(Vec<String>),
}

#[derive(Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    /// Filled in by the type checker.
    pub ty: Option<Type>,
}

#[derive(Clone)]
pub enum ExprKind {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    Char(char),
    Name(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `f(x)` or `f[T](x)`; `target` is the C function, set by the type checker.
    Call {
        callee: String,
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
        target: Option<String>,
    },
    Method {
        receiver: Box<Expr>,
        name: String,
        /// `csv.load[dict](f)`: type arguments of a function reached through a module.
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
    },
    Field {
        base: Box<Expr>,
        name: String,
    },
    /// A call such as `i64(x)`, recognised by the type checker.
    Convert(Box<Expr>),
    /// `range(...)` in a `for` loop: start, end, optional step.
    Range {
        start: Box<Expr>,
        end: Box<Expr>,
        step: Option<Box<Expr>>,
    },
    /// `name=value` inside call arguments.
    Named {
        name: String,
        value: Box<Expr>,
    },
    /// `&x` or `&mut x`
    AddrOf {
        mutable: bool,
        inner: Box<Expr>,
    },
    /// Reading through a reference, inserted by the type checker.
    Deref(Box<Expr>),
    /// A module constant: the module that declares it, and its name.
    Global {
        module: usize,
        name: String,
    },
    /// `base[start:end]`, through the `Slice` trait; an end left out is `None`.
    Slice {
        base: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Tuple(Vec<Expr>),
    /// `[a, b, c]`
    ArrayLit(Vec<Expr>),
    /// A struct value, with one expression per field in declaration order.
    Construct(Vec<Expr>),
    /// `x.copy()`: a duplicate that leaves `x` untouched.
    CopyOf(Box<Expr>),
    /// `T.checked(x)`, `T.truncate(x)` and `T.reinterpret(x)`; the target is the type.
    Cast(Box<Expr>, CastKind),
    /// `[value for var in iter if filter]`, `managed` only. The checker turns it into a
    /// hidden vector `result` filled by `fill`, a `for` loop.
    Comprehension {
        value: Box<Expr>,
        var: String,
        /// `for a, b in pairs`: the names each item is taken apart into, from `var`.
        names: Vec<String>,
        iter: Box<Expr>,
        filter: Option<Box<Expr>>,
        built: Option<(String, Box<Stmt>)>,
    },
    /// A value of the enum variant `index`, with one expression per field.
    Variant {
        index: usize,
        args: Vec<Expr>,
    },
    None,
    /// A value placed into an `Optional`, inserted by the checker.
    Wrap(Box<Expr>),
    /// The value inside an `Optional` known not to be `None`, inserted by the checker.
    Unwrap(Box<Expr>),
    /// `check f(...)`: the call's errors go to the enclosing `try` or to the caller.
    /// `errors` lists the callee's error types, in its order.
    Check {
        call: Box<Expr>,
        errors: Vec<Type>,
    },
    /// `f"..."`: string literals and values, joined into a new `str`.
    Format(Vec<Expr>),
    /// `{x:>8.2f}` in an f-string: a value written to a format. The checker makes it a call
    /// to the prelude.
    Formatted {
        value: Box<Expr>,
        spec: FormatSpec,
    },
    /// `lambda (x: i32) -> i32: x * 2`; `c_name` is set by the checker.
    Lambda {
        captures: Vec<Capture>,
        params: Vec<Param>,
        ret: Option<TypeExpr>,
        body: Box<Expr>,
        c_name: String,
    },
    /// A function used as a value, by its C name.
    FnRef(String),
    /// A call through a value of type `fn(A) -> R`.
    CallValue {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// A value moved out of the variable `root`, which is left empty so it is not released twice.
    Move {
        value: Box<Expr>,
        root: String,
    },
    /// An operation of the language runtime; borrowed operands are given as `AddrOf`.
    Builtin {
        op: Builtin,
        args: Vec<Expr>,
    },
}

/// The conversions that are not `T(x)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastKind {
    /// `Optional[T]`: `None` when the value does not fit.
    Checked,
    /// Keeps the low bits of an integer; a float goes toward zero, saturated.
    Truncate,
    /// The same bits read as another type of the same size.
    Reinterpret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Print,
    Panic,
    Allocate,
    /// `deallocate(b)`: the value of a `heap[T]`, whose memory is given back.
    Deallocate,
    SharedNew,
    PtrOf,
    SimdNew,
    /// `spawn(f, args...)`: the function, then its arguments, moved into the new thread.
    Spawn,
    ThreadJoin,
    /// `channel[T]()`, `tx.send(x)`, `rx.receive()`.
    Channel,
    ChanSend,
    ChanReceive,
    /// The time limit of a `select`: a duration and its unit, in nanoseconds.
    SelectSleep,
    /// `atomic(x)`, then `a.load()`, `a.store(x)`, `a.add(x)`, `a.sub(x)`, `a.exchange(x)`,
    /// `a.compare_exchange(old, new)`; the first argument of a method is the atomic's place.
    AtomicNew,
    /// `mutex(x)`.
    MutexNew,
    /// `scope()`, and `s.spawn(f, args...)`: the scope's place, then the function and its arguments.
    ScopeNew,
    ScopedSpawn,
    AtomicLoad,
    AtomicStore,
    AtomicAdd,
    AtomicSub,
    AtomicExchange,
    AtomicCompareExchange,
    PtrCast,
    VolatileRead,
    VolatileWrite,
    StrLen,
    StrBytes,
    StrToC,
    StrFromC,
    /// `x.hash()` of an integer, `bool`, `char` or `str`.
    Hash,
    /// `a.wrapping_add(b)` and the like: the result modulo the type's range, never a panic.
    WrappingAdd,
    WrappingSub,
    WrappingMul,
    /// `s.byte(i)`, checked against the length; `str.from_bytes(&v)`, checked as UTF-8.
    StrByte,
    StrFromBytes,
    VecNew,
    VecPush,
    VecPop,
    VecLen,
    VecInsert,
    VecRemove,
    VecClear,
    VecSwap,
}

/// The C name of a Hesychia function, unique across modules.
pub fn fn_c_name(module: usize, name: &str) -> String {
    format!("hsy_m{module}_{name}")
}

/// The C name of a module constant.
pub fn global_c_name(module: usize, name: &str) -> String {
    format!("hsy_g{module}_{name}")
}

/// The C name of a method of the type `owner`.
pub fn method_c_name(module: usize, owner: &str, name: &str) -> String {
    format!("hsy_m{module}_{owner}_{name}")
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span, ty: None }
    }

    /// The type assigned by the type checker; only valid after checking.
    pub fn ty(&self) -> &Type {
        self.ty.as_ref().expect("expression checked before use")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Is,
    IsNot,
    /// `x in c`, through the `Contains` trait.
    In,
    NotIn,
}

impl BinOp {
    pub fn is_comparison(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }

    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::FloorDiv => "//",
            BinOp::Mod => "%",
            BinOp::Pow => "**",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Is => "is",
            BinOp::IsNot => "is not",
            BinOp::In => "in",
            BinOp::NotIn => "not in",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
        }
    }
}

/// The format after `:` in an f-string, as Python writes it: `[[fill]align][+][0][width][,]
/// [.precision][kind]`.
#[derive(Clone, Debug, PartialEq)]
pub struct FormatSpec {
    pub fill: char,
    /// `<`, `>`, `^`, or none: numbers go right, the rest left.
    pub align: Option<char>,
    pub plus: bool,
    pub zero: bool,
    pub width: u32,
    pub comma: bool,
    pub precision: Option<u32>,
    /// `d`, `x`, `X`, `b`, `o`, `f`, `e`, `%`, or none.
    pub kind: Option<char>,
}

impl FormatSpec {
    pub fn parse(text: &str) -> Result<FormatSpec, String> {
        let chars: Vec<char> = text.chars().collect();
        let mut at = 0;
        let mut spec = FormatSpec {
            fill: ' ',
            align: None,
            plus: false,
            zero: false,
            width: 0,
            comma: false,
            precision: None,
            kind: None,
        };
        let is_align = |c: Option<&char>| matches!(c, Some('<' | '>' | '^'));
        if is_align(chars.get(1)) {
            spec.fill = chars[0];
            spec.align = Some(chars[1]);
            at = 2;
        } else if is_align(chars.first()) {
            spec.align = Some(chars[0]);
            at = 1;
        }
        if chars.get(at) == Some(&'+') {
            spec.plus = true;
            at += 1;
        }
        if chars.get(at) == Some(&'0') {
            spec.zero = true;
            at += 1;
        }
        let number = |at: &mut usize| -> Result<Option<u32>, String> {
            let start = *at;
            while chars.get(*at).is_some_and(char::is_ascii_digit) {
                *at += 1;
            }
            if start == *at {
                return Ok(None);
            }
            let digits: String = chars[start..*at].iter().collect();
            match digits.parse::<u32>() {
                Ok(n) if n <= 1000 => Ok(Some(n)),
                _ => Err(format!("`{digits}` is too large for a format")),
            }
        };
        spec.width = number(&mut at)?.unwrap_or(0);
        if chars.get(at) == Some(&',') {
            spec.comma = true;
            at += 1;
        }
        if chars.get(at) == Some(&'.') {
            at += 1;
            spec.precision =
                Some(number(&mut at)?.ok_or("a format's `.` is followed by a number of digits")?);
        }
        if let Some(&kind) = chars.get(at) {
            if !"dxXbofe%".contains(kind) {
                return Err(format!("unknown format `{kind}`: it is one of `d x X b o f e %`"));
            }
            spec.kind = Some(kind);
            at += 1;
        }
        if at < chars.len() {
            let rest: String = chars[at..].iter().collect();
            return Err(format!("unexpected `{rest}` in a format"));
        }
        Ok(spec)
    }
}
