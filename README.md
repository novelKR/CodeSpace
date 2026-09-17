# CodeSpace

[English](README.md) | [한국어](README.ko.md)

Personal **execution-tools MCP server**. ChatGPT, Cursor, or another MCP
client decides what to do. This process reads workspace files, applies
Codex-format patches through a pinned Rust `codex-apply-patch` engine,
and runs managed commands in the registered workspace.

The server is a **Cargo workspace** built with [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)
(stdio and Streamable HTTP). There is no TypeScript gateway and no legacy
`native/patch-worker`. The gateway talks to a Rust `codespace-patch`
helper over JSON stdin/stdout. That helper process calls Codex
**in-process**. `exec_command` currently spawns a **host** process
(`tokio::process::Command`) with the workspace as cwd. Isolated Linux
dispatch is the target runner boundary, not the current exec path.
Opt-in `CODESPACE_RUNNER=uds` talks CodeSpace JSON to
`codespace-codex-runtime`; that is not Linux isolation.

This is **not**:

- a fork of CoS or cokacremote
- a Codex agent wrapper
- a host that calls a model internally
- a completed Linux sandbox runner

There are no internal model calls. The MCP client owns judgment; CodeSpace
owns execution contracts: path policy, operation idempotency, patch
apply/rollback reporting, and process lifetime.

## Status

Live MCP tools include `workspace_info`, `read`, `find`, `apply_patch`,
`operation_status`, `exec_command`, `write_stdin`, `read_process`,
`terminate_process`, `work_open`, `steer_status`, `steer_claim_next`,
`steer_complete`, and `work_finish`. Codex V4A apply is `crates/patch`
inside the `codespace-patch` helper
(see [docs/upstream-lock.md](docs/upstream-lock.md)).
Codex product runtime stays out; primitive reuse:
[docs/codex-reuse.md](docs/codex-reuse.md).
Execution-only (no Responses API):
[docs/execution-substrate.md](docs/execution-substrate.md).
Deferred user intent is edited on HTTP `/inbox` (not MCP).

Current vs target process layout:
[docs/architecture.md](docs/architecture.md).
Install, HTTP/stdio, logs, and recovery:
[docs/operations.md](docs/operations.md).
Do not commit features directly to `main`.

## Run

```bash
cargo run -p codespace-server --bin codespace-mcp
# Streamable HTTP at /mcp (default 127.0.0.1:8787); user inbox at /inbox:
cargo run -p codespace-server --bin codespace-mcp -- --http
```

Tests: `cargo test --workspace` and
`cargo test --manifest-path crates/patch/Cargo.toml` and
`cargo test --manifest-path crates/codex-runtime/Cargo.toml`. ChatGPT Custom
Connector steps and what is **not** verified:
[docs/chatgpt-connector.md](docs/chatgpt-connector.md).
Operator install: [docs/operations.md](docs/operations.md).

## License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
