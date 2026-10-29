#!/bin/bash
# Screenshot der GTK-Oberflaeche ohne sichtbaren Bildschirm:
# eigenes Xwayland auf :99 (1600x920), App dorthin, dann ImageMagick import.
#   ./screenshot.sh ziel.png [--theme crosspoint-hell] [--layout breit]
set -e
BIN=${BIN:-./target/debug/crosspoint-gtk}
ZIEL=${1:-screenshot.png}
shift || true
BREITE=${BREITE:-1600}
HOEHE=${HOEHE:-920}

pkill -x crosspoint-gtk 2>/dev/null || true
pkill -x Xwayland 2>/dev/null || true
sleep 0.5
Xwayland :99 -geometry "${BREITE}x${HOEHE}" >/dev/null 2>&1 &
XPID=$!
sleep 2
env -u WAYLAND_DISPLAY DISPLAY=:99 GDK_BACKEND=x11 GTK_A11Y=none XDG_RUNTIME_DIR=/run/user/1000 \
    "$BIN" --size "${BREITE}x${HOEHE}" "$@" > /tmp/crosspoint-gtk.log 2>&1 &
APID=$!
sleep 5
# Zeiger aus dem Weg: sonst steht ein Tooltip im Bild.
DISPLAY=:99 xdotool mousemove 1590 910 2>/dev/null || true
sleep 0.4
DISPLAY=:99 import -window root "$ZIEL"
kill $APID 2>/dev/null || true
kill $XPID 2>/dev/null || true
echo "geschrieben: $ZIEL"
