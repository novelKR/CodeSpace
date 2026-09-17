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
| `INVALID_PATCH` | Codex parse failure (W06 / W09) |
| `PATH_ESCAPE` | `..` or absolute path outside the workspace |
| `SYMLINK_REJECTED` | Symlink file or escape |
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
transport error body. Keep the returned `process_id`; do not start a
duplicate process.

After `begin` mints an `operation_id`, tool errors include that id on
`ErrorBody.operation_id`. Policy / lock / key-conflict refusals before
`begin` do not.

`work_finish` with pending user input is **not** a transport failure. It
returns `{ "closed": false, "reason": "pending_user_input" }`.
