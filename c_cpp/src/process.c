#include "process.h"

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <limits.h>

#include "value.h"

/* ────────────────────────────────────────────────────────────────────── */
/* Internal helpers                                                      */
/* ────────────────────────────────────────────────────────────────────── */

typedef struct BackendProcessImpl {
    pid_t child_pid;
    int stdin_fd;
    int stdout_fd;
    FILE *stdin_file;
    FILE *stdout_file;
    bool alive;
} BackendProcessImpl;

static bool has_python_suffix(const char *path) {
    if (path == NULL) return false;
    size_t len = strlen(path);
    return len >= 3 && strcmp(path + len - 3, ".py") == 0;
}

static void install_full_backend_authority_env(void) {
    setenv("O_BACKEND_AUTHORITIES", "[\"fs_read\",\"fs_write\",\"network\",\"process\"]", 1);
}

static void close_fd(int *fd) {
    if (fd && *fd >= 0) {
        close(*fd);
        *fd = -1;
    }
}

static void safe_fclose(FILE **f) {
    if (f && *f) {
        fclose(*f);
        *f = NULL;
    }
}

/* ── OExecStep ───────────────────────────────────────────────────────── */

void oexec_step_free(OExecStep *step) {
    if (step == NULL) return;
    if (step->value) {
        oval_release(step->value);
        step->value = NULL;
    }
    free(step->src);
    step->src = NULL;
}

/* ── BackendProcess (impl) ───────────────────────────────────────────── */

OBackendProcess *olang_backend_process_new(const char *shim_path) {
    if (shim_path == NULL) return NULL;

    BackendProcessImpl *p = (BackendProcessImpl *)calloc(1, sizeof(*p));
    if (!p) return NULL;

    p->child_pid = -1;
    p->stdin_fd = -1;
    p->stdout_fd = -1;
    p->stdin_file = NULL;
    p->stdout_file = NULL;
    p->alive = false;

    int stdin_pipe[2] = {-1, -1};
    int stdout_pipe[2] = {-1, -1};

    if (pipe(stdin_pipe) != 0) {
        perror("pipe stdin");
        free(p);
        return NULL;
    }
    if (pipe(stdout_pipe) != 0) {
        perror("pipe stdout");
        close(stdin_pipe[0]); close(stdin_pipe[1]);
        free(p);
        return NULL;
    }

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        close(stdin_pipe[0]); close(stdin_pipe[1]);
        close(stdout_pipe[0]); close(stdout_pipe[1]);
        free(p);
        return NULL;
    }

    if (pid == 0) {
        /* child */
        if (dup2(stdin_pipe[0], STDIN_FILENO) < 0 || dup2(stdout_pipe[1], STDOUT_FILENO) < 0) {
            perror("dup2");
            _exit(127);
        }
        close(stdin_pipe[0]); close(stdin_pipe[1]);
        close(stdout_pipe[0]); close(stdout_pipe[1]);

        install_full_backend_authority_env();
        if (has_python_suffix(shim_path)) {
            setenv("PYTHONDONTWRITEBYTECODE", "1", 1);
            char *argv[] = { (char*)"python3", (char*)shim_path, NULL };
            execvp("python3", argv);
        } else {
            char *argv[] = { (char*)shim_path, NULL };
            execvp(shim_path, argv);
        }
        perror("execvp shim");
        _exit(127);
    }

    /* parent */
    close(stdin_pipe[0]);
    close(stdout_pipe[1]);

    p->child_pid = pid;
    p->stdin_fd = stdin_pipe[1];
    p->stdout_fd = stdout_pipe[0];

    p->stdin_file = fdopen(p->stdin_fd, "w");
    if (!p->stdin_file) {
        perror("fdopen stdin");
        close(p->stdin_fd); p->stdin_fd = -1;
        close(p->stdout_fd); p->stdout_fd = -1;
        kill(pid, SIGKILL);
        waitpid(pid, NULL, 0);
        free(p);
        return NULL;
    }
    p->stdin_fd = -1; /* owned by FILE* now */

    p->stdout_file = fdopen(p->stdout_fd, "r");
    if (!p->stdout_file) {
        perror("fdopen stdout");
        fclose(p->stdin_file); p->stdin_file = NULL;
        close(p->stdout_fd); p->stdout_fd = -1;
        kill(pid, SIGKILL);
        waitpid(pid, NULL, 0);
        free(p);
        return NULL;
    }
    p->stdout_fd = -1;

    p->alive = true;
    return (OBackendProcess *)p;
}

void olang_backend_process_free(OBackendProcess *bp) {
    if (!bp) return;
    BackendProcessImpl *p = (BackendProcessImpl *)bp;
    if (p->alive) {
        olang_backend_process_cleanup(bp);
    }
    free(p);
}

void olang_backend_process_send_command(OBackendProcess *bp, const OWireCommand *cmd) {
    BackendProcessImpl *p = (BackendProcessImpl *)bp;
    if (!p || !p->alive || !p->stdin_file) {
        fprintf(stderr, "process: send_command on dead process\n");
        return;
    }
    char *json = owire_cmd_to_json(cmd);
    if (!json) {
        fprintf(stderr, "process: failed to serialize command\n");
        return;
    }
    if (fprintf(p->stdin_file, "%s\n", json) < 0) {
        perror("fprintf to shim");
    }
    fflush(p->stdin_file);
    free(json);
}

OExecStep olang_backend_process_recv_step(OBackendProcess *bp) {
    BackendProcessImpl *p = (BackendProcessImpl *)bp;
    OExecStep step = {0};
    if (!p || !p->alive || !p->stdout_file) {
        step.kind = EXEC_STEP_DONE;
        return step;
    }

    char *line = NULL;
    size_t cap = 0;
    errno = 0;
    ssize_t n = getline(&line, &cap, p->stdout_file);
    if (n < 0) {
        free(line);
        if (errno == 0) {
            fprintf(stderr, "process: shim closed stdout\n");
        } else {
            perror("getline shim");
        }
        p->alive = false;
        step.kind = EXEC_STEP_DONE;
        return step;
    }
    if (n == 0) {
        free(line);
        p->alive = false;
        step.kind = EXEC_STEP_DONE;
        return step;
    }

    OWireResponse *resp = owire_resp_from_json(line);
    free(line);
    if (!resp) {
        fprintf(stderr, "process: bad JSON from shim\n");
        step.kind = EXEC_STEP_DONE;
        return step;
    }

    switch (resp->tag) {
        case WIRE_RESP_OK:
            step.kind = EXEC_STEP_DONE;
            step.value = resp->value;
            resp->value = NULL;
            break;
        case WIRE_RESP_ERR:
            fprintf(stderr, "shim error: %s\n", resp->message ? resp->message : "(no message)");
            step.kind = EXEC_STEP_DONE;
            break;
        case WIRE_RESP_EVAL_REQUEST:
            step.kind = EXEC_STEP_EVAL_REQUEST;
            step.src = resp->src ? strdup(resp->src) : NULL;
            break;
        default:
            fprintf(stderr, "process: unknown resp tag\n");
            step.kind = EXEC_STEP_DONE;
            break;
    }
    owire_resp_free(resp);
    return step;
}

void olang_backend_process_send_eval_result(OBackendProcess *bp, OValue *value) {
    BackendProcessImpl *p = (BackendProcessImpl *)bp;
    if (!p || !p->alive) return;
    OWireCommand cmd = {0};
    cmd.tag = WIRE_CMD_EVAL_RESULT;
    cmd.value = value;
    olang_backend_process_send_command(bp, &cmd);
}

OValue *olang_backend_process_exec(OBackendProcess *bp, const char *code, OValueMap *bindings) {
    BackendProcessImpl *p = (BackendProcessImpl *)bp;
    if (!p || !code) return NULL;

    OWireCommand cmd = {0};
    cmd.tag = WIRE_CMD_EXEC;
    cmd.code = (char *)code; /* wire takes it, but we don't free here */
    cmd.bindings = bindings;
    olang_backend_process_send_command(bp, &cmd);

    OExecStep step = olang_backend_process_recv_step(bp);
    if (step.kind == EXEC_STEP_DONE) {
        OValue *v = step.value;
        step.value = NULL;
        oexec_step_free(&step);
        return v;
    }
    /* Unexpected eval_request in simple exec path */
    fprintf(stderr, "process: unexpected eval_request in simple exec (O.eval needs full registry path)\n");
    if (step.src) free(step.src);
    oexec_step_free(&step);
    return NULL;
}

void olang_backend_process_cleanup(OBackendProcess *bp) {
    BackendProcessImpl *p = (BackendProcessImpl *)bp;
    if (!p || !p->alive) return;

    OWireCommand cmd = {0};
    cmd.tag = WIRE_CMD_CLEANUP;
    if (p->stdin_file) {
        olang_backend_process_send_command(bp, &cmd);
    }

    safe_fclose(&p->stdin_file);
    safe_fclose(&p->stdout_file);
    close_fd(&p->stdin_fd);
    close_fd(&p->stdout_fd);

    if (p->child_pid > 0) {
        if (kill(p->child_pid, SIGKILL) != 0 && errno != ESRCH) {
            perror("kill shim");
        }
        while (waitpid(p->child_pid, NULL, 0) < 0) {
            if (errno != EINTR) break;
        }
    }
    p->child_pid = -1;
    p->alive = false;
}

/* ── Registry (simple dynamic array of entries; N is tiny) ─────────────── */

typedef struct RegistryEntry {
    char *lang;
    uint32_t env_id;
    OBackendProcess *proc;
} RegistryEntry;

struct OProcessRegistry {
    RegistryEntry *entries;
    size_t len;
    size_t cap;
};

OProcessRegistry *olang_process_registry_new(void) {
    OProcessRegistry *r = (OProcessRegistry *)calloc(1, sizeof(*r));
    return r;
}

void olang_process_registry_free(OProcessRegistry *reg) {
    if (!reg) return;
    olang_process_registry_cleanup_all(reg);
    free(reg->entries);
    free(reg);
}

static int registry_find(const OProcessRegistry *reg, const char *lang, uint32_t env_id, size_t *out_idx) {
    if (!reg || !lang) return 0;
    for (size_t i = 0; i < reg->len; ++i) {
        if (reg->entries[i].env_id == env_id && strcmp(reg->entries[i].lang, lang) == 0) {
            if (out_idx) *out_idx = i;
            return 1;
        }
    }
    return 0;
}

static void registry_add(OProcessRegistry *reg, const char *lang, uint32_t env_id, OBackendProcess *proc) {
    if (reg->len == reg->cap) {
        size_t newcap = reg->cap ? reg->cap * 2 : 8;
        RegistryEntry *ne = (RegistryEntry *)realloc(reg->entries, newcap * sizeof(*ne));
        if (!ne) return;
        reg->entries = ne;
        reg->cap = newcap;
    }
    reg->entries[reg->len].lang = strdup(lang);
    reg->entries[reg->len].env_id = env_id;
    reg->entries[reg->len].proc = proc;
    reg->len++;
}

void olang_process_registry_send_exec(OProcessRegistry *reg,
                                      const char *lang, uint32_t env_id,
                                      const char *code, OValueMap *bindings,
                                      const char *shim_path) {
    if (!reg || !lang || !shim_path) return;
    size_t idx;
    if (!registry_find(reg, lang, env_id, &idx)) {
        OBackendProcess *proc = olang_backend_process_new(shim_path);
        if (!proc) {
            fprintf(stderr, "process: failed to start shim for %s\n", lang);
            return;
        }
        registry_add(reg, lang, env_id, proc);
        registry_find(reg, lang, env_id, &idx);
    }
    OBackendProcess *proc = reg->entries[idx].proc;
    OWireCommand cmd = {0};
    cmd.tag = WIRE_CMD_EXEC;
    cmd.code = (char *)code;
    cmd.bindings = bindings;
    olang_backend_process_send_command(proc, &cmd);
}

OExecStep olang_process_registry_recv_exec_step(OProcessRegistry *reg,
                                                const char *lang, uint32_t env_id) {
    OExecStep empty = {0};
    if (!reg || !lang) return empty;
    size_t idx;
    if (!registry_find(reg, lang, env_id, &idx)) {
        fprintf(stderr, "process: no backend for %s[%u]\n", lang, env_id);
        return empty;
    }
    return olang_backend_process_recv_step(reg->entries[idx].proc);
}

void olang_process_registry_send_eval_result(OProcessRegistry *reg,
                                             const char *lang, uint32_t env_id,
                                             OValue *value) {
    if (!reg || !lang) return;
    size_t idx;
    if (!registry_find(reg, lang, env_id, &idx)) return;
    olang_backend_process_send_eval_result(reg->entries[idx].proc, value);
}

OValue *olang_process_registry_exec(OProcessRegistry *reg,
                                    const char *lang, uint32_t env_id,
                                    const char *code, OValueMap *bindings,
                                    const char *shim_path) {
    if (!reg || !lang) return NULL;
    size_t idx;
    if (!registry_find(reg, lang, env_id, &idx)) {
        OBackendProcess *proc = olang_backend_process_new(shim_path);
        if (!proc) return NULL;
        registry_add(reg, lang, env_id, proc);
        registry_find(reg, lang, env_id, &idx);
    }
    return olang_backend_process_exec(reg->entries[idx].proc, code, bindings);
}

void olang_process_registry_cleanup_env(OProcessRegistry *reg, const char *lang, uint32_t env_id) {
    if (!reg || !lang) return;
    size_t idx;
    if (!registry_find(reg, lang, env_id, &idx)) return;
    OBackendProcess *proc = reg->entries[idx].proc;
    /* remove entry */
    free(reg->entries[idx].lang);
    memmove(&reg->entries[idx], &reg->entries[idx+1], (reg->len - idx - 1) * sizeof(RegistryEntry));
    reg->len--;
    olang_backend_process_cleanup(proc);
    olang_backend_process_free(proc);
}

void olang_process_registry_cleanup_all(OProcessRegistry *reg) {
    if (!reg) return;
    for (size_t i = 0; i < reg->len; ++i) {
        OBackendProcess *proc = reg->entries[i].proc;
        olang_backend_process_cleanup(proc);
        olang_backend_process_free(proc);
        free(reg->entries[i].lang);
    }
    reg->len = 0;
}
