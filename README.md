# CodeSpace

Personal **execution-tools MCP server**. ChatGPT, Cursor, or another MCP
client decides what to do. This process reads workspace files, applies
Codex-format patches through a pinned Rust `codex-apply-patch` engine,
and runs commands in an isolated Linux environment.

The server is a **Cargo workspace** built with [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)
(stdio and Streamable HTTP). There is no TypeScript gateway and no
native/patch-worker IPC. The patch crate calls Codex **in-process**. A
later runner split is a process boundary; both sides stay Rust.

This is **not**:

- a fork of CoS or cokacremote
- a Codex agent wrapper
- a host that calls a model internally

There are no internal model calls. The MCP client owns judgment; CodeSpace
owns execution contracts: path policy, operation idempotency, patch
apply/rollback reporting, and process lifetime.

## Status

W01 contracts live in `docs/`. Implementation work packages W02–W13 land
as GitHub issues and pull requests on `main` via branches. Do not commit
features directly to `main`.

## License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
