#!/usr/bin/env bash
# End-to-End: 1-kHz-Sinus in die Engine spielen und uber die Tape-API aufnehmen.
#
#   tools/e2e_1khz.sh <quell-wav> <api-url> [sekunden]
#
# Der Sinus laeuft als eigener PipeWire-Client (`pw-cat`), wird per `pw-link`
# auf `strip1_L/R` der Engine gelegt, danach wird die Aufnahme per
# POST /api/tape gestartet und gestoppt. Die entstandene Datei wird mit
# tests/wav_check.py ausgewertet (Pegel und Frequenz).
set -uo pipefail

QUELLE=${1:?Quell-WAV fehlt}
API=${2:?API-URL fehlt}
SEKUNDEN=${3:-3}
CLIENT=${CLIENT:-crosspoint-e2e}
TAPE_DIR=${TAPE_DIR:?TAPE_DIR fehlt}

# Alte pw-cat-Prozesse beenden, ohne die eigene Kommandozeile zu treffen.
PATTERN='pw-ca'
for p in $(pgrep -f "${PATTERN}t" 2>/dev/null); do
  [ "$p" = "$$" ] && continue
  [ "$p" = "$PPID" ] && continue
  kill "$p" 2>/dev/null || true
done
sleep 1

rm -f "$TAPE_DIR"/*.wav 2>/dev/null || true

pw-cat --playback --media-type Audio "$QUELLE" > /tmp/pwcat-e2e.log 2>&1 &
CAT=$!
sleep 0.4

pw-link pw-cat:output_FL "$CLIENT:strip1_L"
pw-link pw-cat:output_FR "$CLIENT:strip1_R"
echo "verlinkt: pw-cat -> $CLIENT:strip1_L/R"

curl -sS -X POST --data '{"action":"start"}' "$API/api/tape"; echo
sleep "$SEKUNDEN"
curl -sS -X POST --data '{"action":"stop"}' "$API/api/tape"; echo
sleep 1
curl -sS "$API/api/tape"; echo
kill "$CAT" 2>/dev/null || true
wait "$CAT" 2>/dev/null || true

ls -l "$TAPE_DIR"/*.wav
for f in "$TAPE_DIR"/*.wav; do
  echo "== $f"
  python3 "$(dirname "$0")/../tests/wav_check.py" "$f" --freq
done
