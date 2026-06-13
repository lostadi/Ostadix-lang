#include "eval.h"

#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdbool.h>
#include <limits.h>

#include "value.h"
#include "parser.h"
#include "process.h"
#include "scheduler.h"
#include "nix_ops.h"

/* ── Local StringBuilder (mirrors one in value.c) ──────────────────────── */
typedef struct {
    char *buf;
    size_t len;
    size_t cap;
} SB;

static bool sb_init(SB *sb) {
    sb->buf = (char *)malloc(64);
    if (!sb->buf) return false;
    sb->buf[0] = 0;
    sb->len = 0;
    sb->cap = 64;
    return true;
}
static bool sb_reserve(SB *sb, size_t extra) {
    size_t need = sb->len + extra + 1;
    if (need <= sb->cap) return true;
    size_t nc = sb->cap ? sb->cap : 64;
    while (nc < need) nc *= 2;
    char *nb = (char *)realloc(sb->buf, nc);
    if (!nb) return false;
    sb->buf = nb;
    sb->cap = nc;
    return true;
}
static bool sb_append(SB *sb, const char *s) {
    if (!s) return true;
    size_t n = strlen(s);
    if (!sb_reserve(sb, n)) return false;
    memcpy(sb->buf + sb->len, s, n);
    sb->len += n;
    sb->buf[sb->len] = 0;
    return true;
}
static bool sb_append_c(SB *sb, char c) {
    if (!sb_reserve(sb, 1)) return false;
    sb->buf[sb->len++] = c;
    sb->buf[sb->len] = 0;
    return true;
}
static char *sb_take(SB *sb) {
    char *r = sb->buf;
    sb->buf = NULL;
    sb->len = sb->cap = 0;
    return r;
}

/* ── small utils ───────────────────────────────────────────────────────── */
static bool whitespace_only(const char *t) {
    if (!t) return true;
    for (const unsigned char *p = (const unsigned char *)t; *p; ++p) {
        if (!isspace(*p)) return false;
    }
    return true;
}

static char *dup_cstr(const char *s) {
    if (!s) return NULL;
    size_t n = strlen(s);
    char *c = (char *)malloc(n + 1);
    if (c) memcpy(c, s, n + 1);
    return c;
}

static bool is_pure_backend(const char *lang) {
    if (!lang) return false;
    const char *pures[] = {"html","markdown","md","latex","tex","text","plain","nix","nix_expr","nix_store","nixos_test", NULL};
    for (int i=0; pures[i]; ++i) if (strcmp(lang, pures[i])==0) return true;
    return false;
}

/* ── render helpers (C ports of render_python/html etc) ────────────────── */
static char *json_quote(const char *s) {
    if (!s) return strdup("\"\"");
    SB sb; if (!sb_init(&sb)) return strdup("\"\"");
    sb_append_c(&sb, '"');
    for (const unsigned char *p = (const unsigned char *)s; *p; ++p) {
        if (*p == '"' || *p == '\\') { sb_append_c(&sb, '\\'); sb_append_c(&sb, (char)*p); }
        else if (*p < 0x20) { char b[8]; snprintf(b,8,"\\u%04x", *p); sb_append(&sb, b); }
        else sb_append_c(&sb, (char)*p);
    }
    sb_append_c(&sb, '"');
    return sb_take(&sb);
}

static char *render_python(OValue *v) {
    if (!v) return strdup("None");
    switch (v->tag) {
        case OVAL_NULL: return strdup("None");
        case OVAL_BOOL: return strdup(v->data.bool_val ? "True" : "False");
        case OVAL_INT: { char b[32]; snprintf(b,32,"%lld", (long long)v->data.int_val); return strdup(b); }
        case OVAL_FLOAT: { char b[64]; snprintf(b,64,"%.17g", v->data.float_val); return strdup(b); }
        case OVAL_STR: case OVAL_SYSTEM: {
            char *q = json_quote(v->data.str_val ? v->data.str_val : "");
            /* python str */
            SB sb; sb_init(&sb); sb_append(&sb, q); free(q); return sb_take(&sb);
        }
        case OVAL_HTML: {
            /* Preserve the trusted-HTML type across the splice (matches the
               Rust runtime's render_python; the shim defines OHtml). */
            char *q = json_quote(v->data.str_val ? v->data.str_val : "");
            SB sb; sb_init(&sb); sb_append(&sb, "OHtml("); sb_append(&sb, q);
            sb_append(&sb, ")"); free(q); return sb_take(&sb);
        }
        case OVAL_STORE_PATH: {
            char *q = json_quote(v->data.str_val ? v->data.str_val : "");
            SB sb; sb_init(&sb); sb_append(&sb, "OStorePath("); sb_append(&sb, q);
            sb_append(&sb, ")"); free(q); return sb_take(&sb);
        }
        case OVAL_LIST: {
            SB sb; sb_init(&sb); sb_append(&sb, "[");
            for (size_t i=0; i < v->data.list.len; ++i) {
                if (i) sb_append(&sb, ", ");
                char *r = render_python(v->data.list.items[i]);
                sb_append(&sb, r); free(r);
            }
            sb_append(&sb, "]");
            return sb_take(&sb);
        }
        case OVAL_MAP: {
            SB sb; sb_init(&sb); sb_append(&sb, "{");
            /* iterate buckets simply */
            OValueMap *m = v->data.map;
            bool first = true;
            for (size_t b=0; b < m->bucket_count; ++b) {
                for (OMapEntry *e = m->buckets[b]; e; e = e->next) {
                    if (!first) sb_append(&sb, ", ");
                    first = false;
                    char *k = json_quote(e->key ? e->key : "");
                    sb_append(&sb, k); free(k);
                    sb_append(&sb, ": ");
                    char *rv = render_python(e->value);
                    sb_append(&sb, rv); free(rv);
                }
            }
            sb_append(&sb, "}");
            return sb_take(&sb);
        }
        case OVAL_BLOB: {
            SB sb; sb_init(&sb);
            sb_append(&sb, "OBlob(data=base64.b64decode('");
            sb_append(&sb, v->data.blob.data ? v->data.blob.data : "");
            sb_append(&sb, "'), mime='");
            sb_append(&sb, v->data.blob.mime ? v->data.blob.mime : "");
            sb_append(&sb, "')");
            return sb_take(&sb);
        }
        case OVAL_EXPR: {
            SB sb; sb_init(&sb);
            sb_append(&sb, "OExprValue(");
            char *q = json_quote(v->data.str_val ? v->data.str_val : "");
            sb_append(&sb, q); free(q);
            sb_append(&sb, ")");
            return sb_take(&sb);
        }
        case OVAL_NIX_EXPR: {
            SB sb; sb_init(&sb); sb_append(&sb, "ONixExpr('");
            sb_append(&sb, v->data.nix_expr.body ? v->data.nix_expr.body : "");
            sb_append(&sb, "')");
            return sb_take(&sb);
        }
        case OVAL_DERIVATION: {
            SB sb; sb_init(&sb); sb_append(&sb, "ODerivation('");
            sb_append(&sb, v->data.derivation.drv_path ? v->data.derivation.drv_path : "");
            sb_append(&sb, "')");
            return sb_take(&sb);
        }
        case OVAL_REQUEST: case OVAL_THUNK: {
            return strdup("ORequest(...)");
        }
        default: return strdup("OValue(...)");
    }
}

static char *html_escape_c(const char *s) {
    if (!s) return strdup("");
    SB sb; sb_init(&sb);
    for (const unsigned char *p=(const unsigned char*)s; *p; ++p) {
        if (*p=='&') sb_append(&sb, "&amp;");
        else if (*p=='<') sb_append(&sb, "&lt;");
        else if (*p=='>') sb_append(&sb, "&gt;");
        else if (*p=='"') sb_append(&sb, "&quot;");
        else sb_append_c(&sb, (char)*p);
    }
    return sb_take(&sb);
}

static char *render_html(OValue *v) {
    if (!v) return strdup("");
    switch (v->tag) {
        case OVAL_STR:
            /* Untrusted text — escape. Trusted raw HTML must be OVAL_HTML. */
            return html_escape_c(v->data.str_val ? v->data.str_val : "");
        case OVAL_HTML: case OVAL_STORE_PATH:
            return strdup(v->data.str_val ? v->data.str_val : "");
        case OVAL_BLOB: {
            const char *d = v->data.blob.data ? v->data.blob.data : "";
            const char *m = v->data.blob.mime ? v->data.blob.mime : "application/octet-stream";
            SB sb; sb_init(&sb);
            if (strncmp(m, "image/", 6)==0) {
                sb_append(&sb, "<img src=\"data:"); sb_append(&sb, m);
                sb_append(&sb, ";base64,"); sb_append(&sb, d); sb_append(&sb, "\" />");
            } else {
                sb_append(&sb, "<pre data-mime=\""); sb_append(&sb, m); sb_append(&sb, "\">");
                sb_append(&sb, d); sb_append(&sb, "</pre>");
            }
            return sb_take(&sb);
        }
        case OVAL_LIST: {
            SB sb; sb_init(&sb); sb_append(&sb, "<ul>");
            for (size_t i=0; i<v->data.list.len; ++i) {
                sb_append(&sb, "<li>");
                char *c = render_html(v->data.list.items[i]); sb_append(&sb, c); free(c);
                sb_append(&sb, "</li>");
            }
            sb_append(&sb, "</ul>");
            return sb_take(&sb);
        }
        case OVAL_MAP: {
            SB sb; sb_init(&sb); sb_append(&sb, "<div class=\"o-map\">");
            OValueMap *m = v->data.map;
            for (size_t b=0; b<m->bucket_count; ++b) for (OMapEntry *e=m->buckets[b]; e; e=e->next) {
                sb_append(&sb, "<div data-o-key=\"");
                char *ek = html_escape_c(e->key ? e->key : ""); sb_append(&sb, ek); free(ek);
                sb_append(&sb, "\">");
                char *rv = render_html(e->value); sb_append(&sb, rv); free(rv);
                sb_append(&sb, "</div>");
            }
            sb_append(&sb, "</div>");
            return sb_take(&sb);
        }
        default: {
            char *r = oval_splice_repr(v);
            char *h = html_escape_c(r ? r : "");
            free(r);
            return h;
        }
    }
}

static char *render_markdown_or_text(OValue *v) {
    if (!v) return strdup("");
    if (v->tag == OVAL_STR || v->tag == OVAL_HTML) return strdup(v->data.str_val ? v->data.str_val : "");
    char *r = oval_splice_repr(v);
    return r ? r : strdup("");
}

static char *render_latex(OValue *v) { return render_markdown_or_text(v); }

static char *render_nix(OValue *v) {
    if (!v) return strdup("");
    if (v->tag == OVAL_STR || v->tag == OVAL_STORE_PATH) return strdup(v->data.str_val ? v->data.str_val : "");
    char *r = oval_splice_repr(v);
    return r ? r : strdup("");
}

char *olang_render_child(const char *lang, OValue *val) {
    if (!lang) return oval_splice_repr(val);
    if (strcmp(lang, "python") == 0 || strcmp(lang, "py") == 0) return render_python(val);
    if (strcmp(lang, "html") == 0) return render_html(val);
    if (strcmp(lang, "markdown") == 0 || strcmp(lang, "md") == 0) return render_markdown_or_text(val);
    if (strcmp(lang, "latex") == 0 || strcmp(lang, "tex") == 0) return render_latex(val);
    if (strcmp(lang, "text") == 0 || strcmp(lang, "plain") == 0) return render_markdown_or_text(val);
    if (strncmp(lang, "nix", 3) == 0) return render_nix(val);
    return oval_splice_repr(val);
}

/* ── Evaluator struct ──────────────────────────────────────────────────── */
typedef struct ScopeEntry {
    char *name;
    OValue *val;
    struct ScopeEntry *next;
} ScopeEntry;

struct OEvaluator {
    OProcessRegistry *registry;
    char *shim_dir;
    StringSet *registered; /* owned copy */
    OPolicy policy;
    OAutonomousScheduler *scheduler;
    OValue **autonomous_buffer;
    size_t ab_len, ab_cap;
    /* simple top-level scope list for lets */
    ScopeEntry *scope_head;
};

static void eval_release_scope(OEvaluator *ev) {
    ScopeEntry *e = ev->scope_head; ev->scope_head = NULL;
    while (e) {
        ScopeEntry *n = e->next;
        free(e->name);
        oval_release(e->val);
        free(e);
        e = n;
    }
}

OEvaluator *olang_evaluator_new(const char *shim_dir) {
    OEvaluator *ev = (OEvaluator *)calloc(1, sizeof(*ev));
    ev->registry = olang_process_registry_new();
    ev->shim_dir = strdup(shim_dir ? shim_dir : "backends");
    ev->policy = POLICY_EAGER;
    ev->scheduler = olang_autonomous_scheduler_new();
    /* registered filled later */
    return ev;
}

void olang_evaluator_free(OEvaluator *ev) {
    if (!ev) return;
    olang_process_registry_free(ev->registry);
    free(ev->shim_dir);
    if (ev->registered) string_set_free(ev->registered);
    olang_autonomous_scheduler_free(ev->scheduler);
    for (size_t i=0; i<ev->ab_len; ++i) oval_release(ev->autonomous_buffer[i]);
    free(ev->autonomous_buffer);
    eval_release_scope(ev);
    free(ev);
}

void olang_evaluator_set_registered(OEvaluator *ev, const StringSet *backends) {
    if (!ev) return;
    if (ev->registered) string_set_free(ev->registered);
    ev->registered = string_set_new();
    if (!backends) {
        const char *defs[] = {"O","python","html","markdown","latex","text","quote",
                              "nix","nix_expr","nix_store","nixos_test","bash","shell","rust","racket", NULL};
        for (int i=0; defs[i]; ++i) string_set_add(ev->registered, defs[i]);
        return;
    }
    for (size_t i=0; i<backends->len; ++i) string_set_add(ev->registered, backends->items[i]);
}

char *olang_find_shim(OEvaluator *ev, const char *lang) {
    if (!ev || !lang) return NULL;
    const char *cands[4] = {NULL, NULL, NULL, NULL};
    char buf[256];
    snprintf(buf, sizeof(buf), "%s_shim.py", lang); cands[0] = buf; /* but need allocs */
    /* simple: try in order, return first existing or default guess */
    /* For MVP use direct guess under shim_dir */
    SB p; sb_init(&p);
    sb_append(&p, ev->shim_dir); sb_append_c(&p, '/'); sb_append(&p, lang); sb_append(&p, "_shim.py");
    /* existence check omitted for speed; return the py guess */
    char *res = sb_take(&p);
    /* caller frees */
    return res;
}

/* scope helpers for top level lets + $var (very small N) */
static OValue *scope_lookup(OEvaluator *ev, const char *name) {
    for (ScopeEntry *e = ev->scope_head; e; e = e->next) {
        if (strcmp(e->name, name) == 0) return oval_retain(e->val);
    }
    return NULL;
}
static void scope_bind(OEvaluator *ev, const char *name, OValue *val) {
    ScopeEntry *e = (ScopeEntry *)calloc(1, sizeof(*e));
    e->name = strdup(name);
    e->val = oval_retain(val);
    e->next = ev->scope_head;
    ev->scope_head = e;
}

/* ── core eval ─────────────────────────────────────────────────────────── */

static OValue *eval_node(OEvaluator *ev, ONode *node, bool *err);

static OValue *eval_call(OEvaluator *ev, const char *fn, ONode **args, size_t nargs, bool *err) {
    if (!fn) { *err = true; return NULL; }
    if (strcmp(fn, "now") == 0) {
        if (nargs != 1) { *err=true; return NULL; }
        OValue *r = eval_node(ev, args[0], err);
        if (*err || !oval_is_request(r)) { oval_release(r); *err=true; return NULL; }
        /* force via scheduler or direct */
        OValue *out = olang_scheduler_execute(ev->scheduler, r);
        oval_release(r);
        return out;
    }
    if (strcmp(fn, "lazy") == 0) {
        if (nargs != 1) { *err=true; return NULL; }
        OPolicy old = ev->policy;
        ev->policy = POLICY_LAZY;
        OValue *v = eval_node(ev, args[0], err);
        ev->policy = old;
        return v;
    }
    if (strcmp(fn, "autonomous") == 0) {
        if (nargs != 1) { *err=true; return NULL; }
        OPolicy old = ev->policy;
        ev->policy = POLICY_AUTONOMOUS;
        OValue *v = eval_node(ev, args[0], err);
        ev->policy = old;
        /* flush buffer serial */
        for (size_t i=0; i<ev->ab_len; ++i) {
            OValue *f = olang_scheduler_execute(ev->scheduler, ev->autonomous_buffer[i]);
            oval_release(f);
            oval_release(ev->autonomous_buffer[i]);
        }
        ev->ab_len = 0;
        return v;
    }
    if (strcmp(fn, "instantiate") == 0) {
        if (nargs != 1) { *err=true; return NULL; }
        OValue *a = eval_node(ev, args[0], err);
        if (*err) { oval_release(a); return NULL; }
        OValue *req = oval_request((RequestKind){REQ_INSTANTIATE, NULL, 0, false, NULL, false}, a);
        oval_release(a);
        OValue *out = olang_scheduler_execute(ev->scheduler, req);
        oval_release(req);
        return out;
    }
    if (strcmp(fn, "realise") == 0) {
        if (nargs != 1) { *err=true; return NULL; }
        OValue *a = eval_node(ev, args[0], err);
        if (*err) { oval_release(a); return NULL; }
        OValue *req = oval_request((RequestKind){REQ_REALISE, NULL, 0, false, NULL, false}, a);
        oval_release(a);
        OValue *out = olang_scheduler_execute(ev->scheduler, req);
        oval_release(req);
        return out;
    }
    if (strcmp(fn, "activate") == 0) {
        if (nargs < 1 || nargs > 2) { *err=true; return NULL; }
        OValue *a0 = eval_node(ev, args[0], err);
        if (*err) { oval_release(a0); return NULL; }
        const char *prof = "/nix/var/nix/profiles/system";
        if (nargs == 2) {
            OValue *a1 = eval_node(ev, args[1], err);
            if (!*err && a1 && (a1->tag == OVAL_STR || a1->tag == OVAL_SYSTEM) && a1->data.str_val)
                prof = a1->data.str_val;
            oval_release(a1);
        }
        OValue *req = oval_request((RequestKind){REQ_ACTIVATE, NULL, 0, false, dup_cstr(prof), true}, a0);
        oval_release(a0);
        OValue *out = olang_scheduler_execute(ev->scheduler, req);
        oval_release(req);
        return out;
    }
    if (strcmp(fn, "current_system") == 0) {
        return olang_current_system();
    }
    fprintf(stderr, "unknown builtin: %s(...)\n", fn);
    *err = true;
    return NULL;
}

static OValue *eval_typed_expr(OEvaluator *ev, const char *lang, uint32_t env_id,
                               const char *attr, ONode **body, size_t body_len, bool *err) {
    if (strcmp(lang, "quote") == 0) {
        char *src = reconstruct_source(body, body_len);
        OValue *v = oval_expr(src ? src : "");
        free(src);
        return v;
    }

    if (attr) {
        if (strcmp(attr, "lazy") == 0 || strcmp(attr, "defer") == 0) {
            /* build thunk + request */
            SB buf; sb_init(&buf);
            for (size_t i=0; i<body_len; ++i) {
                ONode *ch = body[i];
                if (ch->tag == ONODE_RAW_TEXT) sb_append(&buf, ch->data.text ? ch->data.text : "");
                /* (simplified: ignore nested for attr thunks in MVP) */
            }
            char *b = sb_take(&buf);
            OValue *th = oval_thunk(b, NULL, 0);
            free(b);
            bool cache = (strcmp(attr, "lazy") == 0) && is_pure_backend(lang);
            RequestKind k = {REQ_EVAL, dup_cstr(lang), env_id, cache, NULL, false};
            OValue *req = oval_request(k, th);
            oval_release(th);
            return req;
        }
    }

    if (strcmp(lang, "O") == 0) {
        OValue *last = oval_null();
        for (size_t i = 0; i < body_len; ++i) {
            ONode *ch = body[i];
            if (ch->tag == ONODE_RAW_TEXT && whitespace_only(ch->data.text)) continue;
            OValue *v = NULL;
            switch (ch->tag) {
                case ONODE_RAW_TEXT: v = oval_str(ch->data.text ? ch->data.text : ""); break;
                case ONODE_VAR_REF: v = scope_lookup(ev, ch->data.var_name ? ch->data.var_name : ""); break;
                case ONODE_TYPED_EXPR:
                    v = eval_typed_expr(ev, ch->data.typed_expr.lang, ch->data.typed_expr.env_id,
                                        ch->data.typed_expr.attr, ch->data.typed_expr.body,
                                        ch->data.typed_expr.body_len, err);
                    break;
                case ONODE_CALL:
                    v = eval_call(ev, ch->data.call.fn_name, ch->data.call.args, ch->data.call.args_len, err);
                    break;
                default: break;
            }
            if (v && !oval_is_null(v)) {
                oval_release(last);
                last = oval_retain(v);
            }
            oval_release(v);
            if (*err) break;
        }
        return last;
    }

    /* default splice */
    SB buf; sb_init(&buf);
    for (size_t i=0; i<body_len; ++i) {
        ONode *ch = body[i];
        if (ch->tag == ONODE_RAW_TEXT) {
            sb_append(&buf, ch->data.text ? ch->data.text : "");
            continue;
        }
        if (ch->tag == ONODE_VAR_REF) {
            OValue *val = scope_lookup(ev, ch->data.var_name ? ch->data.var_name : "");
            if (val) {
                char *r = olang_render_child(lang, val);
                sb_append(&buf, r); free(r);
                oval_release(val);
            } else {
                sb_append(&buf, "$"); sb_append(&buf, ch->data.var_name ? ch->data.var_name : "");
            }
            continue;
        }
        if (ch->tag == ONODE_TYPED_EXPR) {
            OValue *cv = eval_typed_expr(ev, ch->data.typed_expr.lang, ch->data.typed_expr.env_id,
                                         ch->data.typed_expr.attr, ch->data.typed_expr.body,
                                         ch->data.typed_expr.body_len, err);
            if (cv) {
                char *r = olang_render_child(lang, cv);
                sb_append(&buf, r); free(r);
                oval_release(cv);
            }
            continue;
        }
        if (ch->tag == ONODE_CALL) {
            OValue *cv = eval_call(ev, ch->data.call.fn_name, ch->data.call.args, ch->data.call.args_len, err);
            if (cv) {
                char *r = olang_render_child(lang, cv);
                sb_append(&buf, r); free(r);
                oval_release(cv);
            }
            continue;
        }
    }

    char *bodystr = sb_take(&buf);

    if (strcmp(lang, "html") == 0) {
        OValue *h = oval_html(bodystr);
        free(bodystr);
        return h;
    }
    if (strcmp(lang, "markdown") == 0 || strcmp(lang, "md") == 0 ||
        strcmp(lang, "text") == 0 || strcmp(lang, "plain") == 0 ||
        strcmp(lang, "latex") == 0 || strcmp(lang, "tex") == 0) {
        OValue *s = oval_str(bodystr);
        free(bodystr);
        return s;
    }
    if (strcmp(lang, "nix_expr") == 0) {
        OValue *nx = oval_nix_expr(bodystr, NULL, 0);
        free(bodystr);
        return nx;
    }

    /* dispatch to shim via registry using full loop (supports O.eval reentrancy for python) */
    OValue *bmapv = oval_map();
    OValueMap *bindings = bmapv ? bmapv->data.map : NULL;
    char *shim = olang_find_shim(ev, lang);
    olang_process_registry_send_exec(ev->registry, lang, env_id, bodystr, bindings, shim ? shim : "backends/python_shim.py");
    OValue *result = NULL;
    while (1) {
        OExecStep step = olang_process_registry_recv_exec_step(ev->registry, lang, env_id);
        if (step.kind == EXEC_STEP_DONE) {
            result = step.value;
            step.value = NULL;
            oexec_step_free(&step);
            break;
        }
        /* EVAL_REQUEST: re-enter */
        OValue *inner = olang_eval_source(ev, step.src ? step.src : "");
        olang_process_registry_send_eval_result(ev->registry, lang, env_id, inner);
        oval_release(inner);
        oexec_step_free(&step);
    }
    if (env_id == UINT32_MAX) {
        olang_process_registry_cleanup_env(ev->registry, lang, UINT32_MAX);
    }
    free(bodystr);
    free(shim);
    oval_release(bmapv);
    if (env_id == UINT32_MAX) {
        olang_process_registry_cleanup_env(ev->registry, lang, UINT32_MAX);
    }
    return result ? result : oval_null();
}

static OValue *eval_node(OEvaluator *ev, ONode *node, bool *err) {
    if (!node) return oval_null();
    switch (node->tag) {
        case ONODE_RAW_TEXT:
            return oval_str(node->data.text ? node->data.text : "");
        case ONODE_VAR_REF:
            return scope_lookup(ev, node->data.var_name ? node->data.var_name : "");
        case ONODE_LET_BINDING: {
            OValue *v = eval_node(ev, node->data.let_binding.expr, err);
            if (!*err && node->data.let_binding.name) scope_bind(ev, node->data.let_binding.name, v);
            return v;
        }
        case ONODE_TYPED_EXPR:
            return eval_typed_expr(ev, node->data.typed_expr.lang, node->data.typed_expr.env_id,
                                   node->data.typed_expr.attr, node->data.typed_expr.body,
                                   node->data.typed_expr.body_len, err);
        case ONODE_CALL:
            return eval_call(ev, node->data.call.fn_name, node->data.call.args,
                             node->data.call.args_len, err);
        default:
            return oval_null();
    }
}

OValue *olang_evaluator_eval_document(OEvaluator *ev, ONodeList *nodes) {
    if (!ev || !nodes) return oval_null();
    bool err = false;
    OValue *last = oval_null();

    /* first pass: bind top lets */
    for (size_t i=0; i<nodes->len; ++i) {
        ONode *n = nodes->items[i];
        if (n && n->tag == ONODE_LET_BINDING) {
            bool e2 = false;
            OValue *v = eval_node(ev, n, &e2);
            if (!e2 && n->data.let_binding.name) scope_bind(ev, n->data.let_binding.name, v);
            oval_release(v);
        }
    }
    /* second: eval non-lets */
    for (size_t i=0; i<nodes->len; ++i) {
        ONode *n = nodes->items[i];
        if (!n) continue;
        if (n->tag == ONODE_LET_BINDING) continue;
        bool e2 = false;
        OValue *v = eval_node(ev, n, &e2);
        if (e2) { err = true; oval_release(v); break; }
        if (v && !oval_is_null(v)) {
            /* skip pure whitespace raw text at top level (formatting) */
            int skip = 0;
            if (v->tag == OVAL_STR && v->data.str_val && whitespace_only(v->data.str_val)) skip = 1;
            if (!skip) {
                oval_release(last);
                last = oval_retain(v);
            }
        }
        oval_release(v);
    }
    if (err) {
        oval_release(last);
        return oval_null();
    }
    return last;
}

OValue *olang_eval_source(OEvaluator *ev, const char *src) {
    if (!ev || !src) return oval_null();
    StringSet *bs = ev->registered ? ev->registered : NULL;
    OParser p;
    parser_init(&p, src, bs);
    ONodeList *nodes = parser_parse(&p);
    if (!nodes) {
        fprintf(stderr, "eval_source parse error: %s\n", p.error_msg);
        return oval_null();
    }
    OValue *r = olang_evaluator_eval_document(ev, nodes);
    onode_list_free(nodes);
    return r;
}
