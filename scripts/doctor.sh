#!/usr/bin/env bash
# make doctor: is this machine ready to set up and run Murmur? Non-interactive, no sudo, a few
# seconds. One line per check, each starting with a stable prefix:
#   OK       fine
#   MISSING  required and absent: the line ends with the command that fixes it
#   WARN     works, but read the note (a port in use, an untested OS, an optional tool)
# Exit status 1 if anything is MISSING, else 0. Sections: the prerequisites (what make setup
# needs), setup's own results (make setup makes them), the ports, the optional AI CLIs.
set -uo pipefail
cd "$(dirname "$0")/.."
ARDUPILOT_HOME="${ARDUPILOT_HOME:-$HOME/ardupilot}"
missing=0
need_setup=0

ok() { printf 'OK       %s\n' "$1"; }
warn() { printf 'WARN     %s\n' "$1"; }
miss() { printf 'MISSING  %s\n' "$1"; missing=$((missing + 1)); }
setup_miss() { miss "$1 (fix: make setup)"; need_setup=$((need_setup + 1)); }
have() { command -v "$1" >/dev/null 2>&1; }
# version a >= b, for dotted numbers
ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]; }

echo "== prerequisites (make setup needs these)"
case "$(uname -s)" in
  Linux)
    if grep -qi microsoft /proc/version 2>/dev/null; then
      if grep -qi 'wsl2\|microsoft-standard' /proc/version; then
        ok "os: WSL2 ($(uname -r))"
      else
        warn "os: looks like WSL1; Murmur is developed on WSL2 (fix: wsl --set-version <distro> 2, in Windows)"
      fi
      case "$PWD" in
        /mnt/*) warn "checkout: $PWD is on the Windows drive (slow, and line endings break scripts); clone it under your Linux home instead" ;;
      esac
    else
      ok "os: Linux ($(uname -r))"
    fi
    ;;
  Darwin) warn "os: macOS is untested (Murmur is developed on Linux and WSL2); expect setup or SITL to need work" ;;
  *) warn "os: $(uname -s) is untested; Murmur runs on Linux or WSL2" ;;
esac

if have git; then ok "git: $(git --version | awk '{print $3}')"; else miss "git: not found (fix: sudo apt install git)"; fi

if have uv; then
  ok "uv: $(uv --version | awk '{print $2}')"
elif [ -x "$HOME/.local/bin/uv" ]; then
  warn "uv: in ~/.local/bin but not on PATH (fix: export PATH=\$HOME/.local/bin:\$PATH, or open a new shell)"
else
  miss "uv: not found (fix: curl -LsSf https://astral.sh/uv/install.sh | sh)"
fi

cargo="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"
rustc="$(command -v rustc || echo "$HOME/.cargo/bin/rustc")"
if [ -x "$cargo" ] && [ -x "$rustc" ]; then
  rv="$("$rustc" --version | awk '{print $2}')"
  where=""
  have cargo || where=" (in ~/.cargo/bin, not on PATH: setup finds it there)"
  if ge "$rv" 1.88; then ok "rust: rustc $rv$where"; else miss "rust: rustc $rv is older than 1.88 (edition 2024) (fix: rustup update stable)"; fi
else
  miss "rust: cargo and rustc not found (fix: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y)"
fi

if have node; then
  nv="$(node --version | tr -d v)"
  if ge "$nv" 22.18; then ok "node: $nv"; else miss "node: $nv is older than 22.18 (fix: nvm install 22, see https://github.com/nvm-sh/nvm)"; fi
else
  miss "node: not found, 22.18 or later needed (fix: install nvm from https://github.com/nvm-sh/nvm, then nvm install 22)"
fi
if have npm; then ok "npm: $(npm --version)"; else miss "npm: not found (it comes with node; fix: nvm install 22)"; fi

# The C and C++ toolchain: the Rust build links with cc, ArduPilot's SITL is C++ built by waf
# (its Python modules go in .venv, so no system Python packages are needed), and the bridge's
# passkey library links OpenSSL through pkg-config.
tool_missing=()
for t in gcc g++ make pkg-config; do have "$t" || tool_missing+=("$t"); done
if [ ${#tool_missing[@]} -eq 0 ]; then
  ok "build tools: gcc $(gcc -dumpversion), g++, make, pkg-config"
else
  miss "build tools: no ${tool_missing[*]} (fix: sudo apt install build-essential pkg-config)"
fi
if have pkg-config && pkg-config --exists openssl; then
  ok "openssl headers: $(pkg-config --modversion openssl)"
else
  miss "openssl headers: not found (fix: sudo apt install libssl-dev pkg-config)"
fi
if have curl; then ok "curl: found"; else miss "curl: not found (the installers above and the readiness checks use it; fix: sudo apt install curl)"; fi

avail_kb="$(df -Pk . | awk 'NR==2 {print $4}')"
avail_gb=$((avail_kb / 1024 / 1024))
if [ ! -d .venv ] && [ "$avail_gb" -lt 10 ]; then
  warn "disk: ${avail_gb} GB free here; a first setup takes about 8 GB (ArduPilot, the Rust build, the Golden Gate data)"
else
  ok "disk: ${avail_gb} GB free"
fi

echo "== setup (make setup builds these once; about 30 minutes, rerunning is safe)"
if [ -x .venv/bin/python ] && [ -x .venv/bin/tidewater ]; then ok "python venv: .venv with tidewater"; else setup_miss "python venv: no .venv with tidewater"; fi

gg=tidewater/data/cache/golden-gate-2026-06
gaps=()
for f in terrain.npz tide.json currents.json sfbofs_currents.npz wind.json; do [ -s "$gg/$f" ] || gaps+=("$f"); done
if [ ${#gaps[@]} -eq 0 ]; then ok "golden gate data: $gg"; else setup_miss "golden gate data: $gg lacks ${gaps[*]}"; fi

bins=(murmur-bridge murmur-standins murmur-linkemu murmur-contacts murmur-auth murmur-replay)
absent=()
for b in "${bins[@]}"; do [ -x "target/release/$b" ] || absent+=("$b"); done
if [ ${#absent[@]} -eq 0 ]; then ok "rust binaries: target/release (${#bins[@]})"; else setup_miss "rust binaries: no ${absent[*]} in target/release"; fi

fw=(arduplane arducopter ardurover ardusub blimp antennatracker)
if [ -d "$ARDUPILOT_HOME" ]; then
  nofw=()
  for f in "${fw[@]}"; do [ -x "$ARDUPILOT_HOME/build/sitl/bin/$f" ] || nofw+=("$f"); done
  if [ ${#nofw[@]} -eq 0 ]; then ok "ardupilot sitl: $ARDUPILOT_HOME (6 firmwares)"; else setup_miss "ardupilot sitl: $ARDUPILOT_HOME has no ${nofw[*]} built"; fi
  if have git && scripts/ardupilot-patches.sh check "$ARDUPILOT_HOME" >/dev/null 2>&1; then
    ok "ardupilot patches: applied"
  else
    warn "ardupilot patches: $ARDUPILOT_HOME lacks Murmur's patches; boats, subs and the tracker need them (fix: scripts/ardupilot-patches.sh apply $ARDUPILOT_HOME, then make setup to rebuild; or move it aside and make setup clones a fresh one)"
  fi
elif [ -d "$ARDUPILOT_HOME.partial" ]; then
  setup_miss "ardupilot sitl: an interrupted clone at $ARDUPILOT_HOME.partial (setup starts it again)"
else
  setup_miss "ardupilot sitl: no $ARDUPILOT_HOME"
fi

if [ -x web/node_modules/.bin/vite ]; then ok "console deps: web/node_modules"; else setup_miss "console deps: no web/node_modules"; fi

echo "== ports (TCP on 127.0.0.1; in use is fine if it is your own Murmur)"
answers() { timeout 1 bash -c "exec 3<>/dev/tcp/127.0.0.1/$1" 2>/dev/null; }
# probe every port at once (a filtered port takes the whole second), then read the answers
probed="$(mktemp -d)"
trap 'rm -rf "$probed"' EXIT
for p in 5173 8760 8761 8765 8766 8770 5673 9265 9266 9270; do
  { answers "$p" && touch "$probed/$p"; } &
done
wait
what_is() {
  # name a Murmur service on the port, if it is one
  have curl || return 0
  case "$1" in
    8760) curl -sf --max-time 1 "http://127.0.0.1:$1/launcher/api/state" >/dev/null && echo " (the launcher, make start)" ;;
    8765|9265) curl -sf --max-time 1 "http://127.0.0.1:$1/api/state" >/dev/null && echo " (a Murmur bridge)" ;;
  esac
}
check_ports() {
  local label="$1" stop="$2"; shift 2
  local busy=()
  for spec in "$@"; do
    local p="${spec%%:*}" name="${spec#*:}"
    [ -e "$probed/$p" ] && busy+=(":$p $name$(what_is "$p")")
  done
  if [ ${#busy[@]} -eq 0 ]; then
    ok "$label: free"
  else
    local joined
    joined="$(printf '%s, ' "${busy[@]}")"
    warn "$label: in use: ${joined%, }; if that is Murmur, $stop"
  fi
}
check_ports "ports, open world and menu (5173 8760 8761 8765 8766 8770)" "make down stops a stack and Ctrl-C in its terminal the menu (or kill \$(cat logs/launcher.pid))" \
  5173:console 8760:launcher 8761:launcher-relay 8765:bridge 8766:link-emulator 8770:tidewater
check_ports "ports, situations (5673 9265 9266 9270)" "make down stops it" \
  5673:console 9265:bridge 9266:link-emulator 9270:tidewater
if [ -f logs/up.pid ] && kill -0 "$(cat logs/up.pid)" 2>/dev/null; then
  warn "stack: one started from this checkout is running (pid $(cat logs/up.pid)); make down stops it"
fi

echo "== optional: AI watch officers (AI=playbook and AI=none need neither)"
if have claude; then
  cv="$(claude --version 2>/dev/null | awk '{print $1}')"
  if ge "$cv" 2.1.294; then ok "claude: $cv (AI=claude; log in once with claude, then /login)"; else warn "claude: $cv is older than 2.1.294, which AI=claude needs (fix: claude update)"; fi
else
  warn "claude: not installed; only AI=claude needs it (https://code.claude.com)"
fi
if have codex; then
  xv="$(codex --version 2>/dev/null | awk '{print $NF}')"
  if ge "$xv" 0.161.0; then ok "codex: $xv (AI=codex; log in once with codex login)"; else warn "codex: $xv is older than 0.161.0, which AI=codex needs (fix: npm install -g @openai/codex)"; fi
else
  warn "codex: not installed; only AI=codex needs it (npm install -g @openai/codex)"
fi

echo
if [ "$missing" -eq 0 ]; then
  echo "doctor: ready. make start, then open http://localhost:5173"
  exit 0
fi
prereq=$((missing - need_setup))
if [ "$prereq" -gt 0 ]; then
  echo "doctor: $missing missing. Fix the $prereq prerequisite(s) first, then run make setup (about 30 minutes)."
else
  echo "doctor: $missing missing, all made by make setup (about 30 minutes; rerunning it is safe)."
fi
exit 1
