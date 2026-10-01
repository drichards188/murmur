#!/usr/bin/env bash
# One-time setup: Python venv, ArduPilot SITL build (plane + rover), web deps.
# No sudo needed: build deps go in .venv instead of install-prereqs-ubuntu.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
ARDUPILOT_HOME="${ARDUPILOT_HOME:-$HOME/ardupilot}"

echo "== python venv"
command -v uv >/dev/null || { echo "install uv first: https://docs.astral.sh/uv/"; exit 1; }
[ -d .venv ] || uv venv -q .venv --python 3.12
uv pip install -q --python .venv/bin/python -r bridge/requirements.txt

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
  ./waf plane rover
)

echo "== web deps"
(cd web && npm install --silent)
echo "done. run: make up"
