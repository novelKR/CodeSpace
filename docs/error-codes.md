# Error codes and transport vs execution

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
| `OPERATION_NOT_FOUND` | Unknown `operation_id` |
| `PROCESS_NOT_FOUND` | Unknown `process_id` |
| `OUTPUT_LIMIT` | Reserved; live `read_process` drops oldest bytes instead of storing unbounded output |
| `TIMEOUT` | Managed process time limit (default 30s; `CODESPACE_PROCESS_TIMEOUT_SECS`) |
| `CHECK_ONLY_CONFLICT` | `check_only` would not be a no-op |

Apply results use `status` (`applied`, `rejected`, `failed_rolled_back`,
`failed_partial`, `unknown`). A failed apply never reports `applied`.
Restart leaves unfinished rows as `unknown` and does not auto-apply.
