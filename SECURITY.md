# Security policy

Ostadix is research-stage software. Its checked-in tests establish bounded
claims; they do not constitute a production-readiness, isolation, or hardware
security certification.

## Report a vulnerability privately

Prefer GitHub private vulnerability reporting:

<https://github.com/lostadi/Ostadix-lang/security/advisories/new>

Do not disclose a suspected vulnerability in public issues, discussions, pull
requests, patches, logs, or demonstrations. If private vulnerability reporting
is temporarily unavailable, retain the details privately until a private
channel with the maintainer is available. This repository does not publish a
security-reporting email address.

Include only information needed to reproduce and assess the report:

- affected commit, package coordinate, binary, protocol, or schema;
- local environment and the smallest safe reproducer;
- observed impact and the boundary an attacker must cross;
- whether secrets, capabilities, signed records, or third-party systems are
  involved; and
- any proposed mitigation or embargo constraint.

Do not include live credentials, private keys, bearer capabilities, personal
data, or unnecessary exploit payloads. Use inert fixtures and redact paths or
identities when possible.

## Supported targets

| Target | Security handling |
|---|---|
| Current default branch | Reports are assessed against the current source and its documented claim boundaries. |
| Older commits, tags, generated artifacts, or downstream forks | No standing backport commitment; handling is decided case by case. |

The package coordinate in `Cargo.toml` is not, by itself, proof that a public
release exists or remains supported. No response or remediation SLA is
promised. The maintainer will coordinate disclosure and credit through the
private advisory when practical.

Security fixes should preserve execution capability and capacity where the
threat model permits. Any unavoidable compatibility reduction, disabled path,
or narrower guarantee must be made explicit and versioned where applicable.

