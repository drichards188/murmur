.PHONY: setup up goldengate down sim bridge bridge-rs web check

FLEET ?= sandiego   # any sim/fleets/*.toml: sandiego, canaveral, golden-gate, ...
CARGO ?= $(shell command -v cargo || echo $(HOME)/.cargo/bin/cargo)

setup:   ## one-time: venv, ArduPilot SITL build, npm install
	./scripts/setup.sh

up:      ## swarm + bridge + web, all at once (MURMUR_BRIDGE=rust for the Rust bridge)
	./scripts/up.sh --fleet $(FLEET) $(ARGS)

goldengate: ## Golden Gate with real tide/current/wind via ../tidewater (AT= scenario start time)
	MURMUR_SCENARIO=$(HOME)/tidewater/scenarios/golden-gate-2026-06.toml MURMUR_AT=$(or $(AT),2026-06-14T10:00Z) ./scripts/up.sh --fleet $(or $(GG_FLEET),full-spectrum) $(ARGS)

down:    ## stop a stack started with make up (e.g. from another terminal)
	@if [ -f logs/up.pid ]; then kill -INT $$(cat logs/up.pid) && echo stopping; else echo "not running"; fi

sim:     ## 6 planes + 4 boats (pass ARGS="--planes 3 --boats 2" to change)
	.venv/bin/python sim/swarm.py --fleet $(FLEET) $(ARGS)

bridge:  ## MAVLink <-> WebSocket bridge on :8765
	cd bridge && ../.venv/bin/python -m murmur_bridge

bridge-rs: ## the same bridge, Rust port (crates/murmur-bridge)
	$(CARGO) run --release -p murmur-bridge

web:     ## console on http://127.0.0.1:5173
	cd web && npx vite --host 127.0.0.1

check:   ## typecheck + lint + Rust lint and tests
	cd web && npx tsc -b && npx oxlint src
	.venv/bin/python -m py_compile bridge/murmur_bridge/*.py sim/swarm.py scripts/*.py
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) test
