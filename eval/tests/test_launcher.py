"""The launcher (`make start`, eval/launcher.py): the menu's scenarios from the situation files, the
command and environment each choice runs (the command line's own), what a start request may say,
the origin check, the reasons it reports, and the bridge relay. No stack starts.

    .venv/bin/python -m pytest eval/tests/test_launcher.py
"""

from __future__ import annotations

import json
import socket
import threading
from email.message import Message
from pathlib import Path

import pytest

from eval import REPO
from eval import launcher as L
from eval import situation as S


@pytest.fixture(scope="module")
def menu() -> tuple[list[dict], dict]:
    loaded: dict = {}
    return L.catalog(loaded=loaded), loaded


def test_the_open_world_comes_first_then_the_situations(menu):
    entries, loaded = menu
    first = entries[0]
    assert first["id"] == L.OPEN_WORLD and first["default"] and first["runnable"] and first["kind"] == "open_world"
    # the full-spectrum fleet: one of each platform type, most of them in SITL
    assert first["vehicles"] == first["platform_types"] == 22 and first["sitl"] > 0 and first["typical_s"] == [30, 60]
    assert first["contacts"] > 0 and first["duration_s"] is None and first["cli"] == "make goldengate"
    files = {p.stem for p in S.SITUATIONS.glob("*.toml")}
    assert {e["id"] for e in entries[1:]} == files
    runnable = [e["id"] for e in entries[1:] if e["runnable"]]
    assert runnable == ["fog-watch", "harbor-watch", "hostile-act-engagement", "hostile-act-loitering-munition", "morning-harbor-watch", "tutorial"]
    assert [e["id"] for e in entries if e.get("tutorial")] == ["tutorial"], "one tutorial, which the menu offers apart"
    assert sorted(loaded) == runnable
    # every runnable one before the rest, each of the rest saying why not
    kinds = [e["runnable"] for e in entries]
    assert kinds == sorted(kinds, reverse=True)
    why = {e["id"]: e["why_not"] for e in entries if not e["runnable"]}
    assert why["approval-flood"].startswith("a draft") and why["fault-drill"].startswith("headless only")
    assert why["golden-gate-v3-watch"].startswith("headless only")
    hae = next(e for e in entries if e["id"] == "hostile-act-engagement")
    assert (hae["operator"], hae["ai"], hae["vehicles"], hae["sitl"], hae["duration_s"]) == ("autoplay", "claude", 2, 0, 720.0)
    assert hae["operators"] == ["live", "autoplay"] and hae["ais"] == list(S.AI_MODES) and hae["typical_s"] == [5, 20]
    morning = next(e for e in entries if e["id"] == "morning-harbor-watch")
    assert (morning["operator"], morning["ai"], morning["vehicles"], morning["contacts"], morning["duration_s"]) == ("live", "playbook", 8, 109, 3600)
    # each fleet by domain, every vehicle counted once: the full spectrum has all four
    for e in entries:
        if e["runnable"]:
            assert list(e["domains"]) == list(L.DOMAINS) and sum(e["domains"].values()) == e["vehicles"]
    assert all(first["domains"].values()) and hae["domains"] == {"air": 1, "ground": 0, "surface": 1, "subsurface": 0}


def test_the_menu_never_carries_a_directors_notes(menu):
    entries, loaded = menu
    text = json.dumps(entries)
    for sit in loaded.values():
        assert sit.director and sit.director[:60] not in text
    assert not any("director" in e for e in entries)


def test_the_open_world_is_what_make_goldengate_runs():
    choice = L.Choice(L.OPEN_WORLD, "open_world")
    assert L.command(choice) == ["make", "--no-print-directory", "goldengate"]
    assert L.cli_text(choice) == "make goldengate"
    # what the menu says of it is the Makefile's default
    recipe = (REPO / "Makefile").read_text().split("\ngoldengate:", 1)[1].split("\n\n", 1)[0]
    assert f"$(or $(GG_FLEET),{L.OPEN_WORLD_FLEET})" in recipe and L.OPEN_WORLD_CONTACTS in recipe
    assert f"{L.OPEN_WORLD_TIDEWATER}.toml" in recipe and f"$(or $(AT),{L.OPEN_WORLD_AT})" in recipe
    # make's variables from the shell never change it; up.sh starts no console of its own
    env = L.launch_env({"PATH": "/bin", "GG_FLEET": "sandiego", "AT": "x", "CONTACTS": "", "MAKELEVEL": "1", "HOME": "/h"}, 5173)
    assert env == {"PATH": "/bin", "HOME": "/h", "MURMUR_WEB": "0", "MURMUR_WEB_PORT": "5173", "PYTHONUNBUFFERED": "1"}
    assert not S.up_starts_console(env) and S.up_starts_console({}) and S.up_starts_console({"MURMUR_WEB": "1"})
    ports = L.stack_ports_for(choice, env)
    assert ports == {"vehicle_base": 14560, "bridge_base": 15560, "http": 8765, "web": 5173, "linkemu": 8766, "tidewater": 8770, "sitl_instance_base": 0}
    # each MURMUR_* port variable overrides one, as up.sh reads them
    moved = L.stack_ports_for(choice, {**env, "MURMUR_HTTP_PORT": "8865", "MURMUR_BASE_PORT": "24560"})
    assert (moved["http"], moved["vehicle_base"], moved["bridge_base"]) == (8865, 24560, 25560)


def test_a_situation_runs_as_make_situation_does():
    choice = L.Choice("hostile-act-engagement", "situation", operator="autoplay", ai="none")
    argv = L.command(choice, python="/py")
    assert argv == ["/py", "-m", "eval.situation_run", "hostile-act-engagement", "--operator", "autoplay", "--ai", "none"]
    assert "-m eval.situation_run" in (REPO / "Makefile").read_text()
    assert L.cli_text(choice) == "make situation NAME=hostile-act-engagement OPERATOR=autoplay AI=none"
    # on the situation's own block (its own restore pointers), with the launcher's console
    env = S.situation_env(L.launch_env({"PATH": "/bin"}, 5173))
    ports = L.stack_ports_for(choice, env)
    assert (ports["http"], ports["web"], ports["vehicle_base"], ports["sitl_instance_base"]) == (9265, 5173, 33000, 160)
    with pytest.raises(L.BadRequest):
        L.command(L.Choice("--help", "situation", operator="live", ai="none"))
    with pytest.raises(L.BadRequest):
        L.command(L.Choice("harbor-watch", "situation", operator="live", ai="gpt"))


def test_a_start_request_names_only_the_menus_scenarios_and_options(menu):
    entries, loaded = menu
    v = lambda body: L.validate_start(body, entries, loaded)  # noqa: E731
    assert v({"scenario": "open-world"}) == L.Choice("open-world", "open_world")
    assert v({"scenario": "open-world", "replace": True}).replace
    # a situation's own defaults unless the request picks another
    assert v({"scenario": "hostile-act-engagement"}) == L.Choice("hostile-act-engagement", "situation", "autoplay", "claude")
    assert v({"scenario": "harbor-watch", "operator": "autoplay", "ai": "playbook"}) == L.Choice("harbor-watch", "situation", "autoplay", "playbook")
    bad = [
        [],
        "open-world",
        {"scenario": "nowhere"},
        {"scenario": "../../etc/passwd"},
        {"scenario": "harbor-watch; rm -rf ~"},
        {"scenario": "fault-drill"},  # headless only
        {"scenario": "approval-flood"},  # a draft
        {"scenario": "harbor-watch", "ai": "gpt-5"},
        {"scenario": "harbor-watch", "operator": "op-1"},
        {"scenario": "harbor-watch", "cosigner": "you"},  # not an option of the menu
        {"scenario": "harbor-watch", "args": ["--reveal", "1"]},
        {"scenario": "open-world", "ai": "claude"},
        {"scenario": "open-world", "replace": "yes"},
    ]
    for body in bad:
        with pytest.raises(L.BadRequest):
            v(body)


def test_the_tutorial_starts_with_its_own_modes_only(menu):
    """The menu's Tutorial starts it in one click with the file's modes (you live, no AI, op-2 and
    op-3 signing your nomination), as `make situation NAME=tutorial` does; nothing else is offered
    or taken, so the console's walk-through always meets the run it was written for."""
    entries, loaded = menu
    tut = next(e for e in entries if e["id"] == "tutorial")
    assert tut["runnable"] and tut["tutorial"] and (tut["operator"], tut["ai"]) == ("live", "none")
    assert tut["operators"] == ["live"] and tut["ais"] == ["none"]
    assert (tut["vehicles"], tut["sitl"], tut["duration_s"]) == (2, 0, 1200.0)
    v = lambda body: L.validate_start(body, entries, loaded)  # noqa: E731
    choice = v({"scenario": "tutorial"})
    assert choice == L.Choice("tutorial", "situation", "live", "none")
    assert v({"scenario": "tutorial", "operator": "live", "ai": "none"}) == choice
    assert L.command(choice, python="/py") == ["/py", "-m", "eval.situation_run", "tutorial", "--operator", "live", "--ai", "none"]
    for body in ({"scenario": "tutorial", "operator": "autoplay"}, {"scenario": "tutorial", "ai": "claude"}, {"scenario": "tutorial", "ai": "playbook"}):
        with pytest.raises(L.BadRequest):
            v(body)
    assert not any(e.get("tutorial") for e in entries if e["id"] != "tutorial")


def test_a_post_from_another_origin_is_refused():
    def headers(**h: str) -> Message:
        m = Message()
        for k, val in h.items():
            m[k.replace("_", "-")] = val
        return m

    ok = "application/json"
    assert L.refusal("POST", headers(Origin="http://localhost:5173", Content_Type=ok), 5173) is None
    assert L.refusal("POST", headers(Origin="http://127.0.0.1:5173", Content_Type=ok), 5173) is None
    for h in (
        headers(Origin="http://evil.example", Content_Type=ok),
        headers(Origin="http://localhost:5174", Content_Type=ok),
        headers(Origin="null", Content_Type=ok),
        headers(Content_Type=ok),  # no Origin: not the console's page
        headers(Origin="http://localhost:5173", Content_Type="text/plain"),  # a form's simple request
    ):
        assert L.refusal("POST", h, 5173), dict(h)
    assert L.refusal("GET", headers(), 5173) is None


def test_the_reason_a_launch_failed_is_its_commands_own():
    runner = [
        "# make situation NAME=harbor-watch OPERATOR=live AI=claude",
        "situation: warning: tidewater scenario 'x' not found",
        "situation: AI=claude: Claude Code is not logged in: run `claude`, then `/login` in it",
    ]
    assert L.failure_reason(runner).startswith("AI=claude: Claude Code is not logged in")
    up = ["auth: DEV run", "tidewater serve: starting on :8770.......", "tidewater serve exited; see logs/tidewater-serve.log",
          "make: *** [Makefile:16: goldengate] Error 1"]
    assert L.failure_reason(up) == "tidewater serve exited; see logs/tidewater-serve.log"
    assert L.failure_reason([]) and L.progress_line(up) == "tidewater serve exited; see logs/tidewater-serve.log"
    # the launch log's own header is never the reason nor the progress
    header = ["# make goldengate", "# make --no-print-directory goldengate"]
    assert L.progress_line(header) == "" and L.failure_reason(header) == "it stopped without saying why"
    assert len(L.progress_line(["x" * 400])) == 160


def test_tail_lines_drops_colour_and_blank_lines(tmp_path: Path):
    p = tmp_path / "out.log"
    p.write_text("one\n\n\x1b[32mtwo\x1b[0m\r\nthree...\r... up\n")
    assert L.tail_lines(p) == ["one", "two", "three...", "... up"]
    assert L.tail_lines(tmp_path / "missing.log") == []


def test_the_console_port_is_not_checked_when_the_launcher_serves_it(tmp_path: Path):
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    try:
        free = [socket.create_server(("127.0.0.1", 0)) for _ in range(5)]
        ports = {"vehicle_base": free[0].getsockname()[1], "bridge_base": free[1].getsockname()[1], "http": free[2].getsockname()[1],
                 "web": listener.getsockname()[1], "linkemu": free[3].getsockname()[1], "tidewater": free[4].getsockname()[1],
                 "sitl_instance_base": 0}
        for s in free:
            s.close()
        assert any("MURMUR_WEB_PORT" in p for p in S.port_problems(ports, 1, 0, root=tmp_path))
        assert not any("MURMUR_WEB_PORT" in p for p in S.port_problems(ports, 1, 0, root=tmp_path, skip=("web",)))
    finally:
        listener.close()


def test_the_relay_passes_bytes_both_ways_to_the_bridge_it_names():
    upstream = socket.create_server(("127.0.0.1", 0))

    def echo() -> None:
        conn, _ = upstream.accept()
        with conn:
            while data := conn.recv(1024):
                conn.sendall(data.upper())

    threading.Thread(target=echo, daemon=True).start()
    probe = socket.create_server(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    relay = L.Relay(port)
    threading.Thread(target=relay.serve, daemon=True).start()
    try:
        # no bridge named: closed at once
        with socket.create_connection(("127.0.0.1", port), timeout=3) as c:
            assert c.recv(16) == b""
        relay.target = upstream.getsockname()[1]
        with socket.create_connection(("127.0.0.1", port), timeout=3) as c:
            c.sendall(b"get /api/state")
            got = b""
            while len(got) < 14:
                got += c.recv(64)
            assert got == b"GET /API/STATE"
    finally:
        relay.close()
        upstream.close()
