<a id="아키텍처"></a>
<a id="정체성"></a>
<a id="정체성"></a>

# 아키텍처

[English](../architecture.md) | [한국어](architecture.md)

CodeSpace는 에이전트의 판단과 작업 공간의 실행을 분리합니다. MCP 서버가 권한과 작업 기록을 관리하고, Runner가 허용된 파일·프로세스 작업을 수행합니다. Codex 라이브러리는 어댑터 뒤에서 사용합니다.

<a id="현재-배치"></a>
<a id="현재-배치"></a>

## 현재 실행 구조

```text
외부 Agent Loop
  → MCP 서버: 등록 정보, 권한, 패치 기록, 작업·지시 큐
  → Runner
      ├─ InProcessRunner (기본값)
      └─ UdsRunner → codespace-codex-runtime → InProcessRunner
           ├─ 파일 작업 → codespace-fs
           ├─ 패치 처리 → codespace-patch
           └─ 프로세스 관리
                ├─ 파이프 / codespace-pty
                └─ 사용 가능한 Linux 도우미 → 샌드박스 / 관리 프록시
```

UDS worker와 기본 러너는 모두 같은 호스트에서 실행합니다. 두 방식 모두 명령 시작 시 Linux 샌드박스를 적용할 수 있습니다. Compose의 컨테이너 실험 구성으로 실행을 전달하는 경로는 없습니다.

<a id="id"></a>
<a id="저장소-배치"></a>
<a id="저장소-배치"></a>

## 구성 요소의 책임과 상태

| 구성 요소 | 책임 |
| --- | --- |
| `server` | MCP 전송, HTTP 인증·inbox, 요청 검증과 실행 연결 |
| `domain` | CodeSpace 도구 인자·결과, ID, 오류·실행 타입 |
| `policy` | 등록 경로, 환경, 권한 프로필, 네트워크 정책 |
| `store` | SQLite의 패치 작업·확인 홀드·논리적 작업·사용자 지시, 메모리의 점유 상태 |
| `runner` | 실행 데이터 타입, 파일 범위, 패치 처리, 프로세스 관리, UDS 통신 |
| 분리된 어댑터 | Codex 패치·PTY·파일 시스템·worker 보호와 소켓·Linux 샌드박스 구현 연결 |

패치 작업, 확인 홀드, 작업·지시 큐는 SQLite 파일을 설정한 경우에만 재시작 후 유지됩니다. 프로세스 핸들과 점유 상태는 메모리에만 있습니다. `operation_status`는 명령 실행을 조회하지 않습니다. 전송 요청 ID, 패치 작업 ID, 프로세스 ID, 작업 ID, 지시 ID, 승인 ID는 서로 다른 대상을 가리킵니다.

<a id="패치-적용-파이프라인"></a>
<a id="패치-적용-파이프라인"></a>

## 패치 처리 과정

게이트웨이는 작업 공간 권한을 확인하고 쓰기 점유를 확보한 다음 작업 키를 검사합니다. 이후 Runner에 패치 요청 하나를 전달하고 결과를 기록합니다. Runner는 예상 버전 확인, 사전 검증, 파일 스냅샷 저장, 패치 도우미 호출, 실제 디스크 해시 검증을 수행합니다.

도우미의 적용 호출이 실패하면 파일별 스냅샷 복원을 시도합니다. 현재 적용 후 검증 오류는 이 복원 분기를 거치지 않고 반환됩니다. 따라서 파일 시스템 전체의 원자적 트랜잭션을 보장하지 않습니다. 클라이언트 처리에 미치는 영향은 [패치 동작](behavior-differences.md)에 설명합니다.

## 프로세스 수명

MCP 요청이 끝나도 관리 중인 프로세스는 유지됩니다. 클라이언트는 `process_id`로 후속 호출을 수행합니다. 서버 재시작 후에는 핸들이 사라집니다. UDS 모드에서는 게이트웨이가 worker를 관리하므로 내부 연결 종료나 서버 종료 시 worker와 자식 프로세스가 종료되며 재접속은 지원하지 않습니다. [러너 격리](runner-isolation.md)에서 전송 경계와 격리 경계를 구분해 설명합니다.

## 계획된 실행 조정 구조

**현재 동작.** Runner의 프로세스 관리는 파이프 명령을 Tokio(`tokio::process`)로 시작하고, `tty: true` 명령은 [고정 버전](upstream-lock.md) `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`(`rust-v0.154.0`)의 Codex `codex-utils-pty` 위에 있는 `codespace-pty` 어댑터로 시작합니다. 고정 버전의 PTY spawn은 자식 프로세스를 내부에서 reap합니다. 파이프 경로에서는 종료 대기, 제한 시간 작업, 종료 요청이 각각 `try_wait`를 호출합니다.

**CS-RG 목표 구조.** DevGuard [설계 개정 1](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/ko/design-revision-1.md)은 모든 실행에 대해 CodeSpace가 소유하는 Runner 조정 계층 하나를 계획합니다. 이 계층은 실행 식별자, 승인과 실행의 연결, 상태 전이, 제한 시간, 종료 요청, 출력 기록, 정리 조정을 담당합니다. 플랫폼별 차이인 자식 프로세스 생성, 터미널 설정, 입출력 연결, 종료 관측, 실제 reap은 좁은 백엔드 경계 뒤에 둡니다. 이 구조는 구현되지 않았으며 아래 이름은 현재 API가 아니라 설계 개념입니다. DevGuard client 타입과 Codex 타입은 공개 MCP 타입에 들어가지 않습니다.

```text
Runner 실행 조정자 (계획)
  ├─ 자원 관리: off / DevGuard
  ├─ PreparedExecution → LaunchPlan, 한 번만 소비
  ├─ 프로세스 supervisor: 상태, 제한 시간, 종료, 종료 관측,
  │                      reap 순서, 출력과 해제 조정
  └─ 프로세스 백엔드
       ├─ 기존 Codex PTY    (BackendReaped)
       ├─ 기존 Tokio 파이프  (BackendReaped)
       └─ 자체 Unix 프로세스 (OwnerControlledReap, 파이프·PTY 전송)
```

| reap 모델 | 의미 | 계획된 사용 범위 |
| --- | --- | --- |
| `BackendReaped` | 백엔드가 reap하고 결과를 보고 | 자원 참여 `off`(기본값)의 기존 경로 |
| `OwnerControlledReap` | CodeSpace가 reap하지 않고 종료를 관측한 뒤 같은 소유자가 reap하는 시점을 제어 | reap 전에 관측해야 하는 DevGuard `required` 경로 |

이미 자식 프로세스를 reap한 백엔드는 `ExitedUnreaped` capability를 제공한다고 알리면 안 되며, 공통 인터페이스는 백엔드가 보장할 수 없는 기능을 약속하지 않습니다. 공통 supervisor를 두는 것만으로 Runner가 기존 백엔드의 종료 대기 소유자가 되지는 않습니다. 계약은 [실행 계약](execution-substrate.md), 상태와 작업 순서는 [DevGuard 결합 로드맵](devguard-integration.md)에 정리되어 있습니다.

<a id="목표-배치"></a>
<a id="목표-배치"></a>
<a id="초기-범위-밖"></a>
<a id="초기-범위-밖"></a>

## 확장 시 유지할 경계

핵심 계층은 Codex 타입을 직접 가져오지 않습니다. 어댑터가 여러 Codex 실행 구성 요소에 의존할 수는 있지만 게이트웨이가 Codex 에이전트가 되는 것은 아닙니다. 실행 환경은 운영자가 설정하고, MCP 클라이언트는 등록된 작업 공간만 선택합니다. 컨테이너 실행과 원격 러너는 아직 구현되지 않았습니다. 작업 공간 점유는 요청 소유 작업에 대해 자원별 인메모리 FIFO에서 기다리며, FIFO는 `acquire()`에서 시작합니다. 라이브 프로세스는 뒤 대기자와 새 요청에 `WORKSPACE_BUSY`를 반환합니다. 큐 포화는 `RESOURCE_QUEUE_FULL`입니다.

확인 홀드 도구(`approval_create`, `approval_resolve`, `operation_resume`)는 구현되어 있습니다. 프로필이 이미 허용한 변경을 홀드가 승인될 때까지 멈춥니다. 보안 경계가 아닙니다. `read-only`를 쓰기·실행으로 올리거나 `ClientClaims.approved`를 인정하거나 프로필을 바꾸지 않습니다. 같은 MCP 호출자가 grant할 수 있습니다. 재개 시 정책을 다시 검사합니다. v1은 호스트와 모델을 구분하지 않습니다.

새 전송 방식을 추가하더라도 패치 처리는 하나의 Runner 호출로 유지합니다. Codex 사용자·세션 권한을 실행 허용의 근거로 가져오지 않고 게이트웨이에서 결정합니다. 현재 불변 조건은 [실행 계약](execution-substrate.md), 연결된 어댑터는 [Codex 재사용 범위](codex-reuse.md)에 정리되어 있습니다.

<a id="프로토콜-호환성"></a>
<a id="프로토콜-호환성"></a>
<a id="mvp-도구"></a>
<a id="mvp-도구"></a>

## 도구와 프로토콜 참고

호출할 도구와 연동 예시는 [Agent Loop 연동](agent-integration.md)을, 버전 협상과 전송 테스트는 [프로토콜 호환성](protocol-compatibility.md)을 참고하세요. 아키텍처 문서에 도구 명세를 중복해서 관리하지 않습니다.
