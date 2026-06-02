#ifndef O_LANG_VALUE_H
#define O_LANG_VALUE_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── OValue type tags ─────────────────────────────────────────────────── */
typedef enum {
    OVAL_NULL = 0,
    OVAL_BOOL,
    OVAL_INT,
    OVAL_FLOAT,
    OVAL_STR,
    OVAL_HTML,
    OVAL_STORE_PATH,
    OVAL_EXPR,
    OVAL_LIST,
    OVAL_MAP,
    OVAL_BLOB,
    OVAL_NIX_EXPR,
    OVAL_DERIVATION,
    OVAL_REQUEST,
    OVAL_THUNK,
    OVAL_SYSTEM,
} OValueTag;

/* ── RequestKind tags ─────────────────────────────────────────────────── */
typedef enum {
    REQ_INSTANTIATE = 0,
    REQ_REALISE,
    REQ_EVAL,
    REQ_ACTIVATE,
} RequestKindTag;

/* Forward declarations */
typedef struct OValue OValue;
typedef struct OValueList OValueList;
typedef struct OValueMap OValueMap;
typedef struct OStringVec OStringVec;

/* ── String vector (for outputs, etc.) ──────────────────────────────── */
struct OStringVec {
    char **items;
    size_t len;
    size_t cap;
};

/* ── OValue list ──────────────────────────────────────────────────────── */
struct OValueList {
    OValue **items;
    size_t len;
    size_t cap;
};

/* ── OValue map entry ─────────────────────────────────────────────────── */
typedef struct OMapEntry {
    char *key;
    OValue *value;
    struct OMapEntry *next;
} OMapEntry;

/* ── OValue map (simple hash map) ─────────────────────────────────────── */
struct OValueMap {
    OMapEntry **buckets;
    size_t bucket_count;
    size_t len;
};

/* ── RequestKind ──────────────────────────────────────────────────────── */
typedef struct {
    RequestKindTag tag;
    /* For REQ_EVAL: */
    char *lang;
    uint32_t env_id;
    bool cacheable;
    /* For REQ_ACTIVATE: */
    char *profile;
    bool dry_run;
} RequestKind;

/* ── The OValue tagged union ──────────────────────────────────────────── */
struct OValue {
    OValueTag tag;
    int refcount;
    union {
        bool bool_val;
        int64_t int_val;
        double float_val;
        char *str_val;       /* For STR, HTML, STORE_PATH, EXPR, SYSTEM(profile_path) */
        struct {
            OValue **items;
            size_t len;
            size_t cap;
        } list;
        OValueMap *map;
        struct {
            char *data;    /* base64 string */
            char *mime;
        } blob;
        struct {
            char *body;
            OValue **deps;
            size_t deps_len;
            char *fingerprint;
        } nix_expr;        /* Also used for THUNK (same fields) */
        struct {
            char *drv_path;
            char **outputs;
            size_t outputs_len;
            OValue **deps;
            size_t deps_len;
        } derivation;
        struct {
            RequestKind kind;
            OValue *source;
            char *fingerprint;
        } request;
    } data;
};

/* ── Constructors ─────────────────────────────────────────────────────── */
OValue *oval_null(void);
OValue *oval_bool(bool v);
OValue *oval_int(int64_t v);
OValue *oval_float(double v);
OValue *oval_str(const char *s);
OValue *oval_html(const char *s);
OValue *oval_store_path(const char *path);
OValue *oval_expr(const char *src);
OValue *oval_list(OValue **items, size_t len);
OValue *oval_map(void);  /* creates empty map */
OValue *oval_blob(const unsigned char *data, size_t len, const char *mime);
OValue *oval_nix_expr(const char *body, OValue **deps, size_t deps_len);
OValue *oval_derivation(const char *drv_path, const char **outputs, size_t outputs_len, OValue **deps, size_t deps_len);
OValue *oval_request(RequestKind kind, OValue *source);
OValue *oval_thunk(const char *body, OValue **deps, size_t deps_len);
OValue *oval_system(const char *profile_path);

/* ── Reference counting ───────────────────────────────────────────────── */
OValue *oval_retain(OValue *v);
void oval_release(OValue *v);

/* ── Type predicates ──────────────────────────────────────────────────── */
bool oval_is_null(const OValue *v);
bool oval_is_request(const OValue *v);
bool oval_is_nix_expr(const OValue *v);
bool oval_is_derivation(const OValue *v);
bool oval_is_thunk(const OValue *v);
bool oval_is_system(const OValue *v);
const char *oval_type_name(const OValue *v);

/* ── Accessors ────────────────────────────────────────────────────────── */
bool oval_as_bool(const OValue *v, bool *out);
bool oval_as_int(const OValue *v, int64_t *out);
bool oval_as_float(const OValue *v, double *out);
const char *oval_as_str(const OValue *v);  /* Returns NULL if not a string type */

/* ── Map operations ───────────────────────────────────────────────────── */
void oval_map_set(OValue *map, const char *key, OValue *value);
OValue *oval_map_get(const OValue *map, const char *key);
size_t oval_map_len(const OValue *map);

/* ── Splice representation ────────────────────────────────────────────── */
char *oval_splice_repr(const OValue *v);

/* ── Content identity ─────────────────────────────────────────────────── */
char *oval_content_identity(const OValue *v);

/* ── JSON wire protocol ───────────────────────────────────────────────── */
/* Serialize OValue to JSON string (caller must free) */
char *oval_to_json(const OValue *v);
/* Deserialize JSON string to OValue (caller must oval_release) */
OValue *oval_from_json(const char *json);

/* Wire command/response types */
typedef enum {
    WIRE_CMD_EXEC = 0,
    WIRE_CMD_CLEANUP,
    WIRE_CMD_PING,
    WIRE_CMD_EVAL_RESULT,
} WireCmdTag;

typedef struct {
    WireCmdTag tag;
    char *code;           /* for EXEC */
    OValueMap *bindings;  /* for EXEC */
    OValue *value;        /* for EVAL_RESULT */
} OWireCommand;

typedef enum {
    WIRE_RESP_OK = 0,
    WIRE_RESP_ERR,
    WIRE_RESP_EVAL_REQUEST,
} WireRespTag;

typedef struct {
    WireRespTag tag;
    OValue *value;     /* for OK */
    char *message;     /* for ERR */
    char *src;         /* for EVAL_REQUEST */
} OWireResponse;

char *owire_cmd_to_json(const OWireCommand *cmd);
OWireResponse *owire_resp_from_json(const char *json);
void owire_resp_free(OWireResponse *resp);

/* ── Utility: SHA-256 hex digest ──────────────────────────────────────── */
char *sha256_hex(const char *data, size_t len);

/* ── Utility: base64 encode/decode ────────────────────────────────────── */
char *base64_encode(const unsigned char *data, size_t len);
unsigned char *base64_decode(const char *b64, size_t *out_len);

#ifdef __cplusplus
}
#endif

#endif /* O_LANG_VALUE_H */
