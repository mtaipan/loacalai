#!/bin/bash
set -euo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Build DMG on macOS.'; exit 1; }
TASK_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
bash "$TASK_ROOT/scripts/build-macos.sh"
mkdir -p "$TASK_ROOT/dist" "$TASK_ROOT/data/temp"
TASK_STAGE="$(mktemp -d "$TASK_ROOT/data/temp/dmg.XXXXXX")"
case "$TASK_STAGE" in "$TASK_ROOT"/data/temp/dmg.*) ;; *) echo 'Unexpected staging path'; exit 1;; esac
trap 'rm -rf -- "$TASK_STAGE"' EXIT
TASK_KIT="$TASK_STAGE/Local Gemma Studio"
mkdir -p "$TASK_KIT/runtime"
for folder in src web scripts; do cp -R "$TASK_ROOT/$folder" "$TASK_KIT/$folder"; done
for source in "$TASK_ROOT"/runtime/*.py "$TASK_ROOT"/runtime/*.ps1 "$TASK_ROOT"/runtime/*.txt; do [ ! -f "$source" ] || cp "$source" "$TASK_KIT/runtime/$(basename "$source")"; done
for source in "$TASK_ROOT"/*.command; do cp "$source" "$TASK_KIT/$(basename "$source")"; done
for name in Cargo.toml Cargo.lock README.md START-HERE.md MODELS.md RELEASE-0.4.md RELEASE-0.13.md WEB-AND-THINKING.md LICENSE; do cp "$TASK_ROOT/$name" "$TASK_KIT/$name"; done
cp "$TASK_ROOT/target/release/local-gemma-studio" "$TASK_KIT/LocalGemmaStudio"
awk -F '"' '/^version =/ {print $2;exit}' "$TASK_ROOT/Cargo.toml" > "$TASK_KIT/.build-version"
cp -R "$TASK_ROOT/Local Gemma Studio.app" "$TASK_KIT/Local Gemma Studio.app"
cat > "$TASK_STAGE/READ-FIRST.txt" <<'TXT'
Copy the entire Local Gemma Studio folder to a writable location, then run Start-macOS.command once to download local runtime components. After setup, open Local Gemma Studio.app. Models, runtime and personal data are not bundled in this DMG.
TXT
chmod +x "$TASK_KIT"/*.command "$TASK_KIT"/scripts/*.sh
hdiutil create -volname 'Local Gemma Studio' -srcfolder "$TASK_STAGE" -format UDZO -ov "$TASK_ROOT/dist/LocalGemmaStudio-macOS-$(uname -m).dmg"
