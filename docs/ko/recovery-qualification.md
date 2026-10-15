<a id="recovery-qualification"></a>

# Gateway 복구 qualification 절차

[English](../recovery-qualification.md) | [한국어](recovery-qualification.md)

이 문서는 Gateway 복구(P1-RECOVERY)의 opt-in 모드인 [독립 Runner](runner-isolation.md#independent-runner)를 qualification하는 절차입니다. 어떤 조합을 지원한다고 주장하는지, 각 주장을 어떻게 시험하고 측정하는지, 무엇이 통과인지, 실행이 어떤 증거를 남기는지를 정합니다. [CS-RG qualification 절차](cs-rg-qualification.md)를 따르며, 그 후보 조합, 증거, 확인 방식을 그대로 쓰고, 하네스도 같은 [`scripts/qualify-devguard.py`](../../scripts/qualify-devguard.py)입니다. 이 절차는 첫 qualification 실행 전에 하네스와 함께 고정합니다. 첫 실행 뒤에 둘 중 하나라도 바꾸면 새 절차가 되며, 다른 변경처럼 검토하고 영향받는 실행은 다시 합니다. 각 단위가 무엇을 만들었는지는 [결합 로드맵](devguard-integration.md#resource-gateway-recovery)에 있고, 이 문서는 그 결과가 qualification을 통과하는지를 판단합니다.

**현재 상태:** 첫 qualification 실행 전에 하네스와 함께 고정했습니다. 아직 통과한 조합이 없으므로, 독립 Runner는 어디에서도 지원하는 모드가 아닙니다.

<a id="recovery-candidate"></a>

## 후보 조합

한 번의 실행은 정확히 한 조합을 qualification합니다. 조합은 CS-RG처럼 하네스의 `candidate` 스위트가 기록합니다([후보 조합](cs-rg-qualification.md#candidate)). CodeSpace commit과 그 code tree, DevGuard client pin, 기록된 패치를 적용한 Codex pin, 바이너리, 설치된 DevGuard release와 그 정책, 도구와 호스트입니다. 독립 Runner는 `uds` worker와 같은 `codespace-codex-runtime` 바이너리입니다. 실행은 설치된 DevGuard release와 정책을 있는 그대로 쓰며, 둘 다 바꾸지 않고 아무것도 설치하거나 provisioning하지 않습니다. 코드, 바이너리, release, 정책, Codex, 도구 가운데 무엇이든 바뀌면 새 후보가 되고 영향받는 실행을 다시 합니다. 문서만 바꾼 commit은 후보를 유지합니다.

<a id="recovery-supported-modes"></a>

## 지원 모드

주장하는 칸은 각각 따로 시험합니다. 어떤 칸도 다른 칸에서 추론하지 않습니다. CS-RG가 qualification한 모드는 그 주장을 유지하며, 실행은 그 주장이 여전히 맞음을 보여 줍니다([회귀](#recovery-regressions)).

| 플랫폼 | runner | I/O | 참여 | 주장 | 근거 |
| --- | --- | --- | --- | --- | --- |
| macOS, 실제 호스트 증거가 있는 설치 서비스 | `--runner-lifecycle independent`를 쓴 `uds`, 운영자가 시작한 독립 Runner | pipe, PTY | `off`와 `required` | qualification | 두 참여 모두로 실행하는 recovery 스위트와 gateway 재시작 측정 |
| Linux | 같음 | pipe, PTY | `off` | CI 기능 시험만 | Linux CI에서 실행하는 CodeSpace의 독립 Runner 시험. 하네스의 `off` 시나리오도 Linux에서 실행됨. 기능 확인일 뿐 Linux qualification이 아님 |
| Linux | 같음 | pipe, PTY | `required` | DG-LINUX 전까지 지원하지 않음 | Linux의 모든 `required` 실행처럼 아무것도 실행하기 전에 거부 |
| macOS, Linux | 같음 | pipe, PTY | Runner 자신이 재시작한 뒤 프로세스 복원 | 지원하지 않음 | P1-RECOVERY 범위 밖. gateway가 잃은 Runner 자리에 새 Runner를 받아들이지 않음을 `runner-replaced`가 보여 줌 |

독립 모드는 opt-in입니다. 이 모드를 기본값으로 바꾸는 것은 이 절차 밖의 별도 결정입니다.

<a id="recovery-running"></a>

## 하네스 실행

하네스는 CS-RG 스위트처럼 qualification 호스트에서 실행합니다([하네스 실행](cs-rg-qualification.md#running)). 시스템 Python을 쓰고, 외부 패키지를 쓰지 않으며, 하네스 자신과 하네스가 시작하는 모든 프로세스는 최소 환경만 받습니다. 독립 Runner는 하네스가 운영자처럼 직접 시작합니다. `codespace-codex-runtime <socket> --lifecycle independent`를 자기 세션에서, 전용 소켓 디렉터리로 실행하고, 그 로그를 증거로 남깁니다. `required`이면 쓰는 authority의 DevGuard 설정을 주며, credential 파일은 경로만 넘기고 하네스는 읽지 않습니다. LaunchAgent로 설치하는 것은 없고 정책도 바꾸지 않습니다. gateway는 `--runner uds --runner-socket <socket> --runner-lifecycle independent`, 종료 의도, 작업 데이터베이스로 실행하며, `required`이면 `--devguard register`도 줍니다.

| 스위트 | 보여 주는 것 | 추가 인자 |
| --- | --- | --- |
| `recovery-lifecycle` | gateway가 끝나는 모든 방식을 지나는 Runner, timeout, 설정([생명주기](#recovery-lifecycle)) | `--participation`. macOS에서 `required`이면 `faults`처럼 `--fixture-bin`과 `--fixture-helper` |
| `recovery-store` | 실행 기록과 작업 데이터베이스의 단일 writer([실행 기록](#recovery-store)) | `recovery-lifecycle`과 같음 |
| `recovery-fence` | Claim, controller epoch, 소켓에 닿은 다른 프로세스([Claim과 fence](#recovery-fence)) | `recovery-lifecycle`과 같음 |
| `recovery-reconcile` | 재시작 뒤 대조, 그 경쟁, authority 장애([대조](#recovery-reconcile)) | `recovery-lifecycle`과 같음 |
| `recovery-observation` | 출력 공백, 보관, Runner 손실([관찰과 Runner 손실](#recovery-observation)) | `recovery-lifecycle`과 같음 |
| `recovery` | 위 스위트 모두, 그 뒤 [gateway 재시작 측정](#recovery-measurement) | 위와 같고, macOS에서 `required`이면 `control` 스위트의 `--release`, `--work`, `--qualify-bin`, `--devguard-checkout`, `--load`, `--rehearsal`, `--repetitions` |

`--participation`은 `off`, `required`, 또는 둘 다(기본)를 받습니다. `off` 시나리오는 authority 없이 모든 플랫폼에서 실행합니다. macOS에서 `required` 시나리오는 CS-RG 장애 사례처럼 DevGuard의 fixture authority를 대상으로 실행하며, 시나리오마다 새 fixture authority가 설치 정책처럼 인스턴스 하나짜리 `codespace` 소비자를 스스로 provisioning합니다. 관리 대상 workspace는 50 mCPU, 32 MiB, 4 tasks를 요청합니다. Linux에서 `required` 시나리오는 이유와 함께 실행하지 않은 것으로 기록하고 세지 않습니다. 스위트는 실행한 시나리오가 모두 통과하고 하나 이상 실행했을 때 통과합니다. 종료 상태는 CS-RG 스위트와 같습니다.

```bash
env -i HOME="$HOME" TMPDIR="$TMPDIR" PATH="<Rust 1.95.0>/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  /usr/bin/python3 scripts/qualify-devguard.py recovery --out evidence/<새 디렉터리> \
  --mcp-bin <codespace-mcp> --runtime-bin <codespace-codex-runtime> \
  --fixture-bin <fixture_authority> --fixture-helper <devguard-launch> \
  --release <release ID> --work <새 디렉터리> --qualify-bin <devguard-qualify> \
  --devguard-checkout <DevGuard checkout>
```

<a id="recovery-functional"></a>

## 기능 스위트

시나리오마다 작업 디렉터리, 작업 데이터베이스, Runner를 따로 쓰고, `required`이면 fixture authority도 따로 씁니다. 시나리오가 남긴 실행 중인 것은 그 뒤에 끝냅니다. 시나리오는 확인 항목이 모두 통과해야 통과합니다. 시나리오가 따로 말하지 않으면 gateway의 프로세스 timeout은 10분이고 종료 의도는 `detach`입니다. I/O 칸의 "둘 다"는 pipe로 한 번, PTY로 한 번 실행한다는 뜻이고, 참여 칸은 그 시나리오를 실행하는 참여를 나열합니다.

<a id="recovery-lifecycle"></a>

### 생명주기

| 시나리오 | I/O | 참여 | 통과 조건 |
| --- | --- | --- | --- |
| exit-kinds | 둘 다 | `off`, `required` | 프로세스가 실행되는 동안 gateway가 끝나는 모든 방식으로 gateway를 끝내고, 매번 다음 gateway를 시작합니다. 종료 의도 `detach`로 입력을 닫거나 SIGTERM을 보내고, `stop`으로 SIGUSR1을 보내고(재시작은 언제나 detach), SIGKILL을 보냅니다. Runner와 프로세스는 그 모두보다 오래 살고, 다음 gateway마다 다음 epoch로 Runner를 Claim해 같은 프로세스가 실행 중임을 확인하며 앞 gateway가 멈춘 곳부터 출력을 이어 읽습니다. 그 뒤 `stop`인 gateway의 입력을 닫으면 프로세스가 끝나고, Runner는 소켓을 지운 뒤 종료 상태 0으로 끝납니다 |
| original-timeout | 둘 다 | `off`, `required` | 두 프로세스가 gateway 재시작을 지나 timeout에 이릅니다. 첫째는 gateway가 하나도 실행되지 않을 때, 둘째는 프로세스 timeout이 훨씬 긴 뒤 gateway가 요청을 받는 동안입니다. 각각 시작할 때 기록에 있던 deadline에 끝나고, 재시작이 옮기거나 늘린 deadline에 끝나지 않으며, `timeout`으로 보고됩니다 |
| runner-sigterm | | `off`, `required` | 운영자가 멈추듯 Runner에 SIGTERM을 보내면 controller의 Stop처럼 pipe와 PTY 프로세스를 끝내고, 종료 상태 0으로 끝나며 소켓을 지웁니다. 그 gateway는 두 프로세스 모두 실행 중이 아니라고 보고하고 새 작업을 거부하며, authority가 있으면 두 attempt가 해제되고 인스턴스가 retire됩니다 |
| invalid-settings | | `off` | 서로 맞지 않는 설정은 Runner에 닿기 전에 거부됩니다. 독립 생명주기 없이 준 종료 의도, 종료 의도 없이 준 독립 생명주기, in-process, 시작할 worker가 있는 경우, 소켓이나 작업 데이터베이스가 없는 경우, 독립 Runner에 연결한 managed gateway, managed worker에 연결한 독립 gateway입니다. Runner는 전용이 아닌 소켓 디렉터리, 공유 디렉터리, 소켓이 없는 경우, 알 수 없는 생명주기, 건네받은 credential, 다른 Runner가 듣고 있는 소켓을 거부합니다. 듣고 있던 Runner는 그대로입니다. Claim을 기록하지 않았고, 그 뒤 gateway가 epoch 1로 Runner를 Claim해 명령을 실행합니다 |

<a id="recovery-store"></a>

### 실행 기록

| 시나리오 | I/O | 참여 | 통과 조건 |
| --- | --- | --- | --- |
| records | 둘 다 | `off`, `required` | Runner에 보낸 실행에 대해, gateway 재시작 뒤에도 작업 데이터베이스가 다음을 가집니다. Hello가 밝힌 Runner의 정체, controller epoch, workspace, argv가 아닌 argv의 digest, pipe 실행의 root 프로세스(PTY의 root는 적지 않음), 원래 deadline, 그리고 뒤 gateway가 종료한 뒤의 끝난 방식 |
| writer-lock | | `off` | 첫 gateway가 요청을 받는 동안 같은 데이터베이스로 시작한 둘째 gateway는 정해진 시간만 기다린 뒤 lock을 가진 쪽을 밝히며 시작하지 않고, Runner에 닿지 않습니다. 첫 gateway가 끝나는 동안 시작한 gateway는 그것을 기다린 뒤 요청을 받으며, 그 뒤에 Runner를 Claim합니다 |
| schema | | `off` | 새 스키마 버전의 데이터베이스와 데이터베이스가 아닌 파일은 Runner에 닿기 전에 거부되고 바뀌지 않습니다 |
| records-command | | `off` | `codespace-mcp records list`는 아직 workspace를 점유할 수 있는 기록을 나열하고, `records release`는 gateway가 실행되지 않을 때만, 끝나지 않은 실행만, 그리고 그 Runner의 PID가 어떤 프로세스로 실행 중이 아닐 때만 기록을 풉니다 |

<a id="recovery-fence"></a>

### Claim과 fence

| 시나리오 | I/O | 참여 | 통과 조건 |
| --- | --- | --- | --- |
| foreign-connection | 둘 다 | `off`, `required` | gateway가 Runner를 제어하는 동안 같은 사용자의 다른 프로세스가 Runner의 Hello를 읽습니다. 그 Claim은 controller가 연결되어 있어 거부되고, controller의 epoch든 다른 epoch든 프로세스를 바꾸려는 요청은 모두 stale로 거부되며, Detach는 아무것도 바꾸지 않습니다. gateway는 그 뒤에도 Runner와 그 프로세스를 계속 제어합니다 |
| stale-epoch | | `off` | 두 gateway가 차례로 detach한 뒤, 다른 작업 데이터베이스를 위한 Claim은 거부됩니다. gateway가 Runner를 제어하지 않는 동안 같은 사용자의 프로세스라면 누구나 할 수 있는, Runner 자신의 데이터베이스를 위한 Claim([노출](runner-isolation.md#claim-exposure), 결정 D6이 받아들임)은 다음 epoch로 허용되고 그 PID와 함께 기록되며, 그 연결이 이전 epoch나 epoch 없이 보낸 요청은 stale로 거부됩니다. 다음 gateway는 그다음 epoch로 Runner를 Claim해 같은 프로세스를 찾고, 그 출력을 있던 곳부터 이어 읽습니다 |
| stopped-gateway | | `off` | SIGSTOP으로 멈춘 controller gateway는 연결을 유지하므로 아무도 밀어내지 못합니다. 다른 프로세스의 Claim은 거부되고, 둘째 gateway는 데이터베이스 writer 때문에 시작하지 않습니다. 멈춘 gateway를 죽이면 다음 gateway가 epoch 2로 Runner를 Claim해 프로세스가 실행 중임을 확인합니다 |
| concurrent-claims | | `off` | gateway가 Runner를 제어하지 않는 동안 연결 여덟 개가 Runner 자신의 데이터베이스로 한꺼번에 Claim하면, 정확히 하나만 허용되고 나머지는 controller가 연결되어 있어 거부됩니다. 그 연결이 detach하면 다음 gateway가 그다음 epoch로 Runner를 Claim합니다 |
| other-database | | `off` | Runner의 첫 Claim이 밝힌 것과 다른 작업 데이터베이스를 쓰는 gateway는, gateway가 Runner를 제어하든 아니든 거부되고 아무것도 시작하지 않습니다 |
| concurrent-gateways | | `off` | Runner의 데이터베이스로 한꺼번에 시작한 gateway 둘 가운데 하나가 요청을 받으며 Runner를 Claim하고, 다른 하나는 writer를 기다린 뒤 시작하지 않으며 Runner에 닿지 않습니다 |

<a id="recovery-reconcile"></a>

### 대조

| 시나리오 | I/O | 참여 | 통과 조건 |
| --- | --- | --- | --- |
| running | 둘 다 | `off`, `required` | 프로세스가 실행 중일 때 gateway를 죽이면, 다음 gateway는 그 workspace를 대조할 때까지 새 작업을 `RECOVERY_PENDING`으로 거부하고, 그 뒤 그 프로세스를 위해 workspace를 다시 점유합니다(`WORKSPACE_BUSY`). 남은 작업이 없던 workspace는 곧바로 새 작업을 받습니다. 물려받은 프로세스를 종료하면 그 workspace가 풀리며, 거부된 명령은 실행되지 않았습니다 |
| terminal | 둘 다 | `off`, `required` | gateway가 하나도 실행되지 않는 동안 프로세스가 끝나면, 다음 gateway는 끝난 방식을 Runner에서 알아내 기록하고 workspace를 풉니다 |
| approval | | `off`, `required` | gateway를 죽이기 전에 잡아 두고 허가한 승인은 다음 gateway가 한 번 재개하며, 다시 재개하면 같은 프로세스를 돌려줍니다. gateway를 죽일 때 재개가 진행 중이던 승인은(재개를 보낸 뒤 0에서 160 ms 사이의 여러 시점) 다음 gateway가 Runner가 그 승인을 위해 만든 프로세스로 재개하거나, Runner가 아무것도 만들지 않았음이 증명되면 그때 시작합니다. 각 명령은 정확히 한 번 실행되었습니다 |
| authority-outage | | `required` | fixture authority가 lock을 잡고 있는 동안 죽인 gateway를 바꿉니다. 물려받은 관리 대상 프로세스는 곧바로 답하고, Runner가 그 attempt를 authority에 물을 수 없는 동안 그 workspace는 `RECOVERY_PENDING`으로 거부되며, 관리하지 않는 작업은 실행됩니다. authority가 답하면 gateway가 이를 대조해 각 workspace를 그 프로세스를 위해 다시 점유하고, 그 프로세스를 종료하면 attempt가 해제됩니다 |
| missing-record | | `off` | gateway가 실행되지 않는 동안 데이터베이스에서 기록 하나를 지우면, 다음 gateway는 Runner가 가진 그 프로세스를 여전히 조회할 수 있고, 그 workspace는 무엇을 시작하는 대신 운영자를 위해 `RECOVERY_PENDING`으로 거부하며, 다른 workspace는 새 작업을 받습니다 |
| kill-during-exec | 둘 다 | `off`, `required` | `exec_command`가 진행 중일 때 0에서 160 ms 사이의 여러 시점에 gateway를 죽이면, 다음 gateway가 각각 남긴 것을 대조하며, 어떤 명령도 두 번 이상 실행되지 않았습니다 |
| kill-during-terminate | 둘 다 | `off`, `required` | `terminate_process`가 진행 중일 때 같은 시점들에 gateway를 죽이면, 다음 gateway는 프로세스를 종료됨으로 보고하거나, 아직 실행 중으로 보고한 뒤 종료합니다. 각 프로세스는 한 번 실행되고 한 번 종료로 끝나며, 그 뒤 그 workspace는 새 작업을 받습니다 |

<a id="recovery-observation"></a>

### 관찰과 Runner 손실

| 시나리오 | I/O | 참여 | 통과 조건 |
| --- | --- | --- | --- |
| ring-overflow | 둘 다 | `off` | gateway가 하나도 실행되지 않는 동안 프로세스가 Runner의 256 KiB 출력 ring보다 많이 씁니다. 다음 gateway는 마지막 읽기가 멈춘 곳부터 이어 읽고, 돌려주는 출력 앞의 출력을 잃었다는 것과 그 이유(`ring_overflow`)를 알리며, 가장 최근 출력은 온전히 돌려줍니다. 상태도 이를 알립니다 |
| retention | | `off` | Runner가 보관하는 것보다 많은 프로세스가 끝나면(64개, 그리고 핸들을 놓은 64개 더), 가장 오래된 것에 대해 `process_status`와 `read_process`는 기록으로 답합니다. 출력 없이 끝난 방식과 `retention_expired`로 답하며, 다음 gateway에서도 같습니다 |
| runner-loss | 둘 다 | `off`, `required` | Runner를 SIGKILL로 죽이면 그 gateway는 기록으로 답합니다. 실행 중이던 프로세스는 `unknown`, 끝난 프로세스는 끝난 그대로, 둘 다 `runner_lost`와 함께이며, 새 작업은 `RUNNER_DISCONNECTED`로 거부합니다. 출력하지 않는 pipe 프로세스는 죽은 Runner보다 오래 삽니다. `records list`는 그 기록을 보여 주고, `records release`는 gateway가 실행 중인 동안과 그 프로세스가 실행 중인 동안 그 기록을 거부합니다. 운영자가 프로세스를 끝낸 뒤(authority가 있으면 DevGuard가 attempt를 해제하고 잃은 Runner의 인스턴스를 retire한 뒤) 기록이 풀리고, 새 Runner와 gateway가 그 workspace에서 새 작업을 받습니다. PTY의 root 프로세스는 기록에 적지 않으므로, 운영자가 풀기 전에 끝냅니다 |
| runner-replaced | | `off` | 첫 Runner를 죽인 뒤 다른 Runner가 그 소켓에서 답하면, gateway는 그것을 옛 Runner로 받아들이지 않고, 옛 Runner의 프로세스를 `runner_lost`와 함께 `unknown`으로 보고하며 새 작업을 거부합니다. 다음 gateway는 운영자가 그 기록을 풀 때까지 그 workspace를 계속 거부하고, 다른 곳에서는 새 작업을 받습니다 |

<a id="recovery-gaps"></a>

### 공백

다음은 실행할 때마다 이유와 함께 실행하지 않은 시나리오로 기록하거나 여기에 적으며, 어느 것도 통과로 세지 않습니다.

| 공백 | 이유와, 대신 그 동작을 보여 주는 것 |
| --- | --- |
| 호스트 재시작 | 실행 중에 만들 수 없습니다. CodeSpace 시험은 기록에 다른 boot ID를 주어([`crates/server/src/mcp.rs`](../../crates/server/src/mcp.rs)) 재시작이 `host_rebooted`로 보고됨을 보여 주며, 실제 재시작은 qualification하지 않은 채로 남습니다 |
| Runner 자신이 재시작한 뒤 프로세스 복원 | 지원하지 않으며 P1-RECOVERY 범위 밖입니다. `runner-replaced`가 거부를 보여 줍니다 |
| gateway가 Runner를 제어하지 않는 동안 다른 프로세스의 Claim | Runner 사용자의 프로세스라면 설계상 가능합니다(결정 D6이 이 노출을 받아들임). `stale-epoch`는 이를 막는 것이 아니라 기록하고 fence함을 보여 줍니다 |
| gateway가 읽지 않는 동안 256 KiB ring을 넘는 출력 | 설계상 잃습니다. `ring-overflow`와 측정은 손실이 조용히 일어나지 않고 늘 알려짐을 보여 줍니다 |
| Runner를 잃은 뒤의 PTY 프로세스 | 기록에 root를 적지 않으므로 `records release`는 그것이 실행 중인지 알 수 없습니다. 운영자가 먼저 끝냅니다 |
| runner 통신에서 준비나 launch 메시지의 재전송 | CS-RG처럼 주입하지 않습니다. epoch fence가 이전 controller의 메시지를 거부합니다(`foreign-connection`, `stale-epoch`) |
| Linux `required` | DG-LINUX 전까지 지원하지 않습니다 |

<a id="recovery-measurement"></a>

## gateway 재시작 측정

이 측정은 관리 대상 pipe와 PTY 프로세스가 구간 내내 실행되는 동안, 유휴 상태와 DG-1의 개발 부하 아래에서, 설치된 서비스를 대상으로 gateway를 거듭 재시작하고, 재시작할 때마다 [완료 증거](#recovery-completion)가 요구하는 것을 확인합니다. macOS에서 `required`로 관리 대상으로 실행하며, 그 밖의 곳에서는 같은 실행이 `independent-off`라는 이름의 하네스 자체 시험이 되고 qualification을 통과하지 않습니다.

<a id="recovery-intervals"></a>

### 구간, 주기, 표본

한 번의 반복은 10분 유휴 구간과 그 뒤 30분 이상의 부하 구간이고, 절차는 세 번 반복합니다. 구간마다 새 Runner를 시작하며, 이 Runner는 `codespace` 소비자의 유일한 인스턴스로 등록합니다(결정 D12). 첫 gateway는 구간 내내 실행되는 프로세스를 시작합니다. 각각 25 mCPU, 16 MiB, 2 tasks를 요청하는 관리 대상 workspace의 `cat`(pipe)과 `sleep`(PTY), 그리고 관리하지 않는 workspace에서 0.2초마다 번호 붙은 줄을 쓰는 PTY와, 하네스가 신호를 줄 때마다 출력 ring이 담는 것보다 많은 번호 붙은 줄 40,000개를 한꺼번에 쓰는 pipe입니다. 요청을 받는 gateway가 무엇이든 그것을 통해 `control` 스위트의 일정대로 제어 경로를 표본으로 잽니다([구간과 표본](cs-rg-qualification.md#control-intervals)). status는 1초마다, 새 관리 대상 종료는 20초마다, I/O echo는 5초마다, 관리 대상 거부는 30초마다, 그리고 host, service, footprint(Runner와 gateway의 것)입니다. 두 출력 흐름은 2초마다 읽습니다. 요청을 받는 gateway가 없을 때 돌아온 일정은 표본이 아닌 `restarting`으로 기록하며, 기대하는 표본 수는 gateway가 요청을 받은 시간을 따릅니다.

구간은 gateway 주기의 연속입니다. 첫 주기는 20초이고, 그 뒤의 gateway는 모두 프로세스 timeout 20초로 실행을 시작하며, 그 주기의 길이는 그 주기의 timeout 대상이 정합니다. 각 주기에서:

1. 요청을 받는 gateway는 그 주기의 epoch로 Runner를 Claim했고, 둘째 주기부터는 오래 실행되는 프로세스가 모두 `running`임을 확인합니다.
2. 같은 사용자의 다른 프로세스가 소켓에 연결합니다. 그 Claim은 거부되고, 쓰기, 크기 변경, 프로세스 종료, workspace의 프로세스 종료, Runner 중지 요청을 이전 epoch, 현재 epoch, epoch 없이 보내면 각각 `STALE_CONTROLLER`로 거부됩니다.
3. 같은 작업 데이터베이스로 경쟁 gateway를 시작합니다. 경쟁 gateway는 writer를 기다린 뒤 포기해야 하며 Runner에 닿지 않습니다. 주기가 끝날 때까지 기다리고 있으면 죽입니다.
4. 재시작 전에 허가한 승인은 gateway가 그 workspace를 대조한 뒤 재개하고, 다시 재개해 같은 프로세스를 돌려받아야 합니다. 그 명령은 한 번 실행됩니다.
5. deadline이 지난 timeout 대상마다, 그 프로세스가 처음 사라진 것으로 보인 시각을 시작할 때 기록한 deadline과 비교하고, 요청을 받는 gateway가 어떻게 보고하는지 판단합니다.
6. 구간이 끝나지 않으면 gateway는 그 주기의 timeout 대상(관리 대상이거나 아닌, pipe이거나 PTY인 `sleep`을 번갈아)을 시작하고, 재시작 2초 전에 승인을 잡아 두고 허가한 뒤 끝납니다. 입력 닫기, SIGTERM, SIGUSR1, SIGKILL을 번갈아 쓰며, 모두 종료 의도 `detach`입니다. SIGKILL 전에는 승인의 재개를 보내 0에서 160 ms 동안 진행 중으로 둡니다. 네 주기씩 묶어, gateway가 대상의 deadline 2초 전에 끝나고 다음 gateway가 4초 뒤에 시작해 gateway가 없는 동안 deadline이 오게 하거나, deadline 3초 뒤에 끝나고 다음 gateway가 곧바로 시작합니다. 넷째 주기와 그 뒤 대략 아홉 주기마다, gateway가 읽지 않는 동안 pipe가 한꺼번에 씁니다.

구간이 끝나면 마지막 gateway가 두 흐름을 한 번 더 읽고, 마지막 승인을 재개하고, 오래 실행되는 프로세스를 모두 종료하며(각 종료는 효과가 있어야 함), 입력을 닫아 끝납니다. 그 뒤 운영자가 하듯 Runner를 drain합니다([도입과 되돌리기](operations.md#independent-runner-adoption)). 종료 의도 `stop`인 gateway가 다음 epoch로 Runner를 Claim해 중지하면, Runner는 종료 상태 0으로 끝나고 소켓을 지우며, 하네스가 프로세스를 먼저 끝냈으므로 종료하는 것이 없습니다. 서비스가 있으면 그 인스턴스가 retire되고 소비자의 자원은 아무것도 남지 않습니다. 부하는 `control` 스위트의 것이고([부하](cs-rg-qualification.md#control-load)) 기본은 cold입니다. 실행하는 동안 화면과 시스템이 잠들지 않게 합니다.

<a id="recovery-measurement-verdicts"></a>

### 판정

구간은 다음과 같으면 잘못된 관찰인 **판정 불가**입니다. Runner가 소비자의 인스턴스로 등록하지 않았거나, 오래 실행되는 프로세스를 시작하지 못했거나, 시작할 때 다른 작업이 자원을 차지하고 있었거나 `codespace` 인스턴스가 활성이었거나, journal을 읽을 수 없었거나, 원시 증거를 쓰지 못했거나, 서비스를 관찰할 수 없었거나 다른 release가 선택되었거나, 부하가 한도 안에 끝나지 않은 경우입니다.

그렇지 않으면, gateway가 요청을 받은 시간에 대해 `control` 스위트의 [규칙](cs-rg-qualification.md#control-verdicts)으로 제어 경로가 실패하거나, 다음 중 하나일 때 **실패**입니다.

- gateway가 요청한 것과 다르게 끝났거나, gateway가 다음 epoch가 아닌 epoch로 Runner를 Claim했거나, 오래 실행되는 프로세스가 다음 gateway에게 `running`이 아님
- 다른 프로세스의 요청이 무엇인가를 바꿨거나(stale mutation), stale이 아닌 다른 이유로 거부되었거나, 그 Claim이 허용됨
- 경쟁 gateway가 Runner에 닿았거나, 요청을 받았거나, writer가 아닌 다른 이유로 끝남
- timeout 대상이 원래 deadline보다 2초 넘게 더 살았거나 기록된 deadline이 바뀜(연장), deadline보다 0.25초 넘게 먼저 끝남, 또는 `timeout`으로 보고되지 않음
- 승인이 재개되지 않았거나, 두 번째 재개가 다른 프로세스를 돌려줬거나, 그 명령이 두 번 실행되었거나 한 번도 실행되지 않음
- 태그를 붙인 명령이 두 번 실행되었거나, 거부된 명령이 실행됨
- Runner가 공백을 알리지 않은 곳에서 출력 흐름이 줄을 건너뛰거나 되풀이함
- drain이 실패했거나, 마지막 gateway가 입력을 닫았을 때 종료 상태 0으로 끝나지 않음
- 주기가 오류로 중단됨

관찰이 너무 적어도 **판정 불가**입니다. 25초마다 한 번꼴로 계획한 재시작의 80%보다 적게 재시작했거나, gateway가 없는 동안 온 deadline이 없거나, 재개가 진행 중인 SIGKILL이 없거나, 어떤 출력 흐름에서 읽은 것이 없거나, 끝을 관찰하지 못한 timeout 대상이 있거나, `control` 스위트의 관찰 부족이 있거나, 부하가 DG-1의 것이 아닌 경우입니다. 그 밖에는 **통과**입니다.

반복, 조합, 실행 전체는 `control`처럼 판정합니다. 조합은 세 반복이 모두 통과하면 qualification을 통과하고, 실행 전체는 전체 절차(관리 대상으로, 10분과 30분을 세 번)에서만 qualification을 통과합니다. `--rehearsal`(유휴 2분, 부하 3분, 한 번 반복)은 실행을 시험해 보는 것이며 qualification을 통과하지 않습니다. `recovery` 스위트는 기능 시나리오가 통과하고 측정이 qualification을 통과했을 때만 qualification을 통과합니다.

<a id="recovery-completion"></a>

## 완료 증거

| P1-RECOVERY가 보여야 하는 것 | 보여 주는 곳 |
| --- | --- |
| 재시작 뒤 같은 실행에 다시 닿음 | 모든 gateway를 지나는 주기마다의 `running` 상태와 출력 흐름. 생명주기 `exit-kinds`, fence `stale-epoch` |
| 두 번 실행된 실행 없음 | 측정의 승인과 태그를 붙인 명령, 서비스가 있으면 gateway들이 실행마다 기록한 launch. 대조 `approval`, `kill-during-exec`, `kill-during-terminate` |
| stale mutation 없음 | 주기마다의 fence 확인과 경쟁 gateway. fence 스위트 |
| timeout 연장 없음 | 측정의 timeout 대상. 생명주기 `original-timeout` |
| 명확한 공백 | 실행마다 기록하는 [공백](#recovery-gaps) |
| 기존 모드가 계속 통과 | [회귀](#recovery-regressions) |
| 지원 모드 | [표](#recovery-supported-modes). Linux `off`는 CI 기능 확인일 뿐 |

<a id="recovery-run"></a>

## qualification 실행

이 하네스가 들어간 병합된 main으로, qualification 호스트에서 다음 순서로 합니다.

1. **고정하기 전에** Mac checkout을 `--prune`으로 fetch해 main으로 fast-forward하고, 메모리 pressure가 정상인지, 설치된 DevGuard release와 정책이 CS-RG가 qualification한 그대로인지 확인합니다.
2. **후보를 고정합니다.** `candidate`가 통과해야 합니다.
3. **`recovery`를 실행합니다.** 두 참여 모두로, 기능 스위트를 실행한 뒤 측정을 세 번 반복합니다.
4. **drain합니다.** 구간마다 그 Runner를 drain하며, 그 뒤 DevGuard에 활성 `codespace` 인스턴스도 차지한 자원도 없어야 합니다. 소비자는 인스턴스 하나만 허용하므로 살아 있는 Runner가 있으면 CS-RG 스위트의 gateway가 등록하지 못합니다.
5. **CS-RG 스위트** `candidate`, `matrix`, `faults`, `authority`를 같은 후보로 실행합니다.
6. **확인합니다.** 봉인한 디렉터리마다 `verify`를 실행합니다.

<a id="recovery-regressions"></a>

## 회귀

5단계의 CS-RG 스위트는 qualification을 마친 모드가 그 후보에서도 qualification대로 동작함을 보여 주며, 그중 하나라도 실패하면 qualification은 실패합니다. 후보 commit의 CI가 독립 Runner 시험을 포함한 Linux와 macOS의 회귀 단계를 보여 줍니다.

<a id="recovery-evidence"></a>

## 증거

모든 스위트는 CS-RG처럼 새 디렉터리를 만들어 쓰고 봉인합니다([증거](cs-rg-qualification.md#evidence)). recovery 디렉터리에는 Runner마다 argv와 로그, gateway마다 argv, 로그, MCP 대화 기록이 들어가고, 측정이면 구간마다 원시 행이 들어갑니다. 표본, 모든 주기(`restarts.jsonl`), timeout 대상, 승인, 태그를 붙인 모든 명령(`execs.jsonl`), 출력 읽기, 그리고 각 workspace의 명령이 실행마다 한 줄씩 쓴 marker 파일입니다. 어떤 디렉터리에도 credential, credential digest, 자식 프로세스의 환경은 들어가지 않습니다.

`qualify-devguard.py verify <디렉터리>`는 manifest를 확인하고 원시 파일에서 모든 판정을 다시 계산합니다. 시나리오 판정은 그 확인 항목에서, recovery 구간의 지표와 판정은 그 행에서, 그다음 반복, 조합, 실행 전체 순서로 계산합니다. 아무것도 쓰지 않습니다.
