//! Satellite tasking in the bridge: `collect` and `cover` intents from `POST /api/intents`
//! (and the console), and the simulated execution of their orders.
//!
//! There is no satellite operator API behind this (docs/DECISIONS.md, "Space as taskable
//! infrastructure"): an approved collect becomes an abstract tasking with two events on the
//! scenario clock, and a cover becomes a watch over its window. Everything is reported to
//! the engine as op states under the order's resource (`norad:<id>`, `satcom:<class>`,
//! `gnss:<class>`), so it is in the audit log, and the intent is finished at the end.
//!
//! **Collect** ([`SpaceRun::collect`]): `tasked` at dispatch; `collected` when the scenario
//! clock reaches `collect_time`; `product_ready` at `product_time` (then `finish(Done)`), or
//! done at collection when the class's product timing is unknown. `missed` (then
//! `finish(Failed)`) when the pass is lost: the collect time had already passed at dispatch,
//! or the bridge first saw the scenario clock past it more than [`MISS_AFTER_MS`] of engine
//! time late (the clock jumped, or the bridge stalled through the pass).
//!
//! **Cover** ([`SpaceRun::cover`]): `watching` at dispatch, then `outage` and `restored` at
//! each outage the answer predicted (SATCOM, at the area's centroid), `fewest_in_view` at
//! the GNSS sample with the fewest satellites, and `ended` at the window end (then
//! `finish(Done)`). Outages are predictions from the logged answer, not measurements.
//!
//! **Time.** Order times are scenario time. The bridge's scenario clock is a
//! [`ScenarioMap`] from the engine clock (`MURMUR_AT` at bridge start, `MURMUR_RATE`
//! scenario seconds per second), so a fast scenario runs a collect through in minutes and
//! the engine's deadlines (the collect's tasking-lead approval deadline) follow the same map.

use murmur_decide::space::{
    CoverService, GnssSummary, LinkSummary, SpaceResult, fmt_time, from_utc,
};
use murmur_decide::{
    ChosenCollect, CollectCapability, CollectOrder, CoverOrder, Intent, IntentId, LonLat, Proposal,
    Rationale, ScenarioMap, SpaceAnswer, SpaceArea, Task, TaskOutcome, Time, Window,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::DecisionError;

/// A collect first seen past its collect time by more than this much engine time is missed.
pub const MISS_AFTER_MS: i64 = 30_000;
/// Finished runs kept for the console (products list, command log).
pub const KEEP_FINISHED: usize = 50;

// ---- requests ---------------------------------------------------------------------------

/// A time in a request: integer milliseconds (scenario time) or RFC 3339.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TimeIn {
    Ms(i64),
    Text(String),
}

impl TimeIn {
    fn time(&self, what: &str) -> Result<Time, DecisionError> {
        match self {
            TimeIn::Ms(ms) => Ok(Time(*ms)),
            TimeIn::Text(s) => crate::space::parse_time(s)
                .map(|t| Time(t.timestamp_millis()))
                .map_err(|e| DecisionError::new(400, format!("{what}: {e}"))),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WindowIn {
    pub from: TimeIn,
    pub to: TimeIn,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChosenIn {
    pub norad_id: u64,
    pub collect_time: TimeIn,
}

/// An area: `{"kind": "polygon", "ring": [[lon, lat], ...]}`, `{"kind": "circle", "center":
/// [lon, lat], "radius_km": r}`, or `{"bbox": [w, s, e, n]}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AreaIn {
    Bbox { bbox: [f64; 4] },
    Area(SpaceArea),
}

impl AreaIn {
    fn area(&self) -> SpaceArea {
        match self {
            AreaIn::Area(a) => a.clone(),
            AreaIn::Bbox { bbox: [w, s, e, n] } => SpaceArea::Polygon {
                ring: vec![
                    LonLat::new(*w, *s),
                    LonLat::new(*e, *s),
                    LonLat::new(*e, *n),
                    LonLat::new(*w, *n),
                ],
            },
        }
    }
}

/// The collect and cover fields of `POST /api/intents` (flattened beside the vehicle task
/// fields). Times are scenario time.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SpaceParams {
    #[serde(default)]
    pub area: Option<AreaIn>,
    #[serde(default)]
    pub window: Option<WindowIn>,
    /// collect: optical | sar | any (default any)
    #[serde(default)]
    pub capability: Option<CollectCapability>,
    #[serde(default)]
    pub min_resolution_m: Option<f64>,
    #[serde(default)]
    pub product_deadline: Option<TimeIn>,
    /// collect: the option (from `POST /api/space/task`) to commit to; without it the
    /// intent is a query and ends rejected with the options.
    #[serde(default)]
    pub chosen: Option<ChosenIn>,
    /// cover: `{"type": "satcom", "class": "starlink"}` or `{"type": "gnss"}`
    #[serde(default)]
    pub service: Option<CoverService>,
    #[serde(default)]
    pub min_level: Option<f64>,
}

/// The engine proposal for a collect or cover request.
pub fn proposal(
    task: &str,
    uids_given: bool,
    p: &SpaceParams,
    rationale: Option<&str>,
) -> Result<Proposal, DecisionError> {
    let bad = |m: &str| DecisionError::new(400, m.to_string());
    if uids_given {
        return Err(bad(
            "collect and cover request satellites, not fleet vehicles: send no uids",
        ));
    }
    let area = p
        .area
        .as_ref()
        .ok_or_else(|| bad("needs area (polygon, circle or bbox)"))?
        .area();
    let w = p
        .window
        .as_ref()
        .ok_or_else(|| bad("needs window {from, to} (scenario time)"))?;
    let window = Window {
        from: w.from.time("window.from")?,
        to: w.to.time("window.to")?,
    };
    let task = match task {
        "collect" => Task::Collect {
            area,
            window,
            capability: p.capability.unwrap_or(CollectCapability::Any),
            min_resolution_m: p.min_resolution_m,
            product_deadline: p
                .product_deadline
                .as_ref()
                .map(|t| t.time("product_deadline"))
                .transpose()?,
            chosen: p
                .chosen
                .as_ref()
                .map(|c| {
                    Ok::<_, DecisionError>(ChosenCollect {
                        norad_id: c.norad_id,
                        collect_time: c.collect_time.time("chosen.collect_time")?,
                    })
                })
                .transpose()?,
        },
        "cover" => Task::Cover {
            area,
            window,
            service: p
                .service
                .clone()
                .ok_or_else(|| bad("cover needs service {type: satcom, class} or {type: gnss}"))?,
            min_level: p.min_level,
        },
        other => return Err(bad(&format!("{other} is not a space task"))),
    };
    let mut proposal = Proposal::space(task);
    proposal.rationale = rationale
        .filter(|s| !s.trim().is_empty())
        .map(Rationale::new)
        .transpose()
        .map_err(|e| DecisionError::new(400, e.to_string()))?;
    proposal
        .validate()
        .map_err(|e| DecisionError::new(400, e.to_string()))?;
    Ok(proposal)
}

// ---- execution --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Tasked,
    Collected,
    Ended,
}

#[derive(Debug, Clone)]
struct CollectRun {
    order: CollectOrder,
    stage: Stage,
}

#[derive(Debug, Clone)]
struct Outage {
    start: Time,
    end: Time,
    opened: bool,
    closed: bool,
}

#[derive(Debug, Clone)]
struct CoverRun {
    order: CoverOrder,
    outages: Vec<Outage>,
    /// GNSS: the sample with the fewest satellites, reported when reached.
    fewest: Option<(Time, String)>,
    /// What the answer said, for `watching` and `ended`.
    summary: String,
    ended: bool,
}

#[derive(Debug, Clone)]
enum RunKind {
    Collect(CollectRun),
    Cover(CoverRun),
}

/// One op state the run reported (and the engine logged).
#[derive(Debug, Clone)]
pub struct Report {
    pub state: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct Logged {
    t: Time,
    scenario_t: Time,
    state: &'static str,
    detail: String,
}

/// A dispatched collect or cover, followed by the decision loop.
#[derive(Debug, Clone)]
pub struct SpaceRun {
    pub intent: IntentId,
    kind: RunKind,
    events: Vec<Logged>,
    outcome: Option<TaskOutcome>,
}

impl SpaceRun {
    pub fn collect(order: CollectOrder) -> SpaceRun {
        SpaceRun {
            intent: order.intent,
            kind: RunKind::Collect(CollectRun {
                order,
                stage: Stage::Tasked,
            }),
            events: Vec::new(),
            outcome: None,
        }
    }

    /// A cover watch, with the outages the intent's answer predicted.
    pub fn cover(order: CoverOrder, answer: Option<&SpaceAnswer>) -> SpaceRun {
        let w = order.window;
        let (outages, fewest, summary) = match answer.map(|a| &a.result) {
            Some(SpaceResult::Links(l)) => (outages(l, &w), None, links_summary(l)),
            Some(SpaceResult::Gnss(g)) => (Vec::new(), fewest(g), gnss_summary(g)),
            _ => (Vec::new(), None, "no coverage answer recorded".into()),
        };
        SpaceRun {
            intent: order.intent,
            kind: RunKind::Cover(CoverRun {
                order,
                outages,
                fewest,
                summary,
                ended: false,
            }),
            events: Vec::new(),
            outcome: None,
        }
    }

    pub fn resource(&self) -> &murmur_decide::Uid {
        match &self.kind {
            RunKind::Collect(c) => &c.order.resource,
            RunKind::Cover(c) => &c.order.resource,
        }
    }

    pub fn finished(&self) -> bool {
        self.outcome.is_some()
    }

    /// The outcome to `finish` the intent with, once the run has ended.
    pub fn outcome(&self) -> Option<&TaskOutcome> {
        self.outcome.as_ref()
    }

    /// Advance to engine time `now` (scenario time through `map`): what to report, in order.
    /// The first step (at dispatch) reports `tasked` or `watching`.
    pub fn step(&mut self, now: Time, map: &ScenarioMap) -> Vec<Report> {
        if self.outcome.is_some() {
            return Vec::new();
        }
        let first = self.events.is_empty();
        let scen = map.scenario(now);
        let mut out = Vec::new();
        match &mut self.kind {
            RunKind::Collect(c) => {
                let o = &c.order;
                if first {
                    out.push(Report {
                        state: "tasked",
                        detail: format!(
                            "simulated tasking request accepted: {} ({}, NORAD {}) collect {}, product {}{}",
                            o.satellite,
                            o.class_id,
                            o.norad_id,
                            fmt_time(o.collect_time),
                            o.product_time.map_or("time unknown".into(), fmt_time),
                            res(o.resolution_m),
                        ),
                    });
                }
                if c.stage == Stage::Tasked && scen >= o.collect_time {
                    let due = map.engine(o.collect_time);
                    let late = now.ms_since(due);
                    // the last step was before the pass: lost if this one is far later, or if
                    // the pass was already over at dispatch
                    let lost = if first {
                        scen > o.collect_time
                    } else {
                        late > MISS_AFTER_MS
                    };
                    if lost {
                        let why = if first {
                            format!(
                                "the pass at {} was already over when the order was dispatched (scenario now {})",
                                fmt_time(o.collect_time),
                                fmt_time(scen)
                            )
                        } else {
                            format!(
                                "the pass at {} was lost: the bridge first saw the scenario clock at {}, {:.0} s of bridge time after it (time jumped or the bridge stalled)",
                                fmt_time(o.collect_time),
                                fmt_time(scen),
                                late as f64 / 1000.0
                            )
                        };
                        out.push(Report {
                            state: "missed",
                            detail: why.clone(),
                        });
                        c.stage = Stage::Ended;
                        self.outcome = Some(TaskOutcome::Failed {
                            detail: format!("collection missed: {why}"),
                        });
                    } else {
                        out.push(Report {
                            state: "collected",
                            detail: format!(
                                "{} collected the scene at {} (simulated){}",
                                o.satellite,
                                fmt_time(o.collect_time),
                                res(o.resolution_m)
                            ),
                        });
                        c.stage = Stage::Collected;
                        if o.product_time.is_none() {
                            c.stage = Stage::Ended;
                            self.outcome = Some(TaskOutcome::Done);
                        }
                    }
                }
                if c.stage == Stage::Collected
                    && let Some(p) = o.product_time
                    && scen >= p
                {
                    out.push(Report {
                        state: "product_ready",
                        detail: format!(
                            "product ready at {}: {} scene of {}{} (simulated, no imagery)",
                            fmt_time(p),
                            o.satellite,
                            fmt_time(o.collect_time),
                            res(o.resolution_m)
                        ),
                    });
                    c.stage = Stage::Ended;
                    self.outcome = Some(TaskOutcome::Done);
                }
            }
            RunKind::Cover(c) => {
                let w = c.order.window;
                let what = service_label(&c.order.service);
                if first {
                    let starts = if scen < w.from {
                        format!(", starts {}", fmt_time(w.from))
                    } else {
                        String::new()
                    };
                    out.push(Report {
                        state: "watching",
                        detail: format!(
                            "{what} over the area until {}{starts}: {}",
                            fmt_time(w.to),
                            c.summary
                        ),
                    });
                }
                for g in &mut c.outages {
                    if !g.opened && scen >= g.start {
                        g.opened = true;
                        out.push(Report {
                            state: "outage",
                            detail: format!(
                                "no {what} satellite above the mask from {} to {} ({:.0} s, predicted)",
                                fmt_time(g.start),
                                fmt_time(g.end),
                                g.end.ms_since(g.start) as f64 / 1000.0
                            ),
                        });
                    }
                    if g.opened && !g.closed && scen >= g.end {
                        g.closed = true;
                        out.push(Report {
                            state: "restored",
                            detail: format!("{what} back at {}", fmt_time(g.end)),
                        });
                    }
                }
                if let Some((t, d)) = &c.fewest
                    && scen >= *t
                {
                    out.push(Report {
                        state: "fewest_in_view",
                        detail: d.clone(),
                    });
                    c.fewest = None;
                }
                if !c.ended && scen >= w.to {
                    c.ended = true;
                    out.push(Report {
                        state: "ended",
                        detail: format!(
                            "coverage window ended at {}: {}",
                            fmt_time(w.to),
                            c.summary
                        ),
                    });
                    self.outcome = Some(TaskOutcome::Done);
                }
            }
        }
        for r in &out {
            self.events.push(Logged {
                t: now,
                scenario_t: scen,
                state: r.state,
                detail: r.detail.clone(),
            });
        }
        out
    }

    /// For the console: what was ordered, every reported state, and the outcome.
    pub fn json(&self) -> Value {
        let events: Vec<Value> = self
            .events
            .iter()
            .map(|e| json!({"t": e.t, "scenario_t": e.scenario_t, "state": e.state, "detail": e.detail}))
            .collect();
        let (outcome, outcome_detail) = match &self.outcome {
            None => (Value::Null, Value::Null),
            Some(TaskOutcome::Done) => (json!("done"), Value::Null),
            Some(TaskOutcome::Failed { detail }) => (json!("failed"), json!(detail)),
        };
        let mut v = json!({
            "intent_id": self.intent.0,
            "resource": self.resource(),
            "state": self.events.last().map(|e| e.state),
            "outcome": outcome,
            "outcome_detail": outcome_detail,
            "events": events,
        });
        match &self.kind {
            RunKind::Collect(c) => {
                let o = &c.order;
                v["kind"] = json!("collect");
                v["collect"] = json!({
                    "satellite": o.satellite,
                    "norad_id": o.norad_id,
                    "class_id": o.class_id,
                    "capability": o.capability,
                    "collect_time": o.collect_time,
                    "product_time": o.product_time,
                    "resolution_m": o.resolution_m,
                    "scene": o.scene,
                    "area": ring(&o.area),
                });
            }
            RunKind::Cover(c) => {
                let o = &c.order;
                v["kind"] = json!("cover");
                v["cover"] = json!({
                    "service": o.service,
                    "window": o.window,
                    "area": ring(&o.area),
                    "outages": c.outages.iter().map(|g| [g.start, g.end]).collect::<Vec<_>>(),
                    "summary": c.summary,
                });
            }
        }
        v
    }
}

fn res(m: Option<f64>) -> String {
    m.map_or(String::new(), |m| format!(", {m:.2} m"))
}

fn ring(a: &SpaceArea) -> Vec<[f64; 2]> {
    a.to_space().ring()
}

fn service_label(s: &CoverService) -> String {
    match s {
        CoverService::Satcom { class } => format!("{class} SATCOM"),
        CoverService::Gnss { class } => format!("{} GNSS", class.to_uppercase()),
    }
}

/// Predicted outages inside the window, scenario time.
fn outages(l: &LinkSummary, w: &Window) -> Vec<Outage> {
    l.outages
        .iter()
        .map(|o| (from_utc(o.start).max(w.from), from_utc(o.end).min(w.to)))
        .filter(|(s, e)| e > s)
        .map(|(start, end)| Outage {
            start,
            end,
            opened: false,
            closed: false,
        })
        .collect()
}

fn links_summary(l: &LinkSummary) -> String {
    format!(
        "{:.1}% covered, {} outage{} predicted (longest {:.0} s), {}-{} in view",
        l.coverage_fraction * 100.0,
        l.outages.len(),
        if l.outages.len() == 1 { "" } else { "s" },
        l.longest_outage_s,
        l.visible_min,
        l.visible_max
    )
}

fn gnss_summary(g: &GnssSummary) -> String {
    let pdop = g
        .pdop_max
        .map_or("PDOP unknown".into(), |p| format!("PDOP max {p:.2}"));
    format!(
        "{}-{} satellites in view over {} samples, {pdop}",
        g.visible_min, g.visible_max, g.samples
    )
}

fn fewest(g: &GnssSummary) -> Option<(Time, String)> {
    let t = from_utc(g.visible_min_at?);
    Some((
        t,
        format!(
            "fewest {} satellites in view at {} (predicted; geometry only)",
            g.visible_min,
            fmt_time(t)
        ),
    ))
}

// ---- the intent as the console reads it ---------------------------------------------------

/// A compact space answer for intent JSON (the full answer is in the log as `space.query`
/// and in `GET /api/intents/{id}`): the chosen option as the last check found it, the first
/// ranked options, the exclusion counts; for a cover, its summary.
pub fn compact(i: &Intent, tolerance_s: u32) -> Option<Value> {
    let a = i.space_answer()?;
    let mut v = json!({ "time_map": a.time_map, "segment": a.segment.short() });
    match (&a.result, &i.spec().task) {
        (SpaceResult::Tasking(r), Task::Collect { chosen, .. }) => {
            v["options"] = json!(r.options.iter().take(5).collect::<Vec<_>>());
            v["options_total"] = json!(r.options.len());
            v["exclusions_total"] = json!(r.exclusions.len());
            v["exclusion_summary"] = json!(r.exclusion_summary);
            v["now"] = json!(r.request.now);
            if let Some(c) = chosen
                && let Some(o) = murmur_decide::space::matched_option(r, c, tolerance_s)
            {
                v["chosen"] = json!(o);
                let lead = from_utc(o.earliest_collect).ms_since(from_utc(r.request.now));
                v["tasking_lead_s"] = json!(lead / 1000);
            }
        }
        (SpaceResult::Links(l), _) => v["links"] = json!(l),
        (SpaceResult::Gnss(g), _) => v["gnss"] = json!(g),
        _ => {}
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use murmur_decide::Uid;
    use murmur_space::Capability;

    fn order(collect: Time, product: Option<Time>) -> CollectOrder {
        CollectOrder {
            intent: IntentId(7),
            resource: Uid::new("norad:1"),
            norad_id: 1,
            satellite: "SAT".into(),
            class_id: "iceye".into(),
            capability: Capability::Sar,
            collect_time: collect,
            product_time: product,
            resolution_m: Some(1.0),
            area: SpaceArea::Circle {
                center: LonLat::new(-122.4, 37.8),
                radius_km: 3.0,
            },
            scene: None,
            answer_sha256: String::new(),
        }
    }

    fn states(r: &[Report]) -> Vec<&str> {
        r.iter().map(|r| r.state).collect()
    }

    #[test]
    fn a_collect_runs_through_its_states_on_a_fast_clock() {
        let map = ScenarioMap {
            engine_origin: Time(0),
            scenario_origin: Time(1_000_000_000),
            rate: 100.0,
        };
        let collect = Time(1_000_000_000 + 3_600_000);
        let product = collect.plus_ms(1_800_000);
        let mut run = SpaceRun::collect(order(collect, Some(product)));
        assert_eq!(states(&run.step(Time(0), &map)), ["tasked"]);
        // 36 s of engine time is an hour of scenario time
        assert!(run.step(Time(35_999), &map).is_empty());
        assert_eq!(states(&run.step(Time(36_000), &map)), ["collected"]);
        assert!(run.step(Time(53_999), &map).is_empty());
        assert_eq!(states(&run.step(Time(54_000), &map)), ["product_ready"]);
        assert_eq!(run.outcome(), Some(&TaskOutcome::Done));
        assert!(run.step(Time(60_000), &map).is_empty());
    }

    #[test]
    fn a_jump_past_the_pass_is_a_miss() {
        let map = ScenarioMap::IDENTITY;
        let collect = Time(1_000_000);
        let mut run = SpaceRun::collect(order(collect, Some(collect.plus_ms(10))));
        run.step(Time(0), &map);
        assert_eq!(states(&run.step(Time(5_000_000), &map)), ["missed"]);
        assert!(matches!(run.outcome(), Some(TaskOutcome::Failed { .. })));
        // a late but ordinary tick is not a miss
        let mut run = SpaceRun::collect(order(collect, None));
        run.step(Time(0), &map);
        run.step(Time(990_000), &map);
        assert_eq!(states(&run.step(Time(1_002_000), &map)), ["collected"]);
        assert_eq!(run.outcome(), Some(&TaskOutcome::Done));
        // over at dispatch
        let mut run = SpaceRun::collect(order(collect, None));
        assert_eq!(
            states(&run.step(Time(2_000_000), &map)),
            ["tasked", "missed"]
        );
    }
}
