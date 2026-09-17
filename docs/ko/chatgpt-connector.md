# ChatGPT 커넥터 실험

[English](../chatgpt-connector.md) | [한국어](chatgpt-connector.md)

CodeSpace는 Rust `rmcp` 프로세스입니다. **stdio**와 **Streamable HTTP**를
말하며 같은 실제 도구를 제공합니다(`workspace_info`, `read`, `find`,
`apply_patch`, `operation_status`, `exec_command`, `write_stdin`,
`read_process`, `terminate_process`, `work_open`, `steer_status`,
`steer_claim_next`, `steer_complete`, `work_finish`). 사용자 초안은 MCP가
아니라 HTTP `/inbox`입니다. `exec_command`는 현재 호스트 프로세스입니다.
compose 파일은 격리 픽스처이며 ChatGPT 연결 경로가 아닙니다.

핵심 프로토콜 기준은 **MCP 2025-11-25**입니다.
[protocol-compatibility.md](../protocol-compatibility.md)를 보세요.

## 로컬 stdio (cargo test로 검증)

```bash
cargo run -p codespace-server --bin codespace-mcp -- --transport stdio
```

Cursor 스타일 MCP 설정:

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

## 로컬 Streamable HTTP (실험)

```bash
export CODESPACE_HTTP_HOST=127.0.0.1
export CODESPACE_HTTP_PORT=8787
# Optional. Leave unset to disable auth. Never log this value.
export CODESPACE_HTTP_TOKEN="replace-me"
cargo run -p codespace-server --bin codespace-mcp -- --transport http
```

엔드포인트: `http://127.0.0.1:8787/mcp`

선택적 정적 Bearer는 **HTTP 실험 전용**입니다. OAuth 서버가 아닙니다.
ChatGPT Custom Connector는 종종 OAuth나 다른 인증 이야기를 기대합니다.
실제 계정 검사가 그렇게 말하기 전에는 **ChatGPT에서 Bearer가 동작한다고
가정하지 마세요**.

프로토콜 정책: CI는 stdio와 HTTP에서 **2025-11-25**를 강제합니다
(`protocol_compat.rs`). **2026-07-28**은 점진적 향상이며 그 파일에서
레거시 폴백 **없이** 강제됩니다. `2026-07-28`을 선호하고 `2025-11-25`로
폴백하는 자동 시험(`http_contract`, `transport_contract`)은 2025-11-25
전용 커버리지를 **대체하지 않습니다**.

ChatGPT의 `MCP-Protocol-Version` 헤더는 **관찰되지 않았습니다**. ChatGPT가
2026-07-28, MRTR, Tasks, subscriptions, 또는 `Mcp-Name` 라우팅을 요구한다고
가정하지 마세요.

## ChatGPT 계정 연결

| 검사 | 결과 |
| --- | --- |
| Local stdio `tools/list` + `workspace_info` | `cargo test -p codespace-server` |
| Local Streamable HTTP `tools/list` + `workspace_info` | `cargo test -p codespace-server` |
| ChatGPT Custom Connector against a public HTTPS URL | **Not verified.** This session has no ChatGPT account UI to complete a Custom Connector. Bearer acceptance by ChatGPT is unknown. |
| Public tunnel (ngrok/cloudflare) | **Not verified** |

## 비밀

로그, 이슈 댓글, 도구 오류 텍스트에 `CODESPACE_HTTP_TOKEN`을 넣지 마세요.
HTTP 401 본문은 `{ "error": "unauthorized" }`뿐입니다.
