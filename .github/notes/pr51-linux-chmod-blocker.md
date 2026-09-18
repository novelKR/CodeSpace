# PR #51 Linux chmod blocker

While landing PR #51 (`P0 filesystem: PathSandbox over isolated Codex LOCAL_FS`), Linux CI exposed one platform-specific blocker in rollback mode restoration.

## Symptom

`codespace-fs` used `rustix::fs::chmodat(..., AtFlags::SYMLINK_NOFOLLOW)` for a regular file. On the pinned Linux/rustix backend this returned `EOPNOTSUPP` / `Operation not supported (os error 95)`, causing the filesystem test job to fail.

## Resolution

Do not fall back to pathname `chmod`, because that would weaken the no-follow guarantee. The merged fix walks parent directories without following symlinks, opens the leaf with `O_RDONLY | O_NOFOLLOW | O_CLOEXEC`, then applies `fchmod` to the opened fd.

This makes the mutation target the opened inode and preserves the symlink-safety boundary used by rollback.

## Current limitation

Because the leaf is opened with `O_RDONLY`, `set_unix_mode` is intentionally scoped to rollback-capable regular files whose contents were already readable. It is not a full replacement for general `chmod(2)` semantics; for example, an owner-only mode-`000` file may be chmod-able by the OS but cannot be opened by this primitive.

## References

- PR #51: https://github.com/novelKR/CodeSpace/pull/51
- Failing CI run: https://github.com/novelKR/CodeSpace/actions/runs/35318027048
- Fix commit: https://github.com/novelKR/CodeSpace/commit/8208f011bf69120f54b949c16c59cc1e55d37332

This note records the blocker and its boundary only. PR #52 does not change the chmod implementation; it finalizes filesystem error semantics before Linux command sandbox work begins.
