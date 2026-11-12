//! The console's decisions message (`type: "decisions"`, [`super::Decision::publish`]) as
//! text, each intent's JSON kept until the intent changes (bd murmur-jvdc). Display only:
//! nothing here decides or is logged.
//!
//! Saturation's 31 orders of 100 vehicles made the message 2.4 MB, all but 82 KB of it their
//! intents (each with its checks on every vehicle), every one of them settled long before; the
//! message was made afresh each second, inside the decision loop's tick, as a `Value` tree and
//! then its text, and was most of the tick. An intent's JSON is a function of the intent alone
//! (and of its tasking's id, whether it waits to be followed, and for an engagement in view the
//! world now): each kept text is reused while the intent is equal to the one it was written
//! from (compared whole, field by field, with no allocation) and those beside it are the same,
//! and the message is written around the kept texts as `serde_json` writes it. A test build
//! writes each kept text afresh and checks it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use murmur_decide::{Intent, IntentId};
use serde_json::Value;

/// Each shown intent's JSON as last written, with what it was written from.
#[derive(Default)]
pub(crate) struct IntentTexts {
    kept: HashMap<IntentId, Kept>,
}

struct Kept {
    intent: Intent,
    tasking: Option<u64>,
    follow: bool,
    text: Arc<str>,
}

impl IntentTexts {
    /// The text kept for `i` as it is now, with its tasking `tasking` and `follow`, if any.
    pub(crate) fn get(&self, i: &Intent, tasking: Option<u64>, follow: bool) -> Option<Arc<str>> {
        let k = self.kept.get(&i.id())?;
        (k.tasking == tasking && k.follow == follow && k.intent == *i).then(|| k.text.clone())
    }

    /// Keep `text`, written from `i` as it is now.
    pub(crate) fn put(&mut self, i: &Intent, tasking: Option<u64>, follow: bool, text: Arc<str>) {
        self.kept.insert(i.id(), Kept { intent: i.clone(), tasking, follow, text });
    }

    /// Only the intents in `shown` are kept.
    pub(crate) fn keep_only(&mut self, shown: &HashSet<IntentId>) {
        self.kept.retain(|id, _| shown.contains(id));
    }
}

/// The message as `json!({"type": "decisions", "inbox": .., "intents": [..], "space": ..,
/// "operators": .., "spent": ..}).to_string()` writes it, with each intent's text as written.
pub(crate) fn text(inbox: &Value, intents: &[Arc<str>], space: &Value, operators: &Value, spent: &Value) -> String {
    let inbox = inbox.to_string();
    let (space, operators, spent) = (space.to_string(), operators.to_string(), spent.to_string());
    let len = inbox.len() + intents.iter().map(|t| t.len() + 1).sum::<usize>() + space.len() + operators.len() + spent.len();
    let mut msg = String::with_capacity(len + 96);
    msg.push_str("{\"type\":\"decisions\",\"inbox\":");
    msg.push_str(&inbox);
    msg.push_str(",\"intents\":[");
    for (k, t) in intents.iter().enumerate() {
        if k > 0 {
            msg.push(',');
        }
        msg.push_str(t);
    }
    msg.push_str("],\"space\":");
    msg.push_str(&space);
    msg.push_str(",\"operators\":");
    msg.push_str(&operators);
    msg.push_str(",\"spent\":");
    msg.push_str(&spent);
    msg.push('}');
    msg
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The message is the one `json!(..).to_string()` makes, byte for byte.
    #[test]
    fn the_message_is_the_one_json_makes() {
        let inbox = json!({"pending": [], "alerts": [{"id": "link:3", "since_t": 1.5}], "counts": {"pending": 0}});
        let intents = [json!({"id": 1, "state": "done", "uids": ["1", "2"], "x": 0.1}), json!({"id": 2, "why": "a \"quote\""})];
        let (space, operators, spent) = (json!({"runs": []}), json!({"operators": ["op-1"], "auth": "dev"}), json!([3, 4]));
        let texts: Vec<Arc<str>> = intents.iter().map(|v| Arc::from(v.to_string())).collect();
        let want = json!({
            "type": "decisions",
            "inbox": inbox,
            "intents": intents,
            "space": space,
            "operators": operators,
            "spent": spent,
        })
        .to_string();
        assert_eq!(text(&inbox, &texts, &space, &operators, &spent), want);
        let none: Vec<Arc<str>> = Vec::new();
        let want = json!({"type": "decisions", "inbox": inbox, "intents": [], "space": space, "operators": operators, "spent": []}).to_string();
        assert_eq!(text(&inbox, &none, &space, &operators, &json!([])), want);
    }
}
