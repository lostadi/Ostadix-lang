## Summary

Describe the problem and the resulting behavior.

## Compatibility and capacity

List affected public APIs, schemas, protocols, backends, execution paths, and
capacity. State the migration or write “none.”

## Evidence

List exact commands and outcomes. Identify skipped tools, unavailable
environments, and claims the evidence does not establish.

## Checklist

- [ ] Relevant positive and negative tests were added or updated.
- [ ] Public behavior and `[Unreleased]` notes were updated where needed.
- [ ] No old record, schema, or protocol is silently uplifted.
- [ ] Generated/AOT source closure was checked when embedded runtime code changed.
- [ ] This change contains no secrets, credentials, bearer capabilities, or
      publicly disclosed vulnerability details.

