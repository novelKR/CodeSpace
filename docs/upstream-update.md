<a id="upstream-pin-update"></a>

# Updating Codex dependencies

[English](upstream-update.md) | [한국어](ko/upstream-update.md)

Update the pinned revision through a reviewed PR. The change can affect every adapter listed in [Codex reuse](codex-reuse.md), including process, filesystem, and network behavior.

<a id="checklist"></a>

## Review the candidate

Choose an explicit tag or commit and record the reason. Inspect relevant upstream changes in patch handling, PTY, hardening, filesystem, sandbox, and proxy code. Check runtime and development dependencies separately; the presence of a development-only dependency is not proof that it enters the product binary.

Update the submodule, [pin record](upstream-lock.md), affected adapter locks, and attribution when needed. Preserve the separation between core types and Codex adapter types. Never copy a single upstream crate into the product to conceal an incompatible dependency.

No limited adaptation of upstream code exists today. If one is added under the [adaptation policy](codex-reuse.md), review it on every pin change: compare it with the candidate's upstream source against its recorded re-examination condition, update its recorded provenance and divergence, rerun its tests, and remove it or reconverge with upstream when its removal condition holds. Adopting a newer upstream API needs this separate pin update; it is never a side effect of another change.

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

## Boundaries for future crates and backends

CS-RG plans a DevGuard resource client crate and new execution backends
([CS-RG work packages](https://github.com/novelKR/DevGuard/blob/d4981b4c241cff42687f5c2c681b583c7847776e/docs/planning/milestones/CS-RG.md),
CSRG-C01 and CSRG-C03). None exists yet, so the dependency and CI policies name
no product root or component for them. The rules below apply when the PR that
adds one is opened; they are documentation policy, not an executable gate.

The current checks already enforce these boundaries:

- `scripts/upstream_dependencies.py` inspects the root workspace and each
  workspace in `ADAPTERS`. It fails when a workspace's members differ from its
  `PRODUCTS` entry ("missing or unexpected product roots"). `FORBIDDEN`
  (`codex-core`, `codex-exec`, `codex-app-server`, `codex-login`) is rejected
  from every product root, and `RUNNER_FORBIDDEN` (`codespace-linux-sandbox`,
  `codex-linux-sandbox`) from the Runner.
- `scripts/ci-policy.json` `components` must equal the `crates/` directories
  that contain a `Cargo.toml` (`test_components_are_the_crates`); every tracked
  path must be classified, and each leg's `compiles` list must match the crates
  its stages build (`scripts/tests/test_ci_plan.py`).
- `scripts/check-no-model-deps.sh` checks each existing adapter's direct Codex
  keys against that adapter's own allowlist.

The `full` list in `scripts/ci-policy.json` names `Cargo.toml`, `Cargo.lock`,
`**/Cargo.toml`, `**/Cargo.lock`, `third_party/**`, `.gitmodules`,
`.github/**`, `scripts/**` and `docs/upstream-lock.md`, plus toolchain,
`.cargo` and `.gitignore` files. Changes to manifests and lockfiles, the Codex submodule, the
Codex pin record, CI files and the policy scripts therefore already run every
leg. New execution-contract, spawn-guard or backend code inside an existing
crate is covered by that crate's component, for example `crates/runner/**`.
A PR that adds a crate runs every leg because it adds a manifest, but later
changes to that crate's sources select legs only through its component. Where
the DevGuard client pin and helper provenance will be recorded is not decided
yet (CSRG-C01), so no existing trigger is claimed to cover that record; the PR
that creates it adds it to `full` or to a component.

The PR that adds a crate, backend or test workspace updates these together:
its manifest and lockfile; `PRODUCTS` (and `ADAPTERS` for an isolated
workspace); the adapter allowlist in `check-no-model-deps.sh` when it takes
direct Codex keys; the `ci-policy.json` component and the `compiles` list of
every leg that builds it; and the tests that cover it. It never removes or
narrows `FORBIDDEN`, `RUNNER_FORBIDDEN`, full-graph validation or any other
check. The planned resource client must bring no transitive Codex dependency,
and DevGuard client types stay out of public MCP types.
