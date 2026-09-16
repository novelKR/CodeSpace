# Runner isolation (W05)

Execution isolation OS is a **Linux container**. Gateway unit tests may
run on macOS. That is not a claim that Linux isolation was verified on
the development laptop.

## What the example compose file does

[`deploy/compose.yml`](../deploy/compose.yml) runs an
unprivileged user (`uid 10001`) and bind-mounts **only** the workspace at
`/workspace`. It does not mount:

- host `$HOME`
- SSH agent socket
- `/var/run/docker.sock`
- gateway `.env`, Bearer files, or SQLite

## macOS / no Docker

`codespace-runner::PathSandbox` applies the same relative-path + symlink
+ special-file rules for unit tests. If Docker is not used, **Linux
container isolation is unverified**. Host seccomp/AppArmor and Docker
Desktop vs Linux engine differences are also unverified.

## Later process split

MVP still runs `codespace-mcp` as one process. `crates/runner` is the
place a Unix-socket worker would live; both sides remain Rust.
