#!/bin/bash
set -euo pipefail
TASK_ROOT="$(cd "$(dirname "$0")" && pwd)"
TASK_URL="http://127.0.0.1:${LOCAL_GEMMA_PORT:-3210}"
export LOCAL_GEMMA_BOOTSTRAP_URL="$TASK_URL"
"$TASK_ROOT/runtime/.venv/bin/python" "$TASK_ROOT/scripts/stop.py"
