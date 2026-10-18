#!/usr/bin/env bash
set -euo pipefail
app_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export XDG_STATE_HOME="$app_dir/local-state"
export PYTHONUNBUFFERED=1
export ISS_LOG_LIBAV="${ISS_LOG_LIBAV:-0}"
cd "$app_dir"
exec "$app_dir/.venv/bin/iss" "$@"
