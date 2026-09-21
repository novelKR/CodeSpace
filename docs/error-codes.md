<a id="error-codes-and-transport-vs-execution"></a>

# Errors and uncertain results

[English](error-codes.md) | [한국어](ko/error-codes.md)

First distinguish a transport failure from a tool result. An HTTP or connection error does not tell you whether the requested mutation ran. Patch, process, and coordination states have separate identifiers.

<a id="transport-failures-no-operation"></a>

## Transport failures

Authentication rejection before a handler creates no operation. A disconnect or proxy error can occur after dispatch, so it does not prove that no operation exists. HTTP 401/403, 404, 408, 429, and 502/503/504 are classified as transport failures by the domain helper; inspect the actual request stage and recover recorded work where possible.

<a id="execution-error-codes-tool-results"></a>

## Tool error codes

Errors use uppercase identifiers and a message. An `operation_id` may be present if a patch record was created before the failure. An `approval_id` is present on confirmation-hold errors. Policy, occupancy, and key-conflict failures before a patch record do not have an operation id.

| Code | Meaning |
| --- | --- |
| `UNAUTHORIZED` | Action denied by tool policy |
| `WORKSPACE_NOT_FOUND` | Workspace ID is not registered |
| `WORKSPACE_BUSY` | Another mutation or live command owns the workspace |
| `INVALID_PATCH` | Patch parsing, preflight, or result verification failed; also retained in some helper/input error paths |
| `INVALID_COMMAND` | Malformed argv rejected before dispatch |
| `PROCESS_SPAWN_FAILED` | Backend confirmed no managed process was established |
| `PATH_ESCAPE` | Requested path leaves workspace scope |
| `FILE_NOT_FOUND` | Target path does not exist |
| `PATH_NOT_DIRECTORY` | A required directory component is not a directory |
| `FILE_OPERATION_FAILED` | Filesystem operation failed within the authorized scope |
| `SYMLINK_REJECTED` | A symlink path was rejected |
| `SPECIAL_FILE_REJECTED` | Device, socket, FIFO, or other non-regular target rejected |
| `ADD_FILE_EXISTS` | Add destination already exists |
| `MOVE_DESTINATION_EXISTS` | Move destination already exists |
| `VERSION_CONFLICT` | Current content does not match the expected version |
| `OPERATION_KEY_CONFLICT` | Same patch key was used with different arguments |
| `OPERATION_NOT_FOUND` | Unknown lookup ID/key, or not exactly one identifier supplied |
| `PROCESS_NOT_FOUND` | Process handle missing, expired, or stdin already closed on write |
| `PROCESS_NOT_TTY` | `process_resize` on a pipe-backed (`tty: false`) process |
| `PROCESS_NOT_RUNNING` | `process_resize` on a handle that exists but is not running |
| `OUTPUT_LIMIT` | Helper output exceeded its bound; also `read`/`find` when `limit` is 0 or above the advertised cap. Process reads instead discard old bytes |
| `TIMEOUT` | A managed command or helper exceeded its time limit |
| `WORK_NOT_FOUND` | Unknown logical work |
| `WORK_CLOSED` | Operation requires an open work |
| `INTENT_NOT_FOUND` | Unknown user instruction |
| `INTENT_ALREADY_CLAIMED` | Instruction has already been claimed |
| `INTENT_NOT_EDITABLE` | Instruction state does not permit editing |
| `INTENT_REVISION_CONFLICT` | Instruction revision changed |
| `QUEUE_NOT_EMPTY` | Reserved code; work_finish currently returns closed:false |
| `APPROVAL_REQUIRED` | Policy allowed the mutation; confirmation is required before execution. Includes `approval_id`. Not a privilege grant |
| `APPROVAL_NOT_FOUND` | Unknown confirmation-hold id |
| `APPROVAL_CONFLICT` | Hold is still pending, already decided, or a resume is already in progress |
| `APPROVAL_AMBIGUOUS` | Resume was interrupted and the terminal result is not on disk. Includes `approval_id`; patch cases may also include `operation_id` |

## Dispatch and completion

`dispatch_status: unknown` is a successful exec result describing an uncertain dispatch, not proof that no process started. Use the returned process handle if reachable and avoid duplicate launches. `confirmed` acknowledges dispatch; it does not mean command success.

Linux helper preparation/protocol/start errors occur before a managed process exists and use `PROCESS_SPAWN_FAILED`. Once the helper is running, plan load, inner sandbox, or proxy-start failure becomes process termination. Use `process_status` to observe that outcome; EOF is not success. On Linux sandbox the wait status belongs to the managed child (helper argv).

For patches, see the [status table and rollback limits](behavior-differences.md). For retries, timeouts, missing handles, and user-instruction completion, follow [Agent Loop integration](agent-integration.md). A `work_finish` response with `closed: false` and `reason: "pending_user_input"` is an application result, not a transport failure.
