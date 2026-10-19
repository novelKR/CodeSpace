<a id="운영"></a>
<a id="운영"></a>

# 설치와 운영

[English](../operations.md) | [한국어](operations.md)

macOS 또는 Linux에서 개인용 단일 사용자 서버를 준비하는 방법입니다. 설치 후 도구 호출 순서는 [Agent Loop 연동](agent-integration.md)을 참고하세요.

<a id="설치"></a>

## 설치

Git, Python 3, CI에서 사용하는 최신 Rust stable 도구 체인을 준비하세요. 루트 manifest에는 Rust 1.88이 선언되어 있지만 Codex 어댑터 전체의 최소 빌드 버전으로 검증된 값은 아닙니다. 고정된 업스트림 소스는 Rust 1.95.0을 지정합니다. macOS에는 Xcode 명령줄 도구를 설치하세요. Linux 명령 격리에는 샌드박스 도우미, bubblewrap, 필요한 네임스페이스를 생성할 권한이 추가로 필요합니다. Docker는 별도의 [컨테이너 실험 구성](../../deploy/README.md)을 사용할 때만 필요합니다.

```bash
git clone --recurse-submodules https://github.com/novelKR/CodeSpace.git
cd CodeSpace
# 이미 복제한 저장소에서도 서브모듈을 초기화합니다.
git submodule update --init --recursive
# CodeSpace가 기록한 Codex 패치를 적용합니다. 서브모듈을 갱신할 때마다 다시 실행합니다.
python3 scripts/codex_patch.py apply
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

`approvals` 기본값은 `off`이며, 정책이 허용한 패치와 명령을 바로 실행합니다. `confirm`이면 `begin()`이나 프로세스 시작 전에 해당 도구를 홀드하고 `APPROVAL_REQUIRED`와 `approval_id`를 반환합니다. 같은 패치나 exec를 다시 보내면 활성 홀드를 재사용합니다. `network`와 같은 운영자 JSON이며 MCP 도구 인자가 아니고 프로필을 올리지 않습니다. 확인 행은 패치 원장과 다른 `approvals` 테이블에 저장되며 `CODESPACE_OPERATIONS_DB`를 같이 씁니다. 홀드가 `pending`·`granted`·`queued`·`resuming`인 동안 이 테이블은 V4A 패치나 exec argv를 보관합니다. `denied` 또는 `consumed` 뒤에는 본문을 digest 메타(도구, 작업 공간, fingerprint)로 바꿉니다. 패치 원장은 패치 본문이 아니라 해시를 보관합니다. 확인과 재개는 [Agent Loop 연동](agent-integration.md)을 참고하세요.

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

worker는 같은 호스트에서 실행하는 별도 프로세스이며 컨테이너가 아닙니다. 게이트웨이가 전용 소켓 디렉터리를 만들고 자식 프로세스를 관리합니다. worker 연결이 끊기거나 게이트웨이가 종료되면 해당 worker의 프로세스도 종료됩니다. 재접속과 프로세스 복구는 지원하지 않습니다. MCP/HTTP 클라이언트 끊김은 그 프로세스를 죽이지 않습니다. 기본값은 `in-process`입니다. 프로세스가 게이트웨이보다 오래 살고, 게이트웨이가 그 프로세스를 작업 데이터베이스에 기록하며, 연결을 잃은 게이트웨이가 다시 Claim한 뒤 자기 기록과 대조하는 [독립 Runner](runner-isolation.md#independent-runner)는 [qualification](recovery-qualification.md)이 통과시킨 조합에서만 지원하며, 아직 통과한 조합은 없습니다([도입과 되돌리기](#independent-runner-adoption)). 이런 Runner 자체를 잃으면 운영자가 `codespace-mcp records`로 그 Runner가 실행하던 것을 정리합니다([Runner 손실](#runner-loss)).

<a id="로그"></a>
<a id="로그"></a>
<a id="연결-끊김-또는-재시작-후-복구"></a>
<a id="연결-끊김-또는-재시작-후-복구"></a>

## 로그와 제한, 복구

| 설정 또는 제한 | 동작 |
| --- | --- |
| `RUST_LOG` | stderr 로그 수준. 기본값 `info` |
| `CODESPACE_OPERATIONS_DB` 미설정 | 패치 작업과 지시 큐를 메모리에 보관하며 재시작 시 사라짐. 독립 Runner에 연결하는 게이트웨이는 이 설정 없이 시작하지 않음 |
| `CODESPACE_PROCESS_TIMEOUT_SECS` | 양의 정수. 기본 30초. 러너 환경에 설정 |
| `CODESPACE_MAX_PROCESSES` | 러너 전체의 실행 중 프로세스 기본 상한 8개. 넘는 요청은 프로세스를 만들기 전에 거부. 작업 공간별 점유 규칙도 적용 |
| 프로세스 출력 | 마지막 256 KiB 보관. stdout/stderr를 합침. `read_process`는 `output_lost`와 `retained_from`을 보고하고, 출력이 빠진 이유는 `lost_reason`이 알려 줌. 종료는 `process_status`로 조회. 실행 중인 PTY 크기는 `process_resize`로 변경 |
| 종료된 핸들 | 기본 최대 15분, 최대 64개 보관. 영구 저장하지 않음. 독립 Runner는 끝난 프로세스의 상태와 출력을 게이트웨이가 확인할 때까지 보관하며, 핸들을 놓은 뒤에는 최대 64개까지 보관함([끝난 프로세스](runner-isolation.md#reconciliation)). 그 뒤에는 게이트웨이가 출력 없이 실행 기록으로 답하며 `lost_reason`은 `retention_expired` |

로그와 데이터베이스는 토큰·게이트웨이 설정과 같이 관리 대상 작업 공간 밖에 두세요. stderr 로그의 보관·순환은 운영자가 관리합니다. Bearer 토큰을 로그나 커밋에 넣지 마세요. 데이터베이스를 삭제하면 패치 중복 실행 방지 기록과 확인 홀드 행도 사라집니다.

<a id="operations-database"></a>

**스키마 버전과 이전 버전.** 작업 데이터베이스에는 스키마 버전이 적혀 있습니다. CodeSpace는 자기보다 새 스키마 버전의 데이터베이스를 아무것도 쓰기 전에 거부하고 그대로 둡니다. 그런 데이터베이스는 그것을 쓴 CodeSpace 버전으로 여세요. [독립 Runner](runner-isolation.md#process-records)에 연결한 게이트웨이는 실행도 이 데이터베이스에 기록하고, [Runner를 Claim](runner-isolation.md#claim)할 때 밝히는 데이터베이스의 임의 식별자를 이 데이터베이스에 두며, 실행 중에는 그 옆의 lock 파일 `<database>.gateway.lock`을 잡고 있습니다. 실행 기록이 생기기 전의 CodeSpace 버전도 이런 데이터베이스를 열 수 있고 기록은 건드리지 않지만, 스키마 버전을 확인하지도 lock을 걸지도 않습니다. 그러니 독립 Runner의 게이트웨이가 쓰는 데이터베이스에서는 이전 버전을 실행하지 마세요. 독립 모드에서 되돌아갈 때는 그 모드를 쓰지 않고 데이터베이스는 그대로 둡니다. 이전 사본을 복원하면 그 뒤에 쓴 패치 중복 실행 방지 기록, 확인 홀드, 실행 기록이 사라집니다.

패치 응답을 받지 못했다면 `operation_id` 또는 `operation_key` 중 하나만 지정해 `operation_status`를 조회합니다. 재시작 후 미완료 기록은 `unknown`이 되므로 파일을 확인한 뒤 다음 행동을 결정하세요. 프로세스는 `process_id`로 관리하며 `operation_status`로 복구할 수 없습니다. 프로세스가 죽을 때 `queued`이던 확인 홀드는 다시 acquire합니다. `resuming`이던 홀드는 패치 원장에서 복구하거나, 저장된 단말 결과를 재현하거나, `APPROVAL_AMBIGUOUS`를 반환할 수 있습니다. exec는 다시 spawn하지 않습니다. 독립 Runner에 연결한 게이트웨이는 `resuming`으로 남은 exec 홀드를 [대조](runner-isolation.md#reconciliation)할 때 정리합니다. Runner가 그 홀드를 위해 만든 프로세스로 consumed가 되거나, Runner가 아무것도 만들지 않았음이 증명되면 다시 granted가 됩니다. [재시도와 복구 규칙](agent-integration.md)을 참고하세요.

<a id="independent-runner-adoption"></a>

## 독립 Runner 도입과 되돌리기

[독립 Runner](runner-isolation.md#independent-runner)는 [qualification](recovery-qualification.md)이 통과시킨 조합에서만 지원합니다. 그 조합은 macOS에서 [qualification 보고서](recovery-qualification-report.md#recovery-report-combination)가 기록한 하나뿐입니다. 이 모드는 opt-in이고 기본값은 계속 `in-process`입니다. 통과한 조합에서만, 그것도 qualification을 받은 CodeSpace 빌드, DevGuard release, 정책 그대로 도입하세요. 그 밖의 것은 모두 새 후보입니다.

1. **준비합니다.** 모든 workspace 밖에 상태 디렉터리를 두고, 실행 기록이 생기기 전의 CodeSpace 버전이 열지 않을 작업 데이터베이스([스키마 버전](#operations-database))와 Runner용 mode 0700 소켓 디렉터리를 준비합니다. DevGuard를 쓰면 Runner가 실행 소유자로서 `codespace` 소비자의 인스턴스 하나로 등록하므로, 그 소비자로 등록하는 다른 게이트웨이가 있으면 먼저 멈춥니다.
2. **Runner를 시작합니다.** qualification이 시작하듯 Runner를 자기 세션에서 시작해, 터미널이 닫히거나 어떤 게이트웨이가 끝나도 Runner에 닿지 않게 합니다. Linux에서는 `setsid`가 이렇게 하며, 아래 Python 줄은 macOS와 Linux 모두에서 같은 일을 합니다. DevGuard를 쓰면 [상태 전용 연결](devguard-integration.md#devguard-status-connection)의 소비자 설정과 release의 `devguard-launch`를 줍니다. DevGuard 없이 쓰면 `--devguard-*` 설정을 빼며, 이때 `required` workspace는 거부됩니다.

   ```bash
   python3 -c 'import os, sys; os.setsid(); os.execv(sys.argv[1], sys.argv[1:])' \
     "$PWD/dist/codespace-codex-runtime" "$RUNNER_DIR/runner.sock" --lifecycle independent \
     --devguard-socket "$DEVGUARD_SOCKET" --devguard-consumer codespace \
     --devguard-generation "$DEVGUARD_GENERATION" \
     --devguard-credential-file "$DEVGUARD_CREDENTIAL_FILE" \
     --devguard-launch-helper "$DEVGUARD_LAUNCH" >>"$STATE_DIR/runner.log" 2>&1 </dev/null &
   ```

3. **게이트웨이를 시작합니다.** 종료 의도 `detach`와 작업 데이터베이스를 주고, DevGuard를 쓰면 같은 소비자 설정으로 `--devguard register`를 줍니다. 한 데이터베이스는 한 번에 한 게이트웨이만 맡으며, 다른 게이트웨이는 최대 15초 기다린 뒤 시작하지 않습니다.

   ```bash
   ./dist/codespace-mcp --runner uds --runner-socket "$RUNNER_DIR/runner.sock" \
     --runner-lifecycle independent --runner-exit-intent detach \
     --operations-db "$STATE_DIR/operations.sqlite" --devguard register ...
   ```

4. **게이트웨이를 재시작합니다.** SIGTERM이나 SIGUSR1을 보내거나 입력을 닫아 게이트웨이를 끝내고, 같은 설정으로 다음 게이트웨이를 시작합니다. Runner의 프로세스는 계속 실행되고, 다음 게이트웨이는 Runner를 Claim해 남은 것을 대조한 뒤 요청을 받습니다. `RUNNER_RECONNECTING`과 `RECOVERY_PENDING`은 클라이언트에게 잠시 뒤 재시도하라고 알립니다([Agent Loop 연동](agent-integration.md)).
5. **Runner를 drain합니다.** 더 쓰지 않을 때는 Runner의 프로세스가 끝나기를 기다리거나 종료한 뒤, `--runner-exit-intent stop`으로 게이트웨이를 시작하고 그 입력을 닫습니다. 그 게이트웨이가 Runner를 Claim해 중지하면, Runner는 아직 가진 것을 끝내고 소켓을 지운 뒤 종료 상태 0으로 끝납니다. Runner에 SIGTERM을 보내도 같은 방식으로 멈추지만, 그때 연결된 게이트웨이는 Runner를 잃으므로 게이트웨이를 먼저 멈추세요. Runner가 끝났고, `codespace-mcp records list`가 기록을 하나도 나열하지 않으며, DevGuard를 쓰면 그 인스턴스가 retire되었을 때 drain이 끝난 것입니다. 인스턴스가 retire되면 소비자의 다음 소유자가 `resource_authority.registration.state`를 `registered`로 보고합니다.

**되돌리기.** 독립 모드로 게이트웨이를 더 시작하지 않고, 5단계처럼 Runner를 drain한 뒤, 이전에 qualification을 통과한 조합으로 돌아갑니다. 같은 DevGuard release와 정책으로 `in-process` 모드나 `--runtime-bin`을 쓰는 `uds` 모드의 게이트웨이를 씁니다([qualification과 도입](devguard-integration.md#devguard-qualification)). 작업 데이터베이스는 그대로 둡니다. 이전 사본을 복원하지 말고, 실행 기록이 생기기 전의 CodeSpace 버전으로 열지 마세요. 잃어서 drain할 수 없는 Runner는 [Runner 손실](#runner-loss)대로 정리합니다. DevGuard가 그 인스턴스를 retire할 때까지는 다른 소유자가 그 소비자로 등록할 수 없습니다.

<a id="runner-loss"></a>

## Runner 손실

[독립 Runner](runner-isolation.md#independent-runner)에 연결한 게이트웨이에만 해당하며, 이 모드는 [qualification](recovery-qualification.md)을 통과한 곳에서만 지원합니다. 그 Runner가 kill되거나, 크래시하거나, 멈추거나, 호스트가 재시작하면 Runner가 실행하던 것을 Runner에서 더는 알아낼 수 없고, 그 프로세스는 Runner보다 오래 살 수 있습니다([Runner 손실](runner-isolation.md#runner-loss)). 게이트웨이는 그 프로세스에 대해 기록으로 `lost_reason`과 함께 `unknown`이라고 답합니다. 그 프로세스가 점유하던 workspace는 운영자가 그중 아무것도 실행 중이지 않음을 확인하고 기록을 풀 때까지 계속 거부되며, 다음 게이트웨이에서는 `RECOVERY_PENDING`으로 거부됩니다. CodeSpace는 이 프로세스를 하나도 종료하지 않고, 아무것도 다시 시작하지 않으며, 이 프로세스에 대해 DevGuard에 묻지도 않습니다.

1. **기록을 나열합니다.** 아직 workspace를 점유할 수 있는 기록을 보여 줍니다. 데이터베이스를 읽기만 하므로 게이트웨이가 실행 중이어도 됩니다.

   ```bash
   ./dist/codespace-mcp records list --operations-db "$CODESPACE_OPERATIONS_DB"
   ```

   결과는 JSON입니다. 각 기록에는 `process_id`와 `workspace_id`, `dispatching`·`running`·`unknown` 중 하나인 `phase`, 게이트웨이나 release가 손실로 표시했으면 `runner_lost` 또는 `host_rebooted`이고 그 전에는 null인 `lost`, `participation`과 DevGuard `attempt_id`, 그 실행이 재개한 `approval_id`, `tty`가 있습니다. `runner`에는 Runner의 PID와 부팅, 호스트의 이번 부팅에서 실행되었는지를 뜻하는 `this_boot`, 그 PID를 가진 프로세스가 지금 실행 중인지를 뜻하는 `pid_running`이 있습니다. 이 프로세스는 그 PID를 이어받은 다른 프로세스일 수 있습니다. `root`에는 root 프로세스와, 바로 그 프로세스가 아직 실행 중인지를 뜻하는 `running`이 있으며, 기록으로 알 수 없으면 `running`은 null입니다.

2. **아직 실행 중인 것을 찾습니다.** 호스트가 재시작했다면(`host_rebooted`이거나 `this_boot`가 false) 아무것도 실행 중이지 않습니다. 그렇지 않다면 다음을 확인합니다.
   - `running`이 true이면 root 프로세스가 실행 중입니다. `ps -o pid,pgid,sid,lstart,command -p <pid>`는 프로세스를 그 프로세스 그룹, 세션과 함께 보여 줍니다.
   - pipe 프로세스와 그 프로세스가 시작한 것은, 스스로 벗어나지 않는 한 자신이 시작된 프로세스 그룹에 남습니다. 그 그룹은 Runner의 프로세스 그룹이거나, 네트워크를 허용한 샌드박스 도우미와 DevGuard의 launch helper처럼 root가 이끄는 그룹입니다. `pgrep -g <pgid>`는 그룹을, `pgrep -P <pid>`는 프로세스의 자식을 나열하고, `lsof -d cwd`는 어떤 프로세스가 workspace 디렉터리에서 작업하는지 보여 줍니다. Runner가 끝난 뒤 출력에 쓴 프로세스는, SIGPIPE를 무시하지 않았다면 그 신호로 끝났을 가능성이 큽니다.
   - `tty: true` 프로세스는 자기 세션을 이끌며, `pgrep -s <root pid>`가 그 세션을 나열합니다. 이 프로세스는 Runner가 끝날 때 받았을 가능성이 큰 SIGHUP을 무시한 경우에만 Runner보다 오래 삽니다.
   - Runner를 잃었을 때 아직 `dispatching`이어서 root가 없는 기록은 프로세스를 시작했을 수도, 시작하지 않았을 수도 있습니다. 위와 같이 찾아봅니다.
   - Linux 명령 샌드박스에서 네트워크를 허용하지 않은 workspace의 명령은 Runner와 함께 끝나지만, 네트워크를 허용한 workspace의 명령은 그렇지 않습니다. 나머지는 Runner의 서비스가 멈출 때 서비스 관리자가 이미 종료했을 수 있습니다. systemd는 기본값 `KillMode=control-group`에 따라 서비스의 control group을, launchd는 job이 `AbandonProcessGroup`을 설정하지 않았다면 job의 프로세스 그룹을 종료합니다. 가정하지 말고 확인하세요.

3. **아직 실행 중인 것을 종료합니다.** 끝까지 실행하지 않기로 정했다면 `kill -TERM <pid>`를 보내고, 끝나지 않으면 `kill -KILL <pid>`를 보냅니다. deadline을 지키게 하던 Runner가 없으므로 이제 deadline이 이 프로세스를 끝내지 않습니다.

4. **`required` 기록은 DevGuard가 그 attempt를 더는 과금하지 않을 때까지 기다립니다.** 잃은 Runner는 DevGuard의 실행 소유자였고, DevGuard는 attempt의 프로세스가 끝났음을 관찰하거나 호스트가 재시작할 때까지 그 attempt를 과금합니다. macOS에서 `devguard admission --open`은 DevGuard의 `quiescence`를 출력합니다. `charged_attempts`는 과금 중인 attempt 수이고, `attempts`는 그 key를 최대 16개 보여 줍니다. 그 key 중 하나가 기록의 `attempt_id`를 가졌거나 `charged_attempts`가 나열된 key 수보다 크면 DevGuard가 아직 그 attempt를 과금하고 있을 수 있습니다. 이 명령은 admission이 닫혀 있으면 다시 엽니다. 중단된 DevGuard upgrade가 admission을 닫힌 채로 남기므로, 그런 upgrade는 [DevGuard 운영 문서](https://github.com/novelKR/DevGuard/blob/7cd86e4c56cdd6c6067ceda183b8f3f82075fcaa/docs/ko/operations.md#제공-명령)대로 먼저 마치세요. DevGuard에는 quiescence를 읽기만 하는 명령이 없습니다. `quiescence`가 null로 출력되면 실행 중인 DevGuard release가 이를 보고하지 않는 것이므로 DevGuard가 그 attempt를 아직 과금하는지 알 수 없습니다. 이때는 호스트가 재시작할 때까지 기록을 그대로 두세요. 호스트가 재시작하면 DevGuard에서도 그 attempt가 끝납니다. [DevGuard 대조 계약](https://github.com/novelKR/DevGuard/blob/7cd86e4c56cdd6c6067ceda183b8f3f82075fcaa/docs/ko/contracts.md#대조)이 설명하듯 DevGuard가 attempt의 프로세스를 추적하지 못하게 되면, 프로세스가 모두 끝난 뒤에도 호스트가 재시작할 때까지 그 attempt를 과금합니다. DevGuard가 attempt를 과금하는 동안에는 `required` 기록을 풀지 마세요.

5. **게이트웨이를 멈추고 기록을 풉니다.**

   ```bash
   ./dist/codespace-mcp records release --operations-db "$CODESPACE_OPERATIONS_DB" <process_id>...
   ```

   실행 중인 게이트웨이는 lock 파일을 잡고 있으므로, 이 명령은 그 데이터베이스에서 게이트웨이가 실행 중이지 않을 때만 기록을 바꿉니다. Runner를 잃은 기록을 그 root 프로세스가 더는 실행 중이지 않을 때 풀고, `released`, `already_released`, 그리고 각각 이유가 붙은 `refused`를 JSON으로 출력합니다. 하나라도 거부하면 종료 상태 1로 끝납니다. 끝난 기록은 workspace를 점유하지 않으므로 거부하고, root 프로세스가 아직 실행 중인 기록과 Runner가 아직 실행 중일 수 있는 기록도 거부합니다. 게이트웨이가 실행되지 않는 동안 멈춘 Runner는 자기 기록을 `running`으로 남깁니다. 이 명령은 그 Runner의 PID를 가진 프로세스가 하나도 없을 때만 그 기록을 그 Runner의 끝나지 않은 다른 기록과 함께 손실로 표시합니다. 이 명령은 프로세스를 종료하지 않습니다. 푼 기록은 `unknown`으로 남고, 그 실행이 재개한 승인에 대해 `operation_resume`은 `APPROVAL_AMBIGUOUS`로 답합니다. 그 명령은 다시 실행되지 않습니다.

6. **Runner와 게이트웨이를 다시 시작합니다.** 게이트웨이는 각 workspace를 대조하며, 기록이 모두 끝났거나 풀린 workspace는 다시 새 작업을 받습니다. DevGuard를 쓰면 새 Runner는 `codespace` 소비자의 인스턴스 하나로 등록하는데, DevGuard는 잃은 Runner의 인스턴스를 retire한 뒤에만, 즉 그 Runner의 attempt가 더는 과금되지 않을 때만 이를 허용합니다. 그때까지 `workspace_info`는 `resource_authority.registration.state`를 `refused`로 보고하고 `required` workspace는 실행되지 않습니다. `off` workspace는 영향을 받지 않습니다. DevGuard는 소비자의 인스턴스가 하나라도 retire되지 않은 동안 그 소비자의 인스턴스 한도를 바꾸지 못하게 합니다.

`RECOVERY_PENDING`이 기록, Runner, DevGuard가 서로 맞지 않는다고 알리는 workspace는 그 Runner가 실행 중인 동안 `records release`로 정리되지 않습니다. 그 Runner를 멈추면 그 Runner가 점유하던 모든 workspace에서 끝나지 않은 기록이 잃은 Runner의 기록이 되고, 그 뒤에는 위 단계를 따릅니다.

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
| `RESOURCE_QUEUE_FULL` | 대기자가 한도 초과. 잠시 뒤 재시도 |
| 명령 시간 초과 | 운영자 제한 시간. 긴 빌드가 완료되었다고 가정하지 않기 |
| Linux에서 샌드박스가 없다고 표시 | 도우미 위치, bubblewrap, 네임스페이스 지원 확인 |
| `enabled` 명령 시작 실패 | 사용 가능한 Linux 샌드박스 도우미 필요 |
| HTTP 요청 거부 | `/mcp` 경로, Bearer 헤더, Host 검증 |
| 게이트웨이가 Runner를 잃었다고 알리는 `RUNNER_DISCONNECTED`, 또는 `codespace-mcp records`를 알려 주는 `RECOVERY_PENDING` | [Runner 손실](#runner-loss) 절차를 따르기 |

로컬 전송 테스트는 실제 ChatGPT 연결, 모든 패키지 관리자의 프록시 호환성, 커널·컨테이너 탈출 방어까지 검증하지 않습니다.
