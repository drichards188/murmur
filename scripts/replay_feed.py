"""A stand-in bridge for console performance work: serves a recorded feed (logs/rec/) the way
the bridge does, on one port (HTTP for /api, WebSocket for /ws).

The recording is a live Saturation station's `/ws?lod=1` first messages (state, decisions,
tracks, a taskings delta) and its REST answers. The station was idle when recorded, so the
motion is synthetic: after the client sends a view, vehicles inside it go to full detail with
a trail and move 4 times a second, a quarter of the lite vehicles are updated each tick (each
once a second), tracks move once a second, decisions are re-sent every 3 s and taskings every
5 s. Seeded, so every run sends the same sequence.

    python scripts/replay_feed.py [--port 9500] [--rec logs/rec] [--full-max 400]
"""
import argparse, asyncio, copy, json, math, random
from http import HTTPStatus
from pathlib import Path

import websockets
from websockets.http11 import Response
from websockets.datastructures import Headers


def load(rec: Path):
    rows = [json.loads(l) for l in open(rec / "ws.jsonl")]
    msgs = {}
    for r in rows:
        if "m" in r:
            m = json.loads(r["m"])
            msgs.setdefault(m["type"], m)
    deltas = [m for m in (json.loads(r["m"]) for r in rows if "m" in r) if m["type"] == "delta"]
    rest = {}
    for f in rec.glob("rest_*.json"):
        rest["/api/" + f.stem[5:].replace("_", "/")] = f.read_bytes()
    return msgs, deltas, rest


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=9500)
    ap.add_argument("--rec", default="logs/rec")
    ap.add_argument("--full-max", type=int, default=400)
    a = ap.parse_args()
    msgs, deltas, rest = load(Path(a.rec))
    taskings = next(d["taskings"] for d in deltas if d.get("taskings"))

    def http(conn, req):
        if req.path.startswith("/ws"):
            return None
        path = req.path.split("?")[0]
        body = rest.get(path)
        status = HTTPStatus.OK if body is not None else HTTPStatus.NOT_FOUND
        body = body if body is not None else b"{}"
        return Response(status.value, status.phrase, Headers([("Content-Type", "application/json"), ("Content-Length", str(len(body)))]), body)

    async def handler(ws):
        rng = random.Random(1)
        state = copy.deepcopy(msgs["state"])
        t = state["t"]
        vehicles = {v["uid"]: v for v in state["vehicles"]}
        uids = sorted(vehicles)
        await ws.send(json.dumps(state))
        await ws.send(json.dumps(msgs["decisions"]))
        await ws.send(json.dumps(msgs["tracks"]))
        full: list[int] = []
        view = asyncio.Queue()

        async def reader():
            async for raw in ws:
                m = json.loads(raw)
                if m.get("type") == "view":
                    await view.put(m)

        asyncio.create_task(reader())
        tick = 0
        while True:
            await asyncio.sleep(0.25)
            tick += 1
            t += 0.25
            out = []
            while not view.empty():
                m = await view.get()
                b = m.get("bounds")
                if b:
                    inside = [u for u in uids if vehicles[u]["position"] and b[0] <= vehicles[u]["position"][0] <= b[2] and b[1] <= vehicles[u]["position"][1] <= b[3]]
                    full = inside[: a.full_max]
                    for u in full:
                        v = vehicles[u]
                        x, y = v["position"]
                        v["lod"] = "full"
                        v["heading"] = rng.uniform(0, 360)
                        v["alt_rel"] = v.get("alt_msl") or 0.0
                        v["roll"] = v["pitch"] = 0.0
                        v["trail"] = [[x - 0.0001 * k, y - 0.00005 * k, v.get("alt_msl") or 0.0] for k in range(30, 0, -1)]
                        v["link"] = {**v["link"], "rtt_ms": 120, "last_rx_t": t}
                        out.append(dict(v))
            # full vehicles: 4 Hz, a step along their heading, a trail point
            for u in full:
                v = vehicles[u]
                x, y = v["position"]
                h = math.radians(v.get("heading") or 0)
                nx, ny = x + 0.00002 * math.sin(h), y + 0.00002 * math.cos(h)
                v["position"] = [nx, ny]
                v["link"] = {**v["link"], "last_rx_t": t}
                out.append({"uid": u, "position": [nx, ny], "alt_msl": v.get("alt_msl"), "heading": v["heading"], "groundspeed": 5.0, "link": v["link"], "trail_add": [[nx, ny, v.get("alt_msl") or 0.0]]})
            fs = set(full)
            for u in uids[tick % 4 :: 4]:
                if u in fs:
                    continue
                v = vehicles[u]
                if v["position"]:
                    v["position"] = [v["position"][0] + rng.uniform(-1e-5, 1e-5), v["position"][1] + rng.uniform(-1e-5, 1e-5)]
                out.append({"uid": u, "position": v["position"], "mode": v.get("mode"), "groundspeed": v.get("groundspeed"), "lod": "lite", "link": v["link"]})
            d = {"type": "delta", "t": t, "vehicles": out}
            if tick % 20 == 0:
                d["taskings"] = taskings
            await ws.send(json.dumps(d))
            if tick % 4 == 0:
                tr = copy.deepcopy(msgs["tracks"])
                tr["t"] = t
                for k in tr["tracks"]:
                    if k.get("position"):
                        k["position"] = [k["position"][0] + rng.uniform(-2e-4, 2e-4), k["position"][1] + rng.uniform(-2e-4, 2e-4)]
                await ws.send(json.dumps(tr))
            if tick % 12 == 0:
                await ws.send(json.dumps(msgs["decisions"]))

    async with websockets.serve(handler, "127.0.0.1", a.port, max_size=None, process_request=http, ping_interval=None, compression=None):
        print(f"replay on 127.0.0.1:{a.port}", flush=True)
        await asyncio.Future()


asyncio.run(main())
