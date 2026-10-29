#!/usr/bin/env bash
# crosspoint-start.sh – Engine und eigenes Fenster, gemeinsam gestartet.
#
# Im Flatpak gibt es keinen Browser im Sandbox: die Oberflaeche zeigt
# crosspoint-fenster (WebKitGTK, siehe fenster/fenster.c). Dieses Skript ist
# der Einstiegspunkt des Flatpaks:
#
#   1. Engine starten (falls nicht schon vorhanden),
#   2. warten, bis sie auf HTTP 200 antwortet,
#   3. crosspoint-fenster im Vordergrund starten,
#   4. nach dessen Ende die Engine beenden - sie raeumt dabei ihre virtuellen
#      Geraete ab - und zur Sicherheit noch einmal crosspoint-wire.sh down.
#
# Schaubild:
#   crosspoint-start -> crosspoint-engine (Hintergrund) + crosspoint-fenster
#   Fenster zu       -> Engine beendet, Geraete weg
#
# Auf dem Host (ohne Flatpak) bleibt scripts/crosspoint-launch.sh zustaendig;
# es startet die Oberflaeche im Browser der Wahl.
set -uo pipefail

# --minimized: ohne Fenster starten, nur Tray-Icon (Autostart).
MINIMIERT=""
for argument in "$@"; do
  case "$argument" in
    --minimized|-m) MINIMIERT=1 ;;
    *) echo "crosspoint-start: unbekanntes Argument: $argument" >&2; exit 2 ;;
  esac
done

HIER="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
URL="${CROSSPOINT_URL:-http://127.0.0.1:8770/}"
LOG="${XDG_RUNTIME_DIR:-/tmp}/crosspoint-engine.log"
WARTE_SEKUNDEN="${CROSSPOINT_WARTE_S:-60}"

binaer() {
  # Reihenfolge: ausdrueckliche Vorgabe, /app/bin (Flatpak), neben dem Skript,
  # target/release (Repo).
  local name="$1" var="$2"
  local vorgabe="${!var:-}"
  if [ -n "$vorgabe" ] && [ -x "$vorgabe" ]; then echo "$vorgabe"; return 0; fi
  local kandidat
  for kandidat in "/app/bin/$name" "$HIER/$name" "$HIER/../target/release/$name"; do
    [ -x "$kandidat" ] && { echo "$kandidat"; return 0; }
  done
  command -v "$name" 2>/dev/null
}

ENGINE="$(binaer crosspoint-engine CROSSPOINT_ENGINE)"
FENSTER="$(binaer crosspoint-fenster CROSSPOINT_FENSTER)"
WIRE="$(binaer crosspoint-wire.sh CROSSPOINT_WIRE)"

antwortet() {
  [ "$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "$URL" 2>/dev/null)" = "200" ]
}

# Der Prozessname wird vom Kernel auf 15 Zeichen gekuerzt
# ("crosspoint-engi"), deshalb ueber die Kommandozeile suchen.
ENGINE_MUSTER='(^|/)crosspoint-engine( |$)'
engine_pids() { pgrep -f "$ENGINE_MUSTER" 2>/dev/null; }

eigene_engine=""
FENSTER_PID=""

beende_engine() {
  if [ -z "$eigene_engine" ]; then
    return 0
  fi
  pkill -f "$ENGINE_MUSTER" 2>/dev/null || true
  for _ in $(seq 1 40); do
    [ -z "$(engine_pids)" ] && break
    sleep 0.2
  done
  pkill -9 -f "$ENGINE_MUSTER" 2>/dev/null || true
  # Sicherheitsnetz: die Engine raeumt beim Beenden selbst ab; falls sie hart
  # beendet wurde oder ein Modul liegen blieb, hier nachziehen.
  [ -n "$WIRE" ] && bash "$WIRE" down >/dev/null 2>&1
  echo "Engine beendet, virtuelle Geraete entfernt" >&2
}

aufraeumen() {
  [ -n "$FENSTER_PID" ] && kill "$FENSTER_PID" 2>/dev/null
  beende_engine
}
trap aufraeumen EXIT INT TERM

if [ -z "$ENGINE" ]; then
  echo "crosspoint-start: crosspoint-engine nicht gefunden" >&2
  exit 1
fi
if [ -z "$FENSTER" ]; then
  echo "crosspoint-start: crosspoint-fenster nicht gefunden" >&2
  exit 1
fi

# Engine nur starten, wenn sie nicht schon laeuft: eine fremde Engine wird
# auch nicht wieder beendet.
if antwortet; then
  echo "crosspoint-start: Engine laeuft bereits" >&2
else
  echo "crosspoint-start: starte $ENGINE" >&2
  # Im Tray-Betrieb gibt es keine sichtbare Seite, die die Engine am Leben
  # haelt (sie beendet sich sonst nach ihrer Leerlaufzeit) - also Dauerbetrieb.
  if [ -n "$MINIMIERT" ]; then
    export CROSSPOINT_KEEP_RUNNING=1
  fi
  # setsid: eigenes Prozessgruppen-Handle, damit die Engine das Ende des
  # Startskripts ueberlebt, bis das Fenster geschlossen ist.
  if command -v setsid >/dev/null 2>&1; then
    setsid "$ENGINE" >"$LOG" 2>&1 &
  else
    "$ENGINE" >"$LOG" 2>&1 &
  fi
  eigene_engine=1
fi

# Auf HTTP 200 warten - nicht nur auf den offenen Port, denn die Engine bindet
# den Socket, bevor JACK und die virtuellen Geraete fertig sind.
bereit=""
for _ in $(seq 1 $((WARTE_SEKUNDEN * 4))); do
  if antwortet; then bereit=1; break; fi
  [ -z "$(engine_pids)" ] && [ -n "$eigene_engine" ] && break
  sleep 0.25
done
if [ -z "$bereit" ]; then
  echo "crosspoint-start: Engine antwortet nicht – siehe $LOG" >&2
  exit 1
fi

# Fenster im Vordergrund: es laeuft, bis der Benutzer es schliesst (bzw. im
# Tray-Betrieb bis "Beenden" im Tray-Menue).
FENSTER_ARGUMENTE=()
if [ -n "$MINIMIERT" ]; then
  FENSTER_ARGUMENTE+=(--minimized)
  echo "crosspoint-start: minimiert (nur Tray-Icon)" >&2
fi
"$FENSTER" "${FENSTER_ARGUMENTE[@]}" &
FENSTER_PID=$!
wait "$FENSTER_PID"
FENSTER_STATUS=$?
FENSTER_PID=""

echo "crosspoint-start: Fenster geschlossen" >&2
beende_engine
trap - EXIT INT TERM
exit "$FENSTER_STATUS"
