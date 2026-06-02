#ifndef O_LANG_NIX_OPS_HPP
#define O_LANG_NIX_OPS_HPP

#include <string>
extern "C" {
#include "value.h"
}

namespace olang {
OValue *instantiate_nix(OValue *source);
OValue *realise_nix(OValue *source);
} // namespace olang
#endif
