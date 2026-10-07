# murmur-contacts

Scenario contacts that are not ours (Phase C2): ferries on their routes, cargo ships in the
shipping lane, fishing boats drifting on the current, a sightseeing aircraft, an unknown UAS,
a submarine, a suspect fast boat with a spoofed AIS identity, a coast guard cutter. This
binary is the **truth**: it moves them and publishes where they really are and what they
really are, on the bridge's simulation channel. The C2 (bridge tracks, console, agents)
only ever sees what our own platforms' sensors make of them.

```
make goldengate CONTACTS=sim/scenarios/golden-gate.toml                  # with the stack
MURMUR_CONTACTS=sim/scenarios/golden-gate.toml scripts/up.sh --fleet ...  # up.sh reads the variable
target/release/murmur-contacts sim/scenarios/golden-gate.toml           # alone
```

| | |
|---|---|
| Scenario format | `sim/scenarios/*.toml`, documented in `src/scenario.rs`: affiliation, domain, kind, AIS identity (spoofable), motion (`route` looping or back and forth with dwell, `transit`, `loiter`, `station`, `drift` with the tidewater current), lifetime, signature and emissions per kind. |
| Truth channel | `POST /api/sim/contacts` on the bridge, once a second: `src/truth.rs` (the bridge parses the same shape in `murmur-bridge/src/tracks/truth.rs`). The bridge never serves it back except on `GET /api/sim/truth` with `MURMUR_EVAL_TRUTH=1`. |
| Evaluation log | `logs/contacts-truth.jsonl` (`MURMUR_CONTACTS_TRUTH`): the same message, one line per tick, for Phase G to score tracks against. |
| Engagement effects (Phase E) | The bridge's answer to a POST may carry `effects`: an abstract effect (a point, a radius, a time, the contact the target track was associated with, our vehicles' reported positions). `src/effect.rs` flags every contact inside the radius with the time; `on_effect = "out_of_play"` (default) takes it out of the scenario, `"persists"` keeps it moving with `effect_t` set. The next message's `effects` (truth log only) records what was inside, whether the intended contact was, and collateral (other contacts, our vehicles other than the engaging platform, which is named apart; one of ours with no reported position is listed as unknown). No damage, blast or lethality model. |
| Clock | `MURMUR_AT` at `MURMUR_RATE`, as the stand-ins (a rate other than 1 without `MURMUR_AT` is refused at start); `MURMUR_TIDEWATER_URL` gives drifters the real current. |
| Determinism | Motion jitter from a seeded generator per contact (`seed` in the scenario, `MURMUR_CONTACTS_SEED` overrides); the bridge's sensing draws are seeded from the same value. |

Why a separate binary rather than a stand-in behavior: stand-ins are our vehicles and speak
their real protocol to the bridge (docs/AUTHENTICITY.md); contacts have no protocol to the
C2 at all, and keeping truth out of the process that talks MAVLink keeps the stand-in
contract honest. The engine's conditions client and geo helpers are reused as a library.

What the sensing model does with this truth (detection against range and horizon, dwell,
independent sensors, AIS identity, misclassification) is documented in
`crates/murmur-bridge/src/tracks/sensing.rs`.
