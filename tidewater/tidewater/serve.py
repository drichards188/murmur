"""Batch conditions over HTTP, for clients that cannot import tidewater.

POST /v1/sample  {"t": unix_s, "points": [[lat, lon, depth_m|null], ...]}
                 -> {"t": ..., "samples": [{lat, lon, elevation_m, water_level_m, ...}, ...]}
GET  /v1/info    scenario name, bounds and valid time range
Errors are {"error": "..."} with status 400 (bad input or time outside the scenario).
"""

import json
import math
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np

from .conditions import Conditions
from .scenario import Scenario

MAX_BODY_BYTES = 16 * 1024 * 1024


class BadRequest(Exception):
    pass


def _number(v, what):
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v):
        raise BadRequest(f"{what} must be a finite number")
    return float(v)


class Service:
    """Request logic, separate from HTTP so it can be tested directly."""

    def __init__(self, scenario: Scenario, conditions: Conditions):
        self.scenario = scenario
        self.cond = conditions
        self.t_min = scenario.start.timestamp()
        self.t_max = scenario.end.timestamp()

    def info(self):
        a = self.scenario.area
        return {
            "name": self.scenario.name,
            "title": self.scenario.title,
            "weather": {"enabled": bool(self.cond.weather),
                        "archive": self.scenario.weather.get("archive"),
                        "model": self.cond.weather.data["model"] if self.cond.weather else None},
            "bounds": {"west": a.west, "south": a.south, "east": a.east, "north": a.north},
            "time_range": {
                "start": self.scenario.start.isoformat(),
                "end": self.scenario.end.isoformat(),
                "start_unix": self.t_min,
                "end_unix": self.t_max,
            },
        }

    def sample(self, body):
        if not isinstance(body, dict):
            raise BadRequest("body must be a JSON object with 't' and 'points'")
        if "t" not in body:
            raise BadRequest("missing 't' (unix seconds)")
        if "points" not in body:
            raise BadRequest("missing 'points' ([[lat, lon, depth_m_or_null], ...])")
        t = _number(body["t"], "'t'")
        if not self.t_min <= t <= self.t_max:
            s, e = self.scenario.start.isoformat(), self.scenario.end.isoformat()
            raise BadRequest(f"t={t} is outside the scenario's time range: "
                             f"{self.t_min:.0f} to {self.t_max:.0f} unix seconds ({s} to {e})")
        points = body["points"]
        if not isinstance(points, list):
            raise BadRequest("'points' must be a list")
        n = len(points)
        lat, lon, depth = np.empty(n), np.empty(n), np.full(n, np.nan)
        for i, p in enumerate(points):
            if not isinstance(p, list) or len(p) != 3:
                raise BadRequest(f"points[{i}] must be [lat, lon, depth_m_or_null]")
            lat[i] = _number(p[0], f"points[{i}] lat")
            lon[i] = _number(p[1], f"points[{i}] lon")
            if p[2] is not None:
                depth[i] = _number(p[2], f"points[{i}] depth_m")
            if not (-90 <= lat[i] <= 90 and -180 <= lon[i] <= 180):
                raise BadRequest(f"points[{i}] lat/lon out of range")
        batch = self.cond.sample_many(lat, lon, t, depth)
        samples = batch.to_dicts()
        return {"t": body["t"], "samples": samples}


def make_handler(service: Service):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"  # keep-alive: clients poll about once a second

        def _send(self, status, obj):
            data = json.dumps(obj, separators=(",", ":")).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path.split("?")[0] == "/v1/info":
                self._send(200, service.info())
            else:
                self._send(404, {"error": f"not found: {self.path}"})

        def do_POST(self):
            if self.path.split("?")[0] != "/v1/sample":
                self._send(404, {"error": f"not found: {self.path}"})
                return
            try:
                length = int(self.headers.get("Content-Length", ""))
            except ValueError:
                self._send(400, {"error": "Content-Length required"})
                self.close_connection = True
                return
            if length < 0 or length > MAX_BODY_BYTES:
                self._send(400, {"error": f"body must be at most {MAX_BODY_BYTES} bytes"})
                self.close_connection = True
                return
            raw = self.rfile.read(length)
            try:
                try:
                    body = json.loads(raw)
                except ValueError as e:
                    raise BadRequest(f"invalid JSON: {e}")
                self._send(200, service.sample(body))
            except BadRequest as e:
                self._send(400, {"error": str(e)})

        def log_message(self, fmt, *args):
            pass

    return Handler


def make_server(scenario: Scenario, host="127.0.0.1", port=8770):
    service = Service(scenario, Conditions.for_scenario(scenario))
    server = ThreadingHTTPServer((host, port), make_handler(service))
    server.daemon_threads = True
    return server


def serve(args):
    sc = Scenario.load(args.scenario)
    server = make_server(sc, args.host, args.port)
    print(f"{sc.title}\nserving http://{args.host}:{server.server_address[1]}  (POST /v1/sample, GET /v1/info)", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
