<a id="devguard-integration"></a>

# DevGuard 결합 로드맵

> **상태: 구현 지시로서 효력 중지.** 이 페이지의 ‘CS-RG 작업 순서’, ‘소비 경계’, ‘설계 개정 1 참조’ 절은 적힌 대로 구현하지 않습니다. 현재 상태와 선행 조건은 영향이 없습니다. CodeSpace 통합 계획에 대해 소유자가 지시한 검토인 CS-RG 통합 경계 재검증(작업 단위가 아닙니다)이 끝날 때까지 적용됩니다. 대체 구조는 승인되지 않았으며, 소유자가 재검증 결과를 검토한 뒤 결정합니다. 이 표기는 지시의 효력만 중지하며 어떤 안전 요구도 완화하지 않습니다. 아래 내용은 이력 추적을 위해 바꾸지 않고 남깁니다. 다만 현재 상태, 소유자가 2026-10-02에 이 효력 중지와 별도로 승인한 CSRG-U1의 [상태 전용 연결](#devguard-status-connection), 소유자가 2026-10-03에 지시한 CSRG-U2의 [실행 소유자 등록](#devguard-owner-registration)은 예외입니다.

[English](../devguard-integration.md) | [한국어](devguard-integration.md)

[DevGuard](https://github.com/novelKR/DevGuard)는 개발 작업의 자원을 중앙에서 관리하는 독립 시스템이며 CodeSpace와 동일한 Apache-2.0 라이선스를 적용한다. 승인된 결합 경로에 따라 실행 허용과 자원 회계를 공통 계층에 맡기고, CodeSpace는 프로세스 소유권, PTY, 입출력, 권한, 승인 hold와 workspace 조정을 계속 담당한다.

**현재 상태:** DevGuard는 독립 저장소에서 DG-1을 완료했다([`395315d`](https://github.com/novelKR/DevGuard/tree/395315d34b5d458ea1774446727f0cb14bd8a120), [마일스톤 원장](https://github.com/novelKR/DevGuard/blob/395315d34b5d458ea1774446727f0cb14bd8a120/milestones.json)). DG-0 계약 위에 macOS 실제 호스트 증거, 현재 사용자의 LaunchAgent로 설치하는 인증 daemon, fenced launch helper와 대조, Cargo adapter를 갖춘 `devguard` 명령행 owner, 후보 시험용 parent lease, upgrade와 repair를 제공한다. 측정한 호스트와 정책에서 release `0.1.0-5daee5d-b3fa569e`(Git tag가 아닌 release ID)의 macOS SLO qualification을 마쳤으며 Linux 강제 보호는 qualification하지 않았다. CodeSpace 소비(CS-RG)는 진행 중이며 qualification을 마치지 않았다. CSRG-U1은 opt-in 상태 전용 연결을 추가했다. `devguard` feature로 빌드하고 `--devguard status`로 시작한 gateway는 [`f1f9084`](upstream-lock.md#devguard-client-pin)에 고정한 DevGuard client로 DevGuard의 상태를 `workspace_info`에 보고한다. CSRG-U2부터는 CodeSpace의 실행을 소유한 프로세스가 스스로를 DevGuard에 등록하며(`--devguard register`), workspace의 `resources` 설정으로 자원 참여를 필수로 지정할 수 있다. 필수로 지정한 workspace는 관리형 실행 허용과 실행이 생길 때까지 새 실행을 모두 거부한다. 아무것도 허용하거나 실행하지 않으므로 DevGuard 서비스가 실행 중이어도 CodeSpace 실행을 관리하지 않는다. qualification을 마친 release는 실행에 쓸 후보로 남으며 선택된 pin이 아니다. client pin은 client crate의 소스 pin이다.

DevGuard [설계 개정 1](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/design-revision-1.md)은 [DevGuard PR #8](https://github.com/novelKR/DevGuard/pull/8)의 병합 `d4981b4c241cff42687f5c2c681b583c7847776e`로 반영되었으며 CS-RG의 실행 계층과 작업 순서를 개정했다. 이 개정은 계획과 이후 구현을 구속하는 설계 결정을 바꾼다. 현재 CodeSpace 동작이나 구현·qualification 상태는 바꾸지 않는다.

<a id="resource-integration-prerequisites"></a>

## 선행 조건과 우선순위

기존 P1-SCHED 다음에 **DG-0 → DG-1 → CS-RG → P1-RECOVERY**를 배치한다. 프로세스 복구와 자원 회계 복구는 별도 책임으로 유지한다.

| 마일스톤 | 소유 저장소 | 완료 조건 |
| --- | --- | --- |
| DG-0 | DevGuard | 독립 저장소, 승인 설계, 안정적인 실행 시도 식별자, 예약·계획·적용 증거 타입, 실행 권한을 한 번만 발급하는 영속 상태 전이, 등록·호환성 계약과 가짜 backend 계약 검증 |
| DG-1 | DevGuard | 실제 macOS daemon/client/launcher, generic·Cargo 소비, 기능 시험을 통과한 bootstrap 기준의 상위 예산 안에서 후보 시험, 독립 복구와 별도로 qualification을 마친 안정 artifact |
| CS-RG | CodeSpace | 실행 경계 적합성, 검증된 full SHA 소비, spawn 전 실행 슬롯과 실행마다 reaper 하나를 두는 공통 실행 조정, 한 번만 소비하는 PrepareExec/ExecPrepared, 승인 보존, 상한이 있는 관제·데이터 경로와 replay, InProcess/UDS 동등성, 기존 backend 결정, 그 결과 head의 upstream 회귀 검증 |
| P1-RECOVERY | CodeSpace | 독립 Runner가 프로세스·입출력을 계속 소유하는 동안 운영자 선택 모드로 Gateway 재시작·재연결을 복구. workspace·승인·자원 lease를 대조하며 불확실한 실행을 재실행하지 않음 |
| DG-LINUX | 양쪽 | 실제 Linux cgroup controller, ancestor 제약, sandbox·proxy를 포함한 scope와 관제 보호. 전체 제품 완료에 필수 |
| DG-CACHE / DG-ADAPTERS | DevGuard | 등록된 캐시의 안전한 회수와 추가 도구 adapter. P1-RECOVERY의 선행 조건은 아님 |

이 선행 경로 이후에는 watch 잔여, 파일 검색 엔진, 결정적 hook, skill, 원격 환경, 연합, artifact 작업의 기존 상대 순서를 유지한다. 가짜 Linux scope를 사용한 계약 시험을 실제 Linux 강제 보호 검증으로 취급하지 않는다.

DG-1은 독립 daemon/CLI, 개발 workload와 상위 예산 안의 자기 적용을 검증한다. CS-RG는 이후 결합된 Runner, 승인, replay와 포화 상태의 관제 경로를 검증한다. DG-1 완료에 아직 구현하지 않은 CS-RG 기능을 요구하지 않는다.

DG-1은 6개 PR 묶음을 순차 전달한다. 각 PR의 검토, 현재 head 검사, 정상 병합과 별도 main 검증을 끝낸 뒤 다음으로 진행한다. P4까지 foreground daemon을 사용하고 P5에서 현재 사용자의 LaunchAgent를 도입한다. C10에서는 새 부모 예산 기능을 먼저 시험하고 그 기능을 포함한 부모를 동결한 직후 실제 bounded 자기 적용을 시작한다. 앞선 P4/P5 기능 artifact가 새 부모 기능을 이미 지원한다고 가정하지 않는다. C12는 측정한 artifact·정책·환경의 SLO 자격과 승격을 별도로 판정한다.

<a id="devguard-status-connection"></a>

## 상태 전용 연결(CSRG-U1)

CSRG-U1은 CS-RG의 첫 단위다. 두 설정이 필요하며 둘 다 기본값은 꺼짐이다.

- **빌드.** gateway의 Cargo feature `devguard`(`cargo build -p codespace-server --features devguard`). DevGuard의 `devguard-client`와 `devguard-contract`를 [고정한 커밋](upstream-lock.md#devguard-client-pin)에서 링크하며 Rust 1.95가 필요하다. 이 feature 없이 빌드하면 실행 파일의 의존성 그래프, 플래그, MCP 계약은 바뀌지 않는다.
- **실행.** `--devguard status`(`CODESPACE_DEVGUARD=status`)와 운영자가 준비한 DevGuard consumer:

| 플래그 | 환경 변수 | 값 |
| --- | --- | --- |
| `--devguard-socket` | `CODESPACE_DEVGUARD_SOCKET` | DevGuard socket. 보통 `/private/tmp/devguard-<uid>/authority.sock` |
| `--devguard-consumer` | `CODESPACE_DEVGUARD_CONSUMER` | DevGuard 운영자 설정의 consumer ID |
| `--devguard-generation` | `CODESPACE_DEVGUARD_GENERATION` | 그 consumer의 generation |
| `--devguard-credential-file` | `CODESPACE_DEVGUARD_CREDENTIAL_FILE` | consumer의 64자 secret만 담은 비공개 파일(0600, 이 사용자 소유, 링크 하나)의 절대 경로. 경로의 모든 디렉터리는 이 사용자나 root가 소유한 실제 디렉터리여야 하며 그룹이나 다른 사용자가 쓸 수 없어야 한다. 단 root가 소유한 sticky `/tmp`는 예외다. 파일이 들어 있는 디렉터리는 이 사용자의 것이어야 한다 |

**보고하는 내용.** `workspace_info`를 호출할 때마다 DevGuard client로 세션 하나를 열고(연결, `Hello`, `Authenticate`, `Status`) 닫는다. DevGuard의 frame당 250 ms 기한 때문에 세션은 약 1.75초 안에 끝난다. 세션이 진행 중일 때 들어온 호출은 기다렸다가 다음 세션 결과를 함께 쓰므로 CodeSpace가 여는 세션은 동시에 하나뿐이다. 결과는 `workspace_info.resource_authority`에 담긴다.

- `participation: status`, `governs_execution: false`는 항상 이 값이다.
- `state`: `available`, `unavailable`, `untrusted_authority`, `incompatible`, `credential_refused`, `credential_unavailable` 중 하나.
- `error_code`: 실패한 단계에 대한 DevGuard의 코드. DevGuard wire의 이름을 쓰는 CodeSpace 열거값 중 하나이며, 메시지는 전달하지 않는다.
- `report`(available일 때): DevGuard의 protocol, capability와 부여한 role(둘 다 열거값), 저장소·등록·실행 준비 상태. DevGuard의 자유 서술 이유는 전달하지 않으며 `devguardd status`에서 볼 수 있다.

보고하는 값은 모두 열거값, 플래그, protocol 번호이므로 DevGuard의 텍스트는 MCP client에 전달되지 않는다.

DevGuard가 실패해도 `workspace_info`나 다른 도구는 실패하지 않으며, 알 수 없는 workspace는 세션을 열기 전에 거부한다.

**하지 않는 일.** 상태 참여는 아무것도 등록·허용·실행하지 않으며 어떤 실행 경로도 이 연결을 참조하지 않는다. UDS 모드에서는 worker가 아니라 gateway가 세션을 연다.

**secret.** CodeSpace는 세션마다 파일에서 secret을 읽어 `Authenticate`에만 보낸다. 플래그, 환경 변수, `workspace_info`, 로그에는 넣지 않으며 로그에는 상태와 DevGuard의 코드만 남긴다.

**자식 프로세스.** DevGuard client는 `socket()`으로 세션 socket을 만드는데, macOS에서는 이를 원자적으로 close-on-exec로 만들 수 없다. CodeSpace의 pipe, patch helper, sandbox helper, worker 실행은 자식에서 2보다 큰 descriptor를 모두 close-on-exec로 표시하고([#79](https://github.com/novelKR/CodeSpace/issues/79)) PTY 자식은 자신의 descriptor를 닫으므로, 실행 중에 열려 있던 세션 socket은 어떤 자식에게도 전달되지 않는다. 테스트가 pipe, PTY, worker 실행에서 이를 확인한다.

<a id="devguard-owner-registration"></a>

## 실행 소유자 등록(CSRG-U2)

CSRG-U2는 CodeSpace의 실행을 소유한 프로세스가 DevGuard에 등록된 정체성을 만들고 증명하게 한다. 아무것도 허용하거나 실행하지 않는다.

- **빌드.** 상태 연결과 같은 gateway의 `devguard` feature, 그리고 UDS 모드에서는 worker 자신의 feature(`cargo build --manifest-path crates/codex-runtime/Cargo.toml --features devguard`). 이 feature 없이 빌드한 worker는 등록할 수 없으며, `--devguard register`로 시작한 gateway는 그런 worker를 멈추고 시작하지 않는다.
- **실행.** 상태 연결과 같은 네 설정에 `--devguard register`(`CODESPACE_DEVGUARD=register`). DevGuard 운영자는 consumer를 control service로 준비한다. role `control_service`, generation, 비공개 credential 파일, instance 한도, gateway와 Runner의 관제 비용을 덮는 정적 관제 예약이 필요하다.

**소유자.** 등록하는 프로세스는 runner 모드를 따른다.

| Runner 모드 | 등록하는 프로세스 | consumer secret |
| --- | --- | --- |
| `in-process` | 실행을 직접 수행하는 gateway | 상태 연결처럼 세션마다 credential 파일에서 읽음 |
| `uds`와 `--runtime-bin` | gateway가 시작하고 실행 handle을 소유하는 worker | gateway가 한 번 읽어 비공개 descriptor 하나로 그 worker에게만 넘김 |
| `uds`와 `--runner-socket` | 없음: `unsupported_mode` | 없음: gateway가 시작하지 않은 worker에는 secret을 넘길 수 없음 |

DevGuard는 등록할 프로세스를 세션의 peer에서 얻으므로 프로세스는 자신만 등록할 수 있고, gateway가 worker를 대신해 등록하는 일은 없다. 소유자는 DevGuard가 등록한 정체성이 자기 프로세스이며 세션마다 같은지도 확인한다.

**세션.** 등록 한 번은 소유자가 열고 닫는 상한 있는 세션 하나다. 연결하고, protocol 1과 capability `static_control_reservations`를 요구하는 `Hello`, `control_service`를 부여해야 하는 `Authenticate`, 등록 준비를 보고해야 하는 `Status`를 거쳐 소유자의 instance 정체성으로 `Register`한다. DevGuard의 frame당 250 ms 기한 때문에 세션은 약 2.25초 안에 끝난다. instance 정체성(`codespace-`와 무작위 16진수 32자리)은 소유자 프로세스마다 한 번 만들고 모든 세션이 다시 등록하므로, DevGuard에는 소유자마다 instance가 하나만 있다. 세션 동안에는 active이고 세션이 끝나면 suspect가 된다. 다시 시작한 소유자는 새 프로세스이므로 새 정체성을 쓴다. 소유자는 시작할 때, 그리고 `workspace_info` 호출과 거부되는 `required` 실행마다 등록한다. 세션이 진행 중일 때 들어온 호출은 기다렸다가 다음 세션 결과를 함께 쓴다.

**보고하는 내용.** `register`이면 `workspace_info.resource_authority`는 소유자의 세션에서 나온다. `participation: registration`, `governs_execution: false`, 상태 연결과 같은 authority의 `state`, `error_code`, `report`, 그리고 `registration`:

- `owner`: `in_process` 또는 `worker`.
- `state`: `registered`, `unavailable`, `untrusted_authority`, `incompatible`, `credential_unavailable`, `credential_refused`, `role_mismatch`, `not_ready`, `refused`, `owner_mismatch`, `owner_unreachable`, `unsupported_mode` 중 하나.
- `error_code`: 실패한 단계에 대한 DevGuard의 코드. 상태 연결과 같은 열거값이며 메시지는 전달하지 않는다.
- `pid`: `registered`일 때 DevGuard가 등록한 소유자의 프로세스 ID.

소유자에게 물을 수 없을 때(`owner_unreachable`, `unsupported_mode`)는 authority 항목을 gateway 자신의 상태 확인 결과로 채운다.

**실패.** consumer, generation, secret이 틀리면 `credential_refused`다. control service가 아닌 consumer는 `role_mismatch`다. protocol이 다르거나 capability가 없으면 `incompatible`이다. 다른 프로세스가 instance 정체성을 쓰고 있거나 generation이 폐기되었거나 instance pool이 가득 차는 등 DevGuard가 등록을 거부하면 DevGuard의 코드와 함께 `refused`다. 소유자의 것이 아닌 정체성은 `owner_mismatch`다. 실패해도 어떤 도구도 실패하지 않는다.

**자원 참여.** registry의 workspace별 `resources` 설정이 그 workspace의 실행이 자원 관리 시스템에 참여하는지 정한다.

```json
{"workspaces": {"demo": {"root": "/abs/path", "profile": "workspace-write",
                         "resources": {"participation": "required"}}}}
```

기본값 `off`와 설정이 없는 경우는 실행을 이전과 같게 둔다. `required`이면 `exec_command`나 재개한 승인으로 들어온 새 실행을 승인, lease, 프로세스가 생기기 전에 `RESOURCE_POLICY_UNSUPPORTED`로 모두 거부한다. CodeSpace는 아직 DevGuard를 통해 실행을 허용하거나 실행하지 않기 때문이다(CSRG-U3과 U4). 메시지는 실행 소유자가 등록되었는지 알려 준다. feature 없이 빌드했거나 `--devguard register` 없이 시작한 경우도 포함해 모든 빌드가 같게 거부하며, `required`가 자원 관리 없이 실행하는 쪽으로 물러서는 일은 없다. 알 수 없는 `resources` 설정은 registry 로드를 실패시킨다. 읽기, 찾기, 패치와 실행 중인 프로세스의 제어는 영향을 받지 않는다.

**프로세스 제어.** `process_status`, `read_process`, `write_stdin`, `process_resize`, `terminate_process`와 timeout은 DevGuard에 묻지 않는다. UDS 모드의 worker는 등록 요청을 다른 요청과 나란히 처리하므로 등록 세션이 이들을 지연시키지 않는다.

**secret.** UDS 모드에서 gateway는 상태 연결과 같은 규칙으로 credential 파일을 읽어 DevGuard의 `CredentialHandoff`에 담는다. secret을 담은 socket pair로, 쓰는 쪽은 이미 닫혀 있고 읽는 쪽은 gateway에서 close-on-exec다. worker를 시작할 때만 다른 descriptor를 모두 제외한 뒤 그 descriptor의 close-on-exec를 해제하며, gateway의 사본은 시작이 성공하든 실패하든 시작 호출이 끝날 때 닫힌다. worker는 secret이 아니라 descriptor 번호를 인자로 받는다. 무엇이든 처리하기 전에 descriptor를 소비해 닫고, secret은 세션에 쓰도록 메모리에만 둔다. secret은 플래그, 환경 변수, MCP 입력과 출력, 로그, 상태 어디에도 나타나지 않는다. 파일을 쓸 수 없으면 worker는 descriptor 없이 시작해 `credential_unavailable`을 보고한다.

**자식 프로세스.** 등록 세션 socket과 handoff의 socket pair는 macOS에서 원자적으로 close-on-exec로 만들 수 없다. 상태 연결처럼 CodeSpace의 실행 경로가 이들을 자식에게서 제외한다. 테스트는 pipe, PTY, worker 실행이 자식을 시작하는 동안 carrier를 넘기며 worker를 시작하고, worker가 pipe와 PTY 자식을 시작하는 동안 worker의 등록을 반복한다. socket을 가진 자식은 없다.

**플랫폼.** macOS에서는 실제 호스트 증거를 가진 DevGuard 서비스가 소유자를 등록한다. 실제 증거가 없으면(DG-LINUX 전의 Linux) DevGuard가 capability를 하나도 밝히지 않으므로 등록은 `incompatible`이다.

**하지 않는 일.** 실행 허용, 실행, permit, carrier, launch helper는 없다. spawn, PTY, 출력, timeout, 종료, reap, lifecycle은 CodeSpace에 남으며 DevGuard는 CodeSpace 실행을 관리하지 않는다.

## CS-RG 작업 순서

> **상태: 구현 지시로서 효력 중지.** CS-RG 통합 경계 재검증이 끝날 때까지 이 절을 근거로 구현하지 않습니다. 대체 구조는 승인되지 않았고, 어떤 안전 요구도 완화되지 않습니다. 내용은 이력 추적을 위해 바꾸지 않고 남깁니다.

설계 개정 1은 CS-RG를 6개 논리 PR 묶음의 10개 작업 단위로 계획한다. ID는 DevGuard 계획 라벨이며 commit이나 GitHub PR 번호가 아니다. 구현을 마친 단위는 없다.

| 예정 묶음 | 단위 | 계획 내용 |
| --- | --- | --- |
| CSRG-P0 | C00 | 실행 경계 적합성 검증. 모든 spawn·reap 경로의 소유권 표, DevGuard launch helper를 사용하는 최소 managed PTY, descriptor·spawn guard 적합성 보고, deadline 전파와 reap 전 관측 측정. 시험 전용이며 제품 동작은 바뀌지 않음 |
| CSRG-P1 | C01, C02 | client와 helper 출처를 구분해 기록하는 검증된 DevGuard client revision 소비, 실행 소유자 등록과 `resources` 설정 |
| CSRG-P2 | C03, C04 | spawn 전 슬롯과 실행마다 reaper 하나를 두는 공통 supervisor, 한 번만 소비하는 launch plan과 불확실한 작업을 재실행하지 않는 승인 처리 |
| CSRG-P3 | C05, C06 | 관제·데이터 보호, 상한이 있는 replay·출력·수명 이벤트 전달 |
| CSRG-P4 | C07, C09 | 모드 전반의 동등성·장애 시험, 이어서 backend 수렴 결정 |
| CSRG-P5 | C08 | C09가 남긴 head의 qualification |

CSRG-C09는 최종 qualification 전에 반드시 내려야 하는 결정이다. 기존 `off` backend를 통합하고 대체된 코드·분기·fixture·의존성을 제거하거나, 이유·범위·중복 부분·제공하지 않는 capability·재검토 시점·제거 기준을 기록한 제한적 호환 backend로 유지한다. 문서에 "검토함"이라고 적는 것만으로는 완료되지 않는다. C09가 코드를 바꾸면 영향받는 C07 동등성 시험을 다시 실행하고, CSRG-C08은 그 결과 head를 qualification한다.

이 대응 문서는 DevGuard [PR 진행서](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/planning/pr-delivery.md)의 CSP-D04이며 CSRG-P0 시작 전에 전달해야 하는 조건이다. 계획한 구조는 [아키텍처](architecture.md), 실행 계약은 [실행 계약](execution-substrate.md), 재사용 결정은 [Codex 재사용 범위](codex-reuse.md), 의존성·CI 경계는 [업스트림 업데이트](upstream-update.md)에 정리한다.

<a id="resource-adoption-levels"></a>

## 최소 도입 조건

| 수준 | 최소 증거 | 허용 범위 |
| --- | --- | --- |
| 계약 준비 | DG-0 계약과 정확한 source의 시험 | adapter·오류·상태 대응 설계. 운영 보호가 있다고 표시하지 않음 |
| 제한된 기능 시험 | 실제 인증·launch·회수 경로를 갖춘 DG-1 후보와 명시적인 시험 환경 | 후보 기능 시험. 일상 사용 qualification은 아님 |
| macOS 개발 | DG-1 qualification, 실제 host probe, 충분한 예산과 실제 CLI/adapter 진입점 | 검증한 개발 명령과 호스트 조합 |
| CodeSpace macOS 런타임 | DG-1·CS-RG qualification, 지원 client/artifact/wire 조합 | 검증한 모드의 명시적 required 참여와 측정된 관제 보호 |
| Linux 강제 보호 | 실제 controller·위임 권한·ancestor 상한의 추가 qualification | 검증한 자원과 scope의 kernel 제어 |
| DevGuard 자기 적용 | 부모 예산 기능을 시험한 뒤 동결한 C10 부모 artifact, parent lease, 격리된 시험 상태·자격·cache, 독립 복구 | C10부터 부모 예산 안에서 실제 후보 개발. 일상 사용은 C12 SLO qualification 필요 |

실제 실행 호스트에서 참여하는 모든 소비자는 정상 authority와 예산 하나를 공유한다. 설정 파일을 두는 것만으로 명령이 governor를 통과하지는 않는다. 호스트 여유분과 정적 제어 예약을 제외한 뒤 작업을 허용하며 최소 요구량이 들어가지 않으면 거절한다. 자원마다 요구·지원·실제 적용 결과를 구분한다. socket이나 state 경로를 바꾸어 두 번째 전체 호스트 예산을 만들 수 없어야 한다.

<a id="resource-consumer-boundary"></a>

## 소비 경계

> **상태: 구현 지시로서 효력 중지.** CS-RG 통합 경계 재검증이 끝날 때까지 이 절을 근거로 구현하지 않습니다. 대체 구조는 승인되지 않았고, 어떤 안전 요구도 완화되지 않습니다. 내용은 이력 추적을 위해 바꾸지 않고 남깁니다.

개발 과정에서는 DG-1에서 제공한 독립 CLI인 `devguard exec`로 두 저장소의 빌드와 테스트를 관리한다. 제품 런타임의 소비 지점은 실제 실행 호스트의 Runner다. DevGuard가 프로세스나 PTY 관제를 대신 소유하지 않는다. 현재 UDS worker는 gateway와 같은 호스트에서 실행되며 원격 worker가 아니다.

실행을 소유하는 **Runner가 한 번만 등록**한다. InProcess는 Gateway PID, UDS는 worker PID로 등록한다. 정적 제어 예약 하나에 Gateway와 Runner 비용을 함께 포함한다. DG-1에는 별도의 `service-exec` 경로가 없다. Gateway는 `CredentialHandoff`로 UDS worker에 소비자 자격을 전달하고 InProcess는 이를 직접 읽으며, 상한이 있는 각 세션은 같은 인스턴스를 다시 등록한다. 서비스·하위 worker 등록 모델은 여러 Runner나 공유 서비스 예약이 실제로 필요해질 때 재검토한다.

private 자격 FD는 필요한 helper 단계까지만 유지하고 사용자 executable 전에 닫으며, MCP 인자로 자격을 노출하지 않는다. pinned Codex PTY는 선택 FD 상속을 받지만 고수준 spawn이 자식을 내부에서 reap하고 이미 상속 가능한 descriptor만 유지한다. 따라서 현재 PTY wrapper를 확장하는 방식으로는 DevGuard가 관리하는 launch를 처리할 수 없으며, 계획한 경로는 [실행 계약](execution-substrate.md)에 정리한다. 설계 개정 1은 Codex pin을 유지하며 pin 변경은 검증에 근거한 별도 결정이다.

런타임 adapter는 회계상 예약, 실행 계획과 적용이 확인된 정책을 구분해야 한다. macOS에서 참여를 필수로 설정했다고 전체 자손에 대한 강제 상한이 생기지는 않는다. DevGuard journal은 CodeSpace 프로세스 handle을 복구하거나 patch operations 원장을 대체하지 않는다.

CS-RG에서는 기존 인가와 workspace FIFO 획득 뒤 spawn 전에 실행 슬롯을 확보하고, 자원 준비에 성공한 뒤 승인 resume를 실행 단계로 전환한다. 자원 거절은 소비하지 않은 hold를 queued로 유지한다. 해당 attempt가 시작되지 않았다고 확인된 거절·실패만 승인 재사용을 허용한다. timeout, 응답 유실, 찾을 수 없는 process handle은 그런 증거가 아니다. 관리 helper 추적 성공인 `confirmed`, helper의 `READY`, 사용자 executable의 실행 성공은 서로 다른 사건이다.

실행 슬롯, 자원 lease, 완료 출력 보존은 별도 수명이다. 자원 부족, authority 장애, 요구 정책 미지원, 실행 여부 불확실을 구분한다. 자원 관리 서비스 장애 중에는 신규 작업을 빠르게 거절하고 기존 `process_status`와 `terminate_process`는 새로운 admission 없이 Runner가 가진 handle로 처리한다. 공개 MCP 도구 이름은 유지한다.

관제 보호는 대기·동시 요청 수뿐 아니라 누적 queued/replay/response byte와 보존 시간도 제한한다. control/data 소켓을 분리해도 공유 mutex·writer·callback이 관제를 막으면 충분하지 않다. 현재 전체 파일 read/hash 경로도 별도의 메모리 상한 개선이 필요하다. 해결 전 qualification은 파일 크기·동시성을 고정하고 임의 크기 파일까지 보호한다고 주장하지 않는다.

contract/client pin, daemon/helper artifact와 CodeSpace Runner wire는 각각 검증한다. 필수 capability가 없으면 거절하며, 같은 호스트 예산을 다시 발급하는 daemon을 추가 실행하거나 정책을 조용히 끄지 않는다.

후속 runtime 구현에서도 참여의 기본값은 `off`이며 `required`는 운영자가 명시적으로 선택한다. 계약·journal의 엄격한 역직렬화를 고려해 실제 구·신 reader/writer를 시험한다. 필드 추가만으로 자동 후방 호환을 가정하지 않는다. upgrade 복구는 현재 원장을 보존하며 새 admission 이후 오래된 snapshot으로 되돌리지 않는다.

<a id="resource-gateway-recovery"></a>

## 최초 Gateway 복구 범위

P1-RECOVERY는 운영자가 선택하는 독립 Runner 모드와 명시적인 capability를 도입한다. 기존 모드의 종료·연결 소실 계약은 유지한다. InProcess는 Gateway와 PID를 공유하므로 Gateway 재시작 중 live 복구 대상에서 제외한다.

새 모드는 정상 종료, 명시적인 서비스 중지, 재시작용 detach, 예상치 못한 연결 소실을 구분한다. 살아 있는 Runner는 detach나 Gateway 소실 중에도 handle, PTY·pipe 소유권, 상한이 있는 출력과 원래 timeout을 유지한다. 새 Gateway는 인증하고 오래된 제어자 epoch의 mutation을 차단하며, workspace 점유·승인·자원 lease를 대조한 뒤 변경 작업을 재개한다. 기존 실행에 재연결할 때 새로운 작업 예산을 요구하지 않는다.

Runner나 호스트 손실은 실제 종료가 확인될 때까지 불확실 상태로 유지한다. 저장된 PID·상태만으로 PTY·pipe 소유권을 복원할 수 없고, 저장한 argv를 자동 재실행하지 않는다. Runner와 입출력 소유자 자체의 손실을 넘는 복구는 별도 후속 설계가 필요하다.

## 설계 개정 1 참조

> **상태: 구현 지시로서 효력 중지.** CS-RG 통합 경계 재검증이 끝날 때까지 이 절을 근거로 구현하지 않습니다. 대체 구조는 승인되지 않았고, 어떤 안전 요구도 완화되지 않습니다. 내용은 이력 추적을 위해 바꾸지 않고 남깁니다.

아래 고정 링크는 설계 개정 1을 반영한 문서를 가리킨다. 설계 출처이며 런타임 client pin이 아니다.

| `d4981b4`의 한국어 대응 문서 | 용도 |
| --- | --- |
| [설계 개정 1](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/design-revision-1.md) | 실행 소유권, F1a–F1e, D1–D3, 제한적 adaptation과 개정된 작업 순서의 전체 명세 |
| [확정 결정](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/planning/decisions.md) | ADR-006 실행 소유권과 재사용 정책, 대체된 문구를 표시한 ADR-001 등록 규칙 |
| [CodeSpace 결합 명세](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/planning/codespace-integration.md) | CodeSpace `b6e7ed2` 기준 소스 대응, 실행 소유권과 backend 결정 |
| [CS-RG 작업 패키지](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/planning/milestones/CS-RG.md) | CSRG-C00~C09, 시험, 진입/완료 조건과 rollback |
| [검증 규칙](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/planning/verification.md) / [PR 진행서](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/planning/pr-delivery.md) | CS-RG 실행 검증 matrix, 유지보수 측정과 최종 head 검증 순서 |

이 대응 문서를 작성한 2026-09-27 시점의 DevGuard main은 `30b5fa6f705f053876a8da8d00882772bcf4c41b`로, 시험 파일 하나만 바꾼 [DevGuard PR #9](https://github.com/novelKR/DevGuard/pull/9) 이후의 commit이다. 이 commit은 설계 출처가 아니며, qualification을 마친 release `0.1.0-5daee5d-b3fa569e`는 두 commit과 별도로 기록한다.

개정은 확인 기준을 CodeSpace `b6e7ed22e2c730ac987297455e250cbd6e8e8b0c`로 다시 고정하고, 아래의 기존 `e94d214`를 과거 점검 기준으로 유지한다. CSRG-C00과 CSRG-C09를 추가해 계획 전체는 **후속 구현 48개 커밋 단위와 25개 논리 PR 묶음**이다. 이 중 DG-1의 6개 묶음 12개 단위는 구현을 마쳤고 나머지 19개 묶음 36개 단위는 시작하지 않았다.

<a id="resource-plan-evidence"></a>

## 계획과 검증 근거

최초 상세 계획의 문서 revision은 `3abf08f6feffeda63f58b17ac2bbe8fff19ec20b`이며 [DevGuard 문서 PR #1](https://github.com/novelKR/DevGuard/pull/1)로 제출했다. 아래 링크는 PR 병합 전에도 존재하는 고정 commit을 가리킨다. 영문이 편집 정본이며 검토된 한국어 번역과 hash 검사를 유지한다. [영문 설계 참조](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/design.md)와 [한국어 대응 설계](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/design.md)는 불변 승인 원문과 별도로 관리한다. 이 값은 문서 식별자이며 런타임 client dependency pin 선정이 아니다. 이 revision의 계획은 7개 마일스톤에 걸쳐 후속 구현 46개 커밋 단위와 23개 논리 PR 묶음을 정의했으며, 설계 개정 1이 바꾼 전체 수는 앞 절에 적었다.

| 영문 정본에 대응하는 한국어 계획 문서 | 용도 |
| --- | --- |
| [계획 index와 마일스톤 지도](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/README.md) | 7개 전체 마일스톤과 commit·PR 경계 탐색 |
| [확정 결정](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/decisions.md) | 등록·복구 대안 비교, 채택 이유와 재검토 조건 |
| [최소 소비 조건](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/consumer-readiness.md) | 범용 도입 조건과 플랫폼별 지원 주장 범위 |
| [CodeSpace 결합 명세](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/codespace-integration.md) | 현재 소스 경로, 모드별 등록·실행·장애·복구 흐름 |
| [CS-RG 작업 패키지](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/milestones/CS-RG.md) / [P1 복구 작업 패키지](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/milestones/P1-RECOVERY.md) | CodeSpace 예정 commit·시험·진입/완료·rollback |
| [검증 규칙](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/verification.md) / [PR 진행서](https://github.com/novelKR/DevGuard/blob/3abf08f6feffeda63f58b17ac2bbe8fff19ec20b/docs/ko/planning/pr-delivery.md) | 현재·예정 명령, 증거·SLO와 검토 인계 |

qualification은 idle 기준선 10분, 부하 최소30분, 3회 반복과 원시 측정값 보존을 유지한다. 로컬 관제 지연과 원격 네트워크 시간을 분리한다. 기존 초기 목표인 process status p99 ≤500ms와 종료 요청 응답 p99 ≤1초를 유지하며 실제 scope 종료 시간은 별도로 측정한다. 새 문서 commit이나 가짜 backend 시험 통과를 OS 제어·제품 SLO qualification으로 해석하지 않는다.

과거 점검 기준 CodeSpace commit은 `e94d21475643608ad2a466256fb57266b86faa47`이며, 설계 개정 1은 앞 절과 같이 `b6e7ed22e2c730ac987297455e250cbd6e8e8b0c`를 확인 기준으로 삼는다. Codex pin은 `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`을 유지한다. 승인된 DevGuard 설계는 독립 저장소의 `docs/design.ko.md`에 보존하며 SHA-256은 `97b67a1f9518c1781156a4b3b26829b285f84f5c9a44da60f3c5dcf1bc768df8`이다.

최초 DG-0 소스 참조는 [DevGuard commit d59cbd4](https://github.com/novelKR/DevGuard/tree/d59cbd43d206a9a9281328a946eddf1dc199f710)이며 [macOS·Ubuntu 계약 CI](https://github.com/novelKR/DevGuard/actions/runs/35671367559)와 연결된다. 검토한 기반 소스를 식별하는 참조이며 CodeSpace 런타임의 client pin은 아니다. 로컬 checkout 없이도 [고정 commit의 설계](https://github.com/novelKR/DevGuard/blob/d59cbd43d206a9a9281328a946eddf1dc199f710/docs/design.ko.md)와 [마일스톤 원장](https://github.com/novelKR/DevGuard/blob/d59cbd43d206a9a9281328a946eddf1dc199f710/milestones.json)을 확인할 수 있다.

지정한 소스는 DevGuard 저장소의 로컬 checkout이다. 이 저장소의 `milestones.json`, `docs/contracts.md`, `docs/milestones.md`에서 구현 상태와 플랫폼 검증 상태를 구분한다. Rust 1.95.0 환경에서 `python3 scripts/validate.py`로 해당 소스의 보고서를 만들고 `scripts/qualify.py`로 DG-1 suite를 실행한다. suite는 Linux 강제 보호, 브라우저 응답성, 설치된 부모 아래의 실제 자기 적용을 `not_run`으로 남긴다. macOS SLO는 `scripts/measure.py`가 대상 호스트에서 측정하며, CodeSpace 결합은 CS-RG에서만 qualification한다. 기존 로드맵 commit `fb822fc24c98f6628dce62d33a5cc67275f8ca34`는 이번 문서 전달에 함께 포함하며, 런타임 기준은 앞서 명시한 별도 commit을 유지한다.

이후 개발은 영문 설계 참조와 확정 결정을 기준으로 하며, 설정·CLI 예시의 승인 이력은 원래 전체 설계에 보존한다. 현재 CodeSpace 릴리스의 설치 명령으로 사용하지 않는다. `target/upstream-reports/local`, 운영 DB, Git 메타데이터와 안정 복구 artifact는 자동 캐시 회수에서 보호한다.
