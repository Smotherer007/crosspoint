#!/usr/bin/env python3
"""Installiert den Crosspoint-Icon-Satz und den Startmenue-Eintrag im Benutzerprofil."""
import os
import shutil
import subprocess
from PIL import Image

QUELLE = "/home/patrickweppelmann/Dokumente/Workspaces/VoicemeeterForLinux/docs/brand/crosspoint-icon-1024.png"
HOME = os.path.expanduser("~")
ICONS = os.path.join(HOME, ".local/share/icons/hicolor")
APPS = os.path.join(HOME, ".local/share/applications")
NAME = "de.weppelmann.Crosspoint"
LAUNCHER = os.path.join(HOME, "Dokumente/Workspaces/VoicemeeterForLinux/scripts/vm8-launch.sh")

bild = Image.open(QUELLE).convert("RGBA")
groessen = [16, 24, 32, 48, 64, 128, 256, 512]
for n in groessen:
    ziel = os.path.join(ICONS, f"{n}x{n}", "apps")
    os.makedirs(ziel, exist_ok=True)
    bild.resize((n, n), Image.LANCZOS).save(os.path.join(ziel, f"{NAME}.png"))
    print(f"  {n}x{n} -> {os.path.join(ziel, NAME + '.png')}")

# Skalierbar (SVG gibt es nicht, daher groesste PNG als Fallback)
os.makedirs(os.path.join(ICONS, "scalable", "apps"), exist_ok=True)
shutil.copy(QUELLE, os.path.join(ICONS, "scalable", "apps", f"{NAME}.png"))

os.makedirs(APPS, exist_ok=True)
eintrag = f"""[Desktop Entry]
Type=Application
Name=Crosspoint
GenericName=Audio-Mixer
Comment=8 Kanaele, 8 Busse, Routing auf PipeWire
Exec={LAUNCHER}
Icon={NAME}
Terminal=false
Categories=AudioVideo;Audio;Mixer;
Keywords=mixer;audio;routing;pipewire;jack;
StartupWMClass=Crosspoint
"""
pfad = os.path.join(APPS, f"{NAME}.desktop")
with open(pfad, "w") as f:
    f.write(eintrag)
os.chmod(pfad, 0o755)
print("  Desktop-Eintrag:", pfad)

alt = os.path.join(APPS, "vm8-mixer.desktop")
if os.path.exists(alt):
    os.remove(alt)
    print("  alten Eintrag vm8-mixer.desktop entfernt")

for cmd in (["gtk-update-icon-cache", "-f", "-t", ICONS],
            ["update-desktop-database", APPS]):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True)
        print(f"  {cmd[0]}: exit {r.returncode}")
    except FileNotFoundError:
        print(f"  {cmd[0]}: nicht vorhanden (uebersprungen)")
