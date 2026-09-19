# Agent Loop 연동

[English](../agent-integration.md) | [한국어](agent-integration.md)

CodeSpace는 에이전트의 실행 계층입니다. 외부 루프가 도구를 선택하고 패치를 작성하며 결과를 해석하고 완료 여부를 판단합니다. 먼저 [서버 설정과 작업 공간 등록](operations.md)을 마치세요.

## 연결과 기능 확인

MCP 클라이언트 SDK로 stdio 또는 Streamable HTTP를 초기화하고, 초기화 완료 알림을 보낸 다음 `tools/list`로 도구를 조회합니다. 저장소 테스트의 기준 버전은 MCP 2025-11-25입니다. `/mcp`에 도구 인자만 일반 HTTP 본문으로 보내지 말고, MCP 메시지 형식·세션 헤더·결과 해석은 SDK를 통해 처리하세요.

아래 예시는 완전한 JSON-RPC 메시지가 아니라 `tools/call`의 매개변수 객체입니다. SDK에 따라 도구 결과가 구조화된 데이터 또는 JSON을 담은 텍스트로 전달될 수 있습니다. MCP 오류와 도구 자체의 결과를 모두 확인한 뒤 진행하세요.

```json
{
  "name": "workspace_info",
  "arguments": {
    "workspace_id": "demo"
  }
}
```

`execution.files.*.available`, `execution.process.available`, `execution.isolation.command_sandbox`, `execution.network`를 확인합니다. 도구 목록은 제공되는 기능을, 작업 공간 정보는 해당 환경의 권한과 지원 여부를 나타냅니다. 사용 가능 표시는 작업 공간 예약을 뜻하지 않습니다. 읽기 전용 작업 공간에서는 명령을 실행할 수 없습니다.

## 파일 읽기와 패치

예시는 임시 프로젝트에서 실행하세요. 연결 전에 `hello.txt`를 만들고 `hi`와 줄바꿈을 넣습니다. 먼저 파일을 읽습니다.

```json
{
  "name": "read",
  "arguments": {
    "workspace_id": "demo",
    "path": "hello.txt"
  }
}
```

응답의 `version`을 보관합니다. 경로는 등록된 루트 기준 상대 경로입니다. `find`는 파일 내용이 아닌 경로 glob으로 검색합니다. `read`는 최대 1 MiB, `find`는 제한된 개수의 경로를 반환합니다. `truncated`가 참이면 전체 결과를 받지 못한 것이며, 공개 API에는 범위 읽기나 페이지 지정 인자가 없습니다.

아래 `VERSION_FROM_READ`를 실제 `read` 응답의 버전으로 바꾸세요. V4A는 `*** Begin Patch`, `*** Update File` 같은 표식을 사용하는 Codex 텍스트 패치 형식입니다. 아래 예시는 JSON의 줄바꿈 이스케이프를 사용하는 완전한 패치입니다.

```json
{
  "name": "apply_patch",
  "arguments": {
    "workspace_id": "demo",
    "patch": "*** Begin Patch\n*** Update File: hello.txt\n@@\n-hi\n+hello\n*** End Patch\n",
    "expected_versions": {
      "hello.txt": "VERSION_FROM_READ"
    },
    "operation_key": "hello-preview-1",
    "check_only": true
  }
}
```

사전 검증이 성공하면 파일을 쓰지 않고 `status: "checked"`를 반환합니다. 실제 적용 시에는 패치와 예상 버전을 유지하고 `check_only: false`와 **새 키**(예: `hello-apply-1`)를 사용하세요. 사전 검증과 실제 적용은 인자가 다른 요청이므로 같은 중복 실행 방지 키를 쓸 수 없습니다. 적용 후 파일을 다시 읽어 확인합니다. [패치 상태와 복구 한계](behavior-differences.md)도 참고하세요.

## 명령 실행과 결과 확인

```json
{
  "name": "exec_command",
  "arguments": {
    "workspace_id": "demo",
    "command": [
      "/bin/echo",
      "agent-smoke"
    ]
  }
}
```

명령은 인자 배열입니다. 셸을 명시적으로 실행하지 않는 한 셸 따옴표, 파이프, `&&`는 해석되지 않습니다. 작업 디렉터리는 작업 공간 루트이며 환경변수와 제한 시간은 운영자 설정을 따릅니다. 터미널이 필요한 프로그램은 `"tty": true`로 가상 터미널(PTY)을 할당합니다. 크기는 24×80으로 고정되며 크기 변경 API는 없습니다.

응답에는 서버가 발급한 `process_id`와 `dispatch_status`가 있습니다. `confirmed`는 실행 요청이 확인되었다는 뜻이며 **명령의 성공을 뜻하지 않습니다**. ID를 저장한 뒤 적절한 간격으로 출력을 조회하고, 매번 반환된 커서를 다음 조회에 사용합니다.

```json
{
  "name": "read_process",
  "arguments": {
    "process_id": "PROCESS_ID_FROM_EXEC",
    "cursor": 0
  }
}
```

결과에는 `chunk`, `cursor`, `eof`가 있습니다. stdout/stderr는 구분 없이 합쳐집니다. EOF는 출력 수집이 끝났다는 뜻이며 성공 종료를 나타내지 않습니다. 현재 MCP에는 종료 코드가 없고, 마지막 256 KiB만 보관하므로 앞부분이 별도 표시 없이 사라질 수 있습니다. EOF나 불완전한 로그만으로 빌드·테스트 성공을 선언하지 마세요. 신뢰할 수 있는 작업별 결과로 성공을 확인할 수 없다면 미검증으로 보고해야 합니다.



다음은 위 명령의 결과를 해석하는 예시입니다. ID는 설명용이며 실제 응답의 값을 사용해야 합니다. `coordination`이 없는 호출의 도구 결과 본문만 표시했습니다.

```json
{
  "process_id": "SERVER_ISSUED_PROCESS_ID",
  "dispatch_status": "confirmed"
}
```

```json
{
  "process_id": "SERVER_ISSUED_PROCESS_ID",
  "cursor": 12,
  "chunk": "agent-smoke\n",
  "eof": true
}
```

대화형 입력과 취소에도 같은 핸들을 사용합니다.

```json
{
  "name": "write_stdin",
  "arguments": {
    "process_id": "PROCESS_ID_FROM_EXEC",
    "data": "input\n"
  }
}
```

```json
{
  "name": "terminate_process",
  "arguments": {
    "process_id": "PROCESS_ID_FROM_EXEC"
  }
}
```

실행 중인 명령은 작업 공간을 점유합니다. 패치를 적용하거나 다른 명령을 시작하려면 기존 명령이 끝날 때까지 기다리거나 종료하세요. 읽기와 검색은 계속 가능합니다. 개발 서버를 오래 실행하는 작업 흐름이라면 수정 전에 서버를 멈추는 절차가 필요합니다.

## 재시도와 복구

| 상황 | 에이전트가 해야 할 일 |
| --- | --- |
| 패치 응답을 받지 못함 | 원래 키 또는 알고 있는 작업 ID로 `operation_status` 조회 |
| `VERSION_CONFLICT` | 현재 파일을 읽고 새 패치 작성. 이전 패치를 강제로 적용하지 않기 |
| `OPERATION_KEY_CONFLICT` | 다른 인자에 사용된 키이므로 이전 요청 확인 |
| 패치 `unknown` 또는 `failed_partial` | 해당 파일을 확인하고 불확실성을 보고한 뒤 새 작업 여부 판단 |
| 실행 `dispatch_status: unknown` | 프로세스가 존재할 수 있음. 연결 가능하면 해당 핸들을 조회·종료하고 무조건 재실행하지 않기 |
| `WORKSPACE_BUSY` | 점유 중인 작업을 기다리거나 프로세스 취소. 빠른 반복 재시도 피하기 |
| `TIMEOUT` | 실행이 중단된 것으로 처리하고 일부 변경이 남았는지 확인 |
| 서버·worker 연결 손실 | 재연결 후 기능과 파일 상태 확인. 기존 프로세스 핸들은 복구되지 않음 |

```json
{
  "name": "operation_status",
  "arguments": {
    "operation_key": "hello-apply-1"
  }
}
```

조회 ID는 하나만 지정합니다. `operation_status`는 패치 기록을 조회하며 명령 실행을 조회하지 않습니다. 요청 ID는 전송 메시지, `operation_id`는 기록된 패치, `process_id`는 관리 중인 프로세스를 가리킵니다. ID 자체는 권한을 부여하지 않습니다. 실행을 시작한 뒤 기록된 패치 거부 상태만으로 파일이 그대로라고 판단할 수는 없습니다. 적용 후 검증에 실패했다면 파일을 확인하세요.

## 사용자 지시 처리와 완료

필요하면 `workspace_id`와 제목으로 `work_open`을 호출합니다. 반환된 `work_id`를 지원하는 도구에 전달하세요. 안전한 중간 지점에서 `steer_status`, `steer_claim_next`를 호출하고, 가져온 지시를 처리한 뒤 `steer_complete`로 `done` 또는 `blocked`를 기록합니다. `coordination`의 개수 정보는 알림이며 지시 본문이 아닙니다.

사용자는 HTTP `/inbox`로 초안을 만들고 큐에 넣습니다. 초안은 큐에 등록된 뒤에만 전달됩니다. 이 API에는 HTTP 모드가 필요하며 기본 브라우저 UI는 없습니다. 큐의 지시문도 작업 공간 권한을 바꾸지 않습니다.

지시를 모두 처리한 뒤 작업 ID로 `work_finish`를 호출합니다. `closed: false`, `reason: "pending_user_input"`이면 완료를 선언하지 말고 남은 큐를 처리하세요. 이 호출은 작업 관리 상태를 닫을 뿐, 코드의 정확성을 인증하거나 테스트 결과를 대신하지 않습니다.

연동 검증은 읽기 → 사전 검증 → 적용 → 다시 읽기 → 실행 → 출력 수집 → 긴 명령 취소 → 키로 패치 결과 복구 순서로 구성할 수 있습니다. 실패하는 명령과 긴 로그도 포함해 현재 결과 계약의 한계를 루프가 정확히 처리하는지 확인하세요.
