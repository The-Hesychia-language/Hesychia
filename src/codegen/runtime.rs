//! C source of the runtime pieces, emitted only when a program uses them.

/// Allocation that stops the program when memory runs out.
pub const ALLOC: &str = r#"HSY_UNUSED static void* hsy_alloc(size_t size) {
    void* p = malloc(size ? size : 1);
    if (!p) hsy_panic();
    return p;
}

HSY_UNUSED static void* hsy_realloc(void* old, size_t size) {
    void* p = realloc(old, size ? size : 1);
    if (!p) hsy_panic();
    return p;
}

"#;

/// `str`: UTF-8 bytes, always followed by a zero byte. `cap == 0` marks a literal,
/// which is never freed and is copied before being changed.
pub const STR: &str = r#"HSY_UNUSED static void hsy_str_drop(hsy_str* s) {
    if (s->cap) free(s->data);
    *s = (hsy_str){0};
}

HSY_UNUSED static void hsy_str_append(hsy_str* s, const char* p, size_t n) {
    size_t need = s->len + n + 1;
    if (s->cap < need) {
        size_t cap = s->cap ? s->cap : 16;
        while (cap < need) cap *= 2;
        char* data = s->cap ? hsy_realloc(s->data, cap) : hsy_alloc(cap);
        if (!s->cap && s->len) memcpy(data, s->data, s->len);
        s->data = data;
        s->cap = cap;
    }
    if (n) memcpy(s->data + s->len, p, n);
    s->len += n;
    s->data[s->len] = 0;
}

HSY_UNUSED static void hsy_str_push_str(hsy_str* s, const hsy_str* t) {
    hsy_str_append(s, t->data, t->len);
}

HSY_UNUSED static hsy_str hsy_str_concat(const hsy_str* a, const hsy_str* b) {
    hsy_str r = {0};
    hsy_str_append(&r, a->data, a->len);
    hsy_str_append(&r, b->data, b->len);
    return r;
}

HSY_UNUSED static hsy_str hsy_str_copy(const hsy_str* s) {
    if (!s->cap) return *s;
    hsy_str r = {0};
    hsy_str_append(&r, s->data, s->len);
    return r;
}

HSY_UNUSED static bool hsy_str_eq(const hsy_str* a, const hsy_str* b) {
    return a->len == b->len && (a->len == 0 || memcmp(a->data, b->data, a->len) == 0);
}

/* Characters are counted as code points: every byte that does not continue a UTF-8 sequence. */
HSY_UNUSED static size_t hsy_str_chars(const hsy_str* s) {
    size_t n = 0;
    for (size_t i = 0; i < s->len; i++) n += ((unsigned char)s->data[i] & 0xC0) != 0x80;
    return n;
}

/* Byte order, which is also the order of code points in UTF-8. */
HSY_UNUSED static int hsy_str_cmp(const hsy_str* a, const hsy_str* b) {
    size_t n = a->len < b->len ? a->len : b->len;
    int c = n ? memcmp(a->data, b->data, n) : 0;
    return c ? c : (a->len > b->len) - (a->len < b->len);
}

HSY_UNUSED static uint8_t hsy_str_byte(const hsy_str* s, size_t i) {
    if (i >= s->len) hsy_panic();
    return (uint8_t)s->data[i];
}

/* The text of valid UTF-8 bytes (shortest forms, no surrogates, at most U+10FFFF); `*ok` says
   whether they were, and the text is empty when not. */
HSY_UNUSED static hsy_str hsy_str_from_bytes(const char* p, size_t n, bool* ok) {
    const uint8_t* b = (const uint8_t*)p;
    hsy_str r = {0};
    *ok = false;
    for (size_t i = 0; i < n;) {
        uint8_t c = b[i];
        size_t extra = c < 0x80 ? 0 : c >= 0xC2 && c < 0xE0 ? 1 : c >= 0xE0 && c < 0xF0 ? 2 : c >= 0xF0 && c < 0xF5 ? 3 : 4;
        if (extra == 4 || (extra > 0 && i + extra >= n)) return r;
        for (size_t k = 1; k <= extra; k++) if ((b[i + k] & 0xC0) != 0x80) return r;
        if ((c == 0xE0 && b[i + 1] < 0xA0) || (c == 0xED && b[i + 1] >= 0xA0)) return r;
        if ((c == 0xF0 && b[i + 1] < 0x90) || (c == 0xF4 && b[i + 1] >= 0x90)) return r;
        i += extra + 1;
    }
    *ok = true;
    hsy_str_append(&r, p, n);
    return r;
}

HSY_UNUSED static size_t hsy_str_bytes(const hsy_str* s) {
    return s->len;
}

HSY_UNUSED static uint8_t* hsy_str_c(const hsy_str* s) {
    return (uint8_t*)(s->data ? s->data : "");
}

HSY_UNUSED static hsy_str hsy_str_from_c(const uint8_t* p) {
    hsy_str r = {0};
    hsy_str_append(&r, (const char*)p, strlen((const char*)p));
    return r;
}

HSY_UNUSED static void hsy_str_push_uint(hsy_str* s, unsigned __int128 v) {
    char buf[40];
    size_t i = sizeof buf;
    do buf[--i] = (char)('0' + (int)(v % 10)); while ((v /= 10) != 0);
    hsy_str_append(s, buf + i, sizeof buf - i);
}

HSY_UNUSED static void hsy_str_push_int(hsy_str* s, __int128 v) {
    if (v < 0) hsy_str_append(s, "-", 1);
    hsy_str_push_uint(s, v < 0 ? -(unsigned __int128)v : (unsigned __int128)v);
}

/* The shortest text that reads back as the same number, with `.0` for whole values. */
/* Like Python: the fewest digits that read back as the same value (as a `float` when
   `single`), in plain notation for exponents from -4 to 15, in scientific notation otherwise. */
HSY_UNUSED static void hsy_str_push_float_as(hsy_str* s, double v, bool single) {
    char buf[64];
    int n = 0, digits = 1;
    if (v != v || v - v != 0) {
        n = snprintf(buf, sizeof buf, "%g", v);
        hsy_str_append(s, buf, (size_t)n);
        return;
    }
    for (; digits <= 17; digits++) {
        snprintf(buf, sizeof buf, "%.*e", digits - 1, v);
        double back = strtod(buf, NULL);
        if (single ? (float)back == (float)v : back == v) break;
    }
    int exponent = atoi(strchr(buf, 'e') + 1);
    if (exponent >= -4 && exponent < 16) {
        int decimals = digits - 1 - exponent;
        n = snprintf(buf, sizeof buf, "%.*f", decimals > 0 ? decimals : 0, v);
        hsy_str_append(s, buf, (size_t)n);
        if (!strchr(buf, '.')) hsy_str_append(s, ".0", 2);
        return;
    }
    n = snprintf(buf, sizeof buf, "%.*e", digits - 1, v);
    char* e = strchr(buf, 'e');
    /* `1e+16`, not `1e+016`: the exponent without leading zeros, as Python writes it. */
    char sign = e[1];
    int value = abs(atoi(e + 1));
    n = (int)(e - buf);
    n += snprintf(e, sizeof buf - (size_t)n, "e%c%02d", sign, value);
    hsy_str_append(s, buf, (size_t)n);
}

HSY_UNUSED static void hsy_str_push_float(hsy_str* s, double v) { hsy_str_push_float_as(s, v, false); }
HSY_UNUSED static void hsy_str_push_float32(hsy_str* s, float v) { hsy_str_push_float_as(s, v, true); }

HSY_UNUSED static void hsy_str_push_bool(hsy_str* s, bool v) {
    if (v) hsy_str_append(s, "True", 4); else hsy_str_append(s, "False", 5);
}

HSY_UNUSED static void hsy_str_push_char(hsy_str* s, uint32_t c) {
    char b[4];
    size_t n;
    if (c < 0x80) { b[0] = (char)c; n = 1; }
    else if (c < 0x800) { b[0] = (char)(0xC0 | (c >> 6)); b[1] = (char)(0x80 | (c & 0x3F)); n = 2; }
    else if (c < 0x10000) {
        b[0] = (char)(0xE0 | (c >> 12)); b[1] = (char)(0x80 | ((c >> 6) & 0x3F));
        b[2] = (char)(0x80 | (c & 0x3F)); n = 3;
    } else {
        b[0] = (char)(0xF0 | (c >> 18)); b[1] = (char)(0x80 | ((c >> 12) & 0x3F));
        b[2] = (char)(0x80 | ((c >> 6) & 0x3F)); b[3] = (char)(0x80 | (c & 0x3F)); n = 4;
    }
    hsy_str_append(s, b, n);
}

__attribute__((noreturn)) HSY_UNUSED static void hsy_panic_text(const hsy_str* s) {
#ifdef HSY_PANIC_HANDLER
    HSY_PANIC_HANDLER((hsy_str*)s);
    fflush(stdout);
    __builtin_trap();
#endif
    fflush(stdout);
    fputs("panic: ", stderr);
    fwrite(s->data ? s->data : "", 1, s->len, stderr);
    fputc('\n', stderr);
    hsy_test_where();
    __builtin_trap();
}

int hsy_conn_write(uint8_t* conn, const uint8_t* data, size_t n);

/* `connection`: the stream is a network connection of `net`, written without a buffer. */
HSY_UNUSED static void hsy_print(const hsy_str* s, const hsy_str* end, void* stream, bool connection) {
    if (connection) {
        if (s->len) hsy_conn_write(stream, (const uint8_t*)s->data, s->len);
        if (end->len) hsy_conn_write(stream, (const uint8_t*)end->data, end->len);
        return;
    }
    fwrite(s->data ? s->data : "", 1, s->len, (FILE*)stream);
    fwrite(end->data ? end->data : "", 1, end->len, (FILE*)stream);
}

"#;

/// The functions of one `vector[T]` type, given its C names.
pub fn vector(
    v: &str,
    item: &str,
    drop_item: Option<&str>,
    optional: &str,
    some: &str,
    none: &str,
) -> String {
    let drop_all =
        drop_item.map_or(String::new(), |d| format!("for (size_t i = 0; i < v->len; i++) {d}(&v->data[i]);"));
    format!(
        r#"HSY_UNUSED static void {v}_reserve({v}* v, size_t need) {{
    if (v->cap >= need) return;
    size_t cap = v->cap ? v->cap * 2 : 4;
    while (cap < need) cap *= 2;
    v->data = hsy_realloc(v->data, cap * sizeof({item}));
    v->cap = cap;
}}

HSY_UNUSED static void {v}_push({v}* v, {item} x) {{
    {v}_reserve(v, v->len + 1);
    v->data[v->len++] = x;
}}

HSY_UNUSED static {optional} {v}_pop({v}* v) {{
    if (!v->len) return {none};
    {item} x = v->data[--v->len];
    return {some};
}}

HSY_UNUSED static void {v}_insert({v}* v, size_t i, {item} x) {{
    if (i > v->len) hsy_panic();
    {v}_reserve(v, v->len + 1);
    memmove(v->data + i + 1, v->data + i, (v->len - i) * sizeof({item}));
    v->data[i] = x;
    v->len++;
}}

HSY_UNUSED static {item} {v}_remove({v}* v, size_t i) {{
    if (i >= v->len) hsy_panic();
    {item} x = v->data[i];
    memmove(v->data + i, v->data + i + 1, (v->len - i - 1) * sizeof({item}));
    v->len--;
    return x;
}}

HSY_UNUSED static void {v}_clear({v}* v) {{
    {drop_all}
    v->len = 0;
}}

HSY_UNUSED static void {v}_swap({v}* v, size_t i, size_t j) {{
    if (i >= v->len || j >= v->len) hsy_panic();
    {item} x = v->data[i];
    v->data[i] = v->data[j];
    v->data[j] = x;
}}

"#,
    )
}

/// Reference counting for `shared[T]`, with the synchronous cycle collector of Bacon and
/// Rajan: a count that drops without reaching zero marks a possible cycle root; roots are
/// scanned from time to time, and at exit.
pub const SHARED: &str = r#"enum { HSY_BLACK, HSY_GRAY, HSY_WHITE, HSY_PURPLE };
static hsy_rc** hsy_roots;
static size_t hsy_root_count, hsy_root_cap;
static int hsy_collecting;

HSY_UNUSED static void hsy_rc_no_children(hsy_rc* s, void (*f)(hsy_rc*)) { (void)s; (void)f; }
HSY_UNUSED static void hsy_rc_no_release(hsy_rc* s) { (void)s; }
static void hsy_collect_cycles(void);

HSY_UNUSED static void hsy_rc_inc(hsy_rc* s) {
    s->count++;
    s->color = HSY_BLACK;
}

static void hsy_rc_dec(hsy_rc* s) {
    if (hsy_collecting) return;
    if (--s->count == 0) {
        s->color = HSY_BLACK;
        s->release(s);
        if (!s->buffered) free(s);
        return;
    }
    if (s->color == HSY_PURPLE) return;
    s->color = HSY_PURPLE;
    if (s->buffered) return;
    s->buffered = 1;
    if (hsy_root_count == hsy_root_cap) {
        hsy_root_cap = hsy_root_cap ? 2 * hsy_root_cap : 64;
        hsy_roots = hsy_realloc(hsy_roots, hsy_root_cap * sizeof *hsy_roots);
    }
    hsy_roots[hsy_root_count++] = s;
    if (hsy_root_count >= 4096) hsy_collect_cycles();
}

static void hsy_mark_gray(hsy_rc* s);
static void hsy_gray_child(hsy_rc* t) { t->count--; hsy_mark_gray(t); }
static void hsy_mark_gray(hsy_rc* s) {
    if (s->color == HSY_GRAY) return;
    s->color = HSY_GRAY;
    s->children(s, hsy_gray_child);
}

static void hsy_scan_black(hsy_rc* s);
static void hsy_black_child(hsy_rc* t) {
    t->count++;
    if (t->color != HSY_BLACK) hsy_scan_black(t);
}
static void hsy_scan_black(hsy_rc* s) {
    s->color = HSY_BLACK;
    s->children(s, hsy_black_child);
}

static void hsy_scan(hsy_rc* s) {
    if (s->color != HSY_GRAY) return;
    if (s->count > 0) {
        hsy_scan_black(s);
        return;
    }
    s->color = HSY_WHITE;
    s->children(s, hsy_scan);
}

static hsy_rc** hsy_white;
static size_t hsy_white_count, hsy_white_cap;
static void hsy_collect_white(hsy_rc* s) {
    if (s->color != HSY_WHITE || s->buffered) return;
    s->color = HSY_BLACK;
    s->children(s, hsy_collect_white);
    if (hsy_white_count == hsy_white_cap) {
        hsy_white_cap = hsy_white_cap ? 2 * hsy_white_cap : 64;
        hsy_white = hsy_realloc(hsy_white, hsy_white_cap * sizeof *hsy_white);
    }
    hsy_white[hsy_white_count++] = s;
}

static void hsy_collect_cycles(void) {
    size_t kept = 0;
    for (size_t i = 0; i < hsy_root_count; i++) {
        hsy_rc* s = hsy_roots[i];
        if (s->color == HSY_PURPLE && s->count > 0) {
            hsy_mark_gray(s);
            hsy_roots[kept++] = s;
        } else {
            s->buffered = 0;
            if (s->color == HSY_BLACK && s->count == 0) free(s);
        }
    }
    hsy_root_count = 0;
    for (size_t i = 0; i < kept; i++) hsy_scan(hsy_roots[i]);
    for (size_t i = 0; i < kept; i++) {
        hsy_roots[i]->buffered = 0;
        hsy_collect_white(hsy_roots[i]);
    }
    // The values of a garbage cycle are released without counting: they all go.
    hsy_collecting = 1;
    for (size_t i = 0; i < hsy_white_count; i++) hsy_white[i]->release(hsy_white[i]);
    hsy_collecting = 0;
    for (size_t i = 0; i < hsy_white_count; i++) free(hsy_white[i]);
    hsy_white_count = 0;
}

HSY_UNUSED static void hsy_shared_exit(void) {
    hsy_collect_cycles();
    free(hsy_roots);
    free(hsy_white);
}

"#;

/// The header of every `shared` block, declared with the other types.
pub const SHARED_HEADER: &str = "typedef struct hsy_rc hsy_rc;\nstruct hsy_rc {\n    size_t count;\n    \
uint8_t color;\n    uint8_t buffered;\n    void (*children)(hsy_rc*, void (*)(hsy_rc*));\n    void (*release)(hsy_rc*);\n};\n\n";

/// Channels: a queue of moved values behind a lock. The channel goes away with its last
/// sender and its receiver; a message sent once the receiver is gone is released at once.
pub const CHANNEL: &str = r#"/* A thread waiting in a `select`, woken by any of the channels it watches. */
typedef struct hsy_waiter {
    pthread_mutex_t lock;
    pthread_cond_t ready;
    bool woken;
} hsy_waiter;

typedef struct hsy_watch {
    hsy_waiter* waiter;
    struct hsy_watch* next;
} hsy_watch;

typedef struct hsy_chan_node {
    struct hsy_chan_node* next;
    max_align_t data[];
} hsy_chan_node;

typedef struct hsy_chan {
    pthread_mutex_t lock;
    pthread_cond_t ready;
    hsy_chan_node* head;
    hsy_chan_node* tail;
    size_t senders;
    bool receiving;
    size_t item_size;
    void (*drop_item)(void*);
    hsy_watch* watchers;
} hsy_chan;

HSY_UNUSED static hsy_chan* hsy_chan_new(size_t item_size, void (*drop_item)(void*)) {
    hsy_chan* c = hsy_alloc(sizeof *c);
    pthread_mutex_init(&c->lock, NULL);
    pthread_cond_init(&c->ready, NULL);
    c->head = c->tail = NULL;
    c->senders = 1;
    c->receiving = true;
    c->item_size = item_size;
    c->drop_item = drop_item;
    c->watchers = NULL;
    return c;
}

/* With the lock held: tells the threads waiting in a `select` that something changed. */
static void hsy_chan_wake(hsy_chan* c) {
    for (hsy_watch* w = c->watchers; w; w = w->next) {
        pthread_mutex_lock(&w->waiter->lock);
        w->waiter->woken = true;
        pthread_cond_signal(&w->waiter->ready);
        pthread_mutex_unlock(&w->waiter->lock);
    }
}

/* With the lock held: drops what is queued. */
static void hsy_chan_drain(hsy_chan* c) {
    while (c->head) {
        hsy_chan_node* node = c->head;
        c->head = node->next;
        if (c->drop_item) c->drop_item(node->data);
        free(node);
    }
    c->tail = NULL;
}

/* With the lock held: frees the channel once both ends are gone, and unlocks it otherwise. */
static void hsy_chan_release(hsy_chan* c) {
    if (c->senders || c->receiving) {
        pthread_mutex_unlock(&c->lock);
        return;
    }
    hsy_chan_drain(c);
    pthread_mutex_unlock(&c->lock);
    pthread_mutex_destroy(&c->lock);
    pthread_cond_destroy(&c->ready);
    free(c);
}

HSY_UNUSED static void hsy_chan_send(hsy_chan* c, void* item) {
    pthread_mutex_lock(&c->lock);
    if (!c->receiving) {
        pthread_mutex_unlock(&c->lock);
        if (c->drop_item) c->drop_item(item);
        return;
    }
    hsy_chan_node* node = hsy_alloc(sizeof *node + c->item_size);
    memcpy(node->data, item, c->item_size);
    node->next = NULL;
    if (c->tail) c->tail->next = node; else c->head = node;
    c->tail = node;
    pthread_cond_signal(&c->ready);
    hsy_chan_wake(c);
    pthread_mutex_unlock(&c->lock);
}

HSY_UNUSED static bool hsy_chan_receive(hsy_chan* c, void* out) {
    pthread_mutex_lock(&c->lock);
    while (!c->head && c->senders) pthread_cond_wait(&c->ready, &c->lock);
    hsy_chan_node* node = c->head;
    if (node) {
        c->head = node->next;
        if (!c->head) c->tail = NULL;
        memcpy(out, node->data, c->item_size);
    }
    pthread_mutex_unlock(&c->lock);
    bool received = node != NULL;
    free(node);
    return received;
}

HSY_UNUSED static void hsy_chan_add_sender(hsy_chan* c) {
    pthread_mutex_lock(&c->lock);
    c->senders++;
    pthread_mutex_unlock(&c->lock);
}

HSY_UNUSED static void hsy_chan_drop_sender(hsy_chan* c) {
    pthread_mutex_lock(&c->lock);
    c->senders--;
    pthread_cond_broadcast(&c->ready);
    hsy_chan_wake(c);
    hsy_chan_release(c);
}

HSY_UNUSED static void hsy_chan_drop_receiver(hsy_chan* c) {
    pthread_mutex_lock(&c->lock);
    c->receiving = false;
    hsy_chan_drain(c);
    hsy_chan_release(c);
}

"#;

/// The threads of a `with scope()` block, waited for at its end.
pub const SCOPE_HEADER: &str = "#include <pthread.h>\n\ntypedef struct hsy_scope_entry {\n    void* block;\n    \
void (*finish)(void*);\n} hsy_scope_entry;\n\ntypedef struct hsy_scope {\n    hsy_scope_entry* items;\n    \
size_t len;\n    size_t cap;\n} hsy_scope;\n\n";

pub const SCOPE: &str = r#"HSY_UNUSED static void hsy_scope_add(hsy_scope* s, void* block, void (*finish)(void*)) {
    if (s->len == s->cap) {
        s->cap = s->cap ? 2 * s->cap : 4;
        s->items = hsy_realloc(s->items, s->cap * sizeof *s->items);
    }
    s->items[s->len++] = (hsy_scope_entry){block, finish};
}

/* Waits for every thread of the scope not joined yet, and releases them all. */
HSY_UNUSED static void hsy_scope_end(hsy_scope* s) {
    for (size_t i = 0; i < s->len; i++) s->items[i].finish(s->items[i].block);
    free(s->items);
    s->items = NULL;
    s->len = s->cap = 0;
}

"#;

/// `parallel for`: the items split into one slice per core, the first run by the calling thread.
pub const PARALLEL: &str = r#"#ifdef _WIN32
int pthread_num_processors_np(void);
#else
#include <unistd.h>
#endif

typedef struct hsy_slice {
    void (*run)(void*, size_t, size_t);
    void* frame;
    size_t lo;
    size_t hi;
} hsy_slice;

static void* hsy_slice_run(void* p) {
    hsy_slice* s = p;
    s->run(s->frame, s->lo, s->hi);
    return NULL;
}

static size_t hsy_cores(void) {
#ifdef _WIN32
    int n = pthread_num_processors_np();
#else
    long n = sysconf(_SC_NPROCESSORS_ONLN);
#endif
    return n > 0 ? (size_t)n : 1;
}

/* How many items `range(start, end, step)` has. */
HSY_UNUSED static size_t hsy_range_count(__int128 start, __int128 end, __int128 step) {
    if (step > 0) return end > start ? (size_t)((end - start + step - 1) / step) : 0;
    return start > end ? (size_t)((start - end - step - 1) / -step) : 0;
}

static void hsy_parallel(size_t n, void (*run)(void*, size_t, size_t), void* frame) {
    size_t k = hsy_cores();
    if (k > n) k = n;
    if (k <= 1) {
        if (n) run(frame, 0, n);
        return;
    }
    hsy_slice* slices = hsy_alloc(k * sizeof *slices);
    pthread_t* ids = hsy_alloc(k * sizeof *ids);
    for (size_t i = 0; i < k; i++) slices[i] = (hsy_slice){run, frame, n * i / k, n * (i + 1) / k};
    for (size_t i = 1; i < k; i++) {
        if (pthread_create(&ids[i], NULL, hsy_slice_run, &slices[i])) hsy_panic();
    }
    hsy_slice_run(&slices[0]);
    for (size_t i = 1; i < k; i++) pthread_join(ids[i], NULL);
    free(slices);
    free(ids);
}

"#;

/// `select`: each channel tells the waiting thread when a message comes or its last sender
/// goes; the channels are tried from a random one, so that none is always served first.
pub const SELECT: &str = r#"#include <time.h>

/* Takes a message without waiting: 1 when one came, 2 when none ever will, 0 otherwise. */
static int hsy_chan_try_receive(hsy_chan* c, void* out) {
    pthread_mutex_lock(&c->lock);
    hsy_chan_node* node = c->head;
    if (node) {
        c->head = node->next;
        if (!c->head) c->tail = NULL;
        memcpy(out, node->data, c->item_size);
    }
    int found = node ? 1 : c->senders ? 0 : 2;
    pthread_mutex_unlock(&c->lock);
    free(node);
    return found;
}

static void hsy_chan_watch(hsy_chan* c, hsy_watch* w) {
    pthread_mutex_lock(&c->lock);
    w->next = c->watchers;
    c->watchers = w;
    pthread_mutex_unlock(&c->lock);
}

static void hsy_chan_unwatch(hsy_chan* c, hsy_watch* w) {
    pthread_mutex_lock(&c->lock);
    for (hsy_watch** at = &c->watchers; *at; at = &(*at)->next) {
        if (*at == w) {
            *at = w->next;
            break;
        }
    }
    pthread_mutex_unlock(&c->lock);
}

/* A time limit in nanoseconds, from `time.sleep(duration, unit)`. */
HSY_UNUSED static int64_t hsy_select_limit(double duration, int64_t unit) {
    if (unit != 3600000000000 && unit != 60000000000 && unit != 1000000000 && unit != 1000000 && unit != 1000 && unit != 1) {
        fputs("time: the unit is time.h, time.m, time.s, time.ms, time.us or time.ns\n", stderr);
        hsy_panic();
    }
    return duration > 0 ? (int64_t)(duration * (double)unit) : 0;
}

/* Waits for the first of `n` channels to give a message or to lose its last sender, at most
   `limit` nanoseconds (no limit when negative, no wait when 0). Gives that channel's index,
   with `*got` set when a message came, or `n` when the time ran out. */
static size_t hsy_select(hsy_chan** chans, void** outs, size_t n, int64_t limit, bool* got) {
    static __thread uint64_t seed;
    if (!seed) seed = (uint64_t)(uintptr_t)&seed ^ (uint64_t)time(NULL) ^ 0x9e3779b97f4a7c15u;
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    size_t first = (size_t)(seed % n);
    struct timespec deadline;
    if (limit > 0) {
        clock_gettime(CLOCK_REALTIME, &deadline);
        int64_t ns = deadline.tv_nsec + limit % 1000000000;
        deadline.tv_sec += (time_t)(limit / 1000000000 + ns / 1000000000);
        deadline.tv_nsec = (long)(ns % 1000000000);
    }
    hsy_waiter waiter;
    pthread_mutex_init(&waiter.lock, NULL);
    pthread_cond_init(&waiter.ready, NULL);
    hsy_watch* watches = hsy_alloc(n * sizeof *watches);
    for (size_t i = 0; i < n; i++) {
        watches[i].waiter = &waiter;
        hsy_chan_watch(chans[i], &watches[i]);
    }
    size_t which = n;
    for (;;) {
        pthread_mutex_lock(&waiter.lock);
        waiter.woken = false;
        pthread_mutex_unlock(&waiter.lock);
        for (size_t k = 0; k < n && which == n; k++) {
            size_t i = (first + k) % n;
            int found = hsy_chan_try_receive(chans[i], outs[i]);
            if (found) {
                which = i;
                *got = found == 1;
            }
        }
        if (which < n || limit == 0) break;
        bool late = false;
        pthread_mutex_lock(&waiter.lock);
        while (!waiter.woken && !late) {
            if (limit < 0) pthread_cond_wait(&waiter.ready, &waiter.lock);
            else late = pthread_cond_timedwait(&waiter.ready, &waiter.lock, &deadline) == ETIMEDOUT;
        }
        pthread_mutex_unlock(&waiter.lock);
        if (late) break;
    }
    for (size_t i = 0; i < n; i++) hsy_chan_unwatch(chans[i], &watches[i]);
    free(watches);
    pthread_mutex_destroy(&waiter.lock);
    pthread_cond_destroy(&waiter.ready);
    return which;
}

"#;

/// What `io` calls on a connection, in a program that makes none: never reached.
pub const CONN_STUB: &str = r#"int hsy_conn_getc(uint8_t* conn);
size_t hsy_conn_read(uint8_t* out, size_t count, uint8_t* conn);
int hsy_conn_failed(uint8_t* conn);
const uint8_t* hsy_conn_error(void);
void hsy_conn_close(uint8_t* conn);
int hsy_conn_getc(uint8_t* conn) { (void)conn; return -1; }
size_t hsy_conn_read(uint8_t* out, size_t count, uint8_t* conn) { (void)out; (void)count; (void)conn; return 0; }
int hsy_conn_failed(uint8_t* conn) { (void)conn; return 1; }
int hsy_conn_write(uint8_t* conn, const uint8_t* data, size_t n) { (void)conn; (void)data; (void)n; return -1; }
const uint8_t* hsy_conn_error(void) { return (const uint8_t*)""; }
void hsy_conn_close(uint8_t* conn) { (void)conn; }
void hsy_conn_shutdown(uint8_t* conn) { (void)conn; }

"#;

/// `net`: TCP connections, read through a buffer and written without one, and UDP sockets.
/// The last failure of the thread is kept as text, for `IoError`.
pub const NET: &str = r#"#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
typedef SOCKET hsy_sock;
#define HSY_NO_SOCKET INVALID_SOCKET
#define hsy_sock_close closesocket
#define HSY_NO_SIGNAL 0
#else
#include <sys/types.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/select.h>
#include <netdb.h>
#include <unistd.h>
#include <fcntl.h>
typedef int hsy_sock;
#define HSY_NO_SOCKET (-1)
#define hsy_sock_close close
#ifdef MSG_NOSIGNAL
#define HSY_NO_SIGNAL MSG_NOSIGNAL
#else
#define HSY_NO_SIGNAL 0
#endif
#endif

static __thread char hsy_net_reason[256];
static __thread char hsy_net_name[1100];

/* The reason the system gives for the last failure. */
static void hsy_net_failed(void) {
#ifdef _WIN32
    int code = WSAGetLastError();
    if (code == WSAETIMEDOUT || code == WSAEWOULDBLOCK) {
        snprintf(hsy_net_reason, sizeof hsy_net_reason, "timed out");
        return;
    }
    DWORD n = FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, NULL, (DWORD)code, 0,
                             hsy_net_reason, sizeof hsy_net_reason, NULL);
    while (n > 0 && (hsy_net_reason[n - 1] == '\n' || hsy_net_reason[n - 1] == '\r' || hsy_net_reason[n - 1] == '.'))
        hsy_net_reason[--n] = 0;
    if (n == 0) snprintf(hsy_net_reason, sizeof hsy_net_reason, "network error %d", code);
#else
    bool late = errno == EAGAIN || errno == EWOULDBLOCK || errno == ETIMEDOUT || errno == EINPROGRESS;
    snprintf(hsy_net_reason, sizeof hsy_net_reason, "%s", late ? "timed out" : strerror(errno));
#endif
}

/* Waits until `s` can be read (or written, with `writing`), `seconds` at most. */
static bool hsy_net_ready(hsy_sock s, double seconds, bool writing) {
    fd_set set;
    FD_ZERO(&set);
    FD_SET(s, &set);
    struct timeval limit;
    limit.tv_sec = (long)seconds;
    limit.tv_usec = (long)((seconds - (double)(long)seconds) * 1e6);
    int found = select((int)s + 1, writing ? NULL : &set, writing ? &set : NULL, NULL, &limit);
    if (found > 0) return true;
    if (found == 0) snprintf(hsy_net_reason, sizeof hsy_net_reason, "timed out");
    else hsy_net_failed();
    return false;
}

/* Reads and writes on `s` fail once they have waited `seconds`; 0 waits without limit. */
static void hsy_net_limit(hsy_sock s, double seconds) {
    if (seconds <= 0) return;
#ifdef _WIN32
    DWORD limit = (DWORD)(seconds * 1000.0);
    if (limit == 0) limit = 1;
#else
    struct timeval limit;
    limit.tv_sec = (long)seconds;
    limit.tv_usec = (long)((seconds - (double)(long)seconds) * 1e6);
    if (limit.tv_sec == 0 && limit.tv_usec == 0) limit.tv_usec = 1;
#endif
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, (const char*)&limit, sizeof limit);
    setsockopt(s, SOL_SOCKET, SO_SNDTIMEO, (const char*)&limit, sizeof limit);
}

/* `connect` that gives up after `seconds`, when there is a limit. */
static int hsy_net_reach(hsy_sock s, const struct sockaddr* address, socklen_t len, double seconds) {
    if (seconds <= 0) return connect(s, address, len);
#ifdef _WIN32
    u_long on = 1, off = 0;
    ioctlsocket(s, FIONBIO, &on);
    int done = connect(s, address, len);
    bool waiting = done != 0 && WSAGetLastError() == WSAEWOULDBLOCK;
#else
    int flags = fcntl(s, F_GETFL, 0);
    fcntl(s, F_SETFL, flags | O_NONBLOCK);
    int done = connect(s, address, len);
    bool waiting = done != 0 && errno == EINPROGRESS;
#endif
    if (waiting) {
        done = -1;
        if (hsy_net_ready(s, seconds, true)) {
            int problem = 0;
            socklen_t size = sizeof problem;
            getsockopt(s, SOL_SOCKET, SO_ERROR, (char*)&problem, &size);
            if (problem == 0) done = 0;
#ifdef _WIN32
            else WSASetLastError(problem);
#else
            else errno = problem;
#endif
            if (problem != 0) hsy_net_failed();
        }
    } else if (done != 0) {
        hsy_net_failed();
    }
#ifdef _WIN32
    ioctlsocket(s, FIONBIO, &off);
#else
    fcntl(s, F_SETFL, flags);
#endif
    return done;
}

const uint8_t* hsy_net_error(void);
const uint8_t* hsy_conn_error(void);
const uint8_t* hsy_net_error(void) { return (const uint8_t*)hsy_net_reason; }
const uint8_t* hsy_conn_error(void) { return (const uint8_t*)hsy_net_reason; }

static struct addrinfo* hsy_net_resolve(const char* host, int32_t port, int type, int family, bool passive) {
#ifdef _WIN32
    WSADATA started;
    WSAStartup(MAKEWORD(2, 2), &started);
#endif
    if (port < 0 || port > 65535) {
        snprintf(hsy_net_reason, sizeof hsy_net_reason, "the port %d is not between 0 and 65535", (int)port);
        return NULL;
    }
    char service[8];
    snprintf(service, sizeof service, "%d", (int)port);
    struct addrinfo hints;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = family;
    hints.ai_socktype = type;
    hints.ai_flags = passive ? AI_PASSIVE : 0;
    struct addrinfo* found = NULL;
    int code = getaddrinfo(host[0] ? host : NULL, service, &hints, &found);
    if (code != 0) {
        snprintf(hsy_net_reason, sizeof hsy_net_reason, "%s", gai_strerror(code));
        return NULL;
    }
    return found;
}

static hsy_sock hsy_net_socket(const struct addrinfo* a) {
    hsy_sock s = socket(a->ai_family, a->ai_socktype, a->ai_protocol);
#ifdef SO_NOSIGPIPE
    if (s != HSY_NO_SOCKET) {
        int on = 1;
        setsockopt(s, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
    }
#endif
    return s;
}

/* `host:port`, or `[host]:port` for IPv6, as numbers. */
static const uint8_t* hsy_net_text(const struct sockaddr* a, socklen_t len) {
    char host[1025], port[32];
    if (getnameinfo(a, len, host, sizeof host, port, sizeof port, NI_NUMERICHOST | NI_NUMERICSERV) != 0)
        snprintf(hsy_net_name, sizeof hsy_net_name, "?");
    else
        snprintf(hsy_net_name, sizeof hsy_net_name, a->sa_family == AF_INET6 ? "[%s]:%s" : "%s:%s", host, port);
    return (const uint8_t*)hsy_net_name;
}

/* A socket for the first address of `host` that takes it: connected, or bound. -1 on failure. */
static int64_t hsy_net_open(const char* host, int32_t port, int type, bool bind_it, double seconds) {
    struct addrinfo* found = hsy_net_resolve(host, port, type, AF_UNSPEC, bind_it);
    if (!found) return -1;
    hsy_sock s = HSY_NO_SOCKET;
    for (struct addrinfo* a = found; a; a = a->ai_next) {
        s = hsy_net_socket(a);
        if (s == HSY_NO_SOCKET) {
            hsy_net_failed();
            continue;
        }
#ifndef _WIN32
        if (bind_it) {
            int on = 1;
            setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on);
        }
#endif
        int done;
        if (bind_it) {
            done = bind(s, a->ai_addr, (socklen_t)a->ai_addrlen);
            if (done != 0) hsy_net_failed();
        } else {
            done = hsy_net_reach(s, a->ai_addr, (socklen_t)a->ai_addrlen, seconds);
            if (done != 0 && seconds <= 0) hsy_net_failed();
        }
        if (done == 0) {
            hsy_net_limit(s, seconds);
            break;
        }
        hsy_sock_close(s);
        s = HSY_NO_SOCKET;
    }
    freeaddrinfo(found);
    return s == HSY_NO_SOCKET ? -1 : (int64_t)s;
}

int64_t hsy_net_connect(const char* host, int32_t port, double seconds);
int64_t hsy_net_listen(const char* host, int32_t port);
int64_t hsy_net_accept(int64_t listener, double seconds);
int64_t hsy_net_udp(const char* host, int32_t port, double seconds);
int hsy_net_send(int64_t s, const uint8_t* data, size_t n, const char* host, int32_t port);
int64_t hsy_net_receive(int64_t s, uint8_t* out, size_t size);
const uint8_t* hsy_net_peer(int64_t s);
const uint8_t* hsy_net_sender(void);
void hsy_net_close(int64_t s);
uint8_t* hsy_net_wrap(int64_t s);

int64_t hsy_net_connect(const char* host, int32_t port, double seconds) {
    return hsy_net_open(host, port, SOCK_STREAM, false, seconds);
}

int64_t hsy_net_listen(const char* host, int32_t port) {
    int64_t s = hsy_net_open(host, port, SOCK_STREAM, true, 0);
    if (s >= 0 && listen((hsy_sock)s, 128) != 0) {
        hsy_net_failed();
        hsy_sock_close((hsy_sock)s);
        return -1;
    }
    return s;
}

int64_t hsy_net_accept(int64_t listener, double seconds) {
    if (seconds > 0 && !hsy_net_ready((hsy_sock)listener, seconds, false)) return -1;
    hsy_sock s = accept((hsy_sock)listener, NULL, NULL);
    if (s == HSY_NO_SOCKET) {
        hsy_net_failed();
        return -1;
    }
#ifdef SO_NOSIGPIPE
    int on = 1;
    setsockopt(s, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof on);
#endif
    hsy_net_limit(s, seconds);
    return (int64_t)s;
}

const uint8_t* hsy_net_peer(int64_t s) {
    struct sockaddr_storage a;
    socklen_t len = sizeof a;
    if (getpeername((hsy_sock)s, (struct sockaddr*)&a, &len) != 0) return (const uint8_t*)"?";
    return hsy_net_text((struct sockaddr*)&a, len);
}

int64_t hsy_net_udp(const char* host, int32_t port, double seconds) {
    return hsy_net_open(host, port, SOCK_DGRAM, true, seconds);
}

int hsy_net_send(int64_t s, const uint8_t* data, size_t n, const char* host, int32_t port) {
    struct sockaddr_storage own;
    socklen_t len = sizeof own;
    int family = getsockname((hsy_sock)s, (struct sockaddr*)&own, &len) == 0 ? own.ss_family : AF_UNSPEC;
    struct addrinfo* found = hsy_net_resolve(host, port, SOCK_DGRAM, family, false);
    if (!found) return -1;
    long sent = (long)sendto((hsy_sock)s, (const char*)data, (int)n, 0, found->ai_addr, (socklen_t)found->ai_addrlen);
    freeaddrinfo(found);
    if (sent < 0) {
        hsy_net_failed();
        return -1;
    }
    return 0;
}

int64_t hsy_net_receive(int64_t s, uint8_t* out, size_t size) {
    struct sockaddr_storage from;
    socklen_t len = sizeof from;
    long n = (long)recvfrom((hsy_sock)s, (char*)out, (int)size, 0, (struct sockaddr*)&from, &len);
    if (n < 0) {
        hsy_net_failed();
        return -1;
    }
    hsy_net_text((struct sockaddr*)&from, len);
    return n;
}

const uint8_t* hsy_net_sender(void) { return (const uint8_t*)hsy_net_name; }

void hsy_net_close(int64_t s) { hsy_sock_close((hsy_sock)s); }

/* A connection as `io` reads it: through a buffer. `tls`, when set, is the TLS session that
   reads and writes go through, with its own functions: the code that sets it is only in the
   programs that open TLS connections, and only they are linked with the TLS library. */
typedef struct hsy_conn {
    hsy_sock sock;
    size_t len, pos;
    bool failed;
    void* tls;
    long (*tls_recv)(void* tls, uint8_t* out, size_t n);
    long (*tls_send)(void* tls, const uint8_t* data, size_t n);
    void (*tls_close)(void* tls);
    uint8_t data[4096];
} hsy_conn;

uint8_t* hsy_net_wrap(int64_t s) {
    hsy_conn* c = hsy_alloc(sizeof *c);
    c->sock = (hsy_sock)s;
    c->len = c->pos = 0;
    c->failed = false;
    c->tls = NULL;
    c->tls_recv = NULL;
    c->tls_send = NULL;
    c->tls_close = NULL;
    return (uint8_t*)c;
}

/* Server TLS, set by the TLS code when a program makes a TLS listener: an accepted socket is
   wrapped through the listener's context, and the context freed with the listener. */
static uint8_t* (*hsy_tls_accept_hook)(int64_t s, uint8_t* context) = NULL;
static void (*hsy_tls_free_hook)(uint8_t* context) = NULL;

uint8_t* hsy_net_accepted(int64_t s, uint8_t* context);
void hsy_net_free_context(uint8_t* context);

/* The connection of an accepted socket: TLS when the listener has a context; NULL if its
   handshake failed. */
uint8_t* hsy_net_accepted(int64_t s, uint8_t* context) {
    if (context && hsy_tls_accept_hook) return hsy_tls_accept_hook(s, context);
    return hsy_net_wrap(s);
}

void hsy_net_free_context(uint8_t* context) {
    if (context && hsy_tls_free_hook) hsy_tls_free_hook(context);
}

/* Bytes in the buffer, waiting for some if it is empty; false at the end, or on a failure. */
static bool hsy_conn_fill(hsy_conn* c) {
    if (c->pos < c->len) return true;
    for (;;) {
        if (c->tls) {
            long n = c->tls_recv(c->tls, c->data, sizeof c->data);
            if (n > 0) {
                c->pos = 0;
                c->len = (size_t)n;
                return true;
            }
            if (n < 0) c->failed = true;
            return false;
        }
        long n = (long)recv(c->sock, (char*)c->data, (int)sizeof c->data, 0);
        if (n > 0) {
            c->pos = 0;
            c->len = (size_t)n;
            return true;
        }
#ifndef _WIN32
        if (n < 0 && errno == EINTR) continue;
#endif
        if (n < 0) {
            c->failed = true;
            hsy_net_failed();
        }
        return false;
    }
}

int hsy_conn_getc(uint8_t* conn);
size_t hsy_conn_read(uint8_t* out, size_t count, uint8_t* conn);
int hsy_conn_failed(uint8_t* conn);
void hsy_conn_close(uint8_t* conn);
void hsy_conn_shutdown(uint8_t* conn);

int hsy_conn_getc(uint8_t* conn) {
    hsy_conn* c = (hsy_conn*)conn;
    return hsy_conn_fill(c) ? c->data[c->pos++] : -1;
}

/* What the buffer holds, or what one wait brings, up to `count` bytes; 0 at the end. */
size_t hsy_conn_read(uint8_t* out, size_t count, uint8_t* conn) {
    hsy_conn* c = (hsy_conn*)conn;
    if (!hsy_conn_fill(c)) return 0;
    size_t n = c->len - c->pos < count ? c->len - c->pos : count;
    memcpy(out, c->data + c->pos, n);
    c->pos += n;
    return n;
}

int hsy_conn_failed(uint8_t* conn) { return ((hsy_conn*)conn)->failed; }

int hsy_conn_write(uint8_t* conn, const uint8_t* data, size_t n) {
    hsy_conn* c = (hsy_conn*)conn;
    if (c->tls) return n == 0 ? 0 : (c->tls_send(c->tls, data, n) < 0 ? -1 : 0);
    while (n > 0) {
        long sent = (long)send(c->sock, (const char*)data, (int)(n > 65536 ? 65536 : n), HSY_NO_SIGNAL);
#ifndef _WIN32
        if (sent < 0 && errno == EINTR) continue;
#endif
        if (sent < 0) {
            hsy_net_failed();
            return -1;
        }
        data += sent;
        n -= (size_t)sent;
    }
    return 0;
}

void hsy_conn_close(uint8_t* conn) {
    hsy_conn* c = (hsy_conn*)conn;
    if (c->tls) c->tls_close(c->tls);
    hsy_sock_close(c->sock);
    free(c);
}

/* Half-closes both directions, waking any blocked read on this connection. */
void hsy_conn_shutdown(uint8_t* conn) {
    hsy_conn* c = (hsy_conn*)conn;
#ifdef _WIN32
    shutdown(c->sock, SD_BOTH);
#else
    shutdown(c->sock, SHUT_RDWR);
#endif
}

"#;

/// TLS over a `net` connection, through OpenSSL: certificates checked against the system's
/// trusted ones and the host name, TLS 1.2 at least. Comes after `NET`, whose `hsy_conn` it
/// fills in.
pub const TLS: &str = r#"#ifndef _WIN32
#include <arpa/inet.h>
#endif
#include <openssl/ssl.h>
#include <openssl/err.h>
#include <openssl/x509v3.h>
#include <openssl/pem.h>
#include <openssl/bn.h>

typedef struct hsy_tls {
    SSL_CTX* ctx;
    SSL* ssl;
} hsy_tls;

/* Keeps the reason of a TLS failure: the certificate check, the library's, or the system's. */
static void hsy_tls_failed(SSL* ssl, int result, const char* doing) {
    long verified = ssl ? SSL_get_verify_result(ssl) : X509_V_OK;
    unsigned long code = ERR_peek_last_error();
    int kind = ssl ? SSL_get_error(ssl, result) : SSL_ERROR_SSL;
    if (verified != X509_V_OK) {
        snprintf(hsy_net_reason, sizeof hsy_net_reason, "certificate rejected: %s",
                 X509_verify_cert_error_string(verified));
    } else if (kind == SSL_ERROR_WANT_READ || kind == SSL_ERROR_WANT_WRITE) {
        /* A blocking socket only asks to wait again when its time limit is reached. */
        snprintf(hsy_net_reason, sizeof hsy_net_reason, "timed out");
    } else if (kind == SSL_ERROR_SYSCALL && code == 0) {
        hsy_net_failed();
        if (result == 0) snprintf(hsy_net_reason, sizeof hsy_net_reason, "connection closed during %s", doing);
    } else if (code != 0) {
        char text[200];
        ERR_error_string_n(code, text, sizeof text);
        snprintf(hsy_net_reason, sizeof hsy_net_reason, "TLS %s failed: %s", doing, text);
    } else {
        snprintf(hsy_net_reason, sizeof hsy_net_reason, "TLS %s failed", doing);
    }
    ERR_clear_error();
}

static long hsy_tls_recv(void* tls, uint8_t* out, size_t n) {
    SSL* ssl = ((hsy_tls*)tls)->ssl;
    int got = SSL_read(ssl, out, n > 65536 ? 65536 : (int)n);
    if (got > 0) return got;
    if (SSL_get_error(ssl, got) == SSL_ERROR_ZERO_RETURN) return 0;
    hsy_tls_failed(ssl, got, "read");
    return -1;
}

static long hsy_tls_send(void* tls, const uint8_t* data, size_t n) {
    SSL* ssl = ((hsy_tls*)tls)->ssl;
    while (n > 0) {
        int sent = SSL_write(ssl, data, n > 65536 ? 65536 : (int)n);
        if (sent <= 0) {
            hsy_tls_failed(ssl, sent, "write");
            return -1;
        }
        data += sent;
        n -= (size_t)sent;
    }
    return 0;
}

static void hsy_tls_close(void* tls) {
    hsy_tls* t = tls;
    SSL_shutdown(t->ssl);
    SSL_free(t->ssl);
    SSL_CTX_free(t->ctx);
    free(t);
    ERR_clear_error();
}

static X509* hsy_tls_read_cert(const char* pem) {
    BIO* bio = BIO_new_mem_buf(pem, -1);
    X509* x = bio ? PEM_read_bio_X509(bio, NULL, NULL, NULL) : NULL;
    BIO_free(bio);
    return x;
}

static EVP_PKEY* hsy_tls_read_key(const char* pem) {
    BIO* bio = BIO_new_mem_buf(pem, -1);
    EVP_PKEY* k = bio ? PEM_read_bio_PrivateKey(bio, NULL, NULL, NULL) : NULL;
    BIO_free(bio);
    return k;
}

static bool hsy_tls_is_ip(const char* host) {
    unsigned char address[16];
    return inet_pton(AF_INET, host, address) == 1 || inet_pton(AF_INET6, host, address) == 1;
}

uint8_t* hsy_tls_wrap(int64_t s, const char* host, int32_t verify, const char* authority);
uint8_t* hsy_tls_server(const char* cert, const char* key);
uint8_t* hsy_tls_certificate(const char* name, const char* names, const char* signer_cert,
                             const char* signer_key, int32_t authority, int32_t days, int32_t rsa);
int32_t hsy_tls_check(const char* cert, const char* key);

/* Starts TLS on the connected socket `s`, for `host`; NULL on a failure, `s` closed then. */
uint8_t* hsy_tls_wrap(int64_t s, const char* host, int32_t verify, const char* authority) {
    SSL_CTX* ctx = SSL_CTX_new(TLS_client_method());
    SSL* ssl = NULL;
    if (!ctx) goto failed;
    SSL_CTX_set_min_proto_version(ctx, TLS1_2_VERSION);
#ifdef SSL_OP_IGNORE_UNEXPECTED_EOF
    /* Many servers close without telling TLS first; HTTP lengths catch a cut answer. */
    SSL_CTX_set_options(ctx, SSL_OP_IGNORE_UNEXPECTED_EOF);
#endif
    if (verify) {
        SSL_CTX_set_verify(ctx, SSL_VERIFY_PEER, NULL);
        if (authority) {
            /* Only certificates this authority signed: the system's are not looked at. */
            X509* ca = hsy_tls_read_cert(authority);
            bool added = ca && X509_STORE_add_cert(SSL_CTX_get_cert_store(ctx), ca) == 1;
            X509_free(ca);
            if (!added) goto failed;
        } else if (SSL_CTX_set_default_verify_paths(ctx) != 1) {
            goto failed;
        }
    }
    ssl = SSL_new(ctx);
    if (!ssl || SSL_set_fd(ssl, (int)s) != 1) goto failed;
    bool ip = hsy_tls_is_ip(host);
    if (!ip) SSL_set_tlsext_host_name(ssl, host);
    if (verify) {
        X509_VERIFY_PARAM* param = SSL_get0_param(ssl);
        X509_VERIFY_PARAM_set_hostflags(param, X509_CHECK_FLAG_NO_PARTIAL_WILDCARDS);
        if ((ip ? X509_VERIFY_PARAM_set1_ip_asc(param, host) : X509_VERIFY_PARAM_set1_host(param, host, 0)) != 1)
            goto failed;
    }
    int done = SSL_connect(ssl);
    if (done != 1) {
        hsy_tls_failed(ssl, done, "handshake");
        SSL_free(ssl);
        SSL_CTX_free(ctx);
        hsy_sock_close((hsy_sock)s);
        return NULL;
    }
    hsy_tls* t = hsy_alloc(sizeof *t);
    t->ctx = ctx;
    t->ssl = ssl;
    hsy_conn* c = (hsy_conn*)hsy_net_wrap(s);
    c->tls = t;
    c->tls_recv = hsy_tls_recv;
    c->tls_send = hsy_tls_send;
    c->tls_close = hsy_tls_close;
    return (uint8_t*)c;
failed:
    hsy_tls_failed(NULL, 0, "setup");
    if (ssl) SSL_free(ssl);
    if (ctx) SSL_CTX_free(ctx);
    hsy_sock_close((hsy_sock)s);
    return NULL;
}

/* --- server side ------------------------------------------------------------------------- */

static void hsy_tls_reason(const char* text) {
    snprintf(hsy_net_reason, sizeof hsy_net_reason, "%s", text);
    ERR_clear_error();
}

/* 0 when `cert` and `key` are a PEM certificate and the private key that goes with it. */
int32_t hsy_tls_check(const char* cert, const char* key) {
    X509* x = hsy_tls_read_cert(cert);
    EVP_PKEY* k = hsy_tls_read_key(key);
    int32_t result = 0;
    if (!x) {
        hsy_tls_reason("not a PEM certificate");
        result = -1;
    } else if (!k) {
        hsy_tls_reason("not a PEM private key");
        result = -1;
    } else if (X509_check_private_key(x, k) != 1) {
        hsy_tls_reason("the key does not go with the certificate");
        result = -1;
    }
    X509_free(x);
    EVP_PKEY_free(k);
    return result;
}

static uint8_t* hsy_tls_accept(int64_t s, uint8_t* context) {
    SSL* ssl = SSL_new((SSL_CTX*)context);
    if (!ssl || SSL_set_fd(ssl, (int)s) != 1) {
        hsy_tls_failed(NULL, 0, "setup");
        if (ssl) SSL_free(ssl);
        hsy_sock_close((hsy_sock)s);
        return NULL;
    }
    int done = SSL_accept(ssl);
    if (done != 1) {
        hsy_tls_failed(ssl, done, "handshake");
        SSL_free(ssl);
        hsy_sock_close((hsy_sock)s);
        return NULL;
    }
    hsy_tls* t = hsy_alloc(sizeof *t);
    SSL_CTX_up_ref((SSL_CTX*)context);
    t->ctx = (SSL_CTX*)context;
    t->ssl = ssl;
    hsy_conn* c = (hsy_conn*)hsy_net_wrap(s);
    c->tls = t;
    c->tls_recv = hsy_tls_recv;
    c->tls_send = hsy_tls_send;
    c->tls_close = hsy_tls_close;
    return (uint8_t*)c;
}

static void hsy_tls_free_context(uint8_t* context) { SSL_CTX_free((SSL_CTX*)context); }

/* A context for accepting TLS connections with `cert` and `key` (PEM); NULL on a failure. */
uint8_t* hsy_tls_server(const char* cert, const char* key) {
    if (hsy_tls_check(cert, key) != 0) return NULL;
    SSL_CTX* ctx = SSL_CTX_new(TLS_server_method());
    X509* x = hsy_tls_read_cert(cert);
    EVP_PKEY* k = hsy_tls_read_key(key);
    bool ok = ctx && x && k && SSL_CTX_set_min_proto_version(ctx, TLS1_2_VERSION) == 1 &&
              SSL_CTX_use_certificate(ctx, x) == 1 && SSL_CTX_use_PrivateKey(ctx, k) == 1;
    X509_free(x);
    EVP_PKEY_free(k);
    if (!ok) {
        hsy_tls_failed(NULL, 0, "setup");
        SSL_CTX_free(ctx);
        return NULL;
    }
    hsy_tls_accept_hook = hsy_tls_accept;
    hsy_tls_free_hook = hsy_tls_free_context;
    return (uint8_t*)ctx;
}

static bool hsy_tls_extend(X509* x, X509* issuer, int nid, const char* value) {
    X509V3_CTX v3;
    X509V3_set_ctx_nodb(&v3);
    X509V3_set_ctx(&v3, issuer, x, NULL, NULL, 0);
    X509_EXTENSION* ext = X509V3_EXT_conf_nid(NULL, &v3, nid, value);
    if (!ext) return false;
    bool ok = X509_add_ext(x, ext, -1) == 1;
    X509_EXTENSION_free(ext);
    return ok;
}

/* Adds one name of `n` bytes to a subjectAltName list, as an IP address or a DNS name. */
static bool hsy_tls_add_alt(char* out, const char* one, size_t n) {
    char name[256];
    if (n == 0) return true;
    if (n >= sizeof name) return false;
    memcpy(name, one, n);
    name[n] = 0;
    if (out[0]) strcat(out, ",");
    strcat(out, hsy_tls_is_ip(name) ? "IP:" : "DNS:");
    strcat(out, name);
    return true;
}

/* The subjectAltName of a server certificate: `name`, then each line of `names`. */
static char* hsy_tls_alt_names(const char* name, const char* names) {
    char* out = malloc(strlen(name) * 2 + strlen(names) * 6 + 16);
    if (!out) return NULL;
    out[0] = 0;
    bool ok = hsy_tls_add_alt(out, name, strlen(name));
    const char* start = names;
    while (ok && *start) {
        const char* end = strchr(start, '\n');
        if (!end) end = start + strlen(start);
        ok = hsy_tls_add_alt(out, start, (size_t)(end - start));
        start = *end ? end + 1 : end;
    }
    if (!ok) {
        free(out);
        return NULL;
    }
    return out;
}

/* A new certificate for `name` (and the other `names`, one per line) with its own new key, as
   PEM text: the certificate, then the key. Signed by `signer_cert`/`signer_key`, or by itself
   when they are NULL. NULL on a failure, with the reason kept. */
uint8_t* hsy_tls_certificate(const char* name, const char* names, const char* signer_cert,
                             const char* signer_key, int32_t authority, int32_t days, int32_t rsa) {
    EVP_PKEY* key = rsa ? EVP_RSA_gen(3072) : EVP_EC_gen("P-256");
    X509* x = X509_new();
    X509* issuer = NULL;
    EVP_PKEY* issuer_key = NULL;
    BIGNUM* serial = BN_new();
    BIO* out = NULL;
    char* alt = NULL;
    uint8_t* text = NULL;
    const char* reason = NULL;
    if (!key || !x || !serial) goto done;
    if (signer_cert) {
        if (hsy_tls_check(signer_cert, signer_key) != 0) {
            reason = "the signer is not a certificate and its key";
            goto done;
        }
        issuer = hsy_tls_read_cert(signer_cert);
        issuer_key = hsy_tls_read_key(signer_key);
        if (X509_check_ca(issuer) == 0) {
            reason = "the signer is not an authority: make it with authority=True";
            goto done;
        }
    }
    X509_set_version(x, 2);
    if (BN_rand(serial, 127, BN_RAND_TOP_ANY, BN_RAND_BOTTOM_ANY) != 1 ||
        !BN_to_ASN1_INTEGER(serial, X509_get_serialNumber(x)))
        goto done;
    /* A few minutes back, for clocks that are a little behind. */
    X509_gmtime_adj(X509_getm_notBefore(x), -300);
    X509_gmtime_adj(X509_getm_notAfter(x), (long)days * 86400);
    X509_set_pubkey(x, key);
    X509_NAME* subject = X509_get_subject_name(x);
    if (X509_NAME_add_entry_by_txt(subject, "CN", MBSTRING_UTF8, (const unsigned char*)name, -1, -1, 0) != 1)
        goto done;
    X509_set_issuer_name(x, issuer ? X509_get_subject_name(issuer) : subject);
    X509* signing = issuer ? issuer : x;
    bool ok = hsy_tls_extend(x, signing, NID_subject_key_identifier, "hash") &&
              hsy_tls_extend(x, signing, NID_authority_key_identifier, "keyid:always");
    if (authority) {
        ok = ok && hsy_tls_extend(x, signing, NID_basic_constraints, "critical,CA:TRUE") &&
             hsy_tls_extend(x, signing, NID_key_usage, "critical,keyCertSign,cRLSign,digitalSignature");
    } else {
        alt = hsy_tls_alt_names(name, names);
        ok = ok && alt && hsy_tls_extend(x, signing, NID_basic_constraints, "critical,CA:FALSE") &&
             hsy_tls_extend(x, signing, NID_key_usage, rsa ? "critical,digitalSignature,keyEncipherment" : "critical,digitalSignature") &&
             hsy_tls_extend(x, signing, NID_ext_key_usage, "serverAuth,clientAuth") &&
             hsy_tls_extend(x, signing, NID_subject_alt_name, alt);
    }
    if (!ok || X509_sign(x, issuer_key ? issuer_key : key, EVP_sha256()) == 0) goto done;
    out = BIO_new(BIO_s_mem());
    if (!out || PEM_write_bio_X509(out, x) != 1 || PEM_write_bio_PrivateKey(out, key, NULL, NULL, 0, NULL, NULL) != 1)
        goto done;
    char* data = NULL;
    long n = BIO_get_mem_data(out, &data);
    text = malloc((size_t)n + 1);
    if (text) {
        memcpy(text, data, (size_t)n);
        text[n] = 0;
    }
done:
    if (!text) {
        if (reason) hsy_tls_reason(reason);
        else hsy_tls_failed(NULL, 0, "certificate");
    }
    free(alt);
    BIO_free(out);
    BN_free(serial);
    X509_free(x);
    X509_free(issuer);
    EVP_PKEY_free(issuer_key);
    EVP_PKEY_free(key);
    return text;
}

"#;
