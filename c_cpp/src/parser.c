#include "parser.h"

#include <ctype.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *SEQUENCING_LANGS[] = {"quote", "O"};
static const size_t SEQUENCING_LANGS_LEN = 2;

typedef struct {
    char *lang;
    uint32_t env_id;
    char *attr;
    char *raw;
} Tag;

typedef struct {
    char *buf;
    size_t len;
    size_t cap;
} StringBuilder;

static bool is_ident_start(unsigned char b);
static bool is_ident_continue(unsigned char b);
static bool is_sequencing_lang(const char *lang);
static void parser_set_error(OParser *p, const char *fmt, ...);
static bool parser_has_error(const OParser *p);
static ONodeList *onode_list_new(void);
static bool onode_list_reserve(ONodeList *list, size_t needed);
static bool onode_list_push(ONodeList *list, ONode *node);
static ONode *onode_new_raw_text_owned(char *text);
static ONode *onode_new_var_ref_owned(char *name);
static ONode *onode_new_let_binding_owned(char *name, ONode *expr);
static ONode *onode_new_typed_expr_owned(char *lang, uint32_t env_id, char *attr,
                                         ONode **body, size_t body_len, size_t body_cap);
static ONode *onode_new_call_owned(char *fn_name, ONode **args, size_t args_len, size_t args_cap);
static void free_tag(Tag *tag);
static char *dup_cstr(const char *s);
static char *dup_range(const char *start, size_t len);
static bool append_owned_string(char **dst, const char *suffix);
static bool string_builder_init(StringBuilder *sb);
static bool string_builder_reserve(StringBuilder *sb, size_t extra);
static bool string_builder_append(StringBuilder *sb, const char *s);
static bool string_builder_append_n(StringBuilder *sb, const char *s, size_t len);
static bool string_builder_append_char(StringBuilder *sb, char c);
static void advance_one_byte(OParser *p);
static void advance_bytes(OParser *p, size_t n);
static unsigned char current_byte(const OParser *p);
static bool has_current_byte(const OParser *p);
static bool starts_with_at(const OParser *p, size_t pos, const char *pat);
static bool starts_with(const OParser *p, const char *pat);
static void skip_horizontal_whitespace(OParser *p);
static void skip_whitespace(OParser *p);
static void skip_to_end_of_line(OParser *p);
static bool starts_with_let_keyword(const OParser *p);
static char *parse_identifier(OParser *p);
static size_t last_opener_start(const OParser *p, size_t raw_len);
static size_t pos_before_var(const OParser *p, const char *name);
static bool flush_text(ONodeList *nodes, const OParser *p, size_t start, size_t end);
static bool append_raw_text_literal(ONodeList *nodes, const char *literal);
static char *try_parse_var_ref(OParser *p);
static Tag *try_parse_opener(OParser *p);
static ONode *try_parse_call(OParser *p);
static ONode *try_parse_let_binding(OParser *p);
static ONodeList *parse_until(OParser *p, const Tag *expected_closer);
static void reconstruct_node(const ONode *node, StringBuilder *sb);

StringSet *string_set_new(void) {
    StringSet *set = (StringSet *)calloc(1, sizeof(StringSet));
    return set;
}

void string_set_add(StringSet *set, const char *s) {
    char **new_items;
    size_t new_cap;

    if (set == NULL || s == NULL) {
        return;
    }
    if (string_set_contains(set, s)) {
        return;
    }
    if (set->len == set->cap) {
        new_cap = (set->cap == 0) ? 4 : set->cap * 2;
        new_items = (char **)realloc(set->items, new_cap * sizeof(char *));
        if (new_items == NULL) {
            return;
        }
        set->items = new_items;
        set->cap = new_cap;
    }
    set->items[set->len] = dup_cstr(s);
    if (set->items[set->len] == NULL) {
        return;
    }
    set->len += 1;
}

bool string_set_contains(const StringSet *set, const char *s) {
    size_t i;

    if (set == NULL || s == NULL) {
        return false;
    }
    for (i = 0; i < set->len; i++) {
        if (strcmp(set->items[i], s) == 0) {
            return true;
        }
    }
    return false;
}

void string_set_free(StringSet *set) {
    size_t i;

    if (set == NULL) {
        return;
    }
    for (i = 0; i < set->len; i++) {
        free(set->items[i]);
    }
    free(set->items);
    free(set);
}

void parser_init(OParser *p, const char *source, const StringSet *backends) {
    if (p == NULL) {
        return;
    }
    p->source = (source != NULL) ? source : "";
    p->source_len = strlen(p->source);
    p->pos = 0;
    p->line = 1;
    p->registered_backends = backends;
    p->error_msg[0] = '\0';
}

ONodeList *parser_parse(OParser *p) {
    if (p == NULL) {
        return NULL;
    }
    p->error_msg[0] = '\0';
    return parse_until(p, NULL);
}

void onode_free(ONode *node) {
    size_t i;

    if (node == NULL) {
        return;
    }

    switch (node->tag) {
        case ONODE_RAW_TEXT:
            free(node->data.text);
            break;
        case ONODE_VAR_REF:
            free(node->data.var_name);
            break;
        case ONODE_LET_BINDING:
            free(node->data.let_binding.name);
            onode_free(node->data.let_binding.expr);
            break;
        case ONODE_TYPED_EXPR:
            free(node->data.typed_expr.lang);
            free(node->data.typed_expr.attr);
            for (i = 0; i < node->data.typed_expr.body_len; i++) {
                onode_free(node->data.typed_expr.body[i]);
            }
            free(node->data.typed_expr.body);
            break;
        case ONODE_CALL:
            free(node->data.call.fn_name);
            for (i = 0; i < node->data.call.args_len; i++) {
                onode_free(node->data.call.args[i]);
            }
            free(node->data.call.args);
            break;
        default:
            break;
    }

    free(node);
}

void onode_list_free(ONodeList *list) {
    size_t i;

    if (list == NULL) {
        return;
    }
    for (i = 0; i < list->len; i++) {
        onode_free(list->items[i]);
    }
    free(list->items);
    free(list);
}

char *reconstruct_source(ONode **nodes, size_t len) {
    StringBuilder sb;
    size_t i;

    if (!string_builder_init(&sb)) {
        return NULL;
    }
    for (i = 0; i < len; i++) {
        reconstruct_node(nodes[i], &sb);
    }
    return sb.buf;
}

static bool is_ident_start(unsigned char b) {
    return isalpha((int)b) != 0 || b == '_';
}

static bool is_ident_continue(unsigned char b) {
    return isalnum((int)b) != 0 || b == '_';
}

static bool is_sequencing_lang(const char *lang) {
    size_t i;

    if (lang == NULL) {
        return false;
    }
    for (i = 0; i < SEQUENCING_LANGS_LEN; i++) {
        if (strcmp(lang, SEQUENCING_LANGS[i]) == 0) {
            return true;
        }
    }
    return false;
}

static void parser_set_error(OParser *p, const char *fmt, ...) {
    va_list ap;

    if (p == NULL || fmt == NULL || parser_has_error(p)) {
        return;
    }
    va_start(ap, fmt);
    vsnprintf(p->error_msg, sizeof(p->error_msg), fmt, ap);
    va_end(ap);
}

static bool parser_has_error(const OParser *p) {
    return p != NULL && p->error_msg[0] != '\0';
}

static ONodeList *onode_list_new(void) {
    ONodeList *list = (ONodeList *)calloc(1, sizeof(ONodeList));
    return list;
}

static bool onode_list_reserve(ONodeList *list, size_t needed) {
    ONode **new_items;
    size_t new_cap;

    if (list == NULL) {
        return false;
    }
    if (needed <= list->cap) {
        return true;
    }
    new_cap = (list->cap == 0) ? 4 : list->cap;
    while (new_cap < needed) {
        if (new_cap > (SIZE_MAX / 2)) {
            return false;
        }
        new_cap *= 2;
    }
    new_items = (ONode **)realloc(list->items, new_cap * sizeof(ONode *));
    if (new_items == NULL) {
        return false;
    }
    list->items = new_items;
    list->cap = new_cap;
    return true;
}

static bool onode_list_push(ONodeList *list, ONode *node) {
    if (list == NULL || node == NULL) {
        return false;
    }
    if (!onode_list_reserve(list, list->len + 1)) {
        return false;
    }
    list->items[list->len] = node;
    list->len += 1;
    return true;
}

static ONode *onode_new_raw_text_owned(char *text) {
    ONode *node = (ONode *)calloc(1, sizeof(ONode));
    if (node == NULL) {
        free(text);
        return NULL;
    }
    node->tag = ONODE_RAW_TEXT;
    node->data.text = text;
    return node;
}

static ONode *onode_new_var_ref_owned(char *name) {
    ONode *node = (ONode *)calloc(1, sizeof(ONode));
    if (node == NULL) {
        free(name);
        return NULL;
    }
    node->tag = ONODE_VAR_REF;
    node->data.var_name = name;
    return node;
}

static ONode *onode_new_let_binding_owned(char *name, ONode *expr) {
    ONode *node = (ONode *)calloc(1, sizeof(ONode));
    if (node == NULL) {
        free(name);
        onode_free(expr);
        return NULL;
    }
    node->tag = ONODE_LET_BINDING;
    node->data.let_binding.name = name;
    node->data.let_binding.expr = expr;
    return node;
}

static ONode *onode_new_typed_expr_owned(char *lang, uint32_t env_id, char *attr,
                                         ONode **body, size_t body_len, size_t body_cap) {
    ONode *node = (ONode *)calloc(1, sizeof(ONode));
    if (node == NULL) {
        size_t i;
        free(lang);
        free(attr);
        for (i = 0; i < body_len; i++) {
            onode_free(body[i]);
        }
        free(body);
        return NULL;
    }
    node->tag = ONODE_TYPED_EXPR;
    node->data.typed_expr.lang = lang;
    node->data.typed_expr.env_id = env_id;
    node->data.typed_expr.attr = attr;
    node->data.typed_expr.body = body;
    node->data.typed_expr.body_len = body_len;
    node->data.typed_expr.body_cap = body_cap;
    return node;
}

static ONode *onode_new_call_owned(char *fn_name, ONode **args, size_t args_len, size_t args_cap) {
    ONode *node = (ONode *)calloc(1, sizeof(ONode));
    if (node == NULL) {
        size_t i;
        free(fn_name);
        for (i = 0; i < args_len; i++) {
            onode_free(args[i]);
        }
        free(args);
        return NULL;
    }
    node->tag = ONODE_CALL;
    node->data.call.fn_name = fn_name;
    node->data.call.args = args;
    node->data.call.args_len = args_len;
    node->data.call.args_cap = args_cap;
    return node;
}

static void free_tag(Tag *tag) {
    if (tag == NULL) {
        return;
    }
    free(tag->lang);
    free(tag->attr);
    free(tag->raw);
    free(tag);
}

static char *dup_cstr(const char *s) {
    size_t len;
    char *copy;

    if (s == NULL) {
        return NULL;
    }
    len = strlen(s);
    copy = (char *)malloc(len + 1);
    if (copy == NULL) {
        return NULL;
    }
    memcpy(copy, s, len + 1);
    return copy;
}

static char *dup_range(const char *start, size_t len) {
    char *copy = (char *)malloc(len + 1);
    if (copy == NULL) {
        return NULL;
    }
    if (len > 0) {
        memcpy(copy, start, len);
    }
    copy[len] = '\0';
    return copy;
}

static bool append_owned_string(char **dst, const char *suffix) {
    size_t dst_len;
    size_t suffix_len;
    char *new_buf;

    if (dst == NULL || suffix == NULL) {
        return false;
    }
    if (*dst == NULL) {
        *dst = dup_cstr(suffix);
        return *dst != NULL;
    }
    dst_len = strlen(*dst);
    suffix_len = strlen(suffix);
    new_buf = (char *)realloc(*dst, dst_len + suffix_len + 1);
    if (new_buf == NULL) {
        return false;
    }
    memcpy(new_buf + dst_len, suffix, suffix_len + 1);
    *dst = new_buf;
    return true;
}

static bool string_builder_init(StringBuilder *sb) {
    if (sb == NULL) {
        return false;
    }
    sb->cap = 64;
    sb->len = 0;
    sb->buf = (char *)malloc(sb->cap);
    if (sb->buf == NULL) {
        sb->cap = 0;
        return false;
    }
    sb->buf[0] = '\0';
    return true;
}

static bool string_builder_reserve(StringBuilder *sb, size_t extra) {
    size_t needed;
    size_t new_cap;
    char *new_buf;

    if (sb == NULL) {
        return false;
    }
    if (extra > SIZE_MAX - sb->len - 1) {
        return false;
    }
    needed = sb->len + extra + 1;
    if (needed <= sb->cap) {
        return true;
    }
    new_cap = sb->cap;
    while (new_cap < needed) {
        if (new_cap > (SIZE_MAX / 2)) {
            return false;
        }
        new_cap *= 2;
    }
    new_buf = (char *)realloc(sb->buf, new_cap);
    if (new_buf == NULL) {
        return false;
    }
    sb->buf = new_buf;
    sb->cap = new_cap;
    return true;
}

static bool string_builder_append(StringBuilder *sb, const char *s) {
    size_t len;

    if (sb == NULL || s == NULL) {
        return false;
    }
    len = strlen(s);
    return string_builder_append_n(sb, s, len);
}

static bool string_builder_append_n(StringBuilder *sb, const char *s, size_t len) {
    if (sb == NULL || s == NULL) {
        return false;
    }
    if (!string_builder_reserve(sb, len)) {
        return false;
    }
    memcpy(sb->buf + sb->len, s, len);
    sb->len += len;
    sb->buf[sb->len] = '\0';
    return true;
}

static bool string_builder_append_char(StringBuilder *sb, char c) {
    if (sb == NULL) {
        return false;
    }
    if (!string_builder_reserve(sb, 1)) {
        return false;
    }
    sb->buf[sb->len] = c;
    sb->len += 1;
    sb->buf[sb->len] = '\0';
    return true;
}

static void advance_one_byte(OParser *p) {
    if (p == NULL || p->pos >= p->source_len) {
        return;
    }
    if ((unsigned char)p->source[p->pos] == (unsigned char)'\n') {
        p->line += 1;
    }
    p->pos += 1;
}

static void advance_bytes(OParser *p, size_t n) {
    size_t i;

    if (p == NULL) {
        return;
    }
    for (i = 0; i < n; i++) {
        advance_one_byte(p);
    }
}

static unsigned char current_byte(const OParser *p) {
    if (p == NULL || p->pos >= p->source_len) {
        return 0;
    }
    return (unsigned char)p->source[p->pos];
}

static bool has_current_byte(const OParser *p) {
    return p != NULL && p->pos < p->source_len;
}

static bool starts_with_at(const OParser *p, size_t pos, const char *pat) {
    size_t pat_len;

    if (p == NULL || pat == NULL) {
        return false;
    }
    pat_len = strlen(pat);
    if (pos > p->source_len || pat_len > p->source_len - pos) {
        return false;
    }
    return strncmp(p->source + pos, pat, pat_len) == 0;
}

static bool starts_with(const OParser *p, const char *pat) {
    if (p == NULL) {
        return false;
    }
    return starts_with_at(p, p->pos, pat);
}

static void skip_horizontal_whitespace(OParser *p) {
    while (has_current_byte(p) && (current_byte(p) == ' ' || current_byte(p) == '\t')) {
        advance_one_byte(p);
    }
}

static void skip_whitespace(OParser *p) {
    while (has_current_byte(p)) {
        unsigned char b = current_byte(p);
        if (b != ' ' && b != '\t' && b != '\n' && b != '\r') {
            break;
        }
        advance_one_byte(p);
    }
}

static void skip_to_end_of_line(OParser *p) {
    while (has_current_byte(p)) {
        unsigned char b = current_byte(p);
        advance_one_byte(p);
        if (b == '\n') {
            break;
        }
    }
}

static bool starts_with_let_keyword(const OParser *p) {
    size_t after;
    bool before_ok;
    bool after_ok;

    if (p == NULL) {
        return false;
    }
    if (!starts_with(p, "let")) {
        return false;
    }

    before_ok = (p->pos == 0) || (isspace((unsigned char)p->source[p->pos - 1]) != 0);
    after = p->pos + 3;
    after_ok = (after >= p->source_len) || (isspace((unsigned char)p->source[after]) != 0);

    return before_ok && after_ok;
}

static char *parse_identifier(OParser *p) {
    size_t start;
    size_t end;

    if (p == NULL || !has_current_byte(p)) {
        return NULL;
    }
    start = p->pos;
    if (!is_ident_start((unsigned char)p->source[start])) {
        return NULL;
    }
    end = start + 1;
    while (end < p->source_len && is_ident_continue((unsigned char)p->source[end])) {
        end += 1;
    }
    p->pos = end;
    return dup_range(p->source + start, end - start);
}

static size_t last_opener_start(const OParser *p, size_t raw_len) {
    return p->pos - raw_len - 2;
}

static size_t pos_before_var(const OParser *p, const char *name) {
    return p->pos - strlen(name) - 1;
}

static bool flush_text(ONodeList *nodes, const OParser *p, size_t start, size_t end) {
    char *text;
    ONode *node;

    if (nodes == NULL || p == NULL) {
        return false;
    }
    if (end <= start) {
        return true;
    }
    text = dup_range(p->source + start, end - start);
    if (text == NULL) {
        return false;
    }
    node = onode_new_raw_text_owned(text);
    if (node == NULL) {
        return false;
    }
    if (!onode_list_push(nodes, node)) {
        onode_free(node);
        return false;
    }
    return true;
}

static bool append_raw_text_literal(ONodeList *nodes, const char *literal) {
    ONode *node;

    if (nodes == NULL || literal == NULL) {
        return false;
    }
    if (nodes->len > 0 && nodes->items[nodes->len - 1] != NULL &&
        nodes->items[nodes->len - 1]->tag == ONODE_RAW_TEXT) {
        return append_owned_string(&nodes->items[nodes->len - 1]->data.text, literal);
    }
    node = onode_new_raw_text_owned(dup_cstr(literal));
    if (node == NULL) {
        return false;
    }
    if (!onode_list_push(nodes, node)) {
        onode_free(node);
        return false;
    }
    return true;
}

static char *try_parse_var_ref(OParser *p) {
    size_t start;
    size_t name_start;
    size_t end;

    if (p == NULL || current_byte(p) != '$') {
        return NULL;
    }

    start = p->pos;
    name_start = start + 1;
    if (name_start >= p->source_len) {
        return NULL;
    }
    if (!is_ident_start((unsigned char)p->source[name_start])) {
        return NULL;
    }

    end = name_start + 1;
    while (end < p->source_len && is_ident_continue((unsigned char)p->source[end])) {
        end += 1;
    }

    p->pos = end;
    return dup_range(p->source + name_start, end - name_start);
}

static Tag *try_parse_opener(OParser *p) {
    size_t start;
    size_t i;
    size_t env_start;
    size_t digits_start;
    unsigned long parsed_env;
    char *lang;
    uint32_t env_id = UINT32_MAX;
    char *attr = NULL;
    char *raw = NULL;
    Tag *tag;

    if (p == NULL || !has_current_byte(p)) {
        return NULL;
    }

    start = p->pos;
    if (!is_ident_start((unsigned char)p->source[start])) {
        return NULL;
    }

    i = start + 1;
    while (i < p->source_len && is_ident_continue((unsigned char)p->source[i])) {
        i += 1;
    }

    lang = dup_range(p->source + start, i - start);
    if (lang == NULL) {
        parser_set_error(p, "Out of memory");
        return NULL;
    }
    if (!string_set_contains(p->registered_backends, lang)) {
        free(lang);
        return NULL;
    }

    raw = dup_cstr(lang);
    if (raw == NULL) {
        free(lang);
        parser_set_error(p, "Out of memory");
        return NULL;
    }

    if (i < p->source_len && p->source[i] == '[') {
        env_start = i;
        i += 1;
        digits_start = i;
        while (i < p->source_len && isdigit((unsigned char)p->source[i]) != 0) {
            i += 1;
        }
        if (digits_start == i) {
            free(lang);
            free(raw);
            return NULL;
        }
        if (i >= p->source_len || p->source[i] != ']') {
            free(lang);
            free(raw);
            return NULL;
        }
        {
            char *digits = dup_range(p->source + digits_start, i - digits_start);
            if (digits == NULL) {
                free(lang);
                free(raw);
                parser_set_error(p, "Out of memory");
                return NULL;
            }
            parsed_env = strtoul(digits, NULL, 10);
            free(digits);
        }
        if (parsed_env > UINT32_MAX) {
            free(lang);
            free(raw);
            parser_set_error(p, "Line %zu: invalid env id", p->line);
            return NULL;
        }
        env_id = (uint32_t)parsed_env;
        i += 1;
        {
            char *env_piece = dup_range(p->source + env_start, i - env_start);
            if (env_piece == NULL) {
                free(lang);
                free(raw);
                parser_set_error(p, "Out of memory");
                return NULL;
            }
            if (!append_owned_string(&raw, env_piece)) {
                free(env_piece);
                free(lang);
                free(raw);
                parser_set_error(p, "Out of memory");
                return NULL;
            }
            free(env_piece);
        }
    }

    if (i < p->source_len && p->source[i] == '{') {
        size_t attr_start = i;
        size_t attr_body_start;

        i += 1;
        attr_body_start = i;
        while (i < p->source_len && p->source[i] != '}') {
            i += 1;
        }
        if (attr_body_start == i) {
            free(lang);
            free(raw);
            return NULL;
        }
        if (i >= p->source_len || p->source[i] != '}') {
            free(lang);
            free(raw);
            return NULL;
        }

        attr = dup_range(p->source + attr_body_start, i - attr_body_start);
        if (attr == NULL) {
            free(lang);
            free(raw);
            parser_set_error(p, "Out of memory");
            return NULL;
        }
        i += 1;
        {
            char *attr_piece = dup_range(p->source + attr_start, i - attr_start);
            if (attr_piece == NULL) {
                free(lang);
                free(attr);
                free(raw);
                parser_set_error(p, "Out of memory");
                return NULL;
            }
            if (!append_owned_string(&raw, attr_piece)) {
                free(attr_piece);
                free(lang);
                free(attr);
                free(raw);
                parser_set_error(p, "Out of memory");
                return NULL;
            }
            free(attr_piece);
        }
    }

    if (i + 2 <= p->source_len && p->source[i] == '^' && p->source[i + 1] == '(') {
        p->pos = i + 2;
        tag = (Tag *)calloc(1, sizeof(Tag));
        if (tag == NULL) {
            free(lang);
            free(attr);
            free(raw);
            parser_set_error(p, "Out of memory");
            return NULL;
        }
        tag->lang = lang;
        tag->env_id = env_id;
        tag->attr = attr;
        tag->raw = raw;
        return tag;
    }

    free(lang);
    free(attr);
    free(raw);
    return NULL;
}

static ONode *try_parse_call(OParser *p) {
    size_t original_pos;
    size_t original_line;
    char *name;
    ONodeList *args;
    ONode *result;

    if (p == NULL) {
        return NULL;
    }

    original_pos = p->pos;
    original_line = p->line;
    name = parse_identifier(p);
    if (name == NULL) {
        return NULL;
    }

    if (string_set_contains(p->registered_backends, name) || current_byte(p) != '(') {
        free(name);
        p->pos = original_pos;
        p->line = original_line;
        return NULL;
    }

    advance_one_byte(p);
    skip_whitespace(p);

    args = onode_list_new();
    if (args == NULL) {
        free(name);
        parser_set_error(p, "Out of memory");
        return NULL;
    }

    while (true) {
        ONode *arg = NULL;

        if (current_byte(p) == ')') {
            advance_one_byte(p);
            break;
        }

        if (current_byte(p) == '$') {
            char *var = try_parse_var_ref(p);
            if (var == NULL) {
                parser_set_error(p, "Line %zu: expected variable reference after $", p->line);
                onode_list_free(args);
                free(name);
                return NULL;
            }
            arg = onode_new_var_ref_owned(var);
        } else {
            arg = try_parse_call(p);
        }

        if (parser_has_error(p)) {
            onode_list_free(args);
            free(name);
            return NULL;
        }
        if (arg == NULL) {
            parser_set_error(p, "Line %zu: in call `%s(...)`, expected $var or nested call",
                             p->line, name);
            onode_list_free(args);
            free(name);
            return NULL;
        }
        if (!onode_list_push(args, arg)) {
            onode_free(arg);
            onode_list_free(args);
            free(name);
            parser_set_error(p, "Out of memory");
            return NULL;
        }

        skip_whitespace(p);
        if (current_byte(p) == ',') {
            advance_one_byte(p);
            skip_whitespace(p);
        } else if (current_byte(p) == ')') {
            advance_one_byte(p);
            break;
        } else {
            parser_set_error(p, "Line %zu: in call `%s(...)`, expected ',' or ')'",
                             p->line, name);
            onode_list_free(args);
            free(name);
            return NULL;
        }
    }

    result = onode_new_call_owned(name, args->items, args->len, args->cap);
    free(args);
    if (result == NULL) {
        parser_set_error(p, "Out of memory");
        return NULL;
    }
    return result;
}

static ONode *try_parse_let_binding(OParser *p) {
    size_t original_pos;
    size_t original_line;
    char *name;
    ONode *call_expr;
    Tag *tag;
    ONodeList *body;
    ONode *typed_expr;

    if (p == NULL || !starts_with_let_keyword(p)) {
        return NULL;
    }

    original_pos = p->pos;
    original_line = p->line;

    advance_bytes(p, 3);
    skip_horizontal_whitespace(p);

    name = parse_identifier(p);
    if (name == NULL) {
        p->pos = original_pos;
        p->line = original_line;
        return NULL;
    }

    skip_horizontal_whitespace(p);
    if (current_byte(p) != '=') {
        free(name);
        p->pos = original_pos;
        p->line = original_line;
        return NULL;
    }

    advance_one_byte(p);
    skip_whitespace(p);

    call_expr = try_parse_call(p);
    if (parser_has_error(p)) {
        free(name);
        return NULL;
    }
    if (call_expr != NULL) {
        ONode *binding = onode_new_let_binding_owned(name, call_expr);
        if (binding == NULL) {
            parser_set_error(p, "Out of memory");
        }
        return binding;
    }

    tag = try_parse_opener(p);
    if (parser_has_error(p)) {
        free(name);
        return NULL;
    }
    if (tag == NULL) {
        parser_set_error(p, "Line %zu: let binding `%s` must be assigned a typed expression or a call",
                         p->line, name);
        free(name);
        return NULL;
    }

    body = parse_until(p, tag);
    if (body == NULL) {
        free(name);
        free_tag(tag);
        return NULL;
    }

    typed_expr = onode_new_typed_expr_owned(tag->lang, tag->env_id, tag->attr,
                                            body->items, body->len, body->cap);
    free(body);
    free(tag->raw);
    free(tag);
    if (typed_expr == NULL) {
        free(name);
        parser_set_error(p, "Out of memory");
        return NULL;
    }

    {
        ONode *binding = onode_new_let_binding_owned(name, typed_expr);
        if (binding == NULL) {
            parser_set_error(p, "Out of memory");
        }
        return binding;
    }
}

static ONodeList *parse_until(OParser *p, const Tag *expected_closer) {
    ONodeList *nodes;
    size_t text_start;
    bool in_seq;
    char *closer = NULL;
    size_t closer_len = 0;

    if (p == NULL) {
        return NULL;
    }

    nodes = onode_list_new();
    if (nodes == NULL) {
        parser_set_error(p, "Out of memory");
        return NULL;
    }

    text_start = p->pos;
    in_seq = (expected_closer == NULL) || is_sequencing_lang(expected_closer->lang);

    if (expected_closer != NULL) {
        size_t raw_len = strlen(expected_closer->raw);
        closer = (char *)malloc(raw_len + 3);
        if (closer == NULL) {
            onode_list_free(nodes);
            parser_set_error(p, "Out of memory");
            return NULL;
        }
        closer[0] = ')';
        closer[1] = '_';
        memcpy(closer + 2, expected_closer->raw, raw_len + 1);
        closer_len = raw_len + 2;
    }

    while (p->pos < p->source_len) {
        if (expected_closer != NULL && starts_with(p, closer)) {
            if (!flush_text(nodes, p, text_start, p->pos)) {
                onode_list_free(nodes);
                free(closer);
                parser_set_error(p, "Out of memory");
                return NULL;
            }
            advance_bytes(p, closer_len);
            free(closer);
            return nodes;
        }

        if (in_seq && current_byte(p) == '#') {
            if (!flush_text(nodes, p, text_start, p->pos)) {
                onode_list_free(nodes);
                free(closer);
                parser_set_error(p, "Out of memory");
                return NULL;
            }
            skip_to_end_of_line(p);
            text_start = p->pos;
            continue;
        }

        if (starts_with_let_keyword(p)) {
            size_t let_start = p->pos;
            ONode *binding = try_parse_let_binding(p);
            if (parser_has_error(p)) {
                onode_list_free(nodes);
                free(closer);
                return NULL;
            }
            if (binding != NULL) {
                if (!flush_text(nodes, p, text_start, let_start)) {
                    onode_free(binding);
                    onode_list_free(nodes);
                    free(closer);
                    parser_set_error(p, "Out of memory");
                    return NULL;
                }
                if (!onode_list_push(nodes, binding)) {
                    onode_free(binding);
                    onode_list_free(nodes);
                    free(closer);
                    parser_set_error(p, "Out of memory");
                    return NULL;
                }
                text_start = p->pos;
                continue;
            }
        }

        if (current_byte(p) == '\\') {
            size_t after_bs = p->pos + 1;
            if (after_bs < p->source_len) {
                size_t temp_pos = p->pos;
                p->pos = after_bs;
                {
                    Tag *escaped_tag = try_parse_opener(p);
                    if (parser_has_error(p)) {
                        onode_list_free(nodes);
                        free(closer);
                        return NULL;
                    }
                    if (escaped_tag != NULL) {
                        char *literal = (char *)malloc(strlen(escaped_tag->raw) + 3);
                        if (literal == NULL) {
                            free_tag(escaped_tag);
                            onode_list_free(nodes);
                            free(closer);
                            parser_set_error(p, "Out of memory");
                            return NULL;
                        }
                        strcpy(literal, escaped_tag->raw);
                        strcat(literal, "^(");
                        if (!flush_text(nodes, p, text_start, temp_pos) ||
                            !append_raw_text_literal(nodes, literal)) {
                            free(literal);
                            free_tag(escaped_tag);
                            onode_list_free(nodes);
                            free(closer);
                            parser_set_error(p, "Out of memory");
                            return NULL;
                        }
                        free(literal);
                        free_tag(escaped_tag);
                        text_start = p->pos;
                        continue;
                    }
                }
                p->pos = temp_pos;
                if (expected_closer != NULL && starts_with_at(p, after_bs, closer)) {
                    p->pos = temp_pos;
                    if (!flush_text(nodes, p, text_start, p->pos) ||
                        !append_raw_text_literal(nodes, closer)) {
                        onode_list_free(nodes);
                        free(closer);
                        parser_set_error(p, "Out of memory");
                        return NULL;
                    }
                    p->pos = after_bs + closer_len;
                    text_start = p->pos;
                    continue;
                }
            }
        }

        if (current_byte(p) == '$') {
            char *name = try_parse_var_ref(p);
            if (name != NULL) {
                ONode *var_node;
                if (!flush_text(nodes, p, text_start, pos_before_var(p, name))) {
                    free(name);
                    onode_list_free(nodes);
                    free(closer);
                    parser_set_error(p, "Out of memory");
                    return NULL;
                }
                var_node = onode_new_var_ref_owned(name);
                if (var_node == NULL || !onode_list_push(nodes, var_node)) {
                    onode_free(var_node);
                    onode_list_free(nodes);
                    free(closer);
                    parser_set_error(p, "Out of memory");
                    return NULL;
                }
                text_start = p->pos;
                continue;
            }
        }

        {
            Tag *tag = try_parse_opener(p);
            if (parser_has_error(p)) {
                onode_list_free(nodes);
                free(closer);
                return NULL;
            }
            if (tag != NULL) {
                size_t opener_start = last_opener_start(p, strlen(tag->raw));
                ONodeList *body;
                ONode *typed_expr;
                if (!flush_text(nodes, p, text_start, opener_start)) {
                    free_tag(tag);
                    onode_list_free(nodes);
                    free(closer);
                    parser_set_error(p, "Out of memory");
                    return NULL;
                }
                body = parse_until(p, tag);
                if (body == NULL) {
                    free_tag(tag);
                    onode_list_free(nodes);
                    free(closer);
                    return NULL;
                }
                typed_expr = onode_new_typed_expr_owned(tag->lang, tag->env_id, tag->attr,
                                                        body->items, body->len, body->cap);
                free(body);
                free(tag->raw);
                free(tag);
                if (typed_expr == NULL || !onode_list_push(nodes, typed_expr)) {
                    onode_free(typed_expr);
                    onode_list_free(nodes);
                    free(closer);
                    parser_set_error(p, "Out of memory");
                    return NULL;
                }
                text_start = p->pos;
                continue;
            }
        }

        if (in_seq) {
            size_t stmt_start = p->pos;
            ONode *call = try_parse_call(p);
            if (parser_has_error(p)) {
                onode_list_free(nodes);
                free(closer);
                return NULL;
            }
            if (call != NULL) {
                if (!flush_text(nodes, p, text_start, stmt_start)) {
                    onode_free(call);
                    onode_list_free(nodes);
                    free(closer);
                    parser_set_error(p, "Out of memory");
                    return NULL;
                }
                if (!onode_list_push(nodes, call)) {
                    onode_free(call);
                    onode_list_free(nodes);
                    free(closer);
                    parser_set_error(p, "Out of memory");
                    return NULL;
                }
                text_start = p->pos;
                continue;
            }
        }

        advance_one_byte(p);
    }

    if (expected_closer != NULL) {
        parser_set_error(p, "Line %zu: Unclosed expression, expected )_%s",
                         p->line, expected_closer->raw);
        onode_list_free(nodes);
        free(closer);
        return NULL;
    }

    if (!flush_text(nodes, p, text_start, p->pos)) {
        onode_list_free(nodes);
        free(closer);
        parser_set_error(p, "Out of memory");
        return NULL;
    }

    free(closer);
    return nodes;
}

static void reconstruct_node(const ONode *node, StringBuilder *sb) {
    size_t i;
    char num_buf[32];

    if (node == NULL || sb == NULL) {
        return;
    }

    switch (node->tag) {
        case ONODE_RAW_TEXT:
            (void)string_builder_append(sb, node->data.text != NULL ? node->data.text : "");
            break;

        case ONODE_VAR_REF:
            (void)string_builder_append_char(sb, '$');
            (void)string_builder_append(sb, node->data.var_name != NULL ? node->data.var_name : "");
            break;

        case ONODE_LET_BINDING:
            (void)string_builder_append(sb, "let ");
            (void)string_builder_append(sb,
                                        node->data.let_binding.name != NULL
                                            ? node->data.let_binding.name
                                            : "");
            (void)string_builder_append(sb, " = ");
            reconstruct_node(node->data.let_binding.expr, sb);
            break;

        case ONODE_TYPED_EXPR:
            (void)string_builder_append(sb,
                                        node->data.typed_expr.lang != NULL
                                            ? node->data.typed_expr.lang
                                            : "");
            if (node->data.typed_expr.env_id != UINT32_MAX) {
                snprintf(num_buf, sizeof(num_buf), "%u", node->data.typed_expr.env_id);
                (void)string_builder_append_char(sb, '[');
                (void)string_builder_append(sb, num_buf);
                (void)string_builder_append_char(sb, ']');
            }
            if (node->data.typed_expr.attr != NULL) {
                (void)string_builder_append_char(sb, '{');
                (void)string_builder_append(sb, node->data.typed_expr.attr);
                (void)string_builder_append_char(sb, '}');
            }
            (void)string_builder_append(sb, "^(");
            for (i = 0; i < node->data.typed_expr.body_len; i++) {
                reconstruct_node(node->data.typed_expr.body[i], sb);
            }
            (void)string_builder_append_char(sb, ')');
            (void)string_builder_append_char(sb, '_');
            (void)string_builder_append(sb,
                                        node->data.typed_expr.lang != NULL
                                            ? node->data.typed_expr.lang
                                            : "");
            if (node->data.typed_expr.env_id != UINT32_MAX) {
                snprintf(num_buf, sizeof(num_buf), "%u", node->data.typed_expr.env_id);
                (void)string_builder_append_char(sb, '[');
                (void)string_builder_append(sb, num_buf);
                (void)string_builder_append_char(sb, ']');
            }
            if (node->data.typed_expr.attr != NULL) {
                (void)string_builder_append_char(sb, '{');
                (void)string_builder_append(sb, node->data.typed_expr.attr);
                (void)string_builder_append_char(sb, '}');
            }
            break;

        case ONODE_CALL:
            (void)string_builder_append(sb,
                                        node->data.call.fn_name != NULL
                                            ? node->data.call.fn_name
                                            : "");
            (void)string_builder_append_char(sb, '(');
            for (i = 0; i < node->data.call.args_len; i++) {
                if (i > 0) {
                    (void)string_builder_append(sb, ", ");
                }
                reconstruct_node(node->data.call.args[i], sb);
            }
            (void)string_builder_append_char(sb, ')');
            break;

        default:
            break;
    }
}
