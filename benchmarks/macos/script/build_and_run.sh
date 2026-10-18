#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:-run}"
duration="${2:-60}"
if [[ ! $duration =~ ^[0-9]+$ ]] || (( duration < 1 || duration > 600 )); then
  echo "Duration must be 1–600 seconds." >&2
  exit 2
fi
bundle="$root/../../native/target/ISSMotionBench.app"
pkill -x ISSMotionBench >/dev/null 2>&1 || true
mkdir -p "$bundle/Contents/MacOS"
xcrun swiftc -O -parse-as-library "$root/Motion.swift" -framework AppKit -framework QuartzCore \
  -o "$bundle/Contents/MacOS/ISSMotionBench"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>ISSMotionBench</string>
<key>CFBundleIdentifier</key><string>org.isharescreen.motionbenchmark</string>
<key>CFBundleName</key><string>iShareScreen Motion Benchmark</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
/usr/bin/open -n "$bundle" --args "$duration"
case "$mode" in
  run) ;;
  --verify) sleep 1; pgrep -x ISSMotionBench >/dev/null ;;
  --debug) sleep 1; lldb -n ISSMotionBench ;;
  --logs|--telemetry) /usr/bin/log stream --info --style compact --predicate 'process == "ISSMotionBench"' ;;
  *) echo "Usage: $0 [run|--verify|--debug|--logs|--telemetry] [duration-seconds]" >&2; exit 2 ;;
esac
