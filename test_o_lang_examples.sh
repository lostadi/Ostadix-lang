#!/usr/bin/env bash
set -euo pipefail

cargo run --quiet -- examples/bindings.O >/tmp/o-bindings.out 2>/tmp/o-bindings.err
grep -q 'Int' /tmp/o-bindings.out
grep -q 'v: 43' /tmp/o-bindings.out

cargo run --quiet -- examples/nested_splice.O >/tmp/o-nested.out 2>/tmp/o-nested.err
grep -q 'v: 42' /tmp/o-nested.out

cargo run --quiet -- examples/html_escape.O >/tmp/o-html-escape.out 2>/tmp/o-html-escape.err
grep -q '&lt;O-lang &amp; friends&gt;' /tmp/o-html-escape.out

cargo run --quiet -- examples/html_raw_roundtrip.O >/tmp/o-html-roundtrip.out 2>/tmp/o-html-roundtrip.err
grep -q '<section>' /tmp/o-html-roundtrip.out
grep -q '<strong>Hello, Lee.</strong>' /tmp/o-html-roundtrip.out

cargo run --quiet -- examples/nix_basic.O >/tmp/o-nix-basic.out 2>/tmp/o-nix-basic.err
grep -q 'Nix inside O-lang' /tmp/o-nix-basic.out
grep -q 'O-lang Nix bridge' /tmp/o-nix-basic.out
grep -q '42' /tmp/o-nix-basic.out

cargo run --quiet -- examples/nix_python_html.O >/tmp/o-nix-python-html.out 2>/tmp/o-nix-python-html.err
grep -q 'Nix → Python → HTML' /tmp/o-nix-python-html.out
grep -q 'Nix-born value says answer=42' /tmp/o-nix-python-html.out

cargo run --quiet -- examples/nix_storepath.O >/tmp/o-nix-storepath.out 2>/tmp/o-nix-storepath.err
grep -q 'O-lang StorePath test' /tmp/o-nix-storepath.out
grep -q '/nix/store/' /tmp/o-nix-storepath.out
grep -q 'hello-from-o-lang.txt' /tmp/o-nix-storepath.out
grep -q 'class="o-store-path"' /tmp/o-nix-storepath.out

cargo run --quiet -- examples/nix_storepath_python.O >/tmp/o-nix-storepath-python.out 2>/tmp/o-nix-storepath-python.err
grep -q 'Python reads Nix StorePath' /tmp/o-nix-storepath-python.out
grep -q 'Hello from O-lang + Nix' /tmp/o-nix-storepath-python.out

cargo run --quiet -- examples/coordination_groups.O >/tmp/o-coordination.out 2>/tmp/o-coordination.err
grep -q 'Step-4 coordination primitives' /tmp/o-coordination.out
grep -q '<group:batch n=3' /tmp/o-coordination.out
grep -q 'pkgs.hello' /tmp/o-coordination.out

echo "All O-lang smoke tests passed."
