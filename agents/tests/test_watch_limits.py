"""Usage-limit messages and their reset times (agents/watch/limits.py)."""

from __future__ import annotations

import datetime as dt
from zoneinfo import ZoneInfo

from watch.limits import from_rate_limit_event, parse_claude, parse_codex

LA = ZoneInfo("America/Los_Angeles")
NOW = dt.datetime(2026, 10, 8, 14, 10, tzinfo=LA)  # a Thursday


def at(y, mo, d, h, mi, tz=LA) -> float:
    return dt.datetime(y, mo, d, h, mi, tzinfo=tz).timestamp()


def test_claude_session_limit_resets_later_today():
    hit = parse_claude("You've hit your session limit · resets 3:45pm", NOW)
    assert hit and hit.kind == "session" and hit.provider == "claude"
    assert hit.reset_at == at(2026, 10, 8, 15, 45)


def test_claude_reset_before_now_is_tomorrow():
    hit = parse_claude("You've hit your limit · resets 9am", NOW)
    assert hit.reset_at == at(2026, 10, 9, 9, 0)
    assert hit.kind == "usage"


def test_claude_reset_with_a_time_zone():
    hit = parse_claude("You've hit your Opus limit · resets 5pm (Europe/London)", NOW)
    assert hit.kind == "opus"
    london = NOW.astimezone(ZoneInfo("Europe/London"))  # 22:10 there: 5pm is tomorrow
    assert hit.reset_at == dt.datetime(london.year, london.month, london.day + 1, 17, 0,
                                       tzinfo=ZoneInfo("Europe/London")).timestamp()


def test_claude_weekly_limit_with_a_date():
    hit = parse_claude("You've hit your weekly limit ∙ resets Oct 13, 3pm", NOW)
    assert hit.kind == "weekly"
    assert hit.reset_at == at(2026, 10, 13, 15, 0)
    hit = parse_claude("You’ve hit your Sonnet limit · resets Oct 13 at 3:30pm (America/Los_Angeles)", NOW)
    assert hit.kind == "sonnet" and hit.reset_at == at(2026, 10, 13, 15, 30)


def test_claude_older_forms():
    assert parse_claude("Claude AI usage limit reached|1791500000", NOW).reset_at == 1791500000.0
    hit = parse_claude("5-hour limit reached ∙ resets 4pm", NOW)
    assert hit.kind == "five_hour" and hit.reset_at == at(2026, 10, 8, 16, 0)


def test_claude_limit_without_a_time():
    hit = parse_claude("You've hit your session limit.", NOW)
    assert hit is not None and hit.reset_at is None


def test_not_a_limit():
    assert parse_claude("The speed limit on the bridge is 45 mph.", NOW) is None
    assert parse_claude("", NOW) is None
    assert parse_codex("You hit the limit of the approved area.", NOW) is None


def test_only_the_clis_own_phrasings_count():
    """Finding 12: a generic "limit reached" in a model's prose (an error turn narrating its
    own battery or turn limit) slept the watch for 15 minutes."""
    for prose in ("The battery reserve limit reached 20% on SKY 1, so I did not propose an orbit.",
                  "Tool call limit reached for this update; no action.",
                  "Note: the endurance limit reached its floor.",
                  "I think we've hit your inbox limit, so no new proposal."):
        assert parse_claude(prose, NOW) is None, prose
    for cli in ("You've hit your session limit · resets 3:45pm",
                "You’ve hit your weekly limit · resets Oct 13, 3pm",
                "5-hour limit reached ∙ resets 4pm",
                "Claude AI usage limit reached|1791500000",
                "API Error: 429\nYou've hit your Opus limit · resets 7pm"):
        assert parse_claude(cli, NOW) is not None, cli


def test_rate_limit_event():
    assert from_rate_limit_event({"status": "allowed", "resetsAt": 1}) is None
    hit = from_rate_limit_event({"status": "rejected", "resetsAt": 1791502200, "rateLimitType": "five_hour"})
    assert hit.reset_at == 1791502200.0 and hit.kind == "five_hour"


def test_codex_try_again_at():
    hit = parse_codex("You’ve hit your usage limit. Try again at 3:42 PM.", NOW)
    assert hit and hit.provider == "codex" and hit.reset_at == at(2026, 10, 8, 15, 42)
    # an ASCII apostrophe and a longer message
    hit = parse_codex("You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro) or try again "
                      "at 9:05 AM.", NOW)
    assert hit.reset_at == at(2026, 10, 9, 9, 5)


def test_codex_dated_and_later():
    hit = parse_codex("You’ve hit your usage limit. Try again at Oct 9th, 2026 3:42 PM.", NOW)
    assert hit.reset_at == at(2026, 10, 9, 15, 42)
    hit = parse_codex("You’ve hit your usage limit. Try again later.", NOW)
    assert hit is not None and hit.reset_at is None


def test_as_dict_is_loggable():
    d = parse_codex("You’ve hit your usage limit. Try again at 3:42 PM.", NOW).as_dict()
    assert d["reset_iso"].endswith("+00:00") and d["provider"] == "codex"
