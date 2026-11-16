#define HSY_COUNT(a) (sizeof(a) / sizeof((a)[0]))

static bool hsy_case_in(const uint32_t (*set)[2], size_t n, uint32_t cp) {
    size_t lo = 0, hi = n;
    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2;
        if (cp < set[mid][0]) hi = mid;
        else if (cp > set[mid][1]) lo = mid + 1;
        else return true;
    }
    return false;
}

static uint32_t hsy_case_one(const hsy_case_range* rows, size_t n, uint32_t cp) {
    size_t lo = 0, hi = n;
    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2;
        if (cp < rows[mid].first) hi = mid;
        else if (cp > rows[mid].last) lo = mid + 1;
        else return (cp - rows[mid].first) % rows[mid].stride == 0 ? (uint32_t)((int32_t)cp + rows[mid].delta) : cp;
    }
    return cp;
}

size_t hsy_case_map(uint32_t cp, bool upper, uint32_t* out);
bool hsy_case_cased(uint32_t cp);
bool hsy_case_ignorable(uint32_t cp);

/* `cp` in capitals with `upper`, else in small letters: one to three code points in `out`. */
size_t hsy_case_map(uint32_t cp, bool upper, uint32_t* out) {
    const uint32_t (*many)[4] = upper ? hsy_case_upper_many : hsy_case_lower_many;
    size_t count = upper ? HSY_COUNT(hsy_case_upper_many) : HSY_COUNT(hsy_case_lower_many);
    for (size_t i = 0; i < count; i++) {
        if (many[i][0] == cp) {
            size_t n = 0;
            while (n < 3 && many[i][n + 1]) {
                out[n] = many[i][n + 1];
                n++;
            }
            return n;
        }
    }
    out[0] = upper ? hsy_case_one(hsy_case_upper, HSY_COUNT(hsy_case_upper), cp)
                   : hsy_case_one(hsy_case_lower, HSY_COUNT(hsy_case_lower), cp);
    return 1;
}

bool hsy_case_cased(uint32_t cp) { return hsy_case_in(hsy_case_cased_set, HSY_COUNT(hsy_case_cased_set), cp); }

bool hsy_case_ignorable(uint32_t cp) {
    return hsy_case_in(hsy_case_ignorable_set, HSY_COUNT(hsy_case_ignorable_set), cp);
}

