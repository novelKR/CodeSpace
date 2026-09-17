# Security tests (W11)

Executable coverage: `tests/security/adversarial.rs` (wired as
`cargo test -p codespace-server --test security`).

Covers path escape (`..`, absolute), symlink, FIFO, invented
`process_id`, client `approved` claims, version conflict, context
mismatch, and Bearer 401 bodies that must not echo the token.

Shell path sandboxing on the host gateway is **not** claimed. Linux
container isolation is specified in `deploy/` and
[docs/runner-isolation.md](../../docs/runner-isolation.md).
