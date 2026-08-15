# Ostadix versioning and compatibility

Ostadix deliberately has more than one version axis. A package release, a
wire protocol, an admission schema, and a backend catalog answer different
compatibility questions and may advance independently.

Run `O version --json` to inspect the coordinates compiled into the current
interpreter. This report is descriptive: it does not prove that an external
runtime is installed, that a placement is authorized, or that a World is live.

## Version axes

| Axis | Current coordinate | Compatibility meaning |
|---|---|---|
| Rust package | `0.2.0` | Source/library/CLI release identity. SemVer applies only to the documented public façade. |
| Minimum Rust | `1.93.1` | Lowest compiler checked by the MSRV lane. |
| Release toolchain | `1.97.1` | Pinned compiler used for release, formatting, Clippy, and generated-runtime evidence. |
| Execution intent | `oexec.execution-intent/v1` | Stable, authority-free identity of exact source and analyzed semantics. |
| Evidence | `oexec.evidence/v5` | Pre-execution evidence vocabulary. |
| Admission | `oexec.admission/v5` | Live process-local admitted-execution contract. |
| Backend catalog | `ostadix.backend-catalog/v4` | Canonical backend specification and implementation-identity projection. |
| Hosted transport | `ostadix.hosted-transport/v1`, `/v2` | Frozen single-operation V1 and opt-in durable-session V2 wire contracts. |
| World records | schema/wire V1 | Offline World identity, value, receipt, and record codecs. |

“V6” in Hosted Placement V6 names the placement milestone and evidence model;
it is not a promise that every schema or transport is numerically version 6.

## Compatibility rules

1. Wire and signed-evidence decoders validate their exact schema. No version is
   silently relabeled or uplifted.
2. A stable execution intent proves sameness of modeled input, not authority,
   current runtime availability, or reusable admission.
3. Backend-catalog generation changes invalidate identities derived from the
   older catalog. Regenerate profiles, warrants, and short-lived evidence.
4. Hosted V1 and V2 are separate protocols. Supporting V2 does not mutate the
   frozen V1 contract.
5. World wire versions cover canonical offline records; they do not claim a
   live World transport or Governor service.
6. The `o_lang::api` façade is the intended embedding surface. Historical
   top-level modules remain available during the 0.2 compatibility period but
   are not all promised as stable external contracts.

## Changing a coordinate

Any version change must update its canonical constant, compatibility tests,
machine-readable version report, release documentation, generated/AOT source
closure where applicable, and mutation/rejection tests for the prior identity.
Package, MSRV, toolchain, and citation metadata are validated separately so a
toolchain upgrade does not masquerade as a package or protocol release.

