#!/usr/bin/env bash
# Policy scan zones (docs/codex-reuse.md, docs/execution-substrate.md):
#
#   cost   what                                      where
#   high   submodule / clippy / cargo test           rust job — not this script
#   mid    pin SHA                                   rust job (submodule already paid)
#   low    core Cargo.toml + source grep             this script
#          adapter Cargo.toml allowlist              this script (no submodule)
#   never  third_party/codex sources                 product scan forbidden
#
# Core must not take any Codex crate dep (agent/product *or* execution).
# Isolated adapters may take an *approved* subgraph only (manifest keys).
# Local HTTP reqwest in codespace-server is OK.
#
# SCAN_BASE (optional): git ref/sha to diff against (PR base or previous
# main). Unset, all-zero, or merge-base failure → scan all core + adapters.
# Never skip the tree because the range could not be computed.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

SOURCE_PATTERNS=(
  'api\.openai\.com::api.openai.com'
  'openai[_-]api::openai_api / openai-api'
  'responses[_-]api::responses-api'
  'codex-login::codex-login'
  'codex-core[[:space:]]*=::codex-core cargo dep'
  'path[[:space:]]*=[[:space:]]*"[^"]*codex-core::codex-core path dep'
  'codex-app-server::codex-app-server'
  'async-openai::async-openai'
)

# Direct keys allowed in crates/patch today (apply-patch workspace graph).
# codex-exec-server here is compile graph, not a product backend choice.
patch_key_allowed() {
  case "$1" in
    codex-apply-patch|codex-exec-server|codex-utils-path-uri|codex-process-hardening) return 0 ;;
    *) return 1 ;;
  esac
}

# Future crates/codex-runtime: prefer / evaluate / protocol from
# docs/codex-reuse.md. Not agent/product: no core, login, app-server, exec.
# Current runtime keys in code: codex-process-hardening, codex-uds.
runtime_key_allowed() {
  case "$1" in
    codex-apply-patch|codex-process-hardening|codex-utils-pty|codex-uds|\
    codex-utils-absolute-path|codex-utils-path-uri|codex-file-search|\
    codex-file-system|codex-shell-command|codex-linux-sandbox|\
    codex-sandboxing|codex-network-proxy|codex-install-context|\
    codex-exec-server-protocol|codex-protocol|codex-execpolicy|\
    codex-exec-server|codex-git-utils|codex-worktree) return 0 ;;
    *) return 1 ;;
  esac
}

# crates/pty: interactive spawn only.
pty_key_allowed() {
  case "$1" in
    codex-utils-pty) return 0 ;;
    *) return 1 ;;
  esac
}

# crates/file-system: no-follow I/O + bounded walk. Direct keys only.
fs_key_allowed() {
  case "$1" in
    codex-file-system|codex-exec-server|codex-utils-path-uri) return 0 ;;
    *) return 1 ;;
  esac
}

# crates/linux-sandbox: helper argv + bwrap/seccomp + Enabled NetworkProxy.
linux_sandbox_key_allowed() {
  case "$1" in
    codex-linux-sandbox|codex-sandboxing|codex-protocol|codex-utils-path-uri|codex-network-proxy) return 0 ;;
    *) return 1 ;;
  esac
}

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

scan_domain=0
scan_policy=0
scan_runner=0
scan_store=0
scan_server=0
scan_root_manifest=0
scan_tests=0
scan_patch=0
scan_runtime=0
scan_pty=0
scan_fs=0
scan_linux_sandbox=0
scan_protocol=0
scan_all=0

is_zero_sha() {
  [[ "$1" =~ ^0+$ ]]
}

want_all() {
  scan_all=1
  scan_domain=1
  scan_policy=1
  scan_runner=1
  scan_store=1
  scan_server=1
  scan_root_manifest=1
  scan_tests=1
  scan_patch=1
  if [[ -d crates/codex-runtime ]]; then
    scan_runtime=1
  fi
  if [[ -d crates/pty ]]; then
    scan_pty=1
  fi
  if [[ -d crates/file-system ]]; then
    scan_fs=1
  fi
  if [[ -d crates/linux-sandbox ]]; then
    scan_linux_sandbox=1
  fi
  if [[ -d crates/linux-sandbox-protocol ]]; then
    scan_protocol=1
  fi
}

base="${SCAN_BASE:-}"
if [[ -z "$base" ]] || is_zero_sha "$base"; then
  want_all
else
  merge_base=""
  if merge_base="$(git merge-base "$base" HEAD 2>/dev/null)"; then
    while IFS= read -r f; do
      [[ -z "$f" ]] && continue
      case "$f" in
        Cargo.toml|Cargo.lock|scripts/check-no-model-deps.sh)
          want_all
          break
          ;;
        crates/domain|crates/domain/*)
          scan_domain=1
          ;;
        crates/policy|crates/policy/*)
          scan_policy=1
          ;;
        crates/runner|crates/runner/*)
          scan_runner=1
          ;;
        crates/store|crates/store/*)
          scan_store=1
          ;;
        crates/server|crates/server/*|tests|tests/*)
          scan_server=1
          scan_tests=1
          ;;
        crates/patch|crates/patch/*)
          scan_patch=1
          ;;
        crates/codex-runtime|crates/codex-runtime/*)
          if [[ -d crates/codex-runtime ]]; then
            scan_runtime=1
          fi
          ;;
        crates/pty|crates/pty/*)
          if [[ -d crates/pty ]]; then
            scan_pty=1
          fi
          ;;
        crates/file-system|crates/file-system/*)
          if [[ -d crates/file-system ]]; then
            scan_fs=1
          fi
          ;;
        crates/linux-sandbox|crates/linux-sandbox/*)
          if [[ -d crates/linux-sandbox ]]; then
            scan_linux_sandbox=1
          fi
          ;;
        crates/linux-sandbox-protocol|crates/linux-sandbox-protocol/*)
          if [[ -d crates/linux-sandbox-protocol ]]; then
            scan_protocol=1
          fi
          ;;
      esac
    done < <(git diff --name-only "$merge_base"...HEAD)
  else
    echo "SCAN_BASE=$base but merge-base failed; scanning all core crates" >&2
    want_all
  fi
fi

if [[ "$scan_all" -eq 0 &&
      "$scan_domain" -eq 0 &&
      "$scan_policy" -eq 0 &&
      "$scan_runner" -eq 0 &&
      "$scan_store" -eq 0 &&
      "$scan_server" -eq 0 &&
      "$scan_root_manifest" -eq 0 &&
      "$scan_patch" -eq 0 &&
      "$scan_runtime" -eq 0 &&
      "$scan_pty" -eq 0 &&
      "$scan_fs" -eq 0 &&
      "$scan_linux_sandbox" -eq 0 &&
      "$scan_protocol" -eq 0 ]]; then
  echo "policy-scan skipped (no core crate or adapter changes)"
  exit 0
fi

selected=()
[[ "$scan_domain" -eq 1 ]] && selected+=("domain")
[[ "$scan_policy" -eq 1 ]] && selected+=("policy")
[[ "$scan_runner" -eq 1 ]] && selected+=("runner")
[[ "$scan_store" -eq 1 ]] && selected+=("store")
[[ "$scan_server" -eq 1 ]] && selected+=("server")

echo "policy-scan: crates=${selected[*]:-none} tests=$scan_tests root-manifest=$scan_root_manifest patch=$scan_patch runtime=$scan_runtime pty=$scan_pty fs=$scan_fs linux-sandbox=$scan_linux_sandbox protocol=$scan_protocol"

scan_manifest() {
  local file="$1"
  local outfile="$2"
  [[ -f "$file" ]] || return 0
  local out
  out="$(grep -nE '^[[:space:]]*codex-[A-Za-z0-9_-]+[[:space:]]*=' "$file" || true)"
  if [[ -n "$out" ]]; then
    {
      echo "forbidden (codex- cargo dep) in $file:"
      echo "$out"
    } >"$outfile"
  fi
}

scan_adapter_manifest() {
  local file="$1"
  local kind="$2"
  local outfile="$3"
  [[ -f "$file" ]] || return 0
  local bad=""
  local line key
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    key="${line#*:}"
    key="${key%%=*}"
    key="${key#"${key%%[![:space:]]*}"}"
    key="${key%"${key##*[![:space:]]}"}"
    case "$kind" in
      patch)
        if ! patch_key_allowed "$key"; then
          bad+="$line"$'\n'
        fi
        ;;
      runtime)
        if ! runtime_key_allowed "$key"; then
          bad+="$line"$'\n'
        fi
        ;;
      pty)
        if ! pty_key_allowed "$key"; then
          bad+="$line"$'\n'
        fi
        ;;
      fs)
        if ! fs_key_allowed "$key"; then
          bad+="$line"$'\n'
        fi
        ;;
      linux-sandbox)
        if ! linux_sandbox_key_allowed "$key"; then
          bad+="$line"$'\n'
        fi
        ;;
    esac
  done < <(grep -nE '^[[:space:]]*codex-[A-Za-z0-9_-]+[[:space:]]*=' "$file" || true)
  if [[ -n "$bad" ]]; then
    {
      echo "forbidden (codex- key not on $kind allowlist) in $file:"
      printf '%s' "$bad"
    } >"$outfile"
  fi
}

scan_source() {
  local dir="$1"
  local pattern="$2"
  local label="$3"
  local outfile="$4"
  [[ -d "$dir" ]] || return 0
  local out
  out="$(grep -R -n -E --exclude-dir=target --exclude-dir=.git -e "$pattern" "$dir" || true)"
  if [[ -n "$out" ]]; then
    {
      echo "forbidden ($label) in $dir:"
      echo "$out"
    } >"$outfile"
  fi
}

# Runner/core must not take the helper as a path library. The protocol
# crate name does not match this pattern (`-protocol` is not whitespace).
scan_no_helper_lib() {
  local file="$1"
  local outfile="$2"
  [[ -f "$file" ]] || return 0
  local out
  out="$(grep -nE '^[[:space:]]*codespace-linux-sandbox[[:space:]]*=' "$file" || true)"
  if [[ -n "$out" ]]; then
    {
      echo "forbidden (codespace-linux-sandbox library path dep) in $file:"
      echo "$out"
    } >"$outfile"
  fi
}

scan_helper_binary_only() {
  local outfile="$1"
  if [[ -f crates/linux-sandbox/src/lib.rs ]]; then
    echo "forbidden: crates/linux-sandbox must be binary-only (src/lib.rs present)" >"$outfile"
  fi
}

pids=()
n=0

launch() {
  n=$((n + 1))
  local outfile="$tmpdir/out_$n"
  "$@" "$outfile" &
  pids+=("$!")
}

if [[ "$scan_root_manifest" -eq 1 ]]; then
  launch scan_manifest Cargo.toml
fi

for crate in "${selected[@]+"${selected[@]}"}"; do
  launch scan_manifest "crates/${crate}/Cargo.toml"
  launch scan_no_helper_lib "crates/${crate}/Cargo.toml"
done

for crate in "${selected[@]+"${selected[@]}"}"; do
  for spec in "${SOURCE_PATTERNS[@]}"; do
    pattern="${spec%%::*}"
    label="${spec#*::}"
    launch scan_source "crates/${crate}" "$pattern" "$label"
  done
done

if [[ "$scan_tests" -eq 1 ]]; then
  for spec in "${SOURCE_PATTERNS[@]}"; do
    pattern="${spec%%::*}"
    label="${spec#*::}"
    launch scan_source tests "$pattern" "$label"
  done
fi

if [[ "$scan_patch" -eq 1 ]]; then
  launch scan_adapter_manifest crates/patch/Cargo.toml patch
fi

if [[ "$scan_runtime" -eq 1 ]]; then
  launch scan_adapter_manifest crates/codex-runtime/Cargo.toml runtime
fi

if [[ "$scan_pty" -eq 1 ]]; then
  launch scan_adapter_manifest crates/pty/Cargo.toml pty
fi

if [[ "$scan_fs" -eq 1 ]]; then
  launch scan_adapter_manifest crates/file-system/Cargo.toml fs
fi

if [[ "$scan_linux_sandbox" -eq 1 ]]; then
  launch scan_adapter_manifest crates/linux-sandbox/Cargo.toml linux-sandbox
  launch scan_helper_binary_only
fi

if [[ "$scan_protocol" -eq 1 ]]; then
  launch scan_manifest crates/linux-sandbox-protocol/Cargo.toml
  launch scan_no_helper_lib crates/linux-sandbox-protocol/Cargo.toml
fi

for pid in "${pids[@]+"${pids[@]}"}"; do
  wait "$pid"
done

hits=0
for f in "$tmpdir"/out_*; do
  [[ -f "$f" ]] || continue
  cat "$f" >&2
  hits=1
done

if [[ "$hits" -ne 0 ]]; then
  echo "Model / Responses / Codex deps are forbidden in CodeSpace core." >&2
  echo "Adapter manifests may only use the approved subgraph allowlist." >&2
  echo "See docs/execution-substrate.md and docs/codex-reuse.md." >&2
  exit 1
fi

echo "no-model-deps ok"
