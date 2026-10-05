#!/usr/bin/env bash
# Run the stack: swarm (SITL vehicles + the Rust stand-in engine), the link emulator, the Rust
# bridge and the web console. With MURMUR_SCENARIO (make goldengate) also tidewater:
# `tidewater serve` for the bridge, the stand-ins and the emulator's terrain line of sight,
# `tidewater drive` for the SITL vehicles. Ctrl-C (or make down) stops everything this script
# started.
#
# Every vehicle's link goes through murmur-linkemu (crates/murmur-linkemu) by default:
#   vehicle i -> UDP MURMUR_BASE_PORT+i -> linkemu -> UDP MURMUR_BRIDGE_BASE_PORT+i -> bridge
# degraded per link class (sim/links.toml): loss, latency, bandwidth, terrain (with a
# scenario), SATCOM from real satellites (with sim/space/cache, `murmur-space fetch`), depth.
# MURMUR_LINKEMU=0 bypasses it (vehicles talk to the bridge on MURMUR_BASE_PORT directly).
# MURMUR_LINK_EVENTS (or MURMUR_EVENTS)=<file> adds scripted [[event]]s for comms and GNSS
# (e.g. sim/link-events/golden-gate.toml, sim/events/golden-gate-gnss.toml);
# MURMUR_LINKS=<file> replaces sim/links.toml (profiles). Emulator log: logs/linkemu.log.
#
# Ports (env), so a second stack can run beside the default one:
#   MURMUR_BASE_PORT         14560  vehicle i sends UDP to base + i (the emulator listens there)
#   MURMUR_BRIDGE_BASE_PORT  base+1000  the bridge's UDP ports behind the emulator
#   MURMUR_LINKEMU_PORT      8766   emulator stats HTTP (/stats, /stats/<uid>, /health)
#   MURMUR_HTTP_PORT         8765   bridge REST + WebSocket
#   MURMUR_WEB_PORT          5173   console (vite)
#   MURMUR_TIDEWATER_PORT    8770   tidewater serve (reused if one already answers there)
#   MURMUR_SITL_INSTANCE_BASE 0     first SITL instance (TCP 5760 + 10*instance)
# MURMUR_TIDEWATER_URL overrides the port with any running `tidewater serve`. The scenario
# clock (MURMUR_AT, MURMUR_RATE) goes to the stand-ins, tidewater drive, the emulator and
# the contacts process.
# MURMUR_CONTACTS=sim/scenarios/<name>.toml also runs the contacts process (contacts that
# are not ours, Phase C2; truth to the bridge's simulation channel and logs/contacts-truth.jsonl).
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p logs
export PYTHONUNBUFFERED=1
export MURMUR_ROOT="$PWD"
export MURMUR_BASE_PORT="${MURMUR_BASE_PORT:-14560}"
export MURMUR_HTTP_PORT="${MURMUR_HTTP_PORT:-8765}"
web_port="${MURMUR_WEB_PORT:-5173}"
export MURMUR_BRIDGE_URL="http://127.0.0.1:$MURMUR_HTTP_PORT"  # vite proxies here
case "${MURMUR_LINKEMU:-1}" in 0|off|false|no) linkemu=0 ;; *) linkemu=1 ;; esac
vehicle_base="$MURMUR_BASE_PORT"
bridge_base="${MURMUR_BRIDGE_BASE_PORT:-$((vehicle_base + 1000))}"
emu_port="${MURMUR_LINKEMU_PORT:-8766}"
# one scripted-events file drives both the link emulator (comms) and the stand-ins (GNSS
# jamming/spoofing); either variable name works
MURMUR_LINK_EVENTS="${MURMUR_LINK_EVENTS:-${MURMUR_EVENTS:-}}"
export MURMUR_EVENTS="$MURMUR_LINK_EVENTS"
if [ -n "${MURMUR_LINK_EVENTS:-}" ] && [ ! -f "$MURMUR_LINK_EVENTS" ]; then
  echo "MURMUR_LINK_EVENTS: no file $MURMUR_LINK_EVENTS" >&2
  exit 1
fi

# build first (a no-op when fresh), so vehicles don't wait on a bridge that is still compiling
cargo="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"
if [ -x "$cargo" ]; then
  "$cargo" build --release -p murmur-bridge -p murmur-standins -p murmur-linkemu -p murmur-decide -p murmur-contacts -p murmur-auth
elif [ ! -x target/release/murmur-bridge ] || [ ! -x target/release/murmur-standins ] \
  || { [ "$linkemu" = 1 ] && [ ! -x target/release/murmur-linkemu ]; } \
  || { [ -n "${MURMUR_CONTACTS:-}" ] && [ ! -x target/release/murmur-contacts ]; }; then
  echo "no cargo and no release binaries in target/release; run make setup" >&2
  exit 1
fi

# who may act (rules 7): operators log in with their own password from the credential file
# (murmur-auth; MURMUR_CREDENTIALS, else ~/.config/murmur/credentials.json); MURMUR_AUTH=dev
# asserts ids by header instead, logged as such (no human-control claim)
export MURMUR_WEB_PORT="$web_port"  # the console's origin, which the bridge accepts requests from
if [ "${MURMUR_AUTH:-credentials}" != dev ] && [ -x target/release/murmur-auth ]; then
  creds="$(target/release/murmur-auth path)"
  if [ ! -s "$creds" ]; then
    echo "no operator credentials at $creds. Add one (password on stdin), e.g." >&2
    echo "  target/release/murmur-auth add op-1 --engage" >&2
    echo "or run with MURMUR_AUTH=dev for header ids (logged as dev, no human-control claim)." >&2
    exit 1
  fi
fi

echo $$ > logs/up.pid
pids=()
cleanup() {
  trap - INT TERM EXIT
  [ ${#pids[@]} -gt 0 ] && kill "${pids[@]}" 2>/dev/null || true
  wait 2>/dev/null || true
  rm -f logs/up.pid logs/tidewater-serve.pid logs/linkemu.pid
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

bridge_env=()
if [ "$linkemu" = 1 ]; then
  # the manifest first, so the emulator binds every vehicle port before any vehicle sends
  .venv/bin/python sim/swarm.py --base-port "$vehicle_base" "$@" --manifest-only
  emu_args=(--fleet sim/run/fleet.json --config "${MURMUR_LINKS:-sim/links.toml}" --vehicle-base "$vehicle_base"
            --bridge-base "$bridge_base" --manifest-out sim/run/bridge-fleet.json
            --http "127.0.0.1:$emu_port" --log-interval 10)
  [ -n "${MURMUR_LINK_EVENTS:-}" ] && emu_args+=(--scenario "$MURMUR_LINK_EVENTS")
  # terrain line of sight needs tidewater (MURMUR_TIDEWATER_URL, set above with a scenario);
  # SATCOM follows real satellites when sim/space/cache exists, else static numbers
  [ -d sim/space/cache ] || echo "linkemu: no sim/space/cache (murmur-space fetch): SATCOM uses static numbers"
  rm -f sim/run/bridge-fleet.json
  target/release/murmur-linkemu "${emu_args[@]}" > logs/linkemu.log 2>&1 & pids+=($!)
  echo "${pids[-1]}" > logs/linkemu.pid
  echo -n "linkemu: starting (vehicles :$vehicle_base+ -> bridge :$bridge_base+, stats :$emu_port)"
  for _ in $(seq 120); do
    curl -sf -m 2 "http://127.0.0.1:$emu_port/health" > /dev/null && break
    kill -0 "${pids[-1]}" 2>/dev/null || { echo; echo "linkemu exited; see logs/linkemu.log" >&2; exit 1; }
    echo -n .; sleep 1
  done
  curl -sf -m 2 "http://127.0.0.1:$emu_port/health" > /dev/null \
    || { echo; echo "linkemu did not answer in 120 s" >&2; exit 1; }
  echo " up"
  bridge_env=(MURMUR_BASE_PORT="$bridge_base" MURMUR_FLEET_MANIFEST="$PWD/sim/run/bridge-fleet.json"
              MURMUR_LINKEMU_URL="http://127.0.0.1:$emu_port")
else
  echo "linkemu: off (MURMUR_LINKEMU=0); vehicles talk to the bridge directly"
fi

# exec in each subshell so the tracked pid is the real process and cleanup reaches it
.venv/bin/python sim/swarm.py --base-port "$vehicle_base" "$@" > logs/sim.log 2>&1 & pids+=($!)
env "${bridge_env[@]}" target/release/murmur-bridge > logs/bridge.log 2>&1 & pids+=($!)
(cd web && exec node_modules/.bin/vite --host 127.0.0.1 --port "$web_port" --strictPort) > logs/web.log 2>&1 & pids+=($!)
if [ -n "${MURMUR_CONTACTS:-}" ]; then
  # contacts that are not ours: truth goes only to the bridge's simulation channel
  target/release/murmur-contacts "$MURMUR_CONTACTS" > logs/contacts.log 2>&1 & pids+=($!)
  echo "contacts: $MURMUR_CONTACTS (logs/contacts.log, truth in logs/contacts-truth.jsonl)"
fi

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
