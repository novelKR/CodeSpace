# Recovery tests

Executable coverage:

- `tests/recovery/restart.rs` (`cargo test -p codespace-server --test recovery`)
- `crates/runner/src/rollback.rs` unit tests
- `crates/server/tests/rollback.rs` MCP tests
- `crates/store` unfinished-operation replay

Rules: best-effort restore of file bytes, existence, and permission mode.
Do **not** `git reset --hard`. Incomplete restore is `failed_partial`, never
`applied`. Restart does not auto-apply `unknown` operations. Duplicate
`operation_key` replays or conflicts; it does not re-run a new write.

Snapshot restoration follows helper-apply failures. Post-apply verification
errors do not currently enter that restoration branch; see
[patch behavior and recovery](../../docs/behavior-differences.md).
