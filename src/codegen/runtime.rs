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

/* FNV-1a over the bytes, then spread like the other hashes. */
HSY_UNUSED static uint64_t hsy_str_hash(const hsy_str* s) {
    uint64_t h = 0xcbf29ce484222325u;
    for (size_t i = 0; i < s->len; i++) h = (h ^ (uint8_t)s->data[i]) * 0x100000001b3u;
    return hsy_hash64(h);
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
    fputs("panic: ", stderr);
    fwrite(s->data ? s->data : "", 1, s->len, stderr);
    fputc('\n', stderr);
    hsy_panic();
}

HSY_UNUSED static void hsy_print(const hsy_str* s, const hsy_str* end, void* stream) {
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
