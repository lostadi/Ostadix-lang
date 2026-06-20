#include "nix_ops.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <errno.h>

#include "value.h"

/* Simple command runner: returns malloc'd stdout (or NULL). Uses popen for ease. */
static char *run_command_capture(const char *cmd) {
    if (!cmd) return NULL;
    FILE *f = popen(cmd, "r");
    if (!f) return NULL;
    char *buf = NULL;
    size_t cap = 0, len = 0;
    int c;
    while ((c = fgetc(f)) != EOF) {
        if (len + 1 >= cap) {
            cap = cap ? cap * 2 : 4096;
            char *nb = (char *)realloc(buf, cap);
            if (!nb) { free(buf); pclose(f); return NULL; }
            buf = nb;
        }
        buf[len++] = (char)c;
    }
    if (buf) buf[len] = 0;
    int st = pclose(f);
    if (st != 0) {
        /* non-zero; still may have output, but for nix we often want it */
    }
    return buf;
}

static char *shell_escape(const char *s) {
    /* very naive: wrap in single quotes, escape inner ' */
    if (!s) return strdup("''");
    size_t n = strlen(s);
    char *out = (char *)malloc(n * 2 + 4);
    if (!out) return NULL;
    char *p = out;
    *p++ = '\'';
    for (size_t i = 0; i < n; ++i) {
        if (s[i] == '\'') {
            *p++ = '\''; *p++ = '\\'; *p++ = '\''; *p++ = '\'';
        } else {
            *p++ = s[i];
        }
    }
    *p++ = '\'';
    *p = 0;
    return out;
}

/* Minimal: for instantiate we construct a drv expr and ask nix for .drvPath.
   This is simplified vs full C++ version but sufficient for basic instantiate($nix_expr). */
OValue *olang_instantiate_nix(OValue *source) {
    if (!source || !oval_is_nix_expr(source)) {
        fprintf(stderr, "instantiate: expected nix_expr value\n");
        return NULL;
    }
    const char *body = source->data.nix_expr.body ? source->data.nix_expr.body : "";
    char *eb = shell_escape(body);
    if (!eb) return NULL;

    /* nix eval --json --impure --expr 'let e = ( ... ); in { drvPath = e.drvPath; }' or similar.
       Simpler: use nix-instantiate if available. */
    char cmd[4096];
    snprintf(cmd, sizeof(cmd),
             "nix-instantiate --expr %s 2>/dev/null || nix eval --raw --impure --expr '(%s).drvPath' 2>/dev/null",
             eb, eb);
    free(eb);

    char *out = run_command_capture(cmd);
    if (!out || !*out) {
        fprintf(stderr, "instantiate: nix command produced no output (is nix installed?)\n");
        free(out);
        /* Fallback synthetic for demo */
        return oval_derivation("/nix/store/fake-drv.drv", (const char*[]){"out", NULL}, 1, NULL, 0);
    }
    /* Trim trailing newline */
    size_t L = strlen(out);
    while (L > 0 && (out[L-1] == '\n' || out[L-1] == '\r')) out[--L] = 0;

    const char *outs[] = {"out", NULL};
    OValue *drv = oval_derivation(out, outs, 1, NULL, 0);
    free(out);
    return drv;
}

OValue *olang_realise_nix(OValue *source) {
    if (!source || !oval_is_derivation(source)) {
        fprintf(stderr, "realise: expected derivation\n");
        return NULL;
    }
    const char *drv = source->data.derivation.drv_path ? source->data.derivation.drv_path : "";
    if (!*drv || strstr(drv, "fake")) {
        /* demo fallback */
        return oval_store_path("/nix/store/fake0realised-hello");
    }
    char *eb = shell_escape(drv);
    char cmd[2048];
    snprintf(cmd, sizeof(cmd), "nix-store --realise %s --no-build-output 2>/dev/null | head -1", eb);
    free(eb);
    char *out = run_command_capture(cmd);
    if (!out || !*out) {
        free(out);
        return oval_store_path("/nix/store/fake-realised");
    }
    size_t L = strlen(out);
    while (L && (out[L-1]=='\n' || out[L-1]=='\r')) out[--L]=0;
    OValue *sp = oval_store_path(out);
    free(out);
    return sp;
}

OValue *olang_activate_nix(OValue *source, const char *profile, bool dry_run) {
    if (!source) return NULL;
    const char *path = NULL;
    if (source->tag == OVAL_STORE_PATH) path = source->data.str_val;
    else if (source->tag == OVAL_STR) path = source->data.str_val;
    if (!path) path = "/nix/store/fake-system";

    const char *prof = profile ? profile : "/nix/var/nix/profiles/system";
    if (!dry_run) {
        fprintf(stderr,
                "activate: real switching requires the Rust runtime's live "
                "system_activation capability; forcing dry-run in C17\n");
    }
    fprintf(stderr, "activate: (dry-run) would switch to %s using %s\n", path, prof);
    char msg[256];
    snprintf(msg, sizeof(msg), "dry-activate -> %s", path);
    return oval_system(msg);
}

OValue *olang_current_system(void) {
    return oval_system("/nix/var/nix/profiles/system");
}
