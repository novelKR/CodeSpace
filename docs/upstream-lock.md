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
