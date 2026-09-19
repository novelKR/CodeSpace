# Codex 재사용: 제품과 프리미티브

[English](../codex-reuse.md) | [한국어](codex-reuse.md)

CodeSpace는 Codex 에이전트를 넣지 **않습니다**. 모든 실행 메커니즘을
처음부터 재구현하지도 **않습니다**.

> CodeSpace는 Codex 실행 코드를 피하지 않습니다. Codex 에이전트/제품
> 소유권이 CodeSpace 핵심에 들어오는 것을 막습니다. 의미와 인가는 여기에
> 남고, 저수준 실행은 핀된 Codex 서브그래프에서 옵니다.

요점은 “코드를 덜 쓰는 것”이 아닙니다. 핀 범프가 PTY, 샌드박스,
hardening, 경로 버그 수정을 물려받아야 합니다. 그다음 CodeSpace는 MCP,
인가, 작업 신원, Runner 계약에 힘을 씁니다.

이 프로세스는 실행 전용입니다. 모델 없음, Responses API 없음
([execution-substrate.md](execution-substrate.md)). App Server
**프로토콜**은 MCP 번역 대상이 아닙니다. `command/exec` **형태**
(독립 argv, 핸들, 이후 TTY)는 Runner DTO에 올 수 있습니다.
`codex-exec`, `codex-core`, App Server는 금지된 채로 남습니다.

```text
WHO MAY  → CodeSpace Gateway
           MCP contract, workspace, profile meaning,
           operation/idempotency, audit, Runner trait

HOW SAFE → pinned Codex execution subgraph
           patch engine, PTY, spawn/reap, Landlock/seccomp,
           process hardening, filesystem mechanics,
           network enforcement
```

서브그래프를 재사용해도 Codex가 인가자가 되지는 **않습니다**.
게이트웨이가 여전히 허용하고, 어댑터가 여전히 실행합니다.

```text
ChatGPT / Cursor / other MCP host
   │ MCP
   ▼
CodeSpace Core          ← only authorization authority
   ├─ MCP / workspace / profile meaning
   ├─ operation_key / operation_id / persist
   └─ Runner contract (CodeSpace DTOs; no Codex types)
          │
          ▼
   isolated adapter workspace
          │  crates/patch (codespace-patch)
          │  crates/codex-runtime (codespace-codex-runtime)
          │    process-hardening + UDS worker; opt-in
          │  crates/pty (codespace-pty)
          │    interactive spawn; runner API에 Codex 타입 없음
          │  crates/file-system (codespace-fs)
          │    no-follow I/O + 제한된 walk; PathSandbox가 인가, I/O 안전은 어댑터
          │  crates/linux-sandbox (codespace-linux-sandbox)
          │    사용자 argv 헬퍼 wrap; Restricted 네트워크 hard deny; Runner에 Codex 타입 없음
          ▼
   Codex execution subgraph (pinned) → OS
```

Codex는 어댑터 뒤의 **구현** 의존성이며 Gateway의 아키텍처 의존성이
아닙니다. `InProcessRunner`, `UdsRunner`, 이후 원격 러너, 또는
다른 샌드박스 백엔드는 Codex 타입이 어댑터를 떠나지 않으면 바뀔 수
있습니다.

## 재사용 단위는 서브그래프

“`codex-apply-patch`만큼 좁아야 한다”를 요구하지 마세요. 물으세요.

1. 이 서브그래프가 **응집력 있는 실행**인가?
2. **에이전트 / 모델 / 제품** 타입이 Runner 경계를 넘는가?
3. **Gateway 허용을 우회**할 수 있는가?

넓은 Cargo 그래프 자체는 거절이 아닙니다. PTY fd 처리, 시그널 레이스,
Landlock, 마운트 탈출, 또는 프로세스 hardening을 재구현하면 Codex 버그
수정이 손으로만 옵니다. 선호:

```text
Codex bugfix → candidate pin → adapter compile / security / parity → promotion
```

그것은 **“`main`을 추적”이 아닙니다.** 릴리스 수락은
[upstream-update.md](upstream-update.md)에 남습니다. 핀 범프에서
hardening / PTY / sandbox / filesystem / network 크레이트의 diff는
실행/보안 변경 로그이지 조용한 의존성 범프가 아닙니다.

컨테이너 격리와 호스트 샌드박스는 **대체물이 아닙니다**. 컨테이너에
no-new-privs / seccomp / Landlock / 네트워크 한도를 더하는 것은
심층 방어입니다. 이후 Environment(로컬 컨테이너, 원격 Linux, 베어
Linux)는 같은 Linux 샌드박스 서브그래프를 공유할 수 있습니다.

## 핵심 대 어댑터

```text
CodeSpace core
  crates/domain, policy, store, server, runner
  ──────────────────────────────────────────
  NO Codex types (including codex-protocol)
  NO Codex crate path dependency


isolated adapter (crates/patch today;
crates/codex-runtime today; crates/pty today;
crates/file-system today; crates/linux-sandbox today)
  ──────────────────────────────────────────
  approved execution subgraph allowed
  including transitive codex-protocol


adapter boundary
  ──────────────────────────────────────────
  CodeSpace DTO  ↔  Codex DTO
```

공개 MCP는 `workspace_id` + 상대 경로로 남습니다. 내부에서 어댑터는
해석할 수 있습니다.

```text
MCP virtual path
      ↓
CodeSpace WorkspaceResolver / path scope
      ↓
Codex AbsolutePath / PathUri  (adapter only)
      ↓
Runner helper
```

## 정책 대 메커니즘

| CodeSpace가 소유 (정책) | Codex를 선호 (메커니즘) |
| --- | --- |
| workspace / profile allow | PTY, UDS transport primitive |
| path permission meaning | filesystem walk / symlink mechanics |
| network permission meaning | seccomp / Landlock / hardening |
| operation approval | network enforcement (when needed) |
| Runner RPC contract | shell parse / argv construction |

`codex-execpolicy`는 어댑터에서 파싱하거나 분류할 수 있습니다. 최종
`allow(command)`가 **아닙니다**.

## `apply_patch` 패턴 (격리이지 크레이트 너비가 아님)

엔진을 재사용하세요. 그 주변 서비스는 소유하세요. 이후 런타임 어댑터도
같은 패턴입니다.

- 핀은 [upstream-lock.md](upstream-lock.md)에 남습니다
  (`6b9826e3aa83b1a5947db50f4332cb9c65f1b340`).
- **격리된** Cargo 워크스페이스에서의 경로 의존성이며 저장소 루트가
  아닙니다. 오늘: `crates/patch`, `crates/codex-runtime`
  (`codespace-codex-runtime`), `crates/pty` (`codespace-pty`),
  `crates/file-system` (`codespace-fs`), `crates/linux-sandbox`
  (`codespace-linux-sandbox`).
- NOTICE + Apache-2.0 귀속.
- 제품 정책은 서브그래프 앞과 뒤에 남습니다.
- Codex 워크스페이스에서 크레이트를 파일 복사하지 마세요.

```text
Gateway → Runner trait → UdsRunner (opt-in)
       → codespace-codex-runtime helper → Codex execution crates
```

루트 워크스페이스에 Codex 경로 의존성이 생기면 안 됩니다.

정책 스캔(`scripts/check-no-model-deps.sh`)은 Codex 서브모듈 **없이**
돌아가는 싼 병렬 CI job입니다. 비용은 이 grep이 아니라 서브모듈
checkout과 cargo가 지배합니다.

| 구역 | 경로 | 비용 | 때 |
| --- | --- | --- | --- |
| core manifests | root + `crates/{domain,policy,runner,store,server}/Cargo.toml` | tiny | crate in the update range |
| core sources | those crates’ trees | low | same |
| server tests | `tests/` | low | `crates/server` or `tests/` changed |
| adapter manifests | `crates/patch/Cargo.toml`; `crates/codex-runtime`; `crates/pty`; `crates/file-system`; `crates/linux-sandbox` | tiny | adapter in the update range; allowlist only |
| upstream | `third_party/codex` | huge / false positives | never |

갱신 범위는 `SCAN_BASE`(PR base / 이전 `main`)입니다. 범위를 모르면
**모든** 핵심 크레이트와 어댑터 매니페스트를 스캔합니다(diff 실패로
건너뛰지 않음). 문서만 바뀌면 이 job은 exit 0으로 건너뛰고, rust job은
여전히 실행됩니다.

핵심 매니페스트는 어떤 `codex-` 의존성 키도 금지합니다. 어댑터
매니페스트는 승인된 서브그래프만 허용합니다(오늘 `crates/patch`:
`codex-apply-patch`, apply-patch 워크스페이스 그래프로서
`codex-exec-server`, `codex-utils-path-uri`, `codex-process-hardening`;
`crates/codex-runtime`: `codex-process-hardening`, `codex-uds`;
`crates/pty`: `codex-utils-pty`; `crates/file-system`:
`codex-file-system`, `codex-exec-server`, `codex-utils-path-uri`;
`crates/linux-sandbox`: `codex-linux-sandbox`, `codex-sandboxing`,
`codex-protocol`, `codex-utils-path-uri`). 소스는 에이전트/모델
패턴을 유지합니다(`api.openai.com`, Responses, `codex-login`,
`codex-core`, `codex-app-server`, `async-openai`). 크레이트 이름을
언급하는 주석은 cargo 의존성이 아닙니다.

linux-sandbox 어댑터는 Rama **0.3.0-alpha.4** leaf
크레이트(`rama-error`, `rama-macros`, `rama-utils`)를 resolver
가드로도 고정합니다. Codex 핀 `6b9826e`는 그 train으로 검증되어
있습니다. 새로 resolve하면 `rama-core`는 alpha.4인데 leaf만
stable `0.3.0`이 될 수 있습니다. 가드는 격리 helper lock과 root lock
(path 의존) 모두에 적용됩니다. CI `cargo clippy` / `cargo test`는
`--locked`입니다.

독립 `apply_patch` 바이너리를 보안 경계로 감싸지 **마세요**. Codex App
Server를 내부 백엔드로 감싸지 **마세요**.

## 감독 코드가 아직 있는 이유

`process_id`, stdin, terminate, timeout이 모이는 이유는 요청 수명이
프로세스 수명이 아니기 때문입니다. 프로세스 내부 감독이 **기본**입니다.
선택적 `UdsRunner`도 그 감독을 `codespace-codex-runtime` 안에서
돌립니다. `operation_key` / `operation_status`는 잃어버린 **원격 MCP
변경 RPC**를 복구하며, Codex 스레드를 복구하지 않습니다.

spawn+PTY를 얻으려고 `codex-core` / `codex-exec` / App Server를 끌어오면
login, models, plugins, rollout도 따라옵니다. 그 폭발 반경은 여전히
거절입니다.

## 단계적 가져오기 (그 WP가 생길 때)

문서화된 순서입니다. **이 WP에서 코드로 가져옴:** process-hardening, UDS,
PTY(`crates/pty` → `codex-utils-pty`), filesystem(`crates/file-system`
→ `LOCAL_FS` / `ExecutorFileSystem`), linux-sandbox
(`crates/linux-sandbox` → 헬퍼 wrap, Restricted hard deny). **아직 안
가져옴:** network(`Enabled` + proxy).

```text
process-hardening → PTY → UDS / path → filesystem → linux-sandbox → network
```

복잡도와 잠금은 그 순서로 커집니다. 핀은 가져가는 모든 서브시스템을
한 번에 정의합니다.

## 핀 `6b9826e`의 후보

Codex `main`이 아니라 핀의 `Cargo.toml` 파일로 판단합니다.

### 지금 재사용 (코드에서)

**`codex-apply-patch`** via `crates/patch`. 파싱, 헝크 검증, 적용,
패리티 부분집합.

**`codex-process-hardening`** via `codespace-patch`와
`codespace-codex-runtime` `pre_main_hardening()`. 워커/헬퍼 **프로세스**
강화이지 command sandbox가 아닙니다. `main` 첫 줄로 유지하고, 의존성
폭이 커지지 않는 한 `ctor`는 넣지 않습니다.

**`codex-uds`** via `codespace-codex-runtime` bind. RPC는 CodeSpace.

**`codex-utils-pty`** via `crates/pty` (`codespace-pty`).
Unix: `portable-pty`, `tokio`, `libc`. 기본 크기 24x80. 연결해도 PTY
MCP 도구가 추가되지는 **않습니다**. `exec_command`에 선택적 `tty`(기본
false). 게이트웨이가 여전히 `process_id`를 발급합니다. Resize는
Runner/MCP 표면에 두지 않습니다(P1).

**`codex-file-system`** via `crates/file-system` (`codespace-fs`).
제한된 walk, `LOCAL_FS`를 통한 no-follow I/O(`sandbox: None`).
공개 타입은 CodeSpace(`Path` / bytes / walk 결과 / `FsError`)만.
`PathSandbox`는 **인가자**(논리 워크스페이스 선택)로 남습니다. I/O
안전 경계가 아닙니다. 살아 있는 프로세스가 사전 검사와 경쟁할 수
있습니다. `codespace-fs`가 레이스에 강한 no-follow
open/read/write/remove/walk와 typed error(`SymlinkRejected`,
`NotRegularFile`)를 소유합니다. MCP `read` / `find`는 워크스페이스
상대로 남습니다.

**`codex-linux-sandbox`** via `crates/linux-sandbox`
(`codespace-linux-sandbox`).
([`codex-rs/linux-sandbox/Cargo.toml`](../../third_party/codex/codex-rs/linux-sandbox/Cargo.toml))

사용자 argv를 `spawn_pipe` / `spawn_pty`에서 헬퍼로 감쌉니다.
Restricted 네트워크는 `--unshare-net`과 Restricted seccomp입니다. 직접
프록시 플래그(`--allow-network-for-proxy`, `--proxy-route-spec`)는
쓰지 않습니다. 그건 다음 WP입니다. 런타임 의존성에는 `codex-core`가
없습니다. **dev-dependencies에는 있습니다** — 어댑터 시험이 그
그래프를 제품 바이너리로 끌어오면 안 됩니다. 공개 타입은
CodeSpace(`SandboxExecSpec` / `SandboxLaunch`)만.
`codex_protocol::PermissionProfile`은 이 크레이트 안에 남습니다.

**전이(어댑터에서 허용):** `codex-sandboxing`, `codex-network-proxy`,
`codex-protocol`. 프록시의 직접 사용은 PermissionProfile **네트워크**
축이 있을 때입니다. 허용 엔진이 아닙니다. 루트 워크스페이스 의존성이
아닙니다.

### 재사용 선호 (그 WP가 올 때)

**`codex-uds`** (이미 `codespace-codex-runtime`에 있음)
([`codex-rs/uds/Cargo.toml`](../../third_party/codex/codex-rs/uds/Cargo.toml))

Unix: Tokio `fs` / `net` / `rt`. 선택적 Runner Unix 소켓 워커의 소켓
프리미티브입니다. **RPC 프로토콜은 CodeSpace 소유로 남습니다.**

**`codex-utils-absolute-path` / `codex-utils-path-uri`**

작은 경로/URI 층(`dirs`, `dunce`, URL). 패치가 이미 필요합니다. MCP는
여전히 워크스페이스 상대 경로만 노출합니다.

**`codex-file-search`**
([`codex-rs/file-search/Cargo.toml`](../../third_party/codex/codex-rs/file-search/Cargo.toml))

`ignore`, `nucleo`, Tokio. `codex-core` 없음. MCP는
`find(query, workspace_id)`로 남고, 엔진은 어댑터 뒤로 옮길 수 있습니다.

### 조건부 / 적극 평가

**`codex-shell-command`**

Tree-sitter Bash/PowerShell, shlex, `which`. 파싱 / 인용 / 실행 파일
해석만. 허용 엔진이 아닙니다.

### 내부 프로토콜 후보

**`codex-exec-server-protocol`**
([`codex-rs/exec-server-protocol/Cargo.toml`](../../third_party/codex/codex-rs/exec-server-protocol/Cargo.toml))

file-system, network-proxy, protocol, shell-command, path-uri. 이후
워커 DTO / 어댑터 기반. MCP나 `crates/domain` 타입이 **아닙니다**.

### 격리 층 전이: `codex-protocol`

무거움: execpolicy, http-client, network-proxy, extension items,
Linux에서 Landlock/seccompiler. **핵심에서 금지.** 어댑터에서는
허용합니다. 금지하면 file-system, sandbox, shell-command를 다시 짜야
합니다. `codex_protocol::PermissionProfile`은 MCP나 `crates/domain`에
나타나면 안 됩니다.

### 실험적 백엔드 (지금은 아님)

**`codex-exec-server`**
([`codex-rs/exec-server/Cargo.toml`](../../third_party/codex/codex-rs/exec-server/Cargo.toml))

HTTP/WS plus `codex-api`, `codex-config`, OTel, protocol, sandboxing,
PTY. 오늘의 Runner 백엔드로는 너무 무겁습니다. 영구 거절은 아닙니다.
나중에 UdsRunner + 저수준 크레이트 대 Gateway 어댑터 →
exec-server를 비교하세요. 컴파일 그래프와 업그레이드 비용을 재세요.

### 이후 Environment (P0 아님)

**`codex-git-utils` / `codex-worktree`** — Environment 프로비저닝이
필요하면 격리된 checkout / worktree 수명주기. file-system, protocol,
PTY, `gix`를 끌어옵니다. 그 WP까지 빼 두세요.

### 코드는 허용, 권한은 금지

**`codex-execpolicy`** — Starlark 접두 규칙. 어댑터의 분류/파싱은
괜찮습니다. 최종 허용은 Gateway에 남습니다.

### 거절

**`codex-exec`** — App Server 클라이언트, `codex-core`, login, config,
rollout, history. 제품 exec 흐름이지 `spawn`이 아닙니다.

**`codex-core`** — 에이전트 루프, 도구, 세션.

**App Server embed** (`ChatGPT → MCP adapter → Codex App Server`) —
에이전트 인프라를 다시 가져오고 인가를 나눕니다.

**login / model / Responses** — 실행 전용 위반.

**Codex 세션 `permissionProfile` / 사용자 샌드박스 설정을 허용으로** —
두 번째 인가자.

## CodeSpace에 남는 것

- MCP 도구 스키마와 도메인 타입(runner/domain에 `rmcp` 없음. 핵심에
  Codex 타입 없음).
- 워크스페이스 레지스트리, 프로필의 **의미**, 경로 정책.
- 쓰기 잠금, 셸 점유, `WORKSPACE_BUSY`(스케줄러 WP까지).
- `operation_key` 재실행, `operation_id`, `operation_status`.
- 호스트/프로세스 내부 프로세스 감독이 기본. UDS 워커는 선택적.
- 컨테이너 수명주기와 워크스페이스 바인드 마운트 **정책**.
- 격리된 어댑터 워크스페이스(`crates/patch`,
  `crates/codex-runtime` / `codespace-codex-runtime`, `crates/pty` /
  `codespace-pty`, `crates/file-system` / `codespace-fs`,
  `crates/linux-sandbox` / `codespace-linux-sandbox`).

## 다음 구현 WP

다음 **코드** 작업 패키지는 기존 `Runner` 트레이트 뒤의 남은 실행
서브그래프(network: `Enabled` + proxy)입니다.
`apply_patch`를 게이트웨이가 구동하는 여러 RPC로 쪼개면 안 됩니다.

기본으로 자체 PTY / Landlock / seccomp 스택을 두지 마세요.
위 표 이후 격리된 워크스페이스를 통해 실행 서브그래프를 가져오세요.
핀 범프는 의도적 릴리스입니다
([upstream-update.md](upstream-update.md)): 지금은 SHA + 패치 패리티.
그 크레이트를 가져가면 런타임 어댑터 빌드와 PTY / sandbox / process
회귀를 더합니다.

남은 도메인 확장(스케줄러 큐, 승인 도구)은
[execution-substrate.md](execution-substrate.md)에서 순서를 정합니다.
실제 MCP 도구 이름은 그대로입니다. `exec_command`에 선택적 `tty`가
생겼습니다.
