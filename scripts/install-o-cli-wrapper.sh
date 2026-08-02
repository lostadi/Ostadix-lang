#!/usr/bin/env bash
# Install the repository-owned lowercase `o` dispatcher at one exact path.
#
# On a case-insensitive filesystem the destination is also reached as `O`.
# The generated wrapper uses the spelling preserved in $0 so uppercase `O`
# remains the direct evaluator while lowercase `o` owns repository commands.
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

if [[ $# -ne 1 || -z "$1" ]]; then
    printf 'usage: %s DESTINATION\n' "${0##*/}" >&2
    exit 2
fi

destination=$1
destination_dir=$(dirname -- "$destination")
mkdir -p -- "$destination_dir"

temporary="${destination}.tmp.$$"
trap 'rm -f -- "$temporary"' EXIT HUP INT TERM
cat >"$temporary" <<WRAPPER
#!/usr/bin/env bash
set -euo pipefail
if [[ "\${0##*/}" == "O" ]]; then
    exec "$ROOT/target/release/O" "\$@"
fi
exec "$ROOT/scripts/o-cli.sh" "\$@"
WRAPPER
chmod +x "$temporary"
mv -f -- "$temporary" "$destination"
trap - EXIT HUP INT TERM
