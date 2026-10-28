#!/usr/bin/env bash
# vm8-launch.sh – Engine starten, verdrahten, Fenster oeffnen.
# Beim Schliessen des Fensters werden Engine und virtuelle Geraete wieder
# abgebaut, damit in den Sound-Einstellungen keine VM8-Reste stehen bleiben.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENT="${VM8_CLIENT:-vm8}"
URL="http://127.0.0.1:8770/"
STARTED_ENGINE=0
WIRED=0

alive() { curl -s -o /dev/null --max-time 1 "$URL/api/state"; }

cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  echo "raeume auf ..." >&2
  if [ "$WIRED" = 1 ]; then
    "$DIR/scripts/vm8-wire.sh" down >&2 || true
  fi
  if [ "$STARTED_ENGINE" = 1 ]; then
    pkill -x vm8-engine 2>/dev/null || true
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

if ! alive; then
  cd "$DIR" || exit 1
  setsid ./target/release/vm8-engine >/tmp/vm8.log 2>&1 &
  STARTED_ENGINE=1
  for _ in $(seq 1 40); do alive && break; sleep 0.25; done
fi

if ! alive; then
  echo "Engine antwortet nicht – siehe /tmp/vm8.log" >&2
  exit 1
fi

# Virtuelle Knoten nur anlegen, wenn Streifen 6 noch nicht verbunden ist
# pw-link -l listet auch unverbundene Ports, deshalb auf eine echte
# Verbindung (Pfeil) pruefen - sonst wird nie verdrahtet.
if ! pw-link -l 2>/dev/null | grep -A1 "^$CLIENT:strip6_L" | grep -q '<-'; then
  DEFAULT_SINK="$(pactl get-default-sink 2>/dev/null || true)"
  if [ -n "$DEFAULT_SINK" ]; then
    VM8_A1="$DEFAULT_SINK" "$DIR/scripts/vm8-wire.sh" up >/tmp/vm8-wire.log 2>&1
  else
    "$DIR/scripts/vm8-wire.sh" up >/tmp/vm8-wire.log 2>&1
  fi
  WIRED=1
fi

# eigenes Fenster (App-Modus) im Vordergrund: dieser Prozess lebt so lange wie
# das Fenster, der Trap oben baut danach Engine und Geraete ab.
if command -v flatpak >/dev/null 2>&1; then
  flatpak run com.brave.Browser --app="$URL" --window-size=1720,900 >/dev/null 2>&1
else
  xdg-open "$URL" >/dev/null 2>&1
fi
