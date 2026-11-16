//! Threads on POSIX threads: a block per `thread[T]` holds the thread and its result; each
//! `spawn` site gets a frame for its arguments and a function that runs it.

use super::Emitter;
use super::types::TypeRegistry;
use crate::ast::{Builtin, Expr, ExprKind};
use crate::types::Type;

impl TypeRegistry<'_> {
    /// `struct hsy_thread_N { pthread_t id; R result; bool scoped; bool joined; }`
    pub(super) fn thread_block(&mut self, result: &Type) -> String {
        self.uses_threads = true;
        self.uses_alloc = true;
        let key = format!("thread {result:?}");
        if let Some(name) = self.made.get(&key) {
            return name.clone();
        }
        let name = format!("hsy_thread_{}", self.made.len());
        self.made.insert(key, name.clone());
        self.forwards += &format!("typedef struct {name} {name};\n");
        let value = if *result == Type::Void {
            String::new()
        } else {
            format!("    {} result;\n", self.c_type(result))
        };
        self.blocks += &format!(
            "struct {name} {{\n    pthread_t id;\n{value}    bool scoped;\n    bool joined;\n}};\n\n"
        );
        name
    }
}

impl Emitter<'_> {
    /// Starts the thread: its arguments are moved into a frame that the thread frees.
    pub(super) fn emit_spawn(&mut self, args: &[Expr], values: &[String], ty: &Type) -> String {
        let Type::Thread(result) = ty else { unreachable!("spawn gives a thread") };
        let block = self.types.thread_block(result);
        let site = self.temps;
        let (frame, run) = (format!("hsy_frame_{site}"), format!("hsy_run_{site}"));
        let function = self.types.c_type(args[0].ty());
        let fields: String = args[1..]
            .iter()
            .enumerate()
            .map(|(i, a)| format!("    {} a{i};\n", self.types.c_type(a.ty())))
            .collect();
        let call_args: Vec<String> = (0..args.len() - 1).map(|i| format!("f->a{i}")).collect();
        let call = format!("f->function({})", call_args.join(", "));
        let store =
            if **result == Type::Void { format!("{call};") } else { format!("f->thread->result = {call};") };
        self.types.forwards += &format!("typedef struct {frame} {frame};\n");
        self.types.blocks +=
            &format!("struct {frame} {{\n    {block}* thread;\n    {function} function;\n{fields}}};\n\n");
        self.types.functions += &format!(
            "static void* {run}(void* p) {{\n    {frame}* f = p;\n    {store}\n    free(f);\n    return NULL;\n}}\n\n"
        );
        let (t, f) = (self.temp("thread"), self.temp("frame"));
        let moves: String =
            values[1..].iter().enumerate().map(|(i, v)| format!("{f}->a{i} = {v}; ")).collect();
        format!(
            "({{ {block}* {t} = hsy_alloc(sizeof *{t}); {t}->scoped = false; {t}->joined = false; \
             {frame}* {f} = hsy_alloc(sizeof *{f}); {f}->thread = {t}; {f}->function = {}; {moves}\
             if (pthread_create(&{t}->id, NULL, {run}, {f})) hsy_panic(); {t}; }})",
            values[0]
        )
    }

    /// Threads, channels, atomics and scopes.
    pub(super) fn emit_thread_builtin(
        &mut self,
        op: Builtin,
        args: &[Expr],
        values: &[String],
        ty: &Type,
    ) -> Option<String> {
        let atomic = |call: &str| format!("{call}(&({}), {}, __ATOMIC_SEQ_CST)", values[0], values[1]);
        Some(match op {
            Builtin::Spawn => self.emit_spawn(args, values, ty),
            Builtin::ScopedSpawn => self.emit_scoped_spawn(args, values, ty),
            Builtin::ThreadJoin => self.emit_join(&values[0], ty),
            Builtin::ScopeNew => "((hsy_scope){0})".into(),
            Builtin::Channel => self.emit_channel(ty),
            Builtin::ChanSend => {
                let c_type = self.types.c_type(args[1].ty());
                let temp = self.temp("message");
                format!("({{ {c_type} {temp} = {}; hsy_chan_send({}, &{temp}); }})", values[1], values[0])
            }
            Builtin::ChanReceive => {
                let Type::Optional(item) = ty else { unreachable!("receive gives an Optional") };
                let (optional, c_type) = (self.types.c_type(ty), self.types.c_type(item));
                let temp = self.temp("message");
                format!(
                    "({{ {c_type} {temp}; memset(&{temp}, 0, sizeof {temp}); bool ok = hsy_chan_receive({}, &{temp}); \
                     ({optional}){{.has = ok, .value = {temp}}}; }})",
                    values[0]
                )
            }
            Builtin::AtomicNew => values[0].clone(),
            Builtin::MutexNew => {
                let c_type = self.types.c_type(ty);
                format!("(({c_type}){{PTHREAD_MUTEX_INITIALIZER, {}}})", values[0])
            }
            Builtin::AtomicLoad => format!("__atomic_load_n(&({}), __ATOMIC_SEQ_CST)", values[0]),
            Builtin::AtomicStore => atomic("__atomic_store_n"),
            Builtin::AtomicAdd => atomic("__atomic_fetch_add"),
            Builtin::AtomicSub => atomic("__atomic_fetch_sub"),
            Builtin::AtomicExchange => atomic("__atomic_exchange_n"),
            Builtin::AtomicCompareExchange => {
                let item = self.types.c_type(args[1].ty());
                let temp = self.temp("expected");
                format!(
                    "({{ {item} {temp} = {}; __atomic_compare_exchange_n(&({}), &{temp}, {}, false, \
                     __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST); }})",
                    values[1], values[0], values[2]
                )
            }
            _ => return None,
        })
    }

    /// `channel[T]()`: one channel, seen from both ends.
    pub(super) fn emit_channel(&mut self, ty: &Type) -> String {
        let Type::Tuple(ends) = ty else { unreachable!("a pair") };
        let Type::Sender(item) = &ends[0] else { unreachable!("a sender first") };
        let pair = self.types.c_type(ty);
        let item_c = self.types.c_type(item);
        let release = match self.types.drop_fn(item) {
            Some(drop) => {
                let name = format!("hsy_chan_drop_{}", self.temps);
                self.types.functions +=
                    &format!("static void {name}(void* p) {{ {drop}(({item_c}*)p); }}\n\n");
                name
            }
            None => "NULL".into(),
        };
        let c = self.temp("channel");
        format!("({{ hsy_chan* {c} = hsy_chan_new(sizeof({item_c}), {release}); ({pair}){{{c}, {c}}}; }})")
    }

    /// `with scope() as s:`: a block whose `s` waits for its threads when it goes away.
    pub(super) fn emit_with(&mut self, name: &str, body: &[crate::ast::Stmt]) {
        let s = super::expr::c_ident(name);
        self.line("{");
        self.indent += 1;
        self.open_scope(false);
        self.line(&format!("hsy_scope {s} = {{0}};"));
        self.own(s, Type::Scope);
        self.emit_block(body, true);
        let jumped = super::stmt::ends_with_jump(body);
        self.close_scope(jumped);
        self.indent -= 1;
        self.line("}");
    }

    /// `with m as x:`: the mutex is locked, `x` points to its value, and the lock is one more
    /// thing the block releases, however it is left.
    pub(super) fn emit_locked(&mut self, value: &Expr, name: &str, body: &[crate::ast::Stmt]) {
        let Type::Mutex(item) = value.ty().deref_ref() else { unreachable!("checked to be a mutex") };
        let item_c = self.types.c_type(item);
        let mutex = self.emit_expr(value);
        let pointer = if matches!(value.ty(), Type::Ref(..)) { mutex } else { format!("&({mutex})") };
        let (held, lock) = (self.temp("mutex"), self.temp("lock"));
        self.line("{");
        self.indent += 1;
        self.open_scope(false);
        self.line(&format!("__typeof__({pointer}) {held} = {pointer};"));
        self.line(&format!("pthread_mutex_t* {lock} = &{held}->lock;"));
        self.line(&format!("pthread_mutex_lock({lock});"));
        self.own(lock, Type::Locked);
        self.line(&format!("HSY_UNUSED {item_c}* {} = &{held}->value;", super::expr::c_ident(name)));
        self.emit_block(body, true);
        let jumped = super::stmt::ends_with_jump(body);
        self.close_scope(jumped);
        self.indent -= 1;
        self.line("}");
    }

    /// `s.spawn(f, args...)`: as `spawn`, with the thread registered in the scope. A lambda's
    /// environment is copied into the frame, so it lives as long as the thread.
    pub(super) fn emit_scoped_spawn(&mut self, args: &[Expr], values: &[String], ty: &Type) -> String {
        let Type::Thread(result) = ty else { unreachable!("spawn gives a thread") };
        let block = self.types.thread_block(result);
        let finish = self.thread_finish(result, &block);
        let site = self.temps;
        let (frame, run) = (format!("hsy_frame_{site}"), format!("hsy_run_{site}"));
        let callable = &args[1];
        let function = self.types.c_type(callable.ty());
        let closure = matches!(callable.ty(), Type::Ref(..));
        let env = match &callable.kind {
            ExprKind::Lambda { captures, c_name, .. } if !captures.is_empty() => {
                Some(format!("struct {c_name}_env"))
            }
            _ => None,
        };
        let mut fields: String = args[2..]
            .iter()
            .enumerate()
            .map(|(i, a)| format!("    {} a{i};\n", self.types.c_type(a.ty())))
            .collect();
        if let Some(env) = &env {
            fields += &format!("    {env} env;\n");
        }
        let mut call_args: Vec<String> = (0..args.len() - 2).map(|i| format!("f->a{i}")).collect();
        let call = if closure {
            call_args.insert(0, "f->function.env".into());
            format!("f->function.code({})", call_args.join(", "))
        } else {
            format!("f->function({})", call_args.join(", "))
        };
        let store =
            if **result == Type::Void { format!("{call};") } else { format!("f->thread->result = {call};") };
        // A literal lent to the thread is kept in the frame: written in place, it would be
        // gone before the thread reads it.
        let kept = |a: &Expr| match &a.kind {
            ExprKind::AddrOf { inner, .. }
                if matches!(
                    inner.kind,
                    ExprKind::Int(_)
                        | ExprKind::Float(_)
                        | ExprKind::Str(_)
                        | ExprKind::Bool(_)
                        | ExprKind::Char(_)
                        | ExprKind::Unary(..)
                        | ExprKind::Convert(_)
                ) =>
            {
                Some(inner.ty().clone())
            }
            _ => None,
        };
        for (i, arg) in args[2..].iter().enumerate() {
            if let Some(ty) = kept(arg) {
                fields += &format!("    {} t{i};\n", self.types.c_type(&ty));
            }
        }
        let (t, f) = (self.temp("thread"), self.temp("frame"));
        let mut moves: String = values[2..]
            .iter()
            .zip(&args[2..])
            .enumerate()
            .map(|(i, (v, a))| match kept(a) {
                Some(_) => format!("{f}->t{i} = *{v}; {f}->a{i} = &{f}->t{i}; "),
                None => format!("{f}->a{i} = {v}; "),
            })
            .collect();
        self.types.forwards += &format!("typedef struct {frame} {frame};\n");
        // After the lambdas: the frame may hold a lambda's environment.
        self.lambdas += &format!(
            "struct {frame} {{\n    {block}* thread;\n    {function} function;\n{fields}}};\n\n\
             static void* {run}(void* p) {{\n    {frame}* f = p;\n    {store}\n    free(f);\n    return NULL;\n}}\n\n"
        );
        if let Some(env) = &env {
            moves += &format!("{f}->env = *({env}*){f}->function.env; {f}->function.env = &{f}->env; ");
        }
        format!(
            "({{ {block}* {t} = hsy_alloc(sizeof *{t}); {t}->scoped = true; {t}->joined = false; \
             {frame}* {f} = hsy_alloc(sizeof *{f}); {f}->thread = {t}; {f}->function = {}; {moves}\
             if (pthread_create(&{t}->id, NULL, {run}, {f})) hsy_panic(); hsy_scope_add(&{}, {t}, {finish}); {t}; }})",
            values[1], values[0]
        )
    }

    /// What the end of a scope does with one of its threads: wait for it unless joined,
    /// release a result nobody took, free the block.
    fn thread_finish(&mut self, result: &Type, block: &str) -> String {
        let name = format!("hsy_finish_{}", block.trim_start_matches("hsy_thread_"));
        let key = format!("finish {result:?}");
        if self.types.made.contains_key(&key) {
            return name;
        }
        self.types.made.insert(key, name.clone());
        let release = self.types.drop_fn(result).map_or(String::new(), |d| format!(" {d}(&t->result);"));
        self.types.functions += &format!(
            "static void {name}(void* p) {{\n    {block}* t = p;\n    if (!t->joined) {{ pthread_join(t->id, NULL);{release} }}\n    free(t);\n}}\n\n"
        );
        name
    }

    /// Waits for the thread and takes its result.
    pub(super) fn emit_join(&mut self, handle: &str, result: &Type) -> String {
        let block = self.types.thread_block(result);
        let t = self.temp("joined");
        let take = if *result == Type::Void {
            String::new()
        } else {
            let r = self.types.c_type(result);
            format!("{r} r = {t}->result; ")
        };
        let finish = if *result == Type::Void { "" } else { "r; " };
        format!(
            "({{ {block}* {t} = {handle}; pthread_join({t}->id, NULL); {take}\
             if ({t}->scoped) {t}->joined = true; else free({t}); {finish}}})"
        )
    }
}
