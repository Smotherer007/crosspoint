#!/usr/bin/env python3
"""Installiert Icon-Satz und Startmenue-Eintrag von Crosspoint im Benutzerprofil.

Quelle sind die Dateien im Repo: `docs/brand/crosspoint-icon-1024.png` und der
Desktop-Eintrag `data/de.weppelmann.Crosspoint.desktop`. Der Eintrag wird fuer
die Host-Installation auf das Startskript im Repo umgeschrieben (im Flatpak
zeigt er auf `/app/bin/crosspoint-launch`).
"""
import os
import shutil
import subprocess
from PIL import Image

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
QUELLE = os.path.join(REPO, "docs/brand/crosspoint-icon-1024.png")
VORLAGE = os.path.join(REPO, "data/de.weppelmann.Crosspoint.desktop")
LAUNCHER = os.path.join(REPO, "scripts/crosspoint-launch.sh")
HOME = os.path.expanduser("~")
ICONS = os.path.join(HOME, ".local/share/icons/hicolor")
APPS = os.path.join(HOME, ".local/share/applications")
NAME = "de.weppelmann.Crosspoint"

bild = Image.open(QUELLE).convert("RGBA")
for n in (16, 32, 48, 64, 128, 256, 512):
    ziel = os.path.join(ICONS, f"{n}x{n}", "apps")
    os.makedirs(ziel, exist_ok=True)
    pfad = os.path.join(ziel, f"{NAME}.png")
    bild.resize((n, n), Image.LANCZOS).save(pfad)
    print(f"  {n}x{n} -> {pfad}")

os.makedirs(APPS, exist_ok=True)
with open(VORLAGE) as f:
    eintrag = f.read()
eintrag = eintrag.replace("Exec=crosspoint-launch", f"Exec={LAUNCHER}")
pfad = os.path.join(APPS, f"{NAME}.desktop")
with open(pfad, "w") as f:
    f.write(eintrag)
os.chmod(pfad, 0o755)
print("  Desktop-Eintrag:", pfad)

# Alte Eintraege der Vorgaengerversion entfernen.
for alt in ("vm8-mixer.desktop", "vm8.desktop"):
    p = os.path.join(APPS, alt)
    if os.path.exists(p):
        os.remove(p)
        print(f"  alten Eintrag {alt} entfernt")

for cmd in (["gtk-update-icon-cache", "-f", "-t", ICONS],
            ["update-desktop-database", APPS]):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True)
        print(f"  {cmd[0]}: exit {r.returncode}")
    except FileNotFoundError:
        print(f"  {cmd[0]}: nicht vorhanden (uebersprungen)")
