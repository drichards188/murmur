#!/usr/bin/env bash
# Apply tidewater's SITL patches to an ArduPilot checkout on a branch, then rebuild.
#   ARDUPILOT_HOME=~/ardupilot ./scripts/patch-ardupilot.sh
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
AP="${ARDUPILOT_HOME:-$HOME/ardupilot}"
BRANCH=tidewater-water-depth
cd "$AP"
if git rev-parse --verify -q "$BRANCH" >/dev/null; then
  git checkout -q "$BRANCH"
  echo "branch $BRANCH already exists; checked out"
else
  git checkout -q -b "$BRANCH"
  git -c user.name=tidewater -c user.email=tidewater@localhost am -q "$here"/patches/ardupilot/*.patch
  echo "applied $(ls "$here"/patches/ardupilot/*.patch | wc -l) patch(es) on $BRANCH"
fi
# all vehicles share the SIM param table, so rebuild everything that's been built
./waf configure --board sitl >/dev/null
./waf plane copter rover sub blimp antennatracker
