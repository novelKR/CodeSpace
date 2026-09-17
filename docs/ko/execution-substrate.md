# 실행 기반

[English](../execution-substrate.md) | [한국어](execution-substrate.md)

CodeSpace는 **실행 전용 MCP**입니다. ChatGPT(또는 다른 MCP 호스트)가
계획하고 코드를 작성합니다. 이 프로세스는 모델을 호출하지 않고, OpenAI
Responses API를 호출하지 않으며, 에이전트 루프를 실행하지 않습니다.

```text
ChatGPT
  판단 / 계획 / 코드 생성
        │ MCP tools/call
        ▼
Headless execution MCP
  no model, no Responses API, no agent loop
        │
  patch / exec / fs / sandbox / permissions / audit
        ▼
Filesystem / OS / container / (later) remote runner
```

Codex App Server 아이디어는 다음에 **예**라고 답할 때만 가져옵니다.

> 이것이 실행, 권한, 상태, 또는 관찰을 **모델 없이 결정적으로**
> 제공할 수 있는가?

프롬프트, 컨텍스트, 턴, 추론, 검토, 또는 모델 카탈로그가 필요하면
빼 두세요. App Server JSON-RPC를 MCP로 번역하지 마세요. 기반을 추출해
CodeSpace 도구로 다시 노출하세요.

인가(**누가 해도 되는지**)는 Gateway에 남습니다. 안전한 실행(**얼마나
안전한지**)은 Runner 뒤의 핀된 Codex **실행 서브그래프**가 공급합니다
(구현 의존성이지 아키텍처 의존성이 아님). 핵심 크레이트는
`codex-protocol`을 포함해 Codex 타입을 가져오면 안 됩니다. 어댑터는
가져도 됩니다. 가져오기/빼기 표:
[codex-reuse.md](codex-reuse.md). 핀:
[upstream-lock.md](upstream-lock.md) (`6b9826e`, `rust-v0.154.0`).
Codex `main` `4701aa4b`를 언급하는 조사 노트는 핀 범프가 **아닙니다**.
의도적인 W13 갱신 뒤에 그래프를 다시 확인하세요.

CI: `policy-scan` job은 서브모듈 **없이** `scripts/check-no-model-deps.sh`를
`rust`와 병렬로 실행합니다. 핵심 매니페스트는 `codex-*` 의존성을 선언하면
안 됩니다. 핵심 소스는 에이전트/모델 grep을 유지합니다. 격리된 어댑터
매니페스트(`crates/patch`, `crates/codex-runtime`, `crates/pty`)는
**허용 목록**을 사용합니다. `third_party/codex` 소스는 절대 스캔하지
않습니다. `SCAN_BASE`는 트리를 갱신 범위로 제한합니다. 범위를 모르면
모든 핵심 크레이트와 어댑터 매니페스트를 스캔합니다. Clippy/시험은
여전히 항상 실행됩니다. rust job은 fmt/clippy **전에** 핀 SHA를
검사합니다(`PIN_ONLY=1`).

## 불변식

| 해야 함 | 하면 안 됨 |
| --- | --- |
| MCP tools for read, patch, exec, process, operations | Responses / Chat Completions client |
| Gateway as the only allow path | Codex session `permissionProfile` as allow |
| Workspace-relative MCP paths | Absolute paths on the wire |
| Process handles that outlive an MCP connection | Copy App Server “kill on connection close” |
| Isolated `crates/patch` → `codex-apply-patch` | Embed `codex-app-server` / `codex-exec` / `codex-core` |

`read-only` / `workspace-write`가 실제 MCP 프로필로 남습니다. 더 풍부한
파일시스템 glob + 네트워크 축은 `crates/policy`의 `PermissionProfile`에
있고 그 프로필에서 매핑됩니다. `process_exec`가 Exec 축입니다
(`read-only`는 거부, `workspace-write`는 허용). 경로 glob은 **표현만**
있고 live enforcement는 기존 coarse `allow(Write|Exec)` + PathSandbox입니다.
네트워크 축은 기록만 하며 허용을 올리지 않습니다. Codex 사용자 설정을
가져오는 것이 아닙니다.

## 네 축 (목표 도메인)

이 모두가 오늘 MCP 필드는 아닙니다. **실제 도구에 `environment_id`를
추가하지 마세요.** 운영자 JSON은 환경을 등록할 수 있습니다. 생략하면
암시적 로컬 호스트입니다. 알 수 없는 environment id는 설정 로드에
실패합니다. `linux-container`는 로드되지만 exec/patch는 닫힌 실패입니다.

```text
Environment   where command and filesystem ops run
Workspace     which tree inside that environment is in scope
PermissionProfile  what that pair may do (gateway-owned)
Operation     this mutating RPC (id, key, persist, audit)
      ↓
Process / Patch / FS
```

- **Environment**는 에이전트가 아닙니다. 로컬 호스트, Linux 컨테이너,
  또는 이후 원격 러너가 환경입니다. 등록은 제어 평면 / 운영자 동작입니다.
  모델이 `execServerUrl`을 공급하면 안 됩니다.
- **Workspace**는 MCP의 선택자로 남습니다(`workspace_id` + 상대 경로).
  내부에서 러너는 절대 경로로 해석할 수 있습니다.
- **PermissionProfile** 형태(경로, glob, 또는 특수 루트에 대한 Read /
  Write / Deny, `process_exec`, 별도 네트워크 축)는 App Server를 따를
  수 있습니다. glob은 표현만 있고 live enforce 하지 않습니다.
  **부여하는 엔진**은 CodeSpace 정책입니다.
- **Operation**은 이미 `operation_id` / `operation_key` /
  `operation_status`입니다. Diff/감사 원장은 P1이며 대화 이력이
  아닙니다.

```text
MCP virtual path
      ↓
WorkspaceResolver / CodeSpace path scope
      ↓
absolute path (internal; later Codex AbsolutePath / PathUri in the adapter)
      ↓
Runner / patch helper
```

## `command/exec`: 형태 대 크레이트

핀의 App Server `command/exec`
([`command_exec.rs`](../../third_party/codex/codex-rs/app-server-protocol/src/protocol/v2/command_exec.rs))
는 **독립** argv API입니다. thread 없음, turn 없음. 필드에는 argv,
선택적 프로세스 id, tty, stdin/stdout 스트리밍, 출력 한도, 타임아웃,
cwd, env, PTY 크기, `sandboxPolicy` / `permissionProfile`이 있습니다.
후속: write, resize, terminate. 스트리밍은 `outputDelta`입니다.

그 **형태**가 오늘 Runner DTO에 있습니다. PTY spawn은 같은
`process_id` 뒤에 연결됩니다(`exec_command.tty`, 기본 false, 어댑터
크기 24x80). `process_resize` / `tty_size`는 **P1**로 남습니다.
게이트웨이가 `cwd: WorkspaceRoot`, 러너 로컬 env 기본값(`PATH` /
`HOME` / `LANG`은 러너 프로세스에서 적용, PTY일 때만 `TERM=xterm`,
게이트웨이 `PATH`나 호스트 절대 cwd를 직렬화하지 않음), 타임아웃,
출력 한도, 정책 요약을 채웁니다. 실제 MCP는 그대로입니다.

```text
exec_command / write_stdin / read_process / terminate_process
```

모델은 어댑터 토폴로지가 아니라 MCP에서 이것을 배웁니다.
`initialize.instructions`는 전역 불변식입니다. 워크스페이스를 고르면
`workspace_info.execution`이 실제 capability입니다. `exec_command` 결과는
`dispatch_status`를 실습니다. architecture 매뉴얼, Codex crate 그래프,
UDS 와이어를 클라이언트 계약에 넣지 마세요.

`output_combined=true`는 `read_process`가 하나의 combined output stream만
노출한다는 뜻입니다. stdout/stderr origin은 보존하지 않습니다. pipe
프로세스는 stdout과 stderr를 독립적으로 pump하므로 둘 사이의 상대
순서는 보장하지 않습니다. PTY 출력은 terminal master stream입니다.

`codex-exec`(제품 exec 흐름)를 가져오거나 App Server를 넣지 **마세요**.
`codex-exec-server-protocol`은 내부 워커 DTO 후보입니다.
`codex-exec-server`는 실험적 백엔드(`codex-api` / `codex-config`)이며
영구 거절은 아닙니다
([codex-reuse.md](codex-reuse.md)). 샌드박스 정책을 “Codex 사용자
설정”에서 기본값으로 두지 **마세요**. 게이트웨이는 이미 허용된 요청을
러너 DTO로 매핑합니다. `codex-process-hardening`, `codex-utils-pty`,
`codex-uds`(전송 프리미티브, RPC는 CodeSpace)를 선호하세요. PathSandbox
범위 아래 `codex-file-system`을 적극 평가한 뒤, 네트워크 축이 생기면
`codex-linux-sandbox`(dev-dep에 `codex-core` 포함, 제품 그래프에서는
빼 둘 것)와 `codex-network-proxy`를 보세요. 컨테이너가 그 서브그래프를
대체하지 않습니다.

App Server 스트리밍 프로세스는 연결 범위이며 그 연결이 닫히면 죽습니다.
CodeSpace는 **MCP 요청 수명 ≠ 프로세스 수명**을 유지합니다. `process_id`는
서버가 발급하고 애플리케이션 상태로 저장합니다. MCP 요청이 끝나도 살아
있는 프로세스를 죽이지 않습니다. 선택적 UDS 경로는 다릅니다. 게이트웨이 ↔
워커는 1:1입니다. UDS 연결 끊김이나 게이트웨이 종료는 워커를 죽입니다
(호스트 자식도 죽습니다). `process_id`는 워커 죽음 이후 살아남지 않습니다.
러너 `Replay`는 같은 연결 프리미티브이며 연결 끊김 복구가 아닙니다.

## 승인과 MCP 리비전

권한 부족은 오늘 **정책 거절**입니다(`ErrorBody`). 도구 인자
`{ "network": true }`의 조용한 부여가 아닙니다.

핵심 프로토콜은 **MCP 2025-11-25** `tools/call`로 남습니다
([protocol-compatibility.md](protocol-compatibility.md)). MRTR과 Tasks는
2026-07-28 점진적 향상입니다. exec나 patch에 필수가 되면 안 됩니다.

추가 권한을 나중에 설계할 때:

1. 명시적 도구(`approval_create` / `approval_resolve` /
   `operation_resume`)를 선호해 2025-11-25 클라이언트가 동작하게 하세요.
2. 같은 상태를 0728 호스트용 MRTR `input_required`에 선택적으로 매핑하세요.

사람 / 구성된 정책이 모델 요청과 OS exec 사이에 있습니다. 부여를
결정하려고 모델을 호출하지 않습니다.

장시간 **비대화형** 작업은 나중에 MCP Tasks를 쓸 수 있습니다.
**대화형** 작업은 `process_id`를 유지합니다. Tasks가 프로세스 핸들을
대체하면 안 됩니다.

## 스케줄러 (단일 쓰기 잠금 이후)

오늘은 워크스페이스 쓰기 잠금 하나와 셸 점유로 충분합니다. App Server는
자원별로 직렬화합니다(배타 대 공유 읽기). 목표 범위는 Environment,
Workspace, Path, Process, Operation, Watch이며 Thread가 아닙니다.
`crates/store`는 이제 그 키를 위한 메모리 자원 직렬화기를 씁니다.
SQLite 스키마는 그대로입니다. MVP는 `apply_patch`에 요청 소유 배타,
라이브 셸에 프로세스 소유 배타를 씁니다(`WORKSPACE_BUSY`).
`ProcessExited`(또는 프로세스 내부 종료)가 `release_process`를 호출합니다.
확인된 UDS 워커 죽음은 프로세스 소유 임대를 **모두** 풉니다. 응답
유실/모호함만으로는 풀지 않습니다.
`read` / `find`는 잠금이 없습니다. Shared-read는 타입만 유지합니다.

## `fs/watch`와 검색

`fs/watch`를 모델 도구로 노출하지 마세요. 외부 편집기 범프가 버전을
무효화하고 `apply_patch`가 `expected_versions` / 이후 `STALE_READ`로
실패할 수 있도록 내부에서 사용하세요.

퍼지 검색 **세션**은 TUI 입력 UX입니다. MCP는 `find` / 이후
`find_files(query, workspace_id, limit)`로 유지하세요. 그 계약 뒤의
엔진으로 `codex-file-search`를 선호하고 세션 프로토콜은 버리세요.

## 훅과 스킬

훅은 로컬이고 결정적이며 모델을 호출할 수 없을 때만 허용됩니다
(`before_patch` 정책, `after_patch` fmt, 감사). Responses API로 코드를
검토하는 훅은 금지입니다.

스킬은 숨은 에이전트에 자동 주입되지 않습니다. 추가한다면 **호스트**가
읽기로 고르는 MCP 리소스나 프롬프트입니다.

다운스트림 MCP 연합(이 서버가 MCP 클라이언트)은 P3입니다. 모델은 없지만
인증과 도구 이름 충돌이 비쌉니다.

## 가져오기 / 빼기 (개념)

| 가져오기 (기반) | 빼기 (에이전트 런타임) |
| --- | --- |
| V4A parse/verify/apply | `thread/*`, `turn/*`, steer-as-turn |
| Standalone command/exec **shape** | `codex-exec` crate, App Server embed |
| PTY / UDS / linux-sandbox / hardening subgraph | Homegrown Landlock/seccomp/PTY/UDS by default |
| Filesystem mechanics under PathSandbox scope | Replacing PathSandbox wholesale; `codex-protocol` in core |
| Process manager / PTY helper | Connection-scoped process death |
| Sandbox **policy object** (gateway fills) | User Codex config as default allow |
| Permission profile **shape** in `crates/policy` | `permissionProfile` from the model or Codex session |
| Environment as exec location | Agent / account / model provider |
| Resource serialization | Thread-keyed queues |
| Internal fs/watch | Watch as an MCP tool |
| Search engine, not session RPC | Absolute-path `fs/writeFile` on the wire |
| Deterministic hooks | Hook → model |
| Operation / diff / audit | Conversation compaction, memory, review, Guardian, multi-agent, Goal |

개념 대응(타입을 가져오지 말 것): Thread → workspace/operation 이력,
Turn → Operation, Interrupt → cancel, Turn diff → `operation_diff`,
Approval → policy + human, Attachment → artifact 리소스.

## 로드맵 (구현은 나중)

이 기반의 P0 코드는 들어와 있습니다. Runner exec DTO **형태**
(`RunnerCwd::WorkspaceRoot`, 러너 로컬 env 기본값),
`crates/policy`의 `PermissionProfile`(`process_exec`)과 Environment,
자원 직렬화기(요청 vs 프로세스 소유), 선택적 `UdsRunner` +
`codespace-codex-runtime`(process-hardening + UDS), 격리된
`crates/pty` → `codex-utils-pty`. 실제 MCP 도구 **이름**은 그대로입니다.
`exec_command`에 선택적 `tty`(기본 false)가 있습니다.

**P0** — 착수했거나 다음 서브그래프 WP: `codex-apply-patch`(완료),
Runner DTO의 exec 런타임 **형태**(완료), `crates/policy`의
PermissionProfile 도메인(완료), Environment 도메인(운영자 등록, 도구
인자 아님)(완료), 자원 직렬화기(완료), process-hardening + UDS를 받는
전송(`UdsRunner`)(완료, 선택적), PTY I/O 백엔드(완료). 아직 밖:
filesystem → linux-sandbox → network.

**P1** — 작업 상태 기계 / diff 원장, 승인 폴백 도구, 내부 watch, 더
풍부한 프로세스 핸들(resize, caps), 연결 끊김 정책.

**P2** — 기존 MCP 계약 뒤 `codex-file-search`로 `find` 품질, 결정적
훅, 리소스 또는 프롬프트로서의 스킬.

**P3** — 원격 환경, MCP 연합, 아티팩트 레지스트리.

다음 **코드** WP는 기존 트레이트 뒤의 남은 실행 서브그래프이며,
`apply_patch`를 게이트웨이 RPC로 쪼개지 않습니다. filesystem부터
시작합니다. Sandbox / network는 기본 자체 OS 스택이 아닙니다
([codex-reuse.md](codex-reuse.md)).
