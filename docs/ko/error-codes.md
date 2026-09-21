<a id="오류-코드와-전송-대-실행"></a>
<a id="오류-코드와-전송-대-실행"></a>

# 오류와 불확실한 결과

[English](../error-codes.md) | [한국어](error-codes.md)

먼저 전송 실패와 도구 결과를 구분하세요. HTTP·연결 오류만으로 요청한 변경이 실행되었는지 알 수 없습니다. 패치·프로세스·작업 관리 상태는 서로 다른 식별자를 사용합니다.

<a id="전송-실패-작업-없음"></a>
<a id="전송-실패-작업-없음"></a>

## 전송 실패

핸들러 실행 전 인증이 거부되면 작업 기록을 만들지 않습니다. 반면 연결 끊김이나 프록시 오류는 실행 요청 전달 후에도 발생할 수 있으므로 작업이 없다는 증거가 아닙니다. 도메인 분류 함수는 HTTP 401/403, 404, 408, 429, 502/503/504를 전송 실패로 분류합니다. 실제 요청 단계와 기록된 작업을 확인해 가능한 복구를 수행하세요.

<a id="실행-오류-코드-도구-결과"></a>
<a id="실행-오류-코드-도구-결과"></a>

## 도구 오류 코드

오류는 대문자 식별자와 메시지로 전달합니다. 실패 전에 패치 기록을 만들었다면 `operation_id`가 포함될 수 있습니다. 확인 홀드 오류에는 `approval_id`가 있습니다. 그 전에 발생한 권한·점유·키 충돌 오류에는 작업 ID가 없습니다.

| 코드 | 의미 |
| --- | --- |
| `UNAUTHORIZED` | 도구 정책이 행동을 거부함 |
| `WORKSPACE_NOT_FOUND` | 작업 공간 ID가 등록되지 않음 |
| `WORKSPACE_BUSY` | 다른 변경 작업이나 실행 중인 명령이 작업 공간을 점유함 |
| `INVALID_PATCH` | 패치 파싱·사전 검증·결과 검증 실패. 일부 도우미·입력 오류 경로에도 남아 있음 |
| `INVALID_COMMAND` | 잘못된 명령 인자 배열을 실행 전에 거부함 |
| `PROCESS_SPAWN_FAILED` | 관리 프로세스를 시작하지 못한 것으로 백엔드가 확인함 |
| `PATH_ESCAPE` | 요청 경로가 작업 공간 범위를 벗어남 |
| `FILE_NOT_FOUND` | 대상 경로가 없음 |
| `PATH_NOT_DIRECTORY` | 디렉터리여야 하는 경로 구성 요소가 디렉터리가 아님 |
| `FILE_OPERATION_FAILED` | 허용 범위 안에서 파일 시스템 작업 실패 |
| `SYMLINK_REJECTED` | 심볼릭 링크 경로 거부 |
| `SPECIAL_FILE_REJECTED` | 장치·소켓·FIFO 등 일반 파일이 아닌 대상 거부 |
| `ADD_FILE_EXISTS` | 추가할 파일이 이미 존재함 |
| `MOVE_DESTINATION_EXISTS` | 이동 대상이 이미 존재함 |
| `VERSION_CONFLICT` | 현재 내용이 예상 버전과 다름 |
| `OPERATION_KEY_CONFLICT` | 같은 패치 키를 다른 인자에 사용함 |
| `OPERATION_NOT_FOUND` | 조회 ID·키가 없거나 조회 식별자를 정확히 하나 지정하지 않음 |
| `PROCESS_NOT_FOUND` | 프로세스 핸들이 없거나 만료됨. 입력 시 stdin이 이미 닫힌 경우도 포함 |
| `PROCESS_NOT_TTY` | 파이프(`tty: false`) 프로세스에 `process_resize`를 호출함 |
| `PROCESS_NOT_RUNNING` | 핸들은 있으나 실행 중이 아닌 프로세스에 `process_resize`를 호출함 |
| `OUTPUT_LIMIT` | 도우미 출력 상한 초과. `read`/`find`에서 `limit`가 0이거나 광고된 상한을 넘을 때도 사용. 프로세스 출력 조회는 오래된 바이트를 버리는 방식 |
| `TIMEOUT` | 관리 명령 또는 도우미의 제한 시간 초과 |
| `WORK_NOT_FOUND` | 논리적 작업을 찾을 수 없음 |
| `WORK_CLOSED` | 열린 작업에만 가능한 동작 |
| `INTENT_NOT_FOUND` | 사용자 지시를 찾을 수 없음 |
| `INTENT_ALREADY_CLAIMED` | 이미 가져간 지시임 |
| `INTENT_NOT_EDITABLE` | 지시 상태가 편집을 허용하지 않음 |
| `INTENT_REVISION_CONFLICT` | 지시 수정 버전이 달라짐 |
| `QUEUE_NOT_EMPTY` | 예약된 코드. 현재 work_finish는 closed:false를 반환함 |
| `APPROVAL_REQUIRED` | 정책은 허용했으나 실행 전 확인이 필요함. `approval_id` 포함. 권한 부여가 아님 |
| `APPROVAL_NOT_FOUND` | 알 수 없는 확인 홀드 ID |
| `APPROVAL_CONFLICT` | 홀드가 아직 대기 중이거나 이미 결정되었거나, 재개가 이미 진행 중임 |
| `APPROVAL_AMBIGUOUS` | 재개가 중단되어 단말 결과가 디스크에 없음. `approval_id` 포함. 패치는 `operation_id`도 있을 수 있음 |

## 실행 요청과 완료의 구분

`dispatch_status: unknown`은 실행 여부가 불확실하다는 정상 형식의 응답이며 프로세스가 시작되지 않았다는 뜻이 아닙니다. 연결 가능하면 반환된 핸들을 사용하고 중복 실행을 피하세요. `confirmed`도 요청 확인을 뜻하며 명령 성공을 뜻하지 않습니다.

Linux 도우미 준비·프로토콜·시작 오류는 관리 프로세스 생성 전에 발생하므로 `PROCESS_SPAWN_FAILED`를 사용합니다. 도우미가 이미 시작된 뒤 계획 읽기, 내부 샌드박스, 프록시 시작에서 실패하면 프로세스 종료로 처리됩니다. 그 결과는 `process_status`로 확인하세요. EOF는 성공이 아닙니다. Linux 샌드박스에서 wait 상태는 관리 자식(헬퍼 argv)의 코드입니다.

패치는 [상태 표와 복구 한계](behavior-differences.md)를 참고하세요. 재시도, 시간 초과, 사라진 핸들, 사용자 지시 완료는 [Agent Loop 연동](agent-integration.md)을 따릅니다. `work_finish`의 `closed: false`, `reason: "pending_user_input"`는 애플리케이션 결과이며 전송 실패가 아닙니다.
