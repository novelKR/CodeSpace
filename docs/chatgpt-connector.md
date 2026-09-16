# ChatGPT connector experiment (W02)

CodeSpace is a Rust `rmcp` process. It speaks **stdio** and **Streamable HTTP**
with the same tool: `workspace_info` only. Exec and patch are not registered yet.

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

Protocol revision observed locally with `rmcp` 3.4.0: stdio negotiated
`2025-11-25`; the HTTP contract test prefers `2026-07-28` with
`2025-11-25` as legacy. ChatGPT's `MCP-Protocol-Version` header was
**not** observed.

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
