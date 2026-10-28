#!/usr/bin/env bash
# crosspoint-wire.sh – virtuelle Ein-/Ausgänge anlegen und mit dem Mixer verbinden.
#   ./scripts/crosspoint-wire.sh up|down|status
set -uo pipefail
CLIENT="${CROSSPOINT_CLIENT:-crosspoint}"
RUNDIR="${XDG_RUNTIME_DIR:-/tmp}/crosspoint"
MODFILE="$RUNDIR/modules"
mkdir -p "$RUNDIR"

link() { pw-link "$1" "$2" 2>/dev/null && echo "  $1 -> $2" || echo "  $1 -> $2 (schon da/fehlt)" >&2; }


# Erzeugt eine echte Aufnahmequelle (kein "Monitor", den KDE ausblendet).
# Ohne das erscheinen die virtuellen Geraete nur unter Ausgabe, nicht unter
# Eingabe - Monitore filtert die Oberflaeche weg.
mic() {
  local ziel="$1" master="$2" desc="$3"
  if pactl list short sources | awk '{print $2}' | grep -qx "$ziel"; then
    echo "  Quelle $ziel existiert bereits" >&2
    return 0
  fi
  local id
  id=$(pactl load-module module-remap-source master="$master" source_name="$ziel" \
        "source_properties=device.description=$desc" 2>/dev/null)
  if [ -n "$id" ]; then
    echo "$id $ziel" >> "$MODFILE"
    echo "  Quelle $ziel -> $desc"
  else
    echo "  Quelle $ziel konnte nicht angelegt werden" >&2
  fi
}

# Legt einen Knoten nur an, wenn er nicht schon existiert (sonst sammeln sich
# bei jedem Aufruf Duplikate an, die in den Sound-Einstellungen auftauchen).
sink() {
  local id
  if pactl list short sinks | awk '{print $2}' | grep -qx "$1"; then
    echo "  Knoten $1 existiert bereits" >&2
    return 0
  fi
  id=$(pactl load-module module-null-sink "sink_name=$1" \
        "sink_properties=device.description=\"$2\"" 2>/dev/null)
  if [ -n "$id" ]; then
    echo "$id $1" >> "$MODFILE"
    echo "  Knoten $1 angelegt (Modul $id)"
  else
    echo "  Knoten $1 konnte nicht angelegt werden" >&2
  fi
}

case "${1:-up}" in
up)
  for i in 1 2 3; do sink "crosspoint_in_$i" "CROSSPOINT-Input-$i"; done
  for b in B1 B2 B3; do sink "crosspoint_out_$b" "CROSSPOINT-Output-$b"; done
  sleep 0.5
  for i in 1 2 3; do
    s=$((5 + i))
    link "crosspoint_in_$i:monitor_FL" "$CLIENT:strip${s}_L"
    link "crosspoint_in_$i:monitor_FR" "$CLIENT:strip${s}_R"
  done
  for b in B1 B2 B3; do
    link "$CLIENT:bus${b}_L" "crosspoint_out_$b:playback_FL"
    link "$CLIENT:bus${b}_R" "crosspoint_out_$b:playback_FR"
  done
  # Aufnahmeseite sichtbar machen: je virtueller Eingang und je virtueller
  # Bus-Ausgabe eine echte Quelle.
  for i in 1 2 3; do
    mic "crosspoint_mic_in_$i" "crosspoint_in_$i.monitor" "CROSSPOINT-Input-$i"
  done
  for b in B1 B2 B3; do
    mic "crosspoint_mic_$b" "crosspoint_out_$b.monitor" "CROSSPOINT-Output-$b"
  done

  for bus in A1 A2 A3 A4 A5; do
    # Kein Autopilot: ein Bus wird nur verbunden, wenn ein Ziel gesetzt ist
    # (CROSSPOINT_A1.. oder die Geraeteauswahl in der Oberflaeche).
    var="CROSSPOINT_$bus"; target="${!var:-}"
    if [ -n "$target" ]; then
      link "$CLIENT:bus${bus}_L" "$target:playback_FL"
      link "$CLIENT:bus${bus}_R" "$target:playback_FR"
    else
      echo "  Bus $bus: kein Ziel gesetzt (CROSSPOINT_$bus=<sink>)" >&2
    fi
  done
  ;;
down)
  if [ -f "$MODFILE" ]; then
    while read -r id name; do pactl unload-module "$id" 2>/dev/null && echo "  $name entfernt"; done < "$MODFILE"
    rm -f "$MODFILE"
  fi
  # Nachzuegler: Module, die nicht (mehr) in der Merkliste stehen, z. B. nach
  # einem Absturz oder wenn die Liste geloescht wurde. Sonst bleiben die
  # virtuellen Geraete nach dem Schliessen im System stehen. Mit erfasst werden
  # die Geraete der Vorgaengerversion (Praefix vm8), die sonst liegen bleiben.
  n=0
  for id in $(pactl list short modules | awk -F'\t' '/sink_name=crosspoint|source_name=crosspoint|sink_name=vm8_|source_name=vm8_/{print $1}'); do
    pactl unload-module "$id" 2>/dev/null && { echo "  Modul $id entfernt (Nachzuegler)"; n=$((n+1)); }
  done
  [ "$n" -eq 0 ] && [ ! -f "$MODFILE" ] && echo "  nichts zu entfernen" >&2
  exit 0
  ;;
status)
  pw-link -l 2>/dev/null | grep -A4 -E "^$CLIENT:" || echo "Client '$CLIENT' nicht verbunden"
  ;;
*)
  echo "Aufruf: $0 [up|down|status]" >&2; exit 2 ;;
esac
