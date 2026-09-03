# Operation and realization records V1

Status: **experimental, descriptive, and authority-free**.

This milestone gives Ostadix four canonical records for naming one logical
operation and declarations of ways it might be realized. It also gives the
`o operation` front door a bounded inspection and cross-record verification
surface. The implemented result is **referential consistency only**.

It does not plan a realization, select a winner, establish behavioral
equivalence, authenticate evidence, determine target eligibility, place or
execute work, recover a failed attempt, or grant authority.

## The four-record boundary

Construction and reading progress from the semantic obligation toward the
membership record:

```text
OperationContractV1
    declared semantic obligation
            |
            v
OperationInterfaceV1
    stable operation/version and named ports
            |
            v
RealizationDescriptorV1 ... RealizationDescriptorV1
    declarations bound to that exact interface and contract
            |
            v
RealizationSetV1
    exact, nonempty set of descriptor identities
```

Those downward arrows show progression only; they are not stored-reference
direction. The checked identity references point back to the records on which a
declaration depends:

```text
OperationInterfaceV1      -> OperationContractV1
RealizationDescriptorV1   -> OperationInterfaceV1
RealizationDescriptorV1   -> OperationContractV1
RealizationSetV1          -> OperationInterfaceV1
RealizationSetV1          -> OperationContractV1
RealizationSetV1          -> RealizationDescriptorIdV1...
```

Each arrow in this second diagram is an exact identity reference. It is not an
execution edge, proof edge, placement edge, derivation edge, or authorization
edge.

| Rust record | Exact schema | Descriptive role |
|---|---|---|
| `OperationContractV1` | `ostadix.operation-contract/v1` | Names one versioned declared semantic contract and its bounded contract material. |
| `OperationInterfaceV1` | `ostadix.operation-interface/v1` | Names one version of a logical operation, binds the exact contract identity, and declares its named input/output ports and shape parameters. |
| `RealizationDescriptorV1` | `ostadix.realization-descriptor/v1` | Declares one stably named realization against the exact interface and contract, including bounded input/output representation groups and descriptive artifact, pipeline, fidelity, cost-model, and evidence references where supplied. |
| `RealizationSetV1` | `ostadix.realization-set/v1` | Names an exact nonempty, canonically ordered set of realization-descriptor identities for the same interface and contract. |

The types live in `ostadix_api::computation_core`. The compatibility package
reexports the same nominal types through `o_lang::computation_core`; it does not
compile a second schema implementation.

The records are separate artifacts. V1 defines no bundle, catalog, registry,
filesystem layout, or filename-extension convention for assembling them.

### Record anatomy

`SemanticArtifactRefV1` is the common typed reference: a schema token plus one
SHA-256 content identity. It deliberately contains no locator, credential, or
invocation data.

- `OperationContractV1` binds a stable `operation` name and positive
  `semantic_version` to explicit references for preconditions, postconditions,
  state model, effect model, ordering, determinism, and required fidelity.
- `OperationInterfaceV1` repeats the stable operation/version pair, names the
  exact contract identity, and provides canonical shape parameters plus input
  and output ports. Every port points to a typed value-description artifact;
  every shape parameter points to a constraint artifact.
- `RealizationDescriptorV1` has a stable realization name and binds the exact
  interface and contract identities. It names an implementation digest; an
  execution-pipeline reference; accepted input and produced output
  representations by port; target, state, and actor requirements; supplied
  fidelity; an optional cost model; and zero or more validation-evidence
  references.
- `RealizationSetV1` binds the exact interface and contract identities to a
  canonical nonempty vector of realization-descriptor identities. Membership
  conveys no ordering preference, priority, eligibility, or selected winner.

## Canonical encoding and identity

Each record has one deterministic canonical-CBOR form. Its identity is the
SHA-256 digest of a record-specific domain, the canonical byte length as an
unsigned 64-bit big-endian integer, and the canonical bytes:

```text
SHA256(domain || u64_be(canonical_length) || canonical_cbor)
```

The domains are independent:

```text
OSTADIX/OPERATION-CONTRACT/V1\0
OSTADIX/OPERATION-INTERFACE/V1\0
OSTADIX/REALIZATION-DESCRIPTOR/V1\0
OSTADIX/REALIZATION-SET/V1\0
```

These are the typed record IDs used by cross-record references. When a record
is named as an `OComputationManifestV1` facet, `FacetRefV1.content` instead uses
the ordinary SHA-256 of the canonical record bytes, preserving the existing
facet-byte custody contract. The typed record ID and raw facet content digest
are intentionally distinct.

Canonical-CBOR decoding is bounded, validates and canonicalizes the decoded
record, re-encodes it, and requires exact byte equality. An equivalent but
noncanonical CBOR spelling is rejected rather than assigned an identity.

JSON is an inspection/interchange projection. JSON input is size-bounded,
rejects unknown fields, and passes through the same validation and
canonicalization rules. Record identity is still computed from canonical CBOR,
not from JSON whitespace, object-key order, or a filename.

V1 validation includes these local invariants:

- the exact schema coordinate is required;
- semantic versions are positive;
- reserved all-zero content and record identities are rejected;
- record bytes, vectors, names, and nested decode work are bounded;
- named ports, shape parameters, representation groups, evidence references,
  and realization-set member identities use their required canonical sorted and
  unique forms;
- every descriptor representation group is nonempty; and
- every realization set is nonempty.

These rules establish deterministic syntax and identity. They do not establish
that a contract is true, complete, satisfiable, or suitable for a particular
execution.

## Referential verification

`o operation inspect` validates one record in isolation. It does not resolve
the identities named by that record. In particular, inspecting a realization
set reports its canonically ordered declared descriptor IDs as unresolved
references.

`o operation verify` accepts one complete four-record closure and additionally
checks:

- the interface names the supplied contract's exact operation, semantic
  version, and contract identity;
- every supplied descriptor names that exact interface and contract;
- each descriptor's declared input and output representation groups cover
  exactly the interface's named input and output ports;
- the realization set names that exact interface and contract;
- the realization set names exactly the supplied descriptor identities—no
  missing descriptor, duplicate input, or extra descriptor is accepted; and
- stable realization names are unique within the set.

A pass is reported as `Referential consistency: PASS` for human output and as
`referentially_consistent` in the machine-readable result. It means only that
the supplied declarations close over one another under those checks.

An empty validation-evidence list is valid and means declaration-only. A
nonempty list still contains descriptive references only: V1 does not fetch,
parse, authenticate, authorize, or evaluate the referenced evidence.

## CLI

Inspect one explicitly typed record:

```bash
o operation inspect contract contract.json
o operation inspect interface interface.cbor --json
o operation inspect descriptor descriptor.cbor
o operation inspect set realization-set.json --json
```

Verify one exact closure, repeating `--descriptor` once per member declared by
the set:

```bash
o operation verify \
  --contract contract.cbor \
  --interface interface.cbor \
  --descriptor realization-a.cbor \
  --descriptor realization-b.json \
  --set realization-set.cbor
```

`--json` emits one versioned inspection or verification envelope using
`ostadix.operation-inspection/v1` or
`ostadix.operation-verification/v1`. Without `--json`, stdout is a bounded
human-readable summary.

The explicit `contract`, `interface`, `descriptor`, or `set` argument chooses
the decoder. Kind is never inferred from the filename or silently selected from
an embedded schema. A file whose first non-whitespace byte is `{` is treated as
JSON; every other input must be strict canonical CBOR.

Each operation-record input file is limited to 4 MiB. For `o operation verify`,
the CLI uses checked addition over the raw bytes of the contract, interface,
every supplied descriptor, and set files, and rejects a complete input closure
larger than 64 MiB. Independently, realization-set membership and the supplied
descriptor count are each capped at 65,536 and must match exactly. The aggregate
raw-byte cap is a CLI resource boundary; it is not a record field or an input to
any record identity.

Success exits 0. Decode, record-validation, and cross-reference failures exit
1. Command-line usage errors are Clap errors and exit 2. Inspection and
verification are read-only with respect to the supplied artifacts.

## Relationship to semantic custody

`OComputationManifestV1` can classify these records with the facet kinds
`operation_contract`, `operation_interface`, `realization_descriptor`, and
`realization_set`. That vocabulary permits a computation manifest to name
their exact bytes and explicit derivations when a higher layer actually
constructs such a manifest.

The facet names do not automatically attach these records to an
`OComputation`, derive them from source, or prove that a compiler emitted them.
Canonical decoding reconstructs descriptive records, never admission,
placement, dispatch, a live runtime object, or reusable authority.

Existing project `RouteSet`, placement, HGraph, and runtime-observation systems
remain separate. This V1 does not generalize or silently connect their
selection, scheduling, placement, or execution behavior.

## Explicit nonclaims

Operation/realization V1 provides none of the following:

- a planner, objective function, cost evaluation, candidate ranking, selected
  realization, or winning implementation;
- behavioral equivalence, semantic substitutability, property checking, proof
  validation, numerical-fidelity proof, or compiler-derived conformance;
- artifact resolution, artifact availability, installed-runtime discovery,
  evidence authenticity, signer authorization, freshness, or trust;
- target discovery, target eligibility, capacity reservation, placement,
  physical representation or transfer planning, scheduling, dispatch, or
  execution;
- runtime observation, World state, replanning, checkpointing, migration,
  retry, rollback, failover, or recovery; or
- evidence, admission, capability, lease, permission, execution authority, or
  World authority.

Any future implementation of those behaviors requires its own versioned types,
authority boundary, tests, and evidence. It must not reinterpret a V1
referential-consistency pass as permission or proof.
