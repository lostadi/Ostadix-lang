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
`parser`, `ir`, `hgraph`, `computation`, `computation_core`, `evidence`,
`placement`, `execution_fabric`, `execution_fabric_authority`, `hosted_remote`,
`world`, and `information`.
The Fabric modules expose frozen authority-free execution records and additive
authenticated attempt authority; a remote result remains provisional, and the
coordinator alone may publish or settle graph state.

`computation_core` exposes the authority-free `OComputationManifestV1` identity
spine and the experimental `OperationContractV1`, `OperationInterfaceV1`,
`RealizationDescriptorV1`, and `RealizationSetV1` records; `computation`
provides higher-level authority-free manifest builders.
Canonical decoding and `verify_realization_set_v1` establish local validation
and exact referential consistency only. They do not plan or select a
realization, prove behavioral equivalence or evidence authenticity, determine
target eligibility or placement, execute or recover work, or grant admission,
capability, lease, or World authority. The complete boundary is documented in
the [operation-realization V1 contract](https://github.com/lostadi/Ostadix-lang/blob/master/docs/OPERATION_REALIZATION_V1.md).

The repository-root `o-lang` package is the compatibility and CLI shell. It
depends one-way on this engine and explicitly reexports the historical module
paths. Engine implementation code does not depend on `o-lang`.

License: LGPL-2.1-only. See `LICENSE` and `NOTICE` in this package.
