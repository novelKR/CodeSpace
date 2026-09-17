#!/usr/bin/env bash
# Root workspace crates must not grow a model / Responses / Codex-agent
# dependency (codex-core, login, app-server, Responses). crates/patch
# and third_party/codex are out of scope (apply-patch isolation).
# A future crates/codex-runtime isolated workspace is excluded the same
# way as crates/patch — it is not created in this WP and must never be
# added to CRATES= below. Local HTTP reqwest in codespace-server is OK.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

CRATES=(crates/domain crates/policy crates/runner crates/store crates/server)
hits=0

scan() {
  local pattern="$1"
  local label="$2"
  local out
  out="$(grep -R -n -E --exclude-dir=target --exclude-dir=.git -e "$pattern" "${CRATES[@]}" || true)"
  if [[ -n "$out" ]]; then
    echo "forbidden ($label):" >&2
    echo "$out" >&2
    hits=1
  fi
}

scan 'api\.openai\.com' 'api.openai.com'
scan 'openai[_-]api' 'openai_api / openai-api'
scan 'responses[_-]api' 'responses-api'
scan 'codex-login' 'codex-login'
scan 'codex-core[[:space:]]*=' 'codex-core cargo dep'
scan 'path[[:space:]]*=[[:space:]]*"[^"]*codex-core' 'codex-core path dep'
scan 'codex-app-server' 'codex-app-server'
scan 'async-openai' 'async-openai'

if [[ "$hits" -ne 0 ]]; then
  echo "Model / Responses / Codex-agent deps are forbidden in codespace crates." >&2
  echo "See docs/execution-substrate.md." >&2
  exit 1
fi

echo "no-model-deps ok"
