#ifndef O_LANG_NIXOS_OPS_HPP
#define O_LANG_NIXOS_OPS_HPP

#include <string>
extern "C" {
#include "value.h"
}

namespace olang {
OValue *activate_nix(OValue *source, const std::string &profile, bool dry_run);
} // namespace olang
#endif
