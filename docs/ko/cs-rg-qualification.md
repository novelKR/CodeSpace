<a id="cs-rg-qualification"></a>

# CS-RG qualification 절차

[English](../cs-rg-qualification.md) | [한국어](cs-rg-qualification.md)

이 문서는 CodeSpace가 DevGuard를 쓰는 방식(CS-RG)을 qualification하는 절차입니다. 어떤 조합을 qualification하는지, 어떤 모드를 지원한다고 주장하는지, 각 주장을 어떻게 시험하고 측정하는지, 무엇이 통과인지, 실행이 어떤 증거를 남기는지를 정합니다. 이 절차는 하네스 [`scripts/qualify-devguard.py`](../../scripts/qualify-devguard.py)와 함께, 첫 qualification 실행 전에 고정합니다. 첫 qualification 실행 뒤에 절차나 하네스를 바꾸면 새 절차가 됩니다. 다른 변경처럼 검토하고, 영향받는 실행은 다시 합니다. 각 단위가 무엇을 만들었는지는 [결합 로드맵](devguard-integration.md)에 있고, 이 문서는 그 결과가 qualification을 통과하는지를 판단합니다.

**현재 상태:** 절차와 하네스만 있습니다. qualification 실행은 아직 하지 않았고, 이 문서의 어떤 내용도 qualification 결과가 아닙니다. 이 절차로 한 실행이 통과하고 그 증거를 검토할 때까지 CS-RG는 진행 중이며 qualification을 마치지 않은 상태입니다.

<a id="candidate"></a>

## 후보 조합

한 번의 실행은 정확히 한 조합을 qualification합니다. 조합은 하네스의 `candidate` 스위트가 `candidate.json`에 기록하며, 손으로 적지 않습니다.

| 구성 요소 | 기록하는 내용 |
| --- | --- |
| CodeSpace | commit, Codex submodule 밖의 추적 파일이 그 commit과 다른지 여부, `docs/`·`docs-site/`·`*.md` 파일을 뺀 tree의 SHA-256(Codex gitlink 포함) |
| DevGuard client | `Cargo.lock`의 `devguard-*` 항목([client pin](upstream-lock.md#devguard-client-pin)) |
| Codex | gitlink와 `scripts/codex_patch.py check` 결과([기록된 패치](upstream-lock.md#codex-downstream-patch)) |
| 바이너리 | `codespace-mcp`(`-p codespace-server --features devguard`로 빌드)와 worker `codespace-codex-runtime`(`--features devguard`)의 SHA-256과 크기. fixture, helper, `devguard-qualify` 바이너리를 주면 그것도 기록 |
| DevGuard release | 설치된 release ID, 그 `MANIFEST.json`의 SHA-256, 설치된 각 바이너리가 manifest와 맞는지, 서비스 상태 |
| 정책 | `host.toml`의 SHA-256, `devguard doctor --require admission,registration,macos-cooperative`(구성 지문, capability, protocol), `codespace` 소비자의 role·generation·인스턴스 한도·control 예약. 소비자 credential digest는 기록에 읽어 들이지 않음 |
| DG-1 qualification | 그 release에 대한 DevGuard의 qualification 기록이 있으면 그 SHA-256, 판정, 같은 `host.toml`·지문·capability에서 측정했는지 여부 |
| 도구와 호스트 | `rustc -vV`, `cargo -V`, Python, Xcode, macOS 버전, 모델, 논리 CPU, 메모리, 부팅 시각, 서비스의 work capacity와 pressure 상태 |

`candidate`는 Codex submodule 밖에 커밋하지 않은 변경이 없고, Codex 확인이 통과하고(submodule이 고정 commit에 기록된 패치만 정확히 적용한 상태), 서비스가 그 release를 실행하고, 설치된 바이너리가 모두 manifest와 맞고, `doctor`가 만족하고, `host.toml`에 `control_service` role의 `codespace` 소비자가 있고, `PATH`에서 Rust 1.95.0이 먼저 잡힐 때 통과합니다. 최종 보고서에는 같은 정책에서 같은 release를 측정한 DG-1 qualification도 있어야 합니다. `candidate`는 그 기록을 요구하지 않고 기록만 하므로, 그 기록이 생기기 전에도 하네스를 실행할 수 있습니다.

코드, 바이너리, release, 정책, Codex, 도구 가운데 무엇이든 바뀌면 새 후보가 되고, 영향받는 실행을 다시 합니다. 문서만 바꾼 commit은 후보를 유지하며, code tree hash가 이를 보여 줍니다.

<a id="supported-modes"></a>

## 지원 모드

주장하는 칸은 각각 따로 시험합니다. 어떤 칸도 다른 칸에서 추론하지 않습니다.

| 플랫폼 | runner | I/O | 참여 | 주장 | 근거 |
| --- | --- | --- | --- | --- | --- |
| macOS, 실제 호스트 증거가 있는 설치 서비스 | `in-process` | pipe, PTY | `required` | qualification | 기능 행렬, 장애 사례, 제어 경로 측정 |
| macOS | `--runtime-bin`을 쓴 `uds`(worker가 실행을 소유) | pipe, PTY | `required` | qualification | 위와 같음. worker가 등록됨 |
| macOS | `--runner-socket`을 쓴 `uds` | pipe, PTY | `required` | 지원하지 않음 | attempt가 생기기 전에 `RESOURCE_POLICY_UNSUPPORTED`로 거부 |
| macOS | 모두 | pipe, PTY | `--devguard status`에서의 `required` | 지원하지 않음 | attempt가 생기기 전에 `RESOURCE_POLICY_UNSUPPORTED`로 거부 |
| macOS | 지원 모드 | pipe, PTY | 쓸 수 있는 launch helper가 없는 `required` | 지원하지 않음 | 실행 허용 뒤 attempt를 취소하고 `MANAGED_LAUNCH_UNAVAILABLE`로 거부 |
| macOS | 지원 모드 | pipe, PTY | gateway 하나에 `off`와 `required` workspace를 함께 | qualification | 관리 대상과 관리하지 않는 자식이 동시에 실행되고, 각 자식은 표준 descriptor만 가짐 |
| macOS, Linux | 모두 | pipe, PTY | `off` | 기존 동작 | `devguard` feature를 켜고 끈 [회귀 단계](#regressions) |
| Linux | 모두 | pipe, PTY | `required` | DG-LINUX 전까지 지원하지 않음 | 아무것도 실행하기 전에 거부. `--devguard register`에서는 실제 호스트 증거가 없어 소유자가 등록하지 못하므로 `RESOURCE_AUTHORITY_UNAVAILABLE`, 그 밖에는 `RESOURCE_POLICY_UNSUPPORTED`. Linux CI가 보여 줌 |

`devguard` feature 없이 빌드한 gateway에는 DevGuard 옵션이 없습니다. 그런 gateway에서 `required` workspace는 `--devguard status`일 때처럼 거부됩니다.

<a id="running"></a>

## 하네스 실행

하네스는 release를 설치하고 `codespace` 소비자를 등록한 Mac, 곧 qualification 호스트에서 시스템 Python(3.9 이상)으로 실행하며 외부 패키지를 쓰지 않습니다. 하네스가 시작하는 프로세스는 모두 최소 환경(`HOME`, `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, `TMPDIR`, `LANG=C`와 하네스가 지정하는 `CODESPACE_*` 설정)만 받으며, 하네스 자신도 같은 방식으로 시작합니다.

```bash
env -i HOME="$HOME" TMPDIR="$TMPDIR" PATH="<Rust 1.95.0>/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  /usr/bin/python3 scripts/qualify-devguard.py matrix --out evidence/<새 디렉터리> \
  --mcp-bin <codespace-mcp> --runtime-bin <codespace-codex-runtime> --release <release ID>
```

| 스위트 | 대상 | 추가 인자 |
| --- | --- | --- |
| `candidate` | 설치된 서비스 | `--release`. 기록하려면 `--fixture-bin`, `--fixture-helper`, `--qualify-bin`도 |
| `matrix` | 설치된 서비스 | `--release` |
| `faults` | DevGuard의 fixture authority | `--fixture-bin`(`crates/devguard`의 예제 `fixture_authority`), `--fixture-helper`(client pin에서 빌드한 `devguard-launch`) |
| `authority` | DevGuard의 fixture authority | `faults`와 같음 |
| `control` | DG-1 부하 아래의 설치된 서비스 | `--release`, `--work`(새 디렉터리), `--qualify-bin`(DevGuard의 `devguard-qualify`), `--devguard-checkout`(release의 소스 commit이 있는 DevGuard checkout), `--load cold` 또는 `warm`, `--rehearsal`, `--repetitions` |
| `verify` | 봉인된 증거 디렉터리 | 그 디렉터리 |

`--modes`로 스위트를 `in-process`나 `uds` 하나로 줄일 수 있으며, 기본은 둘 다입니다. 설치된 서비스를 쓰는 스위트는 기본으로 서비스의 socket, journal, 로그, `host.toml`, 소비자 credential 파일을 씁니다. credential 파일은 경로만 gateway에 넘기고 하네스는 읽지 않습니다. 종료 상태는 0 통과 또는 qualification, 1 실패, 2 미완료 또는 판정 불가, 3 하네스 실패, 130 중단입니다. `verify`는 실행 기록이 맞으면 0, 아니면 1로 끝납니다.

하네스는 다른 MCP client와 마찬가지로 표준 입출력으로 `codespace-mcp`와 통신하며(protocol `2025-11-25`), 자체의 작은 client를 씁니다. 따라서 시간 값은 하네스에서 잰 도구 호출의 로컬 왕복 시간이고 네트워크 시간은 들어가지 않습니다. DevGuard의 journal은 DevGuard 자신의 측정처럼 읽기 전용으로 읽고, 관리 대상 실행의 attempt ID와 scope root는 gateway 로그에서 가져옵니다.

<a id="functional-matrix"></a>

## 기능 행렬

`matrix`는 서비스가 release를 실행하고 자원을 차지한 작업이 없음을 확인한 뒤, 설치된 서비스를 대상으로 runner 모드마다 한 번 실행합니다. 관리 대상 workspace는 50 mCPU, 64 MiB, 8 tasks를 요청하고, gateway의 프로세스 timeout은 15초입니다. 자손을 남기거나, 그룹을 벗어나거나, 서비스를 재시작하는 명령은 실행하지 않습니다. 그래서 소유자의 서비스에 suspect로 남는 attempt가 생기지 않습니다.

| 시나리오 | pipe와 PTY | 통과 조건 |
| --- | --- | --- |
| registration | | `workspace_info`가 참여 `registration`, authority 사용 가능, 소유자(`in_process` 또는 `worker`) 등록, 실행 관리 중을 보고하고, DevGuard의 활성 `codespace` 인스턴스 하나가 그 프로세스임 |
| exits | 각각 | exit 0과 exit 3이 그 코드의 exited로 보고됨. SIGTERM으로 죽으면 pipe에서는 signal 15의 `signaled`, PTY에서는 고정된 Codex PTY가 launch descriptor를 물려받는 관리 대상 자식을 보고하는 대로 코드 143(128 + 15)의 exited([프로세스 상태](agent-integration.md)) |
| io | 각각 | pipe에서는 `write_stdin`이 `cat`에 닿고 출력이 돌아오며, `process_resize`가 `PROCESS_NOT_TTY`로 거부되고, 종료가 보고됨. PTY에서는 터미널이 24×80으로 시작하고, 40×100 크기 조정에 답하며 프로세스가 새 크기를 보고, stdin이 닿음 |
| termination | 각각 | `terminate_process`가 답하고, 프로세스가 terminated로 보고되며, scope의 프로세스 그룹이 사라짐 |
| timeout | 각각 | `sleep 600`이 `timeout`으로 보고됨 |
| exec-failed | 각각 | 인터프리터가 없거나 실행할 수 없는 스크립트가 errno 2나 13과 함께 `PROCESS_SPAWN_FAILED`로 거부됨. helper가 grant를 받았고 scope가 끝나자 DevGuard가 해제함 |
| shortage | 각각 | work capacity보다 많은 CPU를 요청하는 workspace에 세 번 호출하면 매번 새 attempt가 거부(denied)되어 `RESOURCE_UNAVAILABLE`이 되고, 자원을 차지하지 않으며, `WORKSPACE_BUSY`는 나오지 않음 |
| unsupported-minimum | 각각 | 최소 요구로 kernel의 task 강제를 요구하는 workspace가 DevGuard에서 거부되어 `RESOURCE_POLICY_UNSUPPORTED`가 됨 |
| mixed | 각각 | gateway 하나에서 관리하지 않는 실행과 관리 대상 실행이 동시에 실행되고, 각 자식은 descriptor 0, 1, 2만 가짐 |
| no-helper, unusable-helper | | launch helper가 없거나 존재하지 않는 helper를 준 gateway는 `MANAGED_LAUNCH_UNAVAILABLE`로 거부하고, DevGuard는 attempt를 취소(cancelled)로 기록하며, `off` workspace는 계속 실행됨 |
| status-mode | | `--devguard status`에서 관리 대상 실행은 attempt가 생기기 전에 `RESOURCE_POLICY_UNSUPPORTED`로 거부되고, `off`는 계속 실행됨 |
| authority-unavailable | | socket에 authority가 없으면 `RESOURCE_AUTHORITY_UNAVAILABLE`로 거부되고, DevGuard에는 그 attempt가 없으며, `off`는 계속 실행됨 |
| runner-socket | `uds`만 | gateway가 시작하지 않은 worker에서는 관리 대상 실행이 attempt가 생기기 전에 `RESOURCE_POLICY_UNSUPPORTED`로 거부됨 |
| settled | | 소비자의 어떤 attempt도 자원을 차지하지 않고 활성 인스턴스도 없음. DevGuard는 소유자가 사라진 것을 본 뒤에 인스턴스를 `retired`로 바꾸므로 둘 다 60초까지 기다림. 아니면 행렬을 멈춤 |

관리 대상 실행은 모두 DevGuard 자신의 attempt 기록과 다시 대조합니다([자원 회계](#accounting)).

<a id="fault-cases"></a>

## 장애 사례

`faults`가 넣는 장애는 설치된 서비스에 suspect나 자원을 차지한 작업을 남길 수 있으므로, DevGuard의 fixture authority를 대상으로 실행합니다. fixture authority는 CodeSpace의 client pin에 있는 DevGuard daemon 코드를 별도 상태 디렉터리에서 실행하며, `codespace` 소비자를 스스로 등록합니다. 사례마다 새 fixture authority를 쓰고, 관리 대상 workspace는 50 mCPU, 32 MiB, 4 tasks를 요청합니다.

| 사례 | pipe와 PTY | 통과 조건 |
| --- | --- | --- |
| descendant | 각각 | 자손을 남기고 끝난 root의 attempt는 자손이 실행되는 동안 suspect가 아닌 상태로 자원을 차지하고, 자손이 끝나면 해제됨. PTY에서는 종료 전후로 reconciler를 멈춰 두므로 reap 전 관찰만이 자손을 채택할 수 있음 |
| escape | 각각 | `setsid`로 scope의 그룹을 벗어난 자손이 있으면 모든 것이 끝난 뒤에도 attempt가 suspect로 자원을 차지함 |
| stalled-observer | 각각 | reconciler를 멈추고 authority를 관찰 상한보다 오래 잡아 두면, CodeSpace는 잡아 둔 시간이 끝나기 전에 root를 reap하고, 증명되지 않은 자손 때문에 attempt는 그 자손이 끝난 뒤에도 suspect로 남음 |
| authority-lost | | pipe와 PTY 실행 중에 authority를 죽여도 둘 다 계속 `running`으로 답하고 4초 안에 종료되며, 새 관리 대상 실행은 DevGuard에 아무것도 남기지 않고 `RESOURCE_AUTHORITY_UNAVAILABLE`로 거부되고, `off`는 계속 실행됨 |
| authority-restart | | 승인 후 재개한 실행 중에 authority를 재시작해도 프로세스는 계속 CodeSpace의 것이고 끝까지 실행되며, 그 attempt는 suspect로 자원을 차지하고, 승인을 다시 재개하면 같은 프로세스를 돌려주며 실행은 한 번뿐이고, 새 실행은 허용되고 해제됨 |
| authority-stalled | | authority의 lock을 소유자의 deadline보다 오래 잡아 두면 호출은 잡아 둔 시간이 끝나기 전에 `ADMISSION_UNKNOWN`이나 `RESOURCE_AUTHORITY_UNAVAILABLE`로 거부되고, 명령은 실행되지 않으며, DevGuard는 attempt를 helper에 넘기지 않고 자원을 남기지 않으며, 다음 실행은 새 attempt로 실행됨 |
| helper-ends | | READY 전에 끝나는 helper를 쓰면 호출은 `PROCESS_SPAWN_FAILED`(ready를 보고하기 전에 끝남)로 거부되고, 명령은 실행되지 않으며, DevGuard는 grant를 시작하지 않은 것(`no_helper_created`)으로 해제함 |
| credential | | authority가 거부하는 credential과 없는 credential 각각에서 관리 대상 실행은 `credential_refused` 또는 `credential_unavailable`을 밝히며 `RESOURCE_AUTHORITY_UNAVAILABLE`로 거부되고, `off`는 계속 실행됨 |
| terminate-race | | 프로세스가 스스로 끝나는 순간 근처에서 pipe와 PTY를 번갈아 12번 종료해도 각각 exited나 terminated로 한 번만 끝나고, 한 번만 실행되며, 해제됨 |
| shutdown | | 관리 대상 pipe와 PTY 실행 중에 gateway의 입력을 닫으면(`uds` 모드에서는 worker도 함께 끝남) gateway와 소유자가 종료하고, 두 scope가 끝나 해제되며, 새 gateway는 등록하고 실행하되 둘 중 어느 것도 다시 실행하지 않음 |
| worker-killed | `uds`만 | worker가 관리 대상 실행을 준비하는 중에 worker를 죽여도 gateway는 호출에 답하고, 명령은 실행되지 않으며, DevGuard는 attempt를 helper에 넘기지 않고 자원을 남기지 않으며, gateway는 계속 실행됨 |

<a id="fault-coverage"></a>

### 작업 지시서의 장애 목록과의 대응

| 장애 | 보여 주는 곳 |
| --- | --- |
| 실행 허용 거부 | 행렬 `shortage`, `unsupported-minimum`(설치된 서비스, 두 모드, pipe와 PTY) |
| 실행 허용 전 authority 사라짐 | 행렬 `authority-unavailable`, 장애 `credential` |
| 시작 뒤 authority 사라짐 | 장애 `authority-lost`, `authority-restart`, `authority` 스위트 |
| launch 답 잃음 | 세션이 기다리는 요청에 대해서는 장애 `authority-stalled`. 등록을 지난 요청의 답을 잃는 경우는 소유자 밖에서 만들 수 없으므로, DevGuard의 client 시험과 스크립트 worker를 쓰는 CodeSpace의 runner·gateway 시험이 다룸 |
| helper 실패 | 행렬 `no-helper`, `unusable-helper`, 장애 `helper-ends` |
| READY 뒤 프로그램 실행 실패 | 행렬 `exec-failed`(errno 2와 13, pipe와 PTY) |
| 관찰 시간 초과 | 장애 `stalled-observer` |
| 살아남은 자손 | 장애 `descendant`, `escape` |
| 종료와 exit의 경쟁 | 장애 `terminate-race` |
| shutdown과 exit의 경쟁 | 장애 `shutdown` |
| runner 연결 끊김 | 준비 중인 경우는 장애 `worker-killed`. launch 자체 중에 worker를 잃는 경우는 스크립트 worker를 쓰는 gateway 시험이 다룸 |
| 오래되거나 중복된 요청 | 장애 `authority-restart`(다시 재개하면 같은 프로세스, 실행 한 번)와 제어 측정의 중복 실행 수. runner wire에서 준비·launch 메시지를 다시 보내는 경우는 시험하지 않으며, 알려진 공백임 |
| 불확실한 실행 | 장애 `authority-stalled`. 불확실한 attempt가 다시 허용되면 제어 측정이 실패함 |
| credential 실패 | 장애 `credential` |
| journal·저장소 거부 | 주입하지 않음. fixture authority는 journal이 거부하게 만들 수 없음. 거부 자체는 DevGuard의 스위트가, 그 코드의 대응은 CodeSpace 시험이 다룸 |

fixture authority를 쓰는 gateway 시험은 DevGuard 시험 바이너리가 있는 macOS에서만 실행되며, qualification 호스트의 [회귀 단계](#regressions)에서 실행합니다. macOS CI는 adapter 시험, runner의 DevGuard 시험, gateway의 `devguard` 모듈 시험을 실행하지만 그 gateway 시험은 실행하지 않습니다.

<a id="control-path"></a>

## 제어 경로 가용성

목표는 DevGuard가 이미 정한 목표 가운데 제어 경로에 해당하는 것이며([SLO 절차](https://github.com/novelKR/DevGuard/blob/ea98d7fadf6ac10e9393db923105bf31f719952a/docs/planning/verification.md#slo-protocol)), 여기서 새로 정하지 않습니다. 로컬 `process_status` p99 500 ms 이하, 종료 확인 응답 p99 1초 이하(scope가 실제로 끝나는 시간은 따로 보고), MCP 연결 끊김 없음, 두 번 실행된 실행 없음, 불확실한 실행의 재시작 없음입니다. 포그라운드 브라우저 목표는 DG-1의 것이며 여기서는 측정하지 않습니다. 새 관리 대상 실행의 거부 지연과 출력 응답성은 정해진 목표가 없으므로 측정해 보고하며, 설계의 250 ms 준비 예산을 참고값으로 씁니다.

<a id="control-intervals"></a>

### 구간과 표본

`control`은 runner 모드(`in-process`, `uds`)마다 세 번 반복해 측정합니다. 한 번의 반복은 10분 유휴 구간과 그 뒤 30분 이상의 부하 구간입니다. 구간마다 설치된 서비스에 등록한 새 gateway를 시작하며, 관리 대상 workspace는 각각 25 mCPU, 16 MiB, 2 tasks를 요청합니다. 표본은 고정 일정으로 수집합니다.

| 일정 | 주기 | 재는 것 |
| --- | --- | --- |
| status | 1초, 대상 둘 | 실행 중인 관리 대상 `cat`(pipe)과 `sleep`(PTY)의 `process_status` |
| termination | 20초 | pipe와 PTY를 번갈아 새로 시작한 관리 대상 `sleep`. `terminate_process`의 답까지, 그리고 따로 exited 상태까지, scope의 프로세스 그룹이 사라질 때까지, DevGuard journal에 attempt 해제가 보일 때까지 |
| I/O | 5초 | `write_stdin`으로 `cat`에 쓴 번호 붙은 줄이 `read_process`로 돌아올 때까지 |
| refusal | 30초 | work capacity보다 많은 CPU를 요청하는 workspace의 관리 대상 실행. 거부되어야 함 |
| host | 5초 | 메모리 pressure 수준, swap, load average |
| service | 60초 | 서비스가 여전히 그 release를 실행하는지와 그 프로세스 ID |
| footprint | 5초 | 실행 소유자의 RSS, thread, 프로세스, CPU(`uds` 모드에서는 gateway의 값도). 소비자의 control 예약과 비교해 보고 |

앞 표본이 아직 끝나지 않은 동안 지나간 일정은 놓친 것으로 기록하며, 늦게 재지 않습니다. 놓친 일정, 오류, 이미 실행 중이 아닌 대상에 대한 답은 빠진 표본이고, 빠진 표본은 DG-1처럼 nearest-rank 백분위에서 모든 값보다 큰 순위로 셉니다. 용량이나 pressure 때문에 허용이 거부된(`RESOURCE_UNAVAILABLE`) 종료 대상은 표본이 아닙니다. 구간이 끝나면 두 status 대상을 종료하며, 각 종료는 실제로 효과가 있어야 합니다.

<a id="control-load"></a>

### 부하

부하는 DG-1의 것입니다. 주어진 DevGuard checkout의 `scripts/measure.py`에 있는 소비자 여섯을, 설치된 release의 `devguard exec`로 `devguard-qualify`와 release 소스의 Cargo 빌드와 함께 실행합니다. 그래서 pressure와 대기열은 서비스 자신의 것입니다. `--load cold`(기본)는 빈 target 디렉터리에서 빌드하고, `--load warm`은 먼저 target 디렉터리를 빌드하며 그 빌드가 실패하면 판정 불가입니다. qualification 실행은 cold 조건을 씁니다. warm 실행은 추가 조건이며, 하면 따로 보고합니다. 소비자는 구간의 예정 끝까지 실행하고, 그 뒤 DG-1의 완료 한도 안에서 진행 중인 실행을 마칩니다. 하네스의 대상과 부하는 work capacity의 절반, 곧 메모리 경고 아래에서 DevGuard가 유지하는 용량 안에 들어갑니다.

<a id="control-verdicts"></a>

### 판정

구간은 다음과 같으면 **판정 불가**입니다. 이는 잘못된 관찰이며 통과가 아닙니다. 시작할 때 다른 작업이 자원을 차지하고 있었거나, journal을 읽을 수 없었거나, status 대상을 시작하지 못했거나, 원시 증거를 쓰지 못했거나, 서비스를 관찰할 수 없었거나 다른 release가 선택되었거나, 부하가 한도 안에 끝나지 않은 경우입니다.

그렇지 않으면 다음 중 하나일 때 **실패**입니다. status p99가 500 ms를 넘음, 확인 응답 p99가 1초를 넘음, 종료가 확인되었는데 대상이 끝나지 않음, 종료한 대상이 해제되지 않음, I/O 줄이 돌아오지 않음, 어떤 표본에서든 MCP 연결이 끊김, status 대상의 마지막 종료가 효과가 없음, 측정 중인 release가 재시작했거나 비정상이 됨, 부하 실행이 불확실하거나 부하 attempt가 자원을 계속 차지함, 하네스의 attempt가 자원을 계속 차지함, 한 실행이 두 번 이상 실행됨, 불확실한 attempt가 다시 허용됨.

관찰이 너무 적어도 **판정 불가**입니다. status 표본이 일정의 90%보다 적거나, 종료 표본이 80%보다 적거나, 부하가 DG-1의 것이 아닌 경우(성공한 실행이 없는 소비자, 구간의 4분의 1보다 짧게 실행된 소비자, 실패한 실행이 있는 소비자, 허용된 부하가 구간의 절반보다 짧게 실행됨)입니다. 그 밖에는 **통과**입니다.

반복은 두 구간이 모두 통과하고 그 뒤에도 서비스가 같은 프로세스로 그 release를 실행할 때 통과입니다. 유휴 구간이 실패하면 그 반복은 판정 불가이며, 부하의 실패로 보지 않습니다. runner 모드는 세 반복이 모두 통과하면 qualification을 통과하고, 하나라도 실패하면 실패합니다. 반복 사이에 값을 합치지 않습니다. 실행 전체는 전체 절차(두 모드, 세 반복, 10분과 30분)에서 두 모드가 모두 통과할 때만 qualification을 통과하고, 어느 한쪽이 실패하면 실패합니다. `--rehearsal`(유휴 30초, 부하 60초, 한 번 반복)은 실행을 시험해 보는 것이며 qualification을 통과하지 않습니다.

<a id="authority-failure"></a>

### authority 장애

`authority`는 fixture authority가 정상이 아닐 때의 프로세스 제어를, runner 모드마다 15초짜리 다섯 단계로, 계속 실행 중인 같은 status 대상에 대해 측정합니다. 단계는 `baseline`, `paused`(reconciler 멈춤), `stalled`(단계 내내 lock을 잡음), `restarted`, `stopped`(프로세스를 죽임)입니다. status는 1초마다, I/O echo는 3초마다 재고, 장애 전에 시작한 관리 대상 셋을 3, 7, 11초에 종료하며, 1, 5, 9, 13초에 새 관리 대상 실행을 시도합니다. 대상은 DevGuard가 허용한 뒤에 시작합니다. authority를 잡아 둔 동안 host 표본이 오래되면 DevGuard는 표본이 30초 동안 정상일 때까지 새 작업을 받지 않으므로, `stalled` 뒤에는 `RESOURCE_UNAVAILABLE`로 거부된 대상을 90초까지 1초마다 다시 시도하고, 걸린 시간을 단계 기록에 남깁니다. 단계마다 위의 구간 규칙으로 판정하며, 새 실행은 authority가 답할 수 있는 동안(`baseline`, `paused`, `restarted`)에는 허용되고, 답할 수 없는 동안(`stalled`, `stopped`)에는 `RESOURCE_AUTHORITY_UNAVAILABLE`이나 `ADMISSION_UNKNOWN`으로 거부되어야 합니다. 재시작한 뒤나 멈춘 뒤에는 DevGuard가 장애 전에 시작한 대상을 해제할 수 없으므로, 그 단계에서 해제되지 않은 대상은 실패가 아니라 보고 대상입니다.

<a id="accounting"></a>

## 자원 회계

하네스는 CodeSpace 자신의 판단이 아니라 DevGuard의 기록을 확인합니다. 행렬의 관리 대상 실행마다 gateway가 로그에 남긴 attempt를 journal에서 읽어, 예약하고 적용한 양이 workspace의 요청과 같은지, CPU·메모리·tasks가 모두 `applied`인지, scope root가 gateway가 실행한 프로세스인지, scope가 끝나면 attempt가 `scope_terminated`로 해제되어 자원을 차지하지 않고 추적이 끝까지 유지되었는지 확인합니다. 거부된 호출은 그 attempt를 밝히고, DevGuard는 그 attempt를 기대한 단계(`denied`, `cancelled`, `released`)로 자원 없이 기록하거나, 거부가 먼저 일어난 경우에는 attempt가 없어야 합니다. 제어 부하 아래에서 잰 gateway와 worker의 footprint는 `codespace` 예약과 비교해 보고합니다. 예약은 DevGuard의 회계 추정이므로 이 비교는 판정에 쓰지 않습니다.

<a id="regressions"></a>

## 회귀와 동등성

후보 조합에서, qualification 호스트의 최소 환경으로 `python3 scripts/validate-upstream.py all macos-core macos-fork-race`를 실행해 모든 단계를 돌립니다. DevGuard 시험 바이너리를 쓰는 단계에서는 그 바이너리가 꼭 있어야 합니다. Linux는 후보 head의 CI가 보여 주며, 그 `dependencies` 단계가 `devguard` feature를 켠 의존성 그래프와 끈 그래프를 모두 확인합니다. 어느 단계에서든 회귀가 있으면 qualification은 실패합니다.

<a id="evidence"></a>

## 증거

`verify`를 뺀 모든 스위트는 새 디렉터리를 만들어 쓰고 봉인합니다. 여기에는 `report.json`(스위트, 인자, 하네스 파일 hash와 commit, 판정, 마지막에 정리한 내용), 스위트의 기록, 원시 표본의 JSON lines가 들어가며, 각 gateway의 MCP 대화 기록과 로그도 포함됩니다. 봉인은 모든 파일의 SHA-256을 `MANIFEST.json`에 기록하고 파일을 읽기 전용으로 바꿉니다. 실패, 판정 불가, 중단된 실행도 봉인해 보관합니다. 어떤 디렉터리에도 credential, credential digest, 자식 프로세스의 환경은 들어가지 않습니다.

`qualify-devguard.py verify <디렉터리>`는 manifest를 확인하고 원시 파일에서 모든 판정을 다시 계산합니다. 시나리오 판정은 그 확인 항목에서, authority 단계와 제어 구간은 각자의 표본에서, 그다음 반복, runner 모드, 실행 전체 순서로 계산합니다. 실행했던 하네스와 지금 확인하는 하네스가 같은지도 보고하며, 아무것도 쓰지 않습니다.

<a id="legacy-off-path"></a>

## 기존 `off` 경로 결정

작업 지시서가 기존 backend에 대해 요구하는 결정 기록(CSRG-C09)입니다.

**결정:** 참여 `off`를 호환 경로이자 기본값으로, 바꾸지 않고 유지합니다.

- **범위.** 기본값이자 설정이 없을 때의 의미인 참여 `off`. 모든 플랫폼과 runner 모드, pipe와 PTY에 해당합니다. 이 실행은 요청한 명령 자체입니다. pipe 자식은 관리 대상 실행과 같은 CodeSpace의 reaper 하나를 쓰고, PTY 자식은 reap 전 관찰 창이 없는 고정 Codex PTY spawn을 씁니다.
- **이유.** 작업 지시서는 깔끔함만을 위해 코드를 옮기지 않습니다. `off`는 제품의 기본값이고, `required`는 운영자가 명시적으로 고르는 것이며 DG-LINUX 전까지 Linux에서는 지원하지 않으므로, Linux에서는 `off`가 유일한 경로입니다. 관리형 실행만 남기면 Linux의 모든 실행과 DevGuard가 없는 모든 환경이 사라집니다.
- **중복된 부분.** spawn 경로는 하나이며 실행할 프로그램만 고릅니다. 요청한 명령이거나, permit과 report descriptor를 받는 DevGuard의 launch helper입니다([`crates/runner/src/process.rs`](../../crates/runner/src/process.rs)의 `How::Direct`와 `How::Managed`). reap은 중복되지 않습니다. 어느 쪽이든 pipe 자식은 CodeSpace의 reaper 하나가, PTY 자식은 고정 Codex PTY의 waiter가 reap합니다. PTY는 호출 하나만 다릅니다. 관리하지 않는 실행은 `spawn_inheriting`, 관리 대상 실행은 [기록된 패치](upstream-patches.md)의 `spawn_observed`를 씁니다. 관리 대상 PTY 자식은 launch descriptor도 물려받으므로 고정 Codex PTY는 descriptor를 유지하는 경로로 띄우고, 이 경로는 신호로 끝난 것을 128에 신호 번호를 더한 exit code로 보고합니다. 관리하지 않는 PTY는 1로 보고합니다([프로세스 상태](agent-integration.md)). 관리 대상 경로에는 실행 허용, launch, 정리가 더해지며([`crates/runner/src/launch.rs`](../../crates/runner/src/launch.rs)), `off`는 이 경로에 들어가지 않습니다. `devguard` feature가 없으면 빌드에 DevGuard 패키지가 없고, 있으면 DevGuard의 client와 contract crate만 들어가며, `dependencies` 단계가 이를 확인합니다.
- **`off`가 제공하지 않는 것.** DevGuard의 실행 허용, 예약, 회계가 없고, scope 단속이 없으며, 관리하지 않는 PTY root를 reap 전에 관찰하지 않습니다. Linux 명령 sandbox는 `off` 실행만 쓸 수 있고, sandbox가 켜진 곳에서 관리 대상 실행은 거부됩니다.
- **재검토 시점.** DG-LINUX가 Linux 강제 보호를 qualification할 때, Codex pin 이동으로 PTY spawn이 바뀔 때, `required`가 기본값이 될 때입니다.
- **제거 기준.** 독립적인 제품상의 이유가 있고, 이 경로 없이도 지원하는 모든 모드의 동작이 유지된다는 회귀 증거가 있을 때만 제거합니다.
