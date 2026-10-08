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
| Consumers | Patch, runtime worker, PTY, filesystem, Linux sandbox and proxy adapters |
| Patch options | `PreserveLineEndings`, `follow_symlinks: false` |

See [connected components](codex-reuse.md) for the exact adapter responsibilities. Patch tests cover selected parity cases, not the entire upstream suite or all Codex behavior.

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
PIN_ONLY=1 ./scripts/check-upstream-pin.sh
```

This checks only the submodule revision against this document. Without `PIN_ONLY`, the script also runs patch tests; it does not replace the other adapter gates. Use the [complete update procedure](upstream-update.md) before changing the pin. Do not use `git submodule update --remote` as a deployment step.
