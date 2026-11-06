//! One second of the truth side, shared by the contacts process (`main.rs`) and the headless
//! simulator (crates/murmur-headless `sim/contacts.rs`) so that the two cannot drift
//! (docs/AUTONOMOUS-NODES.md stage 5, bd murmur-y85j.5, murmur-mmxk).
//!
//! After the movers stepped, [`Step::tick`]: the effects whose munition has come strike (rules
//! 20), every live mine steps against what moved (rules 27), every node runs its order with our
//! vehicles as the bridge last answered them ([`Nodes::tick`]), and the truth message is built:
//! the scripted acts of the second, an armed author's launch (or why it held its fire), the
//! nodes' own shots and events, their looks, the weapons in flight and the strikes due, each
//! drawn by the damage model. [`Step::answer`] reads the bridge's answer back: the mines laid
//! since, where ours are (the weapons' aim, the mines' victims and the nodes' looks), and the
//! effects to apply.
//!
//! What differs between the two callers is only around it: where a drifting contact's current
//! comes from (tidewater, or the episode's water), how the message reaches the bridge (HTTP, or
//! the bridge's own router in process), how an engagement's effect is applied (the headless
//! episode may draw it: the `land` callback), and where our vehicles' damage goes (the stand-ins'
//! damage file, or the episode's fleet: [`Second::mined`] and [`Second::struck`]).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::effect::{self, DamageCtx, EffectApplied, EffectNotice, OursDamaged};
use crate::mine::{MineNotice, Minefields};
use crate::motion::Mover;
use crate::node::{LandWater, Nodes};
use crate::scenario::Scenario;
use crate::truth::TruthMessage;
use crate::weapon::{self, Armory, OursAt, OursLedger};

/// Our vehicles from a bridge answer older than this (s) are no longer what a node perceives.
pub const OURS_FRESH_S: f64 = 5.0;

/// The scenario's nodes (its contacts of armed kinds and those its orders name), steering on the
/// land/water grid its `[sensing] terrain` names (under `root`) when orders can come: the file's,
/// or with `any_second` (the process's own API, the headless simulator's timeline) at any
/// second; and what the contacts process says of the grid (none: no orders can come).
pub fn nodes(scenario: &Scenario, movers: &[Mover], root: &Path, any_second: bool) -> (Nodes, Option<String>) {
    let nodes = Nodes::new(scenario, movers);
    if scenario.orders.is_empty() && !any_second {
        return (nodes, None);
    }
    match &scenario.sensing.terrain {
        Some(t) => match LandWater::load(&root.join(t)) {
            Ok(g) => {
                let said = format!("nodes: steering keeps to the land/water grid {}", g.source);
                (nodes.with_grid(g), Some(said))
            }
            Err(e) => (nodes, Some(format!("nodes: no land/water grid ({e}): steering is not checked against it"))),
        },
        None => (nodes, Some("nodes: the scenario names no [sensing] terrain: steering is not checked against land and water".into())),
    }
}

/// Ours from an answer at `at` as a node may perceive them at `t` ([`OURS_FRESH_S`]).
fn fresh(ours: &[OursAt], at: f64, t: f64) -> &[OursAt] {
    if t - at <= OURS_FRESH_S { ours } else { &[] }
}

/// The truth side's state from one second to the next.
pub struct Step {
    /// The scenario clock at the start, unix s.
    t0: f64,
    seed: u64,
    seq: u64,
    /// The last message's scenario second (acts in between are committed now) and unix time.
    prev_s: f64,
    prev_t: f64,
    /// Effects applied since the last message (Phase E), for the next one.
    applied: Vec<EffectApplied>,
    /// Rules 20: effects whose munition is still in flight, applied at its impact.
    in_flight: effect::InFlight,
    /// Opponent weapons in flight, and what the damage model reads of a node's strikes.
    armory: Armory,
    /// Rules 27: our minefields, laid by the decision engine and handed over by the bridge.
    fields: Minefields,
    /// Every one of ours the bridge has named, where it last said it was.
    ours: BTreeMap<u64, OursAt>,
    /// Ours in the bridge's latest answer (what a node may perceive), and when.
    ours_now: Vec<OursAt>,
    ours_now_t: f64,
    /// Our vehicles a scripted armed author's acts name (`aim_at`).
    aim_at: Vec<u64>,
    /// Ours the damage model destroyed (never drawn again, bd murmur-o72k) and ours a training
    /// aid spares (`[training] spared`).
    ledger: OursLedger,
}

/// What one second gave: its truth message, and our vehicles hurt in it (by a mine, at the
/// second's unix time; by a weapon's strike, at the message's `t`), for the vehicle simulation.
pub struct Second {
    pub msg: TruthMessage,
    pub mined: Vec<OursDamaged>,
    pub struck: Vec<OursDamaged>,
}

impl Step {
    /// The scenario clock starts at `t0` (unix s); `seed` is the run's (the truth message's).
    pub fn new(scenario: &Scenario, seed: u64, t0: f64) -> Step {
        Step {
            t0,
            seed,
            seq: 0,
            prev_s: f64::NEG_INFINITY,
            prev_t: f64::NEG_INFINITY,
            applied: Vec::new(),
            in_flight: effect::InFlight::default(),
            armory: Armory::default(),
            fields: Minefields::default(),
            ours: BTreeMap::new(),
            ours_now: Vec::new(),
            ours_now_t: f64::NEG_INFINITY,
            aim_at: weapon::aim_at(scenario),
            ledger: OursLedger::sparing(scenario.training.as_ref().map(|t| t.spared.clone()).unwrap_or_default()),
        }
    }

    /// Ours the damage model has destroyed so far, and ours a training aid spares.
    pub fn ledger(&self) -> &OursLedger {
        &self.ledger
    }

    /// Ours as a node may perceive them at unix time `t`: the bridge's latest answer, unless it
    /// is older than [`OURS_FRESH_S`].
    pub fn perceived(&self, t: f64) -> &[OursAt] {
        fresh(&self.ours_now, self.ours_now_t, t)
    }

    /// Scenario second `s`, the movers having stepped `dt` to it. `land` applies one effect to
    /// the movers at a scenario second and says what it did.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        scenario: &Scenario,
        s: f64,
        dt: f64,
        movers: &mut [Mover],
        nodes: &mut Nodes,
        ctx: &DamageCtx,
        land: &mut dyn FnMut(&EffectNotice, &mut [Mover], f64) -> EffectApplied,
    ) -> Second {
        let now = self.t0 + s;
        // rules 20: a munition whose impact has come strikes now, before this truth goes out
        for e in self.in_flight.due(now) {
            let a = land(&e, movers, s);
            self.applied.push(a);
        }
        // rules 27: every live mine against what moved this second (a wreck of ours sets off nothing)
        let mined = if self.ledger.destroyed.is_empty() {
            self.fields.step(now, dt, movers, &self.ours, ctx)
        } else {
            let alive: BTreeMap<u64, OursAt> =
                self.ours.iter().filter(|(u, _)| !self.ledger.destroyed.contains(u)).map(|(u, o)| (*u, o.clone())).collect();
            self.fields.step(now, dt, movers, &alive, ctx)
        };
        for d in &mined.ours_damaged {
            if d.state == crate::damage::DamageState::Destroyed
                && let Ok(u) = d.uid.parse::<u64>()
            {
                self.ledger.destroyed.insert(u);
            }
        }
        // every node with an order runs it: its shots fly from here, its steering is the next step's
        let tick = nodes.tick(s, now, movers, fresh(&self.ours_now, self.ours_now_t, now), &mut self.armory);
        self.seq += 1;
        let mut msg = TruthMessage::build(now, s, self.seq, &scenario.title, self.seed, scenario.sensing.clone(), movers);
        msg.effects = std::mem::take(&mut self.applied);
        // rules 9: the hostile acts of this second (truth; each once)
        msg.acts = TruthMessage::acts_between(&scenario.acts, self.prev_s, s, self.t0, movers);
        // an armed author launches its weapon (or holds its fire)
        self.armory.launch(scenario, &mut msg.acts, movers, &self.ours);
        // the nodes' own shots (their flights are in the armory) and what they decided
        msg.acts.extend(tick.acts);
        msg.node_events = tick.events;
        msg.look = nodes.looks(movers);
        msg.aim_at = self.aim_at.clone();
        msg.weapons = self.armory.in_flight(self.prev_t, msg.t);
        msg.impacts = self.armory.impacts(msg.t);
        msg.impacts.extend(tick.impacts);
        // rules 27: ours struck by a mine report it as any strike; the rest is the record
        msg.impacts.extend(mined.impacts);
        msg.mine_events = mined.events;
        msg.mines_live = self.fields.live_domains(msg.t);
        // the damage model on each strike whose weapon lists figures (a node's catalog weapon, or
        // a scripted act's author's)
        let armory = &mut self.armory;
        let struck = weapon::strike_by(&mut msg.impacts, movers, &self.ours, ctx, &mut self.ledger, |i| {
            armory.take(&i.weapon).or_else(|| weapon::scripted(scenario, i))
        });
        self.prev_s = s;
        self.prev_t = msg.t;
        Second { msg, mined: mined.ours_damaged, struck }
    }

    /// The bridge's answer to the message of second `s`: the mines laid since (held), where ours
    /// are (for the mines, the weapons' aim and the nodes' looks), and the effects to apply (each
    /// now, unless a munition is still in flight to it: rules 20). `say`: what the contacts
    /// process prints of it.
    pub fn answer(
        &mut self,
        answer: &Value,
        s: f64,
        movers: &mut [Mover],
        land: &mut dyn FnMut(&EffectNotice, &mut [Mover], f64) -> EffectApplied,
        say: &mut dyn FnMut(String),
    ) {
        let now = self.t0 + s;
        // rules 27: the mines laid since
        if let Some(list) = answer.get("mines").and_then(Value::as_array) {
            for m in list {
                match serde_json::from_value::<MineNotice>(m.clone()) {
                    Ok(m) => {
                        say(format!("mine {} laid ({}), arms in {:.0} s", m.id(), m.weapon.name, m.arms_t - now));
                        self.fields.lay(m);
                    }
                    Err(err) => say(format!("mine: unreadable ({err})")),
                }
            }
        }
        // opponent weapons: where the vehicles they may be aimed at are now (and, for the nodes'
        // looks, those within reach of one)
        if let Some(list) = answer.get("ours").and_then(Value::as_array) {
            self.ours_now.clear();
            self.ours_now_t = now;
            for o in list {
                if let Ok(o) = serde_json::from_value::<OursAt>(o.clone()) {
                    self.ours.insert(o.uid, o.clone());
                    self.ours_now.push(o);
                }
            }
        }
        // the simulation channel back: engagement effects to apply (Phase E)
        if let Some(list) = answer.get("effects").and_then(Value::as_array) {
            for e in list {
                match serde_json::from_value::<EffectNotice>(e.clone()) {
                    Ok(e) => {
                        if let Some(at) = e.impact_t.filter(|at| *at > now) {
                            say(format!("effect (intent {}): munition in flight, strikes in {:.1} s", e.intent, at - now));
                        }
                        self.in_flight.hold(e);
                    }
                    Err(err) => say(format!("effect: unreadable ({err})")),
                }
            }
            // at once, unless a munition is still in flight to it (rules 20)
            for e in self.in_flight.due(now) {
                let a = land(&e, movers, s);
                self.applied.push(a);
            }
        }
    }
}
