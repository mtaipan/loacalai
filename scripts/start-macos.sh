#!/bin/bash
set -euo pipefail
TASK_OS_MAJOR="$(sw_vers -productVersion | cut -d. -f1)"
if [ "$TASK_OS_MAJOR" -lt 14 ]; then
  echo 'Local Gemma Studio requires macOS 14 Sonoma or newer for the current Ollama runtime.'
  exit 1
fi
TASK_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export LOCAL_GEMMA_HOME="$TASK_ROOT"
export PYTHONUTF8=1
cd "$TASK_ROOT"
mkdir -p runtime data/logs
TASK_NEEDS_BUILD=0
if [ ! -x LocalGemmaStudio ] || [ "$(cat .build-version 2>/dev/null || true)" != '0.12.1' ]; then TASK_NEEDS_BUILD=1; fi
if [ "$TASK_NEEDS_BUILD" = 1 ] && ! xcode-select -p >/dev/null 2>&1; then
  echo 'macOS needs Command Line Tools to build the Rust app. Complete the Apple installer, then launch this file again.'
  xcode-select --install
  exit 1
fi
TASK_ARCH="$(uname -m)"
if [ "$TASK_ARCH" = arm64 ]; then TASK_ARCH=aarch64; fi
TASK_PYTHON=""
for candidate in python3.13 python3.12 python3.11; do
  if command -v "$candidate" >/dev/null 2>&1; then TASK_PYTHON="$(command -v "$candidate")"; break; fi
done
if [ -z "$TASK_PYTHON" ]; then
  if [ ! -x runtime/python/bin/python3 ]; then
    echo 'Downloading portable Python 3.13 from the official python-build-standalone repository...'
    curl -fL --retry 3 https://api.github.com/repos/astral-sh/python-build-standalone/releases/latest -o runtime/python-release.json
    TASK_PYTHON_URL="$(grep -o '"browser_download_url": *"[^"]*"' runtime/python-release.json | sed -E 's/^"browser_download_url": *"(.*)"$/\1/' | grep -E "cpython-3\.13\..*${TASK_ARCH}-apple-darwin-install_only(_stripped)?\.tar\.gz$" | sed -n '1p')"
    if [ -z "$TASK_PYTHON_URL" ]; then echo 'Portable Python not found. Install Python 3.13, then launch again.'; exit 1; fi
    curl -fL --retry 3 "$TASK_PYTHON_URL" -o runtime/python.tar.gz
    tar -xzf runtime/python.tar.gz -C runtime
  fi
  TASK_PYTHON="$TASK_ROOT/runtime/python/bin/python3"
fi
if [ ! -x runtime/.venv/bin/python ]; then "$TASK_PYTHON" -m venv runtime/.venv; fi
if ! runtime/.venv/bin/python -c 'import faster_whisper, sounddevice, PIL, Vision, pypdfium2, playwright' >/dev/null 2>&1; then
  runtime/.venv/bin/python -m pip install -r runtime/requirements.txt
fi
if [ ! -f data/models/whisper-small/model.bin ]; then runtime/.venv/bin/python runtime/setup_media.py; fi
if ! command -v ollama >/dev/null 2>&1 && [ ! -x runtime/Ollama.app/Contents/Resources/ollama ]; then
  echo 'Downloading Ollama from ollama.com...'
  curl -fL --retry 3 https://ollama.com/download/Ollama-darwin.zip -o runtime/Ollama-darwin.zip
  ditto -xk runtime/Ollama-darwin.zip runtime
fi
if [ -x runtime/Ollama.app/Contents/Resources/ollama ]; then export LOCAL_GEMMA_OLLAMA_BIN="$TASK_ROOT/runtime/Ollama.app/Contents/Resources/ollama"; fi
if [ "$TASK_NEEDS_BUILD" = 1 ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    export CARGO_HOME="$TASK_ROOT/runtime/cargo"
    export RUSTUP_HOME="$TASK_ROOT/runtime/rustup"
    if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
      curl -fL --retry 3 https://sh.rustup.rs -o runtime/rustup-init.sh
      sh runtime/rustup-init.sh -y --profile minimal --no-modify-path
    fi
    export PATH="$CARGO_HOME/bin:$PATH"
  fi
  cargo build --release --locked
  cp target/release/local-gemma-studio LocalGemmaStudio
  printf '0.12.1\n' > .build-version
fi
TASK_PORT="${LOCAL_GEMMA_PORT:-3210}"
TASK_URL="http://127.0.0.1:$TASK_PORT"
export LOCAL_GEMMA_BOOTSTRAP_URL="$TASK_URL"
runtime/.venv/bin/python scripts/upgrade_server.py
TASK_STARTED_PID=""
if ! curl -fsS "$TASK_URL/api/bootstrap" >/dev/null 2>&1; then
  ./LocalGemmaStudio --server >data/logs/server.out.log 2>data/logs/server.err.log &
  TASK_STARTED_PID=$!
fi
for attempt in $(seq 1 60); do
  if curl -fsS "$TASK_URL/api/bootstrap" >/dev/null 2>&1; then break; fi
  sleep .5
done
export LOCAL_GEMMA_BOOTSTRAP_URL="$TASK_URL"
runtime/.venv/bin/python scripts/prepare_models.py
if [ "${1:-}" = --browser ]; then
  open "$TASK_URL"
  echo "Local web app: $TASK_URL"
else
  if [ -n "$TASK_STARTED_PID" ]; then kill "$TASK_STARTED_PID"; wait "$TASK_STARTED_PID" 2>/dev/null || true; fi
  TASK_APP="$TASK_ROOT/Local Gemma Studio.app"
  mkdir -p "$TASK_APP/Contents/MacOS"
  cp LocalGemmaStudio "$TASK_APP/Contents/MacOS/local-gemma-studio"
  cp scripts/Info.plist "$TASK_APP/Contents/Info.plist"
  open "$TASK_APP"
fi
