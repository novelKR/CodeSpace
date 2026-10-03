#!/bin/bash
# Temporary: repeat on macOS the U2 test runs whose fork-abort isolation was removed, with #86's
# mitigation and, as a control, with `prepare_fork_spawns` doing nothing. $1: the checkout of
# #84's head to test. Outputs go to diag-out/ next to it.
set -u
cd "$1"
out="$(dirname "$PWD")/diag-out"
mkdir -p "$out"
fork_handlers=crates/runner/src/fork_handlers.rs

# The same build and environment as the macos-core stage, which also runs everything once.
python3 scripts/validate-upstream.py macos-core --output target/upstream-reports/u2-check
status=$?
echo "macos-core stage exit: $status"
if [ "$status" -ne 0 ]; then
  grep -E -A3 "panicked at|^test .* FAILED|^\\+ " target/upstream-reports/u2-check/macos-core.log | tail -60
fi
target="$PWD/target/upstream-validation"
export CARGO_TARGET_DIR="$target"
export PIN_ONLY=1
export CODESPACE_PATCH_BIN="$target/debug/codespace-patch"
export CODESPACE_RUNTIME_BIN="$target/debug/codespace-codex-runtime"
export CODESPACE_LINUX_SANDBOX_BIN="$target/debug/codespace-linux-sandbox"
export CODESPACE_DEVGUARD_RUNTIME_BIN="$target/debug/codespace-codex-runtime-devguard"
export CODESPACE_DEVGUARD_FIXTURE_BIN="$target/debug/examples/fixture_authority"
export CODESPACE_REQUIRE_DEVGUARD_BINS=1

crash_reports() { # reports newer than $1 that name _notify_fork_child
  find "$HOME/Library/Logs/DiagnosticReports" /Library/Logs/DiagnosticReports -newer "$1" \
    \( -name '*.ips' -o -name '*.crash' \) 2>/dev/null \
    | while read -r f; do grep -l "_notify_fork_child" "$f" 2>/dev/null; done | wc -l | tr -d ' '
}

results=()
# repeat NAME COUNT COMMAND...
repeat() {
  local name="$1" count="$2"; shift 2
  local fails=0 i
  touch "$out/$name.start"
  echo "== $name: $count runs of $*"
  for i in $(seq 1 "$count"); do
    if ! "$@" > "$out/$name-$i.log" 2>&1; then
      fails=$((fails + 1))
      echo "$name run $i failed:"
      grep -E -A3 "panicked at" "$out/$name-$i.log" | grep -v "^note:" | head -12
      grep -E "^test .* FAILED" "$out/$name-$i.log" | head -6
    fi
  done
  sleep 15 # ReportCrash writes its reports a little later
  local line="$name: $fails/$count runs failed; crash reports naming _notify_fork_child: $(crash_reports "$out/$name.start")"
  echo "$line"
  results+=("$line")
}

runner_filter=(cargo test --locked -p codespace-runner --features devguard --lib -- registration wire)
runner_pair=(cargo test --locked -p codespace-runner --features devguard --lib --
  wire::tests::registration::a_timeout_needs_no_successful_registration
  wire::tests::replay_returns_cached_response)
gateway=(cargo test --locked -p codespace-server --features devguard --lib -- devguard)

scenarios() { # prefix
  # Build both test binaries first, so no timed run includes a build.
  cargo test --locked -p codespace-runner --features devguard --lib --no-run > "$out/$1-build.log" 2>&1 \
    && cargo test --locked -p codespace-server --features devguard --lib --no-run >> "$out/$1-build.log" 2>&1 \
    || { echo "$1: build failed"; tail -20 "$out/$1-build.log"; results+=("$1: build failed"); return; }
  repeat "$1-runner-filter" 40 "${runner_filter[@]}"
  repeat "$1-runner-pair" 60 "${runner_pair[@]}"
  repeat "$1-gateway-parallel" 20 "${gateway[@]}"
}

scenarios with-mitigation

# Control: the same runs with prepare_fork_spawns doing nothing (its libnotify call removed).
cp "$fork_handlers" "$out/fork_handlers.rs.orig"
python3 - "$fork_handlers" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
old = "        notify_is_valid_token(0);"
assert text.count(old) == 1
open(path, "w").write(text.replace(old, ""))
PY
scenarios control-without-mitigation
cp "$out/fork_handlers.rs.orig" "$fork_handlers"
git diff --quiet -- "$fork_handlers" && echo "restored $fork_handlers"

echo
echo "== Results"
printf '%s\n' "${results[@]}"
