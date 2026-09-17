# Upstream pin update (W13)

Changing the Codex revision is a **deliberate release**, not `git
submodule update --remote` to `main`. If the parity subset fails, **do
not ship**. Do not copy `apply-patch` sources into `crates/patch` to
paper over a workspace dependency.

Current pin: [upstream-lock.md](upstream-lock.md).
Product vs crate defaults: [behavior-differences.md](behavior-differences.md).
What may be reused besides apply-patch: [codex-reuse.md](codex-reuse.md).
NOTICE must keep the Apache-2.0 Codex attribution.

## Checklist

1. Choose a **tag or commit** (not floating `main`). Record why.
2. `git submodule update --init third_party/codex`
3. `git -C third_party/codex fetch --tags`
4. `git -C third_party/codex checkout <commit>`
5. Rebuild the isolated adapter:
   `cargo test --manifest-path crates/patch/Cargo.toml`
   `cargo clippy --manifest-path crates/patch/Cargo.toml --all-targets -- -D warnings`
6. Run `scripts/check-upstream-pin.sh` after updating the Commit cell in
   `docs/upstream-lock.md` to the new SHA (the script fails if submodule
   HEAD ≠ lock file).
7. Update [behavior-differences.md](behavior-differences.md) if apply
   options, symlink policy, or parse errors changed.
8. Update [NOTICE](../NOTICE) if the reuse description or pin string
   changed.
9. Judge the pin’s **execution subgraph** against
   [codex-reuse.md](codex-reuse.md): cohesive execution vs agent /
   model semantics vs Gateway allow bypass. Treat diffs in
   process-hardening, PTY, UDS, filesystem, linux-sandbox, and
   network-proxy as an execution/security changelog. Update the
   candidate table. Do not add a Codex path dep to the root
   workspace. Isolation stays in `crates/patch` and, when it exists,
   `crates/codex-runtime` (`codespace-codex-runtime`).
10. Until that runtime workspace exists, the gate is SHA + patch
    parity only. When it exists, also require: runtime adapter
    compile, plus PTY / sandbox / process regressions. This work
    package does not add that suite.
11. Open a PR. CI must run the pin check **and** `crates/patch` tests.
    A red patch job is a failed deploy, not a warning.

There is **no** path that marks a failed parity run as success.

## Local gate

```bash
./scripts/check-upstream-pin.sh
```

Exit non-zero if the submodule SHA mismatches the lock file or if
`cargo test --manifest-path crates/patch/Cargo.toml` fails.

`PIN_ONLY=1 ./scripts/check-upstream-pin.sh` checks the SHA only
(used by CI before the existing full test step).

## Forbidden

- File-copy vendor of `codex-rs/apply-patch` without its workspace
  crates
- Wrapping the standalone `apply_patch` binary as the security boundary
- Silent `git apply` fallback
- Shipping when patch tests fail
