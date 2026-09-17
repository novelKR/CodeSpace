# 프로토콜 호환성

[English](../protocol-compatibility.md) | [한국어](protocol-compatibility.md)

CodeSpace는 실행 도구 MCP 서버입니다. 바깥 클라이언트가 무엇을 할지
고릅니다. 이 프로세스는 모델을 호출하지 않습니다.

## 핵심 기준

| 계층 | 기준 | 참고 |
| --- | --- | --- |
| Protocol | **MCP 2025-11-25** | 1급. CI가 stdio와 Streamable HTTP에서 이 리비전을 강제합니다. |
| Transport | **stdio**와 **Streamable HTTP `/mcp`** | 양쪽 모두 같은 도구 계약입니다. |
| HTTP floor | **2025-03-26** | Streamable HTTP가 있는 첫 리비전. CI 핀은 아닙니다. |
| Progressive enhancement | **2026-07-28** | 선택. 같은 도구 의미. 핵심에 절대 필수 아님. |

핵심 실행에 필요한 원시 연산: `initialize`, `tools/list`,
`tools/call`. 현재 실제 도구는 `workspace_info`, `read`, `find`,
`apply_patch`, `operation_status`, `exec_command`, `write_stdin`,
`read_process`, `terminate_process`, `work_open`, `steer_status`,
`steer_claim_next`, `steer_complete`, `work_finish`입니다. 여전히
`tools/call`을 사용합니다. 사용자 초안과 재정렬은 MCP가 아니라 HTTP
`/inbox`에 있습니다.

## 핵심이 요구하면 안 되는 것

이 2026-07-28(및 관련) 기능은 나중에 UX나 가속으로 협상할 수 있습니다.
**핵심 계약의 일부가 아니며** `tools/call`을 막으면 안 됩니다.

- MRTR (multi-round-trip requests)
- Tasks
- subscriptions
- `Mcp-Method` / `Mcp-Name` 헤더 라우팅 (SEP-2243)
- 애플리케이션 상태로서의 2026-07-28 무상태 수명주기

인증과 라우팅은 Bearer 미들웨어(선택)와 `/mcp` → rmcp 도구 디스패치로
남습니다. 디스패치는 본문에서 JSON-RPC 메서드와 도구 이름을 읽습니다.
`Mcp-Name`으로 라우팅하지 않습니다.

`crates/domain`, `crates/policy`, `crates/patch`, `crates/store`, `crates/runner`는
`ProtocolVersion`이나 `NegotiatedFeatures`를 가져오지 않습니다. 서버
어댑터 `crates/server/src/protocol.rs`가 협상된 리비전을 향상 플래그로
매핑합니다. 그 플래그가 참이어도 핸들러는 `tools/call`을 유지합니다.
work/steer는 애플리케이션 상태(`work_id` / `intent_id`)이며 MCP Tasks,
MRTR, subscriptions가 아닙니다.

이후 **승인** 또는 장시간 **Tasks**(
[execution-substrate.md](execution-substrate.md) 참고)는 2025-11-25
`tools/call` 경로를 유지해야 합니다. 추가 권한은 현재 정책 거절입니다.
MRTR을 요구하기 전에 미래의 `approval_*` 폴백이 옵니다. 대화형
프로세스는 `process_id` 핸들로 남습니다. Tasks가 이를 대체하면 안
됩니다.

## 2026-07-28

클라이언트가 2026-07-28을 협상하면 서버는 향상 플래그를 광고할 수
있습니다. 실제 도구의 의미는 2025-11-25와 동일합니다. 이 리비전은
**점진적 향상 전용**입니다.

2026-07-28을 선호하고 2025-11-25로 폴백하는 기존 Auto 시험은 폴백을
증명합니다. 2025-11-25 전용 커버리지를 **대체하지 않습니다**.

## 범위 밖

- **2024-11-05 HTTP+SSE.** 목표가 아닙니다. `rmcp` 3.x는 그 전송을
  제공하지 않습니다.
- MRTR, Tasks, subscriptions 구현. 선택적 점진적 향상으로 남으며
  work/steer, exec, patch, 또는 이후 승인 흐름에 **필수가 아닙니다**.
- 브라우저 Inbox UI(이 릴리스에는 HTTP `/inbox` JSON이 있습니다).

## 시험

`crates/server/tests/protocol_compat.rs`가 핀합니다.

1. stdio와 HTTP에서 강제 **2025-11-25** (`ClientLifecycleMode::Initialize`
   + `with_protocol_version(V_2025_11_25)`).
2. HTTP와 stdio에서 강제 **2026-07-28**, 레거시 폴백 **없음**
   (`Auto { preferred_versions: [V_2026_07_28], legacy_version: None }`).

핸드셰이크 후 `tools/list`는 `workspace_info`를 포함하고 `tools/call`은
기존 페이로드 계약과 맞습니다. 강제 2025-11-25는 `read` / `apply_patch` /
`exec_command` / `operation_status`와 work/steer 체크포인트(`work_open`
→ `/inbox` queue → `steer_claim_next` → `work_finish`)도 다룹니다. 강제
2026-07-28은 같은 `tools/call` 표면을 사용합니다.
