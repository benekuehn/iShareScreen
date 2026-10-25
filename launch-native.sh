#!/usr/bin/env bash
set -euo pipefail
app_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export XDG_STATE_HOME="$app_dir/local-state"
native_cli="$app_dir/client/target/release/iss-native"
export LD_LIBRARY_PATH="$app_dir/client/target/release${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
if [[ ! -x $native_cli ]]; then
  echo "Build the native client first with scripts/build-on-mini.sh." >&2
  exit 1
fi
exec "$native_cli" --last "$@"
