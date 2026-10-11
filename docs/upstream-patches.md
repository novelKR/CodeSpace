<a id="upstream-patches"></a>

# Changing Codex at a pin

[English](upstream-patches.md) | [한국어](ko/upstream-patches.md)

CodeSpace runs Codex code from one [pinned release](upstream-lock.md). This page decides what CodeSpace does when that release cannot do what CodeSpace needs, and, when the answer is a change to Codex's code, how the change is made: as a recorded patch on the official release, never as a fork. The [earlier cases](#earlier-cases) show how CodeSpace handled such gaps before; the policy follows from them so that later decisions take the same path. The patch in force is in the [pin record](upstream-lock.md#codex-downstream-patch).

<a id="patch-policy"></a>

## Policy

<a id="when-to-patch"></a>

### When a patch is the answer

Weigh the options in this order, and record why each earlier one does not serve:

1. the pinned release's public API, with the gap handled on CodeSpace's side of an adapter;
2. a Codex release that already has the capability, taken through a reviewed [pin update](upstream-update.md);
3. a recorded patch to the pinned release, under this policy;
4. a CodeSpace-owned replacement for the Codex mechanism.

While the work can wait, the gap can also stay as a documented limitation that a test pins.

A patch is chosen only when the pinned release blocks work that CodeSpace has decided to do now, an adapter cannot close the gap, and moving the pin is not justified for that work alone. It is limited to execution mechanisms such as process, PTY, terminal, descriptor and I/O helpers. It never changes `codex-core` product semantics, session authority or the agent loop, and never hides a dependency from the checks.

A patch comes before a CodeSpace-owned replacement because CodeSpace stays a thin adapter over the mechanisms it delegates to Codex: the DevGuard integration alone never justifies moving one of them, notably the PTY, into CodeSpace ([boundary review](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-27-cs-rg-boundary-revalidation.md#1-why-this-review-happened), section 1). [Codex reuse](codex-reuse.md#reuse-decisions-for-managed-execution) recorded the same order for managed execution; that section is suspended as an implementation directive, so this page states the order for every change at the pin.

<a id="patch-form"></a>

### Form

- The submodule stays on `openai/codex` at the official release commit. The gitlink never names a modified commit, and CodeSpace keeps no fork of Codex.
- Each patch is one `git format-patch` file in `third_party/codex-patches/`, made against the pinned commit and numbered in the order it applies.
- `scripts/codex_patch.py` applies the patches to the submodule's files, and the result must be the recorded patched tree. The pin check and `scripts/validate-upstream.py` refuse a missing patch or any other change in the checkout.
- No Codex crate is copied into CodeSpace, and no Cargo `[patch]` entry points a Codex crate at a modified copy. The `[patch]` entries copied from Codex's own workspace, which build third-party crates from OpenAI's forks, are not CodeSpace changes and stay as they are.
- A patch keeps upstream behavior unless a caller opts in. It carries a modification notice in each file it changes (Apache-2.0 section 4(b)) and adds tests to the patched crate that run on Linux and macOS.

<a id="patch-record"></a>

### Record

For each patch, the [pin record](upstream-lock.md#codex-downstream-patch) holds the file and its SHA-256, the patched tree, the files changed, the capability, the reason, its use in CodeSpace, provenance, verification on Linux and macOS, upstream status, the removal condition, what a pin change does with it, and rollback. The record and the patch change in the same PR. `scripts/codex_patch.py` reads the record, so the two cannot drift apart.

<a id="patch-upstream"></a>

### Upstream first

Codex takes code changes only from its own team and asks for feature requests in its issue tracker ([contributing](https://github.com/openai/codex/blob/rust-v0.154.0/docs/contributing.md)). Each patch's capability is proposed there as a feature request, with the patch as a reference. Submitting the request is the owner's decision. Its status stays in the record, and the patch is removed once a Codex release offers an equivalent and CodeSpace moves to it.

<a id="patch-pin-changes"></a>

### Pin changes

A pin update reverts the patches (`python3 scripts/codex_patch.py revert`) and moves the submodule. In the same PR it ports each patch to the candidate or drops it under its removal condition, reruns the patch's tests and every affected adapter on Linux and macOS, and updates the record. A patch never moves the pin by itself.

<a id="patch-approval"></a>

### Approval

Adding, changing or removing a patch is a reviewed CodeSpace PR, merged at its exact head with the owner's approval, like a pin change.

<a id="earlier-cases"></a>

## Earlier cases

From the first pin in September 2026 to the PTY observer in October 2026, the pinned release often lacked something an adapter needed. Until the PTY observer, every case was settled without changing Codex source and without moving the pin. The cases fall into six groups. Each link leads to the issue or PR that recorded the decision.

<a id="case-taking-code"></a>

### Taking Codex code

- **Gap.** `codex-apply-patch` depends on other crates of the Codex workspace, such as `codex-exec-server`, the path utilities and tree-sitter, so it cannot be copied out on its own.
- **Weighed.** Copying a single crate was out of scope from the start ([#1](https://github.com/novelKR/CodeSpace/issues/1)). [#25](https://github.com/novelKR/CodeSpace/pull/25) also ruled out wrapping Codex's standalone `apply_patch` binary, reimplementing the patch format and falling back to `git apply`.
- **Chosen.** The `third_party/codex` submodule at `rust-v0.154.0` ([#25](https://github.com/novelKR/CodeSpace/pull/25)). The pin check rejects any other commit, and `git submodule update --remote` is never a deployment step.
- **Later releases.** `rust-v0.157.1`, `rust-v0.159.0-alpha.11` and stable `rust-v0.159.0` to `rust-v0.159.2` each offered something CodeSpace could use, such as Codex's descriptor policy or `ChildFds`. None was taken as part of other work ([#76](https://github.com/novelKR/CodeSpace/issues/76)). The owner's planning direction of 2026-09-30 keeps `rust-v0.154.0` until a separate, qualified pin PR moves CodeSpace to a reviewed `rust-v0.159.x`, with the PTY adapter's `ChildFds` edit in that PR ([DevGuard #14](https://github.com/novelKR/DevGuard/issues/14#issuecomment-5914489840)).

<a id="case-workspace"></a>

### Building inside Codex's workspace

- **Gap.** The pinned crates expect Codex's own Cargo workspace: its shared dependency versions, and `[patch]` entries that build `tungstenite` and `tokio-tungstenite` from OpenAI's forks with features the crates.io releases lack. Cargo applies `[patch]` only from the manifest at the root of a build.
- **Chosen.** Each adapter is its own Cargo workspace with its own lockfile, and the root workspace excludes them ([#25](https://github.com/novelKR/CodeSpace/pull/25)). Each workspace that reaches Codex copies the `[patch]` entries it needs from Codex's manifest unchanged, and they change and revert together with the pin ([#25](https://github.com/novelKR/CodeSpace/pull/25), [#51](https://github.com/novelKR/CodeSpace/pull/51)).
- **Rama.** Codex pins its `rama` crates at `=0.3.0-alpha.4`, but those crates accept their leaf crates at `^0.3.0-alpha.4`, so a fresh resolve mixed in the stable 0.3.0 leaves and broke the build. The file-system and Linux sandbox adapters pin `rama-error`, `rama-macros` and `rama-utils` exactly, and CI builds with `--locked` ([#53](https://github.com/novelKR/CodeSpace/pull/53), [#54](https://github.com/novelKR/CodeSpace/pull/54)).
- **Lockfiles.** A change to the runner's dependencies must reach the worker's lockfile ([#53](https://github.com/novelKR/CodeSpace/pull/53), [#54](https://github.com/novelKR/CodeSpace/pull/54)). CI's cache keys name each lockfile because a glob walked into the submodule ([#67](https://github.com/novelKR/CodeSpace/pull/67)), and a test build no longer rewrites a stale lockfile ([#69](https://github.com/novelKR/CodeSpace/pull/69)).

<a id="case-graph"></a>

### Keeping Codex's graph out of the core

- **Patch engine.** The gateway could not link `codex-apply-patch` in the root workspace because of a `tokio-tungstenite` feature conflict, so the engine runs in the `codespace-patch` helper, which takes JSON on stdin and answers on stdout ([#27](https://github.com/novelKR/CodeSpace/pull/27)).
- **File system.** No-follow file I/O (`LOCAL_FS`) comes from `codex-exec-server`, which brings `codex-protocol` and other Codex crates into the root lockfile. Public types stay CodeSpace's, core crates take no direct Codex dependency, and the dependency check rejects `codex-core`, `codex-exec`, `codex-app-server` and `codex-login` in every product graph ([#51](https://github.com/novelKR/CodeSpace/pull/51), [#57](https://github.com/novelKR/CodeSpace/pull/57)).
- **Linux sandbox.** `codex-linux-sandbox` re-runs its own executable inside bubblewrap. It is wrapped in a binary-only helper, the runner depends only on a small protocol crate, and the dependency check keeps the helper's graph out of the runner ([#53](https://github.com/novelKR/CodeSpace/pull/53), [#54](https://github.com/novelKR/CodeSpace/pull/54), [#57](https://github.com/novelKR/CodeSpace/pull/57)).

<a id="case-defaults"></a>

### Upstream defaults that CodeSpace overrides

- Codex's workspace-write profile keeps `.git`, `.agents` and `.codex` read-only, and its default `NO_PROXY` lets traffic to loopback addresses bypass the proxy. CodeSpace's workspace is writable throughout, and loopback traffic goes through CodeSpace's proxy ([#53](https://github.com/novelKR/CodeSpace/pull/53), [#55](https://github.com/novelKR/CodeSpace/pull/55)).
- `codex-uds` changes the mode of an existing socket directory to 0700. CodeSpace's gateway allocates a new 0700 directory instead, and the worker only binds in it ([#46](https://github.com/novelKR/CodeSpace/pull/46)).
- The PTY size is set to 24×80 explicitly rather than taken from upstream's default ([#49](https://github.com/novelKR/CodeSpace/pull/49)).

<a id="case-missing"></a>

### Capabilities missing at the pin

Each was closed in CodeSpace or documented and pinned by a test. None moved the pin.

- **File modes.** Codex's file-system interface has no chmod, a no-follow `chmodat` failed on Linux, and Codex's walk from `/` fails on macOS, where `/var` is a symbolic link. CodeSpace opens the file without following links and changes the mode through the open descriptor, and it canonicalizes the workspace root. A path-based chmod was rejected because it weakens no-follow ([#25](https://github.com/novelKR/CodeSpace/pull/25), [#51](https://github.com/novelKR/CodeSpace/pull/51), [#52](https://github.com/novelKR/CodeSpace/pull/52)).
- **Signal deaths on PTY.** The pinned PTY reports a process killed by a signal as exit code 1. The pipe path now reports the signal; the PTY behavior is documented and pinned by a test ([#85](https://github.com/novelKR/CodeSpace/issues/85), [#87](https://github.com/novelKR/CodeSpace/pull/87)).
- **Descriptor inheritance on macOS.** Concurrent spawns could pass each other's descriptors, and Codex's own descriptor policy exists only in later releases. CodeSpace marks inherited descriptors close-on-exec in its own spawns; a pin change was among the alternatives and was not chosen ([#79](https://github.com/novelKR/CodeSpace/issues/79), [#81](https://github.com/novelKR/CodeSpace/pull/81)).
- **Fork safety on macOS.** Forked children could abort inside libnotify, and the PTY's fork belongs to the pinned code. CodeSpace completes libnotify's initialization before it forks, recorded as a mitigation rather than a structural fix ([#86](https://github.com/novelKR/CodeSpace/pull/86)).
- **Descriptors for governed launches.** The pinned PTY passes on only descriptors that are already inheritable, and `ChildFds::Attached` needs a later release. CodeSpace makes the launch descriptors inheritable only around that spawn, while its other spawners exclude them ([#90](https://github.com/novelKR/CodeSpace/pull/90)).
- **Toolchain.** The root manifest declares Rust 1.88, while the pinned checkout selects 1.95.0. The [operations guide](operations.md) says 1.88 is not a verified minimum ([#56](https://github.com/novelKR/CodeSpace/pull/56)).

<a id="case-pre-reap"></a>

### Observing before the reap: the first patch

- **Gap.** The pinned `codex-utils-pty` reaps the child in its own task, with no hook before the reap, so a governed PTY execution cannot be observed before its root is reaped ([DevGuard integration](devguard-integration.md#devguard-lifecycle)).
- **Weighed** on 2026-10-11 ([DevGuard #14](https://github.com/novelKR/DevGuard/issues/14#issuecomment-6110596020)): keeping a documented limitation, a CodeSpace-owned PTY on Codex's public `spawn_from_driver`, or a pre-reap hook in Codex. The [upstream adapter specification](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-28-cs-dg-upstream-adapter-work-spec-1.md#8-preferred-investigation-b-an-upstream-owned-bounded-pre-reap-observation-contract) prefers an upstream-owned, bounded observation before the reap, and a CodeSpace-owned PTY would move a mechanism CodeSpace delegates to Codex into CodeSpace.
- **Chosen.** The owner chose the hook, in parallel with the lifecycle work: [#92](https://github.com/novelKR/CodeSpace/pull/92) merged with PTY observation after the reap, and the hook was prepared as a separate change. The observer had first been built and tested on a Codex prerelease ([DevGuard W3 decision packet](https://github.com/novelKR/DevGuard/blob/668750b4ab6ad582904c3d05ffceffa062b26243/docs/handoff/2026-09-28-w3-decision-packet.md#32-experiment-b-backend-owned-pre-reap-observation-packet-132), section 3.2) and was ported to `rust-v0.154.0`.
- **Form.** A fork whose commit the submodule would name, or a patch file on the official release. Either has to be ported when the pin moves, and `rust-v0.159.x` changes this PTY code. The owner chose the patch file: the submodule stays official, and the change, its record and each later port are reviewed in CodeSpace, with no second repository to keep current. This page makes that choice the rule.

<a id="case-patterns"></a>

### What the cases show

- Codex code comes from a submodule at a release tag, never from a copied crate.
- Adapters live in their own workspaces and take Codex's workspace settings as they are: its `[patch]` entries and exact versions, with locked builds.
- Codex dependency graphs that the core must not carry stay behind helper processes and dependency checks.
- CodeSpace's own policy wins where an upstream default would weaken it.
- A missing capability is closed in CodeSpace, or documented and pinned by a test. A pin move is always its own decision, never part of other work.
- Codex's code changes only when neither an adapter nor a pin move fits and CodeSpace must not take over the mechanism, and then only as a recorded patch on the official release.

<a id="case-open-points"></a>

### Open points

Collecting the cases turned up three points that are not yet resolved:

- `crates/patch` has no Rama resolver guard and relies on its lockfile alone, unlike the file-system and Linux sandbox adapters.
- The conflict recorded for the patch helper ([#27](https://github.com/novelKR/CodeSpace/pull/27)) predates the root's copy of Codex's `[patch]` entries ([#51](https://github.com/novelKR/CodeSpace/pull/51)). Whether the root workspace could now link `codex-apply-patch` is unverified.
- The records name `rust-v0.154.0` as the candidate pin but give no reason for choosing it, although the [update procedure](upstream-update.md) asks for one.
