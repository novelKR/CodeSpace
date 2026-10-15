<a id="러너-격리"></a>

# 러너 격리

[English](../runner-isolation.md) | [한국어](runner-isolation.md)

실행을 담당하는 프로세스, 명령에 적용하는 샌드박스, 컨테이너로 실행을 옮기는 기능은 서로 다릅니다. 현재 앞의 두 가지는 구현되어 있으며 컨테이너 실행은 아직 연결되지 않았습니다.

<a id="이후-프로세스-분리"></a>
<a id="이후-프로세스-분리"></a>

## 호스트와 worker 실행

`in-process`는 `codespace-mcp` 안에서 프로세스를 관리합니다. `uds`(Unix domain socket, Unix 도메인 소켓)는 같은 호스트의 `codespace-codex-runtime` 안에서 같은 관리 코드를 실행합니다. worker는 시작 시 Codex의 프로세스 보호 설정을 적용하고 전용 Unix 소켓을 엽니다. worker 자체를 보호하는 것과 실행할 명령에 샌드박스를 적용하는 것은 별개입니다.

게이트웨이는 임시 디렉터리 또는 `CODESPACE_RUNNER_DIR` 아래에 권한 0700의 고유 디렉터리를 만듭니다. 내부 통신은 u32 길이 접두부, 버전 11 핸드셰이크, 요청 ID, 프로세스 종료 이벤트를 사용하는 CodeSpace JSON입니다. Codex App Server RPC가 아닙니다. 같은 연결에서의 요청 재처리는 재접속 복구를 뜻하지 않습니다.

관리 프로세스의 수명은 MCP 연결이 아니라 러너 인스턴스가 소유합니다. Streamable HTTP나 MCP 클라이언트 끊김은 프로세스를 유지합니다. 게이트웨이와 worker의 UDS 연결이 끊기거나 게이트웨이가 종료되면(stdio EOF 포함) 관리 중인 worker와 자식 프로세스가 종료되고 핸들이 사라집니다. 종료할 때 게이트웨이는 worker와의 연결을 닫으므로 worker는 종료하면서 자기 프로세스를 끝내고, 5초 뒤에도 실행 중인 worker는 강제로 종료됩니다. 재시작은 `process_id`를 복구하지 않습니다. [독립 Runner](#independent-runner)는 예외로, 그 프로세스는 게이트웨이보다 오래 삽니다.

**자식 프로세스를 시작하는 방식.** 파이프 명령, `tty: true` 명령, 패치 도우미, Linux 샌드박스 도우미, worker는 fork와 exec로 시작합니다. 모두 exec 전에 자식 프로세스 안에서 실행할 단계가 있기 때문입니다. 파이프, 패치 도우미, 샌드박스 도우미, worker 자식은 표준 디스크립터만 남기고([#79](https://github.com/novelKR/CodeSpace/issues/79)), PTY 자식은 새 세션의 리더가 됩니다. macOS에서는 같은 프로세스의 다른 스레드가 시스템 알림 라이브러리(libnotify)를 초기화하는 도중에 fork한 자식이 명령을 실행하기도 전에 `fork` 안에서 신호를 받아 종료됩니다. `process_status`는 이렇게 종료된 파이프 명령을 `signaled`로, `tty: true` 명령을 종료 코드 1로 끝난 것으로 보고합니다([Agent Loop 연동](agent-integration.md)). 그래서 `codespace-mcp`와 `codespace-codex-runtime`은 스레드를 시작하기 전에 이 초기화를 마치고, 위의 spawn은 모두 fork하기 전에 초기화를 마칩니다. 이 조치는 이 원인 하나를 없앨 뿐이며, 다중 스레드 프로세스에서 fork하는 것을 일반적으로 안전하게 만들지는 않습니다.

<a id="independent-runner"></a>

## 독립 Runner

> **아직 지원하는 모드가 아닙니다.** 독립 Runner와 그 실행의 기록, 한 게이트웨이에서 다음 게이트웨이로 제어권을 넘기는 Claim, 그리고 Claim마다 뒤따르는 대조는 게이트웨이 복구의 첫 단계입니다. 게이트웨이는 연결을 잃으면 Runner를 다시 Claim하고, 이전 controller가 작업을 시작한 곳에서는 그 controller가 남긴 것을 대조한 뒤에야 새 작업을 시작합니다. Runner 자체를 잃으면 게이트웨이는 그 실행들에 대해 기록으로 답하고, 운영자가 풀 때까지 그 workspace를 점유한 채로 둡니다([Runner 손실](#runner-loss)). 이 모드는 qualification을 마친 뒤에야 지원합니다.

독립 Runner는 자신에게 연결하는 게이트웨이보다 오래 삽니다. 게이트웨이가 종료되거나 재시작되거나 강제로 종료되어도 프로세스와 그 출력, timeout은 계속됩니다. 운영자나 하네스가 독립 Runner를 시작하고, 게이트웨이는 연결만 합니다.

```bash
codespace-codex-runtime "$RUNNER_DIR/runner.sock" --lifecycle independent
codespace-mcp --runner uds --runner-socket "$RUNNER_DIR/runner.sock" \
  --runner-lifecycle independent --runner-exit-intent detach \
  --operations-db "$STATE_DIR/operations.sqlite"
```

Runner는 소켓 디렉터리를 권한 0700으로 직접 만듭니다. 이미 있는 디렉터리는 같은 사용자의 디렉터리이고 권한이 0700이며 심볼릭 링크가 아닐 때만 받아들입니다. 이미 있는 디렉터리를 바꾸지 않으며 `/`, `/tmp`, `/var/tmp`, `$HOME`은 거부합니다. 연결은 여러 개를 동시에 받지만, 한 번에 한 연결만 Runner를 [제어](#claim)합니다. 중지할 때는 소켓을 지우고, 디렉터리도 직접 만든 것이면 지웁니다.

게이트웨이의 `--runner-lifecycle`이 Runner의 수명 모드와 다르거나, Runner가 자기 식별 정보를 알리지 않거나, `--runner-lifecycle independent`에 `--runner uds`, `--runner-socket`, `--runner-exit-intent`, `--operations-db` 중 하나가 없거나, `--runtime-bin`이나 `--runner-dir`가 함께 있으면 게이트웨이는 시작하지 않습니다. `--runner-exit-intent`는 `--runner-lifecycle independent`와 함께일 때만 받습니다.

| 사건 | 관리 worker(기본값) | 독립 Runner |
| --- | --- | --- |
| 게이트웨이 종료: stdin이 끝나거나 SIGTERM이나 SIGINT를 받음 | worker가 프로세스를 끝내고 종료합니다 | `--runner-exit-intent detach`면 게이트웨이가 detach하고 프로세스는 계속됩니다. `stop`이면 Runner가 프로세스를 끝내고 종료합니다 |
| 재시작하려고 게이트웨이에 SIGUSR1을 보냄 | 처리하지 않으므로 게이트웨이가 강제 종료된 경우와 같습니다 | 종료 의도와 관계없이 게이트웨이가 detach하고 프로세스는 계속됩니다 |
| 게이트웨이가 강제 종료되거나, detach나 stop 없이 연결이 끝남 | worker가 프로세스를 끝내고 종료합니다 | 중지가 아니라 연결 손실입니다. 프로세스는 계속되고 Runner는 다음 Claim을 기다립니다. 연결만 잃었다면 같은 게이트웨이가 다시 Claim합니다 |
| Runner 호출이 120초 deadline을 넘김 | 게이트웨이가 연결을 닫고, worker는 프로세스를 끝내고 종료합니다 | 게이트웨이가 연결을 닫고 Runner를 다시 Claim하며, 프로세스는 계속됩니다 |
| 게이트웨이가 Runner에 중지를 요청하거나, Runner가 SIGTERM이나 SIGINT를 받음 | 관리 worker는 요청을 거부하며 이 신호를 처리하지 않습니다 | Runner가 프로세스를 끝내고 최대 5초 동안 기다린 뒤 종료합니다 |
| 프로세스가 timeout에 도달함 | 프로세스를 강제로 종료합니다 | 게이트웨이 연결 여부와 관계없이 원래 deadline에 프로세스를 강제로 종료합니다 |

프로세스를 끝낼 때 tty 프로세스는 그 프로세스 그룹에 신호를 보내지만, 파이프 프로세스는 그 프로세스에만 보냅니다. 파이프 프로세스에는 자기 프로세스 그룹이 없으므로, 그 프로세스가 시작한 프로세스는 중지할 때를 포함해 더 오래 남을 수 있습니다.

<a id="claim"></a>

**Claim과 controller epoch.** 게이트웨이는 자기 작업 데이터베이스를 밝히는 `Claim`으로 Runner의 제어권을 얻습니다. Runner는 소켓의 peer credential로 확인한 연결 상대가 Runner와 같은 사용자로 실행 중이고, Runner가 다른 작업 데이터베이스를 위해 Claim된 적이 없을 때만 Claim을 받아들입니다. Runner는 처음 Claim한 데이터베이스의 식별자를 기억하고, 다른 데이터베이스를 쓰는 게이트웨이를 거부합니다. 이 식별자는 잘못된 데이터베이스로 시작한 게이트웨이를 막는 장치일 뿐 자격 증명이 아닙니다. 데이터베이스를 소유한 사용자는 이 식별자를 읽을 수 있기 때문입니다. 다른 연결이 Runner를 제어하고 있고 그 연결이 아직 열려 있으면, 그 게이트웨이가 멈춰 있더라도 Runner는 Claim을 거부합니다. 살아 있는 controller를 밀어내는 방법은 없습니다. 그 연결이 끝난 뒤에는 Claim이 그 연결이 보낸 요청을 준비와 launch까지 포함해 최대 10초 동안 기다립니다. 요청이 그보다 오래 걸리면 Claim은 거부되고 아무것도 바뀌지 않습니다.

Claim을 받아들일 때마다 Runner의 controller epoch가 하나 올라갑니다. epoch는 Runner가 살아 있는 동안 줄지 않으며, 새 Runner는 다시 1부터 시작합니다. Claim의 응답은 Runner의 프로세스를 각자의 workspace, 상태와 함께 나열하고, 이 목록에 반영된 마지막 종료 이벤트의 번호를 알립니다. Runner를 제어하지 않는 연결은 `Hello`, `Claim`, `Detach`에만 응답을 받고, 종료 이벤트는 제어하는 연결에만 갑니다. 프로세스나 workspace를 바꾸거나 Runner를 중지하는 요청은 그 요청을 보낸 Claim의 epoch를 함께 보냅니다. 실행, 패치, stdin 쓰기, 크기 변경, 프로세스나 workspace 프로세스의 종료, 중지, DevGuard를 쓰면 준비와 launch가 여기에 해당합니다. Runner는 이전 epoch로 온 이런 요청을 `STALE_CONTROLLER`로 거부하며, 그 요청이 요구한 일은 하나도 하지 않습니다. 게이트웨이는 각 실행의 기록에 epoch를 남기고, Runner는 받아들이거나 거부한 Claim마다 Claim한 상대의 프로세스 ID를 로그에 남깁니다.

다른 연결이 Runner를 제어하는 동안이나 그 연결의 요청이 처리되는 동안 시작한 게이트웨이는 간격을 늘려 가며 최대 30초 동안 다시 요청하고, 그래도 안 되면 시작을 거부합니다. 다른 작업 데이터베이스를 쓰는 경우처럼 다른 이유로 거부된 게이트웨이는 바로 시작을 거부하며, Runner는 그대로 남습니다.

**연결을 잃었을 때.** Runner와의 연결을 잃었거나, 호출이 deadline을 넘겨 연결을 닫은 게이트웨이는 다시 연결해 같은 Runner를 Claim합니다. 먼저 소켓에서 응답하는 Runner가 시작할 때 찾은 Runner와 같은 식별 정보를 가졌는지 확인합니다. 바로 한 번 시도하고, 실패하면 100밀리초를 기다린 뒤 다시 시도하며, 실패할 때마다 기다리는 시간을 두 배씩 최대 5초까지 늘립니다. 그동안에는 Runner가 필요한 요청을 모두 `RUNNER_RECONNECTING`으로 거부합니다. 그 요청을 위해서는 아무것도 보내거나 예약하거나 기록하지 않으며, 재개하려던 승인은 소비되지 않습니다. 소켓에서 더 이상 연결을 받지 않거나 다른 Runner가 응답하면 Runner를 [잃은](#runner-loss) 것입니다. 기다려도 풀리지 않는 이유로 Runner가 Claim을 거부할 때도 게이트웨이는 시도를 멈춥니다. 어느 쪽이든 그런 요청을 모두 `RUNNER_DISCONNECTED`로 거부하며, Runner의 프로세스에 다시 닿으려면 게이트웨이를 재시작해야 합니다. 연결을 잃을 때 이미 보낸 요청은 지금처럼 불확실한 결과를 받습니다. 실행이라면 `dispatch_status: unknown`입니다. Runner가 전하지 못한 종료 이벤트는 다음 controller에게 가고, 게이트웨이는 각 종료를 한 번만 전달합니다.

**이전 controller가 있었을 때.** 새로 시작한 Runner를 처음 Claim한 게이트웨이는 관리 worker와 같이 동작합니다. 한 controller가 어떤 workspace에서 작업(실행, 패치, DevGuard를 쓰면 준비나 launch)을 시작한 뒤에는, 그 뒤의 controller들이 Runner의 프로세스를 넘겨받습니다. 연결을 잃었다가 Runner를 다시 Claim한 같은 게이트웨이도 마찬가지이며, Claim의 응답은 앞선 controller가 작업을 시작한 workspace를 모두 알립니다. 게이트웨이는 넘겨받은 프로세스를 `process_id`로 바로 조회하고, 출력을 읽고, stdin에 쓰고, 크기를 바꾸고, 종료할 수 있으며, DevGuard가 응답하지 못할 때도 마찬가지입니다. Claim이 알린 workspace, Runner가 실행 중인 프로세스를 가진 workspace, [대조](#reconciliation)할 기록이 남은 workspace에서는 먼저 앞선 controller들이 남긴 것을 대조합니다. 그때까지는 그 workspace의 새 실행, 패치, 승인 재개를 승인을 가져가기 전에 `RECOVERY_PENDING`으로 거부하므로 승인은 그대로 남습니다. Runner도 Claim이 알린 workspace에서는 게이트웨이가 그 workspace를 대조했다고 알릴 때까지 이런 요청을 거부합니다. 다른 workspace는 바로 새 작업을 받습니다. 게이트웨이는 Runner의 식별 정보(프로세스 ID, 부팅과 시작 시각), Claim의 epoch, 앞선 controller가 작업을 시작한 workspace, 넘겨받은 프로세스 수, 그 목록에 반영된 마지막 종료 이벤트의 번호를 로그에 남깁니다.

<a id="reconciliation"></a>

**대조.** 게이트웨이는 Claim할 때마다, `terminal`이 아니면서 이 Runner에 이번 Claim이나 그 뒤의 Claim으로 보낸 것이 아닌 기록과, 아직 `resuming`인 승인의 기록을 대조합니다. 각 실행에 대해 세 가지를 맞춰 봅니다. 그 실행의 [기록](#process-records), 기록이 가리키는 Runner가 그 프로세스에 대해 가진 것이나 기억하는 것, 그리고 DevGuard를 쓰면 `required` workspace의 실행에 대해 Runner가 DevGuard로 조회한 attempt입니다. Runner는 시작하라고 받은 `process_id`마다, 프로세스를 만들었는지 아니면 요청을 받고도 아무것도 만들지 않았는지를 ID만으로 수명 내내 기억합니다. 요청을 받은 적이 없다는 증명은 Runner 자신에 대해서만 할 수 있습니다. 다른 Runner를 가리키면서 끝나지 않은 기록은 그 소켓에서 다른 Runner가 응답하므로 그 Runner를 [잃었다](#runner-loss)는 뜻이고, 운영자만 정리할 수 있습니다.

| 기록 | 기록이 가리키는 Runner | DevGuard attempt(`required`만) | 결과 |
| --- | --- | --- | --- |
| `dispatching` 또는 `running` | 프로세스를 실행 중으로 가짐 | launch됨 | 기록은 `running`이 되고, 프로세스가 다시 workspace를 점유합니다. 그 workspace의 새 작업은 프로세스가 끝나거나 종료될 때까지 `WORKSPACE_BUSY`를 받습니다 |
| `dispatching` 또는 `running` | 끝난 프로세스를 가짐, 또는 만들었지만 더는 갖지 않음 | scope가 끝나 해제됨, 또는 시작 전에 끝남 | 기록은 프로세스가 어떻게 끝났는지 남기며, Runner가 더는 갖지 않으면 종료 방식은 `unknown`입니다. workspace 점유는 해제됩니다 |
| `dispatching` | 요청을 받은 적 없음, 또는 받고 아무것도 만들지 않음 | 없음, 또는 시작 전에 끝남 | 기록은 실행이 시작되지 않았다고 남기고, workspace 점유는 해제됩니다 |
| `dispatching`, `running` 또는 `unknown` | 다른 Runner이며, 잃었음 | 무엇이든 | 기록은 이유와 함께 `unknown`이 되고, 운영자가 기록을 풀 때까지 workspace는 계속 거부됩니다 |
| 없음 | 프로세스를 실행 중으로 가짐 | 무엇이든 | workspace는 계속 거부됩니다 |
| 그 밖의 모든 경우 | | | workspace는 계속 거부됩니다 |

Runner나 DevGuard에 묻지 못했거나 DevGuard가 attempt를 아직 정리하지 않아 답을 기다리는 workspace는 500밀리초 뒤에 다시 대조하며, 다시 시도할 때마다 기다리는 시간을 두 배씩 최대 30초까지 늘립니다. 세 가지가 서로 맞지 않는 workspace는 운영자가 해결할 때까지, 잃은 Runner의 기록이 있는 workspace는 운영자가 그 기록을 풀 때까지 계속 거부됩니다([운영](operations.md#runner-loss)). Runner가 실행 중인 프로세스를 가졌지만 그 프로세스를 설명하는 기록이 없는 workspace는 그 프로세스가 끝나거나 종료될 때까지 계속 거부됩니다. 게이트웨이는 종료가 있을 때마다 다시 대조합니다. 계속 거부하는 workspace는 그 이유와 함께 로그에 남기고, 그 이유는 `RECOVERY_PENDING`에도 들어갑니다. 대조를 마친 workspace도 로그에 남깁니다. 대조할 수 없는 workspace의 점유는 풀지 않습니다.

앞선 controller가 `resuming`으로 남긴 승인은 Runner가 그 승인을 위해 만든 실행으로 consumed가 되며, 그 뒤의 `operation_resume`은 그 실행의 `process_id`를 `dispatch_status: confirmed`와 함께 돌려줍니다. Runner가 아무것도 만들지 않았음이 증명되면 승인은 다시 granted가 되어 한 번 더 재개할 수 있습니다. 그 밖의 경우에는 `resuming`으로 남고, `operation_resume`은 지금처럼 `APPROVAL_AMBIGUOUS`를 돌려줍니다. 실행이 시작되었을 수도 있는 승인은 다시 granted가 되지 않습니다.

**끝난 프로세스.** 독립 Runner는 끝난 프로세스와 그 상태, 출력을 controller가 그 기록에 프로세스가 어떻게 끝났는지 남겼다고 확인할 때까지 보관합니다. 그래서 어떤 게이트웨이도 받지 못한 종료도 다음 controller에게 갑니다. 프로세스의 핸들은 관리 worker와 같이 15분이 지나거나 끝난 프로세스가 64개를 넘으면 놓고, 핸들을 놓은 끝난 프로세스는 64개까지 보관하며 가장 오래된 것부터 버립니다. 버린 프로세스에 대해서는 Runner가 그 프로세스를 만들었다는 것만 기억합니다. Runner가 더는 갖지 않는 끝난 프로세스에 대해서는 `process_status`와 `read_process`가 그 기록으로 답하며, `lost_reason`은 `retention_expired`입니다.

<a id="runner-loss"></a>

**Runner 손실.** Runner와의 연결을 잃은 게이트웨이는 소켓에서 더 이상 연결을 받지 않거나, 그 소켓의 worker가 다른 식별 정보를 알리거나 아무 식별 정보도 알리지 않으면 Runner를 잃었다고 봅니다. Runner가 강제로 종료되었거나, 비정상 종료했거나, 중지되었고, 다른 Runner로 바뀌었을 수도 있습니다. 그때부터는 그 Runner에 보낸 실행이 어떻게 되었는지 알 수 없습니다. 강제로 종료된 Runner는 아무것도 끝내지 않으므로, 그 파이프 프로세스는 자신이 시작한 프로세스와 함께, 자신이 시작된 프로세스 그룹 안에서 계속 실행됩니다. 그 그룹은 Runner의 프로세스 그룹이거나, 네트워크를 허용한 샌드박스 도우미와 DevGuard의 launch helper처럼 root가 이끄는 그룹입니다. 그 뒤 출력에 쓰는 파이프 프로세스는 SIGPIPE를 받을 가능성이 크고, `tty: true` 프로세스는 Runner가 끝나며 터미널이 닫힐 때 SIGHUP을 받을 가능성이 크지만, 어느 쪽이든 이를 무시할 수 있으므로 끝났다고 보지 않습니다. Linux 명령 샌드박스 안에서 네트워크를 허용하지 않은 workspace의 명령은, 샌드박스가 그 프로세스를 Runner의 수명에 묶으므로 그 명령을 시작한 Runner와 함께 끝납니다. 이 경우의 시험이 이를 확인합니다. 네트워크를 허용하면 명령과 그 네트워크 프록시를 실행하는 샌드박스 도우미가 Runner에 묶이지 않으므로, 명령이 Runner보다 오래 살 수 있습니다. 게이트웨이는 손실과 그에 해당하는 프로세스를 로그에 남기고, Runner가 필요한 요청을 모두 `RUNNER_DISCONNECTED`로 거부합니다.

잃은 Runner에 보낸 것 중 끝나지 않은 기록은 모두 이유와 함께 `unknown`이 되고 workspace를 계속 점유합니다. Runner를 잃은 게이트웨이에서도, [대조](#reconciliation)하며 다른 Runner의 기록을 찾는 다음 게이트웨이에서도 그렇습니다. Runner가 호스트의 이전 부팅에서 실행된 기록은 호스트가 재시작해 실행이 끝났다고 남깁니다. Runner가 손실 전에 알린 종료는 그대로 그 기록을 끝냅니다. 이런 실행은 다시 시작하지 않으며, 다음 게이트웨이는 운영자가 그 실행이 하나도 남아 있지 않음을 확인하고 기록을 풀 때까지 그 workspace의 새 작업을 `RECOVERY_PENDING`으로 거부합니다([운영](operations.md#runner-loss)). 이 오류는 프로세스와 `codespace-mcp records`를 알려 줍니다. 이런 실행이 재개한 승인은 `resuming`으로 남고, 그 기록을 풀면 `operation_resume`은 그 승인에 대해 `APPROVAL_AMBIGUOUS`를 돌려줍니다. 기록의 deadline과 argv digest는 그대로입니다.

Runner가 더는 갖지 않는 실행, 또는 게이트웨이가 Runner와 함께 잃은 실행에 대해서는 `process_status`와 `read_process`가 기록으로 답합니다.

| 기록 | `state` | `lost_reason` |
| --- | --- | --- |
| 끝났고 Runner가 더는 갖지 않음 | 기록된 종료 방식, 종료 코드, 신호와 함께 `exited` | `retention_expired` |
| 끝났고 게이트웨이가 Runner를 잃음 | 기록대로 `exited` | `runner_lost` |
| 호스트 재시작 전에 끝남 | 기록대로 `exited` | `host_rebooted` |
| 끝나지 않았고, 이번 부팅에서 Runner를 잃음 | `unknown`: 프로세스가 아직 실행 중일 수 있으며, 종료가 아닙니다 | `runner_lost` |
| 끝나지 않았고, Runner가 이전 부팅에서 실행됨 | 종료 방식 `unknown`과 함께 `exited` | `host_rebooted` |

이런 답에는 출력이 없습니다. `read_process`는 빈 `chunk`와 함께 `eof`와 `output_lost`를 true로 돌려주고, `retained_from`과 cursor는 게이트웨이가 본 가장 먼 출력 위치에 둡니다. 요청한 cursor가 더 멀면 그 cursor에 둡니다. Runner가 프로세스는 갖고 있지만 출력의 일부만 갖고 있는 동안에는 `lost_reason`이 `ring_overflow`이고, `read_process`에서는 `output_lost`가, `process_status`에서는 0보다 큰 `output_retained_from`이 함께 옵니다. Runner 손실 뒤에는 출력을 되살리지 않습니다.

<a id="claim-exposure"></a>

**누가 제어권을 얻을 수 있는가.** Runner는 작업 데이터베이스를 밝히는 같은 사용자의 프로세스라면 무엇이든 믿습니다. 게이트웨이가 Runner를 제어하는 동안에는 다른 프로세스가 제어권을 얻을 수 없습니다. 그러나 게이트웨이가 제어하지 않는 동안, 곧 한 게이트웨이가 끝나고 다음 게이트웨이가 Claim하기 전이나 게이트웨이가 다시 연결하는 동안에는 소켓에 닿을 수 있는 같은 사용자의 프로세스라면 무엇이든 Runner를 Claim할 수 있습니다. 그러면 그 프로세스가 Runner의 프로세스와 그 입력, 출력, 종료를 갖게 되고, 앞선 controller가 작업을 시작한 적이 없다면 작업을 시작할 수도 있습니다. 게이트웨이는 그 연결이 끝난 뒤에야 제어권을 되찾으며, 그동안 계속 다시 요청합니다. 이런 프로세스는 게이트웨이가 샌드박스 없이 실행한 명령일 수 있습니다. macOS에서, 그리고 Linux에서 샌드박스 도우미가 없을 때가 그렇습니다. Linux 샌드박스가 실행한 명령은 소켓에 연결할 수 없습니다. 네트워크가 restricted이면 샌드박스가 연결을 막고, enabled이면 Unix 소켓 자체를 막기 때문입니다. 이런 프로세스를 막으려면 믿지 않는 다른 프로그램을 실행하지 않는 사용자로 게이트웨이와 Runner를 실행하고, Linux에서는 샌드박스 도우미를 두어 게이트웨이의 명령이 샌드박스 안에서 실행되게 합니다.

<a id="process-records"></a>

**실행 기록.** 게이트웨이는 각 실행을 Runner에 보내기 전에 작업 데이터베이스에 기록하고, 실행이 끝날 때까지 따라갑니다. 그래서 메모리를 잃은 게이트웨이도 같은 실행을 다시 알아볼 수 있습니다. 기록에는 `process_id`, 실행을 보낸 Runner의 식별 정보(프로세스 ID, 부팅, 시작 시각, 임의의 nonce), 실행을 보낸 [controller epoch](#claim), workspace, 실행이 재개하는 승인, DevGuard attempt, DevGuard 참여 여부, tty 여부, argv의 SHA-256 digest가 들어가며, argv 자체와 그 환경, secret은 들어가지 않습니다. 기록의 단계는 Runner가 실행을 시작했다고 답할 때까지 `dispatching`이고, 그 뒤로는 `running`입니다. Runner가 실행이 끝났다고 알리면 Runner가 보고한 종료 방식, 종료 코드, 신호와 함께 `terminal`이 되고, Runner가 실행을 시작하지 않았다고 답할 때도 `terminal`이 됩니다. 기록에는 Runner가 정한 deadline, Runner가 만든 루트 프로세스, 게이트웨이가 출력을 어디까지 읽었는지도 남습니다. 루트 프로세스는 프로세스 ID와, 나중에 같은 ID를 받은 프로세스와 구별해 주는 시작 시각으로 남기며, `tty: true` 실행의 루트는 남기지 않습니다.

승인을 재개하는 실행은 그 승인을 resuming으로 바꾸는 transaction 안에서 기록합니다. 기록을 쓸 수 없으면 실행을 `INTERNAL`로 거부하고 아무것도 보내지 않습니다. 기록에 실행이 어떻게 끝났는지 남기면 게이트웨이는 그것을 Runner에 확인해 줍니다. Runner를 제어하는 게이트웨이가 없는 동안 끝난 실행은 Runner가 보관했다가 다음 controller에 알리고, 그 게이트웨이가 늦어도 대조할 때 기록합니다. Runner를 중지하는 게이트웨이는 Runner의 프로세스가 각각 어떻게 끝났는지 기록합니다. `process_status`와 `read_process`도 Runner가 보고한 내용을 기록합니다. 기록이 핸들을 되살리는 일은 없으며, 기록만으로 실행이 시작되었는지나 어떻게 끝났는지가 증명되지도 않습니다. 기준은 Runner이며, 응답을 잃은 실행의 기록은 [대조](#reconciliation)가 정리할 때까지 `dispatching`으로 남습니다.

**writer는 하나.** 독립 Runner에 연결한 게이트웨이는 자기 작업 데이터베이스에 쓰는 유일한 게이트웨이입니다. 데이터베이스를 열기 전에 그 옆의 파일 `<database>.gateway.lock`에 배타 lock을 걸고, 어떤 식으로 종료하든 종료할 때까지 lock을 유지합니다. 이 파일에는 lock을 가진 게이트웨이의 프로세스 ID가 적힙니다. 앞 게이트웨이가 아직 detach하는 동안 새 게이트웨이를 시작했을 때처럼 lock이 이미 잡혀 있으면, 새 게이트웨이는 기다린다는 로그를 남기고 최대 15초 동안 기다린 뒤 시작을 거부합니다. 이 lock은 이 모드에서만 겁니다. 데이터베이스의 스키마 버전과 이전 CodeSpace 버전은 [운영](operations.md#operations-database)을 참고하세요.

**DevGuard.** `devguard` feature로 빌드한 독립 Runner는 worker의 DevGuard 설정으로 스스로 등록하며, 소비자 secret은 `--devguard-credential-file`로 지정한 비공개 파일에서 직접 읽습니다. `--devguard register`로 시작한 게이트웨이는 Runner에 secret을 넘기지 않습니다. 자원 참여를 요구하는 workspace는 Runner가 등록하고 launch helper로 실행한다고 알릴 때만 실행하며, 그렇지 않으면 `RESOURCE_POLICY_UNSUPPORTED`로 거부합니다. `devguard` feature 없이 빌드한 게이트웨이는 DevGuard에 참여한 실행을 대조할 수 없으므로 그 workspace는 계속 거부됩니다. [DevGuard 결합](devguard-integration.md#resource-gateway-recovery)을 참고하세요.

<a id="macos-docker-없음"></a>
<a id="macos-docker-없음"></a>

## Linux 명령 샌드박스

파이프와 PTY 실행 모두 같은 Linux 도우미 경로를 사용합니다. Linux에서 `CODESPACE_LINUX_SANDBOX_BIN` 또는 실행 파일 옆의 `codespace-linux-sandbox`를 검사하고, 성공하면 샌드박스 준비·실행을 사용합니다. 검사 결과는 해당 프로세스에서 캐시됩니다.

```text
Runner → 도우미 prepare (CodeSpace JSON, 프로토콜 1)
       ← 실행 계획 파일 경로
Runner → 관리 프로세스로 도우미 run --plan 실행
       → Codex 샌드박스 설정 → 요청한 명령
```

Codex 권한 변환과 샌드박스 인자는 실행 파일 전용 도우미 안에서 처리합니다. 실행 계획은 권한 0600의 비공개 파일이며 도우미가 읽어 실행에 사용합니다. Runner는 작은 프로토콜 crate에만 의존하며 샌드박스 구현 라이브러리를 직접 가져오지 않습니다. restricted 실행은 도우미 프로세스가 자신을 실행 명령으로 교체하며, enabled 실행은 도우미가 프록시를 유지하면서 샌드박스 자식 프로세스를 기다립니다.

최초 검사가 실패하면 `restricted`는 비격리 호스트 실행을 허용하며, 실행 정보에 `command_sandbox: none`과 OS 네트워크 집행 없음이 표시됩니다. `enabled`는 도우미가 없으면 실패합니다. 최초 검사가 성공한 뒤 준비·프로토콜·시작 오류가 발생하면 비격리 실행으로 대체하지 않고 `PROCESS_SPAWN_FAILED`를 반환합니다. 이미 시작된 도우미 내부의 실패는 `process_status`로 관측하는 프로세스 종료입니다. 그 wait 상태는 관리 자식(헬퍼 argv)의 코드이며 사용자 argv와 동일하다고 문서화하지 않습니다.

## 네트워크와 파일 접근 범위

restricted 모드는 네트워크 네임스페이스 분리와 seccomp 제한을 사용합니다. enabled 모드는 격리된 네트워크 네임스페이스와 관리 HTTP 프록시를 사용합니다. 프록시 우회 환경변수를 비워 루프백 HTTP도 프록시를 거치게 합니다. 호스트 네트워크에 직접 연결하는 방식으로 대체 실행하지 않습니다. 목적지 도메인별 제한이나 모든 네트워크 클라이언트의 호환성을 보장하는 기능은 아닙니다.

실제 조건은 `workspace_info.execution`에서 확인하세요. 파일 도구의 작업 공간 범위는 명령 샌드박스와 별도로 적용됩니다. 논리적 경로 검사 뒤에서 `codespace-fs`가 no-follow I/O를 수행합니다. 패치 도우미에는 별도의 경로 검사와 Codex 패치 옵션이 있으며, 명령 샌드박스가 모든 파일 도구를 자동으로 감싸지는 않습니다.

샌드박스 명령은 사용자 홈의 도구 체인을 마운트하지 않고 제한된 시스템 PATH를 사용합니다. 필요한 컴파일러, 패키지 캐시, 실행 파일을 실행 환경에 준비하세요. 호스트에서 동작하는 명령도 샌드박스 안에서는 실행 파일이나 의존성을 찾지 못할 수 있습니다.

<a id="compose-픽스처가-하는-일"></a>
<a id="compose-픽스처가-하는-일"></a>

## 컨테이너 실험 구성과 검증

`deploy/compose.yml`은 선택한 작업 공간만 `/workspace`에 마운트하고 일반 사용자로 대기하는 컨테이너입니다. 서버를 설치하거나 Runner를 시작하지 않으며 `exec_command` 요청을 받지 않습니다. 이 구성을 시작했다고 MCP 명령 샌드박스가 활성화되는 것은 아닙니다.

CI의 Linux 격리 작업은 bubblewrap을 설치하고 격리 테스트에서 도우미 사용 가능 여부를 필수로 확인합니다. macOS 검사는 호스트 동작을 대상으로 하며 Linux 정책 집행을 검증하지 않습니다. 커널 탈출, Docker Desktop 차이, 임의의 원격 배포 환경은 별도 평가가 필요합니다. 설치는 [운영](operations.md), 신뢰 조건은 [보안 모델](security-model.md)을 참고하세요.
