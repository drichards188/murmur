#!/usr/bin/env bash
# Run swarm + bridge + web together. Ctrl-C stops everything.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p logs
export PYTHONUNBUFFERED=1
echo $$ > logs/up.pid
pids=()
cleanup() { trap - INT TERM EXIT; kill "${pids[@]}" 2>/dev/null || true; wait 2>/dev/null || true; rm -f logs/up.pid; }
trap cleanup INT TERM EXIT

# exec in each subshell so the tracked pid is the real process and cleanup reaches it
.venv/bin/python sim/swarm.py "$@" > logs/sim.log 2>&1 & pids+=($!)
(cd bridge && exec ../.venv/bin/python -m murmur_bridge) > logs/bridge.log 2>&1 & pids+=($!)
(cd web && exec node_modules/.bin/vite --host 127.0.0.1 --strictPort) > logs/web.log 2>&1 & pids+=($!)

# real-world conditions from a tidewater scenario (sibling repo), if one is set
if [ -n "${MURMUR_SCENARIO:-}" ]; then
  TIDEWATER="${TIDEWATER_HOME:-$HOME/tidewater}/.venv/bin/tidewater"
  n=$(.venv/bin/python sim/swarm.py "$@" --count-only)
  "$TIDEWATER" drive "$MURMUR_SCENARIO" --at "${MURMUR_AT:?set MURMUR_AT, e.g. 2026-06-14T10:00Z}" \
    --rate "${MURMUR_RATE:-1}" --instances "0-$((n - 1))" > logs/tidewater.log 2>&1 & pids+=($!)
  echo "tidewater: $MURMUR_SCENARIO from $MURMUR_AT (logs/tidewater.log)"
fi

echo "swarm, bridge and web starting; logs in ./logs"
echo "open http://127.0.0.1:5173  (vehicles get a GPS fix ~20-30s after start)"
wait -n "${pids[@]}"
echo "a process exited; see logs/. stopping the rest."
