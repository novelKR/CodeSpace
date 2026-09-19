# Security and recovery coverage inventory

This is a map of test sources, not a test-run report. Check the CI run for
the exact commit under review before claiming a scenario passed. Coverage
does not establish that every deployment has the same protections.

| Scenario | Coverage | Where |
| --- | --- | --- |
| Path escape `..` | Test source | `tests/security/adversarial.rs`, `crates/server/tests/read_find.rs` |
| Absolute path | Test source | `tests/security/adversarial.rs` |
| Symlink file | Test source | `tests/security/adversarial.rs`, `crates/runner` |
| Special file (FIFO) | Test source | `tests/security/adversarial.rs` |
| Context mismatch | Test source | `tests/security/adversarial.rs` |
| External edit version conflict | Test source | `tests/security/adversarial.rs` |
| Duplicate operation / key reuse | Test source | `tests/recovery/restart.rs`, `crates/server/tests/operations.rs` |
| Partial write failure is not `applied` | Test source | `tests/recovery/restart.rs`, `crates/server/tests/rollback.rs` |
| HTTP 401 / lost transport is not an operation | Test source | `tests/security/adversarial.rs`, `crates/server/tests/http_contract.rs` |
| Restart leaves unfinished as `unknown` | Test source | `tests/recovery/restart.rs`, `crates/store` |
| Shell + patch concurrent write | Test source | `tests/e2e/flow.rs`, `crates/server/tests/process.rs` |
| Invented `process_id` / `operation_id` | Test source | `tests/security/adversarial.rs`, `crates/server/tests/process.rs` |
| Output / time limits | Test source | `crates/server/tests/process.rs` |
| Bearer token not in error body | Test source | `tests/security/adversarial.rs`, `crates/server/src/logging.rs` |
| `workspace_info` → `read` → `apply_patch` → `exec_command` | Test source | `tests/e2e/flow.rs` |
| Linux command and network isolation | Linux-specific test source | `crates/linux-sandbox/tests`, `crates/runner/tests/isolation_files.rs`; requires a usable helper and namespaces. See [runner isolation](../docs/runner-isolation.md). |
| ChatGPT live-account adversarial calls | UNVERIFIED | No ChatGPT Custom Connector in this environment. |
| Kernel/container escape | UNVERIFIED | Out of product scope. |

Known recovery gap: post-apply verification errors can be recorded as
`rejected` after files changed, without entering snapshot restoration. See
[patch behavior](../docs/behavior-differences.md).
