# The Hesychia Book

Hesychia compiles to C, then to a small native binary. It has two levels:

- **`managed`**: Python-like, the compiler infers types, borrows and ownership.
- **`manual`**: every annotation written out, plus low-level access.

---

## 1. Getting started

### Hello world
The entry point of every program.
```python
from io import print

managed fn main():
    print("Hello, Hesychia!")
```

### Commands
Build, run, test, and create projects.
```sh
hsy run main.hsy                 # build and run
hsy build main.hsy --release     # optimized binary
hsy test                         # run @test functions
hsy new app                      # new project
```

### Comments
Notes for the reader; comments above a struct or field also serve as docs.
```python
# This is a comment
```

---

## 2. Values and variables

### Managed variables
Declare by assigning; the type is inferred.
```python
managed fn main():
    x = 5
    x += 1
    name = "Ada"
```

### Automatic widening
A managed variable grows its type when a value needs more room.
```python
count = 0                 # i32
count += 5000000000       # count becomes i64 for the whole function
```

### `mut` and `const`
Manual variables with a written type: `mut` changes, `const` stays.
```python
manual fn main():
    mut total: i64 = 0
    const limit: i64 = 10
    total += limit
```

### Number types
Integers of every width, floats, and sizes.
```python
const a: i8 = -5
const b: u64 = 42
const c: f64 = 3.14
const n: size = 10        # lengths and indexes
const big: u32 = 0b1010 | 0xff
```

### `bool` and `char`
Truth values and single characters.
```python
const ok: bool = True and not False
const letter: char = 'a'
```

### Arithmetic operators
Usual math, with `//` for integer division and `**` for powers.
```python
q = 17 // 5     # 3
r = 17 % 5      # 2
p = 2 ** 10     # 1024
```

### Bitwise operators
Work on the bits of integers.
```python
flags = 0b0101 & 0b0011 | 1 << 3 ^ ~0 >> 2
```

### Safe arithmetic
Overflow, division by zero and bad indexes stop the program; `wrapping_*` wraps on purpose.
```python
mut h: u32 = 4000000000
h = h.wrapping_add(500000000)
```

### Conversions
Change a number's type; `T(x)` stops the program if the value does not fit.
```python
const y: i32 = i32(3.99)          # 3
const s: Optional[u8] = u8.checked(300)   # None
const t: u8 = u8.truncate(300)    # 44, low bits
const bits: u32 = u32.reinterpret(1.0)
```

### Text to number
Parse a number from a string; failure raises `ParseError`, whose `text` is what was read
and `reason` says why ("not a whole number", "too large for `u8`").
```python
n = i64("42")
x = f64("2.5e1")
s = str(n)
try:
    u8("300")
except ParseError as e:
    print(e.reason)               # too large for `u8`
```
`base=` reads another base; its prefix is allowed:
```python
addr = u64("0x401000", base=16)
mask = u8("0b1010", base=2)
```

---

## 3. Strings

### `str`
UTF-8 text, measured in characters.
```python
s = "été"
len(s)          # 3
byte_len(s)     # 5
s = s + "!"
data = s.bytes()           # vector[u8], the UTF-8 bytes
back = str.from_bytes(data)  # Optional[str]
text = str.from_any_bytes(data)  # str: bad bytes become �
```

### f-strings
Insert values into text.
```python
print(f"{name} is {age} years old")
print(f"{point}")    # Point(x=1, y=2)
```

### Format specs
Control width, alignment, precision and base.
```python
f"{pi:.2f}"     # 3.14
f"{n:,}"        # 1,234,567
f"{name:>6}"    #    Ada
f"{255:x}"      # ff
f"{0.256:.1%}"  # 25.6%
```

### String methods
Everyday text tools from the prelude.
```python
" hi ".strip()
"a,b".split(",")
"Hi".upper()
"-".join(parts)
"abc".starts_with("a")
"ell" in "hello"
s[1:3]
```

---

## 4. Control flow

### `if` / `elif` / `else`
Choose a branch.
```python
if n < 0:
    print("negative")
elif n == 0:
    print("zero")
else:
    print("positive")
```

### `while`
Repeat while a condition holds.
```python
while i < 10:
    i += 1
```

### `for` over a range
Count with `range(start, end, step)`.
```python
for i in range(1, 11):
    total += i
for j in range(10, 0, -3):
    print(f"{j}")
```

### `break`, `continue`, `pass`
Leave a loop, skip a turn, or do nothing.
```python
for i in range(10):
    if i == 2:
        continue
    if i == 5:
        break
    pass
```

### `match`
Pick a case by value or shape; every case must be covered.
```python
match n:
    case 0:
        print("zero")
    case 1 | 2 | 3:
        print("small")
    case other:
        print(f"{other}")
```

### Match guards
Add a condition to a case.
```python
match n:
    case x if x < 0:
        print("negative")
    case _:
        print("other")
```

---

## 5. Functions

### Functions
Name a piece of code with typed parameters and a result.
```python
manual fn square(x: i64) -> i64:
    return x * x
```

### Defaults and named arguments
Leave out a parameter, or pass it by name.
```python
manual fn shift(dx: i32, dy: i32 = 1) -> i32:
    return dx + dy

shift(10)
shift(10, dy=5)
```

### Parameter modes
Say how a function receives a value: read `&`, change `&mut`, keep `owned`.
```python
manual fn show(v: &vector[i32]): ...
manual fn add(v: &mut vector[i32]): ...
manual fn keep(owned v: vector[i32]): ...
```

### Inferred modes
In `managed`, the mode comes from the body, and calls borrow by themselves.
```python
managed fn add_one(v: vector[i32]):   # &mut, since it pushes
    v.push(1)

managed fn main():
    v = vector[i32]()
    add_one(v)
```

### Tuples
Group values; read by position, or unpack.
```python
manual fn pair() -> (i64, str):
    return (1, "a")

p = pair()
print(f"{p[0]} {p[1]}")
n, s = pair()
```

### Function values
Pass a function as a value, compatible with C function pointers.
```python
manual fn apply(f: fn(i32) -> i32, x: i32) -> i32:
    return f(x)
```

### Lambdas
A one-expression function written in place.
```python
apply(lambda (x: i32) -> i32: x * 2, 21)
```

### Captures
Let a lambda use outer variables, always by reference, passed as `&fn`.
```python
manual fn each(values: &vector[i32], f: &fn(i32)): ...

mut seen: vector[i32] = []
each(&values, lambda [&mut seen] (x: i32): seen.push(x))
```

### Nested functions
Define a function inside another, with optional captures.
```python
manual fn main():
    mut lines: vector[str] = []

    manual fn record[&mut lines](x: i32):
        lines.push(f"{x}")

    record(1)
```

---

## 6. Collections

### Lists and `vector`
A growable list; `[1, 2, 3]` builds one.
```python
v = [1, 2, 3]
v.push(4)
last = v.pop()
v.insert(0, 9)
v.remove(1)
print(f"{len(v)} {v[0]}")
```

### Vector methods
Search, sort and slice from the prelude.
```python
v.sort()
v.reverse()
v.contains(3)
v.count(1)
part = v[1:3]
3 in v
v.swap(0, 1)
```

### `array`
A fixed-size list, sized at compile time.
```python
const a: array[i32, 4] = [1, 2, 3, 4]
```

### Iterating a vector
Read items with `&v`, change them with `&mut v`.
```python
for x in &values:
    print(f"{x}")
for x in &mut values:
    x *= 2
```

### Unpacking in loops
Take each item apart in the loop header.
```python
for name, age in ages.items():
    print(f"{name} {age}")
```

### List comprehensions
Build a list from another in one line.
```python
evens = [x * 2 for x in v if x > 0]
```

### `dict`
Map keys to values, kept in insertion order.
```python
ages = dict[str, i32]()
ages["ana"] = 31
ages["ana"] += 1
"ana" in ages
ages.get("eve")      # None
d = {'a': 1}
```

### `set`
Unique values.
```python
seen = {1, 2}
seen.add(3)
both = seen.intersection(other)
```

### Built-in helpers
Common functions on numbers and lists.
```python
min(a, b)
max(v)
sum(v)
```

---

## 7. Optional values

### `Optional[T]` and `None`
A value that may be absent.
```python
manual fn find(v: &vector[i32], x: i32) -> Optional[size]:
    for i in range(len(v)):
        if v[i] == x:
            return i
    return None
```

### Narrowing
After an `is not None` test, the value is used directly.
```python
found = find(v, 7)
if found is not None:
    print(f"at {found}")
```

---

## 8. Structs

### `struct`
Group named fields into one type.
```python
managed struct Point:
    x: i32
    y: i32

p = Point(x=1, y=2)
print(f"{p.x}")
```

### Field defaults
A field that may be left out.
```python
managed struct Options:
    path: str
    retries: i64 = 3

o = Options(path="a")
```

### Methods
Functions attached to a type, receiving `self`.
```python
manual struct Point:
    x: i32
    y: i32

    method sum(&self) -> i32:
        return self.x + self.y

    method shift(&mut self, dx: i32):
        self.x += dx

    method origin() -> Point:        # static
        return Point(0, 0)
```

### Managed methods
A plain `self`; its mode is inferred.
```python
managed struct Counter:
    hits: vector[i32]

    method record(self, n: i32):
        self.hits.push(n)
```

### `priv`
Keep a field or item visible only inside its file.
```python
manual struct Account:
    owner: str
    priv balance: i64
```

### Recursive structs
In `managed`, a field holding its own type goes to the heap by itself.
```python
managed struct Node:
    value: i32
    next: Optional[Node]
```

---

## 9. Enums

### Simple enums
A type with a fixed list of cases.
```python
managed enum Color:
    Red
    Green
    Blue

c = Color.Green
```

### Enums with data
Each case carries its own fields (`manual`).
```python
manual enum Shape:
    Circle(radius: f64)
    Rect(width: f64, height: f64)
    Empty
```

### Matching enums
Open a case and bind its fields.
```python
match shape:
    case Shape.Circle(r):
        return 3.14 * r * r
    case Shape.Rect(w, h):
        return w * h
    case Shape.Empty:
        return 0.0
```

### Matching by reference
`match &x:` binds parts as references and keeps `x` whole.
```python
match &shape:
    case Shape.Circle(r):
        print(f"{r}")
    case _:
        pass
```

---

## 10. Errors

### `raises` and `raise`
Declare and throw the errors a function can fail with.
```python
manual struct TooBig:
    value: i32

manual fn check_size(n: i32) -> i32 raises TooBig:
    if n > 100:
        raise TooBig(value=n)
    return n
```

### `check`
Mark a call that can fail in `manual`; the error goes up.
```python
manual fn double(n: i32) -> i32 raises TooBig:
    return check check_size(n) * 2
```

### `try` / `except`
Catch errors by type, by variant, or all the rest.
```python
try:
    print(f"{check parse(s)}")
except ParseError.InvalidChar(pos):
    print(f"bad char at {pos}")
except TooBig as e:
    print(f"{e.value}")
except _e:
    print("other error")
```

### Several error types
`|` lists them; `match` on the caught error tells them apart.
```python
manual fn open(n: i32) -> i32 raises NotFound | Denied: ...

except e:
    match e:
        case NotFound as m:
            print(m.path)
        case Denied:
            print("denied")
```

### Managed errors
No `check` needed; `raises` is inferred.
```python
managed fn double(n: i32) -> i32:
    return validate(n) * 2
```

### `panic`
Stop the program at once with a message.
```python
panic("unreachable state")
```

---

## 11. Ownership and memory

### Moves
Assigning or passing an owned value hands it over; the old name is closed.
```python
manual fn shout(owned text: str) -> str:
    return text + "!"

const a: str = "hi"
const b: str = shout(a)     # `a` is moved
```

### Borrows
Lend a value with `&` to read or `&mut` to change.
```python
bump(&mut counter)
print(f"{length(&segment)}")
```

### `.copy()`
Make an independent duplicate.
```python
const b: Point = a.copy()
```

### Automatic release
Owned memory is freed when its variable leaves scope.
```python
if True:
    const tmp: str = "freed at the end of this block"
```

### Managed copies
Where a move would be refused, `managed` inserts a copy; `--warn-copies` lists them.
```python
v = [1, 2]
a = v
b = v       # copied
```

### `heap[T]`
Put a value on the heap, often for recursive types.
```python
manual struct Node:
    value: i32
    next: Optional[heap[Node]]

const list: Node = Node(1, allocate(Node(2, None)))
const b: heap[Node] = allocate(Node(3, None))
const inner: Node = deallocate(b)    # the value back, its memory released
```

### `shared[T]`
One value with several owners, cycles collected (`managed`).
```python
alice = shared(Person(name="alice", friends=[]))
bob = shared(Person(name="bob", friends=[]))
alice.friends.push(bob)
bob.friends.push(alice)
```

### `Drop`
Run code just before a value is released.
```python
manual struct File(Drop):
    name: str

    method drop(&mut self):
        print(f"closing {self.name}")
```

---

## 12. Traits and generics

### Traits
A set of methods a type promises; methods may have defaults.
```python
trait Shape:
    method area(&self) -> f64
    method describe(&self) -> str:
        return f"area {self.area()}"

manual struct Square(Shape):
    side: f64

    method area(&self) -> f64:
        return self.side * self.side
```

### Operator traits
`Add`, `Sub`, `Mul`, `Div`, `Mod`, `Equal`, `Order` give a type its operators.
```python
manual struct Vec2(Add):
    x: i32
    y: i32

    method add(&self, other: &Vec2) -> Vec2:
        return Vec2(self.x + other.x, self.y + other.y)

const v: Vec2 = Vec2(1, 2) + Vec2(3, 4)
```

### Collection traits
`Index`, `IndexSet`, `Contains`, `Slice`, `Len` give a type `c[k]`, `in`, `c[a:b]`, `len(c)`.
```python
manual struct Bag(Len, Contains):
    items: vector[i32]

    method len(&self) -> size:
        return len(self.items)

    method contains(&self, item: &i32) -> bool:
        return self.items.contains(item)
```

### `@derive`
Write `Equal`, `Order`, `Hash` and `Show` from the fields.
```python
@derive(Equal, Order, Hash, Show)
manual struct Point:
    x: i32
    label: str
```

### Generic functions
One function for many types, limited by trait bounds.
```python
manual fn total[T: Add](values: &vector[T], owned start: T) -> T:
    mut sum: T = start
    for i in range(len(values)):
        sum = sum + values[i]
    return sum
```

### Generic structs and enums
Types with type parameters.
```python
manual struct Pair[T]:
    first: T
    second: T

manual enum Result[T, E]:
    Ok(value: T)
    Err(error: E)

const p: Pair[i32] = Pair(1, 2)
```

### Value parameters
A type parameter that is a compile-time integer.
```python
manual fn total[N: size](values: &array[i64, N]) -> i64:
    mut sum: i64 = 0
    for i in range(N):
        sum += values[i]
    return sum
```

---

## 13. Modules and projects

### Imports
A file is a module, a folder is a package.
```python
import shop.cart              # shop/cart.hsy, used as cart.name
from io import print
from lib.util import twice as double
```

### Projects
`hesychia.toml` names the project, its dependencies and settings.
```toml
[project]
name = "app"
version = "0.1.0"

[dependencies]
math = { path = "../math" }
text = { git = "https://example.com/text.git", tag = "v1.0" }

[settings]
links = ["curl"]
targets = ["x86_64-linux-gnu", "x86_64-windows-gnu"]
```

### Cross-compiling
Build for another machine.
```sh
hsy build main.hsy --target x86_64-windows-gnu
```

---

## 14. Compile time

### `comptime` constants
Values computed while compiling.
```python
comptime BLOCK: size = 256 * 4
comptime PRIMES: array[i32, 4] = [2, 3, 5, 7]
```

### `comptime if`
Keep only the code for the target machine.
```python
comptime if target.os == "windows":
    extern fn GetTickCount() -> u32
else:
    extern fn clock() -> i64
```

### `comptime assert`
Stop compiling when a condition fails.
```python
comptime assert(size_of[u64]() == 8, "u64 must be 8 bytes")
```

### `comptime fn`
A function run by the compiler, on values or types.
```python
comptime fn fact(n: i64) -> i64:
    if n <= 1:
        return 1
    return n * fact(n - 1)

comptime N: i64 = fact(5)
```

### Reflection
Loop over the fields of a struct at compile time.
```python
comptime for f in fields(T):
    print(f"{f.name} = {field(x, f)}")
```

### Layout queries
Size and alignment of a type.
```python
size_of[i64]()      # 8
align_of[i128]()    # 16
```

---

## 15. Concurrency

### Threads
Run a function on another thread and get its result.
```python
from thread import spawn

t = spawn(square_sum, 100)
print(f"{t.join()}")
```

### Channels
Send values between threads.
```python
from thread import channel

tx, rx = channel[str]()
tx.send("hello")
msg = rx.receive()       # None once every sender is gone
```

### `atomic`
A number or bool safely shared by threads.
```python
from thread import atomic

counter = atomic(0)
counter.add(1)
print(f"{counter.load()}")
```

### `mutex`
A value reached by one thread at a time, inside `with`.
```python
from thread import mutex

total = mutex(0)
with total as n:
    n += 1
```

### Scoped threads
Threads that borrow local values; the block waits for them.
```python
from thread import scope

with scope() as s:
    low = s.spawn(part_sum, &values, 0, 500)
    high = s.spawn(part_sum, &values, 500, 1000)
    print(f"{low.join() + high.join()}")
```

### `parallel for`
Spread a loop over every core.
```python
parallel for x in &mut values:
    x *= 2
```

### `select`
Wait for the first ready channel, with a timeout or `else`.
```python
select:
    case msg = rx.receive():
        print(f"{msg}")
    case time.sleep(2, unit=time.Unit.S):
        print("timeout")
```

---

## 16. Low level (`manual`)

### `extern fn`
Call a C function.
```python
extern fn printf(fmt: ptr[u8], ...) -> i32

printf("%d\n".to_c(), 42)
```

### `@symbol`
Give an extern function a name of its own.
```python
@symbol("abs")
extern fn c_abs(x: i32) -> i32
```

### C callbacks
Pass a `manual` function to C.
```python
extern fn qsort(base: ptr[i32], count: size, width: size, compare: fn(ptr[i32], ptr[i32]) -> i32)

manual fn descending(a: ptr[i32], b: ptr[i32]) -> i32:
    return b[0] - a[0]

qsort(ptr(&mut values[0]), 5, size_of[i32](), descending)
```

### Pointers
Raw, unchecked memory access.
```python
const p: ptr[i32] = ptr(&mut values[0])
p[2] = 7
const q: ptr[i32] = p + 2
const bytes: ptr[u8] = ptr[u8](p)
p.volatile_write(p.volatile_read() + 1)
```

### `@packed` and `@align`
Control a struct's memory layout.
```python
@packed
manual struct Header:
    tag: u8
    length: u32

@align(16)
manual struct Block:
    value: u8
```

### `asm`
Inline Intel assembly with operands bound to registers.
```python
mut sum: u64 = a
asm(inout("rax") sum, in("rcx") b):
    "add rax, rcx"
```

### `SIMD`
Act on several numbers at once.
```python
const a: SIMD[f32, 4] = SIMD[f32, 4](1.0, 2.0, 3.0, 4.0)
const twos: SIMD[f32, 4] = SIMD[f32, 4](2.0)
const p: SIMD[f32, 4] = a * twos
print(f"{p[0]}")
```

### `@panic_handler`
Run code before the program stops on a fault.
```python
@panic_handler
manual fn on_panic(message: &str):
    print(f"handled: {message}")
```

---

## 17. Testing

### `@test`
A function run by `hsy test`.
```python
from testing import expect, equal

@test
managed fn sorts():
    v = [3, 1, 2]
    v.sort()
    equal(v, [1, 2, 3])
    expect(len(v) == 3)
```

---

## 18. Standard library

### `io`
Read and write text.
```python
from io import print, input, read, stdin, stderr

name = input("name? ")
print("oops", to=stderr)
line = read(stdin)
```

### `fs`
Files and paths.
```python
import fs

f = fs.open("out.txt", fs.Mode.Write)
print("hi", to=f)
data = fs.read_bytes("in.bin")
path = fs.join("data", "sales.csv")
```

### `math`
Common math functions and constants.
```python
import math

math.sqrt(2.0)
math.sin(math.pi)
math.log(8.0, base=2.0)
```

### `time`
Clocks, durations and dates.
```python
import time

start = time.now()
time.sleep(10, unit=time.Unit.Ms)
print(f"{time.elapsed(start, unit=time.Unit.Ms)}")
```

### `os`
Command line, environment and processes.
```python
import os

words = os.args()
home = os.env("HOME")
code = os.run(["git", "status"])
```

### `random`
Random numbers.
```python
import random

n = random.int(1, 6)
pick = random.item(v)
random.shuffle(v)
```

### `args`
Read the command line into a struct.
```python
import args

managed struct Options:
    # The file to read
    source: str
    retries: i64 = 3
    @short("v")
    verbose: bool = False

o = args.parse[Options]()      # tool a.txt --retries 5 -v
```

### `scan`
Read a text piece by piece, for a parser of your own.
```python
import scan
from scan import Class

s = scan.over("port = 8080")
key = s.take_while(Class.Word)    # "port"
s.skip_space()
s.expect("=")                     # raises ScanError(line, column, reason) if absent
s.skip_space()
port = i64(s.take_while(Class.Digit))
```

### `json`
Read and write JSON as structs.
```python
import json

user = json.to_struct[User](text)
print(json.to_text(user, indent=2))
```

### `csv`
Load and write tables.
```python
import csv

rows = csv.load[dict](f)          # from a file
table = csv.parse[vector](text)   # from a text
csv.dump(rows)
print(csv.to_text(table, sep=";"))
```
A struct reads each row, its fields from the columns of their names:
```python
users = csv.load[User](f)         # vector[User]
csv.dump(users)                   # the field names, then a row each
```

### `re`
Regular expressions in Python's syntax.
```python
import re

m = re.search("(\\d+)", "abc 42")
if m is not None:
    print(m.group(1))
```

### `url`
Parse and build URLs.
```python
import url

u = url.parse("https://a.com:8080/p?q=1")
print(f"{u.host} {u.port}")
if u.scheme == url.Scheme.Https:
    print("secure")
```

### `html`
Read the tags and the text of a page, given as bytes.
```python
import html

for tag in html.tags(page):
    if tag.name == "a":
        href = html.attribute(tag, "href")   # Optional[str]
        if href is not None:
            print(href)
print(html.text(page))
safe = html.escape(name.bytes())
```

### `http`
Requests to `http://` and `https://` addresses.
```python
import http

r = http.get("https://example.com/")
if r.status == http.Status.Ok and http.media_type(r) == "text/html":
    tags = html.tags(r.body)
print(f"{http.code(r.status)} {r.final_url}")
sent = http.post("https://example.com/search", "q=tea".bytes())
```

### `http.Session`
Requests that share a connection and cookies.
```python
session = http.Session()
session.post("https://site/login", "user=ann&pw=secret".bytes())
profile = session.get("https://site/profile")     # the login cookie goes with it
session.set_cookie("site", "PHPSESSID", "abc123")
sid = session.cookie("site", "PHPSESSID")         # Optional[str]
session.close()
```

### `net`
TCP and UDP connections.
```python
import io
import net

conn = net.connect("example.com", 80)
print("GET / HTTP/1.0\r\n\r\n", to=conn)
reply = io.read(conn, end="")
```

### TLS
The same connection, encrypted; the certificate is checked.
```python
conn = net.connect_tls("example.com", 443)
test = net.connect_tls("localhost", 8443, verify=False)   # self-signed
```

### TLS servers and certificates
Make a certificate authority, sign a server certificate with it, and serve TLS.
```python
ca = net.certificate("Lab CA", authority=True)
cert = net.certificate("localhost", names=["127.0.0.1"], signer=ca)
listener = net.listen_tls("0.0.0.0", 8443, cert)
conn = net.accept(listener)                          # a TLS connection

client = net.connect_tls("localhost", 8443, authority=ca)
quick = net.certificate("localhost")                 # self-signed
saved = net.load_certificate("server.pem", "server.key")
```

### `binary`
Read and write numbers in byte buffers, with bounds checks.
```python
import binary

n = binary.read_u32(data, 0, binary.Order.Little)
binary.push_u16(out, 512, binary.Order.Big)
```

### `elf`
Read an ELF binary into sections, segments and symbols.
```python
import elf

file = elf.parse(data)
print(f"{file.header.entry}")
sym = elf.find_symbol(file, "main")
```

### `elf` editing
Patch an ELF in place and emit the result.
```python
ed = elf.edit(data)
ed.set_entry(0x401000)
ed.patch(offset, &bytes)
out = ed.build()
```

---

## 19. OffSec

### `offsec.cyclic`
De Bruijn pattern to find the overflow offset of a saved return address.
```python
from offsec.cyclic import pattern, padding

send = pattern(200)
off = padding(0x6161616c)     # Optional[size] from the crash value
```

### `offsec.payload`
Join raw bytes, text and addresses into one packed payload.
```python
from offsec.payload import flat, Part

p = flat([Part.Bytes(sled), Part.Address(0x401000), Part.Text("sh")])
```

### `offsec.shellcode`
Ready-made null-free `execve("/bin/sh")` machine code.
```python
from offsec.shellcode import sh
from binary import Width

code = sh(Width.Bits64)
```

### `offsec.encode`
Reversible XOR encoding to dodge bad bytes.
```python
from offsec.encode import xor, has_byte

enc = xor(data, key)
bad = has_byte(enc, u8(0))
```

### `offsec.rop`
Find gadgets in an ELF and build a return-oriented chain.
```python
from offsec.rop import find, chain, Gadget

pop = find(data, file, Gadget.PopRdi)
c = chain(64)
c.set_reg(pop, 0x404000)
bytes = c.bytes()
```

### `offsec.fmt`
Build a `printf` format-string write of a value to an address.
```python
from offsec.fmt import write
from binary import Width

payload = write(0x404040, 0xdeadbeef, offset=6, bits=Width.Bits64)
```

### `offsec.tube`
Bridge your terminal to a connection after a shell opens.
```python
from offsec.tube import interactive

interactive(conn)     # until Ctrl-D
```