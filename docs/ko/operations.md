# 운영

[English](../operations.md) | [한국어](operations.md)

깨끗한 클론에서 로컬 CodeSpace를 재현합니다. 설치하고 게이트웨이를
시작한 뒤 `workspace_info` → `read` → `apply_patch` → `exec_command`
순서로 진행합니다. 이것은 **개인 단일 사용자** 배치입니다. 멀티테넌트
SaaS가 아니고 완전한 OAuth 서버도 아닙니다.

공개 호스트 이름에 대한 ChatGPT Custom Connector는 **검증하지 않았습니다**.
[chatgpt-connector.md](chatgpt-connector.md)를 보세요.

## 설치

Rust 1.88+, Git, 그리고 (Linux 격리 픽스처용) Docker가 필요합니다.

```bash
git clone --recurse-submodules https://github.com/novelKR/CodeSpace.git
cd CodeSpace
# If you already cloned without submodules:
# git submodule update --init --recursive
```

Codex 핀은 [upstream-lock.md](upstream-lock.md)의 커밋에 있는
`third_party/codex`입니다. Codex `main`으로 `git submodule update --remote`를
하지 마세요. 제품 런타임은 게이트웨이 밖에 둡니다.
[codex-reuse.md](codex-reuse.md)와
[execution-substrate.md](execution-substrate.md)를 보세요.

게이트웨이가 패치 헬퍼를 자기 옆에서 찾을 수 있도록 게이트웨이와 패치
헬퍼를 **같은** 디렉터리에 빌드하세요(`CODESPACE_PATCH_BIN`을 설정해도
됩니다). UDS 워커는 선택입니다(`CODESPACE_RUNTIME_BIN`).

```bash
cargo build -p codespace-server --bin codespace-mcp --release
cargo build --manifest-path crates/patch/Cargo.toml --bin codespace-patch --release
mkdir -p dist
cp target/release/codespace-mcp dist/
cp crates/patch/target/release/codespace-patch dist/
# Optional Unix-socket worker (not the default exec path):
cargo build --manifest-path crates/codex-runtime/Cargo.toml --bin codespace-codex-runtime --release
cp crates/codex-runtime/target/release/codespace-codex-runtime dist/
```

`codespace-patch`는 호스트 자식 프로세스입니다. 핀된 Codex 크레이트를
**프로세스 내부에서** 호스팅합니다. 업스트림 독립 `apply_patch` 바이너리가
아니고 폐기된 `native/patch-worker`도 아닙니다. `codespace-codex-runtime`은
`codex-process-hardening`과 `codex-uds`로 비공개 Unix 소켓을 바인드한 뒤
`InProcessRunner`를 실행합니다. hardening은 워커/헬퍼 **프로세스**
강화입니다(`main` 첫 줄 `pre_main_hardening()`, `ctor` 없음). command
sandbox가 아닙니다. 기본 `exec_command`는 여전히 프로세스 내부 호스트
spawn입니다. Exec DTO cwd는 `WorkspaceRoot`이며 `PATH` / `HOME` /
`LANG`은 러너 프로세스에서 적용합니다.

## 워크스페이스 레지스트리

[workspaces.example.json](../workspaces.example.json)을 복사하고 `root`를
**등록한 실제 디렉터리**로 지정하세요. 모델은 워크스페이스를 추가할 수
없습니다.

```json
{
  "workspaces": {
    "demo": {
      "root": "/absolute/path/to/your/project",
      "profile": "workspace-write"
    }
  }
}
```

프로필: `read-only`(기본 의도) 또는 `workspace-write`. `host-admin`은
제품 프로필이 아닙니다. 선택적 운영자 `environments`는 `host` 또는
`linux-container`를 등록할 수 있습니다. 생략하면 암시적 로컬 호스트입니다.
`linux-container`는 exec 경로가 아닙니다. 도구와 `workspace_info`에는
`environment_id`가 없습니다.

## 게이트웨이 실행

stdio(Cursor / 로컬 MCP 호스트):

```bash
export CODESPACE_CONFIG="$PWD/docs/workspaces.example.json"
export CODESPACE_OPERATIONS_DB="$PWD/data/operations.sqlite"
mkdir -p data
./dist/codespace-mcp
```

Streamable HTTP(선택적 정적 Bearer — 실험 전용, 절대 로그하지 마세요):

```bash
export CODESPACE_HTTP_HOST=127.0.0.1
export CODESPACE_HTTP_PORT=8787
# export CODESPACE_HTTP_TOKEN="replace-me"
export CODESPACE_CONFIG="$PWD/docs/workspaces.example.json"
export CODESPACE_OPERATIONS_DB="$PWD/data/operations.sqlite"
./dist/codespace-mcp --http
```

엔드포인트: `http://127.0.0.1:8787/mcp`. 사용자 Inbox JSON은
`http://127.0.0.1:8787/inbox`이며 **같은** HTTP 리스너와 Bearer를
사용합니다. stdio 전용 모드는 `/inbox`를 노출하지 않습니다. 초안은
`POST /inbox/intents/{id}/queue` 전까지 모델에 넘어가지 않습니다.
바인드 주소는 공개 `Host` 헤더와 같지 않습니다. 리버스 프록시는 외부
호스트 이름을 따로 허용하세요. `0.0.0.0`을 그 이름으로 취급하지 마세요.

`CODESPACE_OPERATIONS_DB`가 없으면 작업과 의도 큐는 메모리에 있고
재시작 후 **남지 않습니다**. 프로세스 핸들은 재시작 후 절대 남지
않습니다.

선택적 러너 워커(여전히 호스트 exec이며 Linux 격리가 아님):

```bash
export CODESPACE_RUNNER=uds
export CODESPACE_RUNNER_SOCKET="$PWD/data/runner.sock"
export CODESPACE_RUNTIME_BIN="$PWD/dist/codespace-codex-runtime"
./dist/codespace-mcp
```

## MVP 흐름 재현

자동 커버리지(ChatGPT 계정 불필요):

```bash
cargo test -p codespace-server --test apply
cargo test -p codespace-server --test process
cargo test -p codespace-server --test protocol_compat
cargo test -p codespace-server --test inbox
cargo test -p codespace-server --test e2e
```

이 시험은 패치 적용/디스크 확인과 관리형 프로세스
(`exec_command` / `read_process` / `WORKSPACE_BUSY`)를 다룹니다.

수동 stdio: 위 환경 변수로 MCP 클라이언트를 `./dist/codespace-mcp`에
연결한 뒤, JSON `root`가 존재하고 프로필이 쓰기를 허용한 다음
`workspace_id: "demo"`에 대해 그 도구들을 호출하세요.

## Linux 격리 픽스처

[`deploy/compose.yml`](../../deploy/compose.yml)은 슬리퍼 픽스처입니다.
프로젝트를 uid `10001`로 `/workspace`에 **만** 바인드 마운트합니다.
호스트 `$HOME`, SSH 에이전트 소켓, `/var/run/docker.sock`, 게이트웨이
`.env`, Bearer 파일, operations SQLite 파일은 마운트하지 않습니다.
`codespace-mcp`를 실행하지 않으며 `exec_command`에 **연결되어 있지
않습니다**.

```bash
export CODESPACE_WORKSPACE=/absolute/path/to/your/project
docker compose -f deploy/compose.yml up --build
```

게이트웨이는 여전히 호스트에서 실행됩니다. `exec_command`는
워크스페이스를 cwd로 하는 호스트 `tokio::process::Command`입니다.

## 로그

- 게이트웨이 로그는 **stderr**로 갑니다(`RUST_LOG` / `tracing`, 기본
  `info`).
- 비밀 키(`authorization`, `token`, `bearer`, …)는 구조화 살균기에서
  가려집니다. 공유하는 셸 이력 문서에 `CODESPACE_HTTP_TOKEN`을 출력하지
  마세요.
- stderr 캡처는 직접 순환하거나 잘라내세요. 로그 SaaS는 없습니다.
- operations SQLite 파일은 **패치 작업** 행과 works/intents와 함께
  커집니다. 프로세스 핸들과 자원 잠금은 휘발성 메모리입니다.
  데이터베이스를 이후 러너 마운트에서 빼 두세요. 삭제하면 멱등 키를
  잊습니다.

## 연결 끊김 또는 재시작 후 복구

HTTP/JSON-RPC 요청 id ≠ `operation_id` ≠ `process_id` ≠ `work_id`. 잃어버린
HTTP 응답은 실행 실패가 아닙니다.

- `apply_patch`를 맹목적으로 다시 실행하지 말고, 서버가 발급한
  `operation_id` 또는 클라이언트 `operation_key` **정확히 하나**로
  `operation_status`를 호출하세요. 둘 다 주거나 둘 다 안 주면 오류입니다.
- 충돌 후 미완료 행은 `unknown`입니다. 서버는 이를 자동 재실행하지
  **않습니다**. UDS에서 `apply_patch` 응답이 유실되면 DB는 `unknown`이며
  디스크와 모순되는 `rejected`를 쓰지 않습니다. 워크스페이스를 검사한 뒤,
  그 변경이 여전히 필요하면 **새** `operation_key`를 시작하세요.
- 살아있는 `exec_command` 프로세스는 MCP 요청보다 오래 살 수 있습니다.
  발급된 `process_id`로 `read_process` / `terminate_process`를 사용하세요.
  전송이 모호하면 셸 임대를 유지합니다. 워커 `ProcessExited` 뒤에
  `release_process`가 풀어 무한 `WORKSPACE_BUSY`를 막습니다.
  게이트웨이 재시작 후 옛 OS PID는 CodeSpace 핸들로 재사용되지 않습니다.

## 이 문서가 검증하지 않는 것

- 이 세션에서 다른 사람의 노트북이나 클라우드 VM에 설치하기
- ChatGPT Custom Connector OAuth / 공개 HTTPS `Host` 헤더
- Docker 예제의 커널 탈출
