# Recovery tests (W09-C)

Executable coverage lives in:

- `crates/server/src/rollback.rs` unit tests
- `crates/server/tests/rollback.rs` MCP tests
- `crates/store` unfinished-operation replay

Rules: best-effort restore of file bytes, existence, and permission mode.
Do **not** `git reset --hard`. Incomplete restore is `failed_partial`, never
`applied`. Restart does not auto-apply `unknown` operations.
