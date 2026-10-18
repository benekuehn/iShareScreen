#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 2 ]]; then
  echo "Usage: $0 SSH_HOST ABSOLUTE_REMOTE_REPO_DIRECTORY" >&2
  exit 2
fi
build_host=$1
build_dir=$2
if [[ $build_dir != /* || $build_dir == *[!a-zA-Z0-9_./-]* ]]; then
  echo "Remote path must be absolute and contain only letters, digits, _, ., / or -." >&2
  exit 2
fi
app_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
rsync -az --exclude=.git --exclude='.venv*' --exclude=target \
  --exclude=local-state --exclude=__pycache__ --exclude='*.log' \
  "$app_dir/" "$build_host:$build_dir/"
ssh "$build_host" 'export PATH=/opt/homebrew/bin:$PATH; container exec -w /work/native isharescreen-build cargo test --locked && container exec -w /work/native isharescreen-build cargo build --release --locked'
mkdir -p "$app_dir/native/target/release"
rsync -az "$build_host:$build_dir/native/target/release/libisharescreen_native.so" \
  "$app_dir/native/target/release/"
