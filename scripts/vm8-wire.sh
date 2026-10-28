#!/usr/bin/env bash
# vm8-wire.sh – virtuelle Ein-/Ausgänge anlegen und mit dem Mixer verbinden.
#   ./scripts/vm8-wire.sh up|down|status
set -uo pipefail
CLIENT="${VM8_CLIENT:-vm8}"
RUNDIR="${XDG_RUNTIME_DIR:-/tmp}/vm8"
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
  for i in 1 2 3; do sink "vm8_in_$i" "VM8-Input-$i"; done
  for b in B1 B2 B3; do sink "vm8_out_$b" "VM8-Output-$b"; done
  sleep 0.5
  for i in 1 2 3; do
    s=$((5 + i))
    link "vm8_in_$i:monitor_FL" "$CLIENT:strip${s}_L"
    link "vm8_in_$i:monitor_FR" "$CLIENT:strip${s}_R"
  done
  for b in B1 B2 B3; do
    link "$CLIENT:bus${b}_L" "vm8_out_$b:playback_FL"
    link "$CLIENT:bus${b}_R" "vm8_out_$b:playback_FR"
  done
  # Aufnahmeseite sichtbar machen: je virtueller Eingang und je virtueller
  # Bus-Ausgabe eine echte Quelle.
  for i in 1 2 3; do
    mic "vm8_mic_in_$i" "vm8_in_$i.monitor" "VM8-Input-$i"
  done
  for b in B1 B2 B3; do
    mic "vm8_mic_$b" "vm8_out_$b.monitor" "VM8-Output-$b"
  done

  for bus in A1 A2 A3 A4 A5; do
    var="VM8_$bus"; target="${!var:-}"
    if [ -n "$target" ]; then
      link "$CLIENT:bus${bus}_L" "$target:playback_FL"
      link "$CLIENT:bus${bus}_R" "$target:playback_FR"
    else
      echo "  Bus $bus: kein Ziel gesetzt (VM8_$bus=<sink>)" >&2
    fi
  done
  ;;
down)
  if [ -f "$MODFILE" ]; then
    while read -r id name; do pactl unload-module "$id" 2>/dev/null && echo "  $name entfernt"; done < "$MODFILE"
    rm -f "$MODFILE"
  else
    echo "keine gemerkten Knoten" >&2
  fi
  ;;
status)
  pw-link -l 2>/dev/null | grep -A4 -E "^$CLIENT:" || echo "Client '$CLIENT' nicht verbunden"
  ;;
*)
  echo "Aufruf: $0 [up|down|status]" >&2; exit 2 ;;
esac
