# 오류 코드와 전송 대 실행

[English](../error-codes.md) | [한국어](error-codes.md)

HTTP/JSON-RPC **요청 id**, `operation_id`, `process_id`는 세 개의 서로
다른 식별자입니다. 잃어버린 HTTP 응답은 실행 실패가 아닙니다.
클라이언트는 변경 도구를 재실행하는 대신 `operation_status`(W08)를
호출합니다.

## 전송 실패 (작업 없음)

이들은 `operation_id`를 절대 발급하지 않으며 작업으로 저장되면 안
됩니다.

| 신호 | 의미 |
| --- | --- |
| TCP reset / client disconnect | Wire died. The process or patch may still be running. |
| HTTP 401 / 403 from Bearer middleware | Auth failed **before** any tool handler. |
| HTTP 404 at `/mcp` | Wrong path. |
| HTTP 408 / 502 / 503 / 504 | Transport or proxy. |

`codespace_domain::classify_http_status`는 이를
`FailureClass::Transport`로 매핑합니다. `TRANSPORT_FAILURE_IS_NOT_OPERATION`은
참입니다.

## 실행 오류 코드 (도구 결과)

JSON에서 `SCREAMING_SNAKE_CASE`로 직렬화됩니다.

| 코드 | 때 |
| --- | --- |
| `UNAUTHORIZED` | Tool-layer refusal after a valid transport (not Bearer 401) |
| `WORKSPACE_NOT_FOUND` | Unknown `workspace_id` (W04) |
| `WORKSPACE_BUSY` | Write lock held by a live shell (W08 / W10) |
| `INVALID_PATCH` | 패치 파싱/검증(W06 / W09); after-hash / delete-still-present / 생략된 `after_version`. rollback 파일시스템 I/O는 이 코드가 아님 |
| `INVALID_COMMAND` | Command request is structurally invalid and was rejected before process dispatch |
| `PROCESS_SPAWN_FAILED` | Execution backend confirmed that no managed process was established |
| `PATH_ESCAPE` | 허용된 workspace/path 범위를 벗어나려는 논리적 요청(`../` 또는 절대 경로) |
| `FILE_NOT_FOUND` | 대상 경로가 없음 |
| `PATH_NOT_DIRECTORY` | 디렉터리여야 하는 경로 구성 요소가 일반 파일임(`ENOTDIR`) |
| `FILE_OPERATION_FAILED` | 범위 안 경로에 대한 일반 파일시스템 I/O(`find` 루트 canonicalize 실패 포함) |
| `SYMLINK_REJECTED` | 심링크 파일 또는 조상 |
| `SPECIAL_FILE_REJECTED` | Device, socket, fifo |
| `ADD_FILE_EXISTS` | Add File destination already exists |
| `MOVE_DESTINATION_EXISTS` | Move destination already exists |
| `VERSION_CONFLICT` | `expected_versions` mismatch |
| `OPERATION_KEY_CONFLICT` | Same key, different request (W08) |
| `OPERATION_NOT_FOUND` | Unknown `operation_id` / `operation_key`, or `operation_status` did not receive exactly one of them |
| `PROCESS_NOT_FOUND` | Unknown `process_id` |
| `OUTPUT_LIMIT` | Reserved; live `read_process` drops oldest bytes instead of storing unbounded output |
| `TIMEOUT` | Managed process time limit (default 30s; `CODESPACE_PROCESS_TIMEOUT_SECS`) |
| `WORK_NOT_FOUND` | Unknown `work_id` |
| `WORK_CLOSED` | Mutating steer on a closed work |
| `INTENT_NOT_FOUND` | Unknown `intent_id` |
| `INTENT_ALREADY_CLAIMED` | Edit/cancel after the model claimed the item |
| `INTENT_NOT_EDITABLE` | State is not draft/queued |
| `INTENT_REVISION_CONFLICT` | Optimistic `revision` mismatch |
| `QUEUE_NOT_EMPTY` | Reserved; `work_finish` returns `closed: false` instead of this error |

적용 결과는 `status`를 사용합니다(`applied`, `checked`, `rejected`,
`failed_rolled_back`, `failed_partial`, `unknown`). `checked`는 성공한
`check_only` 미리보기입니다. `rejected`는 실제 거절입니다. 실패한 적용은
절대 `applied`를 보고하지 않습니다. `applied`는 디스크 해시가 헬퍼가
주장한 `after_version`과 일치해야 합니다. 재시작은 미완료 행을
`unknown`으로 남기고 자동 적용하지 않습니다.

`exec_command`는 spawn이 일어났을 수 있을 때 성공 결과에
`dispatch_status=unknown`을 실을 수 있습니다. 그것은 전송 오류 본문이
아닙니다. 반환된 `process_id`는 그 불확정 시도를 식별합니다. 새
프로세스를 시작하지 마세요. 백엔드가 도달 가능할 때만 `read_process` /
`terminate_process`를 쓰세요. unknown이 시작되지 않았다는 뜻은 아닙니다.

`INVALID_PATCH`는 더 이상 exec command 검증, 확인된 process-spawn
실패, rollback 파일시스템 I/O에 쓰이지 않습니다.

`codespace-fs` `FsError`는 제품 코드와 1:1입니다.
`NotFound` → `FILE_NOT_FOUND`, `NotDirectory` → `PATH_NOT_DIRECTORY`,
일반 `Io` → `FILE_OPERATION_FAILED`, `SymlinkRejected` →
`SYMLINK_REJECTED`, `NotRegularFile` → `SPECIAL_FILE_REJECTED`.
`PATH_ESCAPE`는 워크스페이스/경로 범위를 벗어나려는 논리적 요청만
뜻합니다.

`INVALID_COMMAND`는 구조적으로 잘못된 argv입니다. 게이트웨이는
`process_id` 발급과 mutation lease 전에 거절합니다. 러너도 같은 검사를
반복합니다. `PROCESS_SPAWN_FAILED`는 백엔드가 managed process가
**만들어지지 않았음을 확정**한 것입니다. 게이트웨이는 잡은 lease를
해제합니다. 두 코드 모두 `dispatch_status=unknown`이 아닙니다.

`begin`이 `operation_id`를 발급한 뒤, 도구 오류는 그 id를
`ErrorBody.operation_id`에 포함합니다. `begin` 전의 정책 / 잠금 /
키 충돌 거절은 포함하지 않습니다.

대기 중인 사용자 입력이 있는 `work_finish`는 **전송 실패가 아닙니다**.
`{ "closed": false, "reason": "pending_user_input" }`를 반환합니다.
