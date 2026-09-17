# W11 adversarial report

Scenarios from the original plan §12. `PASS` means an executable test in
this repo asserts the behavior. `UNVERIFIED` is in scope for honesty, not a
silent skip of a required gate.

| Scenario | Result | Where |
| --- | --- | --- |
| Path escape `..` | PASS | `tests/security/adversarial.rs`, `crates/server/tests/read_find.rs` |
| Absolute path | PASS | `tests/security/adversarial.rs` |
| Symlink file | PASS | `tests/security/adversarial.rs`, `crates/runner` |
| Special file (FIFO) | PASS | `tests/security/adversarial.rs` |
| Context mismatch | PASS | `tests/security/adversarial.rs` |
| External edit version conflict | PASS | `tests/security/adversarial.rs` |
| Duplicate operation / key reuse | PASS | `tests/recovery/restart.rs`, `crates/server/tests/operations.rs` |
| Partial write failure is not `applied` | PASS | `tests/recovery/restart.rs`, `crates/server/tests/rollback.rs` |
| HTTP 401 / lost transport is not an operation | PASS | `tests/security/adversarial.rs`, `crates/server/tests/http_contract.rs` |
| Restart leaves unfinished as `unknown` | PASS | `tests/recovery/restart.rs`, `crates/store` |
| Shell + patch concurrent write | PASS | `tests/e2e/flow.rs`, `crates/server/tests/process.rs` |
| Invented `process_id` / `operation_id` | PASS | `tests/security/adversarial.rs`, `crates/server/tests/process.rs` |
| Output / time limits | PASS | `crates/server/tests/process.rs` |
| Bearer token not in error body | PASS | `tests/security/adversarial.rs`, `crates/server/src/logging.rs` |
| `workspace_info` → `read` → `apply_patch` → `exec_command` | PASS | `tests/e2e/flow.rs` |
| Shell isolation against `/etc/passwd` | UNVERIFIED | Gateway `exec_command` is host argv + workspace cwd, not a kernel sandbox. Linux runner spec is in `deploy/` / W05. |
| ChatGPT live-account adversarial calls | UNVERIFIED | No ChatGPT Custom Connector in this environment. |
| Kernel/container escape | UNVERIFIED | Out of product scope. |

Required completion gates (path escape on file tools, partial failure
≠ `applied`, duplicate requests do not re-run, concurrent writes locked,
no Bearer in errors) are PASS.
