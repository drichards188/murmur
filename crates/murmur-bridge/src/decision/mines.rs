//! Rules 27 (bd murmur-lbw8.4): minefields on the bridge.
//!
//! A lay order (`task = "lay"`: the platform, its line as `route`, `count`, `live_h`, the mine by
//! `weapon`) is submitted as any vehicle order and signed as an engagement is (only an operator
//! with engagement authority, never its proposer, two of them; an agent proposes and signs
//! nothing). Dispatched, it is flown as a mission along its line once (`tasks::TaskKind::Lay`).
//! Each tick [`Decision::step_lays`] hands the engine the picture: it lays each mine the layer has
//! reached (`mine.laid`, the record) and logs a field whose period is over inert
//! (`minefield.inert`); each mine laid goes to the contacts process on the simulation channel
//! (`crate::tracks::Tracks::queue_mine`), which holds it and sets it off by its trigger. The C2
//! learns of a detonation only from its sensors and from a struck report of ours.
//!
//! A field carried over from the last run is handed to the contacts process again at the first
//! tick (the truth side starts empty), and `GET /api/minefields` and the MCP's `get_areas` serve
//! the live fields for the map and for agents (read-only).

use murmur_decide::{Clock, MineOrder, State, TaskKind, WeaponFacts, WorldSnapshot};
use serde_json::{Value, json};

use super::snapshot::bridge_uid;
use super::{Core, Decision};

impl Decision {
    /// Rules 27: the lays' step (the module doc): only while a lay order is dispatched or a field
    /// of ours is held (no world snapshot otherwise).
    pub(super) fn step_lays(&self, core: &mut Core) {
        if !self.rules.mines() {
            return;
        }
        let laying = core
            .engine
            .intents()
            .any(|i| i.spec().kind() == TaskKind::Lay && i.state() == State::Dispatched);
        let fields = core.engine.minefields();
        // a field carried over from the last run: to the truth side again, once
        if !core.mines_resent {
            core.mines_resent = true;
            if let Some(t) = self.tracks() {
                for f in fields.iter().filter(|f| f.from.is_some()) {
                    let Some(w) = self.mine_facts(&f.weapon) else { continue };
                    for m in f.mines.iter().filter(|m| m.inert_t > self.clock.now()) {
                        t.queue_mine(&MineOrder {
                            intent: f.intent,
                            uid: f.uid.clone(),
                            n: m.n,
                            at: m.at,
                            weapon: w.clone(),
                            arms_t: m.arms_t,
                            inert_t: m.inert_t,
                        });
                    }
                }
            }
        }
        if !laying && fields.is_empty() {
            return;
        }
        let world = if laying { self.snapshot() } else { WorldSnapshot::new(self.clock.now()) };
        match core.engine.step_lays(&world) {
            Ok(step) => {
                if let Some(t) = self.tracks() {
                    for m in &step.laid {
                        t.queue_mine(m);
                    }
                }
                for m in &step.laid {
                    println!("decision: MF-{} mine {} laid at ({:.6}, {:.6})", m.intent.0, m.n, m.at.lon, m.at.lat);
                }
                for f in step.inert {
                    println!("decision: minefield {f} inert");
                }
            }
            Err(e) => eprintln!("decision: step_lays: {e}"),
        }
    }

    /// A mine's catalog facts by its name (the first platform that carries it).
    fn mine_facts(&self, name: &str) -> Option<WeaponFacts> {
        self.catalog.platforms.values().flat_map(|p| p.loadout.iter()).find(|w| w.name == name).cloned()
    }

    /// Rules 27: our fields not logged inert, for the console's map and the agents (`GET
    /// /api/minefields`, `get_areas`): each with its lay order, layer, mine, trigger reach and
    /// danger area (the no-go area's half-width round its mines), the domains it catches, its
    /// mines (place, laid, armed, inert: unix s) and whether any is still live.
    pub fn minefields_json(&self) -> Vec<Value> {
        let now = self.clock.now();
        let fields = self.lock().engine.minefields();
        fields
            .iter()
            .map(|f| {
                json!({
                    "id": f.id(),
                    "intent": f.intent.0,
                    "from": f.from,
                    "layer": bridge_uid(&f.uid),
                    "weapon": f.weapon,
                    "trigger": f.trigger,
                    "reach_m": f.reach_m,
                    "danger_m": f.danger_m(),
                    "targets": f.targets,
                    "live": f.live_at(now),
                    "inert_t": secs(f.inert_t()),
                    "mines": f.mines.iter().map(|m| json!({
                        "n": m.n,
                        "at": [m.at.lon, m.at.lat],
                        "laid_t": secs(m.laid_t),
                        "arms_t": secs(m.arms_t),
                        "inert_t": secs(m.inert_t),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect()
    }
}

fn secs(t: murmur_decide::Time) -> f64 {
    t.0 as f64 / 1000.0
}
