#!/bin/bash
# Temporary: repeat on macOS the U2 test runs whose fork-abort isolation was removed, with #86's
# mitigation and, as a control, with `prepare_fork_spawns` doing nothing. $1: the checkout of
# #84's head to test; $2: `final` for the runs with the mitigation only, the gateway's and the
# DevGuard adapter's 60 times, or `count` for the same runs with each test session that stops at
# the credential and is run again counted (a diagnostic line added to the two test helpers).
# Outputs go to diag-out/ next to it.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
cd "$1"
mode="${2:-full}"
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

# Lines in $DIAG_STOPS so far: one per test session run again after stopping at the credential.
stops_so_far() {
  if [ -n "${DIAG_STOPS:-}" ] && [ -f "$DIAG_STOPS" ]; then
    wc -l < "$DIAG_STOPS" | tr -d ' '
  else
    echo 0
  fi
}

results=()
# repeat NAME COUNT COMMAND...
repeat() {
  local name="$1" count="$2"; shift 2
  local fails=0 stops=0 i before after
  touch "$out/$name.start"
  echo "== $name: $count runs of $*"
  for i in $(seq 1 "$count"); do
    before=$(stops_so_far)
    if ! "$@" > "$out/$name-$i.log" 2>&1; then
      fails=$((fails + 1))
      echo "$name run $i failed:"
      grep -E -A3 "panicked at" "$out/$name-$i.log" | grep -v "^note:" | head -12
      grep -E "^test .* FAILED" "$out/$name-$i.log" | head -6
    fi
    after=$(stops_so_far)
    if [ "$after" -gt "$before" ]; then
      echo "$name run $i: $((after - before)) sessions stopped at the credential and ran again, by thread:"
      tail -n "+$((before + 1))" "$DIAG_STOPS" | sort | uniq -c
      stops=$((stops + after - before))
    fi
  done
  sleep 15 # ReportCrash writes its reports a little later
  local line="$name: $fails/$count runs failed; crash reports naming _notify_fork_child: $(crash_reports "$out/$name.start")"
  if [ -n "${DIAG_STOPS:-}" ]; then
    line="$line; sessions run again after stopping at the credential: $stops"
  fi
  echo "$line"
  results+=("$line")
}

runner_filter=(cargo test --locked -p codespace-runner --features devguard --lib -- registration wire)
runner_pair=(cargo test --locked -p codespace-runner --features devguard --lib --
  wire::tests::registration::a_timeout_needs_no_successful_registration
  wire::tests::replay_returns_cached_response)
gateway=(cargo test --locked -p codespace-server --features devguard --lib -- devguard)
# The DevGuard adapter's own tests, as the macos-core stage runs them.
adapter=(cargo test --locked --manifest-path crates/devguard/Cargo.toml)

scenarios() { # prefix
  # Build the test binaries first, so no timed run includes a build.
  cargo test --locked -p codespace-runner --features devguard --lib --no-run > "$out/$1-build.log" 2>&1 \
    && cargo test --locked -p codespace-server --features devguard --lib --no-run >> "$out/$1-build.log" 2>&1 \
    && "${adapter[@]}" --no-run >> "$out/$1-build.log" 2>&1 \
    || { echo "$1: build failed"; tail -20 "$out/$1-build.log"; results+=("$1: build failed"); return; }
  if [ "$adapter_runs" -gt 0 ]; then
    repeat "$1-adapter" "$adapter_runs" "${adapter[@]}"
  fi
  repeat "$1-runner-filter" 40 "${runner_filter[@]}"
  repeat "$1-runner-pair" 60 "${runner_pair[@]}"
  repeat "$1-gateway-parallel" "$gateway_runs" "${gateway[@]}"
}

gateway_runs=20
adapter_runs=0
if [ "$mode" = count ]; then
  # Each test session that stops at the credential and is run again adds a line naming its
  # thread to $DIAG_STOPS.
  python3 "$here/count_stops.py" crates/devguard/src/lib.rs crates/server/src/devguard.rs
  git diff --stat
  export DIAG_STOPS="$out/stops"
  : > "$DIAG_STOPS"
fi
if [ "$mode" = final ] || [ "$mode" = count ]; then
  gateway_runs=60
  adapter_runs=60
  scenarios "$mode-with-mitigation"
  echo
  echo "== Results"
  printf '%s\n' "${results[@]}"
  exit 0
fi
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
