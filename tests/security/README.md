# Security tests

Executable coverage: `tests/security/adversarial.rs` (wired as
`cargo test -p codespace-server --test security`).

Covers path escape (`..`, absolute), symlink, FIFO, invented
`process_id`, client `approved` claims, version conflict, context
mismatch, and Bearer 401 bodies that must not echo the token.

Command isolation depends on the Linux sandbox helper and its successful
probe. The Compose fixture is not a command backend. See
[runner isolation](../../docs/runner-isolation.md) for host fallback,
enabled-network failure behavior, and the Linux CI checks.
