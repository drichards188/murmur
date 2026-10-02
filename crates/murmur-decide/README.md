# murmur-decide

Murmur's decision layer (Phase C in `docs/PLAN.md`): intents, fail-closed rules, risk-tier
policy, an append-only audit log, and deterministic replay. A standalone library with one
small binary (`murmur-replay`). It does not depend on the bridge; the bridge calls it.

## The model

Every action, from a human or an AI agent, is an **intent**: what (orbit, patrol, hold, rtl,
track; engage arrives in Phase E), to which vehicles (by `uid`), with what constraints, why
(required for agents), from whom. It moves through one state machine:

```
proposed -> checked -> auto_approved | pending_approval
         -> approved | rejected | expired | voided -> dispatched -> done | failed
```

The full table is `intent::next_state`; anything else is a typed `IllegalTransition`.

1. **Rules** (`rules.rs`) are pure functions of the intent and a `WorldSnapshot`:
   capability (the KINDS table), readiness (nav ready, fresh), platform limits (orbit radius
   vs minimum turn radius, with a suggested radius instead of silent widening), domain (boats
   and subs on water, UGVs on land, via a `TerrainOracle`), geofence (no-go polygons from
   `sim/areas/*.geojson`), reserves (endurance left after the task). **Fail-closed**: a rule
   that can't evaluate (unknown vehicle, missing or stale fact, no area file, unanswered
   terrain point) fails and says why. The one exception is PLAN.md's: with no terrain oracle
   the domain rule records `not_evaluated`, which blocks only consequential kinds (engage).
2. **Policy** (`sim/policy.toml`, first match wins): hold/rtl auto; any task on an
   `armed = true` platform (switchblade, fpv, ugv_armed) needs a human; leaving the approved
   area needs a human; else auto. No match means human. Unknown platform counts as armed; no
   area file counts as leaving.
3. **Approval**: pending intents wait in the inbox and expire (default 120 s). Approving
   re-runs the checks against the current world; if the picture no longer holds, the intent
   is voided instead.
4. **Audit**: every step is a JSONL event `{seq, t, type, ...}` written before the change it
   records (a failed write means no change). Decision events carry the exact world facts
   they read, each with its observation time, and the policy file is logged at run start.

## The guarantee: agents nominate, only humans authorize

- `Engine::approve` requires a `&HumanAuthority`; it is the only path to `approved`.
- A `HumanAuthority` lives only inside an `OperatorSession`, which only
  `Engine::open_operator_session` creates (and logs). Neither type has public fields,
  `Clone`, `Default`, `Deserialize`, or a conversion from an agent id, so no JSON (an MCP tool
  call, say) can become one.
- Agent code gets an `AgentPort`: propose (a non-empty `Rationale` is a required argument),
  withdraw its own intents, read. No approve method; its engine reference is private.
- An intent's source is set by the path it came in on, never by the request body.
- `compile_fail` doctests (in `authority.rs` and `engine.rs`) show that forging, cloning or
  deserializing an authority, calling approve through an `AgentPort`, reaching its engine, and
  passing an agent id as an authority all fail to compile. Stable rustdoc only checks that
  they fail, not the error code; each was checked by hand to fail with exactly the code noted.
- Replay checks the same property from the log: no `approved` without an `approval.granted`
  by an operator before the deadline and a passing re-check.

**Limits.** Rust privacy is a code-structure guarantee, not a security boundary. It keeps
code that holds only an `AgentPort` (or only JSON) from authorizing, and it leaves one
greppable minting point. It does not stop someone editing this crate, bridge code that holds
`&mut Engine` opening an operator session on an agent's behalf (rule: only the authenticated
operator endpoint calls `open_operator_session`), a compromised process, or a human who
approves without looking. Authenticating operators is the bridge's job. Whether human
approval stays meaningful at scale is what Murmur measures; this crate makes sure there is a
human decision to measure.

## Replay

```
cargo run -p murmur-decide --bin murmur-replay -- logs/audit/<run-id>.jsonl
```

Re-runs every `check.result` and `policy.decision` from its logged inputs and the logged
policy, checks every `intent.state` is legal and backed by its evidence, and checks `seq` and
`t`. Exit 0 if everything reproduces, 1 on mismatch, 2 if the log can't be read.
Determinism: injected clock, integer-millisecond times, `BTreeMap`s, fixed output order and
fixed-precision detail strings, `serde_json` with `float_roundtrip`, terrain samples keyed at
1e-7 degree. Replay proves decisions follow from the logged inputs; it can't prove the inputs
were true of the world.

## Integration contract for the bridge

The full contract is in the crate docs (`src/lib.rs`). In short:

1. At start: `Policy::load("sim/policy.toml")`, `Catalog::load("sim/platforms.toml")`,
   `AreaSet::load_dir("sim/areas")`, `Engine::new(policy, SystemClock, JsonlFile at
   logs/audit/<run-id>.jsonl)`.
2. Per decision call, build a `WorldSnapshot` keyed by `uid`: platform facts from the catalog,
   and timestamped facts from telemetry (autopilot kind, nav ready, position, home, battery).
   Unknown is `None`.
3. Terrain: a `TerrainOracle` over tidewater's `POST /v1/sample` (lat-first points; null
   elevation is outside coverage; `water_depth_m > 0` is water), or `NoTerrain`.
4. Operator endpoints: `submit` (also behind the legacy `POST /api/tasks`), `approve`,
   `reject`, `cancel`, `inbox`. Agent (MCP) side: an `AgentPort` per agent, nothing else.
5. Every second: `expire_due`. For approved intents: `dispatch` gives a `DispatchOrder` for
   the existing task manager; report back with `record_op_state` and `finish`; log vehicle
   events; `void` intents the world invalidates.
6. Any `Err` from the engine means nothing changed; never dispatch on an error.

## Not done yet

- No HTTP endpoints, no tidewater client, no inbox UI (C.5): bridge and web work.
- The geofence and approved-area checks cover the task's own path, not the transit leg from
  the vehicle's position or the RTL path home. Areas are 2D (no altitude bands).
- Reserves assume cruise speed and a fixed time-on-task horizon (`task_duration_s`), because
  orbits and patrols loop until told otherwise; the bridge doesn't end tasks after a duration
  yet, so `duration_s` can only lengthen the estimate. Platforms without an `endurance_h` in
  the catalog (most SITL types) fail the reserves rule for orbit and patrol unless
  `unknown_endurance = "battery_now"`.
- Every decision event repeats the full area set; logging it once with a content hash would
  shrink logs (about 700 bytes per event today).
- The log isn't hash-chained, so a tamper that leaves every decision consistent (for example,
  deleting a whole intent's events and renumbering) is not detected.
- Cross-platform replay: trigonometry goes through the platform libm. Same machine is exact;
  across machines a last-bit difference could in principle flip a boundary case.
- Phase E pieces: engage, two-person approval, approval holding a track snapshot.
