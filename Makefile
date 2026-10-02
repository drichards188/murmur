.PHONY: setup build up goldengate down sim bridge web check

FLEET ?= sandiego   # any sim/fleets/*.toml: sandiego, canaveral, golden-gate, ...
CARGO ?= $(shell command -v cargo || echo $(HOME)/.cargo/bin/cargo)

setup:   ## one-time: venv, Rust release build, ArduPilot SITL build, npm install
	./scripts/setup.sh

build:   ## release builds of the Rust bridge, stand-in engine, link emulator and murmur-replay (up.sh does this too)
	$(CARGO) build --release -p murmur-bridge -p murmur-standins -p murmur-linkemu -p murmur-decide

up:      ## swarm + link emulator + bridge + web (MURMUR_LINKEMU=0 bypasses the emulator; ports: scripts/up.sh)
	./scripts/up.sh --fleet $(FLEET) $(ARGS)

goldengate: ## Golden Gate with real tide/current/wind and traffic: tidewater serve + drive (AT= start time, EVENTS= scripted events file (comms and GNSS), CONTACTS= contacts scenario, "" for none)
	MURMUR_SCENARIO=$(HOME)/tidewater/scenarios/golden-gate-2026-06.toml MURMUR_AT=$(or $(AT),2026-06-14T10:00Z) \
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

check:   ## typecheck + lint + Rust lint and tests
	cd web && npx tsc -b && npx oxlint src
	.venv/bin/python -m py_compile sim/swarm.py sim/tests/*.py scripts/*.py
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	$(CARGO) test --workspace
