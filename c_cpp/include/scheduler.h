#ifndef O_LANG_SCHEDULER_H
#define O_LANG_SCHEDULER_H

#include <stddef.h>
#include <stdbool.h>

#include "value.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── DiskCache: fingerprint -> OValue JSON files under a dir ───────────── */
typedef struct ODiskCache ODiskCache;

ODiskCache *olang_disk_cache_new(const char *dir);
void olang_disk_cache_free(ODiskCache *dc);

char *olang_disk_cache_default_dir(void); /* malloc'd; caller free */

OValue *olang_disk_cache_get(ODiskCache *dc, const char *fingerprint);
void olang_disk_cache_put(ODiskCache *dc, const char *fingerprint, OValue *value);

/* ── AutonomousScheduler (Nix-family concurrent or serial topo dispatch) ── */
typedef struct OAutonomousScheduler OAutonomousScheduler;

OAutonomousScheduler *olang_autonomous_scheduler_new(void);
OAutonomousScheduler *olang_autonomous_scheduler_new_with_dir(const char *cache_dir);
void olang_autonomous_scheduler_free(OAutonomousScheduler *sch);

void olang_autonomous_scheduler_set_parallelism(OAutonomousScheduler *sch, size_t n);

/* Returns retained OValue or NULL (cache miss). Caller releases. */
OValue *olang_scheduler_cache_get(OAutonomousScheduler *sch, const char *fingerprint);

/* Execute one request (may schedule or run immediate). Returns retained result. */
OValue *olang_scheduler_execute(OAutonomousScheduler *sch, OValue *req);

/* Execute a batch of root requests. If eval_fn provided, used for EVAL-kind.
   Returns a map of fp -> retained OValue (caller must release all values). */
typedef OValue *(*OEvalFn)(OValue *req, void *user);

int olang_scheduler_execute_batch(OAutonomousScheduler *sch,
                                  OValue **roots, size_t roots_len,
                                  OEvalFn eval_fn, void *eval_user,
                                  /* out */ OValueMap **out_results /* map fp->value; caller frees map+values */);

/* Collect all transitive request fingerprints (for dep graph). */
void olang_collect_transitive_requests(OValue *req, OValueMap *out_map /* fp -> req value retained */);

#ifdef __cplusplus
}
#endif

#endif /* O_LANG_SCHEDULER_H */
