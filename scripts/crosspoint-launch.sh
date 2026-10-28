#!/usr/bin/env bash
# crosspoint-launch.sh – Engine starten und die Oberflaeche als eigenes Fenster oeffnen.
#
# Fenstererkennung ohne KWin: Brave laeuft fuer den Mixer mit eigenem Profil und
# eigenem Debug-Port (Vorgabe 9223). Damit ist eindeutig feststellbar, ob das
# Fenster offen ist - und die Engine wird beendet, sobald es verschwindet.
#
#   CROSSPOINT_DEBUG_PORT   Port der Fenstererkennung (Vorgabe 9223)
#   CROSSPOINT_START_GRACE  Sekunden, die das Fenster zum Erscheinen hat (Vorgabe 25)
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
URL="http://127.0.0.1:8770/"
PORT="${CROSSPOINT_DEBUG_PORT:-9223}"
GRACE="${CROSSPOINT_START_GRACE:-25}"
PROFIL="${XDG_CONFIG_HOME:-$HOME/.config}/crosspoint-browser"

alive()   { curl -s -o /dev/null --max-time 1 "$URL/api/state"; }
fenster() { curl -s --max-time 1 "http://127.0.0.1:$PORT/json/list" 2>/dev/null | grep -c 8770; }

beende_engine() {
  pkill -x crosspoint-engine 2>/dev/null || true
  for _ in $(seq 1 30); do [ "$(ps -eo comm | grep -c '^crosspoint-engine')" = 0 ] && break; sleep 0.2; done
  echo "Engine beendet, virtuelle Geraete entfernt" >&2
}

if ! alive; then
  cd "$DIR" || exit 1
  setsid ./target/release/crosspoint-engine >crosspoint-engine.log 2>&1 &
  for _ in $(seq 1 60); do alive && break; sleep 0.25; done
fi

if ! alive; then
  echo "Engine antwortet nicht – siehe crosspoint-engine.log" >&2
  exit 1
fi

if [ "$(fenster)" -gt 0 ]; then
  echo "Fenster ist bereits offen" >&2
else
  flatpak run com.brave.Browser \
    --user-data-dir="$PROFIL" \
    --remote-debugging-port="$PORT" \
    --no-first-run --no-default-browser-check \
    --app="$URL" --window-size=1760,980 >/dev/null 2>&1 &
  for _ in $(seq 1 $((GRACE * 4))); do [ "$(fenster)" -gt 0 ] && break; sleep 0.25; done
fi

if [ "$(fenster)" -eq 0 ]; then
  echo "Kein Fenster zustande gekommen – raeume auf" >&2
  beende_engine
  exit 1
fi

# Fenster zu heisst Programm zu: zwei vergebliche Proben gelten als geschlossen.
weg=0
while :; do
  sleep 2
  if [ "$(fenster)" -eq 0 ]; then
    weg=$((weg + 1))
    [ "$weg" -ge 2 ] && break
  else
    weg=0
  fi
done

echo "Fenster geschlossen" >&2
beende_engine
