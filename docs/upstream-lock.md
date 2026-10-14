<a id="upstream-lock"></a>

# Pinned Codex revision

[English](upstream-lock.md) | [한국어](ko/upstream-lock.md)

All current Codex execution adapters use the Git submodule in `third_party/codex`. A fixed commit makes their shared implementation reproducible. It does not automatically update when upstream publishes a release.

<a id="deployment-pin-w06"></a>

## Deployment pin

| Field | Value |
| --- | --- |
| Project | [OpenAI Codex](https://github.com/openai/codex) |
| License | Apache-2.0; attribution in [NOTICE](../NOTICE) |
| Tag | `rust-v0.154.0` |
| Commit | `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` |
| Path | `third_party/codex` |
| Downstream patch | one, applied to the files of this commit: see [Downstream patch](#codex-downstream-patch) |
| Consumers | Patch, runtime worker, PTY, filesystem, Linux sandbox and proxy adapters |
| Patch options | `PreserveLineEndings`, `follow_symlinks: false` |

See [connected components](codex-reuse.md) for the exact adapter responsibilities. Patch tests cover selected parity cases, not the entire upstream suite or all Codex behavior.

<a id="codex-downstream-patch"></a>

## Downstream patch

CodeSpace carries one patch to `codex-utils-pty`, under the [policy for changing Codex at a pin](upstream-patches.md). The submodule stays on `openai/codex` at the commit above, and the patch is applied to its files; every other Codex crate is identical to the tag. `scripts/codex_patch.py` reads the patch file, its digest and the patched tree from this table.

| Field | Value |
| --- | --- |
| Patch | `third_party/codex-patches/0001-utils-pty-pre-reap-observation.patch`, SHA-256 `6f61579830f311e3c259c0642864ea9eee3c653f12c7e838e97e9b1dd35b1fba` |
| Patched tree | `1999937194e3a68d982be85679557f0e50fe5408` |
| Files | `observe.rs` (new), `observe_tests.rs` (new, tests only), `pty.rs` and `lib.rs` in `codex-rs/utils/pty/src`: 304 product lines added and 5 removed, and 9 tests |
| Capability | `spawn_pty_process_observed` with an `observe::ExitObserver` registered before the spawn. The child's single waiter sees the exit without reaping it (`waitid` with `WNOWAIT`), gives the observer one opportunity for at most the observer's bound, then reaps it as before and reports `Completed`, `TimedOut`, `Failed` or `Unavailable`. Without an observer every path is unchanged. Platforms other than Unix refuse an observed spawn |
| Reason | observing a governed PTY execution's scope before Codex reaps its root ([CSRG-U5](devguard-integration.md#devguard-lifecycle)), as the owner chose on 2026-10-11; the contract is section 8 of the [upstream adapter work specification](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-28-cs-dg-upstream-adapter-work-spec-1.md#8-preferred-investigation-b-an-upstream-owned-bounded-pre-reap-observation-contract) |
| Use in CodeSpace | governed PTY executions ([CSRG-U5](devguard-integration.md#devguard-lifecycle)): the PTY adapter's `spawn_observed` registers an observer with CodeSpace's observation bound (2 s), and its window carries DevGuard's `Observe` before the reap. Ungoverned PTY executions still call `spawn_pty_process` |
| Provenance | ported from the observer of W3 experiment B ([DevGuard decision packet, section 3.2](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-28-w3-decision-packet.md#32-experiment-b-backend-owned-pre-reap-observation-packet-132)) to both Unix PTY paths of this release. Each modified file carries a CodeSpace modification notice (Apache-2.0 section 4(b)) |
| Verification | on the patched tree: Linux x86_64, the crate's 36 tests in each of 30 runs; macOS, its 38 tests in each of 30 runs and W3 experiment B's cases B1-B13 against DevGuard `6e7e065`. Clippy is clean for Linux, macOS and Windows targets |
| Upstream | Codex accepts no external pull requests. Feature request [openai/codex#53298](https://github.com/openai/codex/issues/53298) was filed on 2026-10-12 with the owner's approval and closed by the owner on 2026-10-14 as not planned, because only CodeSpace needs this capability ([upstream first](upstream-patches.md#patch-upstream)). No upstream request is open |
| Removal | drop the patch when a Codex release offers an equivalent observation (one waiter, a bound, an outcome) and CodeSpace moves to it, or when CodeSpace no longer observes PTY executions before the reap |
| Pin changes | revert the patch, move the submodule, then port the patch to the candidate or drop it under the removal condition; rerun its tests on Linux and macOS and update this record |
| Rollback | revert the patch file and this record together, after reverting any CodeSpace use of the observer |

<a id="devguard-client-pin"></a>

## DevGuard client pin

The opt-in `devguard` features of the gateway ([CSRG-U1](devguard-integration.md#devguard-status-connection)) and the UDS worker ([CSRG-U2](devguard-integration.md#devguard-owner-registration)) link DevGuard's generic client from one reviewed, immutable commit.

| Field | Value |
| --- | --- |
| Project | [DevGuard](https://github.com/novelKR/DevGuard) |
| License | Apache-2.0, with the same copyright holder as CodeSpace |
| Commit | `6e7e065a0dd5b3876dc7f4546ab76069489937d1`, DevGuard `main` after [DevGuard PR #27](https://github.com/novelKR/DevGuard/pull/27) |
| Source | Cargo Git dependency at that `rev` |
| Crates | `devguard-client` and `devguard-contract` in the product; `devguard-daemon` with `test-fixtures` only in the adapter's tests |
| Launch helper | `devguard-launch` is never linked. Operators name the helper of the DevGuard release that serves the socket (`--devguard-launch-helper`, [CSRG-U4](devguard-integration.md#devguard-launch)); the launch tests build it from this commit with DevGuard's own lockfile |
| Consumer | `crates/devguard` (`codespace-devguard`), linked by `codespace-server`, `codespace-runner` and `codespace-codex-runtime` only with their `devguard` features |
| Check | `scripts/upstream_dependencies.py` (`DEVGUARD_SOURCE`) |

In every product graph, the dependency check accepts each `devguard-*` package only from this commit and each `codex-*` package only from the Codex gitlink. It admits only `devguard-client` and `devguard-contract` to a product graph, so DevGuard's test fixtures stay development dependencies. It rejects any DevGuard package in a graph built without the features, and any CodeSpace or Codex package that DevGuard's crates reach; the root and worker graphs are checked with their features on as well as off. Change the pin through a reviewed PR that updates the adapter manifest, the three lockfiles that hold it (root, adapter and worker), `DEVGUARD_SOURCE` and this record together.

<a id="why-file-copy-vendor-is-forbidden"></a>
<a id="what-is-reused-vs-rejected"></a>

## Workspace and lockfiles

Adapters have isolated Cargo workspaces so upstream workspace dependencies remain usable without copying and maintaining a forked patch parser. Preserve adapter lockfiles and required upstream Cargo patches. The filesystem and sandbox adapters pin matching Rama alpha dependencies to avoid resolving an incompatible mixture of alpha and stable releases.

The patch adapter calls the library inside `codespace-patch`; it does not invoke upstream's standalone `apply_patch` executable. `LOCAL_FS` and path utilities are implementation dependencies. Codex user settings do not grant workspace access.

<a id="promotion-rule"></a>

## Check and update

```bash
python3 scripts/codex_patch.py apply
PIN_ONLY=1 ./scripts/check-upstream-pin.sh
```

Apply the [recorded patch](#codex-downstream-patch) after each clone or submodule update, before building; `scripts/validate-upstream.py` applies it before every validation. The pin check verifies only the submodule revision against this document and that the submodule's files are exactly the recorded patched tree. Without `PIN_ONLY`, the script also runs patch tests; it does not replace the other adapter gates. Run `python3 scripts/codex_patch.py revert` before moving the submodule, and use the [complete update procedure](upstream-update.md) before changing the pin. Do not use `git submodule update --remote` as a deployment step.
