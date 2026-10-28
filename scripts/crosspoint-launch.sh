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
#   CROSSPOINT_START_GRACE  Sekunden, die das Fenster zum Erscheinen hat (Vorgabe 60)
set -uo pipefail
HIER="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIR="$(cd "$HIER/.." && pwd)"
URL="http://127.0.0.1:8770/"
PORT="${CROSSPOINT_DEBUG_PORT:-9223}"
GRACE="${CROSSPOINT_START_GRACE:-60}"
LOG="${XDG_RUNTIME_DIR:-/tmp}/crosspoint-engine.log"
LOG_FENSTER="${XDG_RUNTIME_DIR:-/tmp}/crosspoint-fenster.log"
# Im Flatpak laufen Browser und Fremdprozesse auf dem Host.
IM_FLATPAK=""
if [ -n "${FLATPAK_ID:-}" ] && command -v flatpak-spawn >/dev/null 2>&1; then
  IM_FLATPAK=1
fi
# --directory ist noetig: das Arbeitsverzeichnis im Sandbox (/app) gibt es auf
# dem Host nicht, sonst scheitert jeder Host-Aufruf am chdir.
host() { if [ -n "$IM_FLATPAK" ]; then flatpak-spawn --host --directory="$HOME" "$@"; else "$@"; fi; }

# Browserprofil fuer das Mixer-Fenster. Im Flatpak zeigt XDG_CONFIG_HOME in das
# Sandbox-Verzeichnis dieser App; der Browser (eigener Flatpak) kann dort nicht
# schreiben und bringt dann kein Fenster zustande. Deshalb immer der echte
# Host-Pfad ~/.config/crosspoint-browser (dort leitet der Browser ihn in sein
# eigenes Datenverzeichnis um).
PROFIL="${CROSSPOINT_PROFILE:-$HOME/.config/crosspoint-browser}"

engine() {
  if [ -n "${CROSSPOINT_ENGINE:-}" ]; then echo "$CROSSPOINT_ENGINE"; return; fi
  if [ -x "$DIR/target/release/crosspoint-engine" ]; then echo "$DIR/target/release/crosspoint-engine"; return; fi
  if [ -x "$HIER/crosspoint-engine" ]; then echo "$HIER/crosspoint-engine"; return; fi
  echo "$DIR/target/release/crosspoint-engine"
}

# Fensterbefehl: im Flatpak auf dem Host, sonst direkt.
fensterbefehl() {
  local cmd="${CROSSPOINT_BROWSER:-flatpak run com.brave.Browser}"
  if [ -n "$IM_FLATPAK" ]; then
    echo "flatpak-spawn --host --directory=$HOME $cmd"
  else
    echo "$cmd"
  fi
}

alive()   { curl -s -o /dev/null --max-time 1 "$URL/api/state"; }
# Fenster vorhanden? Der Browser bindet den Debug-Port je nach Lage auf 127.0.0.1
# oder - wenn IPv4 belegt war - nur auf [::1]; beides wird geprueft.
fenster() {
  local n
  n=$(curl -s --max-time 1 "http://127.0.0.1:$PORT/json/list" 2>/dev/null | grep -c 8770)
  [ "$n" -gt 0 ] && { echo "$n"; return; }
  curl -s --max-time 1 "http://[::1]:$PORT/json/list" 2>/dev/null | grep -c 8770
}

# Raeumt haengende Fenster desselben Profils weg: ein liegengebliebener Prozess
# blockiert den Profil-Lock, der neue Browser oeffnet dann kein Fenster.
FENSTER_MUSTER="crosspoint-browser"
fenster_prozesse() { host pgrep -f "$FENSTER_MUSTER" 2>/dev/null | wc -l; }
# SIGTERM zuerst (der Browser schreibt noch), nach kurzer Frist hart beenden:
# ein haengender Browser haelt sonst Debug-Port und Profil-Lock.
raeume_fenster() {
  host pkill -f "$FENSTER_MUSTER" 2>/dev/null || true
  for _ in $(seq 1 12); do
    [ "$(fenster_prozesse)" -eq 0 ] && return 0
    sleep 0.25
  done
  host pkill -9 -f "$FENSTER_MUSTER" 2>/dev/null || true
  sleep 0.5
}

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

# Immer ein frisches Fenster: ein liegengebliebener Browser desselben Profils
# haelt den Debug-Port (dann bindet der neue nur auf [::1]) und zeigt eine
# veraltete Seite ohne Engine. Deshalb erst wegraeumen, dann starten.
if [ "$(fenster_prozesse)" -gt 0 ]; then
  echo "Altes Fenster wird ersetzt" >&2
  raeume_fenster
  sleep 1
fi
# shellcheck disable=SC2046
$(fensterbefehl) \
  --user-data-dir="$PROFIL" \
  --remote-debugging-port="$PORT" \
  --no-first-run --no-default-browser-check \
  --app="$URL" --window-size=1760,980 >"$LOG_FENSTER" 2>&1 &
for _ in $(seq 1 $((GRACE * 4))); do [ "$(fenster)" -gt 0 ] && break; sleep 0.25; done

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
