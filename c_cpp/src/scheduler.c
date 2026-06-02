#include "scheduler.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/stat.h>
#include <errno.h>

#include "value.h"
#include "nix_ops.h"

/* Very small DiskCache using $HOME/.cache/o-lang or /tmp.
   Files are <fp>.json using oval_to/from_json. Serial only for MVP. */

struct ODiskCache {
    char *dir;
};

static void mkdir_p(const char *path) {
    if (!path) return;
    char tmp[1024];
    snprintf(tmp, sizeof(tmp), "%s", path);
    for (char *p = tmp + 1; *p; ++p) {
        if (*p == '/') {
            *p = 0;
            mkdir(tmp, 0755);
            *p = '/';
        }
    }
    mkdir(tmp, 0755);
}

ODiskCache *olang_disk_cache_new(const char *dir) {
    if (!dir) dir = "/tmp/o-lang-cache";
    ODiskCache *dc = (ODiskCache *)calloc(1, sizeof(*dc));
    dc->dir = strdup(dir);
    mkdir_p(dc->dir);
    return dc;
}

void olang_disk_cache_free(ODiskCache *dc) {
    if (!dc) return;
    free(dc->dir);
    free(dc);
}

char *olang_disk_cache_default_dir(void) {
    const char *home = getenv("HOME");
    const char *xdg = getenv("XDG_CACHE_HOME");
    char buf[512];
    if (xdg && *xdg) {
        snprintf(buf, sizeof(buf), "%s/o-lang", xdg);
    } else if (home && *home) {
        snprintf(buf, sizeof(buf), "%s/.cache/o-lang", home);
    } else {
        snprintf(buf, sizeof(buf), "/tmp/o-lang-cache-%ld", (long)getuid());
    }
    return strdup(buf);
}

OValue *olang_disk_cache_get(ODiskCache *dc, const char *fingerprint) {
    if (!dc || !fingerprint) return NULL;
    char path[1024];
    snprintf(path, sizeof(path), "%s/%s.json", dc->dir, fingerprint);
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz <= 0 || sz > (1<<20)) { fclose(f); return NULL; }
    char *buf = (char *)malloc((size_t)sz + 1);
    if (!buf) { fclose(f); return NULL; }
    size_t n = fread(buf, 1, (size_t)sz, f);
    fclose(f);
    buf[n] = 0;
    OValue *v = oval_from_json(buf);
    free(buf);
    return v; /* already has refcount 1 */
}

void olang_disk_cache_put(ODiskCache *dc, const char *fingerprint, OValue *value) {
    if (!dc || !fingerprint || !value) return;
    char path[1024];
    snprintf(path, sizeof(path), "%s/%s.json", dc->dir, fingerprint);
    char *json = oval_to_json(value);
    if (!json) return;
    FILE *f = fopen(path, "wb");
    if (f) {
        fwrite(json, 1, strlen(json), f);
        fclose(f);
    }
    free(json);
}

/* ── Minimal AutonomousScheduler (serial, in-mem + optional disk) ─────── */

struct OAutonomousScheduler {
    OValueMap *mem_cache; /* fp -> OValue (we retain) */
    ODiskCache *disk;
    size_t parallelism; /* ignored for now; serial */
};

OAutonomousScheduler *olang_autonomous_scheduler_new(void) {
    return olang_autonomous_scheduler_new_with_dir(NULL);
}

OAutonomousScheduler *olang_autonomous_scheduler_new_with_dir(const char *cache_dir) {
    OAutonomousScheduler *s = (OAutonomousScheduler *)calloc(1, sizeof(*s));
    OValue *mcv = oval_map(); s->mem_cache = mcv ? mcv->data.map : NULL;
    if (cache_dir) {
        s->disk = olang_disk_cache_new(cache_dir);
    } else {
        char *d = olang_disk_cache_default_dir();
        s->disk = olang_disk_cache_new(d);
        free(d);
    }
    s->parallelism = 1;
    return s;
}

void olang_autonomous_scheduler_free(OAutonomousScheduler *sch) {
    if (!sch) return;
    /* Note: we do not release values in mem_cache here (ownership is external via retain) */
    if (sch->mem_cache) oval_release((OValue *)sch->mem_cache); /* map itself */
    if (sch->disk) olang_disk_cache_free(sch->disk);
    free(sch);
}

void olang_autonomous_scheduler_set_parallelism(OAutonomousScheduler *sch, size_t n) {
    if (sch) sch->parallelism = n ? n : 1;
}

OValue *olang_scheduler_cache_get(OAutonomousScheduler *sch, const char *fingerprint) {
    if (!sch || !fingerprint) return NULL;
    OValue *v = oval_map_get((OValue *)sch->mem_cache, fingerprint);
    if (v) return oval_retain(v);
    if (sch->disk) {
        v = olang_disk_cache_get(sch->disk, fingerprint);
        if (v) {
            oval_map_set((OValue *)sch->mem_cache, fingerprint, oval_retain(v));
            return v; /* disk already gave +1 */
        }
    }
    return NULL;
}

static void scheduler_cache_put(OAutonomousScheduler *sch, const char *fp, OValue *v) {
    if (!sch || !fp || !v) return;
    oval_map_set((OValue *)sch->mem_cache, fp, oval_retain(v));
    if (sch->disk) olang_disk_cache_put(sch->disk, fp, v);
}

/* For MVP: immediate execution of requests by calling the nix fns directly.
   Non-Nix requests are left to caller (eval). */
OValue *olang_scheduler_execute(OAutonomousScheduler *sch, OValue *req) {
    if (!oval_is_request(req)) return oval_retain(req);

    /* Check cache by fingerprint if present */
    char *fp = oval_content_identity(req);
    if (fp) {
        OValue *hit = olang_scheduler_cache_get(sch, fp);
        if (hit) {
            free(fp);
            return hit;
        }
    }

    OValue *result = NULL;
    RequestKind *rk = &req->data.request.kind; /* internal access, same layout */
    (void)rk; /* use tag via predicates */

    if (oval_is_nix_expr(req->data.request.source)) {
        result = olang_instantiate_nix(req->data.request.source);
    } else if (oval_is_derivation(req->data.request.source)) {
        result = olang_realise_nix(req->data.request.source);
    } else if (oval_is_system(req->data.request.source) || req->data.request.source->tag == OVAL_STORE_PATH) {
        /* activate path */
        const char *profile = NULL;
        if (rk && rk->profile) profile = rk->profile;
        result = olang_activate_nix(req->data.request.source, profile, rk ? rk->dry_run : true);
    } else {
        /* EVAL or unknown: return as-is (caller handles via exec) */
        result = oval_retain(req);
    }

    if (result && fp) {
        scheduler_cache_put(sch, fp, result);
    }
    free(fp);
    return result ? result : oval_retain(req);
}

/* Batch: serial for now. Populates out_results map (fp -> result). */
int olang_scheduler_execute_batch(OAutonomousScheduler *sch,
                                  OValue **roots, size_t roots_len,
                                  OEvalFn eval_fn, void *eval_user,
                                  OValueMap **out_results) {
    if (!out_results) return 0;
    OValue *resv = oval_map(); OValueMap *res = resv ? resv->data.map : NULL;
    for (size_t i = 0; i < roots_len; ++i) {
        OValue *r = roots[i];
        if (!r) continue;
        char *fp = oval_content_identity(r);
        OValue *out = NULL;
        if (oval_is_request(r) && eval_fn && r->data.request.kind.tag == REQ_EVAL) {
            out = eval_fn(r, eval_user);
        } else {
            out = olang_scheduler_execute(sch, r);
        }
        if (fp && out) {
            oval_map_set(resv, fp, oval_retain(out));
        }
        if (out) oval_release(out);
        free(fp);
    }
    *out_results = res;
    return 1;
}

void olang_collect_transitive_requests(OValue *req, OValueMap *out_map) {
    if (!req || !out_map) return;
    if (!oval_is_request(req) && !oval_is_thunk(req) && !oval_is_nix_expr(req)) return;
    char *fp = oval_content_identity(req);
    if (fp) {
        if (!oval_map_get((OValue *)out_map, fp)) {
            oval_map_set((OValue *)out_map, fp, oval_retain(req));
        }
        free(fp);
    }
    /* Walk deps for Nix/Thunk/Request */
    if (req->tag == OVAL_NIX_EXPR || req->tag == OVAL_THUNK) {
        size_t n = req->data.nix_expr.deps_len;
        for (size_t i = 0; i < n; ++i) {
            olang_collect_transitive_requests(req->data.nix_expr.deps[i], out_map);
        }
    } else if (req->tag == OVAL_REQUEST) {
        if (req->data.request.source) {
            olang_collect_transitive_requests(req->data.request.source, out_map);
        }
    } else if (req->tag == OVAL_DERIVATION) {
        size_t n = req->data.derivation.deps_len;
        for (size_t i = 0; i < n; ++i) {
            olang_collect_transitive_requests(req->data.derivation.deps[i], out_map);
        }
    }
}
