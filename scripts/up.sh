#!/usr/bin/env bash
# Run the stack: swarm (SITL vehicles + the Rust stand-in engine), the Rust bridge and the web
# console. With MURMUR_SCENARIO (make goldengate) also tidewater: `tidewater serve` for the
# bridge and the stand-ins, `tidewater drive` for the SITL vehicles. Ctrl-C (or make down)
# stops everything this script started.
#
# Ports (env), so a second stack can run beside the default one:
#   MURMUR_BASE_PORT       14560  vehicle i sends UDP to base + i
#   MURMUR_HTTP_PORT       8765   bridge REST + WebSocket
#   MURMUR_WEB_PORT        5173   console (vite)
#   MURMUR_TIDEWATER_PORT  8770   tidewater serve (reused if one already answers there)
# MURMUR_TIDEWATER_URL overrides the port with any running `tidewater serve`.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p logs
export PYTHONUNBUFFERED=1
export MURMUR_ROOT="$PWD"
export MURMUR_BASE_PORT="${MURMUR_BASE_PORT:-14560}"
export MURMUR_HTTP_PORT="${MURMUR_HTTP_PORT:-8765}"
web_port="${MURMUR_WEB_PORT:-5173}"
export MURMUR_BRIDGE_URL="http://127.0.0.1:$MURMUR_HTTP_PORT"  # vite proxies here

# build first (a no-op when fresh), so vehicles don't wait on a bridge that is still compiling
cargo="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"
if [ -x "$cargo" ]; then
  "$cargo" build --release -p murmur-bridge -p murmur-standins
elif [ ! -x target/release/murmur-bridge ] || [ ! -x target/release/murmur-standins ]; then
  echo "no cargo and no release binaries in target/release; run make setup" >&2
  exit 1
fi

echo $$ > logs/up.pid
pids=()
cleanup() {
  trap - INT TERM EXIT
  [ ${#pids[@]} -gt 0 ] && kill "${pids[@]}" 2>/dev/null || true
  wait 2>/dev/null || true
  rm -f logs/up.pid logs/tidewater-serve.pid
}
trap cleanup EXIT
trap 'cleanup; exit 0' INT TERM  # a requested stop (Ctrl-C, make down) is a clean exit

# real-world conditions from a tidewater scenario (sibling repo), if one is set
TIDEWATER="${TIDEWATER_HOME:-$HOME/tidewater}/.venv/bin/tidewater"
if [ -n "${MURMUR_SCENARIO:-}" ]; then
  : "${MURMUR_AT:?set MURMUR_AT, e.g. 2026-06-14T10:00Z}"
  tw_port="${MURMUR_TIDEWATER_PORT:-8770}"
  export MURMUR_TIDEWATER_URL="${MURMUR_TIDEWATER_URL:-http://127.0.0.1:$tw_port}"
  if curl -sf -m 2 "$MURMUR_TIDEWATER_URL/v1/info" > /dev/null; then
    echo "tidewater serve: using the one at $MURMUR_TIDEWATER_URL"
  else
    # bridge and stand-ins query it from their first tick, so it must answer before they start
    "$TIDEWATER" serve "$MURMUR_SCENARIO" --port "$tw_port" > logs/tidewater-serve.log 2>&1 & pids+=($!)
    echo "${pids[-1]}" > logs/tidewater-serve.pid
    echo -n "tidewater serve: starting on :$tw_port"
    for _ in $(seq 120); do
      curl -sf -m 2 "$MURMUR_TIDEWATER_URL/v1/info" > /dev/null && break
      kill -0 "${pids[-1]}" 2>/dev/null || { echo; echo "tidewater serve exited; see logs/tidewater-serve.log" >&2; exit 1; }
      echo -n .; sleep 1
    done
    curl -sf -m 2 "$MURMUR_TIDEWATER_URL/v1/info" > /dev/null \
      || { echo; echo "tidewater serve did not answer in 120 s" >&2; exit 1; }
    echo " up"
  fi
fi

# exec in each subshell so the tracked pid is the real process and cleanup reaches it
.venv/bin/python sim/swarm.py --base-port "$MURMUR_BASE_PORT" "$@" > logs/sim.log 2>&1 & pids+=($!)
target/release/murmur-bridge > logs/bridge.log 2>&1 & pids+=($!)
(cd web && exec node_modules/.bin/vite --host 127.0.0.1 --port "$web_port" --strictPort) > logs/web.log 2>&1 & pids+=($!)

if [ -n "${MURMUR_SCENARIO:-}" ]; then
  # tidewater drive pushes conditions into the SITL vehicles over their SERIAL1 ports
  instances=$(.venv/bin/python sim/swarm.py "$@" --sitl-instances)
  if [ -n "$instances" ]; then
    "$TIDEWATER" drive "$MURMUR_SCENARIO" --at "$MURMUR_AT" \
      --rate "${MURMUR_RATE:-1}" --instances "$instances" > logs/tidewater.log 2>&1 & pids+=($!)
  fi
  echo "tidewater: $MURMUR_SCENARIO from $MURMUR_AT (logs/tidewater*.log)"
fi

echo "swarm, bridge (:$MURMUR_HTTP_PORT) and web starting; logs in ./logs"
echo "open http://127.0.0.1:$web_port  (vehicles get a GPS fix ~20-30s after start)"
wait -n "${pids[@]}"
echo "a process exited; see logs/. stopping the rest."
