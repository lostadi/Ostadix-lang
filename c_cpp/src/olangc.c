/* olangc — AOT compiler for O-lang (pure C edition)
 *
 *   ./olangc ../examples/hello.O
 *   ./olangc ../examples/hello.O -o myhello
 *   ./olangc ../examples/meta_eval.O --shim-dir ../backends
 *
 * Produces a self-contained native binary that embeds:
 *   - the .O program source (loaded at its startup)
 *   - the backend shims (extracted to a temp dir at startup)
 *   - the entire O-lang C runtime (compiled into the binary)
 *
 * The binary still requires python3 (and nix if used) on the *target* machine.
 * No Rust, no cargo, no source tree needed to *run* the produced binary.
 */

#define _XOPEN_SOURCE 700
#define _DARWIN_C_SOURCE 1
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <dirent.h>
#include <errno.h>
#include <libgen.h>
#include <limits.h>

#if defined(__APPLE__)
#include <mach-o/dyld.h>
#endif

static char *xstrdup(const char *s) {
    if (!s) return NULL;
    size_t n = strlen(s);
    char *c = (char *)malloc(n + 1);
    if (c) memcpy(c, s, n + 1);
    return c;
}

static char *c_string_literal(const char *s) {
    if (!s) return xstrdup("\"\"");
    size_t cap = strlen(s) * 4 + 3;
    char *out = (char *)malloc(cap);
    if (!out) return xstrdup("\"\"");
    char *p = out;
    *p++ = '"';
    for (const unsigned char *q = (const unsigned char *)s; *q; ++q) {
        unsigned char c = *q;
        if (c == '\\') { *p++ = '\\'; *p++ = '\\'; }
        else if (c == '"') { *p++ = '\\'; *p++ = '"'; }
        else if (c == '\n') { *p++ = '\\'; *p++ = 'n'; }
        else if (c == '\r') { *p++ = '\\'; *p++ = 'r'; }
        else if (c == '\t') { *p++ = '\\'; *p++ = 't'; }
        else if (c < 0x20U || c > 0x7eU) {
            *p++ = '\\'; *p++ = 'x';
            static const char hex[] = "0123456789abcdef";
            *p++ = hex[c >> 4]; *p++ = hex[c & 0xf];
        } else {
            *p++ = (char)c;
        }
    }
    *p++ = '"';
    *p = 0;
    return out;
}

static char *get_exe_path(void) {
    char buf[PATH_MAX];
#if defined(__APPLE__)
    uint32_t sz = sizeof(buf);
    if (_NSGetExecutablePath(buf, &sz) == 0) {
        char real[PATH_MAX];
        if (realpath(buf, real)) return xstrdup(real);
    }
#elif defined(__linux__)
    ssize_t n = readlink("/proc/self/exe", buf, sizeof(buf)-1);
    if (n > 0) {
        buf[n] = 0;
        return xstrdup(buf);
    }
#endif
    /* fallback: use argv0 if we had it, or cwd */
    if (realpath("./olangc", buf)) return xstrdup(buf);
    return xstrdup("./olangc");
}

static char *get_runtime_dir(void) {
    char *exe = get_exe_path();
    if (!exe) return xstrdup("src");
    char *tmp = xstrdup(exe);
    char *d = dirname(tmp);
    char *dir = xstrdup(d);
    char cand[PATH_MAX];
    /* when running from c_cpp/ after make: ./src */
    snprintf(cand, sizeof(cand), "%s/src", dir);
    struct stat st;
    if (stat(cand, &st) == 0 && S_ISDIR(st.st_mode)) {
        /* also verify a known c file exists */
        char probe[PATH_MAX]; snprintf(probe,sizeof(probe),"%s/value.c", cand);
        if (stat(probe, &st)==0) { free(dir); free(tmp); free(exe); return xstrdup(cand); }
    }
    /* ../src from bin inside c_cpp/src ? unlikely */
    snprintf(cand, sizeof(cand), "%s/../src", dir);
    if (stat(cand, &st) == 0 && S_ISDIR(st.st_mode)) {
        char probe[PATH_MAX]; snprintf(probe,sizeof(probe),"%s/value.c", cand);
        if (stat(probe, &st)==0) { free(dir); free(tmp); free(exe); return xstrdup(cand); }
    }
    /* last: src under cwd */
    free(dir); free(tmp); free(exe);
    return xstrdup("src");
}

static char *get_include_dir_from_runtime(const char *rt) {
    char *tmp = xstrdup(rt);
    char *d = dirname(tmp);
    char cand[PATH_MAX];
    /* sibling include when rt is c_cpp/src */
    snprintf(cand, sizeof(cand), "%s/../include", d);
    struct stat st;
    if (stat(cand, &st) == 0) { free(tmp); return xstrdup(cand); }
    snprintf(cand, sizeof(cand), "%s/include", d);
    if (stat(cand, &st) == 0) { free(tmp); return xstrdup(cand); }
    /* try from cwd */
    free(tmp);
    return xstrdup("include");
}

static char *get_shim_search_dir(void) {
    /* prefer ../backends from olangc location, else ./backends */
    char *exe = get_exe_path();
    if (!exe) return xstrdup("backends");
    char *tmp = xstrdup(exe);
    char *d = dirname(tmp);
    char cand[PATH_MAX];
    snprintf(cand, sizeof(cand), "%s/../backends", d);
    struct stat st;
    if (stat(cand, &st) == 0) {
        free(tmp);
        free(exe);
        return xstrdup(cand);
    }
    free(tmp);
    free(exe);
    return xstrdup("backends");
}

static int ensure_dir(const char *p) {
    if (mkdir(p, 0755) == 0 || errno == EEXIST) return 0;
    return -1;
}

static int copy_file(const char *src, const char *dst) {
    FILE *in = fopen(src, "rb");
    if (!in) return -1;
    FILE *out = fopen(dst, "wb");
    if (!out) { fclose(in); return -1; }
    char buf[8192];
    size_t n;
    while ((n = fread(buf, 1, sizeof(buf), in)) > 0) fwrite(buf, 1, n, out);
    fclose(out);
    fclose(in);
    return 0;
}

static char *make_temp_build_dir(void) {
    char tmpl[128];
    snprintf(tmpl, sizeof(tmpl), "/tmp/o-build-%d-XXXXXX", (int)getpid());
    char *d = mkdtemp(tmpl);
    if (!d) return NULL;
    return xstrdup(d);
}

static int write_text_file(const char *path, const char *content) {
    FILE *f = fopen(path, "wb");
    if (!f) return -1;
    fputs(content, f);
    fclose(f);
    return 0;
}

static char *read_text_file(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long sz = ftell(f); fseek(f, 0, SEEK_SET);
    if (sz < 0) { fclose(f); return NULL; }
    char *b = (char *)malloc((size_t)sz + 1);
    if (!b) { fclose(f); return NULL; }
    size_t n = fread(b, 1, (size_t)sz, f);
    b[n] = 0;
    fclose(f);
    return b;
}

/* Very small embedded shim table for standalone (python is critical) */
static const char *SHIM_NAMES[] = {
    "python_shim.py",
    "nix_shim.py",
    "nix_store_shim.py",
    "nixos_test_shim.py",
    "bash_shim.py",
    NULL
};

static int write_shims_to(const char *shim_src_dir, const char *dest_dir) {
    ensure_dir(dest_dir);
    for (int i = 0; SHIM_NAMES[i]; ++i) {
        char srcp[PATH_MAX], dstp[PATH_MAX];
        snprintf(srcp, sizeof(srcp), "%s/%s", shim_src_dir, SHIM_NAMES[i]);
        snprintf(dstp, sizeof(dstp), "%s/%s", dest_dir, SHIM_NAMES[i]);
        struct stat st;
        if (stat(srcp, &st) == 0) {
            copy_file(srcp, dstp);
            chmod(dstp, 0755);
        } else {
            /* write a stub if missing */
            if (strstr(SHIM_NAMES[i], "python")) {
                const char *stub =
                    "#!/usr/bin/env python3\n"
                    "import sys, json\n"
                    "print(json.dumps({'status':'ok','value':{'t':'str','v':'(python_shim stub)'}}))\n";
                FILE *f = fopen(dstp, "w");
                if (f) { fputs(stub, f); fclose(f); chmod(dstp, 0755); }
            }
        }
    }
    return 0;
}

static int run_cmd(const char *cmd) {
    int st = system(cmd);
    return st;
}

static void usage(void) {
    fprintf(stderr,
        "olangc — compile .O to native binary (C edition)\n"
        "usage: olangc <input.O> [-o <out>] [--shim-dir DIR] [--keep-build-dir]\n");
}

int main(int argc, char **argv) {
    const char *input = NULL;
    const char *output = NULL;
    const char *shim_dir = NULL;
    int keep = 0;

    for (int i=1; i<argc; ++i) {
        if (strcmp(argv[i], "-o") == 0 && i+1 < argc) { output = argv[++i]; continue; }
        if (strcmp(argv[i], "--shim-dir") == 0 && i+1 < argc) { shim_dir = argv[++i]; continue; }
        if (strcmp(argv[i], "--keep-build-dir") == 0) { keep = 1; continue; }
        if (argv[i][0] == '-') { usage(); return 1; }
        if (!input) input = argv[i];
    }
    if (!input) { usage(); return 1; }

    char *src = read_text_file(input);
    if (!src) { fprintf(stderr, "olangc: cannot read %s\n", input); return 1; }

    /* default output stem */
    char def_out[256];
    if (!output) {
        const char *base = strrchr(input, '/'); base = base ? base+1 : input;
        char stem[128]; snprintf(stem, sizeof(stem), "%s", base);
        char *dot = strrchr(stem, '.'); if (dot) *dot=0;
        snprintf(def_out, sizeof(def_out), "%s", stem);
        output = def_out;
    }

    char *rt_dir = get_runtime_dir();
    char *inc_dir = get_include_dir_from_runtime(rt_dir);
    char *sh_search = shim_dir ? xstrdup(shim_dir) : get_shim_search_dir();

    char *build = make_temp_build_dir();
    if (!build) { fprintf(stderr, "olangc: failed to create temp build dir\n"); return 1; }

    char srcdir[PATH_MAX]; snprintf(srcdir, sizeof(srcdir), "%s/src", build);
    char incdir[PATH_MAX]; snprintf(incdir, sizeof(incdir), "%s/include", build);
    char shdir[PATH_MAX]; snprintf(shdir, sizeof(shdir), "%s/shims", srcdir);
    ensure_dir(srcdir);
    ensure_dir(incdir);
    ensure_dir(shdir);

    /* copy runtime sources */
    DIR *d = opendir(rt_dir);
    if (d) {
        struct dirent *ent;
        while ((ent = readdir(d))) {
            if (ent->d_name[0] == '.') continue;
            char s[PATH_MAX], t[PATH_MAX];
            snprintf(s, sizeof(s), "%s/%s", rt_dir, ent->d_name);
            snprintf(t, sizeof(t), "%s/%s", srcdir, ent->d_name);
            copy_file(s, t);
        }
        closedir(d);
    }
    /* copy includes */
    d = opendir(inc_dir);
    if (d) {
        struct dirent *ent;
        while ((ent = readdir(d))) {
            if (ent->d_name[0] == '.') continue;
            char s[PATH_MAX], t[PATH_MAX];
            snprintf(s, sizeof(s), "%s/%s", inc_dir, ent->d_name);
            snprintf(t, sizeof(t), "%s/%s", incdir, ent->d_name);
            copy_file(s, t);
        }
        closedir(d);
    }

    /* strip shebang in place (for embedding) */
    if (src[0]=='#' && src[1]=='!') {
        char *nl = strchr(src, '\n');
        if (nl) {
            memmove(src, nl+1, strlen(nl));
        } else {
            src[0] = 0;
        }
    }
    char *lit = c_string_literal(src);
    free(src);

    /* extract shims */
    write_shims_to(sh_search, shdir);

    /* write generated_main.c that is self-contained (source embedded as literal) */
    char mainc[PATH_MAX]; snprintf(mainc, sizeof(mainc), "%s/generated_main.c", srcdir);
    char gbuf[65536];
    snprintf(gbuf, sizeof(gbuf),
        "#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n"
        "#include <libgen.h>\n#include <limits.h>\n"
        "#include \"value.h\"\n#include \"parser.h\"\n#include \"eval.h\"\n\n"
        "static char *xsdup(const char *s){ if(!s) return NULL; size_t n=strlen(s); char *c=malloc(n+1); if(c) memcpy(c,s,n+1); return c; }\n"
        "static char *get_shims_dir(const char *arg0) {\n"
        "  if (!arg0) return xsdup(\"shims\");\n"
        "  char rp[PATH_MAX];\n"
        "  if (realpath(arg0, rp) == NULL) return xsdup(\"shims\");\n"
        "  char *tmp = xsdup(rp);\n"
        "  if (!tmp) return xsdup(\"shims\");\n"
        "  char *d = dirname(tmp);\n"
        "  char out[PATH_MAX];\n"
        "  snprintf(out, sizeof(out), \"%%s/shims\", d);\n"
        "  free(tmp);\n"
        "  return xsdup(out);\n"
        "}\n"
        "int main(int argc,char**argv){\n"
        "  (void)argc;(void)argv;\n"
        "  const char *src = %s;\n"
        "  StringSet *bs=string_set_new();\n"
        "  const char*t[]={\"O\",\"python\",\"html\",\"markdown\",\"latex\",\"text\",\"quote\",\"nix\",\"nix_expr\",\"nix_store\",\"nixos_test\",\"bash\",\"shell\",\"rust\",\"racket\",0};\n"
        "  for(int i=0;t[i];++i)string_set_add(bs,t[i]);\n"
        "  OParser p; parser_init(&p,src,bs);\n"
        "  ONodeList *nodes=parser_parse(&p);\n"
        "  if(!nodes){fprintf(stderr,\"parse: %%s\\n\",p.error_msg);return 1;}\n"
        "  char *sd = get_shims_dir( (argc>0 ? argv[0] : 0) );\n"
        "  OEvaluator *ev=olang_evaluator_new(sd);\n"
        "  olang_evaluator_set_registered(ev,bs);\n"
        "  OValue *r = olang_evaluator_eval_document(ev, nodes);\n"
        "  if(r){\n"
        "    if(r->tag==OVAL_STR||r->tag==OVAL_HTML){if(r->data.str_val)fputs(r->data.str_val,stdout);}\n"
        "    else if(!oval_is_null(r)){char*repr=oval_splice_repr(r);if(repr){puts(repr);free(repr);}}\n"
        "    oval_release(r);\n"
        "  }\n"
        "  onode_list_free(nodes);\n"
        "  olang_evaluator_free(ev);\n"
        "  free(sd);\n"
        "  string_set_free(bs);\n"
        "  return 0;\n"
        "}\n",
        lit);
    write_text_file(mainc, gbuf);
    free(lit);

    /* compile */
    char cmd[4096];
    snprintf(cmd, sizeof(cmd),
        "cd %s && %s -std=c17 -Wall -O2 -I../include -I. -pthread "
        "value.c parser.c process.c eval.c scheduler.c "
        "nix_ops.c nixos_ops.c generated_main.c -o prog 2>&1",
        srcdir, getenv("CC") ? getenv("CC") : "cc");

    fprintf(stderr, "olangc: %s\n", cmd);
    int st = run_cmd(cmd);
    if (st != 0) {
        fprintf(stderr, "olangc: compile failed (see above). build dir kept at %s\n", build);
        /* leave build */
        return 1;
    }

    /* copy binary out */
    char built[PATH_MAX]; snprintf(built, sizeof(built), "%s/prog", srcdir);
    char final[PATH_MAX]; snprintf(final, sizeof(final), "./%s", output);
    if (strchr(output, '/')) snprintf(final, sizeof(final), "%s", output);
    copy_file(built, final);
    chmod(final, 0755);

    /* Ship shims/ next to the produced binary so AOT executables are
       runnable without the temp build dir. The generated code will
       prefer shims relative to the binary's own location. */
    {
        char fcopy[PATH_MAX];
        snprintf(fcopy, sizeof(fcopy), "%s", final);
        char *d = dirname(fcopy);
        char shims_d[PATH_MAX];
        snprintf(shims_d, sizeof(shims_d), "%s/shims", d);
        ensure_dir(shims_d);
        for (int i = 0; SHIM_NAMES[i]; ++i) {
            char s[PATH_MAX], t[PATH_MAX];
            snprintf(s, sizeof(s), "%s/shims/%s", srcdir, SHIM_NAMES[i]);
            snprintf(t, sizeof(t), "%s/%s", shims_d, SHIM_NAMES[i]);
            struct stat st;
            if (stat(s, &st) == 0) {
                copy_file(s, t);
                chmod(t, 0755);
            }
        }
    }

    if (!keep) {
        /* best effort rm -rf build */
        char rmc[PATH_MAX]; snprintf(rmc, sizeof(rmc), "rm -rf %s", build);
        run_cmd(rmc);
    } else {
        fprintf(stderr, "olangc: kept %s\n", build);
    }

    fprintf(stderr, "olangc: compiled -> %s\n", final);
    free(rt_dir); free(inc_dir); free(sh_search); free(build);
    return 0;
}
