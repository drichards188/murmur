#!/usr/bin/env bash
# One-time setup: Python venv (fleet launcher, test scripts, ArduPilot build deps), the Rust
# bridge and stand-in engine (release), ArduPilot SITL, web deps.
# No sudo needed: build deps go in .venv instead of install-prereqs-ubuntu.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
ARDUPILOT_HOME="${ARDUPILOT_HOME:-$HOME/ardupilot}"

echo "== python venv"
command -v uv >/dev/null || { echo "install uv first: https://docs.astral.sh/uv/"; exit 1; }
[ -d .venv ] || uv venv -q .venv --python 3.12
uv pip install -q --python .venv/bin/python -r requirements.txt

echo "== Rust: murmur-bridge, murmur-standins (release)"
cargo="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"
[ -x "$cargo" ] || { echo "install Rust first: https://rustup.rs/"; exit 1; }
"$cargo" build --release -p murmur-bridge -p murmur-standins

echo "== ArduPilot SITL at $ARDUPILOT_HOME"
if [ ! -d "$ARDUPILOT_HOME" ]; then
  # -c core.autocrlf=false: a Windows-style global autocrlf breaks ArduPilot's
  # python shebangs ("/usr/bin/env: python3\r") when building under WSL.
  git -c core.autocrlf=false clone --depth 1 --recurse-submodules --shallow-submodules \
    https://github.com/ArduPilot/ardupilot "$ARDUPILOT_HOME"
fi
(
  cd "$ARDUPILOT_HOME"
  export PATH="$OLDPWD/.venv/bin:$PATH"
  ./waf configure --board sitl
  ./waf plane copter rover sub blimp antennatracker
)

echo "== web deps"
(cd web && npm install --silent)

TIDEWATER_HOME="${TIDEWATER_HOME:-$HOME/tidewater}"
if [ ! -x "$TIDEWATER_HOME/.venv/bin/tidewater" ]; then
  echo "note: no tidewater at $TIDEWATER_HOME (needed for make goldengate; see its README)"
fi
echo "done. run: make up"
