# O-lang C edition — easy native build (C17, no CMake, no Rust)
# Usage:
#   make            # builds ./O (interpreter) and ./olangc (AOT compiler)
#   make clean
#   make test       # smoke tests on core examples (requires python3 + backends/)
#   ./O ../examples/hello.O ../backends
#   ./olangc ../examples/hello.O -o /tmp/hello_c && /tmp/hello_c
#
# Requirements: C compiler (cc/clang/gcc), make, python3 (for python^ shims)
# Optional: nix (for nix_* examples and builtins)
#
# The produced ./olangc and ./O are pure C. olangc-produced binaries
# are self-contained native executables (still need python3/nix on target
# for the languages the .O program uses).

CC ?= cc
CFLAGS = -std=c17 -Wall -Wextra -Wpedantic -O2 -Iinclude \
         -D_POSIX_C_SOURCE=200809L -D_XOPEN_SOURCE=700
LDFLAGS = -pthread

# Core runtime (pure C)
CORE_SRCS = \
    src/value.c \
    src/parser.c \
    src/process.c \
    src/eval.c \
    src/scheduler.c \
    src/nix_ops.c \
    src/nixos_ops.c

CORE_OBJS = $(CORE_SRCS:.c=.o)

# Final binaries
all: O olangc

O: $(CORE_OBJS) src/main.o
	$(CC) $(CFLAGS) -o $@ $^ $(LDFLAGS)
	@echo "Built ./O (interpreter)"

olangc: $(CORE_OBJS) src/olangc.o
	$(CC) $(CFLAGS) -o $@ $^ $(LDFLAGS)
	@echo "Built ./olangc (AOT compiler)"
	@echo "Try: ./olangc ../examples/hello.O -o /tmp/hello_c && /tmp/hello_c"

%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

src/main.o: src/main.c
	$(CC) $(CFLAGS) -c $< -o $@

src/olangc.o: src/olangc.c
	$(CC) $(CFLAGS) -c $< -o $@

clean:
	rm -f O olangc src/*.o src/main.o src/olangc.o
	rm -rf /tmp/o-build-* /tmp/o_shims_* /tmp/hello_c /tmp/meta_c 2>/dev/null || true
	@echo "Cleaned."

# Smoke test core (no-nix) examples. Adjust path to backends if needed.
# Run from c_cpp/ dir.
test: all
	@echo "=== Testing O (interpreter) on core examples ==="
	@./O ../examples/hello.O ../backends | grep -q '2' && echo "hello.O: OK"
	@./O ../examples/bindings.O ../backends | grep -q '43' && echo "bindings.O: OK"
	@./O ../examples/trailing_expr.O ../backends | grep -q '42' && echo "trailing_expr.O: OK"
	@./O ../examples/meta_eval.O ../backends > /tmp/meta.out 2>&1 && grep -q '1024\|42' /tmp/meta.out && echo "meta_eval.O (quote/O.eval): OK" || (echo "meta_eval failed; output:"; cat /tmp/meta.out; exit 1)
	@./O ../examples/html_basic.O ../backends | grep -q 'The answer is 42' && echo "html_basic.O: OK"
	@echo "=== All core smoke tests passed ==="

# Test AOT path: compile an example with olangc and run the result.
olangc-test: all
	@echo "=== Testing olangc AOT on hello.O ==="
	@./olangc ../examples/hello.O -o /tmp/hello_c
	@/tmp/hello_c | grep -q '2' && echo "olangc hello -> /tmp/hello_c: OK"
	@echo "=== Testing olangc AOT on meta_eval.O (homoiconicity) ==="
	@./olangc ../examples/meta_eval.O -o /tmp/meta_c
	@/tmp/meta_c > /tmp/meta_c.out 2>&1 && grep -q '1024\|42' /tmp/meta_c.out && echo "olangc meta_eval: OK" || (echo "AOT meta failed:"; cat /tmp/meta_c.out; exit 1)
	@echo "AOT smoke OK. Binaries are self-contained C (no cargo)."

# One-shot run helper: make run EX=hello  (uses ../examples/hello.O)
run:
	@if [ -z "$(EX)" ]; then echo "Usage: make run EX=hello  (or bindings, meta_eval, html_basic, ...)"; exit 1; fi
	@./O ../examples/$(EX).O ../backends

.PHONY: all clean test olangc-test run
