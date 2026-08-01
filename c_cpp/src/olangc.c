/* olangc — AOT compiler for O-lang (pure C edition)
 *
 *   ./olangc ../examples/hello.O
 *   ./olangc ../examples/hello.O -o myhello
 *   ./olangc ../examples/html_basic.O --shim-dir ../backends
 *
 * Produces an AOT application bundle consisting of:
 *   - a native executable with the .O source and C runtime embedded
 *   - a required <executable>.shims directory copied from --shim-dir
 *
 * The bundle still requires python3 (and nix if used) on the target machine.
 * External backend execution requires that per-executable shim directory.
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
#include <stdint.h>
#include <sys/wait.h>

#if defined(__APPLE__)
#include <mach-o/dyld.h>
#endif

static char *xstrdup(const char *s) {
    if (!s) return NULL;
    size_t n = strlen(s);
    if (n == SIZE_MAX) { errno = EOVERFLOW; return NULL; }
    char *c = (char *)malloc(n + 1);
    if (c) memcpy(c, s, n + 1);
    return c;
}

/* Construct paths without fixed buffers or silent snprintf truncation. */
static char *path_join(const char *left, const char *right) {
    if (!left || !right) { errno = EINVAL; return NULL; }
    size_t left_len = strlen(left);
    size_t right_len = strlen(right);
    int needs_slash = left_len > 0 && left[left_len - 1] != '/';
    size_t extra = (size_t)needs_slash + 1U;
    if (left_len > SIZE_MAX - right_len || left_len + right_len > SIZE_MAX - extra) {
        errno = EOVERFLOW;
        return NULL;
    }
    size_t total = left_len + right_len + extra;
    char *result = (char *)malloc(total);
    if (!result) return NULL;
    memcpy(result, left, left_len);
    size_t pos = left_len;
    if (needs_slash) result[pos++] = '/';
    memcpy(result + pos, right, right_len + 1U);
    return result;
}

static char *append_suffix(const char *value, const char *suffix) {
    if (!value || !suffix) { errno = EINVAL; return NULL; }
    size_t value_len = strlen(value);
    size_t suffix_len = strlen(suffix);
    if (value_len > SIZE_MAX - suffix_len - 1U) {
        errno = EOVERFLOW;
        return NULL;
    }
    char *result = (char *)malloc(value_len + suffix_len + 1U);
    if (!result) return NULL;
    memcpy(result, value, value_len);
    memcpy(result + value_len, suffix, suffix_len + 1U);
    return result;
}

static char *path_dirname(const char *path) {
    char *copy = xstrdup(path);
    if (!copy) return NULL;
    char *dir = dirname(copy);
    char *result = xstrdup(dir);
    free(copy);
    return result;
}

static int path_is_dir(const char *path) {
    struct stat st;
    return stat(path, &st) == 0 && S_ISDIR(st.st_mode);
}

static int path_is_file(const char *path) {
    struct stat st;
    return stat(path, &st) == 0 && S_ISREG(st.st_mode);
}

/* Emit source bytes as integer initializers. This avoids both the ISO C
   minimum string-literal limit and ambiguous variable-length \\x escapes. */
static char *c_byte_initializer(const char *s) {
    if (!s) s = "";
    size_t input_len = strlen(s);
    if (input_len > (SIZE_MAX - 32U) / 8U) { errno = EOVERFLOW; return NULL; }
    size_t cap = input_len * 8U + 32U;
    char *out = (char *)malloc(cap);
    if (!out) return NULL;
    char *p = out;
    static const char hex[] = "0123456789abcdef";
    for (size_t i = 0; i < input_len; ++i) {
        unsigned char c = (unsigned char)s[i];
        if (i % 16U == 0U) {
            memcpy(p, "        ", 8U);
            p += 8U;
        }
        *p++ = '0';
        *p++ = 'x';
        *p++ = hex[c >> 4U];
        *p++ = hex[c & 0x0fU];
        *p++ = ',';
        *p++ = (i % 16U == 15U) ? '\n' : ' ';
    }
    if (input_len != 0U && input_len % 16U != 0U) *p++ = '\n';
    *p = 0;
    return out;
}

static char *get_exe_path(void) {
#if defined(__APPLE__)
    char first_byte;
    uint32_t size = 1;
    if (_NSGetExecutablePath(&first_byte, &size) != 0 && size > 1) {
        char *buf = (char *)malloc((size_t)size);
        if (buf && _NSGetExecutablePath(buf, &size) == 0) {
            char *resolved = realpath(buf, NULL);
            free(buf);
            if (resolved) return resolved;
        } else {
            free(buf);
        }
    }
#elif defined(__linux__)
    size_t cap = 256;
    while (cap <= (size_t)PATH_MAX * 16U) {
        char *buf = (char *)malloc(cap);
        if (!buf) break;
        ssize_t n = readlink("/proc/self/exe", buf, cap - 1U);
        if (n >= 0 && (size_t)n < cap - 1U) {
            buf[n] = 0;
            return buf;
        }
        free(buf);
        if (n < 0 || cap > SIZE_MAX / 2U) break;
        cap *= 2U;
    }
#endif
    /* fallback: use argv0 if we had it, or cwd */
    char *resolved = realpath("./olangc", NULL);
    if (resolved) return resolved;
    return xstrdup("./olangc");
}

static char *get_runtime_dir(void) {
    char *exe = get_exe_path();
    if (!exe) return xstrdup("src");
    char *dir = path_dirname(exe);
    if (!dir) { free(exe); return NULL; }
    /* when running from c_cpp/ after make: ./src */
    char *cand = path_join(dir, "src");
    char *probe = cand ? path_join(cand, "value.c") : NULL;
    if (cand && probe && path_is_dir(cand) && path_is_file(probe)) {
        free(probe); free(dir); free(exe); return cand;
    }
    free(probe); free(cand);
    /* ../src from bin inside c_cpp/src ? unlikely */
    cand = path_join(dir, "../src");
    probe = cand ? path_join(cand, "value.c") : NULL;
    if (cand && probe && path_is_dir(cand) && path_is_file(probe)) {
        free(probe); free(dir); free(exe); return cand;
    }
    free(probe); free(cand);
    /* last: src under cwd */
    free(dir); free(exe);
    return xstrdup("src");
}

static char *get_include_dir_from_runtime(const char *rt) {
    char *dir = path_dirname(rt);
    if (!dir) return NULL;
    /* sibling include when rt is c_cpp/src */
    char *cand = path_join(dir, "../include");
    if (cand && path_is_dir(cand)) { free(dir); return cand; }
    free(cand);
    cand = path_join(dir, "include");
    if (cand && path_is_dir(cand)) { free(dir); return cand; }
    free(cand);
    /* try from cwd */
    free(dir);
    return xstrdup("include");
}

static char *get_shim_search_dir(void) {
    /* prefer ../backends from olangc location, else ./backends */
    char *exe = get_exe_path();
    if (!exe) return xstrdup("backends");
    char *dir = path_dirname(exe);
    char *cand = dir ? path_join(dir, "../backends") : NULL;
    if (cand && path_is_dir(cand)) {
        free(dir); free(exe); return cand;
    }
    free(cand); free(dir); free(exe);
    return xstrdup("backends");
}

static int ensure_dir(const char *p) {
    if (mkdir(p, 0755) == 0) return 0;
    if (errno == EEXIST && path_is_dir(p)) return 0;
    return -1;
}

static int copy_file(const char *src, const char *dst) {
    FILE *in = fopen(src, "rb");
    if (!in) return -1;
    FILE *out = fopen(dst, "wb");
    if (!out) { fclose(in); return -1; }
    char buf[8192];
    size_t n;
    int failed = 0;
    while ((n = fread(buf, 1, sizeof(buf), in)) > 0) {
        if (fwrite(buf, 1, n, out) != n) { failed = 1; break; }
    }
    if (ferror(in)) failed = 1;
    if (fclose(out) != 0) failed = 1;
    if (fclose(in) != 0) failed = 1;
    if (failed) { unlink(dst); errno = EIO; return -1; }
    return 0;
}

static char *make_temp_build_dir(void) {
    const char *base = getenv("TMPDIR");
    if (!base || !*base) base = "/tmp";
    char *tmpl = path_join(base, "o-build-XXXXXX");
    if (!tmpl) return NULL;
    if (!mkdtemp(tmpl)) { free(tmpl); return NULL; }
    return tmpl;
}

static int write_text_file(const char *path, const char *content) {
    FILE *f = fopen(path, "wb");
    if (!f) return -1;
    int failed = fputs(content, f) == EOF;
    if (fclose(f) != 0) failed = 1;
    if (failed) { unlink(path); errno = EIO; return -1; }
    return 0;
}

static char *read_text_file(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return NULL; }
    long sz = ftell(f);
    if (sz < 0 || fseek(f, 0, SEEK_SET) != 0) { fclose(f); return NULL; }
    if ((unsigned long)sz > SIZE_MAX - 1U) { fclose(f); errno = EOVERFLOW; return NULL; }
    char *b = (char *)malloc((size_t)sz + 1);
    if (!b) { fclose(f); return NULL; }
    size_t n = fread(b, 1, (size_t)sz, f);
    if (n != (size_t)sz) { free(b); fclose(f); errno = EIO; return NULL; }
    b[n] = 0;
    if (fclose(f) != 0) { free(b); return NULL; }
    return b;
}

static int copy_regular_files(const char *source_dir, const char *dest_dir) {
    DIR *dir = opendir(source_dir);
    if (!dir) return -1;
    int failed = 0;
    int saved_errno = 0;
    struct dirent *ent;
    for (;;) {
        errno = 0;
        ent = readdir(dir);
        if (!ent) {
            if (errno != 0) { failed = 1; saved_errno = errno; }
            break;
        }
        if (ent->d_name[0] == '.') continue;
        char *source = path_join(source_dir, ent->d_name);
        char *dest = path_join(dest_dir, ent->d_name);
        if (!source || !dest) {
            failed = 1;
        } else {
            struct stat st;
            if (stat(source, &st) != 0 || (S_ISREG(st.st_mode) && copy_file(source, dest) != 0)) {
                failed = 1;
            }
        }
        if (failed) saved_errno = errno ? errno : EIO;
        free(source);
        free(dest);
        if (failed) break;
    }
    if (closedir(dir) != 0 && !failed) { failed = 1; saved_errno = errno; }
    if (!failed) return 0;
    errno = saved_errno ? saved_errno : EIO;
    return -1;
}

static int remove_tree(const char *path) {
    struct stat st;
    if (lstat(path, &st) != 0) return errno == ENOENT ? 0 : -1;
    if (!S_ISDIR(st.st_mode) || S_ISLNK(st.st_mode)) return unlink(path);

    DIR *dir = opendir(path);
    if (!dir) return -1;
    int failed = 0;
    int saved_errno = 0;
    struct dirent *ent;
    for (;;) {
        errno = 0;
        ent = readdir(dir);
        if (!ent) {
            if (errno != 0) { failed = 1; saved_errno = errno; }
            break;
        }
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        char *child = path_join(path, ent->d_name);
        if (!child || remove_tree(child) != 0) failed = 1;
        if (failed) saved_errno = errno ? errno : EIO;
        free(child);
        if (failed) break;
    }
    if (closedir(dir) != 0 && !failed) { failed = 1; saved_errno = errno; }
    if (failed) { errno = saved_errno ? saved_errno : EIO; return -1; }
    return rmdir(path);
}

/* Every external backend advertised by generated_main.c is required. */
static const char *SHIM_NAMES[] = {
    "o_shim_common.py",
    "python_shim.py",
    "nix_shim.py",
    "nix_store_shim.py",
    "nixos_test_shim.py",
    "bash_shim.py",
    "shell_shim.py",
    "rust_shim.py",
    "racket_shim.py",
    NULL
};

static int env_enabled(const char *name) {
    const char *value = getenv(name);
    return value && *value && strcmp(value, "0") != 0;
}

static int write_shims_to(
    const char *shim_src_dir,
    const char *dest_dir,
    int inject_publication_failure
) {
    if (ensure_dir(dest_dir) != 0) return -1;
    for (int i = 0; SHIM_NAMES[i]; ++i) {
        char *srcp = path_join(shim_src_dir, SHIM_NAMES[i]);
        char *dstp = path_join(dest_dir, SHIM_NAMES[i]);
        if (!srcp || !dstp) { free(srcp); free(dstp); return -1; }
        if (!path_is_file(srcp)) {
            fprintf(stderr, "olangc: missing required shim asset: %s\n", srcp);
            free(srcp);
            free(dstp);
            errno = ENOENT;
            return -1;
        }
        if (copy_file(srcp, dstp) != 0 || chmod(dstp, 0755) != 0) {
            free(srcp);
            free(dstp);
            return -1;
        }
        free(srcp);
        free(dstp);
        if (inject_publication_failure && i == 0 &&
            env_enabled("OLANGC_TEST_FAIL_AFTER_FIRST_PUBLISHED_SHIM")) {
            errno = EIO;
            return -1;
        }
    }
    return 0;
}

/* Publish the executable and its required sibling shims as one rollback-safe
   transaction. Both new artifacts are built inside a same-directory staging
   tree. The old shim tree is retained until the executable rename commits, so
   every ordinary failure leaves the previous executable+shims pair intact. */
static int publish_bundle(
    const char *built,
    const char *shim_src_dir,
    const char *final,
    const char *output_dir,
    const char *output_shims
) {
    char *transaction = path_join(output_dir, ".olangc-bundle-XXXXXX");
    char *staged_shims = NULL;
    char *staged_program = NULL;
    char *old_shims = NULL;
    int old_shims_moved = 0;
    int new_shims_live = 0;
    int result = -1;
    int saved_errno = 0;

    if (!transaction || !mkdtemp(transaction)) goto cleanup;
    staged_shims = path_join(transaction, "new.shims");
    staged_program = path_join(transaction, "program");
    old_shims = path_join(transaction, "old.shims");
    if (!staged_shims || !staged_program || !old_shims) goto cleanup;

    if (write_shims_to(shim_src_dir, staged_shims, 1) != 0 ||
        copy_file(built, staged_program) != 0 ||
        chmod(staged_program, 0755) != 0) {
        goto cleanup;
    }

    struct stat prior;
    if (lstat(output_shims, &prior) == 0) {
        if (rename(output_shims, old_shims) != 0) goto cleanup;
        old_shims_moved = 1;
    } else if (errno != ENOENT) {
        goto cleanup;
    }

    if (rename(staged_shims, output_shims) != 0) goto rollback;
    new_shims_live = 1;
    if (env_enabled("OLANGC_TEST_FAIL_EXEC_RENAME")) {
        errno = EIO;
        goto rollback;
    }
    if (rename(staged_program, final) != 0) goto rollback;

    result = 0;
    if (old_shims_moved && remove_tree(old_shims) != 0) {
        fprintf(stderr,
                "olangc: warning: committed bundle but could not remove old "
                "shim staging tree %s: %s\n",
                old_shims, strerror(errno));
    }
    goto cleanup;

rollback:
    saved_errno = errno ? errno : EIO;
    if (new_shims_live) {
        if (rename(output_shims, staged_shims) != 0) {
            fprintf(stderr,
                    "olangc: failed to roll back staged output shims %s: %s\n",
                    output_shims, strerror(errno));
            saved_errno = EIO;
        } else {
            new_shims_live = 0;
        }
    }
    if (old_shims_moved) {
        if (rename(old_shims, output_shims) != 0) {
            fprintf(stderr,
                    "olangc: failed to restore prior output shims %s: %s\n",
                    output_shims, strerror(errno));
            saved_errno = EIO;
        } else {
            old_shims_moved = 0;
        }
    }
    errno = saved_errno;

cleanup:
    if (result != 0 && saved_errno == 0) saved_errno = errno ? errno : EIO;
    if (transaction && remove_tree(transaction) != 0 && result == 0) {
        fprintf(stderr,
                "olangc: warning: committed bundle but could not remove "
                "transaction tree %s: %s\n",
                transaction, strerror(errno));
    }
    free(staged_shims);
    free(staged_program);
    free(old_shims);
    free(transaction);
    if (result != 0) errno = saved_errno ? saved_errno : EIO;
    return result;
}

/* Run the compiler as an argv vector. CC is one executable path/name, never shell text. */
static int run_compiler(const char *compiler, const char *working_dir) {
    char *args[32];
    size_t n = 0;
    args[n++] = (char *)compiler;
    args[n++] = "-std=c17";
    args[n++] = "-Wall";
    args[n++] = "-Wextra";
    args[n++] = "-Wpedantic";
    if (env_enabled("OLANGC_WARNINGS_AS_ERRORS")) args[n++] = "-Werror";
    args[n++] = "-O2";
    args[n++] = "-D_POSIX_C_SOURCE=200809L";
    args[n++] = "-D_XOPEN_SOURCE=700";
    args[n++] = "-I../include";
    args[n++] = "-I.";
    args[n++] = "-pthread";
    args[n++] = "value.c";
    args[n++] = "parser.c";
    args[n++] = "process.c";
    args[n++] = "eval.c";
    args[n++] = "scheduler.c";
    args[n++] = "nix_ops.c";
    args[n++] = "nixos_ops.c";
    args[n++] = "generated_main.c";
    args[n++] = "-o";
    args[n++] = "prog";
    args[n] = NULL;

    fprintf(stderr, "olangc: compiler cwd: %s\nolangc: compiler argv:", working_dir);
    for (size_t i = 0; args[i]; ++i) fprintf(stderr, " [%s]", args[i]);
    fputc('\n', stderr);
    fflush(NULL);

    pid_t child = fork();
    if (child < 0) return -1;
    if (child == 0) {
        if (chdir(working_dir) != 0) {
            perror("olangc: compiler chdir");
            _exit(126);
        }
        execvp(compiler, args);
        perror("olangc: compiler execvp");
        _exit(127);
    }

    int status;
    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) return -1;
    }
    if (WIFEXITED(status)) return WEXITSTATUS(status);
    if (WIFSIGNALED(status)) return 128 + WTERMSIG(status);
    return -1;
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
    int keep_requested = 0;

    for (int i=1; i<argc; ++i) {
        if (strcmp(argv[i], "-o") == 0 && i+1 < argc) { output = argv[++i]; continue; }
        if (strcmp(argv[i], "--shim-dir") == 0 && i+1 < argc) { shim_dir = argv[++i]; continue; }
        if (strcmp(argv[i], "--keep-build-dir") == 0) { keep_requested = 1; continue; }
        if (argv[i][0] == '-') { usage(); return 1; }
        if (!input) input = argv[i];
    }
    if (!input) { usage(); return 1; }

    char *src = read_text_file(input);
    if (!src) { fprintf(stderr, "olangc: cannot read %s\n", input); return 1; }

    /* default output stem */
    char *owned_output = NULL;
    if (!output) {
        const char *base = strrchr(input, '/'); base = base ? base+1 : input;
        owned_output = xstrdup(base);
        if (!owned_output) { free(src); fprintf(stderr, "olangc: out of memory\n"); return 1; }
        char *dot = strrchr(owned_output, '.'); if (dot) *dot=0;
        output = owned_output;
    }

    char *rt_dir = get_runtime_dir();
    char *inc_dir = rt_dir ? get_include_dir_from_runtime(rt_dir) : NULL;
    char *sh_search = shim_dir ? xstrdup(shim_dir) : get_shim_search_dir();
    if (!rt_dir || !inc_dir || !sh_search) {
        fprintf(stderr, "olangc: failed to locate runtime inputs\n");
        free(src); free(owned_output); free(rt_dir); free(inc_dir); free(sh_search);
        return 1;
    }

    char *build = make_temp_build_dir();
    if (!build) {
        fprintf(stderr, "olangc: failed to create temp build dir: %s\n", strerror(errno));
        free(src); free(owned_output); free(rt_dir); free(inc_dir); free(sh_search);
        return 1;
    }

    char *srcdir = path_join(build, "src");
    char *incdir = path_join(build, "include");
    char *shdir = srcdir ? path_join(srcdir, "shims") : NULL;
    char *mainc = NULL;
    char *built = NULL;
    char *final = NULL;
    char *output_dir = NULL;
    char *output_shims = NULL;
    char *source_bytes = NULL;
    char *generated = NULL;
    char *resolved_compiler = NULL;
    int result = 1;
    int preserve_build = keep_requested;

    if (!srcdir || !incdir || !shdir ||
        ensure_dir(srcdir) != 0 || ensure_dir(incdir) != 0 || ensure_dir(shdir) != 0) {
        fprintf(stderr, "olangc: failed to construct build tree: %s\n", strerror(errno));
        goto cleanup;
    }

    if (copy_regular_files(rt_dir, srcdir) != 0 || copy_regular_files(inc_dir, incdir) != 0) {
        fprintf(stderr, "olangc: failed to copy runtime sources: %s\n", strerror(errno));
        goto cleanup;
    }

    /* strip shebang in place (for embedding) */
    if (strncmp(src, "#!", 2) == 0) {
        char *nl = strchr(src, '\n');
        if (nl) {
            memmove(src, nl+1, strlen(nl));
        } else {
            src[0] = 0;
        }
    }
    source_bytes = c_byte_initializer(src);
    if (!source_bytes) { fprintf(stderr, "olangc: failed to encode input source\n"); goto cleanup; }

    /* extract shims */
    if (write_shims_to(sh_search, shdir, 0) != 0) {
        fprintf(stderr, "olangc: failed to stage backend shims: %s\n", strerror(errno));
        goto cleanup;
    }

    /* Write generated_main.c with the source embedded as an exact byte array. */
    mainc = path_join(srcdir, "generated_main.c");
    if (!mainc) { fprintf(stderr, "olangc: failed to construct generated source path\n"); goto cleanup; }
    const char *main_template =
        "#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n"
        "#include <stdint.h>\n#include <unistd.h>\n"
        "#if defined(__APPLE__)\n#include <mach-o/dyld.h>\n#endif\n"
        "#include \"value.h\"\n#include \"parser.h\"\n#include \"eval.h\"\n\n"
        "static char *append_suffix(const char *a,const char *b){\n"
        "  if(!a||!b)return NULL;\n"
        "  size_t an=strlen(a),bn=strlen(b);\n"
        "  if(bn>=SIZE_MAX||an>SIZE_MAX-bn-1U)return NULL;\n"
        "  char *out=malloc(an+bn+1U);\n"
        "  if(!out)return NULL;\n"
        "  memcpy(out,a,an);\n"
        "  memcpy(out+an,b,bn+1U);\n"
        "  return out;\n"
        "}\n"
        "static char *get_executable_path(const char *arg0) {\n"
        "#if defined(__APPLE__)\n"
        "  char first; uint32_t size=1;\n"
        "  if(_NSGetExecutablePath(&first,&size)!=0&&size>1){char *buf=malloc((size_t)size);if(buf&&_NSGetExecutablePath(buf,&size)==0){char *rp=realpath(buf,NULL);free(buf);if(rp)return rp;}else free(buf);}\n"
        "#elif defined(__linux__)\n"
        "  size_t cap=256; while(cap<=1048576U){char *buf=malloc(cap);if(!buf)break;ssize_t n=readlink(\"/proc/self/exe\",buf,cap-1U);if(n>=0&&(size_t)n<cap-1U){buf[n]=0;return buf;}free(buf);if(n<0||cap>SIZE_MAX/2U)break;cap*=2U;}\n"
        "#endif\n"
        "  return arg0?realpath(arg0,NULL):NULL;\n"
        "}\n"
        "static char *get_shims_dir(const char *arg0) {\n"
        "  char *rp=get_executable_path(arg0);if(!rp)return NULL;\n"
        "  char *out=append_suffix(rp,\".shims\");free(rp);return out;\n"
        "}\n"
        "static const unsigned char embedded_source[] = {\n"
        "%s"
        "        0\n"
        "};\n"
        "int main(int argc,char**argv){\n"
        "  const char *src=(const char *)embedded_source;\n"
        "  StringSet *bs=string_set_new();\n"
        "  if(!bs){fprintf(stderr,\"failed to create backend set\\n\");return 1;}\n"
        "  const char*t[]={\"O\",\"python\",\"html\",\"markdown\",\"latex\",\"text\",\"quote\",\"nix\",\"nix_expr\",\"nix_store\",\"nixos_test\",\"bash\",\"shell\",\"rust\",\"racket\",0};\n"
        "  for(int i=0;t[i];++i){string_set_add(bs,t[i]);if(!string_set_contains(bs,t[i])){fprintf(stderr,\"failed to register backend tag %%s\\n\",t[i]);string_set_free(bs);return 1;}}\n"
        "  OParser p; parser_init(&p,src,bs);\n"
        "  ONodeList *nodes=parser_parse(&p);\n"
        "  if(!nodes){fprintf(stderr,\"parse: %%s\\n\",p.error_msg);string_set_free(bs);return 1;}\n"
        "  char *sd = get_shims_dir( (argc>0 ? argv[0] : 0) );\n"
        "  if(!sd){fprintf(stderr,\"failed to locate executable shims directory\\n\");onode_list_free(nodes);string_set_free(bs);return 1;}\n"
        "  OEvaluator *ev=olang_evaluator_new(sd);\n"
        "  if(!ev){fprintf(stderr,\"failed to create evaluator\\n\");free(sd);onode_list_free(nodes);string_set_free(bs);return 1;}\n"
        "  if(!olang_evaluator_set_registered(ev,bs)){fprintf(stderr,\"failed to register backends\\n\");olang_evaluator_free(ev);free(sd);onode_list_free(nodes);string_set_free(bs);return 1;}\n"
        "  OValue *r = olang_evaluator_eval_document(ev, nodes);\n"
        "  if(r){\n"
        "    if(r->tag==OVAL_STR||r->tag==OVAL_HTML){if(r->data.str_val)fputs(r->data.str_val,stdout);}\n"
        "    else if(!oval_is_null(r)){char*repr=oval_splice_repr(r);if(repr){puts(repr);free(repr);}}\n"
        "    oval_release(r);\n"
        "  }\n"
        "  int failed=olang_evaluator_had_error(ev)?1:0;\n"
        "  onode_list_free(nodes);\n"
        "  olang_evaluator_free(ev);\n"
        "  free(sd);\n"
        "  string_set_free(bs);\n"
        "  return failed;\n"
        "}\n";
    int generated_size = snprintf(NULL, 0, main_template, source_bytes);
    if (generated_size < 0 || (size_t)generated_size == SIZE_MAX) {
        fprintf(stderr, "olangc: failed to size generated source\n");
        goto cleanup;
    }
    generated = (char *)malloc((size_t)generated_size + 1U);
    if (!generated || snprintf(generated, (size_t)generated_size + 1U, main_template, source_bytes) != generated_size ||
        write_text_file(mainc, generated) != 0) {
        fprintf(stderr, "olangc: failed to write generated source: %s\n", strerror(errno));
        goto cleanup;
    }

    /* compile */
    const char *compiler = getenv("CC");
    if (!compiler || !*compiler) compiler = "cc";
    if (strchr(compiler, '/')) {
        resolved_compiler = realpath(compiler, NULL);
        if (!resolved_compiler) {
            fprintf(stderr, "olangc: cannot resolve compiler path %s: %s\n",
                    compiler, strerror(errno));
            goto cleanup;
        }
        compiler = resolved_compiler;
    }
    int st = run_compiler(compiler, srcdir);
    if (st != 0) {
        fprintf(stderr, "olangc: compile failed with status %d; build dir kept at %s\n", st, build);
        preserve_build = 1;
        goto cleanup;
    }

    /* Resolve publication paths. The bundle publisher stages both artifacts,
       swaps the shims with rollback, then commits the executable. */
    built = path_join(srcdir, "prog");
    final = strchr(output, '/') ? xstrdup(output) : path_join(".", output);
    output_dir = final ? path_dirname(final) : NULL;
    output_shims = final ? append_suffix(final, ".shims") : NULL;
    if (!built || !final || !output_dir || !output_shims) {
        fprintf(stderr, "olangc: failed to construct publication paths\n");
        goto cleanup;
    }

    if (publish_bundle(built, shdir, final, output_dir, output_shims) != 0) {
        fprintf(stderr, "olangc: failed to publish output bundle %s: %s\n",
                output, strerror(errno));
        goto cleanup;
    }

    fprintf(stderr, "olangc: compiled -> %s\n", final);
    result = 0;

cleanup:
    if (preserve_build) {
        if (keep_requested && result == 0) fprintf(stderr, "olangc: kept %s\n", build);
    } else if (remove_tree(build) != 0) {
        fprintf(stderr, "olangc: warning: could not remove build dir %s: %s\n", build, strerror(errno));
    }
    free(src);
    free(owned_output);
    free(rt_dir);
    free(inc_dir);
    free(sh_search);
    free(build);
    free(srcdir);
    free(incdir);
    free(shdir);
    free(mainc);
    free(built);
    free(final);
    free(output_dir);
    free(output_shims);
    free(source_bytes);
    free(generated);
    free(resolved_compiler);
    return result;
}
