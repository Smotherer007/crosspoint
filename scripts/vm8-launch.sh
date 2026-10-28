#!/usr/bin/env bash
# vm8-launch.sh – Engine starten und die Oberflaeche als eigenes Fenster oeffnen.
# Die Engine verdrahtet die virtuellen Geraete selbst beim Start und raeumt sie
# beim Beenden wieder ab; dieses Skript kuemmert sich nur um Prozess und Fenster.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
URL="http://127.0.0.1:8770/"
STARTED_ENGINE=0

alive() { curl -s -o /dev/null --max-time 1 "$URL/api/state"; }
fenster() { curl -s --max-time 1 http://127.0.0.1:9222/json/list 2>/dev/null | grep -c 8770; }

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

if command -v flatpak >/dev/null 2>&1; then
  flatpak run com.brave.Browser --app="$URL" --window-size=1720,900 >/dev/null 2>&1 &
else
  xdg-open "$URL" >/dev/null 2>&1 &
fi

# Warten, bis das Fenster da ist, dann auf sein Verschwinden achten.
# `flatpak run` kehrt sofort zurueck, wenn Brave schon laeuft - deshalb wird
# hier die Fensterliste beobachtet statt auf den Kindprozess zu warten.
for _ in $(seq 1 40); do [ "$(fenster)" -gt 0 ] && break; sleep 0.5; done
[ "$(fenster)" -gt 0 ] || { echo "kein Fenster gefunden" >&2; exit 1; }

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

echo "Fenster geschlossen, beende ..." >&2
# Beendet die Engine und damit (ueber deren Aufraeumen) die virtuellen Geraete.
if [ "$STARTED_ENGINE" = 1 ]; then
  pkill -x vm8-engine 2>/dev/null || true
else
  echo "Engine lief schon vorher – bleibt aktiv (Beenden mit: pkill -x vm8-engine)" >&2
fi
