<a id="실행-기반"></a>
<a id="실행-기반"></a>

# 실행 계약

[English](../execution-substrate.md) | [한국어](execution-substrate.md)

외부 Agent Loop는 계획, 모델 문맥, 완료 판단을 담당합니다. CodeSpace는 정해진 도구 동작을 수행하고 상태를 제공합니다. 실행 기능을 추가하더라도 내부 모델 호출이나 Codex 에이전트 세션이 필수 조건이 되어서는 안 됩니다.

<a id="불변식"></a>
<a id="불변식"></a>
<a id="가져오기-빼기-개념"></a>
<a id="가져오기-빼기-개념"></a>

## 정책과 실행 구현

게이트웨이는 등록된 작업 공간에서 요청한 행동이 허용되는지 결정합니다. Runner 요청은 CodeSpace 타입으로 전달하고, 어댑터가 이를 Codex 실행 타입으로 변환합니다. 핵심 계층의 의존성 선언과 인터페이스에 Codex crate나 타입을 직접 추가하지 않습니다. 다만 어댑터의 간접 의존성이 빌드 그래프에 포함될 수 있습니다.

운영자 레지스트리의 `read-only`와 `workspace-write`를 실제 권한에 대응시키고, 네트워크는 `restricted` 또는 `enabled`로 선택합니다. 내부 정책 타입에는 세밀한 경로 규칙을 표현할 수 있는 glob 필드가 있지만, 이를 외부에서 설정하고 모든 작업에 적용하는 기능은 제공하지 않습니다. 클라이언트 인자로 권한을 높일 수 없습니다.

<a id="네-축-목표-도메인"></a>
<a id="네-축-목표-도메인"></a>

## 실행 환경과 식별자

| 개념 | 현재 용도 |
| --- | --- |
| 실행 환경 | 운영자가 선택하는 실행 위치. 호스트는 구현되어 있으며 등록된 컨테이너 백엔드는 사용 불가 |
| 작업 공간 | `workspace_id`로 선택하는 등록 루트. MCP 파일 경로는 상대 경로 |
| 권한 프로필 | 파일·프로세스 행동의 허용 범위를 게이트웨이가 결정 |
| 패치 작업 | `operation_id`와 선택적 중복 실행 방지 키, `files`/`changes` 해시, minted/finished 이벤트로 저장하는 패치 원장. `operation_status`로 조회. 명령 실행은 추적하지 않음 |
| 프로세스 | 서버가 발급하는 명령 핸들. 메모리에만 보관 |
| 확인 홀드 | 운영자 `approvals` 설정. `approvals` 테이블 행(`pending`/`granted`/`queued`/`resuming`, 단말 결과가 있을 때까지). 패치 작업이 아니며 권한 부여나 격리 경계도 아님 |
| 논리적 작업 | 작업과 사용자 지시 큐. 전송 세션과 별개 |

`environment_id`는 MCP 도구 인자가 아닙니다. 모델이 전달한 프록시 URL이나 Codex 사용자 설정이 실행 권한의 근거가 되지 않습니다.

<a id="pathsandbox와-codespace-fs"></a>
<a id="pathsandbox와-codespace-fs"></a>
<a id="command-exec-형태-대-크레이트"></a>
<a id="commandexec-형태-대-크레이트"></a>

## 실행과 결과 관측

게이트웨이는 내부 Runner 요청에 작업 공간 루트 cwd, 러너 환경 기본값, 시간·출력 제한, PTY 선택, 정책을 채웁니다. spawn 시 공개된 터미널 옵션은 `tty`뿐입니다. `tty_size`는 spawn 인자가 아닙니다. 실행 중인 PTY는 `process_resize`로 크기를 바꿉니다. 공개 호출은 임의의 cwd·환경변수·제한 시간 변경을 받지 않습니다. 기본값은 [운영](operations.md), 결과 처리는 [Agent Loop 연동](agent-integration.md)을 참고하세요.

`exec_command`는 디스패치 식별(`process_id`, `dispatch_status`)만 반환합니다. 종료 판정은 `process_status`의 `running`/`exited`와 termination 메타데이터를 사용합니다. 실행 중인 PTY 크기는 `process_resize`로 바꿉니다. `read_process`는 `output_lost`와 `retained_from`을 포함한 출력을 반환합니다. EOF는 성공이 아닙니다. 핸들이 만료된 뒤의 조회는 새 상태가 아니라 `PROCESS_NOT_FOUND`입니다. 파이프 프로세스의 크기 변경은 `PROCESS_NOT_TTY`, 종료된 핸들은 `PROCESS_NOT_RUNNING`입니다. Linux 샌드박스에서 wait 상태는 관리 자식(헬퍼 argv)의 코드이며, 사용자 argv와 동일하다고 문서화하지 않습니다.

작업 공간 잠금은 패치와 명령이 동시에 파일을 변경하지 못하게 합니다. FIFO 순서는 MCP 메시지 도착이 아니라, 적격 요청이 자원을 `acquire()`할 때 시작됩니다. 같은 작업 공간에서 요청이 소유한 패치·exec는 현재 요청이 임대를 놓을 때까지 메모리 FIFO로 기다립니다. exec가 라이브 프로세스가 되면 이미 줄 서 있던 대기자와 이후 도착은 `WORKSPACE_BUSY`로 거절됩니다. 큐가 가득 차면 `RESOURCE_QUEUE_FULL`입니다. 이 대기는 SQLite나 스레드 키 큐가 아닙니다. 명령 실행 중에도 읽기와 검색은 가능하므로 파일 I/O는 사전 경로 검사에만 의존하지 않고 파일을 여는 시점의 심볼릭 링크 변경도 거부해야 합니다. Runner 파일 작업은 `codespace-fs`, 패치 적용은 별도 패치 도우미를 사용합니다. `operation_status`는 기록된 패치 원장(`kind`는 `patch`)을 조회하며, 실행 중인 명령은 `process_id`로만 다루고 이 조회로 복구하지 않습니다.

UDS 전송과 Linux 샌드박스 준비는 서로 다른 프로토콜과 실패 경계를 가집니다. UDS 변경 요청이 일부만 전달되면 결과가 불확실할 수 있습니다. 연결이 끊겼다는 이유만으로 새 변경 요청을 보내지 마세요. 프로세스 수명은 러너 인스턴스가 소유합니다. MCP/HTTP 클라이언트 끊김은 프로세스를 유지하고, UDS 게이트웨이↔worker 단절이나 게이트웨이 종료는 소유 서브트리를 종료합니다. 영속적인 프로세스 복구는 없습니다. 프로세스·격리 규칙 전체는 [러너 격리](runner-isolation.md)에 설명합니다.

<a id="승인과-mcp-리비전"></a>
<a id="승인과-mcp-리비전"></a>
<a id="스케줄러-단일-쓰기-잠금-이후"></a>
<a id="스케줄러-단일-쓰기-잠금-이후"></a>

점유는 자원별 인메모리 FIFO입니다. FIFO 순서는 `acquire()`에서 시작합니다. 요청 소유 exclusive 대기자는 현재 요청이 임대를 놓으면 순서대로 실행됩니다. 확정된 라이브 프로세스는 장벽입니다. 이미 줄 선 대기자와 새 acquire는 `WORKSPACE_BUSY`를 받습니다. spawn 예약은 그 장벽이 아니며, spawn 실패 시 다음 대기자를 깨웁니다. 큐 깊이는 제한되며 포화는 `RESOURCE_QUEUE_FULL`입니다. `read`와 `find`는 shared 임대를 잡지 않습니다. 이 큐는 SQLite에 저장되지 않고 스레드 키로도 구분하지 않습니다. 라이브 MCP 변경은 경로 단위 잠금이 아니라 작업 공간 exclusive를 사용합니다.

<a id="fs-watch와-검색"></a>
<a id="fswatch와-검색"></a>

## 파일 시스템 관측

Runner는 해당 작업 공간에서 처음 `read`·`find`·`version`·`apply_patch`가 호출될 때 재귀 파일 감시자를 시작할 수 있습니다. 이벤트는 작업 공간 상대 경로의 무효화 힌트(`Create`, `Modify`, `Remove`, `Rename`, `ResyncRequired`)이며 `epoch`와 전달 순서 `seq`를 가집니다. MCP 도구가 아니고, 권한 결정도 아니며, 변경 전제도 아닙니다. watch 경로는 작업 공간 상대 무효화 이름입니다. 경로가 방출되어도 그 경로가 읽기·쓰기 가능하거나 일반 파일·비심링크라는 뜻은 아닙니다. 실제 접근은 계속 `PathSandbox`가 강제합니다.

변경 전제는 여전히 `expected_versions`와 `VERSION_CONFLICT`입니다. 놓친·합쳐진·재시작된 watch 이벤트 때문에 디스크 해시가 달라진 `apply_patch`가 성공해서는 안 됩니다. 오버플로, 수신 실패(뒤처진 구독자 포함), 분류할 수 없는 이벤트는 같은 epoch에서 `ResyncRequired`를 내며 소비자 캐시 전체를 신뢰하지 않아야 합니다. 감시자를 다시 시작할 때는 교체 감시자가 살아 있는 뒤에만 `epoch`가 증가하고 `ResyncRequired`를 냅니다. 이 기반은 자체 apply와 외부 편집을 구분하지 않고, 손실 없는 이벤트 원장을 두지 않으며, UDS로 watch 이벤트를 보내지 않습니다.

`find`는 제한된 glob 탐색입니다. watch API가 아닙니다. `read`와 `find`는 선택적 `offset`·`limit`을 받으며, 생략하면 첫 창입니다. 호출당 상한은 1 MiB와 경로 10000개입니다. `truncated`는 이 창 뒤에 관측된 바이트나 경로가 더 있다는 뜻이며, `next_offset`이 있을 때만 이어 읽습니다. `content_lossy`는 UTF-8 치환 디코딩을 표시합니다. `incomplete`는 다음 페이지가 아니라 walk가 상한에 걸렸다는 뜻입니다. `listing_version`은 반환된 페이지가 아니라 이번 호출에서 관측한 정렬된 매칭 집합의 식별자입니다. `version`은 전체 파일 해시입니다. watch 이벤트는 무효화 힌트이며 목록 식별자가 아닙니다.

<a id="훅과-스킬"></a>
<a id="훅과-스킬"></a>
<a id="로드맵-구현은-나중"></a>
<a id="로드맵-구현은-나중"></a>

## 확인 홀드

`approval_create`, `approval_resolve`, `operation_resume`을 호출할 수 있습니다. 작업 공간 프로필이 이미 허용한 변경을 홀드가 승인될 때까지 멈춥니다. 보안 경계가 아닙니다. 권한을 높이거나 `{ "network": true }`·`ClientClaims.approved`를 적용하거나 패치 원장에 V4A 스냅샷을 넣지 않습니다. 같은 MCP 호출자가 grant할 수 있습니다.

운영자가 작업 공간 `approvals`를 `confirm`으로 두면, 정책이 허용한 `apply_patch`와 `exec_command`는 `begin()`이나 프로세스 시작 전에 `APPROVAL_REQUIRED`와 `approval_id`를 반환합니다. 같은 논리 요청을 다시 보내면 그 활성 홀드를 재사용합니다. 기본값 `off`에서는 해당 도구가 바로 실행됩니다. 세 도구는 목록에 남아 있으므로 명시적 `approval_create`로 홀드를 만들 수 있습니다. grant는 프로필을 바꾸지 않습니다. 재개는 `granted`를 `queued`로 옮긴 뒤 `allow()`를 다시 검사하고, 자원을 받은 다음에야 `resuming`으로 올립니다. `consumed`는 단말 결과와 함께만 기록됩니다. `queued`에서 재시작하면 다시 acquire합니다. `resuming`에서 이후 재개는 그 결과, 패치 원장 복구, 또는 `APPROVAL_AMBIGUOUS`를 반환합니다. 중단된 exec는 다시 spawn하지 않습니다. 정책 거절은 그대로 `UNAUTHORIZED`입니다. 명령은 `process_id`로 다룹니다. v1은 호스트와 모델을 구분하지 않습니다. 서버가 보장하는 것은 재개 시 정책 재검사와 이 내구성 계약입니다.

## 계획된 관리 실행 계약

이 절의 규칙은 DevGuard [설계 개정 1](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/design-revision-1.md)과 [CodeSpace 결합 명세](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/planning/codespace-integration.md)에 따른 CS-RG의 **목표** 계약입니다. 구현된 규칙은 없습니다. 현재 동작은 앞 절들에, 계획한 구조는 [아키텍처](architecture.md)에 설명합니다.

**한 번만 소비하는 준비.** 계획된 `PreparedExecution`은 실행 식별자, 명령 의미, 실행 슬롯, 작업 공간 점유, 자원 상태, 최초 deadline을 소유합니다. 그 `LaunchPlan`은 정확히 한 번 소비되거나 취소됩니다. 복제하거나 저장된 argv로 다시 만들지 않고, 자동 재실행은 없으며, 취소된 작업의 Drop이 자원 lease를 반환했다고 가정하지 않습니다.

**별도의 수명.** 종료 관측, reap, 출력, 작업 공간 점유, 자원 lease는 각각 따로 끝납니다. EOF는 종료가 아니고, 종료는 reap이 아니며, 루트의 reap은 자손 종료가 아닙니다. 출력 보존 만료는 lease 해제가 아니고, helper의 `READY`는 payload 성공이 아닙니다. 루트 reap, stdout EOF, 신호 전송만으로 전체 scope의 종료를 증명할 수 없습니다. 확인할 수 없으면 결과를 미완료로 두고 자원은 계속 사용 중으로 계산합니다.

| 보호 규칙 | 계획된 계약 |
| --- | --- |
| F1a reap 소유권 | 자식마다 reaper는 하나입니다. `required` 경로에서는 자식을 소유한 객체 밖에서 `wait`, `try_wait`, `waitpid`를 호출하지 않으며, 제한 시간, `terminate_process`, 작업 공간 종료, 서버 종료는 supervisor에 의도를 전달합니다. reap하지 않고 종료를 감지한 뒤 전체 예산 안에서 DevGuard `Observe`를 수행하고, 그다음 같은 소유자가 reap합니다 |
| F1b descriptor 전달 | 준비부터 payload까지 descriptor의 소유, 상속, 닫기를 명시합니다. permit·자격·transcript descriptor는 그것이 필요한 helper 하나에만 전달하며, 자격 descriptor는 사용자 executable 전에 닫습니다 |
| F1c 동시 spawn | DevGuard의 `helper_command()`와 `HelperCommand::spawn()`은 `spawn_guard`를 스스로 획득하므로 호출자가 그 주변에서 guard를 잡으면 안 됩니다. 이 guard는 재진입할 수 없습니다. 파이프·PTY spawn, 패치·샌드박스 도우미, 보조 명령, worker 생성, 테스트 도우미를 포함해 같은 OS 프로세스의 다른 모든 자식 생성은 descriptor 생성, 상속 설정, spawn 구간에서만 공통 guard나 검증된 동등 보호를 잡습니다. 기존 Codex PTY의 spawn 경로가 안전하다고 확인되기 전에는 한 프로세스에서 기존·관리 spawn을 섞는 조합을 검증되었다고 선언하지 않습니다. Gateway와 UDS worker는 따로 점검합니다 |
| F1d 출력과 핸들 | 보존 상한이 있는 CodeSpace 출력 수집기 하나를 사용합니다. bridge 내부 손실, 역압, 핸들 Drop이 상위 계약과 어긋나면 안 되며, 손실 여부를 모르는 상태를 `output_lost=false`로 보고하지 않습니다 |
| F1e 유지보수 분기 | 기존 백엔드 위에 공통 인터페이스를 두는 것으로 끝나지 않습니다. CSRG-C09가 최종 qualification 전에 통합 또는 제한적 호환 백엔드를 결정합니다 |

## 아직 제공하지 않는 기능

영속적인 프로세스 복구와 컨테이너·원격 실행은 제공하지 않습니다. MCP `fs/watch`, UDS watch 이벤트, 쓰기 원인 분류도 제공하지 않습니다. 내부 타입이나 협상된 프로토콜 플래그가 존재한다고 해당 기능을 호출할 수 있는 것은 아닙니다.

## 구현 경계 유지

`check-no-model-deps.sh`는 핵심 계층의 의존성 선언과 일부 소스 패턴, 어댑터 의존성 허용 목록을 검사합니다. 업스트림 소스 전체를 검사하거나 가능한 모든 모델 호출의 부재를 증명하는 도구는 아닙니다. CI는 Codex 고정 커밋, 어댑터 빌드·테스트, Runner에서 샌드박스 도우미 라이브러리로 향하는 금지된 의존성도 확인합니다. 릴리스 검증은 [업스트림 업데이트](upstream-update.md)를 참고하세요.
