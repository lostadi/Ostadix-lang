#ifndef O_LANG_PROCESS_H
#define O_LANG_PROCESS_H

#include <stdint.h>
#include <stdbool.h>

#include "value.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Exec step from shim (Done with value, or reentrant EvalRequest) ───── */
typedef enum {
    EXEC_STEP_DONE = 0,
    EXEC_STEP_EVAL_REQUEST
} OExecStepKind;

typedef struct {
    OExecStepKind kind;
    OValue *value;   /* owned; for DONE */
    char *src;       /* owned; for EVAL_REQUEST */
} OExecStep;

/* Free an OExecStep (releases value + src if present) */
void oexec_step_free(OExecStep *step);

/* ── BackendProcess: one live shim subprocess per (lang, env_id) ───────── */
typedef struct OBackendProcess OBackendProcess;

OBackendProcess *olang_backend_process_new(const char *shim_path);
void olang_backend_process_free(OBackendProcess *p);

/* Send a wire command (takes ownership of strings inside cmd where applicable) */
void olang_backend_process_send_command(OBackendProcess *p, const OWireCommand *cmd);

/* Receive one step (caller must oexec_step_free the result) */
OExecStep olang_backend_process_recv_step(OBackendProcess *p);

void olang_backend_process_send_eval_result(OBackendProcess *p, OValue *value);

/* High-level: send exec, get final value (or NULL on err). Handles basic case. */
OValue *olang_backend_process_exec(OBackendProcess *p, const char *code, OValueMap *bindings);
void olang_backend_process_cleanup(OBackendProcess *p);

/* ── ProcessRegistry: owns the (lang,env_id) -> process map ────────────── */
typedef struct OProcessRegistry OProcessRegistry;

OProcessRegistry *olang_process_registry_new(void);
void olang_process_registry_free(OProcessRegistry *reg);

/* Send an exec request (spawns process on first use for the key) */
void olang_process_registry_send_exec(OProcessRegistry *reg,
                                      const char *lang, uint32_t env_id,
                                      const char *code, OValueMap *bindings,
                                      const char *shim_path);

/* Receive next step for a key (caller frees with oexec_step_free) */
OExecStep olang_process_registry_recv_exec_step(OProcessRegistry *reg,
                                                const char *lang, uint32_t env_id);

void olang_process_registry_send_eval_result(OProcessRegistry *reg,
                                             const char *lang, uint32_t env_id,
                                             OValue *value);

/* High-level exec that waits for final result value (or NULL). */
OValue *olang_process_registry_exec(OProcessRegistry *reg,
                                    const char *lang, uint32_t env_id,
                                    const char *code, OValueMap *bindings,
                                    const char *shim_path);

void olang_process_registry_cleanup_env(OProcessRegistry *reg, const char *lang, uint32_t env_id);
void olang_process_registry_cleanup_all(OProcessRegistry *reg);

#ifdef __cplusplus
}
#endif

#endif /* O_LANG_PROCESS_H */
