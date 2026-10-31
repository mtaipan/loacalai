#!/bin/bash
set -euo pipefail
TASK_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$TASK_ROOT"
command -v cargo >/dev/null || { echo 'Install Rust via https://rustup.rs first.'; exit 1; }
xcode-select -p >/dev/null || { xcode-select --install; exit 1; }
cargo build --release --locked
TASK_APP="$TASK_ROOT/Local Gemma Studio.app"
mkdir -p "$TASK_APP/Contents/MacOS" "$TASK_APP/Contents/Resources"
cp target/release/local-gemma-studio "$TASK_APP/Contents/MacOS/local-gemma-studio"
cp scripts/Info.plist "$TASK_APP/Contents/Info.plist"
codesign --force --deep --sign - "$TASK_APP"
echo 'Native app built. Run Start-macOS.command to prepare local runtime and start it.'
