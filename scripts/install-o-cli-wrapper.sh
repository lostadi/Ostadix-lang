#!/bin/sh
# Install the repository-owned `o` dispatcher at one exact path.
#
# On case-sensitive filesystems lowercase `o` and uppercase `O` remain distinct:
# `o` is the dispatcher and `O` is the native evaluator wrapper. On the default
# case-insensitive macOS filesystem they are one directory entry, so both
# spellings intentionally use this dispatcher. Unknown arguments still fall
# through to the native evaluator, and `ostadix-evaluator` is the unambiguous
# raw-evaluator command.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    printf 'usage: %s DESTINATION\n' "${0##*/}" >&2
    exit 2
fi

destination=$1
destination_dir=$(dirname -- "$destination")
mkdir -p -- "$destination_dir"

temporary="${destination}.tmp.$$"
trap 'rm -f -- "$temporary"' EXIT HUP INT TERM
cat >"$temporary" <<WRAPPER
#!/bin/sh
set -eu
exec "$ROOT/scripts/o-cli.sh" "\$@"
WRAPPER
chmod +x "$temporary"
mv -f -- "$temporary" "$destination"
trap - EXIT HUP INT TERM
