<a id="upstream-pin-update"></a>

# Updating Codex dependencies

[English](upstream-update.md) | [한국어](ko/upstream-update.md)

Update the pinned revision through a reviewed PR. The change can affect every adapter listed in [Codex reuse](codex-reuse.md), including process, filesystem, and network behavior.

<a id="checklist"></a>

## Review the candidate

Choose an explicit tag or commit and record the reason. Inspect relevant upstream changes in patch handling, PTY, hardening, filesystem, sandbox, and proxy code. Check runtime and development dependencies separately; the presence of a development-only dependency is not proof that it enters the product binary.

Update the submodule, [pin record](upstream-lock.md), affected adapter locks, and attribution when needed. Preserve the separation between core types and Codex adapter types. Never copy a single upstream crate into the product to conceal an incompatible dependency.

<a id="local-gate"></a>

## Validate before submission

1. Check the pin with `PIN_ONLY=1 ./scripts/check-upstream-pin.sh`.
2. Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, and tests for the root workspace and each isolated adapter: patch, codex-runtime, pty, file-system, linux-sandbox.
3. Build the patch, runtime, and Linux helper binaries; point integration tests at the intended binaries. Run workspace integration and protocol tests.
4. On Linux with bubblewrap and namespace support, run the sandbox isolation tests with `CODESPACE_REQUIRE_LINUX_SANDBOX=1`. Include restricted denial and enabled proxy behavior.
5. Check the dependency-policy scan and Runner's prohibited sandbox-helper library edges, as encoded in [CI](../.github/workflows/ci.yml).
6. Update behavior documentation and review both languages when defaults, errors, or limitations change.

A passing patch subset is insufficient for an update that also affects execution adapters. Keep command outputs tied to the candidate SHA and report unavailable platform checks explicitly. The CI workflow is the authoritative executable list of current gates.

<a id="forbidden"></a>

## Release and rollback

Open a PR with the old/new SHA, behavioral changes, and test evidence. Do not merge a failed compatibility gate or silently fall back to another patch engine. If the update must be reverted, revert the submodule, adapter locks, required Cargo patches, and documentation together; then rerun the affected gates. A pin mismatch is an error, not a warning.
