# Origin and Priority Statement

Ostadix-lang / ^Ostadix_, created by Lee Daghlar Ostadi.

The central mechanism is typed expression boundaries:

    LANG^( body )_LANG

A language boundary is not treated as a string wrapper, template block, code
fence, or external FFI layer. The evaluator is part of the expression syntax
itself, and values cross boundaries through OValue.

This repository is the original public implementation of the Ostadix-lang / ^Ostadix_
system, including hosted `.O` orchestration, OIR, backend shims, OValue,
O-core `.oc`, HIR/MIR lowering, and freestanding x86_64 object generation.

Canonical repository:

    https://github.com/lostadi/O-lang

Canonical phrase:

    The nesting is the interface.
