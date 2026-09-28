# DevGuard upstream-adapter packet (2026-09-28)

This note points CodeSpace agents and reviewers to DevGuard's W0–W2 packet for CS-DG-UPSTREAM-ADAPTER-WORK-SPEC-1 v1.0. It is a dated, non-normative record, not part of the documentation site. It approves no architecture, dependency, pin, experiment or merge, and it changes no CodeSpace behaviour. Re-query GitHub and git before acting.

- Living trackers: [#76](https://github.com/novelKR/CodeSpace/issues/76) (CodeSpace) and [novelKR/DevGuard#14](https://github.com/novelKR/DevGuard/issues/14) (primary, cross-repository).
- The packet: [novelKR/DevGuard#16](https://github.com/novelKR/DevGuard/pull/16), file [`docs/handoff/2026-09-28-upstream-adapter-packet.md`](https://github.com/novelKR/DevGuard/blob/a67facbbebb192d83126c5afa1a9ef2d713c2e2e/docs/handoff/2026-09-28-upstream-adapter-packet.md) at head `a67facbbebb192d83126c5afa1a9ef2d713c2e2e`. The same PR carries the specification and its start instruction.
- The owner's policy direction, recorded on both trackers: DevGuard may use Codex and other external dependencies behind adapters, with reviewed, explicit upstream pins. CodeSpace's own pin and review process is unchanged.

## Unchanged in CodeSpace

- The Codex pin: `6b9826e3aa83b1a5947db50f4332cb9c65f1b340` (rust-v0.154.0), `docs/upstream-lock.md` and the gitlink.
- Every documentation page, including the CS-RG hold notices from #75. The CS-RG suspension stays in force.
- Manifests, locks, scripts, CI policy and the Codex adapters.
- The governance-free `off` path: it needs no DevGuard dependency, service or credential.

## Findings that concern CodeSpace

These are summaries; the packet has the citations and evidence levels.

- **Upstream PTY API (packet section 7).** `ChildFds::Attached` exists at rust-v0.159.0-alpha.11 (`72b8d8b`) and Codex `main`, not in stable rust-v0.157.1. From that revision on, the last parameter of `spawn_pty_process` is `ChildFds<'_>`. A pin move past stable would therefore need a coordinated edit of `crates/pty/src/lib.rs:66-77` in the reviewed pin PR, for example `ChildFds::Inherited(&[])`, which keeps today's behaviour. No pin change is proposed.
- **Descriptor inheritance on macOS (sections 8 and 11).**
  - Rust std and tokio do not create pipes and sockets close-on-exec atomically on macOS, and std `Command` does not use `POSIX_SPAWN_CLOEXEC_DEFAULT`.
  - The BD-1 diagnostic exercised `codex-utils-pty` at `72b8d8b` in a scratch crate outside both repositories. In it, unrelated std and tokio children received a transient PTY master in 104 of 2000 cases and pipe ends in 39 of 2000, from other threads' creation windows.
  - By inference, the same applies to CodeSpace's Tokio pipe path, patch helper and sandbox probe, which spawn without child-side exclusion. The effect on CodeSpace, such as a delayed EOF, was not measured.
  - An independent CodeSpace descriptor-hygiene proposal is an optional owner decision (packet section 14, decision 9). CS-RG does not justify it.
- **Dependency gates (section 6).**
  - `scripts/check-no-model-deps.sh` checks only direct `codex-*` keys in core manifests.
  - `scripts/upstream_dependencies.py` rejects named crates only: the four `FORBIDDEN` names at any depth from every product root, plus `RUNNER_FORBIDDEN` from the Runner.
  - So a transitive `codex-utils-pty` arriving through a DevGuard client would not be flagged today.
  - The packet drafts an executable single-identity and transitive check for the PR in which a DevGuard crate first enters a CodeSpace graph. Nothing is applied.
- **Pipe observation (section 9.2).** Tokio 1.53.1 offers no non-reaping exit observation. The packet identifies no thin, backend-owned pipe facility. A peek added to CodeSpace's own wait loop would conflict with the specification (its section 8.1) unless the owner rules it a generic capability of the existing backend; the packet does not recommend it.

## Wording the packet drafts (not applied)

| Page, lines at `326bdcb` | Current point | Draft direction |
| --- | --- | --- |
| `docs/codex-reuse.md:78` (ko `:96`) | "D3, DevGuard. No Codex dependency is added to DevGuard now…" | D3 superseded: DevGuard may consume Codex behind declared adapters with reviewed pins; CodeSpace's generic resource client imports no Codex types; CodeSpace's pin process unchanged |
| `docs/upstream-update.md:90-131` (ko `:91-129`), under the hold notice | "The planned resource client must bring no transitive Codex dependency…", documentation only (`:94`) | kept, plus: a DevGuard binding linked into a CodeSpace executable consumes Codex through this repository's gitlink, with an executable single-identity check |

Any change to these pages is a separate, reviewed CodeSpace PR with its Korean pages and registry hashes (packet decision 7).

## Status at writing (2026-09-28, about 04:20 UTC)

| Item | State |
| --- | --- |
| `main` | `326bdcb181f62f61bcb3c4e9c3de7535d50b0232`, the merge of #77. CI 36336337315 and documentation 36336337674 passed. Scheduled run 36355653598 on the same commit passed, with no ETXTBSY recurrence in that run |
| novelKR/DevGuard#16 | Open at head `a67facbbebb192d83126c5afa1a9ef2d713c2e2e`; waits for the owner's exact-head decision |
| This note's PR | Open; see #76 for its number and head |
| novelKR/DevGuard#13 | Open at `edf5e2f20f88feed55822a078abffc18af5f9a4e`; a separate decision |

## Next step and stop boundary

The next step is the owner's decisions in packet section 14: merges at exact heads, the bounded experiments A, B and C, and normative reconciliation. Until then there is no merge, no experiment, no normative PR, no pin, dependency, service or credential change, and no upstream submission.

If the packet changes before this note merges, this note is updated to the new head first.
