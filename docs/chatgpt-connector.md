# ChatGPT connector experiment (W02)

CodeSpace is a Rust `rmcp` process. It speaks **stdio** and **Streamable HTTP**
with the same live tools (`workspace_info`, `read`, `find`, `apply_patch`,
`operation_status`). Exec tools are not registered yet.

Core protocol baseline is **MCP 2025-11-25**. See
[protocol-compatibility.md](protocol-compatibility.md).

## Local stdio (verified by cargo test)

```bash
cargo run -p codespace-server --bin codespace-mcp -- --transport stdio
```

Cursor-style MCP config:

```json
{
  "mcpServers": {
    "codespace": {
      "command": "cargo",
      "args": ["run", "-p", "codespace-server", "--bin", "codespace-mcp", "--", "--transport", "stdio"],
      "cwd": "/absolute/path/to/CodeSpace"
    }
  }
}
```

## Local Streamable HTTP (experiment)

```bash
export CODESPACE_HTTP_HOST=127.0.0.1
export CODESPACE_HTTP_PORT=8787
# Optional. Leave unset to disable auth. Never log this value.
export CODESPACE_HTTP_TOKEN="replace-me"
cargo run -p codespace-server --bin codespace-mcp -- --transport http
```

Endpoint: `http://127.0.0.1:8787/mcp`

Optional static Bearer is **HTTP experiment only**. It is not an OAuth
server. ChatGPT Custom Connectors often expect OAuth or a different auth
story; **do not assume Bearer works in ChatGPT** until a live account
check says so.

Protocol policy: CI forces **2025-11-25** on stdio and HTTP
(`protocol_compat.rs`). **2026-07-28** is progressive enhancement and is
also forced in that file with **no** legacy fallback. Auto tests that
prefer `2026-07-28` and fall back to `2025-11-25` (`http_contract`,
`transport_contract`) do **not** replace 2025-11-25-only coverage.

ChatGPT's `MCP-Protocol-Version` header was **not** observed. Do not
assume ChatGPT requires 2026-07-28, MRTR, Tasks, subscriptions, or
`Mcp-Name` routing.

## ChatGPT account connection

| Check | Result |
| --- | --- |
| Local stdio `tools/list` + `workspace_info` | `cargo test -p codespace-server` |
| Local Streamable HTTP `tools/list` + `workspace_info` | `cargo test -p codespace-server` |
| ChatGPT Custom Connector against a public HTTPS URL | **Not verified.** This session has no ChatGPT account UI to complete a Custom Connector. Bearer acceptance by ChatGPT is unknown. |
| Public tunnel (ngrok/cloudflare) | **Not verified** |

## Secrets

Never put `CODESPACE_HTTP_TOKEN` in logs, issue comments, or tool error
text. HTTP 401 body is `{ "error": "unauthorized" }` only.
