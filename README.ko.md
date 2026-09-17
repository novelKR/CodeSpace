# CodeSpace

[English](README.md) | [한국어](README.ko.md)

개인용 **실행 도구 MCP 서버**입니다. ChatGPT, Cursor 또는 다른 MCP
클라이언트가 무엇을 할지 판단합니다. 이 프로세스는 워크스페이스 파일을
읽고, 고정된 Rust `codex-apply-patch` 엔진으로 Codex 형식 패치를 적용하며,
등록된 워크스페이스에서 관리형 명령을 실행합니다.

서버는 [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)로 만든
**Cargo 워크스페이스**입니다(stdio와 Streamable HTTP). TypeScript
게이트웨이와 폐기된 `native/patch-worker`는 없습니다. 게이트웨이는 JSON
stdin/stdout으로 Rust `codespace-patch` 헬퍼와 대화합니다. 그 헬퍼
프로세스는 Codex를 **프로세스 내부에서** 호출합니다. `exec_command`는
현재 워크스페이스를 cwd로 하는 **호스트** 프로세스
(`tokio::process::Command`)를 띄웁니다. 격리된 Linux 디스패치가 목표
러너 경계이며, 현재 exec 경로는 아닙니다.

다음이 **아닙니다**.

- CoS 또는 cokacremote의 포크
- Codex 에이전트 래퍼
- 내부에서 모델을 호출하는 호스트
- 완료된 Linux 샌드박스 러너

내부 모델 호출은 없습니다. MCP 클라이언트가 판단을 소유하고, CodeSpace는
실행 계약을 소유합니다. 경로 정책, 작업 멱등성, 패치 적용/롤백 보고,
프로세스 수명입니다.

## 상태

실제 MCP 도구는 `workspace_info`, `read`, `find`, `apply_patch`,
`operation_status`, `exec_command`, `write_stdin`, `read_process`,
`terminate_process`, `work_open`, `steer_status`, `steer_claim_next`,
`steer_complete`, `work_finish`입니다. Codex V4A 적용은
`codespace-patch` 헬퍼 안의 `crates/patch`입니다
([docs/upstream-lock.md](docs/upstream-lock.md) 참고).
Codex 제품 런타임은 제외합니다. 프리미티브 재사용:
[docs/codex-reuse.md](docs/codex-reuse.md).
실행 전용(Responses API 없음):
[docs/execution-substrate.md](docs/execution-substrate.md).
지연된 사용자 의도는 MCP가 아니라 HTTP `/inbox`에서 편집합니다.

현재와 목표 프로세스 배치:
[docs/architecture.md](docs/architecture.md).
설치, HTTP/stdio, 로그, 복구:
[docs/operations.md](docs/operations.md).
기능을 `main`에 직접 커밋하지 마세요.

## 실행

```bash
cargo run -p codespace-server --bin codespace-mcp
# Streamable HTTP at /mcp (default 127.0.0.1:8787); user inbox at /inbox:
cargo run -p codespace-server --bin codespace-mcp -- --http
```

시험: `cargo test --workspace`와
`cargo test --manifest-path crates/patch/Cargo.toml`. ChatGPT Custom
Connector 절차와 **검증하지 않은** 항목:
[docs/chatgpt-connector.md](docs/chatgpt-connector.md).
운영자 설치: [docs/operations.md](docs/operations.md).

## 라이선스

Apache License 2.0. [LICENSE](LICENSE)와 [NOTICE](NOTICE)를 보세요.
