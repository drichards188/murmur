.PHONY: doctor setup build start up goldengate situation down sim bridge web eval headless check

FLEET ?= sandiego   # any sim/fleets/*.toml: sandiego, canaveral, golden-gate, ...
CARGO ?= $(shell command -v cargo || echo $(HOME)/.cargo/bin/cargo)

doctor:  ## is this machine ready? prerequisites, setup state, ports, optional AI CLIs (OK / MISSING / WARN; exit 1 if anything is missing)
	@./scripts/doctor.sh

setup:   ## one-time: venv, Rust release build, ArduPilot SITL build, npm install
	./scripts/setup.sh

build:   ## release builds of the Rust bridge, stand-in engine, link emulator, contacts, murmur-replay and murmur-auth (up.sh does this too)
	$(CARGO) build --release -p murmur-bridge -p murmur-standins -p murmur-linkemu -p murmur-decide -p murmur-contacts -p murmur-auth

start:   ## the main menu (eval/launcher.py): open http://localhost:5173, pick the open world (what make goldengate runs) or a situation and start it there; Ctrl-C ends the running one and stops the launcher
	.venv/bin/python -m eval.launcher

up:      ## swarm + link emulator + bridge + web (MURMUR_LINKEMU=0 bypasses the emulator; ports: scripts/up.sh)
	./scripts/up.sh --fleet $(FLEET) $(ARGS)

goldengate: ## Golden Gate with real tide/current/wind and traffic: tidewater serve + drive (AT= start time, EVENTS= scripted events file (comms and GNSS), CONTACTS= contacts scenario, "" for none)
	MURMUR_SCENARIO=$(or $(TIDEWATER_HOME),$(CURDIR)/tidewater)/scenarios/$(if $(filter 0,$(WEATHER)),golden-gate-2026-06.toml,golden-gate-weather.toml) MURMUR_AT=$(or $(AT),2026-06-14T10:00:00Z) \
	  MURMUR_WEATHER_ARCHIVE=$(if $(filter 0,$(WEATHER)),,$(or $(TIDEWATER_HOME),$(CURDIR)/tidewater)/data/cache/golden-gate-2026-06/weather.json) \
	  MURMUR_CONTACTS=$(if $(filter undefined,$(origin CONTACTS)),sim/scenarios/golden-gate.toml,$(CONTACTS)) \
	  $(if $(EVENTS),MURMUR_LINK_EVENTS=$(EVENTS)) ./scripts/up.sh --fleet $(or $(GG_FLEET),full-spectrum) $(ARGS)

# a situation's settings come from make's command line only: a shell may export NAME or AI for
# something else
sit_arg = $(if $(filter command line,$(origin $(1))),$($(1)))
situation: ## a pre-built situation (sim/situations/README.md): NAME=harbor-watch, OPERATOR=live|autoplay, AI=claude|codex|playbook|none, COSIGNER=you|scripted|scripted-pair, REVEAL=0|1; the stack stays up after it: make down
	.venv/bin/python -m eval.situation_run $(or $(call sit_arg,NAME),harbor-watch) $(if $(call sit_arg,OPERATOR),--operator $(call sit_arg,OPERATOR)) $(if $(call sit_arg,AI),--ai $(call sit_arg,AI)) $(if $(call sit_arg,COSIGNER),--cosigner $(call sit_arg,COSIGNER)) $(if $(call sit_arg,REVEAL),--reveal $(call sit_arg,REVEAL))

down:    ## stop a stack started with make up (e.g. from another terminal)
	@# TERM, not INT: a shell started in the background ignores SIGINT and can't trap it
	@if [ -f logs/up.pid ]; then kill -TERM $$(cat logs/up.pid) && echo stopping; else echo "not running"; fi

sim:     ## the fleet alone: SITL vehicles + Rust stand-ins (FLEET=, ARGS= for swarm.py)
	.venv/bin/python sim/swarm.py --fleet $(FLEET) $(ARGS)

bridge:  ## MAVLink <-> WebSocket bridge on :8765 (crates/murmur-bridge)
	$(CARGO) run --release -p murmur-bridge

web:     ## console on http://127.0.0.1:5173
	cd web && npx vite --host 127.0.0.1

eval: build ## evaluation batch on isolated stacks (eval/README.md): SCENARIOS=, ARMS=, SEEDS=, PARALLEL=
	.venv/bin/python -m eval.run batch --scenarios $(or $(SCENARIOS),misclassified-neutral spoofed-gnss link-cut approval-flood stale-track plausible-wrong heldout-friend-alongside heldout-early-unknown heldout-dark-ferry) \
	  --arms $(or $(ARMS),rules-only rules-only-reject human-proxy agent+human-proxy) --seeds $(or $(SEEDS),1 2 3) $(if $(PARALLEL),--parallel $(PARALLEL))

headless: ## headless simulator batch (docs/HEADLESS.md): SITUATION=harbor-watch and/or FAMILIES="fast-unknown decoy", SEEDS=1-10, WORKERS=, AGENT="model playbook none" (model: the Anthropic API, MURMUR_EVAL_MODEL=1)
	$(CARGO) build --release -p murmur-headless
	.venv/bin/python -m eval.headless.batch --no-build $(if $(FAMILIES),--families $(FAMILIES)) $(if $(SITUATION),--situation $(SITUATION)) --seeds $(or $(SEEDS),1-10) $(if $(WORKERS),--workers $(WORKERS)) $(if $(AGENT),--agent $(AGENT))

check:   ## typecheck + lint + glide, sea, coverage, weather, sensorReach, munition, weapon, hostileAct, bluf, engageSteps and tutorial checks (node 22.18+: it runs the .ts directly) + Rust lint and tests + evaluation and watch officer unit tests (stand-in CLIs, no model) + tidewater's tests
	cd web && npx tsc -b && npx oxlint src && node src/glide.check.ts && node src/sea.check.ts && node src/coverage.check.ts && node src/weather.check.ts && node src/sensorReach.check.ts && node src/munition.check.ts && node src/weapon.check.ts && node src/hostileAct.check.ts && node src/bluf.check.ts && node src/engageSteps.check.ts && node src/tutorial.check.ts
	.venv/bin/python -m py_compile sim/swarm.py sim/tests/*.py scripts/*.py agents/*.py agents/watch/*.py agents/playbook/*.py agents/playbook/plays/*.py agents/tests/*.py eval/*.py eval/tests/*.py eval/headless/*.py eval/headless/v2/*.py eval/headless/v3/*.py eval/headless/tests/*.py
	.venv/bin/python -m pytest -q eval/tests
	.venv/bin/python -m pytest -q agents/tests
	.venv/bin/python -m pytest -q tidewater/tests
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	$(CARGO) test --workspace
	$(CARGO) build --release -p murmur-headless -p murmur-decide
	.venv/bin/python -m pytest -q eval/headless/tests
