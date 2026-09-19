# PR #53 Linux sandbox CI user-namespace blocker

PR #53 adds the `codespace-linux-sandbox` helper and requires the Linux
isolation tests to exercise the real bubblewrap + seccomp path in CI rather
than silently skipping a failed probe.

## Symptom

GitHub Actions CI run #99 (`35383518697`) reached the dedicated
`crates/linux-sandbox` isolation tests, but all six tests failed at their
common helper probe. With probe stderr enabled, bubblewrap reported:

```text
bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted
```

The helper then exited with status 1, so
`CODESPACE_REQUIRE_LINUX_SANDBOX=1` correctly turned the failed probe into a
hard CI failure.

This is distinct from the earlier dependency-lock, Clippy, and unit-test
environment issues. The linux-sandbox unit suite, including the helper
self-reexec readable-root check, passed before the integration probe ran.

## Cause

The Restricted network profile uses bubblewrap network isolation
(`--unshare-net`). GitHub-hosted Ubuntu images can restrict unprivileged user
namespaces with either `kernel.unprivileged_userns_clone` or Ubuntu 24.04+
AppArmor's `kernel.apparmor_restrict_unprivileged_userns` gate. In that host
configuration bubblewrap can create the sandbox process far enough to report
its loopback setup, but the netlink address operation is rejected with
`EPERM` / `RTM_NEWADDR`.

This is a CI host prerequisite, not a reason to weaken the CodeSpace sandbox
profile. OpenAI's `codex-action` handles the same GitHub-hosted Linux condition
before running bubblewrap-backed Codex sandbox modes:

- https://github.com/openai/codex-action/blob/main/action.yml

## CI resolution

The CodeSpace `ubuntu-latest` Rust job prepares the ephemeral GitHub-hosted
runner immediately after installing bubblewrap:

```bash
current_userns="$(sysctl -n kernel.unprivileged_userns_clone 2>/dev/null || true)"
if [ -n "$current_userns" ] && [ "$current_userns" != "1" ]; then
  sudo sysctl -w kernel.unprivileged_userns_clone=1
fi

current_apparmor="$(sysctl -n kernel.apparmor_restrict_unprivileged_userns 2>/dev/null || true)"
if [ -n "$current_apparmor" ] && [ "$current_apparmor" != "0" ]; then
  sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
fi
```

Both checks are conditional so older Ubuntu images without one of the sysctls
remain valid. The workflow currently runs on GitHub-hosted `ubuntu-latest`;
if a self-hosted runner is introduced, its host policy should be reviewed
explicitly rather than assuming this CI mutation is appropriate there.

## Security / product scope

This change is CI-host preparation only. It does **not**:

- remove `--unshare-net`;
- change Restricted network policy or seccomp enforcement;
- make a failed sandbox probe skippable in Linux CI;
- add an unsandboxed fallback after a successful probe;
- change Runner/MCP wire contracts; or
- change production host sysctls at runtime.

`CODESPACE_REQUIRE_LINUX_SANDBOX=1` remains scoped to the isolation integration
tests, so the CI contract is still: if the GitHub runner has been prepared for
bubblewrap and the real sandbox cannot start, the job fails.

## Related PR #53 fixes

The prior helper-readable-root change remains a separate correctness fix. The
pinned Codex Linux helper re-execs its own executable inside bubblewrap before
applying seccomp, so that infrastructure binary must remain readable in the
Minimal filesystem view. CI run #99 showed that the observed blocker occurs
earlier, during bubblewrap network-namespace setup; therefore the helper-read
fix should not be described as the root cause of the `RTM_NEWADDR` failure.

References:

- PR #53: https://github.com/novelKR/CodeSpace/pull/53
- CI run #99: https://github.com/novelKR/CodeSpace/actions/runs/35383518697
- helper-readable fix: `446929248e7240a5fe867971a5b666ced78dd58a`
