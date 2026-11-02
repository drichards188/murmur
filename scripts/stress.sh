#!/usr/bin/env bash
# The console under load, to watch yourself (docs/load-test.md, "Console with the camera moving, on a real GPU"):
# N stand-ins spread over the Bay (scripts/scale_fleet.py), or a dense swarm with --swarm, on
# the default stack's ports, no link emulator, no tidewater, the production build of the
# console (MURMUR_WEB=preview). N=0 serves the console alone, with no bridge: the map with no
# vehicles. `make down` stops it.
#
#   scripts/stress.sh 3600            # make stress
#   scripts/stress.sh 800 --swarm     # make stress N=800 SWARM=1
#   scripts/stress.sh 0               # make stress N=0
set -euo pipefail
cd "$(dirname "$0")/.."
n="${1:-3600}"
swarm="${2:-}"
[[ "$n" =~ ^[0-9]+$ ]] || { echo "stress: N must be a number of vehicles (got '$n')" >&2; exit 1; }
web_port="${MURMUR_WEB_PORT:-5173}"
http_port="${MURMUR_HTTP_PORT:-8765}"
base="${MURMUR_BASE_PORT:-14560}"

# each stand-in sends from a fixed UDP port (MURMUR_STANDIN_SRC_BASE + i), not an ephemeral one:
# the host's ephemeral range (~4,000 here, shared) is what capped a fleet before (docs/load-test.md)
export MURMUR_STANDIN_SRC_BASE="${MURMUR_STANDIN_SRC_BASE:-20000}"
src_base="$MURMUR_STANDIN_SRC_BASE"

# refuse when a stack already answers here, or a range we bind is taken
udp_taken() { ss -uanH | awk -v lo="$1" -v hi="$2" '{k = split($4, a, ":"); p = a[k]} p >= lo && p < hi {f = 1} END {exit !f}'; }
taken=()
for p in "$web_port" "$http_port"; do
  ss -ltnH "sport = :$p" | grep -q . && taken+=("TCP $p")
done
if [ "$n" -gt 0 ]; then
  udp_taken "$base" "$((base + n))" && taken+=("UDP $base-$((base + n - 1))")
  udp_taken "$src_base" "$((src_base + n))" && taken+=("UDP $src_base-$((src_base + n - 1))")
fi
if [ -f logs/up.pid ] && kill -0 "$(cat logs/up.pid)" 2>/dev/null; then taken+=("this checkout's stack (logs/up.pid)"); fi
if [ ${#taken[@]} -gt 0 ]; then
  echo "stress: something already answers on ${taken[*]}: a running Murmur stack? Stop it with make down (or the menu's End), then try again." >&2
  exit 1
fi

# the fixed ranges must sit below the ephemeral range, or clients drawing from it collide at random
if [ "$n" -gt 0 ]; then
  read -r lo _ < /proc/sys/net/ipv4/ip_local_port_range
  for r in "$base" "$src_base"; do
    if [ $((r + n)) -gt "$lo" ]; then
      echo "stress: UDP $r-$((r + n - 1)) runs into this host's ephemeral range (from $lo): pick a smaller N," >&2
      echo "        or a lower MURMUR_BASE_PORT / MURMUR_STANDIN_SRC_BASE." >&2
      exit 1
    fi
  done
fi

mkdir -p logs sim/run
echo "stress: open http://localhost:$web_port/?perf (the overlay shows fps, p95 frame time and update latency); make down stops it"
if [ "$n" -eq 0 ]; then
  # the map with no vehicles: the production console alone, no bridge (it shows the bridge as down)
  echo $$ > logs/up.pid
  pid=
  cleanup() { trap - INT TERM EXIT; [ -n "$pid" ] && kill "$pid" 2>/dev/null; wait 2>/dev/null; rm -f logs/up.pid; }
  trap cleanup EXIT
  trap 'cleanup; exit 0' INT TERM
  (cd web && node_modules/.bin/vite build --logLevel warn) > logs/web.log 2>&1
  (cd web && exec node_modules/.bin/vite preview --host 127.0.0.1 --port "$web_port" --strictPort) >> logs/web.log 2>&1 &
  pid=$!
  echo "stress: the console with no vehicles on :$web_port (no bridge)"
  wait "$pid"
  exit 0
fi

fleet="sim/run/stress-$n${swarm:+-swarm}.toml"
if [ "$swarm" = --swarm ]; then
  # about 1 km2 just inside the Golden Gate, the small fixed-wing types (they fly from the start)
  .venv/bin/python scripts/scale_fleet.py "$n" --swarm=-122.465,37.818,560 -o "$fleet"
else
  .venv/bin/python scripts/scale_fleet.py "$n" -o "$fleet"
fi
export MURMUR_LINKEMU=0 MURMUR_WEB="${MURMUR_WEB:-preview}"
unset MURMUR_SCENARIO MURMUR_CONTACTS
exec ./scripts/up.sh --fleet "$fleet"
