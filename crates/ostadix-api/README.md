# ostadix-api

`ostadix-api` is the independent Ostadix runtime engine. It owns the parser,
OIR and HGraph representations, evaluator, backend registry and shims,
evidence/admission pipeline, placement and hosted-runtime machinery, World and
Information contracts, O-core support, and the source inventory used by AOT
generation.

The small embedding entry point is `Runtime`:

```rust
use std::path::PathBuf;

use ostadix_api::{OValue, Runtime};

let mut runtime = Runtime::new(PathBuf::new());
let value = runtime.evaluate("")?;
assert_eq!(value, OValue::Null);
# Ok::<(), ostadix_api::RuntimeError>(())
```

Advanced consumers may use the versioned engine modules directly, including
`parser`, `ir`, `hgraph`, `evidence`, `placement`, `hosted_remote`, `world`,
and `information`.

The repository-root `o-lang` package is the compatibility and CLI shell. It
depends one-way on this engine and explicitly reexports the historical module
paths. Engine implementation code does not depend on `o-lang`.

License: LGPL-2.1-only. See `LICENSE` and `NOTICE` in this package.
