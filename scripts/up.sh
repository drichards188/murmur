#!/usr/bin/env bash
# Run swarm + bridge + web together. Ctrl-C stops everything.
# MURMUR_BRIDGE=python|rust picks the bridge (default python). Another stack beside the
# default one: pass --base-port to the swarm and set MURMUR_BASE_PORT, MURMUR_HTTP_PORT and
# MURMUR_WEB_PORT to match.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p logs
export PYTHONUNBUFFERED=1
bridge="${MURMUR_BRIDGE:-python}"
case "$bridge" in
  python) ;;
  rust)
    # build first, so vehicles don't wait on a bridge that is still compiling
    cargo="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"
    "$cargo" build --release -p murmur-bridge ;;
  *) echo "MURMUR_BRIDGE must be python or rust, not $bridge" >&2; exit 2 ;;
esac
echo $$ > logs/up.pid
pids=()
cleanup() { trap - INT TERM EXIT; kill "${pids[@]}" 2>/dev/null || true; wait 2>/dev/null || true; rm -f logs/up.pid; }
trap cleanup INT TERM EXIT

# exec in each subshell so the tracked pid is the real process and cleanup reaches it
.venv/bin/python sim/swarm.py "$@" > logs/sim.log 2>&1 & pids+=($!)
if [ "$bridge" = rust ]; then
  target/release/murmur-bridge > logs/bridge.log 2>&1 & pids+=($!)
else
  (cd bridge && exec ../.venv/bin/python -m murmur_bridge) > logs/bridge.log 2>&1 & pids+=($!)
fi
web_port="${MURMUR_WEB_PORT:-5173}"
(cd web && exec node_modules/.bin/vite --host 127.0.0.1 --port "$web_port" --strictPort) > logs/web.log 2>&1 & pids+=($!)

# real-world conditions from a tidewater scenario (sibling repo), if one is set
if [ -n "${MURMUR_SCENARIO:-}" ]; then
  TIDEWATER="${TIDEWATER_HOME:-$HOME/tidewater}/.venv/bin/tidewater"
  # tidewater drives the SITL vehicles over their SERIAL1 ports (stand-ins aren't SITL)
  instances=$(.venv/bin/python sim/swarm.py "$@" --sitl-instances)
  "$TIDEWATER" drive "$MURMUR_SCENARIO" --at "${MURMUR_AT:?set MURMUR_AT, e.g. 2026-06-14T10:00Z}" \
    --rate "${MURMUR_RATE:-1}" --instances "$instances" > logs/tidewater.log 2>&1 & pids+=($!)
  echo "tidewater: $MURMUR_SCENARIO from $MURMUR_AT (logs/tidewater.log)"
fi

echo "swarm, $bridge bridge and web starting; logs in ./logs"
echo "open http://127.0.0.1:$web_port  (vehicles get a GPS fix ~20-30s after start)"
wait -n "${pids[@]}"
echo "a process exited; see logs/. stopping the rest."
