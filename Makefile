.PHONY: setup build up goldengate down sim bridge web eval check

FLEET ?= sandiego   # any sim/fleets/*.toml: sandiego, canaveral, golden-gate, ...
CARGO ?= $(shell command -v cargo || echo $(HOME)/.cargo/bin/cargo)

setup:   ## one-time: venv, Rust release build, ArduPilot SITL build, npm install
	./scripts/setup.sh

build:   ## release builds of the Rust bridge, stand-in engine, link emulator, contacts, murmur-replay and murmur-auth (up.sh does this too)
	$(CARGO) build --release -p murmur-bridge -p murmur-standins -p murmur-linkemu -p murmur-decide -p murmur-contacts -p murmur-auth

up:      ## swarm + link emulator + bridge + web (MURMUR_LINKEMU=0 bypasses the emulator; ports: scripts/up.sh)
	./scripts/up.sh --fleet $(FLEET) $(ARGS)

goldengate: ## Golden Gate with real tide/current/wind and traffic: tidewater serve + drive (AT= start time, EVENTS= scripted events file (comms and GNSS), CONTACTS= contacts scenario, "" for none)
	MURMUR_SCENARIO=$(HOME)/tidewater/scenarios/golden-gate-2026-06.toml MURMUR_AT=$(or $(AT),2026-06-14T10:00:00Z) \
	  MURMUR_CONTACTS=$(if $(filter undefined,$(origin CONTACTS)),sim/scenarios/golden-gate.toml,$(CONTACTS)) \
	  $(if $(EVENTS),MURMUR_LINK_EVENTS=$(EVENTS)) ./scripts/up.sh --fleet $(or $(GG_FLEET),full-spectrum) $(ARGS)

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

check:   ## typecheck + lint + glide checks (node 22.18+: it runs the .ts directly) + Rust lint and tests + evaluation unit tests
	cd web && npx tsc -b && npx oxlint src && node src/glide.check.ts
	.venv/bin/python -m py_compile sim/swarm.py sim/tests/*.py scripts/*.py agents/*.py eval/*.py eval/tests/*.py
	.venv/bin/python -m pytest -q eval/tests
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	$(CARGO) test --workspace
