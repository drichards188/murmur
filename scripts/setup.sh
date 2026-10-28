#!/usr/bin/env bash
# One-time setup: Python venv (fleet launcher, test scripts, ArduPilot build deps, tidewater
# and its scenarios' data), the Rust bridge and stand-in engine (release), ArduPilot SITL with
# Murmur's patches, web deps.
# No sudo needed: build deps go in .venv instead of install-prereqs-ubuntu.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
ARDUPILOT_HOME="${ARDUPILOT_HOME:-$HOME/ardupilot}"
# The ArduPilot commit Murmur's patches were made on (master, 2 Oct 2026, "AP_Motors: merge the
# three Sub's output functions"), the base of the branch they were written on. A fresh clone is
# this commit plus scripts/ardupilot-patches.sh's patches; moving it means checking that each
# patch still applies, and rebuilding.
ARDUPILOT_PIN=e8de77a80535df7a78ff78564b35781fba6a5b2f
# The tidewater scenarios Murmur runs: the open world (make goldengate, make start) and every
# live situation (sim/situations, through sim/worlds/golden-gate.toml). San Diego's
# (sim/worlds/san-diego.tidewater.toml) only re-bakes its world's headless grids
# (sim/worlds/README.md), so it is not fetched here.
TIDEWATER_SCENARIOS=(tidewater/scenarios/golden-gate-2026-06.toml)

echo "== python venv"
command -v uv >/dev/null || { echo "install uv first: https://docs.astral.sh/uv/"; exit 1; }
[ -d .venv ] || uv venv -q .venv --python 3.12
uv pip install -q --python .venv/bin/python -r requirements.txt

echo "== tidewater (tidewater/): installed editable into .venv, its scenarios' data fetched once"
# sfbofs: h5py, which fetching the Golden Gate's model currents needs (running them does not)
uv pip install -q --python .venv/bin/python -e './tidewater[sfbofs]'
for sc in "${TIDEWATER_SCENARIOS[@]}"; do
  # a layer already in tidewater/data/cache/<scenario>/ is kept, not fetched again
  .venv/bin/tidewater fetch "$sc"
done

echo "== Rust: murmur-bridge, murmur-standins (release)"
cargo="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"
[ -x "$cargo" ] || { echo "install Rust first: https://rustup.rs/"; exit 1; }
"$cargo" build --release -p murmur-bridge -p murmur-standins

echo "== ArduPilot SITL at $ARDUPILOT_HOME"
if [ ! -d "$ARDUPILOT_HOME" ]; then
  # A fresh clone: the pinned commit (fetched by its hash, depth 1), its submodules, and the
  # patches on branch tidewater-water-depth, made beside it and moved into place once complete.
  # core.autocrlf=false: a Windows-style global autocrlf breaks ArduPilot's python shebangs
  # ("/usr/bin/env: python3\r") when building under WSL.
  partial="$ARDUPILOT_HOME.partial"
  rm -rf "$partial"
  git init -q "$partial"
  git -C "$partial" config core.autocrlf false
  git -C "$partial" remote add origin https://github.com/ArduPilot/ardupilot
  git -C "$partial" fetch -q --depth 1 origin "$ARDUPILOT_PIN"
  git -C "$partial" checkout -q -b tidewater-water-depth FETCH_HEAD
  git -C "$partial" -c core.autocrlf=false submodule update -q --init --recursive --depth 1
  scripts/ardupilot-patches.sh apply "$partial"
  mv "$partial" "$ARDUPILOT_HOME"
elif ! scripts/ardupilot-patches.sh check "$ARDUPILOT_HOME"; then
  # an existing checkout is never rewritten here: it may hold someone's own work
  echo "WARNING: $ARDUPILOT_HOME lacks the patches above. Murmur's boats, subs and tracker need"
  echo "  them (water depth, grounding and currents; the tracker's mount and yaw limits). Apply"
  echo "  them on a branch of a commit near $ARDUPILOT_PIN with"
  echo "    scripts/ardupilot-patches.sh apply $ARDUPILOT_HOME"
  echo "  or move it aside and run make setup again for a fresh clone. Building it as it is."
fi
(
  cd "$ARDUPILOT_HOME"
  export PATH="$OLDPWD/.venv/bin:$PATH"
  ./waf configure --board sitl
  ./waf plane copter rover sub blimp antennatracker
)

echo "== web deps"
(cd web && npm install --silent)

echo "done. run: make start"
