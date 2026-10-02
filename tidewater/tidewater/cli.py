import argparse
import json
import sys

from .scenario import Scenario, parse_time


def cmd_fetch(args):
    from .fetch import fetch_all

    sc = Scenario.load(args.scenario)
    print(f"{sc.title}\ncache: {sc.cache_dir}")
    for p in fetch_all(sc):
        print(f"  ok {p.name}")


def cmd_probe(args):
    from .conditions import Conditions

    sc = Scenario.load(args.scenario)
    cond = Conditions.for_scenario(sc)
    t = parse_time(args.at)
    points = [(args.lat, args.lon, "point")] if args.lat is not None else [
        (v[0], v[1], f"spawn:{k}") for k, v in sc.spawn.items()]
    for lat, lon, label in points:
        s = cond.at(lat, lon, t, args.depth)
        if args.json:
            print(json.dumps({"label": label, **s.as_dict()}))
            continue
        if not s.in_area:
            print(f"{label:14} outside scenario area")
            continue
        cs, cd = s.current
        ws, wd = s.wind
        ground = f"water {s.water_depth_m:6.1f} m deep" if s.wet else f"land  {-s.water_depth_m:6.1f} m above water"
        print(f"{label:14} {ground} | tide {s.water_level_m:+.2f} m | "
              f"current {cs:4.2f} m/s toward {cd:3.0f} | wind {ws:4.1f} m/s toward {wd:3.0f}")


def cmd_drive(args):
    from .drivers.ardupilot import run

    run(args)


def main(argv=None):
    ap = argparse.ArgumentParser(prog="tidewater", description="Real-world conditions for robot simulators.")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("fetch", help="download and cache a scenario's data")
    p.add_argument("scenario")
    p.set_defaults(fn=cmd_fetch)

    p = sub.add_parser("probe", help="print conditions at a point (or at every spawn point)")
    p.add_argument("scenario")
    p.add_argument("--at", required=True, help="UTC time, e.g. 2026-06-14T18:00Z")
    p.add_argument("--lat", type=float)
    p.add_argument("--lon", type=float)
    p.add_argument("--depth", type=float, default=0.0, help="metres below the surface (subs)")
    p.add_argument("--json", action="store_true")
    p.set_defaults(fn=cmd_probe)

    p = sub.add_parser("drive", help="push conditions into running ArduPilot SITL vehicles")
    p.add_argument("scenario")
    p.add_argument("--at", required=True, help="scenario time at start, UTC")
    p.add_argument("--rate", type=float, default=1.0, help="scenario seconds per wall second")
    p.add_argument("--instances", default="0", help="SITL instance numbers, e.g. 0-12 or 9,10,11")
    p.add_argument("--base-port", type=int, default=5762, help="SITL SERIAL1 TCP port for instance 0")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--period", type=float, default=2.0, help="seconds between updates")
    p.set_defaults(fn=cmd_drive)

    args = ap.parse_args(argv)
    try:
        args.fn(args)
    except FileNotFoundError as e:
        sys.exit(str(e))


if __name__ == "__main__":
    main()
