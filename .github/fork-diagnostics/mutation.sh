#!/bin/bash
# Temporary removal checks on macOS: with each part of the libnotify mitigation removed, the
# tests that guard it must fail; unmutated, they pass. Each case runs one isolated scenario
# directly. Mutations are applied to the checkout and restored byte for byte; nothing is
# committed.
set -u
cd "$(dirname "$0")/../.."
out="$PWD/diag-out/mutation"
mkdir -p "$out"
results=()
unexpected=0
mutated=""

mutate() { # name file old new
  local name="$1" file="$2" old="$3" new="$4"
  cp "$file" "$out/original"
  python3 - "$file" "$old" "$new" <<'PY'
import sys
path, old, new = sys.argv[1:4]
text = open(path).read()
assert text.count(old) == 1, (path, text.count(old))
open(path, 'w').write(text.replace(old, new))
PY
  mutated="$file"
  echo "### mutation $name in $file"
  git diff --stat -- "$file"
}

restore() {
  cp "$out/original" "$mutated"
  git diff --quiet -- "$mutated" && echo "### restored $mutated"
  mutated=""
}

# case NAME EXPECT(pass|fail|observe) ENV... -- COMMAND...
case_() {
  local name="$1" expect="$2"; shift 2
  local env=()
  while [ "$1" != "--" ]; do env+=("$1"); shift; done
  shift
  echo "== $name (expect $expect): ${env[*]} $*"
  env "${env[@]}" "$@" > "$out/$name.log" 2>&1
  local status=$?
  grep -E "^test .* (ok|FAILED)$|^test result|panicked at|fork_race summary|forked while|did not fork|not complete|scenario failed|once=" "$out/$name.log" | head -30
  local outcome=pass
  [ "$status" -ne 0 ] && outcome=fail
  local verdict=observed
  if [ "$expect" != observe ]; then
    if [ "$outcome" = "$expect" ]; then verdict=as-expected; else verdict=UNEXPECTED; unexpected=1; fi
  fi
  results+=("$name: $outcome (expected $expect): $verdict")
}

runner=(cargo test --locked -p codespace-runner --lib --)
server=(cargo test --locked -p codespace-server --lib --)
iso=CODESPACE_ISOLATED_SCENARIO=1
spawns=fork_handlers::tests::spawns_find_libnotify_initialized_when_they_fork
prepare=fork_handlers::tests::preparing_completes_the_libnotify_initialization
worker=runtime::tests::worker_command_forks_and_excludes_descriptors_up_to_the_table_end

scenarios() { # prefix expect-pipe expect-patch expect-pty expect-prepare expect-worker
  local p="$1"
  case_ "$p-pipe" "$2" "$iso" CODESPACE_ISOLATED_SPAWN_PATH=pipe -- "${runner[@]}" --exact "$spawns" --test-threads=1
  case_ "$p-patch-helper" "$3" "$iso" CODESPACE_ISOLATED_SPAWN_PATH=patch-helper -- "${runner[@]}" --exact "$spawns" --test-threads=1
  case_ "$p-pty" "$4" "$iso" CODESPACE_ISOLATED_SPAWN_PATH=pty -- "${runner[@]}" --exact "$spawns" --test-threads=1
  case_ "$p-prepare" "$5" "$iso" -- "${runner[@]}" --exact "$prepare" --test-threads=1
  case_ "$p-worker" "$6" "$iso" -- "${server[@]}" --exact "$worker" --test-threads=1
}

# R0: unmutated.
scenarios R0 pass pass pass pass pass

# R1: exclude_unrelated (pipe, patch helper, worker) without its call.
mutate R1 crates/runner/src/descriptors.rs \
  "pub fn exclude_unrelated(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    crate::prepare_fork_spawns();" \
  "pub fn exclude_unrelated(command: &mut tokio::process::Command) -> &mut tokio::process::Command {"
scenarios R1 fail fail pass pass fail
restore

# R2: the PTY spawn without its call.
mutate R2 crates/runner/src/process.rs \
  "        // portable-pty forks: see \`prepare_fork_spawns\`.
        crate::prepare_fork_spawns();
" ""
case_ R2-pty observe "$iso" CODESPACE_ISOLATED_SPAWN_PATH=pty -- "${runner[@]}" --exact "$spawns" --test-threads=1
restore

# R3: prepare_fork_spawns does nothing on macOS: the same build as the fix, without its effect.
mutate R3 crates/runner/src/fork_handlers.rs "        notify_is_valid_token(0);" ""
scenarios R3 fail fail observe fail fail
case_ R3-stress fail CODESPACE_FORK_RACE_OUT="$out/fork-race-R3" -- \
  cargo test --locked -p codespace-runner --test fork_race -- --trials 400 --require-zero
restore

echo
echo "== Removal checks"
printf '%s\n' "${results[@]}"
git status --short
exit "$unexpected"
