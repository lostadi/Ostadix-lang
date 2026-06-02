#ifndef O_LANG_NIX_OPS_H
#define O_LANG_NIX_OPS_H

#include "value.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Nix rung climb operations. Return retained OValue (next rung) or NULL on error.
   Errors are printed to stderr; caller should check. */

OValue *olang_instantiate_nix(OValue *source /* ONixExpr */);
OValue *olang_realise_nix(OValue *source /* ODerivation */);

/* activate expects OStorePath (or compatible). profile may be NULL for default.
   dry_run: if true, use dry-activate even if env allows real. */
OValue *olang_activate_nix(OValue *source /* OStorePath */, const char *profile, bool dry_run);

/* Returns a synthetic OSystem for current (no activation performed). */
OValue *olang_current_system(void);

#ifdef __cplusplus
}
#endif

#endif /* O_LANG_NIX_OPS_H */
