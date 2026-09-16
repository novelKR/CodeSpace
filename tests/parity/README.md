# Parity fixtures (W06)

These cases are copied from the pinned Codex apply-patch suite at
`third_party/codex` (`rust-v0.154.0` / `6b9826e3aa83b1a5947db50f4332cb9c65f1b340`).
They are a **subset**. Passing them does not mean the entire upstream suite
ran.

| Fixture | Source |
| --- | --- |
| Add File `nested/new.txt` | `codex-rs/apply-patch/tests/suite/tool.rs` |

`crates/patch` calls `parse_patch` then product policy then
`apply_patch_with_options` with `PreserveLineEndings` and
`follow_symlinks: false`. It does not invoke `git apply` or the standalone
`apply_patch` binary.
