//! Semantic types and their C counterparts.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    Size,
    Offset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FloatTy {
    F32,
    F64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Int(IntTy),
    Float(FloatTy),
    Bool,
    Char,
    /// Only string literals for now; they convert to C with `.to_c()`.
    Str,
    Ptr(Box<Type>),
    Void,
    Struct(Named),
    /// `plain` enums carry no data and are copied like integers.
    Enum {
        named: Named,
        plain: bool,
    },
    Tuple(Vec<Type>),
    Array(Box<Type>, u64),
    /// `&T` or, when `mutable`, `&mut T`.
    Ref(Box<Type>, bool),
    Optional(Box<Type>),
    /// A growable sequence that owns its items.
    Vector(Box<Type>),
    /// An owning pointer to one value, made by `allocate()`.
    Heap(Box<Type>),
    /// `shared[T]`, `managed` only: one value with several owners, counted by reference.
    Shared(Box<Type>),
    /// `SIMD[T, N]`: `N` numbers handled together, operators acting lane by lane.
    Simd(Box<Type>, u64),
    /// `thread[T]`: a running thread that gives a `T` when joined.
    Thread(Box<Type>),
    /// The two ends of a channel carrying `T` values.
    Sender(Box<Type>),
    Receiver(Box<Type>),
    /// `atomic[T]`: an integer or `bool` changed by one operation at a time.
    Atomic(Box<Type>),
    /// `mutex[T]`: a value one thread at a time reaches, inside `with m as x:`.
    Mutex(Box<Type>),
    /// A `mutex` held by a `with` block, unlocked when the block is left; made by the compiler.
    Locked,
    /// The `s` of `with scope() as s:`, which waits for its threads at the end of the block.
    Scope,
    /// A type parameter, or `Self` inside a trait.
    Param(String),
    /// A function: its parameter types and its return type.
    Fn(Vec<Type>, Box<Type>),
    /// A compile-time value given for a parameter such as `[N: size]`.
    Const(u64),
    /// `array[T, N]` where `N` is a compile-time parameter not known yet.
    ArrayParam(Box<Type>, String),
    /// One of several error types, as caught by `except e:`; made by the compiler.
    Errors(Vec<Type>),
    /// A generic type without its arguments, `dict` or `vector`, given to a type parameter:
    /// `csv.load[dict](f)`. Only `comptime` code reads it.
    Family(String),
    /// A type a `comptime fn` gives once its type parameters are known: `row(R)`. `call`
    /// indexes the calls recorded by the type tables.
    Computed {
        call: usize,
        shown: String,
    },
}

/// A user-defined type: its index in the type tables and its name for messages.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Named {
    pub id: usize,
    pub name: String,
}

impl IntTy {
    const ALL: [IntTy; 12] = [
        IntTy::I8,
        IntTy::I16,
        IntTy::I32,
        IntTy::I64,
        IntTy::I128,
        IntTy::U8,
        IntTy::U16,
        IntTy::U32,
        IntTy::U64,
        IntTy::U128,
        IntTy::Size,
        IntTy::Offset,
    ];

    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::I128 => "i128",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::U128 => "u128",
            IntTy::Size => "size",
            IntTy::Offset => "offset",
        }
    }

    pub fn c_name(self) -> &'static str {
        match self {
            IntTy::I8 => "int8_t",
            IntTy::I16 => "int16_t",
            IntTy::I32 => "int32_t",
            IntTy::I64 => "int64_t",
            IntTy::I128 => "__int128",
            IntTy::U8 => "uint8_t",
            IntTy::U16 => "uint16_t",
            IntTy::U32 => "uint32_t",
            IntTy::U64 => "uint64_t",
            IntTy::U128 => "unsigned __int128",
            IntTy::Size => "size_t",
            IntTy::Offset => "ptrdiff_t",
        }
    }

    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128 | IntTy::Offset)
    }

    /// The unsigned type of the same width.
    pub fn unsigned(self) -> IntTy {
        match self {
            IntTy::I8 => IntTy::U8,
            IntTy::I16 => IntTy::U16,
            IntTy::I32 => IntTy::U32,
            IntTy::I64 => IntTy::U64,
            IntTy::I128 => IntTy::U128,
            IntTy::Offset => IntTy::Size,
            unsigned => unsigned,
        }
    }

    /// Width in bits; `size` and `offset` follow the 64-bit host for now.
    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 | IntTy::Size | IntTy::Offset => 64,
            IntTy::I128 | IntTy::U128 => 128,
        }
    }

    /// Whether every value of `other` is representable in this type.
    pub fn contains(self, other: IntTy) -> bool {
        match (self.signed(), other.signed()) {
            (true, true) | (false, false) => self.bits() >= other.bits(),
            (true, false) => self.bits() > other.bits(),
            (false, true) => false,
        }
    }

    /// Whether `value` is representable in this type.
    pub fn fits(self, value: i128) -> bool {
        let bits = self.bits();
        match (self.signed(), bits) {
            (true, 128) => true,
            (false, 128) => value >= 0,
            (true, _) => (-(1i128 << (bits - 1))..(1i128 << (bits - 1))).contains(&value),
            (false, _) => (0..(1i128 << bits)).contains(&value),
        }
    }
}

impl FloatTy {
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }

    pub fn c_name(self) -> &'static str {
        match self {
            FloatTy::F32 => "float",
            FloatTy::F64 => "double",
        }
    }
}

impl Type {
    /// Resolves a scalar type name such as `i32` or `bool`.
    pub fn scalar(name: &str) -> Option<Type> {
        if let Some(ty) = IntTy::ALL.into_iter().find(|t| t.name() == name) {
            return Some(Type::Int(ty));
        }
        match name {
            "f32" => Some(Type::Float(FloatTy::F32)),
            "f64" => Some(Type::Float(FloatTy::F64)),
            "bool" => Some(Type::Bool),
            "char" => Some(Type::Char),
            _ => None,
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Float(_))
    }

    pub fn as_int(&self) -> Option<IntTy> {
        match self {
            Type::Int(ty) => Some(*ty),
            _ => None,
        }
    }

    /// Whether assigning the value copies it instead of moving it.
    /// Replaces each type parameter named in `bindings` by its type.
    pub fn substitute(&self, bindings: &[(String, Type)]) -> Type {
        let sub = |t: &Type| Box::new(t.substitute(bindings));
        match self {
            Type::Param(name) => {
                bindings.iter().find(|(n, _)| n == name).map_or_else(|| self.clone(), |(_, t)| t.clone())
            }
            Type::Ref(inner, m) => Type::Ref(sub(inner), *m),
            Type::Ptr(inner) => Type::Ptr(sub(inner)),
            Type::Optional(inner) => Type::Optional(sub(inner)),
            Type::Vector(inner) => Type::Vector(sub(inner)),
            Type::Heap(inner) => Type::Heap(sub(inner)),
            Type::Shared(inner) => Type::Shared(sub(inner)),
            Type::Simd(item, n) => Type::Simd(sub(item), *n),
            Type::Thread(item) => Type::Thread(sub(item)),
            Type::Sender(item) => Type::Sender(sub(item)),
            Type::Receiver(item) => Type::Receiver(sub(item)),
            Type::Atomic(item) => Type::Atomic(sub(item)),
            Type::Mutex(item) => Type::Mutex(sub(item)),
            Type::Array(inner, n) => Type::Array(sub(inner), *n),
            Type::ArrayParam(inner, len) => match bindings.iter().find(|(n, _)| n == len) {
                Some((_, Type::Const(value))) => Type::Array(sub(inner), *value),
                _ => Type::ArrayParam(sub(inner), len.clone()),
            },
            Type::Tuple(items) => Type::Tuple(items.iter().map(|t| t.substitute(bindings)).collect()),
            other => other.clone(),
        }
    }

    /// The type of a variable that can be called: `fn(...)`, or `&fn(...)`.
    /// The target of a reference, or the type itself.
    pub fn deref_ref(&self) -> &Type {
        match self {
            Type::Ref(target, _) => target,
            other => other,
        }
    }

    pub fn deref_fn(&self) -> Option<Type> {
        match self {
            Type::Fn(..) => Some(self.clone()),
            Type::Ref(inner, _) if matches!(**inner, Type::Fn(..)) => Some(self.clone()),
            _ => None,
        }
    }

    pub fn is_copy(&self) -> bool {
        match self {
            Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Ptr(_)
            | Type::Ref(..)
            | Type::Simd(..)
            | Type::Fn(..) => true,
            Type::Enum { plain, .. } => *plain,
            Type::Tuple(items) => items.iter().all(Type::is_copy),
            Type::Optional(inner) => inner.is_copy(),
            Type::Str
            | Type::Void
            | Type::Struct(_)
            | Type::Array(..)
            | Type::Vector(_)
            | Type::Heap(_)
            | Type::Shared(_)
            | Type::Thread(_)
            | Type::Sender(_)
            | Type::Receiver(_)
            | Type::Atomic(_)
            | Type::Mutex(_)
            | Type::Locked
            | Type::Scope
            | Type::Param(_)
            | Type::Const(_)
            | Type::ArrayParam(..)
            | Type::Errors(_)
            | Type::Family(_)
            | Type::Computed { .. } => false,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Int(ty) => f.write_str(ty.name()),
            Type::Float(ty) => f.write_str(ty.name()),
            Type::Bool => f.write_str("bool"),
            Type::Char => f.write_str("char"),
            Type::Str => f.write_str("str"),
            Type::Ptr(inner) => write!(f, "ptr[{inner}]"),
            Type::Void => f.write_str("nothing"),
            Type::Struct(named) | Type::Enum { named, .. } => f.write_str(&named.name),
            Type::Tuple(items) => {
                let items: Vec<String> = items.iter().map(Type::to_string).collect();
                write!(f, "({})", items.join(", "))
            }
            Type::Array(item, len) => write!(f, "array[{item}, {len}]"),
            Type::Ref(inner, true) => write!(f, "&mut {inner}"),
            Type::Ref(inner, false) => write!(f, "&{inner}"),
            Type::Optional(inner) => write!(f, "Optional[{inner}]"),
            Type::Vector(inner) => write!(f, "vector[{inner}]"),
            Type::Heap(inner) => write!(f, "heap[{inner}]"),
            Type::Shared(inner) => write!(f, "shared[{inner}]"),
            Type::Simd(item, n) => write!(f, "SIMD[{item}, {n}]"),
            Type::Thread(item) if **item == Type::Void => write!(f, "thread"),
            Type::Thread(item) => write!(f, "thread[{item}]"),
            Type::Sender(item) => write!(f, "sender[{item}]"),
            Type::Receiver(item) => write!(f, "receiver[{item}]"),
            Type::Atomic(item) => write!(f, "atomic[{item}]"),
            Type::Mutex(item) => write!(f, "mutex[{item}]"),
            Type::Locked => write!(f, "a locked mutex"),
            Type::Scope => write!(f, "scope"),
            Type::Param(name) | Type::Family(name) | Type::Computed { shown: name, .. } => {
                write!(f, "{name}")
            }
            Type::Const(value) => write!(f, "{value}"),
            Type::ArrayParam(item, len) => write!(f, "array[{item}, {len}]"),
            Type::Errors(types) => {
                let shown: Vec<String> = types.iter().map(std::string::ToString::to_string).collect();
                write!(f, "{}", shown.join(" | "))
            }
            Type::Fn(params, ret) => {
                let params: Vec<String> = params.iter().map(std::string::ToString::to_string).collect();
                match ret.as_ref() {
                    Type::Void => write!(f, "fn({})", params.join(", ")),
                    ret => write!(f, "fn({}) -> {ret}", params.join(", ")),
                }
            }
        }
    }
}

pub struct FieldDef {
    pub name: String,
    pub ty: Type,
    pub private: bool,
    /// The value of the field when it is not given.
    pub default: Option<crate::comptime::Value>,
    /// The comment above the field.
    pub doc: String,
}

pub struct StructDef {
    pub name: String,
    pub module: usize,
    /// The comment above the struct.
    pub doc: String,
    pub fields: Vec<FieldDef>,
    /// C names of the methods implementing built-in traits: `add`, `equal`, `drop`...
    pub operators: std::collections::HashMap<String, String>,
    /// `@packed`: no padding between fields.
    pub packed: bool,
    /// `@align(N)`: at least this alignment.
    pub align: Option<u64>,
}

impl StructDef {
    /// The C name of the `Drop` method, when the struct implements `Drop`.
    pub fn drop_method(&self) -> Option<&String> {
        self.operators.get("drop")
    }
}

pub struct VariantDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
    pub doc: String,
}

pub struct EnumDef {
    pub name: String,
    pub module: usize,
    /// The comment above the enum.
    pub doc: String,
    /// No variant carries data: known from the declaration, before the variants' types.
    pub plain: bool,
    pub variants: Vec<VariantDef>,
    /// C names of the methods implementing built-in traits (`equal`, `hash`, `compare`, ...).
    pub operators: std::collections::HashMap<String, String>,
}

/// The user-defined types of a program, indexed by `Named::id`.
#[derive(Default)]
pub struct TypeTable {
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
}

impl TypeTable {
    /// Size and alignment in bytes, laid out like the C code we emit.
    pub fn layout(&self, ty: &Type) -> Option<(u64, u64)> {
        // Every supported target is 64-bit.
        let pointer = 8;
        Some(match ty {
            Type::Int(t) => (u64::from(t.bits() / 8), u64::from(t.bits() / 8)),
            Type::Float(FloatTy::F32) | Type::Char => (4, 4),
            Type::Float(FloatTy::F64) => (8, 8),
            Type::Bool => (1, 1),
            Type::Ptr(_) | Type::Ref(..) => (pointer, pointer),
            Type::Void
            | Type::Param(_)
            | Type::Const(_)
            | Type::ArrayParam(..)
            | Type::Errors(_)
            | Type::Family(_)
            | Type::Mutex(_)
            | Type::Locked
            | Type::Computed { .. } => {
                return None;
            }
            Type::Str | Type::Vector(_) => (3 * pointer, pointer),
            Type::Heap(_) | Type::Shared(_) | Type::Fn(..) => (pointer, pointer),
            Type::Thread(_) | Type::Sender(_) | Type::Receiver(_) => (pointer, pointer),
            Type::Atomic(item) => self.layout(item)?,
            Type::Scope => (3 * pointer, pointer),
            // gcc and clang align a vector to its size.
            Type::Simd(item, n) => {
                let size = self.layout(item)?.0 * n;
                (size, size)
            }
            Type::Array(item, len) => {
                let (size, align) = self.layout(item)?;
                (size * len, align)
            }
            Type::Tuple(items) => self.record_layout(items.iter())?,
            Type::Struct(named) => {
                let def = &self.structs[named.id];
                let live = def.drop_method().map(|_| &Type::Bool);
                let fields = def.fields.iter().map(|f| &f.ty).chain(live);
                let (size, align) =
                    if def.packed { self.packed_layout(fields)? } else { self.record_layout(fields)? };
                let align = align.max(def.align.unwrap_or(1));
                (round_up(size, align), align)
            }
            Type::Optional(inner) if matches!(**inner, Type::Ptr(_)) => (pointer, pointer),
            Type::Optional(inner) => self.record_layout([&Type::Bool, inner.as_ref()].into_iter())?,
            Type::Enum { named, .. } => {
                let mut payload = (0, 1);
                for variant in &self.enums[named.id].variants {
                    let (size, align) = self.record_layout(variant.fields.iter().map(|f| &f.ty))?;
                    payload = (payload.0.max(size), payload.1.max(align));
                }
                // `struct { uint32_t tag; union { ... } as; }`
                let align = payload.1.max(4);
                let offset = round_up(4, payload.1);
                (round_up(offset + round_up(payload.0, payload.1), align), align)
            }
        })
    }

    /// Whether `ty` holds a value whose struct implements `Drop`; such values are never copied.
    pub fn has_drop_impl(&self, ty: &Type) -> bool {
        self.has_drop_seen(ty, &mut Vec::new())
    }

    /// `seen` holds the user types on the way, so that a recursive type ends.
    fn has_drop_seen(&self, ty: &Type, seen: &mut Vec<Type>) -> bool {
        if matches!(ty, Type::Struct(_) | Type::Enum { .. }) {
            if seen.contains(ty) {
                return false;
            }
            seen.push(ty.clone());
        }
        match ty {
            // Resources: never copied, like a `Drop` type (a `sender` copies only by `.copy()`).
            Type::Thread(_) | Type::Sender(_) | Type::Receiver(_) | Type::Atomic(_) | Type::Scope => true,
            Type::Mutex(_) | Type::Locked => true,
            Type::Struct(named) => {
                let def = &self.structs[named.id];
                def.drop_method().is_some() || def.fields.iter().any(|f| self.has_drop_seen(&f.ty, seen))
            }
            Type::Enum { named, .. } => self.enums[named.id]
                .variants
                .iter()
                .flat_map(|v| &v.fields)
                .any(|f| self.has_drop_seen(&f.ty, seen)),
            Type::Tuple(items) | Type::Errors(items) => items.iter().any(|t| self.has_drop_seen(t, seen)),
            Type::Array(item, _) | Type::Optional(item) | Type::Vector(item) | Type::Heap(item) => {
                self.has_drop_seen(item, seen)
            }
            _ => false,
        }
    }

    /// Whether an f-string can write a `ty`.
    pub fn writable(&self, ty: &Type) -> bool {
        self.writable_seen(ty, &mut Vec::new())
    }

    /// `seen` holds the structs and enums already being asked about, which may hold themselves.
    fn writable_seen(&self, ty: &Type, seen: &mut Vec<(bool, usize)>) -> bool {
        match ty {
            Type::Int(_) | Type::Float(_) | Type::Bool | Type::Char | Type::Str => true,
            Type::Struct(named) => {
                if seen.contains(&(false, named.id)) {
                    return true;
                }
                seen.push((false, named.id));
                let def = &self.structs[named.id];
                // A struct with `Show` writes itself, whatever its fields.
                def.operators.contains_key("show")
                    || def.fields.iter().all(|f| self.writable_seen(&f.ty, seen))
            }
            Type::Enum { named, .. } => {
                if seen.contains(&(true, named.id)) {
                    return true;
                }
                seen.push((true, named.id));
                self.enums[named.id]
                    .variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .all(|f| self.writable_seen(&f.ty, seen))
            }
            Type::Tuple(items) => items.iter().all(|t| self.writable_seen(t, seen)),
            Type::Optional(item)
            | Type::Vector(item)
            | Type::Array(item, _)
            | Type::Heap(item)
            | Type::Shared(item) => self.writable_seen(item, seen),
            _ => false,
        }
    }

    /// Whether a `ty` holds a `shared` value, other than through another `shared` one.
    pub fn contains_shared(&self, ty: &Type) -> bool {
        self.holds_shared(ty, &mut Vec::new())
    }

    fn holds_shared(&self, ty: &Type, seen: &mut Vec<usize>) -> bool {
        match ty {
            Type::Shared(_) => true,
            Type::Struct(named) => {
                if seen.contains(&named.id) {
                    return false;
                }
                seen.push(named.id);
                self.structs[named.id].fields.iter().any(|f| self.holds_shared(&f.ty, seen))
            }
            Type::Enum { named, .. } => self.enums[named.id]
                .variants
                .iter()
                .flat_map(|v| &v.fields)
                .any(|f| self.holds_shared(&f.ty, seen)),
            Type::Tuple(items) | Type::Errors(items) => items.iter().any(|t| self.holds_shared(t, seen)),
            Type::Array(item, _) | Type::Optional(item) | Type::Vector(item) | Type::Heap(item) => {
                self.holds_shared(item, seen)
            }
            _ => false,
        }
    }

    /// Whether values of `ty` own resources that must be released when they go away.
    pub fn needs_drop(&self, ty: &Type) -> bool {
        match ty {
            Type::Str | Type::Vector(_) | Type::Heap(_) | Type::Shared(_) => true,
            Type::Thread(_) | Type::Sender(_) | Type::Receiver(_) | Type::Scope => true,
            Type::Mutex(_) | Type::Locked => true,
            Type::Struct(named) => {
                let def = &self.structs[named.id];
                def.drop_method().is_some() || def.fields.iter().any(|f| self.needs_drop(&f.ty))
            }
            Type::Enum { named, .. } => {
                self.enums[named.id].variants.iter().flat_map(|v| &v.fields).any(|f| self.needs_drop(&f.ty))
            }
            Type::Tuple(items) | Type::Errors(items) => items.iter().any(|t| self.needs_drop(t)),
            Type::Array(item, _) | Type::Optional(item) => self.needs_drop(item),
            _ => false,
        }
    }

    /// Fields one after another with no padding, as `@packed` asks.
    fn packed_layout<'a>(&self, fields: impl Iterator<Item = &'a Type>) -> Option<(u64, u64)> {
        let mut size = 0;
        for field in fields {
            size += self.layout(field)?.0;
        }
        Some((size, 1))
    }

    /// Fields one after another, each aligned, the whole padded to its alignment.
    fn record_layout<'a>(&self, fields: impl Iterator<Item = &'a Type>) -> Option<(u64, u64)> {
        let (mut size, mut align) = (0, 1);
        for field in fields {
            let (field_size, field_align) = self.layout(field)?;
            size = round_up(size, field_align) + field_size;
            align = align.max(field_align);
        }
        Some((round_up(size, align), align))
    }
}

fn round_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_ranges() {
        assert!(IntTy::U8.fits(255));
        assert!(!IntTy::U8.fits(256));
        assert!(IntTy::I8.fits(-128));
        assert!(!IntTy::I8.fits(-129));
        assert!(!IntTy::U64.fits(-1));
    }
}
