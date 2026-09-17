# Behavior differences

Product policy is **not** “whatever `codex-apply-patch` does by
default.” `crates/patch` calls the original parser and apply functions
in-process, then the gateway/runner refuse operations the crate might
accept.

| Topic | Codex library / standalone default (0.154 candidate) | CodeSpace product |
| --- | --- | --- |
| Paths from the model | May accept host-absolute paths; resolve relative to process cwd | **Relative paths only**, resolved inside the registered workspace root |
| Symlinks | Apply options can follow / keep going | **Reject** symlink files and symlink-escape |
| Special files | Not a product gate | **Reject** devices, sockets, fifos |
| Add File | May interact with an existing path depending on hunks | **Reject** Add File if the destination already exists |
| Move destination | Engine may apply if the hunk says so | **Reject** if the move destination already exists |
| Newlines | Multiple modes exist; do not assume | **Preserve-newline mode preferred**; parity uses the same mode |
| `git apply` | Not the V4A engine; other products sometimes fall back | **No silent git-apply fallback** |
| Sandbox | Standalone `apply_patch` uses sandbox `None` | Patch crate is **not** the sandbox; gateway policy + PathSandbox today; Linux container is the **target** |
| Unified diff | Different tool elsewhere | Out of MVP (`git_apply_patch` later, never auto-convert) |
| Rollback | N/A in the crate | Snapshot restore of files; **no** `git reset --hard` |

## Patch request contract

```json
{
  "workspace_id": "demo",
  "patch": "*** Begin Patch\\n*** Update File: src/config.ts\\n...",
  "expected_versions": {
    "src/config.ts": "sha256:<from read>"
  },
  "operation_key": "change-timeout-001",
  "check_only": false
}
```

- `expected_versions` values are content versions from `read`, or
  `"absent"` for a file that must not exist yet. Move validates source
  and destination.
- `operation_key` is idempotency, not a capability token.
- `check_only: true` must leave every target file byte-identical and
  returns `status: "checked"`.
- Successful apply returns `files` (path list) plus `changes` with
  `path`, `before_version`, `after_version`, and `kind`
  (`add` / `update` / `delete` / `move`). `applied` means the helper
  claimed hash matches a fresh disk hash.

Result `status`: `applied` | `checked` | `rejected` |
`failed_rolled_back` | `failed_partial` | `unknown`.

## Write lock

A `workspace-write` shell is a mutating occupant. While it is live,
other mutating patch/exec work on that workspace is blocked
(`WORKSPACE_BUSY`) or waits per a documented queue. The product does not
pretend a shell cannot delete workspace files.

## Transport

stdio and Streamable HTTP expose the **same** tool schemas. Optional
static Bearer is HTTP experiment only and is **not** assumed to satisfy
ChatGPT Custom Connectors until a live account check says so.
