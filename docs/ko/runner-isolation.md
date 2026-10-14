<a id="러너-격리"></a>

# 러너 격리

[English](../runner-isolation.md) | [한국어](runner-isolation.md)

실행을 담당하는 프로세스, 명령에 적용하는 샌드박스, 컨테이너로 실행을 옮기는 기능은 서로 다릅니다. 현재 앞의 두 가지는 구현되어 있으며 컨테이너 실행은 아직 연결되지 않았습니다.

<a id="이후-프로세스-분리"></a>
<a id="이후-프로세스-분리"></a>

## 호스트와 worker 실행

`in-process`는 `codespace-mcp` 안에서 프로세스를 관리합니다. `uds`(Unix domain socket, Unix 도메인 소켓)는 같은 호스트의 `codespace-codex-runtime` 안에서 같은 관리 코드를 실행합니다. worker는 시작 시 Codex의 프로세스 보호 설정을 적용하고 전용 Unix 소켓을 엽니다. worker 자체를 보호하는 것과 실행할 명령에 샌드박스를 적용하는 것은 별개입니다.

게이트웨이는 임시 디렉터리 또는 `CODESPACE_RUNNER_DIR` 아래에 권한 0700의 고유 디렉터리를 만듭니다. 내부 통신은 u32 길이 접두부, 버전 9 핸드셰이크, 요청 ID, 프로세스 종료 이벤트를 사용하는 CodeSpace JSON입니다. Codex App Server RPC가 아닙니다. 같은 연결에서의 요청 재처리는 재접속 복구를 뜻하지 않습니다.

관리 프로세스의 수명은 MCP 연결이 아니라 러너 인스턴스가 소유합니다. Streamable HTTP나 MCP 클라이언트 끊김은 프로세스를 유지합니다. 게이트웨이와 worker의 UDS 연결이 끊기거나 게이트웨이가 종료되면(stdio EOF 포함) 관리 중인 worker와 자식 프로세스가 종료되고 핸들이 사라집니다. 종료할 때 게이트웨이는 worker와의 연결을 닫으므로 worker는 종료하면서 자기 프로세스를 끝내고, 5초 뒤에도 실행 중인 worker는 강제로 종료됩니다. 재시작은 `process_id`를 복구하지 않습니다. [독립 Runner](#independent-runner)는 예외로, 그 프로세스는 게이트웨이보다 오래 삽니다.

**자식 프로세스를 시작하는 방식.** 파이프 명령, `tty: true` 명령, 패치 도우미, Linux 샌드박스 도우미, worker는 fork와 exec로 시작합니다. 모두 exec 전에 자식 프로세스 안에서 실행할 단계가 있기 때문입니다. 파이프, 패치 도우미, 샌드박스 도우미, worker 자식은 표준 디스크립터만 남기고([#79](https://github.com/novelKR/CodeSpace/issues/79)), PTY 자식은 새 세션의 리더가 됩니다. macOS에서는 같은 프로세스의 다른 스레드가 시스템 알림 라이브러리(libnotify)를 초기화하는 도중에 fork한 자식이 명령을 실행하기도 전에 `fork` 안에서 신호를 받아 종료됩니다. `process_status`는 이렇게 종료된 파이프 명령을 `signaled`로, `tty: true` 명령을 종료 코드 1로 끝난 것으로 보고합니다([Agent Loop 연동](agent-integration.md)). 그래서 `codespace-mcp`와 `codespace-codex-runtime`은 스레드를 시작하기 전에 이 초기화를 마치고, 위의 spawn은 모두 fork하기 전에 초기화를 마칩니다. 이 조치는 이 원인 하나를 없앨 뿐이며, 다중 스레드 프로세스에서 fork하는 것을 일반적으로 안전하게 만들지는 않습니다.

<a id="independent-runner"></a>

## 독립 Runner

> **아직 지원하는 모드가 아닙니다.** 독립 Runner와 그 실행의 기록은 게이트웨이 복구의 첫 단계입니다. 게이트웨이는 독립 Runner에 다시 연결하지 않으며, 다른 게이트웨이 뒤에 연결한 게이트웨이는 새 작업을 시작할 수 없습니다. 재연결과 상태 대조는 다음 단계에서 더하며, 이 모드는 qualification을 마친 뒤에야 지원합니다.

독립 Runner는 자신에게 연결하는 게이트웨이보다 오래 삽니다. 게이트웨이가 종료되거나 재시작되거나 강제로 종료되어도 프로세스와 그 출력, timeout은 계속됩니다. 운영자나 하네스가 독립 Runner를 시작하고, 게이트웨이는 연결만 합니다.

```bash
codespace-codex-runtime "$RUNNER_DIR/runner.sock" --lifecycle independent
codespace-mcp --runner uds --runner-socket "$RUNNER_DIR/runner.sock" \
  --runner-lifecycle independent --runner-exit-intent detach \
  --operations-db "$STATE_DIR/operations.sqlite"
```

Runner는 소켓 디렉터리를 권한 0700으로 직접 만듭니다. 이미 있는 디렉터리는 같은 사용자의 디렉터리이고 권한이 0700이며 심볼릭 링크가 아닐 때만 받아들입니다. 이미 있는 디렉터리를 바꾸지 않으며 `/`, `/tmp`, `/var/tmp`, `$HOME`은 거부합니다. 게이트웨이 연결은 한 번에 하나씩 받고, 앞 연결의 요청이 모두 끝난 뒤에 다음 연결을 받습니다. 중지할 때는 소켓을 지우고, 디렉터리도 직접 만든 것이면 지웁니다.

게이트웨이의 `--runner-lifecycle`이 Runner의 수명 모드와 다르거나, Runner가 자기 식별 정보를 알리지 않거나, `--runner-lifecycle independent`에 `--runner uds`, `--runner-socket`, `--runner-exit-intent`, `--operations-db` 중 하나가 없거나, `--runtime-bin`이나 `--runner-dir`가 함께 있으면 게이트웨이는 시작하지 않습니다. `--runner-exit-intent`는 `--runner-lifecycle independent`와 함께일 때만 받습니다.

| 사건 | 관리 worker(기본값) | 독립 Runner |
| --- | --- | --- |
| 게이트웨이 종료: stdin이 끝나거나 SIGTERM이나 SIGINT를 받음 | worker가 프로세스를 끝내고 종료합니다 | `--runner-exit-intent detach`면 게이트웨이가 detach하고 프로세스는 계속됩니다. `stop`이면 Runner가 프로세스를 끝내고 종료합니다 |
| 재시작하려고 게이트웨이에 SIGUSR1을 보냄 | 처리하지 않으므로 게이트웨이가 강제 종료된 경우와 같습니다 | 종료 의도와 관계없이 게이트웨이가 detach하고 프로세스는 계속됩니다 |
| 게이트웨이가 강제 종료되거나, detach나 stop 없이 연결이 끝남 | worker가 프로세스를 끝내고 종료합니다 | 중지가 아니라 연결 손실입니다. 프로세스는 계속되고 Runner는 다음 게이트웨이를 기다립니다 |
| Runner 호출이 120초 deadline을 넘김 | 게이트웨이가 연결을 닫고, worker는 프로세스를 끝내고 종료합니다 | 게이트웨이가 연결을 닫고, 프로세스는 계속됩니다 |
| 게이트웨이가 Runner에 중지를 요청하거나, Runner가 SIGTERM이나 SIGINT를 받음 | 관리 worker는 요청을 거부하며 이 신호를 처리하지 않습니다 | Runner가 프로세스를 끝내고 최대 5초 동안 기다린 뒤 종료합니다 |
| 프로세스가 timeout에 도달함 | 프로세스를 강제로 종료합니다 | 게이트웨이 연결 여부와 관계없이 원래 deadline에 프로세스를 강제로 종료합니다 |

프로세스를 끝낼 때 tty 프로세스는 그 프로세스 그룹에 신호를 보내지만, 파이프 프로세스는 그 프로세스에만 보냅니다. 파이프 프로세스에는 자기 프로세스 그룹이 없으므로, 그 프로세스가 시작한 프로세스는 중지할 때를 포함해 더 오래 남을 수 있습니다.

**연결을 잃었을 때.** 게이트웨이는 다시 연결하지 않습니다. Runner와의 연결이 끝나면 Runner가 필요한 요청을 모두 `RUNNER_DISCONNECTED`로 거부하며, Runner의 프로세스에 다시 닿으려면 게이트웨이를 재시작해야 합니다. 연결이 끝날 때 이미 보낸 요청은 지금처럼 불확실한 결과를 받습니다. 실행이라면 `dispatch_status: unknown`입니다.

**이전 게이트웨이가 있었을 때.** 새로 시작한 Runner에 처음 연결한 게이트웨이는 관리 worker와 같이 동작합니다. 한 게이트웨이가 그 Runner에서 작업(실행, 패치, DevGuard를 쓰면 준비나 launch)을 시작한 뒤에는, 다음 게이트웨이들이 Runner의 프로세스를 넘겨받습니다. 그 게이트웨이는 `process_id`로 프로세스를 조회하고, 출력을 읽고, stdin에 쓰고, 크기를 바꾸고, 종료할 수 있지만, 새 실행, 패치, 승인 재개는 `RECOVERY_PENDING`으로 거부하고 승인은 그대로 둡니다. 넘겨받은 프로세스가 모두 끝난 뒤에도 마찬가지이므로, 작업을 다시 시작하려면 Runner를 중지하고 새로 시작합니다. 게이트웨이는 Runner의 식별 정보(프로세스 ID, 부팅과 시작 시각), 이전에 다른 게이트웨이가 제어했는지, 넘겨받은 프로세스 수를 로그에 남깁니다.

<a id="process-records"></a>

**실행 기록.** 게이트웨이는 각 실행을 Runner에 보내기 전에 작업 데이터베이스에 기록하고, 실행이 끝날 때까지 따라갑니다. 그래서 메모리를 잃은 게이트웨이도 같은 실행을 다시 알아볼 수 있습니다. 기록에는 `process_id`, 실행을 보낸 Runner의 식별 정보(프로세스 ID, 부팅, 시작 시각, 임의의 nonce), workspace, 실행이 재개하는 승인, DevGuard attempt, DevGuard 참여 여부, tty 여부, argv의 SHA-256 digest가 들어가며, argv 자체와 그 환경, secret은 들어가지 않습니다. 기록의 단계는 Runner가 실행을 시작했다고 답할 때까지 `dispatching`이고, 그 뒤로는 `running`입니다. Runner가 실행이 끝났다고 알리면 Runner가 보고한 종료 방식, 종료 코드, 신호와 함께 `terminal`이 되고, Runner가 실행을 시작하지 않았다고 답할 때도 `terminal`이 됩니다. 기록에는 Runner가 정한 deadline, Runner가 만든 루트 프로세스, 게이트웨이가 출력을 어디까지 읽었는지도 남습니다. 루트 프로세스는 프로세스 ID와, 나중에 같은 ID를 받은 프로세스와 구별해 주는 시작 시각으로 남기며, `tty: true` 실행의 루트는 남기지 않습니다.

승인을 재개하는 실행은 그 승인을 resuming으로 바꾸는 transaction 안에서 기록합니다. 기록을 쓸 수 없으면 실행을 `INTERNAL`로 거부하고 아무것도 보내지 않습니다. 연결된 게이트웨이가 없는 동안 끝난 실행은 Runner가 보관했다가 다음 게이트웨이에 알리고, 그 게이트웨이가 기록합니다. `process_status`와 `read_process`도 Runner가 보고한 내용을 기록합니다. 기록이 핸들을 되살리는 일은 없으며, 기록만으로 실행이 시작되었는지나 어떻게 끝났는지가 증명되지도 않습니다. 기준은 Runner이며, 응답을 잃은 실행의 기록은 `dispatching`으로 남습니다. 기록으로 작업을 복구하는 일은 다음 단계에서 더합니다.

**writer는 하나.** 독립 Runner에 연결한 게이트웨이는 자기 작업 데이터베이스에 쓰는 유일한 게이트웨이입니다. 데이터베이스를 열기 전에 그 옆의 파일 `<database>.gateway.lock`에 배타 lock을 걸고, 어떤 식으로 종료하든 종료할 때까지 lock을 유지합니다. 이 파일에는 lock을 가진 게이트웨이의 프로세스 ID가 적힙니다. 앞 게이트웨이가 아직 detach하는 동안 새 게이트웨이를 시작했을 때처럼 lock이 이미 잡혀 있으면, 새 게이트웨이는 기다린다는 로그를 남기고 최대 15초 동안 기다린 뒤 시작을 거부합니다. 이 lock은 이 모드에서만 겁니다. 데이터베이스의 스키마 버전과 이전 CodeSpace 버전은 [운영](operations.md#operations-database)을 참고하세요.

**DevGuard.** `devguard` feature로 빌드한 독립 Runner는 worker의 DevGuard 설정으로 스스로 등록하며, 소비자 secret은 `--devguard-credential-file`로 지정한 비공개 파일에서 직접 읽습니다. `--devguard register`로 시작한 게이트웨이는 Runner에 secret을 넘기지 않습니다. 자원 참여를 요구하는 workspace는 Runner가 등록하고 launch helper로 실행한다고 알릴 때만 실행하며, 그렇지 않으면 `RESOURCE_POLICY_UNSUPPORTED`로 거부합니다. [DevGuard 결합](devguard-integration.md#resource-gateway-recovery)을 참고하세요.

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
