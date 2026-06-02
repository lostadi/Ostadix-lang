#include "value.h"

#include <ctype.h>
#include <errno.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ────────────────────────────────────────────────────────────────────── */
/* Internal helpers                                                      */
/* ────────────────────────────────────────────────────────────────────── */

typedef struct {
    char *buf;
    size_t len;
    size_t cap;
} StringBuilder;

typedef enum {
    JSON_NULL = 0,
    JSON_BOOL,
    JSON_NUMBER,
    JSON_STRING,
    JSON_ARRAY,
    JSON_OBJECT,
} JsonType;

typedef struct JsonNode JsonNode;

typedef struct JsonPair {
    char *key;
    JsonNode *value;
    struct JsonPair *next;
} JsonPair;

struct JsonNode {
    JsonType type;
    union {
        bool bool_val;
        char *str_val;
        struct {
            JsonNode **items;
            size_t len;
            size_t cap;
        } array;
        JsonPair *object;
    } u;
};

typedef struct {
    const char *src;
    size_t pos;
} JsonParser;

typedef struct {
    const OMapEntry *entry;
} MapEntryRef;

static char *dup_cstr(const char *s) {
    size_t len;
    char *copy;

    if (s == NULL) {
        return NULL;
    }
    len = strlen(s);
    copy = (char *)malloc(len + 1U);
    if (copy == NULL) {
        return NULL;
    }
    memcpy(copy, s, len + 1U);
    return copy;
}

static OValue *oval_alloc(OValueTag tag) {
    OValue *v = (OValue *)calloc(1U, sizeof(OValue));
    if (v == NULL) {
        return NULL;
    }
    v->tag = tag;
    v->refcount = 1;
    return v;
}

static OValueMap *ovalue_map_create(void) {
    OValueMap *map = (OValueMap *)calloc(1U, sizeof(OValueMap));
    if (map == NULL) {
        return NULL;
    }
    map->bucket_count = 32U;
    map->buckets = (OMapEntry **)calloc(map->bucket_count, sizeof(OMapEntry *));
    if (map->buckets == NULL) {
        free(map);
        return NULL;
    }
    return map;
}

static unsigned long djb2_hash(const char *s) {
    unsigned long hash = 5381UL;
    unsigned char c;

    if (s == NULL) {
        return hash;
    }
    while ((c = (unsigned char)*s++) != 0U) {
        hash = ((hash << 5U) + hash) + (unsigned long)c;
    }
    return hash;
}

static void request_kind_free(RequestKind *kind) {
    if (kind == NULL) {
        return;
    }
    free(kind->lang);
    free(kind->profile);
    kind->lang = NULL;
    kind->profile = NULL;
}

static RequestKind request_kind_clone(const RequestKind *kind) {
    RequestKind out;
    memset(&out, 0, sizeof(out));
    if (kind == NULL) {
        return out;
    }
    out.tag = kind->tag;
    out.env_id = kind->env_id;
    out.cacheable = kind->cacheable;
    out.dry_run = kind->dry_run;
    out.lang = dup_cstr(kind->lang);
    out.profile = dup_cstr(kind->profile);
    return out;
}

static void ovalue_map_free(OValueMap *map) {
    size_t i;

    if (map == NULL) {
        return;
    }
    if (map->buckets != NULL) {
        for (i = 0U; i < map->bucket_count; ++i) {
            OMapEntry *entry = map->buckets[i];
            while (entry != NULL) {
                OMapEntry *next = entry->next;
                free(entry->key);
                oval_release(entry->value);
                free(entry);
                entry = next;
            }
        }
        free(map->buckets);
    }
    free(map);
}

static char *format_bool(bool value) {
    return dup_cstr(value ? "true" : "false");
}

static bool sb_reserve(StringBuilder *sb, size_t extra) {
    size_t needed;
    size_t new_cap;
    char *new_buf;

    if (sb == NULL) {
        return false;
    }
    needed = sb->len + extra + 1U;
    if (needed <= sb->cap) {
        return true;
    }
    new_cap = sb->cap == 0U ? 64U : sb->cap;
    while (new_cap < needed) {
        if (new_cap > (SIZE_MAX / 2U)) {
            new_cap = needed;
            break;
        }
        new_cap *= 2U;
    }
    new_buf = (char *)realloc(sb->buf, new_cap);
    if (new_buf == NULL) {
        return false;
    }
    sb->buf = new_buf;
    sb->cap = new_cap;
    return true;
}

static bool sb_append_mem(StringBuilder *sb, const char *data, size_t len) {
    if (sb == NULL || (data == NULL && len != 0U)) {
        return false;
    }
    if (!sb_reserve(sb, len)) {
        return false;
    }
    if (len != 0U) {
        memcpy(sb->buf + sb->len, data, len);
        sb->len += len;
    }
    sb->buf[sb->len] = '\0';
    return true;
}

static bool sb_append_str(StringBuilder *sb, const char *s) {
    if (s == NULL) {
        return sb_append_mem(sb, "", 0U);
    }
    return sb_append_mem(sb, s, strlen(s));
}

static bool sb_append_char(StringBuilder *sb, char c) {
    return sb_append_mem(sb, &c, 1U);
}

static bool sb_append_fmt(StringBuilder *sb, const char *fmt, ...) {
    va_list ap;
    va_list ap_copy;
    int needed;

    if (sb == NULL || fmt == NULL) {
        return false;
    }
    va_start(ap, fmt);
    va_copy(ap_copy, ap);
    needed = vsnprintf(NULL, 0, fmt, ap_copy);
    va_end(ap_copy);
    if (needed < 0) {
        va_end(ap);
        return false;
    }
    if (!sb_reserve(sb, (size_t)needed)) {
        va_end(ap);
        return false;
    }
    (void)vsnprintf(sb->buf + sb->len, sb->cap - sb->len, fmt, ap);
    va_end(ap);
    sb->len += (size_t)needed;
    return true;
}

static char *sb_take(StringBuilder *sb) {
    char *out;

    if (sb == NULL) {
        return NULL;
    }
    if (sb->buf == NULL) {
        out = dup_cstr("");
    } else {
        out = sb->buf;
    }
    sb->buf = NULL;
    sb->len = 0U;
    sb->cap = 0U;
    return out;
}

static void sb_free(StringBuilder *sb) {
    if (sb == NULL) {
        return;
    }
    free(sb->buf);
    sb->buf = NULL;
    sb->len = 0U;
    sb->cap = 0U;
}

static char *format_float_value(double value, bool force_decimal) {
    char tmp[64];
    StringBuilder sb = {0};

    (void)snprintf(tmp, sizeof(tmp), "%.17g", value);
    if (!sb_append_str(&sb, tmp)) {
        sb_free(&sb);
        return NULL;
    }
    if (force_decimal != false) {
        if (strchr(tmp, '.') == NULL && strchr(tmp, 'e') == NULL && strchr(tmp, 'E') == NULL) {
            if (!sb_append_str(&sb, ".0")) {
                sb_free(&sb);
                return NULL;
            }
        }
    }
    return sb_take(&sb);
}

static int cmp_cstr_ptrs(const void *a, const void *b) {
    const char *const *sa = (const char *const *)a;
    const char *const *sb = (const char *const *)b;
    return strcmp(*sa, *sb);
}

static int cmp_map_entry_refs(const void *a, const void *b) {
    const MapEntryRef *ea = (const MapEntryRef *)a;
    const MapEntryRef *eb = (const MapEntryRef *)b;
    return strcmp(ea->entry->key, eb->entry->key);
}

static MapEntryRef *map_collect_sorted(const OValueMap *map, size_t *out_len) {
    MapEntryRef *refs;
    size_t i;
    size_t idx = 0U;

    if (out_len != NULL) {
        *out_len = 0U;
    }
    if (map == NULL || map->len == 0U) {
        return NULL;
    }
    refs = (MapEntryRef *)calloc(map->len, sizeof(MapEntryRef));
    if (refs == NULL) {
        return NULL;
    }
    for (i = 0U; i < map->bucket_count; ++i) {
        const OMapEntry *entry = map->buckets[i];
        while (entry != NULL) {
            refs[idx].entry = entry;
            ++idx;
            entry = entry->next;
        }
    }
    qsort(refs, idx, sizeof(MapEntryRef), cmp_map_entry_refs);
    if (out_len != NULL) {
        *out_len = idx;
    }
    return refs;
}

static char *json_escape_string(const char *s) {
    StringBuilder sb = {0};
    const unsigned char *p = (const unsigned char *)(s != NULL ? s : "");

    if (!sb_append_char(&sb, '"')) {
        sb_free(&sb);
        return NULL;
    }
    while (*p != 0U) {
        switch (*p) {
            case '"':
                if (!sb_append_str(&sb, "\\\"")) { sb_free(&sb); return NULL; }
                break;
            case '\\':
                if (!sb_append_str(&sb, "\\\\")) { sb_free(&sb); return NULL; }
                break;
            case '\b':
                if (!sb_append_str(&sb, "\\b")) { sb_free(&sb); return NULL; }
                break;
            case '\f':
                if (!sb_append_str(&sb, "\\f")) { sb_free(&sb); return NULL; }
                break;
            case '\n':
                if (!sb_append_str(&sb, "\\n")) { sb_free(&sb); return NULL; }
                break;
            case '\r':
                if (!sb_append_str(&sb, "\\r")) { sb_free(&sb); return NULL; }
                break;
            case '\t':
                if (!sb_append_str(&sb, "\\t")) { sb_free(&sb); return NULL; }
                break;
            default:
                if (*p < 0x20U) {
                    if (!sb_append_fmt(&sb, "\\u%04x", (unsigned)*p)) {
                        sb_free(&sb);
                        return NULL;
                    }
                } else if (!sb_append_char(&sb, (char)*p)) {
                    sb_free(&sb);
                    return NULL;
                }
                break;
        }
        ++p;
    }
    if (!sb_append_char(&sb, '"')) {
        sb_free(&sb);
        return NULL;
    }
    return sb_take(&sb);
}

static bool json_emit_escaped(StringBuilder *sb, const char *s) {
    char *escaped = json_escape_string(s);
    bool ok = escaped != NULL && sb_append_str(sb, escaped);
    free(escaped);
    return ok;
}

static char *kind_tag_string(const RequestKind *kind) {
    StringBuilder sb = {0};
    char *out;

    if (kind == NULL) {
        return dup_cstr("");
    }
    switch (kind->tag) {
        case REQ_INSTANTIATE:
            return dup_cstr("instantiate");
        case REQ_REALISE:
            return dup_cstr("realise");
        case REQ_EVAL:
            if (!sb_append_fmt(&sb, "eval|%s|%u|%d",
                               kind->lang != NULL ? kind->lang : "",
                               (unsigned)kind->env_id,
                               kind->cacheable ? 1 : 0)) {
                sb_free(&sb);
                return NULL;
            }
            break;
        case REQ_ACTIVATE:
            if (!sb_append_fmt(&sb, "activate|%s|%d",
                               kind->profile != NULL ? kind->profile : "",
                               kind->dry_run ? 1 : 0)) {
                sb_free(&sb);
                return NULL;
            }
            break;
        default:
            return dup_cstr("");
    }
    out = sb_take(&sb);
    return out;
}

static char *compose_body_deps_fingerprint(const char *body, OValue **deps, size_t deps_len) {
    char **ids = NULL;
    StringBuilder sb = {0};
    char *hash = NULL;
    size_t i;

    if (deps_len != 0U) {
        ids = (char **)calloc(deps_len, sizeof(char *));
        if (ids == NULL) {
            return NULL;
        }
        for (i = 0U; i < deps_len; ++i) {
            ids[i] = oval_content_identity(deps[i]);
            if (ids[i] == NULL) {
                goto cleanup;
            }
        }
        qsort(ids, deps_len, sizeof(char *), cmp_cstr_ptrs);
    }

    if (!sb_append_str(&sb, body != NULL ? body : "") ||
        !sb_append_str(&sb, "||")) {
        goto cleanup;
    }
    for (i = 0U; i < deps_len; ++i) {
        if (i != 0U && !sb_append_char(&sb, '|')) {
            goto cleanup;
        }
        if (!sb_append_str(&sb, ids[i])) {
            goto cleanup;
        }
    }
    hash = sha256_hex(sb.buf != NULL ? sb.buf : "", sb.len);

cleanup:
    if (ids != NULL) {
        for (i = 0U; i < deps_len; ++i) {
            free(ids[i]);
        }
        free(ids);
    }
    sb_free(&sb);
    return hash;
}

static OValue *oval_string_like(OValueTag tag, const char *s) {
    OValue *v = oval_alloc(tag);
    if (v == NULL) {
        return NULL;
    }
    v->data.str_val = dup_cstr(s != NULL ? s : "");
    if (v->data.str_val == NULL) {
        free(v);
        return NULL;
    }
    return v;
}

static OValue *oval_list_take(OValue **items, size_t len) {
    OValue *v = oval_alloc(OVAL_LIST);
    if (v == NULL) {
        return NULL;
    }
    if (len != 0U) {
        v->data.list.items = items;
    }
    v->data.list.len = len;
    v->data.list.cap = len;
    return v;
}

static OValue *oval_map_take(OValueMap *map) {
    OValue *v = oval_alloc(OVAL_MAP);
    if (v == NULL) {
        ovalue_map_free(map);
        return NULL;
    }
    v->data.map = map;
    return v;
}

static OValue *oval_blob_take(const char *b64, const char *mime) {
    OValue *v = oval_alloc(OVAL_BLOB);
    if (v == NULL) {
        return NULL;
    }
    v->data.blob.data = dup_cstr(b64 != NULL ? b64 : "");
    v->data.blob.mime = dup_cstr(mime != NULL ? mime : "");
    if (v->data.blob.data == NULL || v->data.blob.mime == NULL) {
        free(v->data.blob.data);
        free(v->data.blob.mime);
        free(v);
        return NULL;
    }
    return v;
}

static OValue *oval_nixish_take(OValueTag tag, const char *body, OValue **deps, size_t deps_len, const char *fingerprint) {
    OValue *v = oval_alloc(tag);
    if (v == NULL) {
        return NULL;
    }
    v->data.nix_expr.body = dup_cstr(body != NULL ? body : "");
    v->data.nix_expr.deps = deps;
    v->data.nix_expr.deps_len = deps_len;
    v->data.nix_expr.fingerprint = dup_cstr(fingerprint != NULL ? fingerprint : "");
    if (v->data.nix_expr.body == NULL || v->data.nix_expr.fingerprint == NULL) {
        free(v->data.nix_expr.body);
        free(v->data.nix_expr.fingerprint);
        free(v->data.nix_expr.deps);
        free(v);
        return NULL;
    }
    return v;
}

static OValue *oval_derivation_take(char *drv_path, char **outputs, size_t outputs_len, OValue **deps, size_t deps_len) {
    OValue *v = oval_alloc(OVAL_DERIVATION);
    if (v == NULL) {
        size_t i;
        free(drv_path);
        if (outputs != NULL) {
            for (i = 0U; i < outputs_len; ++i) {
                free(outputs[i]);
            }
            free(outputs);
        }
        if (deps != NULL) {
            for (i = 0U; i < deps_len; ++i) {
                oval_release(deps[i]);
            }
            free(deps);
        }
        return NULL;
    }
    v->data.derivation.drv_path = drv_path;
    v->data.derivation.outputs = outputs;
    v->data.derivation.outputs_len = outputs_len;
    v->data.derivation.deps = deps;
    v->data.derivation.deps_len = deps_len;
    return v;
}

static OValue *oval_request_take(RequestKind kind, OValue *source, const char *fingerprint) {
    OValue *v = oval_alloc(OVAL_REQUEST);
    if (v == NULL) {
        request_kind_free(&kind);
        oval_release(source);
        return NULL;
    }
    v->data.request.kind = kind;
    v->data.request.source = source;
    v->data.request.fingerprint = dup_cstr(fingerprint != NULL ? fingerprint : "");
    if (v->data.request.fingerprint == NULL) {
        request_kind_free(&v->data.request.kind);
        oval_release(v->data.request.source);
        free(v);
        return NULL;
    }
    return v;
}

static bool map_insert_owned(OValueMap *map, char *key, OValue *value) {
    unsigned long hash;
    size_t idx;
    OMapEntry *entry;

    if (map == NULL || key == NULL) {
        free(key);
        oval_release(value);
        return false;
    }
    hash = djb2_hash(key);
    idx = (size_t)(hash % map->bucket_count);
    entry = map->buckets[idx];
    while (entry != NULL) {
        if (strcmp(entry->key, key) == 0) {
            free(key);
            oval_release(entry->value);
            entry->value = value;
            return true;
        }
        entry = entry->next;
    }
    entry = (OMapEntry *)calloc(1U, sizeof(OMapEntry));
    if (entry == NULL) {
        free(key);
        oval_release(value);
        return false;
    }
    entry->key = key;
    entry->value = value;
    entry->next = map->buckets[idx];
    map->buckets[idx] = entry;
    map->len += 1U;
    return true;
}

/* ────────────────────────────────────────────────────────────────────── */
/* SHA-256                                                               */
/* ────────────────────────────────────────────────────────────────────── */

typedef struct {
    uint32_t state[8];
    uint64_t bitlen;
    uint8_t data[64];
    size_t datalen;
} Sha256Ctx;

static uint32_t rotr32(uint32_t x, uint32_t n) {
    return (x >> n) | (x << (32U - n));
}

static const uint32_t sha256_k[64] = {
    0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U,
    0x3956c25bU, 0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U,
    0xd807aa98U, 0x12835b01U, 0x243185beU, 0x550c7dc3U,
    0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U, 0xc19bf174U,
    0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
    0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU,
    0x983e5152U, 0xa831c66dU, 0xb00327c8U, 0xbf597fc7U,
    0xc6e00bf3U, 0xd5a79147U, 0x06ca6351U, 0x14292967U,
    0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU, 0x53380d13U,
    0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
    0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U,
    0xd192e819U, 0xd6990624U, 0xf40e3585U, 0x106aa070U,
    0x19a4c116U, 0x1e376c08U, 0x2748774cU, 0x34b0bcb5U,
    0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU, 0x682e6ff3U,
    0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
    0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U
};

static void sha256_transform(Sha256Ctx *ctx, const uint8_t data[64]) {
    uint32_t m[64];
    uint32_t a;
    uint32_t b;
    uint32_t c;
    uint32_t d;
    uint32_t e;
    uint32_t f;
    uint32_t g;
    uint32_t h;
    size_t i;

    for (i = 0U; i < 16U; ++i) {
        m[i] = ((uint32_t)data[i * 4U] << 24U) |
               ((uint32_t)data[i * 4U + 1U] << 16U) |
               ((uint32_t)data[i * 4U + 2U] << 8U) |
               ((uint32_t)data[i * 4U + 3U]);
    }
    for (i = 16U; i < 64U; ++i) {
        uint32_t s0 = rotr32(m[i - 15U], 7U) ^ rotr32(m[i - 15U], 18U) ^ (m[i - 15U] >> 3U);
        uint32_t s1 = rotr32(m[i - 2U], 17U) ^ rotr32(m[i - 2U], 19U) ^ (m[i - 2U] >> 10U);
        m[i] = m[i - 16U] + s0 + m[i - 7U] + s1;
    }

    a = ctx->state[0];
    b = ctx->state[1];
    c = ctx->state[2];
    d = ctx->state[3];
    e = ctx->state[4];
    f = ctx->state[5];
    g = ctx->state[6];
    h = ctx->state[7];

    for (i = 0U; i < 64U; ++i) {
        uint32_t s1 = rotr32(e, 6U) ^ rotr32(e, 11U) ^ rotr32(e, 25U);
        uint32_t ch = (e & f) ^ ((~e) & g);
        uint32_t temp1 = h + s1 + ch + sha256_k[i] + m[i];
        uint32_t s0 = rotr32(a, 2U) ^ rotr32(a, 13U) ^ rotr32(a, 22U);
        uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        uint32_t temp2 = s0 + maj;

        h = g;
        g = f;
        f = e;
        e = d + temp1;
        d = c;
        c = b;
        b = a;
        a = temp1 + temp2;
    }

    ctx->state[0] += a;
    ctx->state[1] += b;
    ctx->state[2] += c;
    ctx->state[3] += d;
    ctx->state[4] += e;
    ctx->state[5] += f;
    ctx->state[6] += g;
    ctx->state[7] += h;
}

static void sha256_init(Sha256Ctx *ctx) {
    ctx->datalen = 0U;
    ctx->bitlen = 0U;
    ctx->state[0] = 0x6a09e667U;
    ctx->state[1] = 0xbb67ae85U;
    ctx->state[2] = 0x3c6ef372U;
    ctx->state[3] = 0xa54ff53aU;
    ctx->state[4] = 0x510e527fU;
    ctx->state[5] = 0x9b05688cU;
    ctx->state[6] = 0x1f83d9abU;
    ctx->state[7] = 0x5be0cd19U;
}

static void sha256_update(Sha256Ctx *ctx, const uint8_t *data, size_t len) {
    size_t i;

    for (i = 0U; i < len; ++i) {
        ctx->data[ctx->datalen] = data[i];
        ctx->datalen += 1U;
        if (ctx->datalen == 64U) {
            sha256_transform(ctx, ctx->data);
            ctx->bitlen += 512U;
            ctx->datalen = 0U;
        }
    }
}

static void sha256_final(Sha256Ctx *ctx, uint8_t hash[32]) {
    size_t i;

    i = ctx->datalen;
    ctx->data[i++] = 0x80U;
    if (i > 56U) {
        while (i < 64U) {
            ctx->data[i++] = 0U;
        }
        sha256_transform(ctx, ctx->data);
        i = 0U;
    }
    while (i < 56U) {
        ctx->data[i++] = 0U;
    }
    ctx->bitlen += (uint64_t)ctx->datalen * 8U;
    ctx->data[63] = (uint8_t)(ctx->bitlen);
    ctx->data[62] = (uint8_t)(ctx->bitlen >> 8U);
    ctx->data[61] = (uint8_t)(ctx->bitlen >> 16U);
    ctx->data[60] = (uint8_t)(ctx->bitlen >> 24U);
    ctx->data[59] = (uint8_t)(ctx->bitlen >> 32U);
    ctx->data[58] = (uint8_t)(ctx->bitlen >> 40U);
    ctx->data[57] = (uint8_t)(ctx->bitlen >> 48U);
    ctx->data[56] = (uint8_t)(ctx->bitlen >> 56U);
    sha256_transform(ctx, ctx->data);

    for (i = 0U; i < 4U; ++i) {
        hash[i]      = (uint8_t)((ctx->state[0] >> (24U - i * 8U)) & 0xffU);
        hash[i + 4U] = (uint8_t)((ctx->state[1] >> (24U - i * 8U)) & 0xffU);
        hash[i + 8U] = (uint8_t)((ctx->state[2] >> (24U - i * 8U)) & 0xffU);
        hash[i + 12U] = (uint8_t)((ctx->state[3] >> (24U - i * 8U)) & 0xffU);
        hash[i + 16U] = (uint8_t)((ctx->state[4] >> (24U - i * 8U)) & 0xffU);
        hash[i + 20U] = (uint8_t)((ctx->state[5] >> (24U - i * 8U)) & 0xffU);
        hash[i + 24U] = (uint8_t)((ctx->state[6] >> (24U - i * 8U)) & 0xffU);
        hash[i + 28U] = (uint8_t)((ctx->state[7] >> (24U - i * 8U)) & 0xffU);
    }
}

char *sha256_hex(const char *data, size_t len) {
    Sha256Ctx ctx;
    uint8_t digest[32];
    char *hex;
    static const char digits[] = "0123456789abcdef";
    size_t i;

    hex = (char *)malloc(65U);
    if (hex == NULL) {
        return NULL;
    }
    sha256_init(&ctx);
    sha256_update(&ctx, (const uint8_t *)(data != NULL ? data : ""), len);
    sha256_final(&ctx, digest);
    for (i = 0U; i < 32U; ++i) {
        hex[i * 2U] = digits[(digest[i] >> 4U) & 0x0fU];
        hex[i * 2U + 1U] = digits[digest[i] & 0x0fU];
    }
    hex[64] = '\0';
    return hex;
}

/* ────────────────────────────────────────────────────────────────────── */
/* Base64                                                                */
/* ────────────────────────────────────────────────────────────────────── */

char *base64_encode(const unsigned char *data, size_t len) {
    static const char table[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    char *out;
    size_t out_len;
    size_t i;
    size_t j = 0U;

    out_len = ((len + 2U) / 3U) * 4U;
    out = (char *)malloc(out_len + 1U);
    if (out == NULL) {
        return NULL;
    }
    for (i = 0U; i < len; i += 3U) {
        uint32_t octet_a = data != NULL && i < len ? data[i] : 0U;
        uint32_t octet_b = data != NULL && (i + 1U) < len ? data[i + 1U] : 0U;
        uint32_t octet_c = data != NULL && (i + 2U) < len ? data[i + 2U] : 0U;
        uint32_t triple = (octet_a << 16U) | (octet_b << 8U) | octet_c;

        out[j++] = table[(triple >> 18U) & 0x3fU];
        out[j++] = table[(triple >> 12U) & 0x3fU];
        out[j++] = ((i + 1U) < len) ? table[(triple >> 6U) & 0x3fU] : '=';
        out[j++] = ((i + 2U) < len) ? table[triple & 0x3fU] : '=';
    }
    out[j] = '\0';
    return out;
}

static int base64_index(char c) {
    if (c >= 'A' && c <= 'Z') return c - 'A';
    if (c >= 'a' && c <= 'z') return c - 'a' + 26;
    if (c >= '0' && c <= '9') return c - '0' + 52;
    if (c == '+') return 62;
    if (c == '/') return 63;
    return -1;
}

unsigned char *base64_decode(const char *b64, size_t *out_len) {
    size_t len;
    unsigned char *out;
    size_t i;
    size_t j = 0U;
    int vals[4];
    size_t out_cap;

    if (out_len != NULL) {
        *out_len = 0U;
    }
    if (b64 == NULL) {
        return NULL;
    }
    len = strlen(b64);
    if ((len % 4U) != 0U) {
        return NULL;
    }
    out_cap = (len / 4U) * 3U;
    out = (unsigned char *)malloc(out_cap == 0U ? 1U : out_cap);
    if (out == NULL) {
        return NULL;
    }
    for (i = 0U; i < len; i += 4U) {
        size_t k;
        for (k = 0U; k < 4U; ++k) {
            if (b64[i + k] == '=') {
                vals[k] = -2;
            } else {
                vals[k] = base64_index(b64[i + k]);
                if (vals[k] < 0) {
                    free(out);
                    return NULL;
                }
            }
        }
        if (vals[0] < 0 || vals[1] < 0) {
            free(out);
            return NULL;
        }
        out[j++] = (unsigned char)((vals[0] << 2) | (vals[1] >> 4));
        if (vals[2] == -2) {
            if (vals[3] != -2) {
                free(out);
                return NULL;
            }
            break;
        }
        if (vals[2] < 0) {
            free(out);
            return NULL;
        }
        out[j++] = (unsigned char)(((vals[1] & 0x0f) << 4) | (vals[2] >> 2));
        if (vals[3] == -2) {
            break;
        }
        if (vals[3] < 0) {
            free(out);
            return NULL;
        }
        out[j++] = (unsigned char)(((vals[2] & 0x03) << 6) | vals[3]);
    }
    if (out_len != NULL) {
        *out_len = j;
    }
    return out;
}

/* ────────────────────────────────────────────────────────────────────── */
/* Constructors                                                          */
/* ────────────────────────────────────────────────────────────────────── */

OValue *oval_null(void) {
    return oval_alloc(OVAL_NULL);
}

OValue *oval_bool(bool v) {
    OValue *out = oval_alloc(OVAL_BOOL);
    if (out != NULL) {
        out->data.bool_val = v;
    }
    return out;
}

OValue *oval_int(int64_t v) {
    OValue *out = oval_alloc(OVAL_INT);
    if (out != NULL) {
        out->data.int_val = v;
    }
    return out;
}

OValue *oval_float(double v) {
    OValue *out = oval_alloc(OVAL_FLOAT);
    if (out != NULL) {
        out->data.float_val = v;
    }
    return out;
}

OValue *oval_str(const char *s) {
    return oval_string_like(OVAL_STR, s);
}

OValue *oval_html(const char *s) {
    return oval_string_like(OVAL_HTML, s);
}

OValue *oval_store_path(const char *path) {
    return oval_string_like(OVAL_STORE_PATH, path);
}

OValue *oval_expr(const char *src) {
    return oval_string_like(OVAL_EXPR, src);
}

OValue *oval_list(OValue **items, size_t len) {
    OValue *v = oval_alloc(OVAL_LIST);
    size_t i;

    if (v == NULL) {
        return NULL;
    }
    if (len != 0U) {
        v->data.list.items = (OValue **)calloc(len, sizeof(OValue *));
        if (v->data.list.items == NULL) {
            free(v);
            return NULL;
        }
        for (i = 0U; i < len; ++i) {
            v->data.list.items[i] = oval_retain(items[i]);
        }
    }
    v->data.list.len = len;
    v->data.list.cap = len;
    return v;
}

OValue *oval_map(void) {
    return oval_map_take(ovalue_map_create());
}

OValue *oval_blob(const unsigned char *data, size_t len, const char *mime) {
    char *b64 = base64_encode(data, len);
    OValue *out;
    if (b64 == NULL) {
        return NULL;
    }
    out = oval_blob_take(b64, mime);
    free(b64);
    return out;
}

OValue *oval_nix_expr(const char *body, OValue **deps, size_t deps_len) {
    OValue **owned = NULL;
    char *fingerprint;
    size_t i;

    if (deps_len != 0U) {
        owned = (OValue **)calloc(deps_len, sizeof(OValue *));
        if (owned == NULL) {
            return NULL;
        }
        for (i = 0U; i < deps_len; ++i) {
            owned[i] = oval_retain(deps[i]);
        }
    }
    fingerprint = compose_body_deps_fingerprint(body != NULL ? body : "", owned, deps_len);
    if (fingerprint == NULL) {
        if (owned != NULL) {
            for (i = 0U; i < deps_len; ++i) {
                oval_release(owned[i]);
            }
            free(owned);
        }
        return NULL;
    }
    {
        OValue *out = oval_nixish_take(OVAL_NIX_EXPR, body, owned, deps_len, fingerprint);
        free(fingerprint);
        return out;
    }
}

OValue *oval_derivation(const char *drv_path, const char **outputs, size_t outputs_len, OValue **deps, size_t deps_len) {
    char *drv_copy = dup_cstr(drv_path != NULL ? drv_path : "");
    char **out_names = NULL;
    OValue **owned_deps = NULL;
    size_t i;

    if (drv_copy == NULL) {
        return NULL;
    }
    if (outputs_len != 0U) {
        out_names = (char **)calloc(outputs_len, sizeof(char *));
        if (out_names == NULL) {
            free(drv_copy);
            return NULL;
        }
        for (i = 0U; i < outputs_len; ++i) {
            out_names[i] = dup_cstr(outputs[i] != NULL ? outputs[i] : "");
            if (out_names[i] == NULL) {
                size_t j;
                for (j = 0U; j < i; ++j) {
                    free(out_names[j]);
                }
                free(out_names);
                free(drv_copy);
                return NULL;
            }
        }
    }
    if (deps_len != 0U) {
        owned_deps = (OValue **)calloc(deps_len, sizeof(OValue *));
        if (owned_deps == NULL) {
            for (i = 0U; i < outputs_len; ++i) {
                free(out_names[i]);
            }
            free(out_names);
            free(drv_copy);
            return NULL;
        }
        for (i = 0U; i < deps_len; ++i) {
            owned_deps[i] = oval_retain(deps[i]);
        }
    }
    return oval_derivation_take(drv_copy, out_names, outputs_len, owned_deps, deps_len);
}

OValue *oval_request(RequestKind kind, OValue *source) {
    char *kind_tag = NULL;
    char *source_id = NULL;
    StringBuilder sb = {0};
    char *fingerprint = NULL;
    RequestKind kind_copy;
    OValue *source_copy;
    OValue *out;

    memset(&kind_copy, 0, sizeof(kind_copy));
    source_copy = oval_retain(source);
    if (source_copy == NULL) {
        return NULL;
    }
    kind_copy = request_kind_clone(&kind);
    kind_tag = kind_tag_string(&kind_copy);
    source_id = oval_content_identity(source_copy);
    if (kind_tag == NULL || source_id == NULL) {
        request_kind_free(&kind_copy);
        oval_release(source_copy);
        free(kind_tag);
        free(source_id);
        return NULL;
    }
    if (!sb_append_str(&sb, kind_tag) || !sb_append_str(&sb, "||") || !sb_append_str(&sb, source_id)) {
        request_kind_free(&kind_copy);
        oval_release(source_copy);
        free(kind_tag);
        free(source_id);
        sb_free(&sb);
        return NULL;
    }
    fingerprint = sha256_hex(sb.buf != NULL ? sb.buf : "", sb.len);
    sb_free(&sb);
    free(kind_tag);
    free(source_id);
    if (fingerprint == NULL) {
        request_kind_free(&kind_copy);
        oval_release(source_copy);
        return NULL;
    }
    out = oval_request_take(kind_copy, source_copy, fingerprint);
    free(fingerprint);
    return out;
}

OValue *oval_thunk(const char *body, OValue **deps, size_t deps_len) {
    OValue **owned = NULL;
    char *fingerprint;
    size_t i;

    if (deps_len != 0U) {
        owned = (OValue **)calloc(deps_len, sizeof(OValue *));
        if (owned == NULL) {
            return NULL;
        }
        for (i = 0U; i < deps_len; ++i) {
            owned[i] = oval_retain(deps[i]);
        }
    }
    fingerprint = compose_body_deps_fingerprint(body != NULL ? body : "", owned, deps_len);
    if (fingerprint == NULL) {
        if (owned != NULL) {
            for (i = 0U; i < deps_len; ++i) {
                oval_release(owned[i]);
            }
            free(owned);
        }
        return NULL;
    }
    {
        OValue *out = oval_nixish_take(OVAL_THUNK, body, owned, deps_len, fingerprint);
        free(fingerprint);
        return out;
    }
}

OValue *oval_system(const char *profile_path) {
    return oval_string_like(OVAL_SYSTEM, profile_path);
}

/* ────────────────────────────────────────────────────────────────────── */
/* Reference counting                                                    */
/* ────────────────────────────────────────────────────────────────────── */

OValue *oval_retain(OValue *v) {
    if (v != NULL) {
        v->refcount += 1;
    }
    return v;
}

void oval_release(OValue *v) {
    size_t i;

    if (v == NULL) {
        return;
    }
    v->refcount -= 1;
    if (v->refcount > 0) {
        return;
    }
    switch (v->tag) {
        case OVAL_STR:
        case OVAL_HTML:
        case OVAL_STORE_PATH:
        case OVAL_EXPR:
        case OVAL_SYSTEM:
            free(v->data.str_val);
            break;
        case OVAL_LIST:
            for (i = 0U; i < v->data.list.len; ++i) {
                oval_release(v->data.list.items[i]);
            }
            free(v->data.list.items);
            break;
        case OVAL_MAP:
            ovalue_map_free(v->data.map);
            break;
        case OVAL_BLOB:
            free(v->data.blob.data);
            free(v->data.blob.mime);
            break;
        case OVAL_NIX_EXPR:
        case OVAL_THUNK:
            free(v->data.nix_expr.body);
            for (i = 0U; i < v->data.nix_expr.deps_len; ++i) {
                oval_release(v->data.nix_expr.deps[i]);
            }
            free(v->data.nix_expr.deps);
            free(v->data.nix_expr.fingerprint);
            break;
        case OVAL_DERIVATION:
            free(v->data.derivation.drv_path);
            for (i = 0U; i < v->data.derivation.outputs_len; ++i) {
                free(v->data.derivation.outputs[i]);
            }
            free(v->data.derivation.outputs);
            for (i = 0U; i < v->data.derivation.deps_len; ++i) {
                oval_release(v->data.derivation.deps[i]);
            }
            free(v->data.derivation.deps);
            break;
        case OVAL_REQUEST:
            request_kind_free(&v->data.request.kind);
            oval_release(v->data.request.source);
            free(v->data.request.fingerprint);
            break;
        case OVAL_NULL:
        case OVAL_BOOL:
        case OVAL_INT:
        case OVAL_FLOAT:
            break;
        default:
            break;
    }
    free(v);
}

/* ────────────────────────────────────────────────────────────────────── */
/* Predicates and accessors                                              */
/* ────────────────────────────────────────────────────────────────────── */

bool oval_is_null(const OValue *v) {
    return v != NULL && v->tag == OVAL_NULL;
}

bool oval_is_request(const OValue *v) {
    return v != NULL && v->tag == OVAL_REQUEST;
}

bool oval_is_nix_expr(const OValue *v) {
    return v != NULL && v->tag == OVAL_NIX_EXPR;
}

bool oval_is_derivation(const OValue *v) {
    return v != NULL && v->tag == OVAL_DERIVATION;
}

bool oval_is_thunk(const OValue *v) {
    return v != NULL && v->tag == OVAL_THUNK;
}

bool oval_is_system(const OValue *v) {
    return v != NULL && v->tag == OVAL_SYSTEM;
}

const char *oval_type_name(const OValue *v) {
    if (v == NULL) {
        return "invalid";
    }
    switch (v->tag) {
        case OVAL_NULL: return "null";
        case OVAL_BOOL: return "bool";
        case OVAL_INT: return "int";
        case OVAL_FLOAT: return "float";
        case OVAL_STR: return "str";
        case OVAL_HTML: return "html";
        case OVAL_STORE_PATH: return "store_path";
        case OVAL_EXPR: return "expr";
        case OVAL_LIST: return "list";
        case OVAL_MAP: return "map";
        case OVAL_BLOB: return "blob";
        case OVAL_NIX_EXPR: return "nix_expr";
        case OVAL_DERIVATION: return "derivation";
        case OVAL_REQUEST: return "request";
        case OVAL_THUNK: return "thunk";
        case OVAL_SYSTEM: return "system";
        default: return "unknown";
    }
}

bool oval_as_bool(const OValue *v, bool *out) {
    if (v == NULL || v->tag != OVAL_BOOL || out == NULL) {
        return false;
    }
    *out = v->data.bool_val;
    return true;
}

bool oval_as_int(const OValue *v, int64_t *out) {
    if (v == NULL || v->tag != OVAL_INT || out == NULL) {
        return false;
    }
    *out = v->data.int_val;
    return true;
}

bool oval_as_float(const OValue *v, double *out) {
    if (v == NULL || out == NULL) {
        return false;
    }
    if (v->tag == OVAL_FLOAT) {
        *out = v->data.float_val;
        return true;
    }
    if (v->tag == OVAL_INT) {
        *out = (double)v->data.int_val;
        return true;
    }
    return false;
}

const char *oval_as_str(const OValue *v) {
    if (v == NULL) {
        return NULL;
    }
    switch (v->tag) {
        case OVAL_STR:
        case OVAL_HTML:
        case OVAL_STORE_PATH:
        case OVAL_EXPR:
        case OVAL_SYSTEM:
            return v->data.str_val;
        default:
            return NULL;
    }
}

/* ────────────────────────────────────────────────────────────────────── */
/* Map operations                                                        */
/* ────────────────────────────────────────────────────────────────────── */

void oval_map_set(OValue *map, const char *key, OValue *value) {
    char *key_copy;
    OValue *value_copy;

    if (map == NULL || map->tag != OVAL_MAP || map->data.map == NULL || key == NULL) {
        return;
    }
    key_copy = dup_cstr(key);
    value_copy = oval_retain(value);
    if (key_copy == NULL) {
        oval_release(value_copy);
        return;
    }
    (void)map_insert_owned(map->data.map, key_copy, value_copy);
}

OValue *oval_map_get(const OValue *map, const char *key) {
    unsigned long hash;
    size_t idx;
    OMapEntry *entry;

    if (map == NULL || map->tag != OVAL_MAP || map->data.map == NULL || key == NULL) {
        return NULL;
    }
    hash = djb2_hash(key);
    idx = (size_t)(hash % map->data.map->bucket_count);
    entry = map->data.map->buckets[idx];
    while (entry != NULL) {
        if (strcmp(entry->key, key) == 0) {
            return entry->value;
        }
        entry = entry->next;
    }
    return NULL;
}

size_t oval_map_len(const OValue *map) {
    if (map == NULL || map->tag != OVAL_MAP || map->data.map == NULL) {
        return 0U;
    }
    return map->data.map->len;
}

/* ────────────────────────────────────────────────────────────────────── */
/* Splice representation and identity                                    */
/* ────────────────────────────────────────────────────────────────────── */

char *oval_splice_repr(const OValue *v) {
    StringBuilder sb = {0};
    size_t i;

    if (v == NULL) {
        return NULL;
    }
    switch (v->tag) {
        case OVAL_NULL:
            return dup_cstr("null");
        case OVAL_BOOL:
            return format_bool(v->data.bool_val);
        case OVAL_INT:
            if (!sb_append_fmt(&sb, "%lld", (long long)v->data.int_val)) {
                sb_free(&sb);
                return NULL;
            }
            return sb_take(&sb);
        case OVAL_FLOAT:
            return format_float_value(v->data.float_val, true);
        case OVAL_STR:
        case OVAL_HTML:
        case OVAL_STORE_PATH:
        case OVAL_EXPR:
        case OVAL_SYSTEM:
            return dup_cstr(v->data.str_val != NULL ? v->data.str_val : "");
        case OVAL_LIST:
            if (!sb_append_char(&sb, '[')) {
                sb_free(&sb);
                return NULL;
            }
            for (i = 0U; i < v->data.list.len; ++i) {
                char *item = oval_splice_repr(v->data.list.items[i]);
                if (item == NULL) {
                    sb_free(&sb);
                    return NULL;
                }
                if (i != 0U && !sb_append_str(&sb, ", ")) {
                    free(item);
                    sb_free(&sb);
                    return NULL;
                }
                if (!sb_append_str(&sb, item)) {
                    free(item);
                    sb_free(&sb);
                    return NULL;
                }
                free(item);
            }
            if (!sb_append_char(&sb, ']')) {
                sb_free(&sb);
                return NULL;
            }
            return sb_take(&sb);
        case OVAL_MAP:
            if (!sb_append_char(&sb, '{')) {
                sb_free(&sb);
                return NULL;
            }
            if (v->data.map != NULL && v->data.map->len != 0U) {
                size_t count = 0U;
                MapEntryRef *refs = map_collect_sorted(v->data.map, &count);
                for (i = 0U; refs != NULL && i < count; ++i) {
                    char *key = json_escape_string(refs[i].entry->key);
                    char *val = oval_splice_repr(refs[i].entry->value);
                    if (key == NULL || val == NULL) {
                        free(key);
                        free(val);
                        free(refs);
                        sb_free(&sb);
                        return NULL;
                    }
                    if (i != 0U && !sb_append_str(&sb, ", ")) {
                        free(key);
                        free(val);
                        free(refs);
                        sb_free(&sb);
                        return NULL;
                    }
                    if (!sb_append_str(&sb, key) || !sb_append_str(&sb, ": ") || !sb_append_str(&sb, val)) {
                        free(key);
                        free(val);
                        free(refs);
                        sb_free(&sb);
                        return NULL;
                    }
                    free(key);
                    free(val);
                }
                free(refs);
            }
            if (!sb_append_char(&sb, '}')) {
                sb_free(&sb);
                return NULL;
            }
            return sb_take(&sb);
        case OVAL_BLOB:
            if (!sb_append_fmt(&sb, "data:%s;base64,%s",
                               v->data.blob.mime != NULL ? v->data.blob.mime : "",
                               v->data.blob.data != NULL ? v->data.blob.data : "")) {
                sb_free(&sb);
                return NULL;
            }
            return sb_take(&sb);
        case OVAL_NIX_EXPR:
        case OVAL_THUNK:
            return dup_cstr(v->data.nix_expr.body != NULL ? v->data.nix_expr.body : "");
        case OVAL_DERIVATION:
            return dup_cstr(v->data.derivation.drv_path != NULL ? v->data.derivation.drv_path : "");
        case OVAL_REQUEST: {
            char *tag = kind_tag_string(&v->data.request.kind);
            const char *fp = v->data.request.fingerprint != NULL ? v->data.request.fingerprint : "";
            size_t fp_len = strlen(fp);
            if (tag == NULL) {
                return NULL;
            }
            if (!sb_append_fmt(&sb, "<request:%s fp=%.*s>", tag, (int)(fp_len < 8U ? fp_len : 8U), fp)) {
                free(tag);
                sb_free(&sb);
                return NULL;
            }
            free(tag);
            return sb_take(&sb);
        }
        default:
            return NULL;
    }
}

char *oval_content_identity(const OValue *v) {
    char *repr;
    char *hash;
    const char *s;

    if (v == NULL) {
        return NULL;
    }
    switch (v->tag) {
        case OVAL_NIX_EXPR:
        case OVAL_THUNK:
            return dup_cstr(v->data.nix_expr.fingerprint != NULL ? v->data.nix_expr.fingerprint : "");
        case OVAL_DERIVATION:
            s = v->data.derivation.drv_path != NULL ? v->data.derivation.drv_path : "";
            return sha256_hex(s, strlen(s));
        case OVAL_STORE_PATH:
        case OVAL_SYSTEM:
            s = v->data.str_val != NULL ? v->data.str_val : "";
            return sha256_hex(s, strlen(s));
        case OVAL_REQUEST:
            return dup_cstr(v->data.request.fingerprint != NULL ? v->data.request.fingerprint : "");
        default:
            repr = oval_splice_repr(v);
            if (repr == NULL) {
                return NULL;
            }
            hash = sha256_hex(repr, strlen(repr));
            free(repr);
            return hash;
    }
}

/* ────────────────────────────────────────────────────────────────────── */
/* JSON serialization                                                    */
/* ────────────────────────────────────────────────────────────────────── */

static bool json_emit_value(StringBuilder *sb, const OValue *v);

static bool json_emit_map_payload(StringBuilder *sb, const OValueMap *map) {
    size_t i;
    size_t count = 0U;
    MapEntryRef *refs = NULL;

    if (!sb_append_char(sb, '{')) {
        return false;
    }
    if (map != NULL && map->len != 0U) {
        refs = map_collect_sorted(map, &count);
        for (i = 0U; refs != NULL && i < count; ++i) {
            if (i != 0U && !sb_append_char(sb, ',')) {
                free(refs);
                return false;
            }
            if (!json_emit_escaped(sb, refs[i].entry->key) || !sb_append_char(sb, ':') ||
                !json_emit_value(sb, refs[i].entry->value)) {
                free(refs);
                return false;
            }
        }
        free(refs);
    }
    return sb_append_char(sb, '}');
}

static bool json_emit_request_kind(StringBuilder *sb, const RequestKind *kind) {
    if (sb == NULL || kind == NULL) {
        return false;
    }
    switch (kind->tag) {
        case REQ_INSTANTIATE:
            return json_emit_escaped(sb, "instantiate");
        case REQ_REALISE:
            return json_emit_escaped(sb, "realise");
        case REQ_EVAL:
            return sb_append_str(sb, "{\"eval\":{") &&
                   json_emit_escaped(sb, "lang") && sb_append_char(sb, ':') && json_emit_escaped(sb, kind->lang != NULL ? kind->lang : "") &&
                   sb_append_char(sb, ',') && json_emit_escaped(sb, "env_id") && sb_append_char(sb, ':') && sb_append_fmt(sb, "%u", (unsigned)kind->env_id) &&
                   sb_append_char(sb, ',') && json_emit_escaped(sb, "cacheable") && sb_append_char(sb, ':') && sb_append_str(sb, kind->cacheable ? "true" : "false") &&
                   sb_append_str(sb, "}}");
        case REQ_ACTIVATE:
            return sb_append_str(sb, "{\"activate\":{") &&
                   json_emit_escaped(sb, "profile") && sb_append_char(sb, ':') && json_emit_escaped(sb, kind->profile != NULL ? kind->profile : "") &&
                   sb_append_char(sb, ',') && json_emit_escaped(sb, "dry_run") && sb_append_char(sb, ':') && sb_append_str(sb, kind->dry_run ? "true" : "false") &&
                   sb_append_str(sb, "}}");
        default:
            return false;
    }
}

static bool json_emit_value(StringBuilder *sb, const OValue *v) {
    size_t i;

    if (sb == NULL || v == NULL) {
        return false;
    }
    switch (v->tag) {
        case OVAL_NULL:
            return sb_append_str(sb, "{\"t\":\"null\"}");
        case OVAL_BOOL:
            return sb_append_fmt(sb, "{\"t\":\"bool\",\"v\":%s}", v->data.bool_val ? "true" : "false");
        case OVAL_INT:
            return sb_append_fmt(sb, "{\"t\":\"int\",\"v\":%lld}", (long long)v->data.int_val);
        case OVAL_FLOAT: {
            char *num = format_float_value(v->data.float_val, true);
            bool ok = num != NULL && sb_append_str(sb, "{\"t\":\"float\",\"v\":") && sb_append_str(sb, num) && sb_append_char(sb, '}');
            free(num);
            return ok;
        }
        case OVAL_STR:
            return sb_append_str(sb, "{\"t\":\"str\",\"v\":") && json_emit_escaped(sb, v->data.str_val) && sb_append_char(sb, '}');
        case OVAL_HTML:
            return sb_append_str(sb, "{\"t\":\"html\",\"v\":") && json_emit_escaped(sb, v->data.str_val) && sb_append_char(sb, '}');
        case OVAL_STORE_PATH:
            return sb_append_str(sb, "{\"t\":\"store_path\",\"path\":") && json_emit_escaped(sb, v->data.str_val) && sb_append_char(sb, '}');
        case OVAL_EXPR:
            return sb_append_str(sb, "{\"t\":\"expr\",\"src\":") && json_emit_escaped(sb, v->data.str_val) && sb_append_char(sb, '}');
        case OVAL_LIST:
            if (!sb_append_str(sb, "{\"t\":\"list\",\"v\":[")) {
                return false;
            }
            for (i = 0U; i < v->data.list.len; ++i) {
                if (i != 0U && !sb_append_char(sb, ',')) {
                    return false;
                }
                if (!json_emit_value(sb, v->data.list.items[i])) {
                    return false;
                }
            }
            return sb_append_str(sb, "]}");
        case OVAL_MAP:
            return sb_append_str(sb, "{\"t\":\"map\",\"v\":") && json_emit_map_payload(sb, v->data.map) && sb_append_char(sb, '}');
        case OVAL_BLOB:
            return sb_append_str(sb, "{\"t\":\"blob\",\"v\":") && json_emit_escaped(sb, v->data.blob.data) &&
                   sb_append_str(sb, ",\"mime\":") && json_emit_escaped(sb, v->data.blob.mime) && sb_append_char(sb, '}');
        case OVAL_NIX_EXPR:
            if (!sb_append_str(sb, "{\"t\":\"nix_expr\",\"body\":" ) ||
                !json_emit_escaped(sb, v->data.nix_expr.body) ||
                !sb_append_str(sb, ",\"deps\":[")) {
                return false;
            }
            for (i = 0U; i < v->data.nix_expr.deps_len; ++i) {
                if (i != 0U && !sb_append_char(sb, ',')) {
                    return false;
                }
                if (!json_emit_value(sb, v->data.nix_expr.deps[i])) {
                    return false;
                }
            }
            return sb_append_str(sb, "],\"fingerprint\":") && json_emit_escaped(sb, v->data.nix_expr.fingerprint) && sb_append_char(sb, '}');
        case OVAL_DERIVATION:
            if (!sb_append_str(sb, "{\"t\":\"derivation\",\"drv_path\":") ||
                !json_emit_escaped(sb, v->data.derivation.drv_path) ||
                !sb_append_str(sb, ",\"outputs\":[")) {
                return false;
            }
            for (i = 0U; i < v->data.derivation.outputs_len; ++i) {
                if (i != 0U && !sb_append_char(sb, ',')) {
                    return false;
                }
                if (!json_emit_escaped(sb, v->data.derivation.outputs[i])) {
                    return false;
                }
            }
            if (!sb_append_str(sb, "],\"deps\":[")) {
                return false;
            }
            for (i = 0U; i < v->data.derivation.deps_len; ++i) {
                if (i != 0U && !sb_append_char(sb, ',')) {
                    return false;
                }
                if (!json_emit_value(sb, v->data.derivation.deps[i])) {
                    return false;
                }
            }
            return sb_append_str(sb, "]}");
        case OVAL_REQUEST:
            return sb_append_str(sb, "{\"t\":\"request\",\"kind\":") && json_emit_request_kind(sb, &v->data.request.kind) &&
                   sb_append_str(sb, ",\"source\":") && json_emit_value(sb, v->data.request.source) &&
                   sb_append_str(sb, ",\"fingerprint\":") && json_emit_escaped(sb, v->data.request.fingerprint) &&
                   sb_append_char(sb, '}');
        case OVAL_THUNK:
            if (!sb_append_str(sb, "{\"t\":\"thunk\",\"body\":" ) ||
                !json_emit_escaped(sb, v->data.nix_expr.body) ||
                !sb_append_str(sb, ",\"deps\":[")) {
                return false;
            }
            for (i = 0U; i < v->data.nix_expr.deps_len; ++i) {
                if (i != 0U && !sb_append_char(sb, ',')) {
                    return false;
                }
                if (!json_emit_value(sb, v->data.nix_expr.deps[i])) {
                    return false;
                }
            }
            return sb_append_str(sb, "],\"fingerprint\":") && json_emit_escaped(sb, v->data.nix_expr.fingerprint) && sb_append_char(sb, '}');
        case OVAL_SYSTEM:
            return sb_append_str(sb, "{\"t\":\"system\",\"profile_path\":") && json_emit_escaped(sb, v->data.str_val) && sb_append_char(sb, '}');
        default:
            return false;
    }
}

char *oval_to_json(const OValue *v) {
    StringBuilder sb = {0};
    char *out;

    if (!json_emit_value(&sb, v)) {
        sb_free(&sb);
        return NULL;
    }
    out = sb_take(&sb);
    return out;
}

char *owire_cmd_to_json(const OWireCommand *cmd) {
    StringBuilder sb = {0};
    char *out;

    if (cmd == NULL) {
        return NULL;
    }
    switch (cmd->tag) {
        case WIRE_CMD_EXEC:
            if (!sb_append_str(&sb, "{\"cmd\":\"exec\",\"code\":") ||
                !json_emit_escaped(&sb, cmd->code != NULL ? cmd->code : "") ||
                !sb_append_str(&sb, ",\"bindings\":" ) ||
                !json_emit_map_payload(&sb, cmd->bindings) ||
                !sb_append_char(&sb, '}')) {
                sb_free(&sb);
                return NULL;
            }
            break;
        case WIRE_CMD_CLEANUP:
            if (!sb_append_str(&sb, "{\"cmd\":\"cleanup\"}")) {
                sb_free(&sb);
                return NULL;
            }
            break;
        case WIRE_CMD_PING:
            if (!sb_append_str(&sb, "{\"cmd\":\"ping\"}")) {
                sb_free(&sb);
                return NULL;
            }
            break;
        case WIRE_CMD_EVAL_RESULT:
            if (!sb_append_str(&sb, "{\"cmd\":\"eval_result\",\"value\":") ||
                !json_emit_value(&sb, cmd->value) ||
                !sb_append_char(&sb, '}')) {
                sb_free(&sb);
                return NULL;
            }
            break;
        default:
            sb_free(&sb);
            return NULL;
    }
    out = sb_take(&sb);
    return out;
}

/* ────────────────────────────────────────────────────────────────────── */
/* JSON parser                                                           */
/* ────────────────────────────────────────────────────────────────────── */

static JsonNode *json_node_new(JsonType type) {
    JsonNode *node = (JsonNode *)calloc(1U, sizeof(JsonNode));
    if (node != NULL) {
        node->type = type;
    }
    return node;
}

static void json_node_free(JsonNode *node) {
    size_t i;
    if (node == NULL) {
        return;
    }
    switch (node->type) {
        case JSON_STRING:
        case JSON_NUMBER:
            free(node->u.str_val);
            break;
        case JSON_ARRAY:
            for (i = 0U; i < node->u.array.len; ++i) {
                json_node_free(node->u.array.items[i]);
            }
            free(node->u.array.items);
            break;
        case JSON_OBJECT: {
            JsonPair *pair = node->u.object;
            while (pair != NULL) {
                JsonPair *next = pair->next;
                free(pair->key);
                json_node_free(pair->value);
                free(pair);
                pair = next;
            }
            break;
        }
        case JSON_NULL:
        case JSON_BOOL:
        default:
            break;
    }
    free(node);
}

static void json_skip_ws(JsonParser *p) {
    while (p->src[p->pos] != '\0' && isspace((unsigned char)p->src[p->pos])) {
        p->pos += 1U;
    }
}

static bool json_match_literal(JsonParser *p, const char *lit) {
    size_t len = strlen(lit);
    if (strncmp(p->src + p->pos, lit, len) != 0) {
        return false;
    }
    p->pos += len;
    return true;
}

static bool sb_append_codepoint_utf8(StringBuilder *sb, unsigned codepoint) {
    if (codepoint <= 0x7fU) {
        return sb_append_char(sb, (char)codepoint);
    }
    if (codepoint <= 0x7ffU) {
        return sb_append_char(sb, (char)(0xc0U | ((codepoint >> 6U) & 0x1fU))) &&
               sb_append_char(sb, (char)(0x80U | (codepoint & 0x3fU)));
    }
    if (codepoint <= 0xffffU) {
        return sb_append_char(sb, (char)(0xe0U | ((codepoint >> 12U) & 0x0fU))) &&
               sb_append_char(sb, (char)(0x80U | ((codepoint >> 6U) & 0x3fU))) &&
               sb_append_char(sb, (char)(0x80U | (codepoint & 0x3fU)));
    }
    if (codepoint <= 0x10ffffU) {
        return sb_append_char(sb, (char)(0xf0U | ((codepoint >> 18U) & 0x07U))) &&
               sb_append_char(sb, (char)(0x80U | ((codepoint >> 12U) & 0x3fU))) &&
               sb_append_char(sb, (char)(0x80U | ((codepoint >> 6U) & 0x3fU))) &&
               sb_append_char(sb, (char)(0x80U | (codepoint & 0x3fU)));
    }
    return false;
}

static bool parse_hex4(const char *s, unsigned *out) {
    unsigned value = 0U;
    size_t i;
    for (i = 0U; i < 4U; ++i) {
        unsigned char c = (unsigned char)s[i];
        value <<= 4U;
        if (c >= '0' && c <= '9') value |= (unsigned)(c - '0');
        else if (c >= 'a' && c <= 'f') value |= (unsigned)(c - 'a' + 10U);
        else if (c >= 'A' && c <= 'F') value |= (unsigned)(c - 'A' + 10U);
        else return false;
    }
    *out = value;
    return true;
}

static char *json_parse_string_raw(JsonParser *p) {
    StringBuilder sb = {0};

    if (p->src[p->pos] != '"') {
        return NULL;
    }
    p->pos += 1U;
    while (p->src[p->pos] != '\0') {
        unsigned char c = (unsigned char)p->src[p->pos++];
        if (c == '"') {
            return sb_take(&sb);
        }
        if (c == '\\') {
            unsigned cp = 0U;
            unsigned low = 0U;
            char esc = p->src[p->pos++];
            switch (esc) {
                case '"': if (!sb_append_char(&sb, '"')) goto fail; break;
                case '\\': if (!sb_append_char(&sb, '\\')) goto fail; break;
                case '/': if (!sb_append_char(&sb, '/')) goto fail; break;
                case 'b': if (!sb_append_char(&sb, '\b')) goto fail; break;
                case 'f': if (!sb_append_char(&sb, '\f')) goto fail; break;
                case 'n': if (!sb_append_char(&sb, '\n')) goto fail; break;
                case 'r': if (!sb_append_char(&sb, '\r')) goto fail; break;
                case 't': if (!sb_append_char(&sb, '\t')) goto fail; break;
                case 'u':
                    if (!parse_hex4(p->src + p->pos, &cp)) goto fail;
                    p->pos += 4U;
                    if (cp >= 0xd800U && cp <= 0xdbffU && p->src[p->pos] == '\\' && p->src[p->pos + 1U] == 'u') {
                        p->pos += 2U;
                        if (!parse_hex4(p->src + p->pos, &low)) goto fail;
                        p->pos += 4U;
                        if (low >= 0xdc00U && low <= 0xdfffU) {
                            cp = 0x10000U + (((cp - 0xd800U) << 10U) | (low - 0xdc00U));
                        }
                    }
                    if (!sb_append_codepoint_utf8(&sb, cp)) goto fail;
                    break;
                default:
                    goto fail;
            }
        } else {
            if (!sb_append_char(&sb, (char)c)) {
                goto fail;
            }
        }
    }

fail:
    sb_free(&sb);
    return NULL;
}

static JsonNode *json_parse_value(JsonParser *p);

static JsonNode *json_parse_array(JsonParser *p) {
    JsonNode *node = json_node_new(JSON_ARRAY);
    if (node == NULL) {
        return NULL;
    }
    p->pos += 1U;
    json_skip_ws(p);
    if (p->src[p->pos] == ']') {
        p->pos += 1U;
        return node;
    }
    while (p->src[p->pos] != '\0') {
        JsonNode *item = json_parse_value(p);
        if (item == NULL) {
            json_node_free(node);
            return NULL;
        }
        if (node->u.array.len == node->u.array.cap) {
            size_t new_cap = node->u.array.cap == 0U ? 4U : node->u.array.cap * 2U;
            JsonNode **new_items = (JsonNode **)realloc(node->u.array.items, new_cap * sizeof(JsonNode *));
            if (new_items == NULL) {
                json_node_free(item);
                json_node_free(node);
                return NULL;
            }
            node->u.array.items = new_items;
            node->u.array.cap = new_cap;
        }
        node->u.array.items[node->u.array.len++] = item;
        json_skip_ws(p);
        if (p->src[p->pos] == ',') {
            p->pos += 1U;
            json_skip_ws(p);
            continue;
        }
        if (p->src[p->pos] == ']') {
            p->pos += 1U;
            return node;
        }
        break;
    }
    json_node_free(node);
    return NULL;
}

static JsonNode *json_parse_object(JsonParser *p) {
    JsonNode *node = json_node_new(JSON_OBJECT);
    JsonPair **tail;

    if (node == NULL) {
        return NULL;
    }
    p->pos += 1U;
    json_skip_ws(p);
    if (p->src[p->pos] == '}') {
        p->pos += 1U;
        return node;
    }
    tail = &node->u.object;
    while (p->src[p->pos] != '\0') {
        char *key;
        JsonNode *value;
        JsonPair *pair;

        if (p->src[p->pos] != '"') {
            break;
        }
        key = json_parse_string_raw(p);
        if (key == NULL) {
            break;
        }
        json_skip_ws(p);
        if (p->src[p->pos] != ':') {
            free(key);
            break;
        }
        p->pos += 1U;
        json_skip_ws(p);
        value = json_parse_value(p);
        if (value == NULL) {
            free(key);
            break;
        }
        pair = (JsonPair *)calloc(1U, sizeof(JsonPair));
        if (pair == NULL) {
            free(key);
            json_node_free(value);
            break;
        }
        pair->key = key;
        pair->value = value;
        *tail = pair;
        tail = &pair->next;
        json_skip_ws(p);
        if (p->src[p->pos] == ',') {
            p->pos += 1U;
            json_skip_ws(p);
            continue;
        }
        if (p->src[p->pos] == '}') {
            p->pos += 1U;
            return node;
        }
        break;
    }
    json_node_free(node);
    return NULL;
}

static JsonNode *json_parse_number(JsonParser *p) {
    size_t start = p->pos;
    size_t len;
    JsonNode *node;

    if (p->src[p->pos] == '-') {
        p->pos += 1U;
    }
    if (!isdigit((unsigned char)p->src[p->pos])) {
        return NULL;
    }
    if (p->src[p->pos] == '0') {
        p->pos += 1U;
    } else {
        while (isdigit((unsigned char)p->src[p->pos])) {
            p->pos += 1U;
        }
    }
    if (p->src[p->pos] == '.') {
        p->pos += 1U;
        if (!isdigit((unsigned char)p->src[p->pos])) {
            return NULL;
        }
        while (isdigit((unsigned char)p->src[p->pos])) {
            p->pos += 1U;
        }
    }
    if (p->src[p->pos] == 'e' || p->src[p->pos] == 'E') {
        p->pos += 1U;
        if (p->src[p->pos] == '+' || p->src[p->pos] == '-') {
            p->pos += 1U;
        }
        if (!isdigit((unsigned char)p->src[p->pos])) {
            return NULL;
        }
        while (isdigit((unsigned char)p->src[p->pos])) {
            p->pos += 1U;
        }
    }
    len = p->pos - start;
    node = json_node_new(JSON_NUMBER);
    if (node == NULL) {
        return NULL;
    }
    node->u.str_val = (char *)malloc(len + 1U);
    if (node->u.str_val == NULL) {
        json_node_free(node);
        return NULL;
    }
    memcpy(node->u.str_val, p->src + start, len);
    node->u.str_val[len] = '\0';
    return node;
}

static JsonNode *json_parse_value(JsonParser *p) {
    json_skip_ws(p);
    switch (p->src[p->pos]) {
        case '\0':
            return NULL;
        case 'n': {
            JsonNode *node;
            if (!json_match_literal(p, "null")) return NULL;
            node = json_node_new(JSON_NULL);
            return node;
        }
        case 't': {
            JsonNode *node;
            if (!json_match_literal(p, "true")) return NULL;
            node = json_node_new(JSON_BOOL);
            if (node != NULL) node->u.bool_val = true;
            return node;
        }
        case 'f': {
            JsonNode *node;
            if (!json_match_literal(p, "false")) return NULL;
            node = json_node_new(JSON_BOOL);
            if (node != NULL) node->u.bool_val = false;
            return node;
        }
        case '"': {
            JsonNode *node = json_node_new(JSON_STRING);
            if (node == NULL) return NULL;
            node->u.str_val = json_parse_string_raw(p);
            if (node->u.str_val == NULL) {
                json_node_free(node);
                return NULL;
            }
            return node;
        }
        case '[':
            return json_parse_array(p);
        case '{':
            return json_parse_object(p);
        default:
            if (p->src[p->pos] == '-' || isdigit((unsigned char)p->src[p->pos])) {
                return json_parse_number(p);
            }
            return NULL;
    }
}

static JsonNode *json_parse_document(const char *json) {
    JsonParser p;
    JsonNode *root;

    if (json == NULL) {
        return NULL;
    }
    p.src = json;
    p.pos = 0U;
    root = json_parse_value(&p);
    if (root == NULL) {
        return NULL;
    }
    json_skip_ws(&p);
    if (p.src[p.pos] != '\0') {
        json_node_free(root);
        return NULL;
    }
    return root;
}

static const JsonNode *json_object_get(const JsonNode *obj, const char *key) {
    JsonPair *pair;
    if (obj == NULL || obj->type != JSON_OBJECT || key == NULL) {
        return NULL;
    }
    pair = obj->u.object;
    while (pair != NULL) {
        if (strcmp(pair->key, key) == 0) {
            return pair->value;
        }
        pair = pair->next;
    }
    return NULL;
}

static const char *json_node_string(const JsonNode *node) {
    return (node != NULL && node->type == JSON_STRING) ? node->u.str_val : NULL;
}

static bool json_node_bool(const JsonNode *node, bool *out) {
    if (node == NULL || node->type != JSON_BOOL || out == NULL) {
        return false;
    }
    *out = node->u.bool_val;
    return true;
}

static bool parse_int64_str(const char *s, int64_t *out) {
    char *end;
    long long val;
    if (s == NULL || out == NULL) {
        return false;
    }
    errno = 0;
    val = strtoll(s, &end, 10);
    if (errno != 0 || end == s || *end != '\0') {
        return false;
    }
    *out = (int64_t)val;
    return true;
}

static bool parse_uint32_str(const char *s, uint32_t *out) {
    char *end;
    unsigned long long val;
    if (s == NULL || out == NULL) {
        return false;
    }
    errno = 0;
    val = strtoull(s, &end, 10);
    if (errno != 0 || end == s || *end != '\0' || val > 0xffffffffULL) {
        return false;
    }
    *out = (uint32_t)val;
    return true;
}

static bool parse_double_str(const char *s, double *out) {
    char *end;
    double val;
    if (s == NULL || out == NULL) {
        return false;
    }
    errno = 0;
    val = strtod(s, &end);
    if (errno != 0 || end == s || *end != '\0') {
        return false;
    }
    *out = val;
    return true;
}

static bool parse_request_kind_node(const JsonNode *node, RequestKind *out) {
    memset(out, 0, sizeof(*out));
    if (node == NULL) {
        return false;
    }
    if (node->type == JSON_STRING) {
        if (strcmp(node->u.str_val, "instantiate") == 0) {
            out->tag = REQ_INSTANTIATE;
            return true;
        }
        if (strcmp(node->u.str_val, "realise") == 0) {
            out->tag = REQ_REALISE;
            return true;
        }
        return false;
    }
    if (node->type != JSON_OBJECT) {
        return false;
    }
    if (json_object_get(node, "eval") != NULL) {
        const JsonNode *eval = json_object_get(node, "eval");
        const JsonNode *lang = json_object_get(eval, "lang");
        const JsonNode *env_id = json_object_get(eval, "env_id");
        const JsonNode *cacheable = json_object_get(eval, "cacheable");
        bool cache_val;
        uint32_t env_val;
        if (eval == NULL || eval->type != JSON_OBJECT || json_node_string(lang) == NULL ||
            env_id == NULL || env_id->type != JSON_NUMBER || !json_node_bool(cacheable, &cache_val) ||
            !parse_uint32_str(env_id->u.str_val, &env_val)) {
            return false;
        }
        out->tag = REQ_EVAL;
        out->lang = dup_cstr(lang->u.str_val);
        out->env_id = env_val;
        out->cacheable = cache_val;
        return out->lang != NULL;
    }
    if (json_object_get(node, "activate") != NULL) {
        const JsonNode *act = json_object_get(node, "activate");
        const JsonNode *profile = json_object_get(act, "profile");
        const JsonNode *dry_run = json_object_get(act, "dry_run");
        bool dry_val;
        if (act == NULL || act->type != JSON_OBJECT || json_node_string(profile) == NULL ||
            !json_node_bool(dry_run, &dry_val)) {
            return false;
        }
        out->tag = REQ_ACTIVATE;
        out->profile = dup_cstr(profile->u.str_val);
        out->dry_run = dry_val;
        return out->profile != NULL;
    }
    return false;
}

static OValue *ovalue_from_json_node(const JsonNode *node) {
    const JsonNode *type_node;
    const char *tag;

    if (node == NULL || node->type != JSON_OBJECT) {
        return NULL;
    }
    type_node = json_object_get(node, "t");
    tag = json_node_string(type_node);
    if (tag == NULL) {
        return NULL;
    }
    if (strcmp(tag, "null") == 0) {
        return oval_null();
    }
    if (strcmp(tag, "bool") == 0) {
        bool v;
        if (!json_node_bool(json_object_get(node, "v"), &v)) return NULL;
        return oval_bool(v);
    }
    if (strcmp(tag, "int") == 0) {
        int64_t v;
        const JsonNode *n = json_object_get(node, "v");
        if (n == NULL || n->type != JSON_NUMBER || !parse_int64_str(n->u.str_val, &v)) return NULL;
        return oval_int(v);
    }
    if (strcmp(tag, "float") == 0) {
        double v;
        const JsonNode *n = json_object_get(node, "v");
        if (n == NULL || n->type != JSON_NUMBER || !parse_double_str(n->u.str_val, &v)) return NULL;
        return oval_float(v);
    }
    if (strcmp(tag, "str") == 0) {
        const char *s = json_node_string(json_object_get(node, "v"));
        return s != NULL ? oval_str(s) : NULL;
    }
    if (strcmp(tag, "html") == 0) {
        const char *s = json_node_string(json_object_get(node, "v"));
        return s != NULL ? oval_html(s) : NULL;
    }
    if (strcmp(tag, "store_path") == 0) {
        const char *s = json_node_string(json_object_get(node, "path"));
        return s != NULL ? oval_store_path(s) : NULL;
    }
    if (strcmp(tag, "expr") == 0) {
        const char *s = json_node_string(json_object_get(node, "src"));
        return s != NULL ? oval_expr(s) : NULL;
    }
    if (strcmp(tag, "list") == 0) {
        const JsonNode *arr = json_object_get(node, "v");
        OValue **items;
        size_t i;
        if (arr == NULL || arr->type != JSON_ARRAY) return NULL;
        items = arr->u.array.len == 0U ? NULL : (OValue **)calloc(arr->u.array.len, sizeof(OValue *));
        if (arr->u.array.len != 0U && items == NULL) return NULL;
        for (i = 0U; i < arr->u.array.len; ++i) {
            items[i] = ovalue_from_json_node(arr->u.array.items[i]);
            if (items[i] == NULL) {
                size_t j;
                for (j = 0U; j < i; ++j) oval_release(items[j]);
                free(items);
                return NULL;
            }
        }
        return oval_list_take(items, arr->u.array.len);
    }
    if (strcmp(tag, "map") == 0) {
        const JsonNode *obj = json_object_get(node, "v");
        OValueMap *map;
        JsonPair *pair;
        if (obj == NULL || obj->type != JSON_OBJECT) return NULL;
        map = ovalue_map_create();
        if (map == NULL) return NULL;
        pair = obj->u.object;
        while (pair != NULL) {
            OValue *value = ovalue_from_json_node(pair->value);
            char *key = dup_cstr(pair->key);
            if (value == NULL || key == NULL || !map_insert_owned(map, key, value)) {
                free(key);
                oval_release(value);
                ovalue_map_free(map);
                return NULL;
            }
            pair = pair->next;
        }
        return oval_map_take(map);
    }
    if (strcmp(tag, "blob") == 0) {
        const char *b64 = json_node_string(json_object_get(node, "v"));
        const char *mime = json_node_string(json_object_get(node, "mime"));
        if (b64 == NULL || mime == NULL) return NULL;
        return oval_blob_take(b64, mime);
    }
    if (strcmp(tag, "nix_expr") == 0 || strcmp(tag, "thunk") == 0) {
        const char *body = json_node_string(json_object_get(node, "body"));
        const char *fingerprint = json_node_string(json_object_get(node, "fingerprint"));
        const JsonNode *deps_node = json_object_get(node, "deps");
        OValue **deps;
        size_t i;
        if (body == NULL || fingerprint == NULL || deps_node == NULL || deps_node->type != JSON_ARRAY) return NULL;
        deps = deps_node->u.array.len == 0U ? NULL : (OValue **)calloc(deps_node->u.array.len, sizeof(OValue *));
        if (deps_node->u.array.len != 0U && deps == NULL) return NULL;
        for (i = 0U; i < deps_node->u.array.len; ++i) {
            deps[i] = ovalue_from_json_node(deps_node->u.array.items[i]);
            if (deps[i] == NULL) {
                size_t j;
                for (j = 0U; j < i; ++j) oval_release(deps[j]);
                free(deps);
                return NULL;
            }
        }
        return oval_nixish_take(strcmp(tag, "nix_expr") == 0 ? OVAL_NIX_EXPR : OVAL_THUNK,
                                body, deps, deps_node->u.array.len, fingerprint);
    }
    if (strcmp(tag, "derivation") == 0) {
        const char *drv = json_node_string(json_object_get(node, "drv_path"));
        const JsonNode *outputs = json_object_get(node, "outputs");
        const JsonNode *deps_node = json_object_get(node, "deps");
        char **out_names;
        OValue **deps;
        size_t i;
        char *drv_copy;
        if (drv == NULL || outputs == NULL || outputs->type != JSON_ARRAY || deps_node == NULL || deps_node->type != JSON_ARRAY) return NULL;
        drv_copy = dup_cstr(drv);
        if (drv_copy == NULL) return NULL;
        out_names = outputs->u.array.len == 0U ? NULL : (char **)calloc(outputs->u.array.len, sizeof(char *));
        if (outputs->u.array.len != 0U && out_names == NULL) {
            free(drv_copy);
            return NULL;
        }
        for (i = 0U; i < outputs->u.array.len; ++i) {
            const char *name = json_node_string(outputs->u.array.items[i]);
            if (name == NULL) {
                size_t j;
                for (j = 0U; j < i; ++j) free(out_names[j]);
                free(out_names);
                free(drv_copy);
                return NULL;
            }
            out_names[i] = dup_cstr(name);
            if (out_names[i] == NULL) {
                size_t j;
                for (j = 0U; j < i; ++j) free(out_names[j]);
                free(out_names);
                free(drv_copy);
                return NULL;
            }
        }
        deps = deps_node->u.array.len == 0U ? NULL : (OValue **)calloc(deps_node->u.array.len, sizeof(OValue *));
        if (deps_node->u.array.len != 0U && deps == NULL) {
            for (i = 0U; i < outputs->u.array.len; ++i) free(out_names[i]);
            free(out_names);
            free(drv_copy);
            return NULL;
        }
        for (i = 0U; i < deps_node->u.array.len; ++i) {
            deps[i] = ovalue_from_json_node(deps_node->u.array.items[i]);
            if (deps[i] == NULL) {
                size_t j;
                for (j = 0U; j < i; ++j) oval_release(deps[j]);
                free(deps);
                for (j = 0U; j < outputs->u.array.len; ++j) free(out_names[j]);
                free(out_names);
                free(drv_copy);
                return NULL;
            }
        }
        return oval_derivation_take(drv_copy, out_names, outputs->u.array.len, deps, deps_node->u.array.len);
    }
    if (strcmp(tag, "request") == 0) {
        RequestKind kind;
        OValue *source;
        const char *fingerprint = json_node_string(json_object_get(node, "fingerprint"));
        if (fingerprint == NULL || !parse_request_kind_node(json_object_get(node, "kind"), &kind)) {
            return NULL;
        }
        source = ovalue_from_json_node(json_object_get(node, "source"));
        if (source == NULL) {
            request_kind_free(&kind);
            return NULL;
        }
        return oval_request_take(kind, source, fingerprint);
    }
    if (strcmp(tag, "system") == 0) {
        const char *profile = json_node_string(json_object_get(node, "profile_path"));
        return profile != NULL ? oval_system(profile) : NULL;
    }
    return NULL;
}

OValue *oval_from_json(const char *json) {
    JsonNode *root = json_parse_document(json);
    OValue *out;
    if (root == NULL) {
        return NULL;
    }
    out = ovalue_from_json_node(root);
    json_node_free(root);
    return out;
}

OWireResponse *owire_resp_from_json(const char *json) {
    JsonNode *root = json_parse_document(json);
    OWireResponse *resp;
    const char *status;

    if (root == NULL || root->type != JSON_OBJECT) {
        json_node_free(root);
        return NULL;
    }
    status = json_node_string(json_object_get(root, "status"));
    if (status == NULL) {
        json_node_free(root);
        return NULL;
    }
    resp = (OWireResponse *)calloc(1U, sizeof(OWireResponse));
    if (resp == NULL) {
        json_node_free(root);
        return NULL;
    }
    if (strcmp(status, "ok") == 0) {
        resp->tag = WIRE_RESP_OK;
        resp->value = ovalue_from_json_node(json_object_get(root, "value"));
        if (resp->value == NULL) {
            free(resp);
            json_node_free(root);
            return NULL;
        }
    } else if (strcmp(status, "err") == 0) {
        const char *message = json_node_string(json_object_get(root, "message"));
        if (message == NULL) {
            free(resp);
            json_node_free(root);
            return NULL;
        }
        resp->tag = WIRE_RESP_ERR;
        resp->message = dup_cstr(message);
        if (resp->message == NULL) {
            free(resp);
            json_node_free(root);
            return NULL;
        }
    } else if (strcmp(status, "eval_request") == 0) {
        const char *src = json_node_string(json_object_get(root, "src"));
        if (src == NULL) {
            free(resp);
            json_node_free(root);
            return NULL;
        }
        resp->tag = WIRE_RESP_EVAL_REQUEST;
        resp->src = dup_cstr(src);
        if (resp->src == NULL) {
            free(resp);
            json_node_free(root);
            return NULL;
        }
    } else {
        free(resp);
        json_node_free(root);
        return NULL;
    }
    json_node_free(root);
    return resp;
}

void owire_resp_free(OWireResponse *resp) {
    if (resp == NULL) {
        return;
    }
    switch (resp->tag) {
        case WIRE_RESP_OK:
            oval_release(resp->value);
            break;
        case WIRE_RESP_ERR:
            free(resp->message);
            break;
        case WIRE_RESP_EVAL_REQUEST:
            free(resp->src);
            break;
        default:
            break;
    }
    free(resp);
}
