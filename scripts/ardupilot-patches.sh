#!/usr/bin/env bash
# The ArduPilot patches Murmur needs, in the order they apply on the pinned commit (scripts/setup.sh):
#   scripts/ardupilot-patches.sh apply <ardupilot dir>   git am each one not already there
#   scripts/ardupilot-patches.sh check <ardupilot dir>   name each one missing (exit 1 if any)
# A patch counts as there when its changes are in the working tree (it reverses cleanly).
# SITL physics (tidewater/patches/ardupilot/) and fixes to ArduPilot itself
# (sim/patches/ardupilot/); after applying, rebuild every firmware (setup.sh does).
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
PATCHES=(
  tidewater/patches/ardupilot/0001-SITL-SIM_WATER_DEPTH-for-marine-models.patch
  sim/patches/ardupilot/0001-AP_Param-keep-defaults-pending-when-any-file-in-a-li.patch
  sim/patches/ardupilot/0002-AntennaTracker-go-the-long-way-round-when-the-short-.patch
  tidewater/patches/ardupilot/0002-SITL-centre-the-tracker-mount-on-its-start-heading-a.patch
)
[ $# -eq 2 ] || { echo "usage: $0 apply|check <ardupilot dir>" >&2; exit 2; }
what="$1"
ap="$2"
there() { git -C "$ap" apply --reverse --check "$repo/$1" 2>/dev/null; }
case "$what" in
  apply)
    for p in "${PATCHES[@]}"; do
      if there "$p"; then
        echo "  already there: $p"
      else
        git -C "$ap" -c core.autocrlf=false -c user.name=murmur -c user.email=murmur@localhost am -q "$repo/$p"
        echo "  applied: $p"
      fi
    done
    ;;
  check)
    missing=0
    for p in "${PATCHES[@]}"; do
      there "$p" || { echo "  missing: $p"; missing=1; }
    done
    exit "$missing"
    ;;
  *) echo "usage: $0 apply|check <ardupilot dir>" >&2; exit 2 ;;
esac
