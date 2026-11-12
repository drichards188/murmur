"""The launcher's console server (eval/webconsole.py): a build is current only while its sources are.

    .venv/bin/python -m pytest eval/tests/test_webconsole.py
"""

from eval import webconsole as W


def test_the_built_console_is_stale_when_its_sources_change(tmp_path):
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "a.ts").write_text("1")
    (tmp_path / "dist").mkdir()
    (tmp_path / "dist" / "index.html").write_text("x")
    assert not W.built_is_current(tmp_path)  # no stamp
    (tmp_path / "dist" / ".murmur-stamp").write_text(W.source_stamp(tmp_path) + "\n")
    assert W.built_is_current(tmp_path)
    (tmp_path / "src" / "a.ts").write_text("12")
    assert not W.built_is_current(tmp_path)


def test_the_mode_defaults_to_built_and_remembers_the_choice(tmp_path, monkeypatch):
    monkeypatch.delenv("MURMUR_WEB_MODE", raising=False)
    pref = tmp_path / "console-mode.json"
    assert W.default_mode(pref) == "build"
    pref.write_text('{"mode": "dev"}')
    assert W.default_mode(pref) == "dev"
    pref.write_text("garbage")
    monkeypatch.setenv("MURMUR_WEB_MODE", "dev")
    assert W.default_mode(pref) == "dev"
