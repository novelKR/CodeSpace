# Upstream lock

CodeSpace reuses OpenAI Codex **only** as a pinned Rust library for
parse / verify / apply of the V4A patch format. It does not vendor a
single source file, wrap the standalone `apply_patch` binary as the
security boundary, or follow `main`.

## Candidate pin (not yet a deployment pin)

| Field | Value |
| --- | --- |
| Project | [openai/codex](https://github.com/openai/codex) |
| License | Apache-2.0 (see root `NOTICE`) |
| Tag | `rust-v0.154.0` |
| Commit | `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` |
| Intended path | `third_party/codex` git submodule |
| Crate | `codex-apply-patch` via Cargo path dependency from `crates/patch` |

This commit is a **candidate**. It becomes a deployment pin only after
W06 parity tests pass against the original crate with the **same apply
options** CodeSpace will ship (newline preserve preferred).

W01 does **not** add the submodule. Adding it is W06 unless an
implementation PR needs it to compile the patch crate.

## Why file-copy vendor is forbidden

`codex-apply-patch` 0.154.0 is a workspace crate. It depends on other
crates in the same repo, including:

- `codex-exec-server`
- `codex-utils-absolute-path`
- `codex-utils-path-uri`
- tree-sitter related workspace crates

Copying `apply-patch` sources into `crates/patch` would either fail to
build or quietly fork the engine. CodeSpace therefore uses:

```text
git submodule add https://github.com/openai/codex.git third_party/codex
git -C third_party/codex checkout 6b9826e3aa83b1a5947db50f4332cb9c65f1b340
```

and a path dependency, not a crates.io moving version, for
`crates/patch`. The adapter calls `parse_patch` then product policy then
`apply_patch_with_options` in the same process.

## What is reused vs rejected

Reused: parse, hunk verification, apply APIs, and selected upstream
fixtures for parity.

Rejected as product defaults even if the crate allows them: symlink
follow, sandbox `None` standalone CLI, host-absolute paths from the
model, silent `git apply`.

Do not wrap `codex-rs` standalone `apply_patch` and call that a sandbox.
Preview / `check_only` is implemented through library verify plus
CodeSpace preflight, not by assuming `apply_patch --check` exists.

## Promotion rule

1. Record the candidate (this file).
2. W06: submodule + adapter + parity subset.
3. If parity fails, **do not ship**. Change adapter options or pick
   another revision; do not paper over mismatches.
4. W13: pin-update procedure. Never `git submodule update` to latest
   `main` as a deploy step.
