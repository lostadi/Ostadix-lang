#ifndef O_LANG_EVAL_H
#define O_LANG_EVAL_H

#include <stdint.h>
#include <stdbool.h>

#include "parser.h"
#include "value.h"
#include "process.h"
#include "scheduler.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Policy for lazy/autonomous ────────────────────────────────────────── */
typedef enum {
    POLICY_EAGER = 0,
    POLICY_LAZY,
    POLICY_AUTONOMOUS
} OPolicy;

/* ── Main evaluator ────────────────────────────────────────────────────── */
typedef struct OEvaluator OEvaluator;

/* Create with default shim dir (e.g. "backends"). Caller owns. */
OEvaluator *olang_evaluator_new(const char *shim_dir);

/* Free evaluator and any cached state / child processes. */
void olang_evaluator_free(OEvaluator *ev);

/* Register the set of known language tags (used by parser + dispatch).
   Pass a StringSet (from parser.h) or NULL for defaults. The set is copied. */
void olang_evaluator_set_registered(OEvaluator *ev, const StringSet *backends);

/* Evaluate a full parsed document (list of top-level nodes).
   Returns a retained OValue (caller must oval_release). */
OValue *olang_evaluator_eval_document(OEvaluator *ev, ONodeList *nodes);

/* Low-level: evaluate a single source fragment (used for O.eval reentrancy).
   Creates a fresh parser + scope for the fragment. */
OValue *olang_eval_source(OEvaluator *ev, const char *src);

/* ── Helpers exposed for generated/standalone mains ────────────────────── */
/* Render a value using the child rules for a given lang (for final output etc.). */
char *olang_render_child(const char *lang, OValue *val);

/* Find a shim path for lang under the evaluator's shim_dir (malloc'd result). */
char *olang_find_shim(OEvaluator *ev, const char *lang);

#ifdef __cplusplus
}
#endif

#endif /* O_LANG_EVAL_H */
