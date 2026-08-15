# O-lang C edition — easy native build (C17, no CMake, no Rust)
# Usage:
#   make            # builds ./O (interpreter) and ./olangc (AOT compiler)
#   make clean
#   make test       # smoke tests on core examples (requires python3 + backends/)
#   ./O ../examples/hello.O ../backends
#   mkdir -p /tmp/hello-c17
#   ./olangc ../examples/hello.O -o /tmp/hello-c17/hello && /tmp/hello-c17/hello
#
# Requirements: C compiler (cc/clang/gcc), make, python3 (for python^ shims)
# Optional: nix (for nix_* examples and builtins)
#
# The produced ./olangc and ./O are pure C. Each olangc application is a
# native executable plus its required <executable>.shims directory (and still
# needs python3/nix on the target for languages used by the .O program).

CC ?= cc
CFLAGS = -std=c17 -Wall -Wextra -Wpedantic -O2 -Iinclude \
         -D_POSIX_C_SOURCE=200809L -D_XOPEN_SOURCE=700
LDFLAGS = -pthread
STRICT_CFLAGS = $(CFLAGS) -Werror

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
	@echo "Try: mkdir -p /tmp/hello-c17 && ./olangc ../examples/hello.O -o /tmp/hello-c17/hello && /tmp/hello-c17/hello"

%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

src/main.o: src/main.c
	$(CC) $(CFLAGS) -c $< -o $@

src/olangc.o: src/olangc.c
	$(CC) $(CFLAGS) -c $< -o $@

clean:
	rm -f O olangc tests/test_fresh_env src/*.o src/main.o src/olangc.o
	@echo "Cleaned."

# Smoke test core (no-nix) examples. Adjust path to backends if needed.
# Run from c_cpp/ dir.
test: all
	@echo "=== Testing manifest-selected C17 interpreter examples ==="
	@python3 ../tests/example_manifest.py run \
		--edition c17 --runner ./O --backends ../backends

tests/test_fresh_env: $(CORE_OBJS) tests/test_fresh_env.c
	$(CC) $(CFLAGS) -o $@ tests/test_fresh_env.c $(CORE_OBJS) $(LDFLAGS)

fresh-env-test: tests/test_fresh_env
	@./tests/test_fresh_env ../backends

# Test AOT path for the manifest entries that declare C17 AOT support.
olangc-test: all
	@echo "=== Testing manifest-selected C17 AOT examples ==="
	@python3 ../tests/example_manifest.py run-c17-aot \
		--compiler ./olangc --backends ../backends

# Regression for argv-based compiler launch and literal handling of spaces and
# shell metacharacters in TMPDIR, input/output/shim paths, and CC.
path-test: all
	@sh tests/test_olangc_paths.sh ./olangc "$(CC)" ./O

# Release-clean native + generated-AOT path. OLANGC_WARNINGS_AS_ERRORS makes
# the compiler apply -Werror to the copied runtime and generated_main.c too.
warnings-as-errors:
	@$(MAKE) clean
	@$(MAKE) CFLAGS="$(STRICT_CFLAGS)" all test
	@OLANGC_WARNINGS_AS_ERRORS=1 $(MAKE) CFLAGS="$(STRICT_CFLAGS)" olangc-test path-test

# One-shot run helper: make run EX=hello  (uses ../examples/hello.O)
run:
	@if [ -z "$(EX)" ]; then echo "Usage: make run EX=hello  (or bindings, html_basic, ...)"; exit 1; fi
	@./O ../examples/$(EX).O ../backends

.PHONY: all clean test fresh-env-test olangc-test path-test warnings-as-errors run
