#!/bin/bash
set -e
TASK_ROOT="$(cd "$(dirname "$0")" && pwd)"
exec /bin/bash "$TASK_ROOT/scripts/build-macos.sh" "$@"
