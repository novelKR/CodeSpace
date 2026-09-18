# Error codes and transport vs execution

[English](error-codes.md) | [한국어](ko/error-codes.md)

HTTP/JSON-RPC **request id**, `operation_id`, and `process_id` are three
different identifiers. A lost HTTP response is not an execution failure.
Clients call `operation_status` (W08) instead of replaying a mutating tool.

## Transport failures (no operation)

These never mint `operation_id` and must not be stored as operations:

| Signal | Meaning |
| --- | --- |
| TCP reset / client disconnect | Wire died. The process or patch may still be running. |
| HTTP 401 / 403 from Bearer middleware | Auth failed **before** any tool handler. |
| HTTP 404 at `/mcp` | Wrong path. |
| HTTP 408 / 502 / 503 / 504 | Transport or proxy. |

`codespace_domain::classify_http_status` maps these to
`FailureClass::Transport`. `TRANSPORT_FAILURE_IS_NOT_OPERATION` is true.

## Execution error codes (tool results)

Serialized as `SCREAMING_SNAKE_CASE` in JSON:

| Code | When |
| --- | --- |
| `UNAUTHORIZED` | Tool-layer refusal after a valid transport (not Bearer 401) |
| `WORKSPACE_NOT_FOUND` | Unknown `workspace_id` (W04) |
| `WORKSPACE_BUSY` | Write lock held by a live shell (W08 / W10) |
| `INVALID_PATCH` | Patch parsing/validation (W06 / W09); after-hash / delete-still-present / omitted `after_version`. Rollback filesystem I/O is not this code |
| `INVALID_COMMAND` | Command request is structurally invalid and was rejected before process dispatch |
| `PROCESS_SPAWN_FAILED` | Execution backend confirmed that no managed process was established |
| `PATH_ESCAPE` | Workspace/path containment violation: a `../` or absolute request, or a `find` walk result outside `workspace.root` |
| `FILE_NOT_FOUND` | Target path does not exist |
| `PATH_NOT_DIRECTORY` | A path component that must be a directory is a regular file (`ENOTDIR`) |
| `FILE_OPERATION_FAILED` | Containment held, but the filesystem operation itself failed (`find` root canonicalize included) |
| `SYMLINK_REJECTED` | Symlink file or ancestor |
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

Apply results use `status` (`applied`, `checked`, `rejected`,
`failed_rolled_back`, `failed_partial`, `unknown`). `checked` is a
successful `check_only` preview. `rejected` is an actual refusal. A
failed apply never reports `applied`. `applied` requires disk hashes to
match the helper's claimed `after_version`. Restart leaves unfinished
rows as `unknown` and does not auto-apply.

`exec_command` can return a successful result with
`dispatch_status=unknown` when spawn may have occurred. That is not a
transport error body. The returned `process_id` identifies the uncertain
attempt. Do not start a duplicate process. Use `read_process` or
`terminate_process` when the backend remains reachable; do not assume
unknown means the process did not start.

`INVALID_PATCH` is no longer used for exec command validation, confirmed
process-spawn failures, or rollback filesystem I/O.

`codespace-fs` `FsError` maps 1:1 onto product codes:
`NotFound` → `FILE_NOT_FOUND`, `NotDirectory` → `PATH_NOT_DIRECTORY`,
generic `Io` → `FILE_OPERATION_FAILED`, `SymlinkRejected` →
`SYMLINK_REJECTED`, `NotRegularFile` → `SPECIAL_FILE_REJECTED`.
`PATH_ESCAPE` is a workspace/path containment violation: a client
request that would leave scope (`../`, absolute), or a walk result that
fails `strip_prefix(workspace.root)`. `FILE_OPERATION_FAILED` means
containment held and the operation itself failed.

`INVALID_COMMAND` is a structurally invalid argv. The gateway rejects it
before minting a `process_id` or taking a mutation lease. The runner
repeats the same check. `PROCESS_SPAWN_FAILED` means the backend
**confirmed** that no managed process was established; the gateway
releases any lease. Neither code is `dispatch_status=unknown`.

After `begin` mints an `operation_id`, tool errors include that id on
`ErrorBody.operation_id`. Policy / lock / key-conflict refusals before
`begin` do not.

`work_finish` with pending user input is **not** a transport failure. It
returns `{ "closed": false, "reason": "pending_user_input" }`.
