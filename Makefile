.PHONY: setup up sim bridge web check

setup:   ## one-time: venv, ArduPilot SITL build, npm install
	./scripts/setup.sh

up:      ## swarm + bridge + web, all at once
	./scripts/up.sh

sim:     ## 6 planes + 4 boats (pass ARGS="--planes 3 --boats 2" to change)
	.venv/bin/python sim/swarm.py $(ARGS)

bridge:  ## MAVLink <-> WebSocket bridge on :8765
	cd bridge && ../.venv/bin/python -m murmur_bridge

web:     ## console on http://127.0.0.1:5173
	cd web && npx vite --host 127.0.0.1

check:   ## typecheck + lint
	cd web && npx tsc -b && npx oxlint src
	.venv/bin/python -m py_compile bridge/murmur_bridge/*.py sim/swarm.py
