#!/usr/bin/env bash
# crosspoint-launch.sh – Engine starten und die Oberflaeche als eigenes Fenster oeffnen.
#
# Fenstererkennung ohne KWin: der Browser laeuft fuer den Mixer mit eigenem Profil
# und eigenem Debug-Port (Vorgabe 9223). Damit ist eindeutig feststellbar, ob das
# Fenster offen ist - und die Engine wird beendet, sobald es verschwindet.
#
# Das Skript laeuft aus dem Repo (target/release) wie aus dem Flatpak (/app/bin).
# Im Flatpak gibt es keinen Browser im Sandbox: das Fenster oeffnet der Host,
# gestartet ueber `flatpak-spawn --host` (Finish-Arg --talk-name=org.freedesktop.Flatpak).
#
#   CROSSPOINT_ENGINE       Pfad zur Engine (Vorgabe: neben dem Skript bzw. target/release)
#   CROSSPOINT_BROWSER      Befehl fuer das Fenster (Vorgabe: flatpak run com.brave.Browser)
#   CROSSPOINT_DEBUG_PORT   Port der Fenstererkennung (Vorgabe 9223)
#   CROSSPOINT_START_GRACE  Sekunden, die das Fenster zum Erscheinen hat (Vorgabe 25)
set -uo pipefail
HIER="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIR="$(cd "$HIER/.." && pwd)"
URL="http://127.0.0.1:8770/"
PORT="${CROSSPOINT_DEBUG_PORT:-9223}"
GRACE="${CROSSPOINT_START_GRACE:-25}"
PROFIL="${XDG_CONFIG_HOME:-$HOME/.config}/crosspoint-browser"
LOG="${XDG_RUNTIME_DIR:-/tmp}/crosspoint-engine.log"

engine() {
  if [ -n "${CROSSPOINT_ENGINE:-}" ]; then echo "$CROSSPOINT_ENGINE"; return; fi
  if [ -x "$DIR/target/release/crosspoint-engine" ]; then echo "$DIR/target/release/crosspoint-engine"; return; fi
  if [ -x "$HIER/crosspoint-engine" ]; then echo "$HIER/crosspoint-engine"; return; fi
  echo "$DIR/target/release/crosspoint-engine"
}

# Fensterbefehl: im Flatpak auf dem Host, sonst direkt.
fensterbefehl() {
  local cmd="${CROSSPOINT_BROWSER:-flatpak run com.brave.Browser}"
  if [ -n "${FLATPAK_ID:-}" ] && command -v flatpak-spawn >/dev/null 2>&1; then
    echo "flatpak-spawn --host $cmd"
  else
    echo "$cmd"
  fi
}

alive()   { curl -s -o /dev/null --max-time 1 "$URL/api/state"; }
fenster() { curl -s --max-time 1 "http://127.0.0.1:$PORT/json/list" 2>/dev/null | grep -c 8770; }

# Der Prozessname wird vom Kernel auf 15 Zeichen gekuerzt ("crosspoint-engi"),
# deshalb ueber die Kommandozeile suchen statt ueber comm.
ENGINE_MUSTER='(^|/)crosspoint-engine( |$)'
engine_pids() { pgrep -f "$ENGINE_MUSTER" 2>/dev/null; }

beende_engine() {
  pkill -f "$ENGINE_MUSTER" 2>/dev/null || true
  for _ in $(seq 1 30); do [ -z "$(engine_pids)" ] && break; sleep 0.2; done
  echo "Engine beendet, virtuelle Geraete entfernt" >&2
}

if ! alive; then
  cd "$DIR" || exit 1
  setsid "$(engine)" >"$LOG" 2>&1 &
  for _ in $(seq 1 60); do alive && break; sleep 0.25; done
fi

if ! alive; then
  echo "Engine antwortet nicht – siehe $LOG" >&2
  exit 1
fi

if [ "$(fenster)" -gt 0 ]; then
  echo "Fenster ist bereits offen" >&2
else
  # shellcheck disable=SC2046
  $(fensterbefehl) \
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
