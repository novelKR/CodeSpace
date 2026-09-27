# PR #75 CS-RG suspension handoff

This note hands off the CodeSpace side of the 2026-09-27 session that suspended the planned CS-RG managed-execution design and revalidated the CodeSpace–DevGuard integration boundary. It is a dated record for agents and reviewers, not part of the documentation site, which avoids past-session narrative. It approves no architecture, starts no work and authorizes no merge. Re-query GitHub and git before acting.

- Living trackers: [#76](https://github.com/novelKR/CodeSpace/issues/76) (CodeSpace) and [novelKR/DevGuard#14](https://github.com/novelKR/DevGuard/issues/14) (primary, cross-repository).
- Full session record: [DevGuard #15](https://github.com/novelKR/DevGuard/pull/15), file `docs/handoff/2026-09-27-session-close.md`. It defines the terms used below (N1, N2, P1 to P12, K1 to K9).
- Reasoning: the DevGuard [revalidation analysis](https://github.com/novelKR/DevGuard/blob/1af1921fb72bb969b4f19ff859bd59fdb361ca55/docs/handoff/2026-09-27-cs-rg-boundary-revalidation-analysis.md) in DevGuard #13.

## Status at close (2026-09-27 15:05 UTC)

| Item | State |
| --- | --- |
| `main` | `794867ef52f530be6bc0d91aa10416d5195367b7`, the merge of #73. CI run 36308518615 and documentation run 36308519199 passed |
| Codex pin | `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` (rust-v0.154.0), unchanged |
| [#75](https://github.com/novelKR/CodeSpace/pull/75) | Hold notices, head `1bee230595698b0974df43561bccfce67d7e8cb9`. Open and CLEAN; 5 checks passed and 6 were not selected by the CI plan (runs 36317763003, 36317763516). Waits for the owner's exact-head merge approval |
| This note's PR | Open; see #76 for its number and head |
| Last scheduled full run | 36275459398 on `b6e7ed2`, before the ETXTBSY fix: failure. No scheduled run on `794867e` has been read yet |
| Kept branches | `codex/ci-fixture-docs-only` and `codex/ci-fixture-pty-only` (closed #71 and #72). `codex/diag-etxtbsy-fixture`, used only for diagnostic run 36302744023; never to be merged. All other remote task branches |

## What is suspended

The owner suspended DevGuard's CS-RG units C00, C03 and C09 as implementation directives. C00 is the managed PTY through DevGuard's `HelperCommand`. C03 is a runner-owned transport outside the Codex adapter. C09 is converging the legacy backends. No replacement is approved.

In CodeSpace, #75 marks the matching sections as suspended:

- `docs/codex-reuse.md`
- `docs/devguard-integration.md`
- `docs/execution-substrate.md`
- `docs/architecture.md`
- `docs/upstream-update.md`
- the Korean counterparts of these five pages

Until #75 merges, those sections on `main` still read like a plan to implement. Nobody implements from them.

## Constraints a CodeSpace agent follows

CodeSpace's product semantics, responsibility boundaries and selective Codex delegation (N1) are joint hard constraints with DevGuard's safety invariants (N2). DevGuard is the opt-in that adapts. For CS-RG this means:

- **Execution ownership**
  - No CodeSpace-owned PTY stack, Unix transport or process backend.
  - No governed PTY built on Codex `ProcessDriver`.
  - PTY stays delegated to `codex-utils-pty`.
- **Unaffected paths stay as they are**
  - No rewrite or move of the Tokio pipe path for DevGuard or for symmetry.
  - No wrapping of CodeSpace spawns in DevGuard's `spawn_guard`.
- **The `off` path**
  - No change to it.
  - No mandatory DevGuard service, credential or dependency.
- **Needs the owner's approval**: a Codex pin change, a DevGuard client dependency, an MCP contract change.
- **Allowed adapter scope**: a thin adapter only. That covers admission and identity binding, error and result translation, capability negotiation, outcome submission, the `resources=off|required` setting, and consuming generic backend events. It never takes over spawn, PTY, reaping or lifecycle to create an event.

## CodeSpace facts from the review

These are source facts at `794867e`; outside tests, the code is identical to `b6e7ed2`.

- **Spawns that DevGuard's guard does not cover.** Three spawn sites neither take DevGuard's `spawn_guard` nor close unintended descriptors in the child:
  - the pipe spawn ([`process.rs` L266-L300](https://github.com/novelKR/CodeSpace/blob/794867ef52f530be6bc0d91aa10416d5195367b7/crates/runner/src/process.rs#L266-L300));
  - the patch helper ([`patch_helper.rs` L50-L74](https://github.com/novelKR/CodeSpace/blob/794867ef52f530be6bc0d91aa10416d5195367b7/crates/runner/src/patch_helper.rs#L50-L74));
  - the Linux sandbox probe ([`linux_sandbox.rs` L60-L146](https://github.com/novelKR/CodeSpace/blob/794867ef52f530be6bc0d91aa10416d5195367b7/crates/runner/src/linux_sandbox.rs#L60-L146)).

  Codex PTY children do close those descriptors. This is how DevGuard's client session-socket window (D6) becomes reachable from CodeSpace processes. Exposure has not been demonstrated.
- **Both backends reap at once.** The pipe path polls `try_wait` every 20 ms ([`process.rs` L676-L698](https://github.com/novelKR/CodeSpace/blob/794867ef52f530be6bc0d91aa10416d5195367b7/crates/runner/src/process.rs#L676-L698)), and Codex PTY reaps inside `spawn_blocking(child.wait())`. No existing event fires before the reap. Under DevGuard's current contract on macOS, a survivor that was not adopted before the reap keeps its reservation charged until reboot.
- **Codex primitives.**
  - Codex exposes no PTY child PID at the pin or on its `main`.
  - Close-on-exec descriptor attachment (`ChildFds::Attached`, openai/codex#47797) exists only on Codex `main` and in prereleases, not in stable rust-v0.157.1.
  - No checked Codex revision offers exit observation before reap for PTY children.
- **Rust floor.** CodeSpace's floor is Rust 1.88 and DevGuard's is 1.95. Any DevGuard client dependency would have to stay optional so that the `off` build keeps 1.88.

## Unresolved CodeSpace-only issues

These are outside CS-RG. Each needs its own proposal and the owner's approval, and none is justified by CS-RG.

- **OX-01.** The MCP tool text says losing the worker connection or shutting down the gateway "terminates the owned subtree" ([`mcp.rs` L123-L125](https://github.com/novelKR/CodeSpace/blob/794867ef52f530be6bc0d91aa10416d5195367b7/crates/server/src/mcp.rs#L123-L125)). The pipe path, however, sets no process group.
- **OX-02.** Completed slots are evicted after `DEFAULT_COMPLETED_TTL`, which is 15 minutes ([`process.rs` L33](https://github.com/novelKR/CodeSpace/blob/794867ef52f530be6bc0d91aa10416d5195367b7/crates/runner/src/process.rs#L33), [L208-L211](https://github.com/novelKR/CodeSpace/blob/794867ef52f530be6bc0d91aa10416d5195367b7/crates/runner/src/process.rs#L208-L211)). The dropped Codex `ProcessHandle` then calls `terminate()`, which signals the stored numeric process group (`codex-rs/utils/pty/src/process.rs` L273-L277 at the pin). By then that group may have been reused.
- **OX-03.** If UDS worker setup fails before the handshake, the worker process and its directory are left behind ([`runtime.rs` L51-L64](https://github.com/novelKR/CodeSpace/blob/794867ef52f530be6bc0d91aa10416d5195367b7/crates/server/src/runtime.rs#L51-L64)).

None of these was run; they are source readings.

## Known intermittent CI failures

| Failure | Cause, at its evidence level | Status |
| --- | --- | --- |
| `linux_sandbox::tests::prepare_unread_large_stdin_times_out` (Rust / Integration) in scheduled run 36275459398 | **Suspected** ETXTBSY: a concurrently forked test child holds a write handle to a freshly written fixture script. A direct experiment showed the mechanism: 42 of 1600 starts failed with an in-process writer, none with a child-process writer. The failing run's holder was not captured | Fixed by #74 (`701e2b1`). **Not yet confirmed** by a scheduled run |
| `hashFiles('**/Cargo.lock')` template error (runs 36218741807 attempt 1, 35628255763) | The directory walk failed intermittently | Fixed by #67 (`339ae8e`) |

## Next steps and procedures

1. **Merge only on the owner's approval naming the exact head.**
   - Right before merging, re-check head and base.
   - Merge with `gh pr merge <n> --repo novelKR/CodeSpace --merge --match-head-commit <sha>`. Never use `--admin`, `--auto` or `--delete-branch`.
   - Afterwards, read the post-merge CI and the documentation publication once.
   - Preserve both in the DevGuard checkout's evidence directory, `<DEVGUARD_CHECKOUT>/evidence/codespace-delivery/`, with a manifest.
   - Only then remove the task worktree. Remote branches are kept.
2. **Read the first scheduled full run on `794867e` once.** It runs daily at `43 19 * * *` UTC; GitHub may start it hours late. If a fixture exec failure recurs, first classify it from the job history and the other legs: the change, a known flaky recurrence, or the environment. Then propose a fix. Apply, rerun or push nothing without confirmation.
3. **Do no CS-RG CodeSpace work before the owner's decisions** on the DevGuard architecture track (novelKR/DevGuard#14). Four choices are open:
   - D6 direction;
   - reap-first operability;
   - upstream proposals;
   - UDS mode timing.
4. **Changing this note triggers a full CI run.** A change under `.github/**` runs all legs by `scripts/ci-policy.json`; a `docs/**` change runs no Rust leg.

## References

- Hold notices: #75, novelKR/DevGuard#12.
- Revalidation record and analysis: novelKR/DevGuard#13.
- Session record: novelKR/DevGuard#15.
- Trackers: #76, novelKR/DevGuard#14.
- Earlier CodeSpace changes of the same day: #73 (CSP-D04), #74 (ETXTBSY fixture race).
