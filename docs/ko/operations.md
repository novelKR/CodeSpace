<a id="운영"></a>
<a id="운영"></a>

# 설치와 운영

[English](../operations.md) | [한국어](operations.md)

macOS 또는 Linux에서 개인용 단일 사용자 서버를 준비하는 방법입니다. 설치 후 도구 호출 순서는 [Agent Loop 연동](agent-integration.md)을 참고하세요.

<a id="설치"></a>

## 설치

Git과 CI에서 사용하는 최신 Rust stable 도구 체인을 준비하세요. 루트 manifest에는 Rust 1.88이 선언되어 있지만 Codex 어댑터 전체의 최소 빌드 버전으로 검증된 값은 아닙니다. 고정된 업스트림 소스는 Rust 1.95.0을 지정합니다. macOS에는 Xcode 명령줄 도구를 설치하세요. Linux 명령 격리에는 샌드박스 도우미, bubblewrap, 필요한 네임스페이스를 생성할 권한이 추가로 필요합니다. Docker는 별도의 [컨테이너 실험 구성](../../deploy/README.md)을 사용할 때만 필요합니다.

```bash
git clone --recurse-submodules https://github.com/novelKR/CodeSpace.git
cd CodeSpace
# 이미 복제한 저장소에서도 서브모듈을 초기화합니다.
git submodule update --init --recursive
cargo build --locked -p codespace-server --bin codespace-mcp --release
cargo build --locked --manifest-path crates/patch/Cargo.toml --bin codespace-patch --release
mkdir -p dist
cp target/release/codespace-mcp dist/
cp crates/patch/target/release/codespace-patch dist/
```

서버가 패치 도우미를 찾을 수 있도록 두 실행 파일을 같은 디렉터리에 두세요. 다른 위치에 두려면 `CODESPACE_PATCH_BIN`에 절대 경로를 지정합니다. 도우미에서 Codex 패치 라이브러리를 호출하므로 서버만 빌드해서는 패치를 적용할 수 없습니다.

Linux 명령 격리를 사용하려면 다음 도우미도 빌드하여 서버 옆에 둡니다.

```bash
cargo build --locked --manifest-path crates/linux-sandbox/Cargo.toml --bin codespace-linux-sandbox --release
cp crates/linux-sandbox/target/release/codespace-linux-sandbox dist/
```

빌드 성공만으로 격리가 활성화되었다고 판단할 수 없습니다. 연결 후 `workspace_info.execution.isolation.command_sandbox`를 확인하세요. 자세한 내용은 [활성 조건과 실패 처리](runner-isolation.md)에 있습니다.

<a id="워크스페이스-레지스트리"></a>
<a id="워크스페이스-레지스트리"></a>

## 작업 공간 등록

사용할 프로젝트 디렉터리를 먼저 만들고, 레지스트리는 그 밖에 둡니다. 아래 `/absolute/path/to/project`를 실제 프로젝트 경로로 바꾸세요.

```json
{
  "workspaces": {
    "demo": {
      "root": "/absolute/path/to/project",
      "profile": "workspace-write",
      "network": "restricted",
      "approvals": "off"
    }
  }
}
```

CodeSpace 저장소의 `workspaces.json`으로 저장합니다. `read-only`는 읽기를 허용하고, `workspace-write`는 패치와 명령 실행도 허용합니다. 경로와 권한은 운영자가 등록합니다. 도구에 전달하는 `workspace_id`는 등록 항목을 선택할 뿐, 권한을 부여하지 않습니다.

`network` 기본값은 `restricted`입니다. `enabled`를 사용하려면 Linux 도우미가 필요하며, 지원되는 HTTP 통신은 관리 프록시를 거칩니다. 호스트 네트워크에 무제한 접근하는 설정이 아닙니다. 도우미를 사용할 수 없으면 `enabled` 실행은 실패합니다. `restricted`이고 도우미가 없으면 호스트 실행은 가능하지만 네트워크 제한은 OS 수준에서 강제되지 않습니다. 실제 환경의 적합성은 응답의 정책 집행 상태를 확인해 판단하세요.

`approvals` 기본값은 `off`이며, 정책이 허용한 패치와 명령을 바로 실행합니다. `confirm`이면 `begin()`이나 프로세스 시작 전에 해당 도구를 홀드하고 `APPROVAL_REQUIRED`와 `approval_id`를 반환합니다. 같은 패치나 exec를 다시 보내면 활성 홀드를 재사용합니다. `network`와 같은 운영자 JSON이며 MCP 도구 인자가 아니고 프로필을 올리지 않습니다. 확인 행은 패치 원장과 다른 `approvals` 테이블에 저장되며 `CODESPACE_OPERATIONS_DB`를 같이 씁니다. 홀드가 `pending`·`granted`·`resuming`인 동안 이 테이블은 V4A 패치나 exec argv를 보관합니다. `denied` 또는 `consumed` 뒤에는 본문을 digest 메타(도구, 작업 공간, fingerprint)로 바꿉니다. 패치 원장은 패치 본문이 아니라 해시를 보관합니다. 확인과 재개는 [Agent Loop 연동](agent-integration.md)을 참고하세요.

<a id="게이트웨이-실행"></a>
<a id="게이트웨이-실행"></a>

## 서버 시작

CodeSpace 저장소에서 절대 경로와 패치·작업 기록 저장 위치를 설정합니다.

```bash
export CODESPACE_CONFIG="$PWD/workspaces.json"
mkdir -p data
export CODESPACE_OPERATIONS_DB="$PWD/data/operations.sqlite"
export CODESPACE_PATCH_BIN="$PWD/dist/codespace-patch"
# 기본 제한은 30초입니다. 운영자가 빌드에 맞는 시간을 지정합니다.
export CODESPACE_PROCESS_TIMEOUT_SECS=300
```

stdio를 사용하려면 MCP 클라이언트가 위 환경변수를 전달하고 `dist/codespace-mcp`의 절대 경로를 실행하도록 설정합니다. 서버의 기본 전송 방식은 stdio입니다. 터미널에서 `./dist/codespace-mcp`를 실행하면 MCP 입력을 기다리며, 대화 화면이 열리지는 않습니다. 로그는 stderr로, MCP 메시지는 stdout으로 출력합니다.

Streamable HTTP를 사용하려면 다음 프로세스를 계속 실행해 둡니다.

```bash
export CODESPACE_HTTP_HOST=127.0.0.1
export CODESPACE_HTTP_PORT=8787
# 클라이언트가 Bearer 토큰을 보낸다면 CODESPACE_HTTP_TOKEN을 안전하게 설정합니다.
./dist/codespace-mcp --http
```

MCP 클라이언트에서 `http://127.0.0.1:8787/mcp`로 연결합니다. 같은 포트의 `/inbox`는 JSON API이며 동일한 선택적 Bearer 인증을 사용합니다. stdio 전용 모드에는 `/inbox`가 없습니다. 서버는 `.env.example`을 자동으로 읽지 않습니다.

공개 HTTPS와 리버스 프록시는 별도로 검증해야 합니다. 현재 HTTP Host 허용 목록은 루프백과 바인드 호스트 값으로 구성되며, 공개 호스트를 따로 지정하는 설정은 없습니다. 따라서 공개 도메인 요청이 거부될 수 있습니다. `0.0.0.0`에 바인드하는 것과 외부 호스트 이름을 허용하는 것은 다릅니다.

<a id="mvp-흐름-재현"></a>
<a id="mvp-흐름-재현"></a>

## 첫 연결 확인

MCP 초기화와 도구 목록 조회를 마친 뒤 `workspace_info`를 `{"workspace_id":"demo"}`로 호출합니다. 파일·프로세스 사용 가능 여부가 의도한 권한과 맞는지 확인하세요. 이어서 프로젝트의 알려진 파일을 읽고 [작은 패치와 프로세스 예시](agent-integration.md)를 실행합니다.

`available`은 권한과 백엔드 지원 여부를 나타내며 현재 점유 상태는 포함하지 않습니다. 이후 패치나 명령에서 `WORKSPACE_BUSY`가 발생할 수 있습니다. `linux-container` 환경은 등록할 수 있지만 파일·명령 실행 백엔드는 아직 없습니다.

## 선택적 Unix 소켓 worker

worker를 빌드하고 게이트웨이를 시작하기 전에 선택합니다.

```bash
cargo build --locked --manifest-path crates/codex-runtime/Cargo.toml --bin codespace-codex-runtime --release
cp crates/codex-runtime/target/release/codespace-codex-runtime dist/
export CODESPACE_RUNNER=uds
export CODESPACE_RUNTIME_BIN="$PWD/dist/codespace-codex-runtime"
```

worker는 같은 호스트에서 실행하는 별도 프로세스이며 컨테이너가 아닙니다. 게이트웨이가 전용 소켓 디렉터리를 만들고 자식 프로세스를 관리합니다. worker 연결이 끊기거나 게이트웨이가 종료되면 해당 worker의 프로세스도 종료됩니다. 재접속과 프로세스 복구는 지원하지 않습니다. 기본값은 `in-process`입니다.

<a id="로그"></a>
<a id="로그"></a>
<a id="연결-끊김-또는-재시작-후-복구"></a>
<a id="연결-끊김-또는-재시작-후-복구"></a>

## 로그와 제한, 복구

| 설정 또는 제한 | 동작 |
| --- | --- |
| `RUST_LOG` | stderr 로그 수준. 기본값 `info` |
| `CODESPACE_OPERATIONS_DB` 미설정 | 패치 작업과 지시 큐를 메모리에 보관하며 재시작 시 사라짐 |
| `CODESPACE_PROCESS_TIMEOUT_SECS` | 양의 정수. 기본 30초. 러너 환경에 설정 |
| `CODESPACE_MAX_PROCESSES` | 러너 전체의 실행 중 프로세스 기본 상한 8개. 작업 공간별 점유 규칙도 적용 |
| 프로세스 출력 | 마지막 256 KiB 보관. stdout/stderr를 합침. `read_process`는 `output_lost`와 `retained_from`을 보고하고, 종료는 `process_status`로 조회 |
| 종료된 핸들 | 기본 최대 15분, 최대 64개 보관. 영구 저장하지 않음 |

로그와 데이터베이스는 토큰·게이트웨이 설정과 같이 관리 대상 작업 공간 밖에 두세요. stderr 로그의 보관·순환은 운영자가 관리합니다. Bearer 토큰을 로그나 커밋에 넣지 마세요. 데이터베이스를 삭제하면 패치 중복 실행 방지 기록과 확인 홀드 행도 사라집니다.

패치 응답을 받지 못했다면 `operation_id` 또는 `operation_key` 중 하나만 지정해 `operation_status`를 조회합니다. 재시작 후 미완료 기록은 `unknown`이 되므로 파일을 확인한 뒤 다음 행동을 결정하세요. 프로세스는 `process_id`로 관리하며 `operation_status`로 복구할 수 없습니다. 프로세스가 죽을 때 `resuming`이던 확인 홀드는 패치 원장에서 복구하거나, 저장된 단말 결과를 재현하거나, `APPROVAL_AMBIGUOUS`를 반환할 수 있습니다. exec는 다시 spawn하지 않습니다. [재시도와 복구 규칙](agent-integration.md)을 참고하세요.

<a id="linux-격리-픽스처"></a>
<a id="linux-격리-픽스처"></a>
<a id="이-문서가-검증하지-않는-것"></a>
<a id="이-문서가-검증하지-않는-것"></a>

## 문제 해결

| 증상 | 먼저 확인할 항목 |
| --- | --- |
| `WORKSPACE_NOT_FOUND` | 레지스트리 경로, 등록 ID, 서버에 전달된 환경변수 |
| 패치 도우미 시작 실패 | `codespace-patch` 빌드 여부와 절대 경로 |
| `WORKSPACE_BUSY` | 기존 명령이 끝났는지 확인하거나 종료한 뒤 패치 |
| 명령 시간 초과 | 운영자 제한 시간. 긴 빌드가 완료되었다고 가정하지 않기 |
| Linux에서 샌드박스가 없다고 표시 | 도우미 위치, bubblewrap, 네임스페이스 지원 확인 |
| `enabled` 명령 시작 실패 | 사용 가능한 Linux 샌드박스 도우미 필요 |
| HTTP 요청 거부 | `/mcp` 경로, Bearer 헤더, Host 검증 |

로컬 전송 테스트는 실제 ChatGPT 연결, 모든 패키지 관리자의 프록시 호환성, 커널·컨테이너 탈출 방어까지 검증하지 않습니다.
