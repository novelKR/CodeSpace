# End-to-end tests (W11)

Executable coverage: `tests/e2e/flow.rs` (`cargo test -p codespace-server --test e2e`).

Happy path: `workspace_info` → `read` → `apply_patch` → `exec_command` →
`read_process`. Concurrent shell + patch is `WORKSPACE_BUSY`.

Not verified here: ChatGPT Custom Connector, a second person's laptop,
or a production Linux runner.
