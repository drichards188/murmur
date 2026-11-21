//! Operator tasking for the live AI watch officer: guidance an
//! operator types ("prioritise the ferry lane", "ignore the sailboats north of the span") that
//! the officer reads and weighs on each wake.
//!
//! It is guidance and priority, never authority: the officer still proposes orders that go
//! through the engine and the policy, and nothing here reaches a gated decision. Like groups
//! it is never logged and this module imports nothing from `murmur_decide`. In memory only.

use serde::Serialize;

/// The longest guidance text, in characters.
pub const TEXT_MAX: usize = 500;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TaskingItem {
    pub id: String,
    pub text: String,
    pub by: String,
    pub at_t: u64,
    pub active: bool,
}

#[derive(Default)]
pub struct TaskingStore {
    /// oldest first; `list` answers newest first
    items: Vec<TaskingItem>,
    next: u64,
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
        };
        self.items.push(item.clone());
        Ok(item)
    }

    /// Retire an item (idempotent); `None` for an unknown id.
    pub fn deactivate(&mut self, id: &str) -> Option<TaskingItem> {
        let item = self.items.iter_mut().find(|i| i.id == id)?;
        item.active = false;
        Some(item.clone())
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
}
