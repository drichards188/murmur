//! Operator groups: named, mutable, server-shared sets of our vehicle
//! uids the operator makes and edits while the stack runs. Distinct from a fleet swarm (fixed
//! by the fleet file, `crate::swarms`).
//!
//! A group is an organizing convenience and nothing the decision layer reads: it is never
//! logged, never in the audit log, and this module imports nothing from `murmur_decide`.
//! Commanding a group is the caller's separate act (a normal group order with the group's
//! current members). In memory only: a stack restart starts with none (persisting them is a
//! later option).

use std::collections::HashSet;

use serde::Serialize;

use crate::vehicle::Uid;

/// The longest group name, in characters.
pub const NAME_MAX: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub members: Vec<Uid>,
    pub created_t: u64,
    pub updated_t: u64,
}

/// What a `PATCH` may change; every field is optional.
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub name: Option<String>,
    /// `Some(None)` clears the colour
    pub color: Option<Option<String>>,
    pub add: Vec<Uid>,
    pub remove: Vec<Uid>,
    pub members: Option<Vec<Uid>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GroupError {
    Name(String),
    Color(String),
}

impl std::fmt::Display for GroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupError::Name(s) | GroupError::Color(s) => f.write_str(s),
        }
    }
}

#[derive(Default)]
pub struct GroupStore {
    groups: Vec<Group>,
    next: u64,
}

fn check_name(name: &str) -> Result<String, GroupError> {
    let n = name.trim();
    if n.is_empty() {
        return Err(GroupError::Name("a group needs a name".into()));
    }
    if n.chars().count() > NAME_MAX {
        return Err(GroupError::Name(format!("a group's name is at most {NAME_MAX} characters")));
    }
    Ok(n.to_string())
}

fn check_color(color: Option<String>) -> Result<Option<String>, GroupError> {
    let Some(c) = color.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) else {
        return Ok(None);
    };
    let ok = c.len() <= 32 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || "#(),.% ".contains(ch));
    if ok { Ok(Some(c)) } else { Err(GroupError::Color("a colour is a CSS colour such as #c49bff".into())) }
}

/// De-duplicate, keep order, split into (kept, dropped) by the known fleet.
fn sift(uids: &[Uid], known: &HashSet<Uid>) -> (Vec<Uid>, Vec<Uid>) {
    let mut seen = HashSet::new();
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    for &u in uids {
        if !seen.insert(u) {
            continue;
        }
        if known.contains(&u) { kept.push(u) } else { dropped.push(u) }
    }
    (kept, dropped)
}

impl GroupStore {
    pub fn list(&self) -> Vec<Group> {
        self.groups.clone()
    }

    /// Make a group; returns it and the uids dropped as not in the fleet.
    pub fn create(
        &mut self,
        name: &str,
        members: &[Uid],
        color: Option<String>,
        known: &HashSet<Uid>,
        now_ms: u64,
    ) -> Result<(Group, Vec<Uid>), GroupError> {
        let name = check_name(name)?;
        let color = check_color(color)?;
        let (members, dropped) = sift(members, known);
        self.next += 1;
        let g = Group {
            id: format!("og-{}", self.next),
            name,
            color,
            members,
            created_t: now_ms,
            updated_t: now_ms,
        };
        self.groups.push(g.clone());
        Ok((g, dropped))
    }

    /// Change a group: `members` replaces, then `add`, then `remove`. `Ok(None)` for an
    /// unknown id. Nothing changes if the patch is refused.
    pub fn patch(
        &mut self,
        id: &str,
        p: Patch,
        known: &HashSet<Uid>,
        now_ms: u64,
    ) -> Result<Option<(Group, Vec<Uid>)>, GroupError> {
        let name = p.name.as_deref().map(check_name).transpose()?;
        let color = match p.color {
            Some(c) => Some(check_color(c)?),
            None => None,
        };
        let Some(g) = self.groups.iter_mut().find(|g| g.id == id) else {
            return Ok(None);
        };
        let mut dropped = Vec::new();
        if let Some(n) = name {
            g.name = n;
        }
        if let Some(c) = color {
            g.color = c;
        }
        if let Some(all) = &p.members {
            let (kept, d) = sift(all, known);
            g.members = kept;
            dropped.extend(d);
        }
        if !p.add.is_empty() {
            let (kept, d) = sift(&p.add, known);
            dropped.extend(d);
            for u in kept {
                if !g.members.contains(&u) {
                    g.members.push(u);
                }
            }
        }
        if !p.remove.is_empty() {
            let gone: HashSet<Uid> = p.remove.iter().copied().collect();
            g.members.retain(|u| !gone.contains(u));
        }
        g.updated_t = now_ms;
        Ok(Some((g.clone(), dropped)))
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g.id != id);
        self.groups.len() != before
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn known() -> HashSet<Uid> {
        (1..=10).collect()
    }

    #[test]
    fn create_dedups_keeps_order_and_drops_unknown() {
        let mut s = GroupStore::default();
        let (g, d) = s.create(" North ", &[5, 2, 5, 99, 3], None, &known(), 7).unwrap();
        assert_eq!(g.id, "og-1");
        assert_eq!(g.name, "North");
        assert_eq!(g.members, vec![5, 2, 3]);
        assert_eq!(d, vec![99]);
        assert_eq!((g.created_t, g.updated_t), (7, 7));
        let (g2, _) = s.create("B", &[], Some("#c49bff".into()), &known(), 8).unwrap();
        assert_eq!(g2.id, "og-2");
        assert_eq!(g2.color.as_deref(), Some("#c49bff"));
        assert_eq!(s.list().len(), 2);
    }

    #[test]
    fn names_are_checked() {
        let mut s = GroupStore::default();
        assert!(s.create("  ", &[], None, &known(), 0).is_err());
        assert!(s.create(&"x".repeat(65), &[], None, &known(), 0).is_err());
        assert!(s.create(&"x".repeat(64), &[], None, &known(), 0).is_ok());
        assert!(s.create("a", &[], Some("red;evil".into()), &known(), 0).is_err());
    }

    #[test]
    fn patch_add_remove_replace() {
        let mut s = GroupStore::default();
        s.create("A", &[1, 2, 3], None, &known(), 1).unwrap();
        let p = Patch { add: vec![3, 4, 77], remove: vec![1], ..Patch::default() };
        let (g, d) = s.patch("og-1", p, &known(), 2).unwrap().unwrap();
        assert_eq!(g.members, vec![2, 3, 4]);
        assert_eq!(d, vec![77]);
        assert_eq!(g.updated_t, 2);
        let p = Patch { members: Some(vec![9, 9, 8]), ..Patch::default() };
        let (g, _) = s.patch("og-1", p, &known(), 3).unwrap().unwrap();
        assert_eq!(g.members, vec![9, 8]);
        let p = Patch { name: Some("Z".into()), color: Some(None), ..Patch::default() };
        let (g, _) = s.patch("og-1", p, &known(), 4).unwrap().unwrap();
        assert_eq!((g.name.as_str(), g.color), ("Z", None));
    }

    #[test]
    fn refused_patch_changes_nothing_and_unknown_is_none() {
        let mut s = GroupStore::default();
        s.create("A", &[1], None, &known(), 1).unwrap();
        let p = Patch { name: Some(String::new()), add: vec![2], ..Patch::default() };
        assert!(s.patch("og-1", p, &known(), 2).is_err());
        assert_eq!(s.list()[0].members, vec![1]);
        assert!(s.patch("og-9", Patch::default(), &known(), 2).unwrap().is_none());
    }

    #[test]
    fn delete_and_ids_are_not_reused() {
        let mut s = GroupStore::default();
        s.create("A", &[], None, &known(), 1).unwrap();
        assert!(s.delete("og-1"));
        assert!(!s.delete("og-1"));
        let (g, _) = s.create("B", &[], None, &known(), 2).unwrap();
        assert_eq!(g.id, "og-2");
    }
}
