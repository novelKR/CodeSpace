# 아키텍처

[English](../architecture.md) | [한국어](architecture.md)

CodeSpace는 개인용 **실행 도구 MCP 서버**입니다. 바깥 클라이언트
(ChatGPT, Cursor, 또는 다른 MCP 호스트)가 무엇을 할지 판단합니다. 이
프로세스는 모델을 호출하지 않습니다. 파일을 읽고, 핀된 Rust 엔진으로
Codex V4A 패치를 적용하며, 등록된 워크스페이스에서 관리형 명령을
실행합니다.

## 정체성

| 해당 | 해당하지 않음 |
| --- | --- |
| Independent MCP server | Fork of CoS or cokacremote |
| Execution-only environment + contract | Codex agent / App Server wrapper |
| File read / patch / managed exec | Internal chat, Goal/Loop, multi-agent, Responses API |
| Gateway policy + (target) OS/container isolation | Kernel sandbox equivalent to Codex CLI |

빌린 아이디어(코드 덤프 아님):

- cokacremote에서: 헤드리스 서버, Streamable HTTP, **요청 수명**과
  **프로세스 수명**을 분리.
- CoS에서: 승인된 워크스페이스, 엔진 전 헝크별 경로 해석, 프리플라이트,
  최선을 다하는 롤백, 분리된 도구 표면.
- Codex에서: 원본 Rust `codex-apply-patch` 파싱 / 검증 / 적용, 그리고
  이후 Runner 뒤에 격리된 응집력 있는 **실행 서브그래프**(hardening,
  PTY, UDS, path, filesystem, Linux sandbox, network). Codex는 구현
  의존성이며 제어 평면이 아닙니다
  ([codex-reuse.md](codex-reuse.md)).

가져오지 않은 것: Electron, Chrome 확장, ChatGPT DOM, agents spawn,
Desktop, 플러그인 마켓플레이스, TypeScript `apply-patch` 포트,
`git apply --unsafe-paths`, 독립 `apply_patch` 바이너리를 보안 경계로
감싸기(그 경로는 sandbox `None`을 쓰고 기본적으로 심링크를 따름),
TypeScript MCP 게이트웨이, 폐기된 `native/patch-worker` 트리, 또는 어떤
아웃바운드 모델 클라이언트. 실행 전용 불변식:
[execution-substrate.md](execution-substrate.md).

## 현재 배치

MVP 기본은 **호스트 프로세스 하나**입니다: `codespace-mcp`와 프로세스
내부 `Runner`. `exec_command`는 compose로 디스패치되지 않습니다.
[`deploy/compose.yml`](../../deploy/compose.yml)은 격리 **픽스처**일
뿐입니다. 선택적 Unix 소켓 전송(`CODESPACE_RUNNER=uds`)은 CodeSpace
JSON으로 `codespace-codex-runtime`과 대화합니다. 그것은 Linux 격리가
아니고 기본값도 아닙니다.

```text
CURRENT

MCP Client
   │
   ▼
codespace-mcp  (host gateway)
   ├─ policy / store / coordination / operation persist
   ├─ structured logging (stderr tracing)
   │
   │  Runner execution DTO
   │  (command/exec shape; no work_id / operation_id / coordination)
   ▼
RuntimeBackend
   ├─ default: InProcessRunner
   └─ opt-in: UdsRunner (CODESPACE_RUNNER=uds)
          │ length-prefixed CodeSpace JSON (protocol 1, request_id rrpc-…)
          ▼
     codespace-codex-runtime
          ├─ codex-process-hardening
          ├─ codex-uds bind
          └─ InProcessRunner (same methods as default)
                 ├─ read / find / version (PathSandbox)
                 ├─ apply_patch (one transaction)
                 │      expected versions → preflight → snapshot
                 │      → helper apply → verify → rollback
                 │              │ JSON stdin/stdout
                 │              ▼
                 │         codespace-patch (host child)
                 │              └─ Codex Rust crate in-process
                 └─ exec / stdin / read / terminate
                        └─ host process (tokio::process::Command,
                           cwd = workspace root, env from runner-local defaults)

deploy/compose.yml
   └─ isolation fixture only; not connected to exec_command
```

```text
MCP JSON  →  domain params  →  gateway (policy/store)  →  Runner DTO  →  RuntimeBackend
                 │
                 └─ rmcp / JsonSchema stay on MCP types, not on runner DTOs
```

게이트웨이는 **어느 워크스페이스에서 무엇을 해도 되는지**를 소유합니다.
토큰, 서버 설정, 워크스페이스 레지스트리, 환경, operations 데이터베이스가
여기에 있습니다. MCP 파라미터를 러너 DTO로 매핑하며
`ExecCommandParams`를 러너에 넘기지 **않습니다**. 도구에는 여전히
`environment_id`가 없습니다.

`crates/runner`는 `Runner` 트레이트, 실행 DTO, `InProcessRunner`
(파일시스템, `apply_patch` 트랜잭션 하나, 호스트 프로세스 감독),
`UdsRunner`(Unix 소켓 클라이언트), compose 픽스처 검사를
소유합니다. 기본 백엔드는 프로세스 내부입니다. 워커 바이너리는 격리된
`crates/codex-runtime` / `codespace-codex-runtime`입니다.

`codespace-patch`는 제품 헬퍼 프로세스이며, 업스트림 독립 `apply_patch`
바이너리가 아니고 `native/patch-worker`도 아닙니다. Runner ↔ 헬퍼는
JSON stdin/stdout입니다. Codex 자체는 **그 헬퍼 안에서** 프로세스
내부로 실행됩니다.

MVP에는 단일 인스턴스로 충분합니다. SQLite는 **패치 작업**과
works/intents를 저장합니다. 프로세스 핸들과 자원 잠금(워크스페이스
배타 쓰기 / 셸 점유)은 메모리에 있습니다. 메시지 브로커는 없습니다.

게이트웨이 단위 시험은 macOS 개발 호스트에서 실행할 수 있습니다. Linux
컨테이너는 **목표** 격리 OS이며 현재 exec 경계가 아닙니다. 등록된
`linux-container` 환경은 닫힌 실패입니다(`UNAUTHORIZED`).

## 목표 배치

이후 러너 분리는 양쪽 모두 Rust로 남습니다. Codex를 호출하려고
TypeScript 게이트웨이나 `native/patch-worker`를 다시 들이지 마세요.

```text
TARGET

MCP Client
   │
   ▼
Gateway
   │ authorized RunnerRequest
   │ (policy, operation_key replay, write lock,
   │  dispatch, result persistence)
   ▼
Runner process boundary
   │
   ├─ filesystem
   ├─ patch transaction (one runner-side operation)
   └─ process supervisor
          │
          ▼
   isolated Linux workspace
```

목표 **도메인**(실제 MCP 필드 아님): Environment(어디), Workspace(무엇),
PermissionProfile(해도 되는지), Operation(이 RPC). 도구에
`environment_id`를 넣지 마세요. 운영자 설정은 환경을 등록할 수
있습니다.
[execution-substrate.md](execution-substrate.md)를 보세요.

Unix 소켓 **전송**(`UdsRunner`)은 기존 `Runner` / `InProcessRunner`
타입 뒤에 선택적으로 있습니다. Host + UDS는 같은 호스트이며 Linux
격리를 주장하지 않습니다. `LinuxContainer`는 닫힌 실패입니다
(`UNAUTHORIZED`, `operation_id` 없음). 패치 적용을 게이트웨이가
구동하는 여러 RPC로 **쪼개면 안 됩니다**.

```text
Runner.apply_patch(request)
  expected versions → path policy → preflight → snapshot
  → Codex apply → after-version verify → rollback on failure
```

게이트웨이는 인가, `operation_key` 재실행, 쓰기 잠금, 디스패치,
영속을 유지합니다. 기본은 `InProcessRunner`입니다. 선택적
`CODESPACE_RUNNER=uds`는 비공개 Unix 소켓을 쓰며 compose 픽스처가
아닙니다.

Sandbox, PTY, 네트워크 격리는 **“기본적으로 Codex OS 공학을
재구현”이 아닙니다.** `crates/patch`와 `crates/codex-runtime` /
`codespace-codex-runtime`과 같은 패턴으로, Runner 뒤에 격리된 응집력
있는 실행 서브그래프를 선호합니다. 이 WP는 `codex-process-hardening`과
`codex-uds`를 가져옵니다. Codex 타입은 어댑터에 남습니다. App Server나
`codex-exec`를 넣지 마세요. `codex-exec-server`는 미래 측정이며 현재
백엔드가 아닙니다.

## 프로토콜 호환성

핵심 실행은 **stdio**와 **Streamable HTTP `/mcp`** 위의 **MCP
2025-11-25**입니다. 필수 원시 연산은 `initialize`, `tools/list`,
`tools/call`입니다. HTTP의 스펙 하한은 2025-03-26(Streamable HTTP가
존재)이며, CI는 2025-11-25를 핀합니다. **2026-07-28은 점진적 향상
전용입니다.**

핵심은 MRTR, Tasks, subscriptions, `Mcp-Name` 라우팅, 또는 애플리케이션
상태로서의 0728 무상태 수명주기를 요구하면 안 됩니다. 인증은 선택적
Bearer 미들웨어로 남고, 디스패치는 `/mcp` → rmcp 도구입니다.
`ProtocolVersion`과 `NegotiatedFeatures`는 `crates/server`에만 있습니다.

2024-11-05 HTTP+SSE는 목표가 아닙니다. 전체 매트릭스:
[protocol-compatibility.md](protocol-compatibility.md).

## MVP 도구

실행 도구:

| 도구 | 역할 |
| --- | --- |
| `workspace_info` | Selector metadata, profile, roots (not a credential) |
| `read` | File contents + version |
| `find` | Relative-path search |
| `apply_patch` | Codex V4A only |
| `exec_command` | Start a managed **host** process |
| `write_stdin` | Write to a managed process |
| `read_process` | Cursor-based output |
| `terminate_process` | Kill a server-issued handle |
| `operation_status` | Recover by `operation_id` **or** `operation_key` |

조정 도구(일반 `tools/call`, MCP 2025-11-25 1급):

| 도구 | 역할 |
| --- | --- |
| `work_open` | Mint a `work_id` for one logical job |
| `steer_status` | Counts only; no intent bodies |
| `steer_claim_next` | Atomically claim one queued item |
| `steer_complete` | Mark claimed intent done or blocked |
| `work_finish` | Close only if the queue is drained |

사용자는 MCP가 아니라 HTTP `/inbox`에서 초안을 편집하고 큐 항목을
재정렬합니다. 의도 본문은 지시이지 능력이 아닙니다.

내부 모델 호출 도구는 없습니다. `git_apply_patch`는 MVP 밖입니다.
오류 코드와 전송 대 실행 규칙: [error-codes.md](error-codes.md).
Linux 격리 픽스처: [runner-isolation.md](runner-isolation.md).
Codex 제품 대 프리미티브: [codex-reuse.md](codex-reuse.md).
실행 전용 기반: [execution-substrate.md](execution-substrate.md).

## ID

HTTP/JSON-RPC 요청 id, `operation_id`, `operation_key`, `process_id`,
`work_id`, `intent_id`, 러너 `request_id`(`rrpc-…`)는 서로 다른
식별자입니다. 잃어버린 HTTP 응답은 실행 실패가 아닙니다. 유실된 UDS
`apply_patch` 응답은 `rejected`가 아니라 `unknown`으로 기록됩니다.
클라이언트는 변경 도구를 재실행하는 대신 `operation_status`를
호출합니다.

```text
Workspace (workspace_id)
  └── Work (work_id)
        ├── Operation (operation_id)
        ├── Process (process_id)
        └── User Intent Queue (intent_id)
```

`workspace_id`와 `work_id`는 **선택자**이며 인가 증명이 아닙니다.
클라이언트 인자 `approved: true`와 `user_id`는 무시됩니다. 사용자 의도
텍스트는 권한 프로필을 올리지 않습니다.

## 패치 적용 파이프라인

게이트웨이는 인가, `operation_key` 재실행, 쓰기 잠금, 영속을
유지합니다. `InProcessRunner.apply_patch`는 실행 트랜잭션을 한 번의
호출로 돌립니다. 게이트웨이는 이를 프리플라이트 / 스냅샷 / 적용 RPC로
쪼개지 않습니다.

```text
validate request
  → auth + workspace policy
  → operation_key replay / conflict
  → workspace write lock
  → Runner.apply_patch
        expected_versions
        → full preflight (no writes)
        → save rollback snapshot
        → original engine apply (`apply_patch_with_options`)
        → verify disk hash == helper claimed after_version
        → rollback on failure
  → persist operation status (gateway fills operation_id)
  → MCP response
```

`crates/patch`(`codespace-patch` 안)는 `parse_patch`, 그다음 제품 정책,
그다음 `apply_patch_with_options`를 **프로세스 내부에서** 호출합니다.
독립 `apply_patch` 바이너리를 감싸지 않고 파서를 재구현하지 않습니다.

`apply_patch`는 `git apply`로 조용히 폴백하지 않습니다. 상태 값은
`applied` / `checked` / `rejected` / `failed_rolled_back` /
`failed_partial` / `unknown`입니다. `checked`는 성공한 `check_only`
미리보기(쓰기 없음)입니다. `rejected`는 실제 거절입니다. 전송이 모호하면
(디스패치 후 소켓 끊김) `unknown`으로 끝내며 `rejected`로 저장하면 안
됩니다. after-version 검증 없는 성공 문구는 금지입니다.

롤백은 `git reset --hard`를 쓰면 안 되고, 파일별 복원 대신 디렉터리
트리 전체를 덮어쓰면 안 됩니다.

## 저장소 배치

```text
Cargo.toml              workspace root
crates/server/          bin codespace-mcp: rmcp stdio + Streamable HTTP + /inbox
crates/domain/          workspace, capabilities, operation, errors (no rmcp)
crates/policy/          registry, PermissionProfile, Environment
crates/patch/           Codex adapter + codespace-patch helper (own workspace)
crates/codex-runtime/   isolated worker: hardening + UDS + InProcessRunner
crates/store/           SQLite operations, works, intents; in-memory resource locks
crates/runner/          Runner trait + execution DTOs, PathSandbox, patch transaction, host supervisor, UdsRunner, fixture checks
third_party/codex/      git submodule, pinned revision (W06)
tests/{security,recovery,e2e}/
docs/                   including operations.md (W12), codex-reuse.md,
                        execution-substrate.md (W17)
deploy/                 unprivileged Linux isolation fixture
```

## 초기 범위 밖

브라우저 확장, ChatGPT DOM 자동화, Goal/Loop, 멀티 에이전트, Desktop
제어, 플러그인 마켓플레이스, 완전한 OAuth 서버, 자동 unified-diff 변환,
Codex App Server RPC 전체 전달, 내부 모델 호출, TypeScript MCP SDK,
`native/patch-worker`.
