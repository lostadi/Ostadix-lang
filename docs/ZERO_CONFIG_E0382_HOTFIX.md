# Zero-configuration client ownership hotfix

## Observed failure

Rust correctly rejected two client constructors that moved the owned
`resolved.address: String` and then borrowed the partially moved `resolved`
structure to call `resolved.tls_identity()`.

## Correction

The TLS identity is now cloned from the still-intact resolved connection first:

```rust
let tls_identity = resolved.tls_identity();
let mut client = HostedNodeClient::new(resolved.address, tls_identity);
```

The V2 constructor uses the same ordering. The transport address, TLS identity,
receipt key, and timeout values are unchanged; only the ownership-safe order in
which they are extracted changes.

## Scope

This is an implementation correction, not a new user-facing requirement. No
new flag, path, credential, hostname, or other configuration is introduced.
