#!/usr/bin/env bash
# Serve the minarrow docs site on the local network at http://<host>:8002
set -euo pipefail
cd "$(dirname "$0")"

# Prefer the worktree's pyo3 venv (it carries the docs toolchain and minarrow);
# otherwise use the active python.
PY="../pyo3/.venv/bin/python"
[ -x "$PY" ] || PY="python"

exec "$PY" -m mkdocs serve --dev-addr 0.0.0.0:8002
