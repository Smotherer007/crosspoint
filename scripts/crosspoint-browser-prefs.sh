#!/usr/bin/env bash
# Setzt im Brave-Profil der App die Einwilligungen vorab, damit Brave beim
# Oeffnen kein Hinweisband (Produktanalysen/P3A, "Alles klar") einblendet.
set -uo pipefail
PROFIL="${CROSSPOINT_PROFILE:-$HOME/.config/crosspoint-browser}"
PREFS="$PROFIL/Default/Preferences"
mkdir -p "$(dirname "$PREFS")"

python3 - "$PREFS" <<'PY'
import json, os, sys
pfad = sys.argv[1]
daten = {}
if os.path.exists(pfad):
    try:
        with open(pfad, encoding="utf-8") as f:
            daten = json.load(f)
    except Exception:
        daten = {}
brave = daten.setdefault("brave", {})
p3a = brave.setdefault("p3a", {})
p3a["enabled"] = False
p3a["notice_acknowledged"] = True
brave.setdefault("stats", {})["reporting_enabled"] = False
daten.setdefault("distribution", {})["skip_first_run_ui"] = True
daten.setdefault("browser", {})["has_seen_welcome_page"] = True
with open(pfad, "w", encoding="utf-8") as f:
    json.dump(daten, f)
print("Vorbelegung geschrieben:", pfad)
for k in ("brave.p3a.enabled", "brave.p3a.notice_acknowledged", "brave.stats.reporting_enabled"):
    knoten = daten
    for teil in k.split("."):
        knoten = knoten.get(teil, {}) if isinstance(knoten, dict) else {}
    print(" ", k, "=", knoten)
PY
