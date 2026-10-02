# ArduPilot patches (non-physics)

Fixes to ArduPilot itself that Murmur relies on, on branch `fix/sitl-tracker-streams` of
`~/ardupilot` (from `tidewater-water-depth`). SITL physics patches live in
`~/tidewater/patches/ardupilot/` instead. Apply with `git am` and rebuild (see CLAUDE.md).

- `0001-AP_Param-...`: a `--defaults a.parm,b.parm` list kept only the last file's "all names
  found" flag, so MAVn_* stream rates set in an earlier file missed the MAVLink channels and they
  streamed built-in rates (murmur-yjq). Murmur also avoids it without the patch: `sim/swarm.py`
  passes one merged defaults file.
- `0002-AntennaTracker-...`: with YAW_RANGE under 360 the tracker never took the long way round
  to a target past its yaw limit, and its limit estimate lagged a slewing mount (murmur-9bf).
