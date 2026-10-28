#!/usr/bin/env bash
# vm8-launch.sh – Engine starten (falls nötig), verdrahten und als eigenes Fenster öffnen.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENT="${VM8_CLIENT:-vm8}"
URL="http://127.0.0.1:8770/"

alive() { curl -s -o /dev/null --max-time 1 "$URL/api/state"; }

if ! alive; then
  cd "$DIR" || exit 1
  setsid ./target/release/vm8-engine >/tmp/vm8.log 2>&1 &
  for _ in $(seq 1 40); do alive && break; sleep 0.25; done
fi

# Virtuelle Knoten nur anlegen, wenn Streifen 6 noch nicht verbunden ist
if ! pw-link -l 2>/dev/null | grep -q "$CLIENT:strip6_L"; then
  DEFAULT_SINK="$(pactl get-default-sink 2>/dev/null)"
  if [ -n "$DEFAULT_SINK" ]; then
    VM8_A1="$DEFAULT_SINK" "$DIR/scripts/vm8-wire.sh" up >/tmp/vm8-wire.log 2>&1
  else
    "$DIR/scripts/vm8-wire.sh" up >/tmp/vm8-wire.log 2>&1
  fi
fi

if ! alive; then
  echo "Engine antwortet nicht – siehe /tmp/vm8.log" >&2
  exit 1
fi

# eigenes Fenster (App-Modus), sonst normaler Browser
flatpak run com.brave.Browser --app="$URL" --window-size=1720,900 >/dev/null 2>&1 &
sleep 1
exit 0
