//! Operator groups: named, mutable, server-shared sets of our vehicle
//! uids the operator makes and edits while the stack runs. Distinct from a fleet swarm (fixed
//! by the fleet file, `crate::swarms`).
//!
//! A group is an organizing convenience and nothing the decision layer reads: it is never
//! logged, never in the audit log, and this module imports nothing from `murmur_decide`.
//! Commanding a group is the caller's separate act (a normal group order with the group's
//! current members). In memory only: a stack restart starts with none (persisting them is a
//! later option).
//!
//! Each group says who made it (`by`). The watch officer, an agent, may make groups too, but only
//! for one of the operator's tasking items and only through its own narrow path
//! ([`GroupStore::agent_make`], [`GroupStore::agent_change`]; MCP `make_group`): such a group is
//! marked `agent` with the item it was made for (`tasking`), at most [`AGENT_GROUPS_MAX`] a run per
//! agent, its members all in the fleet. An agent changes only a group it made itself that no
//! operator has changed since (`changed_by`), and never deletes one; the operator may change or
//! delete any group, an agent's included. The operator's own routes stay the operator's: an agent
//! is refused there (`crate::api`).

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::vehicle::Uid;

/// The longest group name, in characters.
pub const NAME_MAX: usize = 64;
/// The most groups one agent may make in a run (the bridge's life): a request splits a few sets,
/// never a group per vehicle.
pub const AGENT_GROUPS_MAX: usize = 12;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub members: Vec<Uid>,
    pub created_t: u64,
    pub updated_t: u64,
    /// who made it: an operator's id, or an agent's (then `agent` is true)
    pub by: String,
    /// made by an agent (the watch officer) for the operator's tasking item `tasking`
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub agent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tasking: Option<String>,
    /// who changed it last, when anyone has: an operator's change to an agent's group takes it
    /// from the agent, which may change it no more
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<String>,
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

/// Why an agent's group was refused (nothing changed).
#[derive(Debug, PartialEq, Eq)]
pub enum AgentGroupError {
    /// a name, a colour or the members will not do
    Invalid(String),
    /// no group of that id
    Unknown(String),
    /// a group this agent may not change: the operator's, another agent's, one made for another
    /// item, or one an operator has changed since
    NotYours(String),
    /// the agent has made its [`AGENT_GROUPS_MAX`] this run
    Spent(String),
}

impl std::fmt::Display for AgentGroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentGroupError::Invalid(s)
            | AgentGroupError::Unknown(s)
            | AgentGroupError::NotYours(s)
            | AgentGroupError::Spent(s) => f.write_str(s),
        }
    }
}

impl From<GroupError> for AgentGroupError {
    fn from(e: GroupError) -> Self {
        AgentGroupError::Invalid(e.to_string())
    }
}

/// A group an agent asks for ([`GroupStore::agent_make`]).
#[derive(Clone, Copy, Debug)]
pub struct AgentGroup<'a> {
    pub name: &'a str,
    pub members: &'a [Uid],
    pub color: Option<&'a str>,
}

/// What an agent asks of its own group ([`GroupStore::agent_change`]); every field optional.
#[derive(Clone, Debug, Default)]
pub struct AgentPatch {
    pub name: Option<String>,
    /// `Some(None)` clears the colour
    pub color: Option<Option<String>>,
    /// replaces the members
    pub members: Option<Vec<Uid>>,
}

#[derive(Default)]
pub struct GroupStore {
    groups: Vec<Group>,
    next: u64,
    /// groups each agent made this run (a deleted one still counts)
    agent_made: HashMap<String, usize>,
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

/// An agent's members: at least one, every one in the fleet (an operator's unknown uids are left
/// out and named; an agent's are refused, so it never makes a group other than it meant).
fn agent_members(uids: &[Uid], known: &HashSet<Uid>) -> Result<Vec<Uid>, AgentGroupError> {
    let (kept, dropped) = sift(uids, known);
    if !dropped.is_empty() {
        let list: Vec<String> = dropped.iter().map(ToString::to_string).collect();
        return Err(AgentGroupError::Invalid(format!(
            "uids {} are not in the fleet: a group names only our vehicles (get_fleet)",
            list.join(", ")
        )));
    }
    if kept.is_empty() {
        return Err(AgentGroupError::Invalid("a group needs at least one member".into()));
    }
    Ok(kept)
}

impl GroupStore {
    pub fn list(&self) -> Vec<Group> {
        self.groups.clone()
    }

    /// Make an operator's group (`by`: the operator); returns it and the uids dropped as not in
    /// the fleet.
    pub fn create(
        &mut self,
        name: &str,
        members: &[Uid],
        color: Option<String>,
        known: &HashSet<Uid>,
        now_ms: u64,
        by: &str,
    ) -> Result<(Group, Vec<Uid>), GroupError> {
        let name = check_name(name)?;
        let color = check_color(color)?;
        let (members, dropped) = sift(members, known);
        let g = self.push(name, color, members, now_ms, by, None);
        Ok((g, dropped))
    }

    fn push(&mut self, name: String, color: Option<String>, members: Vec<Uid>, now_ms: u64, by: &str, tasking: Option<&str>) -> Group {
        self.next += 1;
        let g = Group {
            id: format!("og-{}", self.next),
            name,
            color,
            members,
            created_t: now_ms,
            updated_t: now_ms,
            by: by.to_string(),
            agent: tasking.is_some(),
            tasking: tasking.map(str::to_string),
            changed_by: None,
        };
        self.groups.push(g.clone());
        g
    }

    /// Change a group, as the operator `by`: `members` replaces, then `add`, then `remove`.
    /// `Ok(None)` for an unknown id. Nothing changes if the patch is refused. Any group, an
    /// agent's included: the operator's change takes an agent's group from it (`changed_by`).
    pub fn patch(
        &mut self,
        id: &str,
        p: Patch,
        known: &HashSet<Uid>,
        now_ms: u64,
        by: &str,
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
        g.changed_by = Some(by.to_string());
        Ok(Some((g.clone(), dropped)))
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g.id != id);
        self.groups.len() != before
    }

    /// The groups `agent` made for the tasking item `tasking`, oldest first.
    pub fn made_for(&self, agent: &str, tasking: &str) -> Vec<&Group> {
        self.groups
            .iter()
            .filter(|g| g.agent && g.by == agent && g.tasking.as_deref() == Some(tasking))
            .collect()
    }

    /// Make a group as `agent` for the operator's tasking item `tasking` (the caller checks that
    /// the item is one in force): every member in the fleet, at least one, at most
    /// [`AGENT_GROUPS_MAX`] a run. Nothing changes if it is refused.
    pub fn agent_make(
        &mut self,
        agent: &str,
        tasking: &str,
        asked: AgentGroup,
        known: &HashSet<Uid>,
        now_ms: u64,
    ) -> Result<Group, AgentGroupError> {
        let name = check_name(asked.name)?;
        let color = check_color(asked.color.map(str::to_string))?;
        let members = agent_members(asked.members, known)?;
        let made = self.agent_made.get(agent).copied().unwrap_or(0);
        if made >= AGENT_GROUPS_MAX {
            return Err(AgentGroupError::Spent(format!(
                "{agent} has made its {AGENT_GROUPS_MAX} groups this run: change one it made (group_id), or say in the reply what the operator should group"
            )));
        }
        *self.agent_made.entry(agent.to_string()).or_default() += 1;
        Ok(self.push(name, color, members, now_ms, agent, Some(tasking)))
    }

    /// Change a group `agent` made for `tasking`, while no operator has changed it since: its name,
    /// its colour, its members (replaced; every one in the fleet). Nothing changes if it is
    /// refused.
    pub fn agent_change(
        &mut self,
        id: &str,
        agent: &str,
        tasking: &str,
        p: AgentPatch,
        known: &HashSet<Uid>,
        now_ms: u64,
    ) -> Result<Group, AgentGroupError> {
        let name = p.name.as_deref().map(check_name).transpose()?;
        let color = match p.color {
            Some(c) => Some(check_color(c)?),
            None => None,
        };
        let members = p.members.as_deref().map(|m| agent_members(m, known)).transpose()?;
        let g = self
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or_else(|| AgentGroupError::Unknown(format!("no group {id}")))?;
        if !g.agent || g.by != agent {
            return Err(AgentGroupError::NotYours(format!(
                "{id} was made by {}: an agent changes only the groups it made itself",
                g.by
            )));
        }
        if g.tasking.as_deref() != Some(tasking) {
            return Err(AgentGroupError::NotYours(format!(
                "{id} was made for tasking {}, not {tasking}",
                g.tasking.as_deref().unwrap_or("none")
            )));
        }
        if let Some(by) = g.changed_by.as_deref().filter(|b| *b != agent) {
            return Err(AgentGroupError::NotYours(format!(
                "{id} was changed by {by} since you made it: it is theirs now, and you may not change it"
            )));
        }
        if let Some(n) = name {
            g.name = n;
        }
        if let Some(c) = color {
            g.color = c;
        }
        if let Some(m) = members {
            g.members = m;
        }
        g.updated_t = now_ms;
        g.changed_by = Some(agent.to_string());
        Ok(g.clone())
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
        let (g, d) = s.create(" North ", &[5, 2, 5, 99, 3], None, &known(), 7, "op-1").unwrap();
        assert_eq!(g.id, "og-1");
        assert_eq!(g.name, "North");
        assert_eq!(g.members, vec![5, 2, 3]);
        assert_eq!(d, vec![99]);
        assert_eq!((g.created_t, g.updated_t), (7, 7));
        assert_eq!((g.by.as_str(), g.agent, g.tasking.as_deref(), g.changed_by.as_deref()), ("op-1", false, None, None));
        let (g2, _) = s.create("B", &[], Some("#c49bff".into()), &known(), 8, "op-1").unwrap();
        assert_eq!(g2.id, "og-2");
        assert_eq!(g2.color.as_deref(), Some("#c49bff"));
        assert_eq!(s.list().len(), 2);
        // an operator's group says who made it, and nothing of an agent's
        let v = serde_json::to_value(&g2).unwrap();
        assert_eq!(v["by"], "op-1");
        assert!(v.get("agent").is_none() && v.get("tasking").is_none() && v.get("changed_by").is_none(), "{v}");
    }

    #[test]
    fn names_are_checked() {
        let mut s = GroupStore::default();
        assert!(s.create("  ", &[], None, &known(), 0, "op-1").is_err());
        assert!(s.create(&"x".repeat(65), &[], None, &known(), 0, "op-1").is_err());
        assert!(s.create(&"x".repeat(64), &[], None, &known(), 0, "op-1").is_ok());
        assert!(s.create("a", &[], Some("red;evil".into()), &known(), 0, "op-1").is_err());
    }

    #[test]
    fn patch_add_remove_replace() {
        let mut s = GroupStore::default();
        s.create("A", &[1, 2, 3], None, &known(), 1, "op-1").unwrap();
        let p = Patch { add: vec![3, 4, 77], remove: vec![1], ..Patch::default() };
        let (g, d) = s.patch("og-1", p, &known(), 2, "op-2").unwrap().unwrap();
        assert_eq!(g.members, vec![2, 3, 4]);
        assert_eq!(d, vec![77]);
        assert_eq!(g.updated_t, 2);
        assert_eq!((g.by.as_str(), g.changed_by.as_deref()), ("op-1", Some("op-2")));
        let p = Patch { members: Some(vec![9, 9, 8]), ..Patch::default() };
        let (g, _) = s.patch("og-1", p, &known(), 3, "op-1").unwrap().unwrap();
        assert_eq!(g.members, vec![9, 8]);
        let p = Patch { name: Some("Z".into()), color: Some(None), ..Patch::default() };
        let (g, _) = s.patch("og-1", p, &known(), 4, "op-1").unwrap().unwrap();
        assert_eq!((g.name.as_str(), g.color), ("Z", None));
    }

    #[test]
    fn refused_patch_changes_nothing_and_unknown_is_none() {
        let mut s = GroupStore::default();
        s.create("A", &[1], None, &known(), 1, "op-1").unwrap();
        let p = Patch { name: Some(String::new()), add: vec![2], ..Patch::default() };
        assert!(s.patch("og-1", p, &known(), 2, "op-1").is_err());
        assert_eq!(s.list()[0].members, vec![1]);
        assert_eq!(s.list()[0].changed_by, None);
        assert!(s.patch("og-9", Patch::default(), &known(), 2, "op-1").unwrap().is_none());
    }

    #[test]
    fn delete_and_ids_are_not_reused() {
        let mut s = GroupStore::default();
        s.create("A", &[], None, &known(), 1, "op-1").unwrap();
        assert!(s.delete("og-1"));
        assert!(!s.delete("og-1"));
        let (g, _) = s.create("B", &[], None, &known(), 2, "op-1").unwrap();
        assert_eq!(g.id, "og-2");
    }

    #[test]
    fn an_agent_makes_a_group_for_a_request_marked_as_its_own() {
        let mut s = GroupStore::default();
        let g = s.agent_make("agent-watch", "tk-3", AgentGroup { name: " West picket ", members: &[1, 2, 2, 3], color: Some("#7fb8ff") }, &known(), 5).unwrap();
        assert_eq!((g.id.as_str(), g.name.as_str(), g.members.clone()), ("og-1", "West picket", vec![1, 2, 3]));
        assert_eq!((g.by.as_str(), g.agent, g.tasking.as_deref(), g.changed_by.as_deref()), ("agent-watch", true, Some("tk-3"), None));
        let v = serde_json::to_value(&g).unwrap();
        assert_eq!((v["by"].clone(), v["agent"].clone(), v["tasking"].clone()), (serde_json::json!("agent-watch"), serde_json::json!(true), serde_json::json!("tk-3")));
        s.agent_make("agent-watch", "tk-3", AgentGroup { name: "East picket", members: &[4, 5], color: None }, &known(), 6).unwrap();
        s.create("Mine", &[6], None, &known(), 7, "op-1").unwrap();
        let mine: Vec<&str> = s.made_for("agent-watch", "tk-3").iter().map(|g| g.id.as_str()).collect();
        assert_eq!(mine, ["og-1", "og-2"]);
        assert!(s.made_for("agent-watch", "tk-4").is_empty() && s.made_for("agent-planner", "tk-3").is_empty());
    }

    #[test]
    fn an_agents_group_names_only_the_fleet_and_is_refused_whole() {
        let mut s = GroupStore::default();
        for (name, members) in [("A", vec![1, 99]), ("A", vec![]), ("  ", vec![1]), (&*"x".repeat(NAME_MAX + 1), vec![1])] {
            assert!(matches!(s.agent_make("agent-watch", "tk-1", AgentGroup { name, members: &members, color: None }, &known(), 1), Err(AgentGroupError::Invalid(_))), "{name} {members:?}");
        }
        assert!(matches!(s.agent_make("agent-watch", "tk-1", AgentGroup { name: "A", members: &[1], color: Some("red;evil") }, &known(), 1), Err(AgentGroupError::Invalid(_))));
        let e = s.agent_make("agent-watch", "tk-1", AgentGroup { name: "A", members: &[1, 99, 100], color: None }, &known(), 1).unwrap_err();
        assert!(e.to_string().contains("uids 99, 100 are not in the fleet"), "{e}");
        assert!(s.list().is_empty(), "nothing made");
        // a refused one is not counted against the limit
        for n in 0..AGENT_GROUPS_MAX {
            s.agent_make("agent-watch", "tk-1", AgentGroup { name: &format!("G{n}"), members: &[1], color: None }, &known(), 2).unwrap();
        }
        assert!(matches!(s.agent_make("agent-watch", "tk-1", AgentGroup { name: "one more", members: &[1], color: None }, &known(), 3), Err(AgentGroupError::Spent(_))));
        // deleting one gives none back; another agent has its own count
        assert!(s.delete("og-1"));
        assert!(matches!(s.agent_make("agent-watch", "tk-2", AgentGroup { name: "again", members: &[1], color: None }, &known(), 4), Err(AgentGroupError::Spent(_))));
        assert!(s.agent_make("agent-planner", "tk-1", AgentGroup { name: "planner's", members: &[1], color: None }, &known(), 5).is_ok());
    }

    #[test]
    fn an_agent_changes_only_its_own_group_until_an_operator_changes_it() {
        let mut s = GroupStore::default();
        s.create("Operator's", &[1, 2], None, &known(), 1, "op-1").unwrap(); // og-1
        s.agent_make("agent-watch", "tk-1", AgentGroup { name: "West", members: &[3, 4], color: None }, &known(), 2).unwrap(); // og-2
        s.agent_make("agent-planner", "tk-1", AgentGroup { name: "Planner's", members: &[5], color: None }, &known(), 3).unwrap(); // og-3
        let change = |members: Vec<Uid>| AgentPatch { members: Some(members), ..AgentPatch::default() };
        // the operator's, another agent's, another item's, an unknown one: refused, nothing changed
        assert!(matches!(s.agent_change("og-1", "agent-watch", "tk-1", change(vec![9]), &known(), 4), Err(AgentGroupError::NotYours(_))));
        assert!(matches!(s.agent_change("og-3", "agent-watch", "tk-1", change(vec![9]), &known(), 4), Err(AgentGroupError::NotYours(_))));
        assert!(matches!(s.agent_change("og-2", "agent-watch", "tk-2", change(vec![9]), &known(), 4), Err(AgentGroupError::NotYours(_))));
        assert!(matches!(s.agent_change("og-9", "agent-watch", "tk-1", change(vec![9]), &known(), 4), Err(AgentGroupError::Unknown(_))));
        assert!(matches!(s.agent_change("og-2", "agent-watch", "tk-1", change(vec![99]), &known(), 4), Err(AgentGroupError::Invalid(_))));
        assert_eq!(s.list().iter().map(|g| g.members.clone()).collect::<Vec<_>>(), vec![vec![1, 2], vec![3, 4], vec![5]]);
        // its own: name, colour and members
        let p = AgentPatch { name: Some("West span".into()), color: Some(Some("#f2c14e".into())), members: Some(vec![4, 3, 6]) };
        let g = s.agent_change("og-2", "agent-watch", "tk-1", p, &known(), 5).unwrap();
        assert_eq!((g.name.as_str(), g.color.as_deref(), g.members.clone(), g.updated_t), ("West span", Some("#f2c14e"), vec![4, 3, 6], 5));
        assert_eq!((g.by.as_str(), g.changed_by.as_deref()), ("agent-watch", Some("agent-watch")));
        // once an operator changes it, it is theirs: the agent may change it no more
        s.patch("og-2", Patch { remove: vec![6], ..Patch::default() }, &known(), 6, "op-1").unwrap().unwrap();
        let e = s.agent_change("og-2", "agent-watch", "tk-1", change(vec![3]), &known(), 7).unwrap_err();
        assert!(matches!(e, AgentGroupError::NotYours(_)) && e.to_string().contains("changed by op-1"), "{e}");
        let g = &s.list()[1];
        assert_eq!((g.members.clone(), g.by.as_str(), g.agent, g.changed_by.as_deref()), (vec![4, 3], "agent-watch", true, Some("op-1")));
        // and the operator may delete an agent's group
        assert!(s.delete("og-2"));
    }
}
