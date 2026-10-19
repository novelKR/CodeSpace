#!/usr/bin/env bash
# Fail the deploy if the Codex submodule does not match docs/upstream-lock.md,
# if its files are not exactly the recorded patched tree, or if the isolated
# patch adapter tests fail. Never treat a red parity run as success.
set -euo pipefail

# Outcomes in color on a terminal, unless NO_COLOR is set or TERM is dumb, as scripts/console.py
# chooses; redirected, every line is as before.
colors() { [[ -t "$1" && -z "${NO_COLOR:-}" && "${TERM:-}" != dumb ]]; }
if colors 1; then OK=$'\e[1;32mok\e[0m'; else OK=ok; fi
fail() {
  if colors 2; then printf '\e[1;31m%s\e[0m\n' "$1" >&2; else printf '%s\n' "$1" >&2; fi
}

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
LOCK_FILE="docs/upstream-lock.md"

if [[ ! -f "$LOCK_FILE" ]]; then
  fail "missing $LOCK_FILE"
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
  fail "could not parse 40-char commit from $LOCK_FILE (got: ${expected:-empty})"
  exit 1
fi

if [[ ! -e third_party/codex ]]; then
  fail "third_party/codex is missing; init the submodule"
  exit 1
fi

actual="$(git -C third_party/codex rev-parse HEAD)"
if [[ "$actual" != "$expected" ]]; then
  fail "Codex submodule $actual does not match lock $expected"
  echo "Do not git submodule update --remote onto Codex main. See docs/upstream-update.md." >&2
  exit 1
fi

# The recorded downstream patches must be applied, and nothing else changed.
python3 scripts/codex_patch.py check

echo "upstream pin $OK $actual"

if [[ "${PIN_ONLY:-}" == "1" ]]; then
  exit 0
fi

echo "This gate covers SHA + patch only; full qualification: python3 scripts/validate-upstream.py all"
cargo test --locked --manifest-path crates/patch/Cargo.toml
