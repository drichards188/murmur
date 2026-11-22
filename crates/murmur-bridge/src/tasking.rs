//! Operator tasking for the live AI watch officer: requests an operator types
//! ("prioritise the ferry lane", "split the picket in two groups, one patrolling each side of the
//! bridge") that the officer reads on each wake and carries out with the orders it may propose.
//!
//! A request gives the officer no authority: what it proposes still goes through the engine and
//! the policy and waits for the operator where it always waits, and nothing here reaches a gated
//! decision (posting a request approves nothing). Like groups it is never logged and this module
//! imports nothing from `murmur_decide`. In memory only.
//!
//! How far the officer has each item (`delivery`, display only, for the console's Tasking
//! drawer) moves only forward and only on the officer's own acknowledgements ([`TaskingStore::ack`]):
//! `posted` (stored), `seen` (an update listing it went to the model), `processed` (the turn on
//! that update finished). An acknowledgement changes nothing else: never the text, the author or
//! whether the item is active, and it cannot make or retire one. Beside the state, the officer may
//! say why a posted item waits (`waiting`: the hour's wakes spent, its model backing off) and that
//! the turn that carried a seen one failed (`failed`: the next update gives it again).
//!
//! The officer answers each item ([`TaskingStore::reply`], MCP `reply_tasking` only): a short text,
//! bottom line first, an outcome ([`Outcome`]: done, partial, blocked, declined), the intents it
//! proposed for the item and the groups it made for it. A reply is display only, like the rest:
//! it decides nothing, changes nothing of the item (its text, author, whether it is active, its
//! delivery) and is not logged. At most [`REPLIES_MAX`] an item, each at most [`REPLY_MAX`]
//! characters in [`REPLY_LINES_MAX`] lines.

use serde::{Deserialize, Serialize};

/// The longest guidance text, in characters.
pub const TEXT_MAX: usize = 500;
/// The longest reason an acknowledgement gives, in characters.
pub const WHY_MAX: usize = 200;
/// The longest reply, in characters (trimmed, blank lines dropped), and its most lines: a bottom
/// line and up to four bullets, as a rationale is shaped.
pub const REPLY_MAX: usize = 600;
pub const REPLY_LINES_MAX: usize = 5;
/// The most intents one reply names.
pub const REPLY_INTENTS_MAX: usize = 20;
/// The most replies one item takes: the officer replies again only when the outcome changes.
pub const REPLIES_MAX: usize = 6;

/// What the officer says came of an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// every order it needs proposed, or the groups it asked for made
    Done,
    /// some of it
    Partial,
    /// none of it could go: the reply says why and what would let it
    Blocked,
    /// not the officer's to do (a person's decision, or outside what it may propose)
    Declined,
}

impl Outcome {
    pub fn parse(s: &str) -> Option<Outcome> {
        match s.trim() {
            "done" => Some(Outcome::Done),
            "partial" => Some(Outcome::Partial),
            "blocked" => Some(Outcome::Blocked),
            "declined" => Some(Outcome::Declined),
            _ => None,
        }
    }
}

/// One answer of the officer's to an item.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reply {
    /// bridge ms
    pub t: u64,
    /// the agent that wrote it (resolved by the caller, never taken from the arguments)
    pub agent: String,
    pub status: Outcome,
    pub text: String,
    /// the intents it proposed for the item (each checked to be the agent's own by the caller)
    pub intents: Vec<u64>,
    /// the groups it had made for the item when it replied
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// no item of that id (404)
    Unknown,
    /// the reply itself will not do (400)
    Invalid(String),
    /// the item has its [`REPLIES_MAX`] (409)
    Full(String),
}

/// A reply's text as kept: each line trimmed, blank lines dropped; or why it will not do.
pub fn reply_text(text: &str) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return Err("a reply needs some text: its bottom line first".into());
    }
    if lines.len() > REPLY_LINES_MAX {
        return Err(format!(
            "a reply is at most {REPLY_LINES_MAX} lines ({}): the bottom line, then up to {} lines starting \"- \"",
            lines.len(),
            REPLY_LINES_MAX - 1
        ));
    }
    let kept = lines.join("\n");
    let n = kept.chars().count();
    if n > REPLY_MAX {
        return Err(format!("a reply is at most {REPLY_MAX} characters ({n})"));
    }
    Ok(kept)
}

/// How far the watch officer has an item: only ever forward.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    #[default]
    Posted,
    Seen,
    Processed,
}

/// Why a posted item waits, or how the turn that carried a seen one failed, as the officer said.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeliveryNote {
    pub t: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake: Option<u64>,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TaskingItem {
    pub id: String,
    pub text: String,
    pub by: String,
    pub at_t: u64,
    pub active: bool,
    pub delivery: Delivery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen_t: Option<u64>,
    /// the officer's update (wake) number that first listed it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen_wake: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processed_t: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processed_wake: Option<u64>,
    /// the agent whose acknowledgement moved it last
    #[serde(skip_serializing_if = "Option::is_none")]
    pub officer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<DeliveryNote>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<DeliveryNote>,
    /// the officer's answers, oldest first (none: not answered yet)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<Reply>,
}

/// What an officer's acknowledgement says of an item (`POST /api/tasking/{id}/ack`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckState {
    /// posted, read by the officer's driver, not yet given to the model: why (`why`)
    Waiting,
    /// an update listing it went to the model (`wake`)
    Seen,
    /// the turn on an update listing it finished (`wake`)
    Processed,
    /// the turn on an update listing it failed (`wake`, `why`): the next update gives it again
    Failed,
}

#[derive(Clone, Copy, Debug)]
pub struct Ack<'a> {
    pub state: AckState,
    pub wake: Option<u64>,
    pub why: Option<&'a str>,
    /// the agent acknowledging (resolved by the caller, never taken from the body)
    pub agent: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AckError {
    /// no item of that id (404)
    Unknown,
    /// the acknowledgement itself is wrong (400)
    Invalid(String),
    /// it would move the item back, or says what the item's state no longer allows (409)
    Behind(String),
}

#[derive(Default)]
pub struct TaskingStore {
    /// oldest first; `list` answers newest first
    items: Vec<TaskingItem>,
    next: u64,
    /// the agent that last read the list, and when (ms): whether an officer is connected
    read: Option<(String, u64)>,
    /// whether agents here may answer items (the bridge offers `reply_tasking`)
    replies_kept: bool,
}

impl TaskingStore {
    /// Newest first.
    pub fn list(&self) -> Vec<TaskingItem> {
        self.items.iter().rev().cloned().collect()
    }

    pub fn post(&mut self, text: &str, by: &str, now_ms: u64) -> Result<TaskingItem, String> {
        let t = text.trim();
        if t.is_empty() {
            return Err("tasking needs some text".into());
        }
        if t.chars().count() > TEXT_MAX {
            return Err(format!("tasking is at most {TEXT_MAX} characters"));
        }
        self.next += 1;
        let item = TaskingItem {
            id: format!("tk-{}", self.next),
            text: t.to_string(),
            by: by.to_string(),
            at_t: now_ms,
            active: true,
            delivery: Delivery::Posted,
            seen_t: None,
            seen_wake: None,
            processed_t: None,
            processed_wake: None,
            officer: None,
            waiting: None,
            failed: None,
            replies: Vec::new(),
        };
        self.items.push(item.clone());
        Ok(item)
    }

    /// One item, by id.
    pub fn get(&self, id: &str) -> Option<&TaskingItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// The officer's answer to an item (`text` as [`reply_text`] keeps it; `intents` de-duplicated
    /// in order, at most [`REPLY_INTENTS_MAX`]), on an active or a retired item alike (it answers
    /// what was asked). Only `replies` changes; at most [`REPLIES_MAX`] an item.
    pub fn reply(&mut self, id: &str, reply: Reply) -> Result<TaskingItem, ReplyError> {
        let text = reply_text(&reply.text).map_err(ReplyError::Invalid)?;
        let mut intents = Vec::new();
        for i in reply.intents {
            if !intents.contains(&i) {
                intents.push(i);
            }
        }
        if intents.len() > REPLY_INTENTS_MAX {
            return Err(ReplyError::Invalid(format!("a reply names at most {REPLY_INTENTS_MAX} intents")));
        }
        let item = self.items.iter_mut().find(|i| i.id == id).ok_or(ReplyError::Unknown)?;
        if item.replies.len() >= REPLIES_MAX {
            return Err(ReplyError::Full(format!(
                "{id} has its {REPLIES_MAX} replies: the operator can post it again if more is needed"
            )));
        }
        item.replies.push(Reply { text, intents, ..reply });
        Ok(item.clone())
    }

    /// Retire an item (idempotent); `None` for an unknown id. It keeps its delivery state.
    pub fn deactivate(&mut self, id: &str) -> Option<TaskingItem> {
        let item = self.items.iter_mut().find(|i| i.id == id)?;
        item.active = false;
        Some(item.clone())
    }

    /// Agents here may answer items (the bridge offers `reply_tasking`): `GET /api/tasking` says
    /// so (`officer_replies`), so the console tells an item the officer left unanswered.
    pub fn keep_replies(&mut self) {
        self.replies_kept = true;
    }

    /// Whether agents here may answer items ([`TaskingStore::keep_replies`]).
    pub fn replies_kept(&self) -> bool {
        self.replies_kept
    }

    /// An agent read the list (`GET /api/tasking` with its identity).
    pub fn read_by(&mut self, agent: &str, now_ms: u64) {
        self.read = Some((agent.to_string(), now_ms));
    }

    /// The agent that last read the list, and when.
    pub fn reader(&self) -> Option<(&str, u64)> {
        self.read.as_ref().map(|(a, t)| (a.as_str(), *t))
    }

    /// The officer's acknowledgement of an item: `seen` and `processed` move it forward only
    /// (`processed` straight from `posted` stands for `seen` too, as one whose `seen` was lost);
    /// `waiting` is for a posted item, `failed` for a seen one. Only the delivery fields change,
    /// on an active or a retired item alike.
    pub fn ack(&mut self, id: &str, ack: Ack, now_ms: u64) -> Result<TaskingItem, AckError> {
        let wake = match (ack.state, ack.wake) {
            (AckState::Waiting, _) => ack.wake,
            (_, Some(n)) if n >= 1 => Some(n),
            (s, _) => return Err(AckError::Invalid(format!("{s:?} needs the update's number (wake, from 1)").to_lowercase())),
        };
        let why = ack.why.map(str::trim).filter(|w| !w.is_empty());
        if why.is_some_and(|w| w.chars().count() > WHY_MAX) {
            return Err(AckError::Invalid(format!("why is at most {WHY_MAX} characters")));
        }
        if ack.state == AckState::Waiting && why.is_none() {
            return Err(AckError::Invalid("waiting needs why".into()));
        }
        let item = self.items.iter_mut().find(|i| i.id == id).ok_or(AckError::Unknown)?;
        let at = |want: &str, was: Delivery| AckError::Behind(format!("{id} is {}: {want}", name(was)));
        match ack.state {
            AckState::Waiting => {
                if item.delivery != Delivery::Posted {
                    return Err(at("only a posted item waits", item.delivery));
                }
                item.waiting = Some(DeliveryNote { t: now_ms, wake, why: why.unwrap_or_default().to_string() });
            }
            AckState::Seen => {
                if item.delivery >= Delivery::Seen {
                    return Err(at("seen once, never again or back", item.delivery));
                }
                item.delivery = Delivery::Seen;
                item.seen_t = Some(now_ms);
                item.seen_wake = wake;
                item.waiting = None;
            }
            AckState::Processed => {
                if item.delivery == Delivery::Processed {
                    return Err(at("processed once", item.delivery));
                }
                if item.seen_t.is_none() {
                    item.seen_t = Some(now_ms);
                    item.seen_wake = wake;
                }
                item.delivery = Delivery::Processed;
                item.processed_t = Some(now_ms);
                item.processed_wake = wake;
                item.waiting = None;
            }
            AckState::Failed => {
                if item.delivery != Delivery::Seen {
                    return Err(at("only a seen item's turn can fail", item.delivery));
                }
                item.failed = Some(DeliveryNote { t: now_ms, wake, why: why.unwrap_or("the turn failed").to_string() });
            }
        }
        item.officer = Some(ack.agent.to_string());
        Ok(item.clone())
    }
}

fn name(d: Delivery) -> &'static str {
    match d {
        Delivery::Posted => "posted",
        Delivery::Seen => "seen",
        Delivery::Processed => "processed",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn posts_list_newest_first_and_deactivate_keeps_the_item() {
        let mut s = TaskingStore::default();
        let a = s.post("  watch the ferry lane ", "op-1", 5).unwrap();
        assert_eq!((a.id.as_str(), a.text.as_str(), a.by.as_str(), a.at_t, a.active), ("tk-1", "watch the ferry lane", "op-1", 5, true));
        s.post("hold the north pickets", "op-1", 6).unwrap();
        let ids: Vec<_> = s.list().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, ["tk-2", "tk-1"]);
        let d = s.deactivate("tk-1").unwrap();
        assert!(!d.active);
        assert!(s.deactivate("tk-1").is_some(), "idempotent");
        assert!(s.deactivate("tk-9").is_none());
        let l = s.list();
        assert_eq!((l.len(), l[1].active, l[0].active), (2, false, true));
    }

    #[test]
    fn text_is_checked() {
        let mut s = TaskingStore::default();
        assert!(s.post("   ", "op-1", 0).is_err());
        assert!(s.post(&"x".repeat(501), "op-1", 0).is_err());
        assert!(s.post(&"x".repeat(500), "op-1", 0).is_ok());
    }

    fn ack(state: AckState, wake: Option<u64>, why: Option<&str>) -> Ack<'_> {
        Ack { state, wake, why, agent: "agent-watch" }
    }

    #[test]
    fn delivery_moves_forward_only_and_changes_nothing_else() {
        let mut s = TaskingStore::default();
        let posted = s.post("watch the ferry lane", "op-1", 5).unwrap();
        assert_eq!(posted.delivery, Delivery::Posted);
        let w = s.ack("tk-1", ack(AckState::Waiting, None, Some("the hour's wakes are spent")), 6).unwrap();
        assert_eq!(w.waiting.as_ref().map(|n| n.why.as_str()), Some("the hour's wakes are spent"));
        assert!(matches!(s.ack("tk-1", ack(AckState::Failed, Some(3), None), 7), Err(AckError::Behind(_))), "nothing to fail yet");
        let seen = s.ack("tk-1", ack(AckState::Seen, Some(3), None), 8).unwrap();
        assert_eq!((seen.delivery, seen.seen_t, seen.seen_wake, seen.waiting.is_none()), (Delivery::Seen, Some(8), Some(3), true));
        assert_eq!(seen.officer.as_deref(), Some("agent-watch"));
        assert!(matches!(s.ack("tk-1", ack(AckState::Seen, Some(4), None), 9), Err(AckError::Behind(_))), "seen once");
        assert!(matches!(s.ack("tk-1", ack(AckState::Waiting, None, Some("x")), 9), Err(AckError::Behind(_))), "never back");
        let f = s.ack("tk-1", ack(AckState::Failed, Some(3), Some("the turn ended error")), 10).unwrap();
        assert_eq!((f.delivery, f.failed.as_ref().and_then(|n| n.wake)), (Delivery::Seen, Some(3)));
        let p = s.ack("tk-1", ack(AckState::Processed, Some(4), None), 11).unwrap();
        assert_eq!((p.delivery, p.processed_t, p.processed_wake, p.seen_wake), (Delivery::Processed, Some(11), Some(4), Some(3)));
        for st in [AckState::Seen, AckState::Processed, AckState::Failed] {
            assert!(matches!(s.ack("tk-1", ack(st, Some(5), None), 12), Err(AckError::Behind(_))), "{st:?} after processed");
        }
        // only the delivery fields: text, author, time and active as posted
        let now = s.list().pop().unwrap();
        assert_eq!((now.text.as_str(), now.by.as_str(), now.at_t, now.active), ("watch the ferry lane", "op-1", 5, true));
        // a retired item keeps its state, and may still move on (a turn that carried it finishing)
        s.post("hold the north pickets", "op-1", 20).unwrap();
        s.ack("tk-2", ack(AckState::Seen, Some(6), None), 21).unwrap();
        assert_eq!(s.deactivate("tk-2").unwrap().delivery, Delivery::Seen);
        assert_eq!(s.ack("tk-2", ack(AckState::Processed, Some(6), None), 22).unwrap().delivery, Delivery::Processed);
        // processed straight from posted stands for seen too
        s.post("ignore the sailboats", "op-1", 30).unwrap();
        let p = s.ack("tk-3", ack(AckState::Processed, Some(7), None), 31).unwrap();
        assert_eq!((p.seen_wake, p.processed_wake), (Some(7), Some(7)));
    }

    #[test]
    fn a_bad_acknowledgement_is_refused() {
        let mut s = TaskingStore::default();
        s.post("watch the ferry lane", "op-1", 5).unwrap();
        assert_eq!(s.ack("tk-9", ack(AckState::Seen, Some(1), None), 6), Err(AckError::Unknown));
        for bad in [ack(AckState::Seen, None, None), ack(AckState::Processed, Some(0), None), ack(AckState::Waiting, None, Some("  "))] {
            assert!(matches!(s.ack("tk-1", bad, 6), Err(AckError::Invalid(_))), "{bad:?}");
        }
        let long = "x".repeat(WHY_MAX + 1);
        assert!(matches!(s.ack("tk-1", ack(AckState::Waiting, None, Some(&long)), 6), Err(AckError::Invalid(_))));
        assert_eq!(s.list()[0].delivery, Delivery::Posted, "nothing changed");
    }

    fn reply(status: Outcome, text: &str, intents: Vec<u64>) -> Reply {
        Reply { t: 50, agent: "agent-watch".into(), status, text: text.into(), intents, groups: vec!["og-1".into()] }
    }

    #[test]
    fn a_reply_is_kept_under_its_item_and_changes_nothing_else() {
        let mut s = TaskingStore::default();
        s.post("split the picket in two groups", "op-1", 5).unwrap();
        s.ack("tk-1", ack(AckState::Seen, Some(2), None), 6).unwrap();
        let text = "  Proposed two patrols; both wait for you in the Inbox \n\n - i41: SKY 1-30 west of the span\n- i42: SKY 31-60 east  ";
        let got = s.reply("tk-1", reply(Outcome::Done, text, vec![41, 42, 41])).unwrap();
        assert_eq!(got.replies.len(), 1);
        let r = &got.replies[0];
        assert_eq!(r.text, "Proposed two patrols; both wait for you in the Inbox\n- i41: SKY 1-30 west of the span\n- i42: SKY 31-60 east");
        assert_eq!((r.status, r.intents.clone(), r.agent.as_str(), r.t), (Outcome::Done, vec![41, 42], "agent-watch", 50));
        // only the replies: text, author, active and delivery as they were
        assert_eq!((got.text.as_str(), got.by.as_str(), got.active, got.delivery, got.seen_wake), ("split the picket in two groups", "op-1", true, Delivery::Seen, Some(2)));
        let v = serde_json::to_value(&got).unwrap();
        assert_eq!((v["replies"][0]["status"].clone(), v["replies"][0]["groups"].clone()), (serde_json::json!("done"), serde_json::json!(["og-1"])));
        // an item with none says nothing of replies (an older bridge's shape)
        s.post("watch the ferry lane", "op-1", 7).unwrap();
        assert!(serde_json::to_value(&s.list()[0]).unwrap().get("replies").is_none());
        // a retired item may still be answered
        s.deactivate("tk-2").unwrap();
        assert_eq!(s.reply("tk-2", reply(Outcome::Declined, "Nothing to do: it was retired", vec![])).unwrap().replies.len(), 1);
    }

    #[test]
    fn a_reply_is_bounded() {
        let mut s = TaskingStore::default();
        s.post("split the picket", "op-1", 5).unwrap();
        assert_eq!(s.reply("tk-9", reply(Outcome::Done, "x", vec![])), Err(ReplyError::Unknown));
        for bad in ["", "  \n  ", &"x".repeat(REPLY_MAX + 1), "a\n- b\n- c\n- d\n- e\n- f"] {
            assert!(matches!(s.reply("tk-1", reply(Outcome::Done, bad, vec![])), Err(ReplyError::Invalid(_))), "{bad:?}");
        }
        assert!(s.reply("tk-1", reply(Outcome::Done, &"x".repeat(REPLY_MAX), vec![])).is_ok(), "at the cap");
        assert!(s.reply("tk-1", reply(Outcome::Partial, "a\n- b\n- c\n- d\n- e", vec![])).is_ok(), "five lines");
        let many: Vec<u64> = (1..=REPLY_INTENTS_MAX as u64 + 1).collect();
        assert!(matches!(s.reply("tk-1", reply(Outcome::Done, "x", many)), Err(ReplyError::Invalid(_))));
        // characters, not bytes
        assert!(s.reply("tk-1", reply(Outcome::Blocked, &"é".repeat(REPLY_MAX), vec![])).is_ok());
        while s.list()[0].replies.len() < REPLIES_MAX {
            s.reply("tk-1", reply(Outcome::Partial, "again", vec![])).unwrap();
        }
        assert!(matches!(s.reply("tk-1", reply(Outcome::Done, "one more", vec![])), Err(ReplyError::Full(_))));
        assert_eq!(s.list()[0].replies.len(), REPLIES_MAX);
        assert_eq!(["done", "partial", "blocked", "declined", "x"].map(Outcome::parse), [Some(Outcome::Done), Some(Outcome::Partial), Some(Outcome::Blocked), Some(Outcome::Declined), None]);
    }

    #[test]
    fn replies_are_kept_only_where_the_bridge_offers_the_tool() {
        let mut s = TaskingStore::default();
        assert!(!s.replies_kept(), "a bridge without reply_tasking (an evaluation stack, headless)");
        s.keep_replies();
        assert!(s.replies_kept());
    }

    #[test]
    fn the_last_reader_is_kept() {
        let mut s = TaskingStore::default();
        assert_eq!(s.reader(), None);
        s.read_by("agent-watch", 40);
        assert_eq!(s.reader(), Some(("agent-watch", 40)));
    }
}
