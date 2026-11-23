# Hesychia

Hesychia is a compiled programming language with two levels:

- **`managed`**: Python-like syntax where the compiler infers types, borrows and ownership.
- **`manual`**: the same language with every annotation written out, plus low-level access (pointers, `extern`, assembly, SIMD).

The compiler, `hsy`, is written in Rust and emits C11, which the system C compiler turns into a small native binary.

## Install

Hesychia needs `git`, [Rust](https://rustup.rs) 1.88 or later to build the compiler, and a C compiler.

```sh
curl -fsSLO https://raw.githubusercontent.com/The-Hesychia-language/Hesychia/main/install.sh
chmod +x install.sh && ./install.sh
```

The script downloads the sources into `~/.hesychia/src`, builds `hsy` and puts it in `~/.hesychia/bin`, which it adds to `PATH` in `~/.bashrc` and `~/.zshrc` (and `~/.bash_profile` on macOS). Run it again to update: it fetches the latest sources and replaces the old `hsy`. `HESYCHIA_HOME` installs elsewhere than `~/.hesychia`. To remove Hesychia, delete `~/.hesychia` and the two lines under `# Hesychia` in those files.

## Learn

[hsy-train](https://github.com/The-Hesychia-language/training-tool) teaches the language through exercises in the terminal: programs to fix, and small ones to build. Inspired by Rustlings and Leet code challenges.

## Status

Milestones 1 to 6: the `manual` level with structs, enums, ownership, strings, vectors, errors, traits, generic functions and lambdas, across several modules. Milestone 7: the `managed` level.

| Supported | Not yet |
| --- | --- |
| `manual fn`, parameters with defaults (compile-time values, including a variant without fields such as `Speed.Slow`), named arguments | |
| `mut` / `const` variables with explicit types, tuple destructuring (`a, b = pair` in `managed`) | |
| Integers (`i8`…`i128`, `u8`…`u128`, `size`, `offset`), `f32`, `f64`, `bool`, `char`; conversions `T(x)` (stops if it does not fit), `T.checked(x)` (`Optional[T]`), `T.truncate(x)` (low bits; floats toward zero, saturated), `T.reinterpret(x)` (same bits, same size) | |
| `struct` with fields, `priv` fields, methods (`&self`, `&mut self`, `owned self`) and static methods | |
| `enum` (C-like in `managed`, with data in `manual`, recursive through a `vector` or a `dict`), `match` with exhaustiveness, `\|` patterns, `case pattern if condition:` guards, which read what the pattern binds and leave the following cases to be tried when they fail; `match x:` moves the parts it binds out of `x`, `match &x:` and a match on a reference bind them as `&` references | |
| `Optional[T]`, `None`, `is None` / `is not None` with narrowing | |
| `for x in &v` over a vector (items by value if copied implicitly, through `&` otherwise), `for x in &mut v` (items changed through `&mut`), and in `manual` `for x in v`, which moves `v` and each item into `x`; `managed` borrows by itself, with `&mut` when the body changes the items; a `managed` loop that changes the vector it reads iterates on a copy | |
| Tuples (`pair[0]`, by a position written in the code), `array[T, N]`, `vector[T]` (`len(v)`, push, pop, insert, remove, clear), bounds-checked indexing | |
| `str` (`len(s)` in characters, `byte_len(s)` in bytes, `+`, `+=`, `==`, f-strings, `to_c()`, `str.from_c()`, `s.byte(i)` checked against the length, `str.from_bytes(&v)` from a `vector[u8]`, `None` unless it is UTF-8, `hash()`), `print(text, end="\n", to=stdout)` from `io`; f-strings write structs, enums, tuples, lists and `Optional` values as Python does: `Point(x=1, y=2.0)`, `Shape.Circle(radius=1.5)`, `[1, 2]`, `None`, with text quoted inside them (`Person(name='Ann')`) | |
| Threads (module `thread`): `spawn(f, args...)` gives a `thread[T]`, `t.join()` its result; arguments are moved, and nothing holding a reference or a `shared` value goes to another thread; a thread not joined is waited for when its handle goes away; a `managed` function given to `spawn` owns its parameters; `thread` alone when the function returns nothing; `channel[T]()` gives `(sender[T], receiver[T])`: `tx.send(x)` moves `x`, `tx.copy()` adds a sender, `rx.receive()` waits and gives `None` once every sender is gone; `atomic(x)` gives an `atomic[T]` (integer or `bool`) with `load`, `store`, `exchange`, `compare_exchange`, and `add`, `sub` for integers, usable through `&`; `with scope() as s:` starts threads with `s.spawn(f, args...)` that may borrow (`&x`, `&mut x`, or a lambda that captures) what lives outside the block: the block waits for them at its end, and a value lent with `&mut` cannot be used meanwhile, nor one lent with `&` be changed; `parallel for` and `select` (below) | |
| Ownership: moves, `&` / `&mut` borrows, `.copy()`, `heap[T]` with `allocate()`, automatic release | |
| Errors: `raises A \| B`, `raise`, `check f(...)`, `try` / `except` with patterns, `panic(message)` | |
| Traits with default methods; `Equal`, `Add`/`Sub`/`Mul`/`Div`/`Mod`, `Drop`; generated `==` | |
| Generic functions `fn f[T: A + B]`, structs `struct Pair[T]` and enums `enum Result[T, E]`, type arguments inferred or written, specialized per use | |
| `fn(A) -> R` values, lambdas, nested functions; captures by `&` / `&mut` only, by design: a lambda never owns what it captures; passed as `&fn(...)` | |
| `extern fn`, including variadic C functions, from C libraries named by `[settings] links`, and `@symbol("sqrt")` above one to give it a name of its own; `Optional[ptr[T]]` maps to `NULL`; a `manual` function passes as a C callback (`qsort(..., compare)`), to a variadic C function too (`curl_easy_setopt(h, option, write)`), where `None` is `NULL`; pointers in `manual`, unchecked: `ptr(&x)`, `p[i]`, `p + n`, `p2 - p1`, `ptr[U](p)`, `p.volatile_read()`, `p.volatile_write(x)` | |
| `@packed` and `@align(N)` on `manual` structs; `@panic_handler` on one `manual fn name(message: &str)`, called before the program stops on a fault or a `panic` | |
| `asm(in("reg") x, out("reg") y, inout("reg") z, clobber("reg")):` with lines of Intel assembly, in `manual`; each operand is bound to its register (`-masm=intel` is passed to the C compiler) | |
| `SIMD[T, N]` (gcc and clang vector extensions): `SIMD[f32, 4](1.0, 2.0, 3.0, 4.0)`, or one value for every lane; `+ - * /`, and `& \| ^` on integer lanes, act lane by lane; `v[i]` reads and writes a lane | |
| `managed fn`: `x = 5` declares, types widened for the whole function, checked conversions with a warning at typed boundaries; parameter modes (`&`, `&mut`, `owned`) inferred from the body, arguments borrowed automatically; hidden copies where `manual` would refuse a move (reuse after a move, a part of a value, a loop), never for `Drop` types; errors: no `check`, `raises` inferred, an error escaping `main` stops the program; methods take a plain `self` whose mode is inferred; `managed` uses generic types (`vector[str]`, a `Stack[T]` written in `manual`) and calls generic `manual` functions, but declares no type parameters; a list literal `[1, 2, 3]` is a `vector` unless an `array` is asked for; a temporary value is lent to a `&` parameter for the call, then released; list comprehensions `[x * 2 for x in v if x > 0]`; a field holding its own struct (`next: Optional[Node]`) is placed on the heap by itself; so is a variable larger than 64 KiB (`[settings] heap_threshold` in `hesychia.toml` changes the limit); `shared(x)` gives `shared[T]`, one value with several owners, counted by reference with cycle collection; a warning points out a copy that the code seems to expect shared; `manual` cannot call `managed` | |
| Modules and packages, `comptime` constants and tables (`comptime PRIMES: array[i32, 4] = [2, 3, 5, 7]`, whose items are values known when compiling), `comptime if`, `comptime assert`, `comptime fn` on values and types, `comptime for f in fields(T):` over the fields of a struct, `size_of`, `align_of`, `target.*` | |

Integer overflow, out-of-range conversions, division by zero, oversized shifts, negative exponents and out-of-bounds indexing stop the program in both debug and release builds. Where wrapping is wanted, as in hashes and random generators, `a.wrapping_add(b)`, `wrapping_sub` and `wrapping_mul` compute modulo the type's range. Unused variables produce a warning; only the functions reachable from `main` end up in the binary.

### Managed variables

```python
managed fn steps(limit: i32) -> i64:
    count = 0               # default to i32
    i = 0
    while i < limit:
        count += 5000000000 # automatically widened count to i64 for the whole function
        i += 1
    return count
```

Mixed numbers take the type holding both (`u8 + i32` is `i32`, `i32 + f32` is `f64`); a length (`size`, from `len`) meets signed integers as an `i64`; `u64` with `i64`, and 64-bit integers with floats, are rejected. Where a type is written (a parameter, a return value), a narrower one is converted with a check and a warning.

A parameter written without a mode is `&` if the body only reads it, `&mut` if it changes it, and `owned` if it keeps it (returns it, stores it, passes it to an `owned` parameter); calls borrow for it:

```python
managed fn add_one(v: vector[i32]):   # &mut: it pushes
    v.push(1)

managed fn main():
    v = vector[i32]()
    add_one(v)                         # borrowed, no `&mut` to write
```

Where `manual` would refuse a move, `managed` copies instead: a value used again after it was moved, a field or item taken by value, a value moved inside a loop. Types implementing `Drop` are never copied.

Errors need no `check` in `managed`, and each function's `raises` is inferred from what it lets escape. An error escaping `main` stops the program with its type.

`shared(x)` makes a value with several owners; changes through one owner are seen by all, and cycles are collected:

```python
managed struct Person:
    name: str
    friends: vector[shared[Person]]

managed fn main():
    alice = shared(Person(name="alice", friends=vector[shared[Person]]()))
    bob = shared(Person(name="bob", friends=vector[shared[Person]]()))
    alice.friends.push(bob)
    bob.friends.push(alice)   # a cycle, released at the end anyway
    bob.name = "robert"       # alice.friends[0].name is "robert"
```

### Threads

```python
from thread import spawn, channel, scope, atomic

manual fn part_sum(values: &vector[i64], start: size, end: size) -> i64: ...

manual fn main():
    const values: vector[i64] = [1, 2, 3, 4]
    const counter: atomic[i64] = atomic(0)
    with scope() as s:                  # threads that borrow: waited for at the end
        const low: thread[i64] = s.spawn(part_sum, &values, 0, 2)
        const high: thread[i64] = s.spawn(part_sum, &values, 2, 4)
        s.spawn(lambda [&counter] () -> i64: counter.add(1))
    const (tx, rx): (sender[str], receiver[str]) = channel[str]()
    tx.send("hello")                    # moved into the channel
```

A `mutex` holds a value that one thread at a time reaches. There is no `lock()` to forget: the value is only reached inside `with`, which locks, and unlocks when the block is left, by `return` or `break` too. A `mutex` is lent to threads like an `atomic`. Locking a mutex inside its own `with` is refused.

```python
total = mutex(0)
parallel for i in range(1000):
    with total as n:
        n += i
```

`parallel for` runs its body on every core, over `range(...)`, `&v` or `&mut v` (in `managed`, `v`, lent with `&mut` when the body changes its item). The body reads what is outside it, and changes only its item or an `atomic`; it cannot `break`, `return` or let an error out, and `continue` ends its item's turn. The items are cut into one slice per core, and the loop ends when every slice is done.

```python
parallel for x in &mut values:
    x *= 2
parallel for i in range(n):
    hits.add(1)
```

`select` waits for the first message among several receivers. A message is an `Optional`, `None` once the channel has no sender left, as `receive()` gives it. A `time.sleep(...)` case limits the wait, and `else` runs at once when nothing is ready. When several channels are ready, one is taken at random.

```python
select:
    case msg = rx.receive():
        print(f"{msg}")
    case n = counts.receive():
        print(f"count {n}")
    case time.sleep(2, unit=time.s):
        print("nothing for 2 seconds")
```

### Traits and generics

```python
trait Shape:
    method area(&self) -> f64

manual struct Square(Shape):
    side: f64

    method area(&self) -> f64:
        return self.side * self.side

manual fn total[T: Add](values: &vector[T], owned start: T) -> T:
    mut sum: T = start
    for i in range(len(values)):
        sum = sum + values[i]
    return sum
```

A struct names the traits it implements and defines their methods; a missing or mismatched method is an error. `Add`, `Sub`, `Mul`, `Div` and `Mod` enable the operators through `add`, `sub`, `mul`, `div` and `mod`; `Equal` replaces the generated `==` with `equal`; `Drop` runs `drop(&mut self)` before release. Six more give a struct the syntax of a collection, each by a call of its method: `Index` makes `c[k]` the place `index(&self, key: &K) -> ptr[V]` points to, so `c[k] += 1` and `c[k].push(x)` work, and the place cannot be kept past the statement; `IndexSet` makes `c[k] = v` call `set_index(&mut self, owned key: K, owned value: V)`; `Contains` gives `x in c` and `x not in c` through `contains(&self, item: &T) -> bool`; `Slice` gives `c[a:b]`, `c[a:]`, `c[:b]` through `slice(&self, start: size, end: size) -> Self`, an end left out being the largest `size`; `Len` gives `len(c)` through `len(&self) -> size`; `Hash` asks for `hash(&self) -> u64`, which integers, `bool`, `char` and `str` already have; `Order` asks for `compare(&self, other: &Self) -> i32` and gives `<`, `<=`, `>`, `>=`, which numbers, `char` and `str` (byte order) already have; `Show` asks for `show(&self) -> str`, which f-strings then use to write the struct. `K`, `V` and `T` are whatever the method uses. A generic body only uses what its bounds allow; each set of types it is called with becomes its own C function. A generic struct, `struct Pair[T]`, becomes one struct per set of types used; `Pair(1, 2)` takes its type arguments from its fields or from the expected type, and its methods are checked once called. It can be bounded and implement traits: `struct Pair[T: Add](Add, Equal)`; its static methods take their types from their arguments, `Pair.make(1, 2)`. Methods can have type parameters of their own: `method map[U](&self, f: &fn(&T) -> U) -> Pair[U]`. A parameter can also be a compile-time integer: `fn total[N: size](values: &array[i64, N])` takes `N` from the array, or from `total[4](...)`; `array[T, N]` also accepts a `comptime` constant as its size. Generic enums work the same way: `Result.Ok(1)` takes its types from its values or from the expected type, and `case Result.Err(e):` matches it. Type parameters are declared in `manual` only; `managed` code uses what they declare.

### Functions as values

```python
manual fn each(values: &vector[i32], f: &fn(i32)):
    for i in range(len(values)):
        f(values[i])

manual fn main():
    mut seen: vector[i32] = []
    each(&values, lambda [&mut seen] (x: i32): seen.push(x * 2))

    manual fn record[&mut seen](x: i32):
        seen.push(x)
```

`fn(A) -> R` is a plain function pointer, compatible with C. A lambda (one expression) or a nested function that captures, by reference only, is a borrowed function: it is called in its block or passed to a `&fn(...)` parameter, never stored or returned.

### Errors

A function that can fail declares its error types, which are structs or enums. Every call to it is marked with `check`; the error goes to the enclosing `try`, or back to the caller, which must declare it too. Errors are ordinary return values in the generated C: there is no unwinding.

```python
manual enum ParseError:
    Empty
    InvalidChar(pos: size)

manual fn parse(s: &vector[u8]) -> u16 raises ParseError:
    if len(s) == 0:
        raise ParseError.Empty
    ...

manual fn show(s: &vector[u8]):
    try:
        print(f"{check parse(s)}")
    except ParseError.InvalidChar(pos):
        print(f"invalid character at {pos}")
    except _e:
        print("empty input")
```

`except T as e:` catches one type, `except T.Variant(...)` one variant, and `except e:` whatever is left (a name starting with `_` avoids the unused-variable warning). When several types are left, `e` holds one of them, in a hidden enum made by the compiler: `raise e` sends it on with its own type. `match e:` tells them apart by type: `case NotFound as missing:` binds the error, `case Disk:` only tests it; the cases must cover every type, or end with `_`. Errors no clause catches go on to the caller. An error raised in the middle of an expression releases what the expression had already computed: the compiler moves those values into variables first, in evaluation order (left to right); the right side of `and` / `or` gets a block of its own, run only when needed, and a `while` condition is computed again at the start of each turn. `panic("message")` stops the program instead.

### Ownership

Values that own memory (`str`, `vector`, `heap`, and anything containing them) are released when their variable goes out of scope, including on `return`, `break` and `continue`. Assigning a new value releases the old one. Moving a value out of a variable leaves it empty in the generated C, so it is never released twice; the checker refuses any later use of it.

```python
from io import print

manual fn shout(owned text: str) -> str:
    return text + "!"

manual fn main():
    mut words: vector[str] = ["a", "tree"]
    words.push(f"{len(words)} words")
    print(shout(words.remove(1)))     # tree!
```

### Modules

The standard library is written in Hesychia, in `stdlib/`, and built into `hsy`: nothing is fetched. `io` and `thread` are standard modules: `from io import print`, or `import io` and `io.print(...)`; `print`, `spawn` and a few others are provided by the compiler itself. The prelude, `stdlib/prelude.hsy`, is loaded with every program and its public names need no import; it adds methods to `str` and `vector` with `extend str:` / `extend vector[T: Order]:` blocks, which only the standard library may write.

| Prelude | |
|---|---|
| `str` | `starts_with`, `ends_with`, `find` (a character position), `strip()` and `strip(side=Side.Start)` or `side=Side.End`, `split(sep)` and `split()` on spaces, `replace`, `upper`, `lower` (Unicode, as Python: `"straße".upper()` is `"STRASSE"`), `sep.join(parts)`, `s[a:b]` by characters, `part in s` |
| `vector` | `contains`, `find`, `count`, `reverse`, `extend`, `sort` (stable), `v[a:b]` (a copy), `x in v`; `swap(i, j)` is the compiler's |
| `dict[K, V]` | `d = dict[str, i32]()`, `d[k] = v`, `d[k] += 1`, `k in d`, `len(d)`, `get`, `remove`, `clear`, `keys`, `values`, `items`, in the order entries were added; a missing key in `d[k]` stops the program; written `{'a': 1}` |
| `set[T]` | `add`, `remove`, `in`, `len`, `clear`, `values`, `union`, `intersection`; written `{1, 2}`, or `set()` when empty |
| functions | `min(a, b)` and `min(v)` (`None` for an empty vector), the same for `max`, `sum(v)`, `sum(v, start)` |
| conversions | `i64("42")`, `u8(s)`, `f64("2.5e1")`... raise `ParseError` (spaces around are allowed); `str(x)` writes `x` as `f"{x}"` does |

| Module | |
|---|---|
| `io` | `print(text, end="\n", to=stdout)`; `read(f, end="\n")` up to `end` (`end=""` reads everything), `None` once nothing is left; `read_bytes_until(f, delimiter="\n", keep=False)` the same as raw bytes rather than text, for a binary stream, raising `IoError` if the stream ends before a non-empty `delimiter`; `input(prompt)`; `flush(f)`, `close(f)`; `shutdown(f)` half-closes a connection in both directions, so a read blocked on it in another thread ends at once (nothing for a file or stream); `stdin`, `stdout`, `stderr`; `File`, `IoError`. In `manual` only: `sys_write(f, &bytes)`, one system call giving how many bytes went out, and `sys_read(f, n)` |
| `fs` | `open(path, fs.Mode.Write)` (`Mode.Read` by default, `Mode.Write`, `Mode.Append`, and `Mode.Create` for a new file, refused if anything is already at its path, a link included) for an `io.File` closed at the end of its scope; `exists`; `kind(path, fs.Kind.Dir)` with `Kind.File`, `Kind.Dir`, `Kind.Link`; `extension(path, "csv")`; `join("data", "sales.csv")`, `parent(path)` (`"."` for a name alone), `name(path)`, `stem(path)` (the name without its extension), with `/`, and on Windows `\` too, as separators; `list_dir` (paths, such as `data/sales.csv`); `make_dir(path, parents=False, mode=0o755)`, where `parents=True` creates the missing folders along the path and takes a folder already there, as `mkdir -p`, and `mode` sets the permissions of the folder (less the umask; none on Windows); `rename(old, new)`, which replaces a file already at `new` on every system; `remove(path, recursive=False)`. Bytes: `read_bytes(path)` gives the whole file, and `read_bytes(path, limit=n)` raises `IoError` for a file larger than `n` bytes, as one from outside can be of any size; `write_bytes(path, &bytes)` writes atomically by default: to a new file beside it, written to the disk, given the old file's permissions, then moved over it (through a link, onto its target), so a reader sees the old content or the new, never half; `atomic=False` writes in place. `write_unique(path, &bytes)` writes a new file at `path`, or at `stem_1.ext`, `stem_2.ext`... when the name is taken (`archive.tar_1.gz`, `README_1`), and gives the path written: it never overwrites nor follows what is already there, as the name is taken by the system in one step, and is atomic as `write_bytes`; after 10,000 names taken it raises `IoError`. For names and paths taken from outside (a URL, an HTTP header, an archive), where `../../.bashrc` would write outside the folder: `sanitize_name(text)` makes a plain file name, valid and staying in its folder on every system, by replacing with `_` control characters, `/ \ : < > " | ? *`, the Unicode controls that reverse the reading order, and a leading `.`, ` ` or `-`, dropping trailing dots and spaces, prefixing Windows device names (`CON`, `NUL`, `COM1`...) and cutting to 255 bytes (`ascii=True` replaces every non-ASCII character too, `fallback=` names what an empty result becomes; the extension is left for the caller to judge); `join_within(folder, path)` joins and raises `IoError` if the result would leave `folder` (`..` above it, an absolute path, a drive), keeping subfolders, with `/` and `\` separators on every system: it checks the text only, not links already in `folder`. `stat(path)` gives a `Stat`: `size`, `kind`, `modified`, `accessed`, and `changed` (Unix ctime) and `created` as `Optional`s, `None` where the system does not record them (no `changed` on Windows, no `created` on some Linux file systems); dates are seconds as `time.timestamp()` gives, and `follow=False` describes a link itself |
| `math` | `sqrt`, `floor`, `ceil`, `round`, `abs` (integers and reals), `sin`, `cos`, `tan`, `atan2`, `log(x, base=e)`, `exp`, `is_nan`; `pi`, `e`, `inf`, `nan`; powers are `x ** y` |
| `time` | `now()`, `elapsed(start)`, `sleep(x)`, `timestamp()`, all in seconds unless `unit=` is `time.h`, `time.m`, `time.ms`, `time.us` or `time.ns`; `date(t)` gives a `Date` written `2026/11/16 00:06:00`, in UTC |
| `os` | `args()`, `env(name)`, `cwd()`, `run(["git", "status"])` giving the exit code, `exit(code)` |
| `random` | `int(low, high)`, `float(low, high)` (0 to 1 by default), `item(v)`, `shuffle(v)`, `seed(n)`, from a generator each thread keeps (xoshiro256**, seeded by the clock); `sys_bytes(n)` asks the system for unpredictable bytes, for keys and tokens |
| `csv` | `load[vector](f, sep=",")` to rows of fields, `load[dict](f)` to a `dict` per row keyed by the header; `dump(rows, to=stdout)` writes either; `CsvError` |
| `json` | `to_struct[User](text)` reads a text as a struct, and `to_text(user, indent=0)` writes one: each field from the key of its name. A field is a text, a number, a `bool`, a struct, a `vector`, a `dict` from texts, an `Optional` (absent or `null` is `None`) or a `Value`; keys that name no field are passed over; `to_struct[vector[User]](text)` reads a list. `load[User](f)` and `dump(user, to=stdout, indent=0)` do the same with a file. Without a type, `parse(text)` and `load(f)` give a `json.Value` (`Null`, `Bool`, `Number`, `Text`, `List`, `Object`). `JsonError` says what is wrong, and where: `users[2].age: expected a number` |
| `thread` | `spawn`, `channel`, `scope`, `atomic`, `mutex`; `parallel for` and `select` are part of the language |
| `net` | `connect(host, port)` for a TCP connection, an `io.File` read with `io.read` and written with `print(text, to=conn)`; `listen(host, port)` for a `Listener` and `accept(listener)` for each client's connection; `udp(host, port)` for a `Socket`, `send(sock, text, host, port)` and `receive(sock)`, giving the message and its sender as `"host:port"`. IPv4 and IPv6; failures raise `IoError`. `connect`, `listen` and `udp` take `timeout=` in seconds: a wait that lasts longer (connecting, `accept`, a read, a write, `receive`) fails with the reason `"timed out"`; 0, the default, waits without limit |
| `re` | Regular expressions in Python's syntax, on characters: `search(pattern, text)` gives the first `Match` or `None` (`m.group()`, `m.group(1)`, `""` for a group that took no part, `m.start()`, `m.end()`, and `m.named("year")` for a group written `(?P<year>...)`); `find_all(pattern, text)` the text of every match; `replace(pattern, text, new)`, where `\1` and `\g<12>` and `\g<year>` stand for groups; `split(pattern, text)`. A pattern may start with flags: `(?i)` ignores case (of ASCII letters in a range, of any single letter), `(?m)` makes `^` and `$` hold on each line, `(?s)` lets `.` take a line break. A pattern that cannot be read raises `PatternError` (`at`, `reason`). Matching takes a time in proportion to the pattern and the text: there are no backreferences or lookaround, a repeated group that can match nothing may stop elsewhere than in Python, `\d`, `\w`, `\s` and `\b` are ASCII, and `$` is the end of the text |
| `url` | `parse(text)` gives a `Url` with `scheme`, `user`, `host`, `port` (an `Optional`), `path`, `query` (a dict from each name to its values) and `fragment`, all decoded, and `raw_path`, the path as written when encoding `path` would not give it back (`/a%2Fb`, whose `%2F` is no separator; `""` otherwise), which `to_text` and `join` follow; `to_text(u)` writes one, encoding what needs it; `join(base, link)` resolves a link as a browser does; `encode(text)` and `decode(text)` turn `a b` into `a%20b` and back. A bad port raises `UrlError` |
| `args` | The command line read into a struct that describes it: `args.parse[Options]()`. A field without a default is a word that must be given; one with a default (`retries: i64 = 3`) or an `Optional` is an option, `--retries 5` or `--retries=5`; a `bool` is a flag, `--verbose`; a `vector` takes the words left over; `dry_run` is `--dry-run`. With an enum each case is a command: `tool build --release` gives `Command.Build(release=True)`. The comments above the struct, its fields and the cases are the text of `--help`; a command line that does not fit writes why and ends the program with the code 2. `args.to_struct[T](words)` reads given words and raises `ArgsError`; `args.help[T](program)` gives the help text |
| `binary` | Numbers read from and written to bytes for parsing binary formats, where indexing past the end would stop the program: `read_u8`, `read_u16`, `read_u32`, `read_i16`, `read_i32` give an `i64`, `read_u64` a `u64`, at an offset and in an `Order` (`Big`: 0x1234 stored 12 34; `Little`: 34 12); `need(data, at, count)` and `slice(data, start, end)` check a block; `starts(data, at, "GIF89a")` and `find(data, byte, start, end)` never raise; `put_u16` / `put_u32` write over bytes and `push_u16` / `push_u32` add them, checking the value fits; `put_u64` / `push_u64` write a full 64-bit `u64` (an address), which always fits so they take no check; `fill(byte, count)` makes a run of bytes (a NOP sled) and `pad(data, length, byte)` extends a buffer up to `length`. A read or write past the end raises `BinaryError` (`at`, `needed`, `length`, `reason`) |
| `elf` | Reads an ELF (executable, object, or shared library) into a structured form: `parse(data) -> Elf` gives a `Header` (`bits`, `data`, `kind`, `machine`, `entry`, `flags`, table locations), `sections`, `segments`, `symbols` (from `.symtab`) and `dynamic_symbols` (from `.dynsym`), with names resolved and every address and size widened to `u64`. Handles 32- and 64-bit in either byte order (built on `binary`); object files have no segments, a stripped file no `.symtab`. Classified fields are enums with an `Unknown(raw)` fallback (`Kind`, `Machine`, `SectionKind`, `SegmentKind`, `Bind`, `SymbolType`); the two bitfields `Section.flags` / `Segment.flags` are integers, tested with `SHF_*` / `PF_*`. Parsing is robust for analysing malformed binaries, raising `ElfError` only on a fatal problem (bad magic, a truncated header, a table past the end). `find_section(file, name)` and `find_symbol(file, name)` (which searches `.symtab` then `.dynsym`) give the first match or `None`. For changes, `edit(data) -> Editor` opens a copy for surgical edits: `set_entry`, `set_segment_field` / `set_section_field` (an enum picks the field, 32-/64-bit aware), `patch` / `patch_u32` / `patch_u64` at an offset, `append` (returns the file offset), then `build()` emits the modified ELF. Patch-in-place: field changes, byte patches and appends, no structural re-layout |
| `offsec.cyclic` | A De Bruijn ("cyclic") pattern, to find how far into an overflow a saved return address or a controlled register sits. `pattern(length)` gives the bytes to send, with a unique 4-byte window everywhere, so up to 26^4 = 456976 bytes stay unique and a longer one raises `CyclicError`. After the crash, `padding(value)` gives the padding length to that spot from the `value` left in the register, or `None` when it is not a window of the pattern. `width=Width.Bits64` and `order=Order.Little` (shared with `binary`) default to x86-64 and are passed for a 32-bit or big-endian target |
| `offsec.payload` | Builds an exploit payload from mixed parts. A `Part` is `Bytes(v)` (raw), `Text(s)` (the string's UTF-8 bytes, no null added) or `Address(a)` (packed for the target); `flat(parts, width=Width.Bits64, order=Order.Little)` joins them into one `vector[u8]`, packing every `Address` once for the whole payload (x86-64 by default), and raises `BinaryError` only when a 32-bit `Address` does not fit 4 bytes. For byte runs to drop in a `Part.Bytes`, see `binary.fill` / `binary.pad` |
| `offsec.rop` | Finds gadgets in an ELF and builds a return-oriented chain (x86, 32- and 64-bit). `find_gadget(data, file, pattern)` gives the virtual address of the first occurrence of a byte pattern in an executable segment (unaligned on purpose — any run ending in `ret` is a gadget), or `None`; `find(data, file, gadget)` does the same for a common `Gadget` (`Ret`, `Syscall`, `PopRdi`…), picking the encoding from the ELF's class. For a PIE binary the address is the static one, to which you add the leaked base. `chain(bits)` makes a `Chain`: `raw(value)` adds a word, `set_reg(pop_gadget, value)` a `pop; ret` and the value it loads, and `bytes()` gives the chain as little-endian words (8 bytes for 64-bit, 4 for 32-bit) to place after the padding |
| `offsec.shellcode` | Ready-made shellcode — the raw machine-code bytes to inject and jump to. `sh(bits)` is the standard null-free Linux `execve("/bin/sh")` for the class (64-bit `syscall`, 32-bit `int 0x80`); drop it in a `payload.Part.Bytes` or inject it with `elf` and point the entry at it |
| `offsec.encode` | Reversible byte encodings for payloads. `xor(data, key)` repeats `key` over `data` and is its own inverse (the same call decodes); pick a key whose result dodges the bytes a target rejects. `has_byte(data, byte)` checks a result for a byte to avoid |
| `offsec.fmt` | Builds a format-string write. `write(target, value, offset, bits)` makes the `printf` payload that writes `value` to `target`, one byte at a time with `%hhn` (count padded by `%c`, addresses after the specifiers, low bytes first). `offset` is the stack argument index where the payload begins (find it with `%p`); targets glibc, 32- and 64-bit |
| `offsec.tube` | Drives a target interactively over a connection. `interactive(conn)` bridges your terminal to it — the connection to stdout, your keyboard to the connection, each on its own thread — until you end input with Ctrl-D (or the connection closes and you press Ctrl-D); input is line-buffered. Use it after the exploit opens a shell. Built on `io.shutdown` to end the read side cleanly |
| `testing` | `expect(condition)`, `equal(a, b)`, for `@test` functions run by `hsy test` |

An f-string formats a value after `:`, as Python does: `[[fill]align][+][0][width][,][.digits][kind]`.

```python
f"{pi:.2f}"      # 3.14          f"{n:,}"       # 1,234,567      f"{n:012}"    # 000001234567
f"{name:>6}"     #    Ada        f"{name:*^9}"  # ***Ada***      f"{255:x}"    # ff
f"{ratio:.1%}"   # 25.6%         f"{x:+.1f}"    # +3.1           f"{big:.2e}"  # 1.23e+04
```

`<`, `>` and `^` align in `width` (numbers go right, the rest left), `0` pads a number with zeros, `+` writes the sign of a positive number, `,` groups digits by three. The kinds are `d`, `x`, `X`, `b`, `o` for integers and `f`, `e`, `%` for numbers; digits after `.` without a kind mean `f`. A format that does not fit the type of its value is a compile error. The formatting itself is written in Hesychia, in the prelude.

`deallocate(b)` takes the value out of a `heap[T]` and gives its memory back, the reverse of `allocate(x)`; `&b` lends what a `heap` holds where a `&T` is expected. An enum's type parameters can have bounds (`enum Tree[T: Order]`). `x = f(x)` can move `x` in a loop, since it is filled again. `while True:` without a `break` counts as never ending, so a function may end with one. A `comptime` constant can be a variant without fields of an enum (`comptime MODE: Mode = Mode.Safe`), which `comptime if` compares with `==` and `!=`.

A value can be moved inside a loop when the loop is left right after, by `return` or `break`, with no `continue` in between. A value given where an `Optional[T]` is expected is converted to `T` first, as `managed` converts. A parameter's name cannot be declared again at the top of its function.

The modules share their words: `load` and `dump` for formats, `read` and `print` for text, `sys_` for direct system calls, `to=` for where writing goes, `end=` for where reading or writing stops, `unit=` for durations. Each module works on Linux, macOS and Windows.

A local variable may take a prelude name (`sum = 0`). `for a, b in pairs:` takes each item apart, in a comprehension too (`[n for w, n in d.items()]`); `for w in ["a", "b"]:` goes over a list written in place; a loop over a temporary, `for w in s.split():`, owns it. A struct whose fields are all private is built empty with `S()`, which is how `dict` and `set` start. Only the parts of the standard library a program uses end up in its binary. A file is a module and a folder is a package, relative to the folder of the main file: `import shop.cart` reads `shop/cart.hsy` and makes its items available as `cart.name`. Imports may be circular. Everything is public unless marked `priv`, which limits it to its file.

### comptime

```python
comptime BLOCK: size = 256 * 4

comptime if target.os == "windows":
    extern fn GetTickCount() -> u32
else:
    extern fn clock() -> i64

comptime assert(size_of[u64]() == 8, "u64 must be 8 bytes")
```

A `comptime fn` runs in the compiler, on values and on types. It can declare variables, assign them, loop with `while` and `for ... in range(...)`, branch, recurse and call other `comptime fn`s; it cannot reach the system. Its value replaces the call, in code as in types:

```python
comptime fn fact(n: i64) -> i64:
    if n <= 1:
        return 1
    return n * fact(n - 1)

comptime fn row(R: type) -> type:
    if R == dict:
        return dict[str, str]
    return vector[str]

manual fn load[R](f: &File) -> vector[row(R)]:
    comptime if R == dict:
        ...
    else:
        ...
```

`R == dict` holds for any `dict`, `R == dict[str, str]` for that type only. A generic type without its arguments can be given to a type parameter, as in `csv.load[dict](f)`, for `comptime` code to test. `item(T)` is the type of what `T` holds, and `panic("...")` stops the compilation with its message, reported where the call is written. A generic function whose signature uses a `comptime fn`, or whose body tests its type parameters, is checked for each set of types it is called with. A `comptime fn` that runs more than a million steps is stopped. Defaults may be `None` for an `Optional` parameter.

A field can have a default, known when compiling: with `retries: i64 = 3`, `Options(source="a")` leaves it at 3; so can a field of an enum's case, `Build(jobs: i64 = 4)`.

`comptime for f in fields(T):` runs its body once for each field of the struct `T`: there `f.name` is the field's name, `f.doc` the comment lines right above it, `f.has_default` whether it has a default, and `field(x, f)` that field of `x`, which is how `json` and `args` read any struct. `comptime for v in variants(T):` goes over the cases of an enum, with `v.name` and `v.doc`; there `fields(v)` are the fields of the case, `empty[T](v)` a value of that case, and `field(x, f)` a field of `x`, which must be of that case. `doc(T)` is the comment above `T`. `empty[T]()` is the value a `T` starts with (`0`, `False`, `""`, `[]`, `None`, a struct of them, a field with a default starting with it), and `is_struct(T)` and `is_enum(T)` test a type in a `comptime if`. A type parameter can have a default, `fn load[T = Value](...)`, used when a call neither gives the type nor shows it by its arguments. `T.checked(x)` converts to the number type `T` stands for. In `manual`, a value with nothing to release can be lent where it is computed, `min(&(a + b), c)`.

Only the selected branch of a `comptime if` is type-checked. `target` describes the machine the program is built for (see `--target`): `os` (`linux`, `windows`, `macos`), `arch` (`x86_64`, `aarch64`, `riscv64`), `env` (`gnu`, `musl`, or empty on macOS), `pointer_bits`, `endian` and `debug`.

## Usage

```sh
cargo build --release
./target/release/hsy run examples/milestone.hsy
./target/release/hsy build examples/milestone.hsy --release -o milestone
```

| Command | Effect |
| --- | --- |
| `hsy build <file.hsy>` | Debug build: no optimization, debug info, `#line` mapping to the `.hsy` source |
| `hsy build <file.hsy> --release` | Optimized for size (`-Os`), unused code removed, symbols stripped |
| `hsy build ... -o <path>` | Output path (default: the file name without extension) |
| `hsy build ... --emit-c` | Keeps the generated C file next to the binary |
| `hsy run <file.hsy> [--release]` | Builds to a temporary binary and runs it |
| `hsy test [<file.hsy>] [<name>]` | Runs the `@test` functions of the file, or of a project's `src/` and `tests/`, those whose name holds `<name>` |
| `hsy new <name>` | Creates a project: `hesychia.toml`, `src/main.hsy`, `.gitignore` |
| `hsy build` / `hsy run` without a file | Builds the project whose `hesychia.toml` is in this folder or above, into `target/debug/<name>` or `target/release/<name>` |
| `hsy build ... --target <triple>` | Builds for another machine: `x86_64-windows-gnu`, `aarch64-linux-gnu`, `x86_64-linux-musl`, `aarch64-macos`... A project puts the binary in `target/<triple>/debug/`; Windows binaries end in `.exe` and are linked statically |
| `... --warn-copies` | Shows each copy that `managed` code inserts, and why |

The C compiler is `cc` for the host, and the usual cross compiler for another target: `<arch>-w64-mingw32-gcc` for Windows, `<arch>-linux-<env>-gcc` for Linux. `CC_<triple>` (with `_` for `-`, as in `CC_x86_64_windows_gnu`) names another one; so does `CC` when the command builds for a single target. MSVC cannot build the generated C, and macOS needs `CC_<triple>`.

### Projects and dependencies

```toml
[project]
name = "app"
version = "0.1.0"

[dependencies]
math = { path = "../math" }
text = { git = "https://example.com/text.git", tag = "v1.0" }   # or branch = "...", rev = "..."

[settings]
heap_threshold = 65536   # managed variables larger than this, in bytes, live on the heap
targets = ["x86_64-linux-gnu", "x86_64-windows-gnu"]   # what `hsy build` builds for (default: the host)
links = ["curl", "z"]   # C libraries the binary is linked with: `-lcurl -lz`
```

A dependency is a project too: `import math` reads its `src/lib.hsy`, `import math.ops` its `src/ops.hsy`, and its own imports resolve inside it. A name is looked up in the project, then its dependencies, then the standard library; a name found in two of these places is an error. Git dependencies are cloned into `.hesychia/deps/`, and the commit each one was built from is written to `hesychia.lock`, which later builds follow even when the tag moves. Everything is compiled from source into one binary. With `targets`, `hsy build` makes one binary for each; `hsy run` always builds for this machine. `links` makes the C functions of a library reachable through `extern fn`: each name is given to the C compiler as `-l<name>`, so `"gtk-3"` is `libgtk-3`, found where the C compiler looks (`LIBRARY_PATH` adds folders); `":libcurl.so.4"` names the file itself, as GNU ld takes it (Linux, and Windows with MinGW; not Apple's linker), for a library installed without its development files. A dependency's `links` go to every binary that uses it. When a value cannot be used again because a `managed` function now keeps it (say, after a dependency changed), the error names the function and the line where it keeps the value.

## Example

```python
extern fn printf(fmt: ptr[u8], ...) -> i32

manual fn square(x: i64) -> i64:
    return x * x

manual fn main():
    mut total: i64 = 0
    mut i: i64 = 1
    while i <= 10:
        total += square(i)
        i += 1
    printf("sum = %lld\n".to_c(), total)
```

## Layout

| Path | Role |
| --- | --- |
| `src/loader.rs` | Finds and parses the main file and every imported module, in the project, its dependencies or the standard library |
| `src/project.rs` | `hesychia.toml`, the packages of a program, Git dependencies and `hesychia.lock`, `hsy new` |
| `src/lexer/` | Tokens, including `INDENT` / `DEDENT` for blocks |
| `src/parser/` | Recursive-descent parser producing the syntax tree (`src/ast.rs`) |
| `src/typeck/` | Name resolution, `comptime` expansion and type checking; `managed.rs` for the managed level's inference, `threads.rs` for the `thread` module, `low_level.rs` for pointers, casts, SIMD and `asm`, `comptime_fn.rs` for running `comptime fn`s |
| `src/comptime.rs` | Compile-time evaluation |
| `src/codegen/` | C emission: types and their drop/copy functions, ownership, runtime, checked helpers; `hoist.rs` takes out what an expression computes before a fallible call, in statements, `while` conditions and `and` / `or` |
| `src/driver.rs` | Pipeline and C compiler invocation |
| `src/stdlib.rs`, `stdlib/` | The standard library, in Hesychia, built into the compiler; `stdlib/offsec/` holds OffSec, its modules for security work |
| `src/platform.rs` | Target triples and the C compiler of each target |
| `src/diag.rs` | Error and warning rendering |
| `tests/run/`, `tests/fail/`, `tests/panic/` | Programs that must run, be rejected, or stop at runtime |
| `tools/unicode_case.py` | Writes `stdlib/casing.hsy`, the Unicode case tables of `str.upper` / `str.lower`, from Python's Unicode data; the lookups appended to it live in `tools/case_lookup.hsy` |

## Editor

The extension for [Zed](https://zed.dev) lives in its own repository, [Zed-LSP](https://github.com/The-Hesychia-language/Zed-LSP): coloring in the manner of Python, matched brackets, indentation after `:`, and an outline of functions and types.

## Testing Hesychia programs

A test is a function marked `@test`, with no parameters and no result, in `managed` or `manual` code. It goes beside the code it checks, or in a project's `tests/` folder. The `testing` module checks with `expect(condition)` and `equal(a, b)`, which writes both values when they differ. A failure says which line of the test it was on. A test fails when a check fails, when an error leaves it, or when the program stops on a fault.

```python
from testing import expect, equal

@test
managed fn sorts():
    v = [3, 1, 2]
    v.sort()
    equal(v, [1, 2, 3])
```

```
$ hsy test
ok    parses_numbers
FAIL  sorts  (src/list.hsy)
      panic: equal: [1, 3, 2] != [1, 2, 3]
2 tests, 1 failed
```

Each test is built and run on its own, so one that stops does not take the others with it; what it wrote is shown when it fails. A program built by `hsy build` or `hsy run` leaves its tests out. `hsy test` ends with code 1 when a test fails.

## Tests

```sh
cargo test
```

Every program in `tests/run` is built in debug and release, its output is compared with its `# expect:` lines (`# expect-debug:` / `# expect-release:` for one mode), its generated C must compile with `-Wall -Wextra -Werror`, and, when `valgrind` is installed, the debug build must not leak or touch invalid memory. Programs in `tests/fail` must be rejected with their `# error:` text; programs in `tests/panic` must stop at runtime. Helper modules live in `lib/` subfolders. `tests/projects.rs` builds projects with path and Git dependencies, and for several targets, in `target/tmp/projects`. The `app_*` programs in `tests/run` are small whole programs: a word count through files and CSV, a search tree, accounts with errors and JSON, a pipeline of threads. With MinGW and Wine installed, `cargo test --test programs -- --ignored` also builds every program in `tests/run` for Windows and runs it under Wine.

`cargo test --test fuzz -- --ignored` damages every test program in small ways (lines removed, swapped or cut, pieces of the language put in at random) and compiles each result: the compiler must accept it or reject it with a message, never crash, hang, or write C that does not compile. `HSY_FUZZ` sets the damaged copies per program (8 by default), `HSY_FUZZ_SEED` the seed, `HSY_FUZZ_ONLY` a part of the names to keep; what fails stays in `target/tmp/fuzz-<seed>`.
