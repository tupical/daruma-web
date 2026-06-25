#!/usr/bin/env bash
# Canonical local dev env: ~/.agents/daruma/data + sibling daruma OSS.
# Usage:
#   eval "$(./scripts/dev-stack.sh)"
#   ./scripts/dev-stack.sh server
#   ./scripts/dev-stack.sh print-url
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OSS_ROOT="${DARUMA_OSS_ROOT:-$(cd "$ROOT/../taskagent" && pwd)}"
AGENT_HOME="${DARUMA_AGENT_DIR:-${HOME:?HOME unset}/.agents/daruma}"
DATA_DIR="${DARUMA_DATA_DIR:-$AGENT_HOME/data}"

export DARUMA_DATA_DIR="$DATA_DIR"
export DARUMA_OSS_ROOT="$OSS_ROOT"

emit_env() {
  cat <<EOF
export DARUMA_DATA_DIR='$DATA_DIR'
export DARUMA_OSS_ROOT='$OSS_ROOT'
EOF
}

cmd="${1:-env}"

case "$cmd" in
  env)
    emit_env
  ;;
  server)
    mkdir -p "$DATA_DIR"
    emit_env
    cd "$OSS_ROOT"
    exec env DARUMA_DATA_DIR="$DATA_DIR" \
      cargo run -p daruma-server
  ;;
  print-url)
    token_file="$DATA_DIR/bootstrap.token"
    if [[ ! -f "$token_file" ]]; then
      echo "bootstrap.token missing — run: ./scripts/dev-stack.sh server" >&2
      exit 1
    fi
    token="$(tr -d '\n' <"$token_file")"
    printf 'http://127.0.0.1:5174/?token=%s\n' "$token"
    printf 'http://127.0.0.1:5174/workspaces\n'
    printf '# start UI: NO_COLOR=false trunk serve --config Trunk.dev.toml\n'
  ;;
  *)
    echo "usage: $0 [env|server|print-url]" >&2
    exit 2
  ;;
esac
