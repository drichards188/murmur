"""A small client for the bridge's HTTP API (urllib only, no dependencies).

Operators act two ways (rules 8). On a bridge that authenticates (a credentials run, study
mode) each scripted operator logs in with its own password ([`Bridge.login`]) and its requests
carry its session cookie. On a dev bridge (`MURMUR_AUTH=dev`, the proxy batches) a request
names its operator in `X-Murmur-Operator`, which the bridge believes and records as dev.
"""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.request
from typing import Any

OPERATOR_HEADER = "X-Murmur-Operator"


class BridgeError(RuntimeError):
    def __init__(self, status: int, body: Any):
        self.status = status
        self.body = body
        detail = body.get("detail") if isinstance(body, dict) else body
        super().__init__(f"bridge {status}: {detail}")


class Bridge:
    def __init__(self, url: str, operator: str | None = None, timeout: float = 8.0):
        self.url = url.rstrip("/")
        self.operator = operator
        self.timeout = timeout
        # operator -> "name=token": the session cookie of each operator logged in here
        self.cookies: dict[str, str] = {}

    def login(self, operator: str, password: str) -> dict:
        """Log `operator` in (a bridge that authenticates); its later requests carry the
        session cookie. The password is not kept."""
        req = urllib.request.Request(
            self.url + "/api/auth/login",
            method="POST",
            data=json.dumps({"operator": operator, "password": password}).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                cookie = (r.headers.get("Set-Cookie") or "").split(";", 1)[0]
                body = json.loads(r.read() or b"{}")
        except urllib.error.HTTPError as e:
            raise BridgeError(e.code, {"detail": f"login of {operator} refused ({e.code})"}) from None
        if not cookie:
            raise BridgeError(500, {"detail": f"login of {operator}: no session cookie"})
        self.cookies[operator] = cookie
        return body

    def _call(self, method: str, path: str, body: Any = None, operator: str | None = None) -> tuple[int, Any]:
        headers = {"Content-Type": "application/json"}
        op = operator or self.operator
        if op and op in self.cookies:
            headers["Cookie"] = self.cookies[op]
        elif op:
            headers[OPERATOR_HEADER] = op
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(self.url + path, method=method, data=data, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                raw = r.read()
                return r.status, (json.loads(raw) if raw else {})
        except urllib.error.HTTPError as e:
            raw = e.read()
            try:
                return e.code, json.loads(raw) if raw else {}
            except ValueError:
                return e.code, {"detail": raw.decode(errors="replace")}

    def get(self, path: str) -> Any:
        status, body = self._call("GET", path)
        if status >= 400:
            raise BridgeError(status, body)
        return body

    def post(self, path: str, body: Any = None, operator: str | None = None, ok: tuple[int, ...] = (200, 201, 202)) -> Any:
        status, out = self._call("POST", path, body if body is not None else {}, operator)
        if status not in ok:
            raise BridgeError(status, out)
        return out

    def try_get(self, path: str) -> Any | None:
        try:
            return self.get(path)
        except (BridgeError, urllib.error.URLError, OSError, ValueError):
            return None

    def alive(self) -> bool:
        return self.try_get("/api/stats") is not None

    def wait(self, timeout: float, path: str = "/api/stats") -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.try_get(path) is not None:
                return True
            time.sleep(0.5)
        return False

    # ---- reads ---------------------------------------------------------------------------

    def state(self) -> dict:
        return self.get("/api/state")

    def vehicles(self) -> list[dict]:
        return list(self.state().get("vehicles", []))

    def inbox(self) -> dict:
        return self.get("/api/inbox")

    def intents(self) -> list[dict]:
        return list(self.get("/api/intents").get("intents", []))

    def intent(self, id: int) -> dict:
        return self.get(f"/api/intents/{id}")

    def tracks(self) -> list[dict]:
        return list(self.get("/api/tracks").get("tracks", []))

    def track(self, id: str) -> dict | None:
        status, body = self._call("GET", f"/api/tracks/{id}")
        return body if status == 200 else None

    def truth(self) -> dict | None:
        """Evaluation only (MURMUR_EVAL_TRUTH=1): truth, tracks and the association."""
        return self.try_get("/api/sim/truth")

    def links(self) -> dict | None:
        return self.try_get("/api/links")

    # ---- writes --------------------------------------------------------------------------

    def submit(self, body: dict, operator: str) -> dict:
        return self.post("/api/intents", body, operator=operator)

    def approve(self, id: int, note: str | None, operator: str) -> dict:
        return self.post(f"/api/intents/{id}/approve", {"note": note} if note else {}, operator=operator)

    def reject(self, id: int, note: str | None, operator: str) -> dict:
        return self.post(f"/api/intents/{id}/reject", {"note": note} if note else {}, operator=operator)

    def declare(self, track_id: str, affiliation: str, note: str | None, operator: str) -> dict:
        body: dict[str, Any] = {"affiliation": affiliation}
        if note:
            body["note"] = note
        return self.post(f"/api/tracks/{track_id}/declare", body, operator=operator)

    def view(
        self,
        intent_id: int,
        event: str,
        shown: dict,
        operator: str,
        action: str | None = None,
        open_ms: int | None = None,
    ) -> None:
        body: dict[str, Any] = {
            "intent_id": intent_id,
            "event": event,
            "shown": shown,
            "client_t": int(time.time() * 1000),
        }
        if action:
            body["action"] = action
        if open_ms is not None:
            body["open_ms"] = int(open_ms)
        self.post("/api/operator/view", body, operator=operator)
