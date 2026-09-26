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

## Reproducible validation reports

Run `python3 scripts/validate-upstream.py all` for the complete local sequence.
CI calls the same named stages in parallel; use `--help` to list them.
Reports and command logs default to `target/upstream-reports/local`.
Choose a different ignored directory with `--output` for each attempt; preserve
previous reports before repeating a run. Reports record the source HEAD, Codex
SHA, Rust host, and source/lockfile hashes. A changed input invalidates the run.
`passed` applies only to the listed stages, not to all release gates.
On macOS, Linux isolation is explicitly `not_run` and the overall result is
`incomplete`; Linux CI evidence is still required. macOS CI additionally checks
PTY and filesystem contracts. Neither result alone replaces the other platform.

CI does not run every job for every change. A planning job selects the legs a
change needs from `scripts/ci-policy.json`: a crate change runs every leg that
compiles that crate, documentation runs none, and build inputs, CI files or
unknown paths run everything. The policy, Python, pin and format stages always
run. The required `rust` job passes only when every planned leg succeeded with
a report for the tested commit and every other leg was skipped. Daily scheduled
and manual runs are full. Preview a plan with
`python3 scripts/ci_plan.py --base <revision>`. CI builds without incremental
data or debuginfo, and each job restores a dependency cache that only `main`
saves.

The dependency stage uses locked, target-filtered Cargo metadata and follows
normal/build edges from product roots, excluding development edges. It rejects
agent/product crates and Runner-to-sandbox-library edges with a dependency path.
This checks package reachability, not whether a binary executes every linked API.
Cargo's resolved feature unification can conservatively include optional edges.

Generate a candidate report and compare it with an archived report for the same
Rust target (the baseline is never updated automatically):

```bash
python3 scripts/upstream_dependencies.py --target x86_64-unknown-linux-gnu \
  --compare target/baseline/dependencies.json \
  --output target/candidate/dependencies.json
```

The comparison lists added/removed package identities and edges. A version or
source change appears as removal plus addition. Ordinary changes require review;
forbidden dependencies, malformed metadata, missing roots, or Cargo failure fail
the gate. Source paths are repository-relative, never machine-specific identities.
CI uploads reports/logs even on failed validation. The legacy pin script still
checks only SHA and patch tests; it is not a complete qualification command.

<!-- CI fixture: documentation-only selection; not for merging. -->
