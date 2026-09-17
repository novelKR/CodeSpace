# Deploy examples

See [docs/operations.md](../docs/operations.md) for install, env vars,
recovery, and what is unverified.

- `Dockerfile` / `compose.yml`: unprivileged Linux runner. The only bind
  mount is `${CODESPACE_WORKSPACE}` → `/workspace`. Do not add host home,
  SSH agent, Docker socket, or gateway secrets.
