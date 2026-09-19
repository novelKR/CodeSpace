#!/usr/bin/env bash
# Fail the deploy if the Codex submodule does not match docs/upstream-lock.md
# or if the isolated patch adapter tests fail. Never treat a red parity run
# as success.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
LOCK_FILE="docs/upstream-lock.md"

if [[ ! -f "$LOCK_FILE" ]]; then
  echo "missing $LOCK_FILE" >&2
  exit 1
fi

expected="$(awk -F'|' '
  $2 ~ /Commit/ {
    sha = $3
    gsub(/[ `]/, "", sha)
    print sha
    exit
  }
' "$LOCK_FILE")"

if [[ ! "$expected" =~ ^[0-9a-f]{40}$ ]]; then
  echo "could not parse 40-char commit from $LOCK_FILE (got: ${expected:-empty})" >&2
  exit 1
fi

if [[ ! -e third_party/codex ]]; then
  echo "third_party/codex is missing; init the submodule" >&2
  exit 1
fi

actual="$(git -C third_party/codex rev-parse HEAD)"
if [[ "$actual" != "$expected" ]]; then
  echo "Codex submodule $actual does not match lock $expected" >&2
  echo "Do not git submodule update --remote onto Codex main. See docs/upstream-update.md." >&2
  exit 1
fi

echo "upstream pin ok $actual"

if [[ "${PIN_ONLY:-}" == "1" ]]; then
  exit 0
fi

echo "This gate covers SHA + patch only; full qualification: python3 scripts/validate-upstream.py all"
cargo test --locked --manifest-path crates/patch/Cargo.toml
