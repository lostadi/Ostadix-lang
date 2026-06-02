#ifndef O_LANG_PARSER_H
#define O_LANG_PARSER_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── AST node types ───────────────────────────────────────────────────── */
typedef enum {
    ONODE_RAW_TEXT = 0,
    ONODE_VAR_REF,
    ONODE_LET_BINDING,
    ONODE_TYPED_EXPR,
    ONODE_CALL,
} ONodeTag;

typedef struct ONode ONode;

/* ── ONode list ───────────────────────────────────────────────────────── */
typedef struct {
    ONode **items;
    size_t len;
    size_t cap;
} ONodeList;

/* ── The ONode tagged union ───────────────────────────────────────────── */
struct ONode {
    ONodeTag tag;
    union {
        /* ONODE_RAW_TEXT */
        char *text;

        /* ONODE_VAR_REF */
        char *var_name;

        /* ONODE_LET_BINDING */
        struct {
            char *name;
            ONode *expr;
        } let_binding;

        /* ONODE_TYPED_EXPR */
        struct {
            char *lang;
            uint32_t env_id;
            char *attr;    /* NULL if no attribute */
            ONode **body;
            size_t body_len;
            size_t body_cap;
        } typed_expr;

        /* ONODE_CALL */
        struct {
            char *fn_name;
            ONode **args;
            size_t args_len;
            size_t args_cap;
        } call;
    } data;
};

/* ── String set (for registered backends) ─────────────────────────────── */
typedef struct {
    char **items;
    size_t len;
    size_t cap;
} StringSet;

StringSet *string_set_new(void);
void string_set_add(StringSet *set, const char *s);
bool string_set_contains(const StringSet *set, const char *s);
void string_set_free(StringSet *set);

/* ── Parser ───────────────────────────────────────────────────────────── */
typedef struct {
    const char *source;
    size_t source_len;
    size_t pos;
    size_t line;
    const StringSet *registered_backends;
    char error_msg[512];
} OParser;

/* Initialize a parser. source must remain valid for the lifetime of the parser. */
void parser_init(OParser *p, const char *source, const StringSet *backends);

/* Parse the full document. Returns a list of ONodes. On error, returns NULL
   and parser->error_msg contains the error description. */
ONodeList *parser_parse(OParser *p);

/* Free an ONode and all its children */
void onode_free(ONode *node);

/* Free an ONodeList and all contained nodes */
void onode_list_free(ONodeList *list);

/* ── Source reconstruction ────────────────────────────────────────────── */
/* Reconstruct O source text from a list of ONodes. Returns malloc'd string. */
char *reconstruct_source(ONode **nodes, size_t len);

#ifdef __cplusplus
}
#endif

#endif /* O_LANG_PARSER_H */
