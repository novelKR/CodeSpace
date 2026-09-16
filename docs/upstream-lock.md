# Upstream lock

CodeSpace reuses OpenAI Codex **only** as a pinned Rust library for
parse / verify / apply of the V4A patch format. It does not vendor a
single source file, wrap the standalone `apply_patch` binary as the
security boundary, or follow `main`.

## Deployment pin (W06)

| Field | Value |
| --- | --- |
| Project | [openai/codex](https://github.com/openai/codex) |
| License | Apache-2.0 (see root `NOTICE`) |
| Tag | `rust-v0.154.0` |
| Commit | `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` |
| Path | `third_party/codex` git submodule |
| Crate | `codex-apply-patch` via Cargo path dependency from `crates/patch` |
| Apply options | `PreserveLineEndings`, `follow_symlinks: false` |
| Parity | subset in `tests/parity/` and `crates/patch` tests; not the full upstream suite |

`crates/patch` is an **isolated Cargo workspace** (excluded from the repo
root workspace) so Codex crates keep their own `workspace.dependencies`.
Its `Cargo.lock` starts from the pinned Codex lockfile so transitive
crates (for example matching `rama-*` alphas) do not float. Codex
`[patch.crates-io]` git forks are copied into `crates/patch/Cargo.toml`.

The adapter calls `parse_patch`, then product path policy (including
symlink-ancestor rejection), then `apply_patch_with_options` in the same
process with `LOCAL_FS`. It does not invoke `git apply` or the standalone
`apply_patch` binary. Passing `sandbox: None` to the library is **not**
the product sandbox; policy + no-follow I/O + the Linux runner are.

On macOS, `/var` is a symlink to `/private/var`. The adapter canonicalizes
the workspace root before building the `PathUri` cwd so no-follow walks
do not fail on that host alias.

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

and a path dependency, not a crates.io moving version.

## What is reused vs rejected

Reused: parse, hunk verification, apply APIs, and selected upstream
fixtures for parity.

Rejected as product defaults even if the crate allows them: symlink
follow, sandbox `None` standalone CLI, host-absolute paths from the
model, silent `git apply`.

Do not wrap `codex-rs` standalone `apply_patch` and call that a sandbox.
Preview / `check_only` is implemented through library parse plus
CodeSpace preflight, not by assuming `apply_patch --check` exists.

## Promotion rule

1. Record the candidate (W01).
2. W06: submodule + adapter + parity subset (this pin).
3. If parity fails, **do not ship**. Change adapter options or pick
   another revision; do not paper over mismatches.
4. W13: pin-update procedure. Never `git submodule update` to latest
   `main` as a deploy step.
