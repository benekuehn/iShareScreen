#!/usr/bin/env bash
set -euo pipefail
app_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export XDG_STATE_HOME="$app_dir/local-state"
native_cli="$app_dir/client/target/release/iss-native"
if [[ ! -x $native_cli ]]; then
  echo "Build the native client first with scripts/build-on-mini.sh." >&2
  exit 1
fi
exec "$native_cli" --last "$@"
