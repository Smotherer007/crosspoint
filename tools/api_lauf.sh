#!/usr/bin/env bash
# Nimmt die API-Antworten einer Engine-Binary auf und vergleicht zwei Laeufe.
#
#   tools/api_lauf.sh <binary> <zielordner> [port]
#
# Startet die Engine mit eigenem, leerem Konfigurationsverzeichnis (gleicher
# Pfad fuer alle Laeufe, damit Pfade in den Antworten identisch sind), wartet
# auf den Port, ruft tools/api_capture.py auf und beendet die Engine wieder.
# Ohne JACK-Verdrahtung, ohne Wiederherstellung, mit Dauerbetrieb - die Engine
# soll nur die API beantworten.
set -euo pipefail

BIN=${1:?Binary fehlt}
ZIEL=${2:?Zielordner fehlt}
PORT=${3:-18770}
CFG=/tmp/crosspoint-api-lauf

mkdir -p "$ZIEL"
rm -rf "$CFG"
mkdir -p "$CFG"

# Alte Engine auf diesem Port beenden: Muster ueber eine Variable, damit die
# eigene Kommandozeile nicht mitgemeint ist (pkill -f sieht sie sonst).
MUSTER="listen 127.0.0.1:$PORT"
for p in $(pgrep -f "$MUSTER" 2>/dev/null || true); do
  [ "$p" = "$$" ] && continue
  kill "$p" 2>/dev/null || true
done
sleep 1

CROSSPOINT_NO_WIRE=1 \
CROSSPOINT_NO_RESTORE=1 \
CROSSPOINT_KEEP_RUNNING=1 \
CROSSPOINT_IDLE_MS=600000 \
XDG_CONFIG_HOME="$CFG" \
  "$BIN" --client crosspoint-api --listen "127.0.0.1:$PORT" \
  > "$ZIEL/engine.log" 2>&1 &
PID=$!

for _ in $(seq 1 100); do
  if curl -fsS -m 1 "http://127.0.0.1:$PORT/api/state" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

python3 "$(dirname "$0")/api_capture.py" "http://127.0.0.1:$PORT" "$ZIEL"
kill "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true
