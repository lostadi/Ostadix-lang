#!/usr/bin/env bash
# Compatibility entrypoint; the root setup owns all distro policy.
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/../../setup.sh" "$@"
