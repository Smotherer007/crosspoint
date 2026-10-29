# Crosspoint — offene Punkte

Stand: 2026-10-29. Erledigt ist alles bis Release `v0.2.0` (natives Fenster,
Zustand merken, Autostart, Tray). Diese Liste ist die Arbeitsgrundlage für das,
was noch fehlt — sortiert nach Nutzen pro Aufwand.

## 1. Läuft gerade

- [ ] **VBAN-Dialog im Original-Look**: `IP Audio Network Configuration` mit
      VBAN an/aus, UDP-Port, IP, Main Stream 48 000 Hz, Incoming- und
      Outgoing-Tabelle (Streamname, IP, SR, Ch, Format, Quality, Destination
      bzw. Source, Net Quality, Errors), Reset/Load/Save Config.
      Abnahme: Dialog per Screenshot, `GET/POST /api/vban/config` per `curl`
      gesetzt und zurückgelesen.
- [ ] **Eigene Namen für Ein- und Ausgänge**: Parameter `label` je Streifen und
      Bus, Anzeige im Kanalzug statt `HW IN 1` / `Bus A1`, gespeichert über den
      vorhandenen Autosave.
      Abnahme: `curl` setzt Label, UI zeigt es, nach Neustart noch da.

## 2. MIDI — über MIDIMonster, nicht selbst gebaut

- [ ] **OSC-Eingang in der Engine**: `POST`/UDP-OSC auf `127.0.0.1:9000`,
      Nachrichten `/strip/<n>/<param> <wert>` und `/bus/<n>/<param> <wert>`,
      angewendet über dieselbe Funktion wie `/api/set`.
      Abnahme: `oscsend` setzt einen Fader, `/api/state` zeigt den Wert.
- [ ] **MIDIMonster-Konfiguration** mitliefern (`data/midimonster.cfg`):
      MIDI-Backend auf das vorhandene Gerät, Mapping CC → OSC-Pfad, ein
      Beispiel-Mapping für acht Fader und Mute.
      Abnahme: CC `B0 07 64` bewegt Streifen 1 über den ganzen Weg.
- [ ] **Learn-Hilfe im UI**: Zeile anklicken, Fader bewegen, Zuordnung wird in
      `midimonster.cfg` nachgetragen (die Map-Tabelle bleibt Anzeige + Editor,
      die Arbeit macht MIDIMonster).
      Abnahme: neuer Eintrag in der Datei, nach Neustart wirksam.

## 3. Effekte

- [ ] **LV2-Inserts über mod-host**: je Streifen und Bus ein Insert-Slot,
      mod-host als betreuter Kindprozess (Socket 5555), Plugin-Liste aus
      `lv2ls`, Zustand im Preset.
      Abnahme: Kompressor-Plugin geladen, Pegel vor/nach messbar.
- [ ] **Carla als VST-Brücke** (optional, nur wenn installiert): Erkennung,
      Start, Rack je Streifen, Parameter über OSC; im UI als „externer Host"
      gekennzeichnet. VST2/VST3 laufen dort, nicht in Crosspoint.
      Abnahme: Carla-Rack mit einem VST, Signal läuft durch den Streifen.

## 4. Parität zum Original (aus `docs/04-abgleich-original.md`)

- [ ] Intellipan / Voice Color (`paneltype`, `Panel3Dx/y`, `ColorPanel*`)
- [ ] Sends `R`, `D`, `Fx1`, `Fx2` mit Pre/Post-Schalter
- [ ] Bus-Returns `retR`, `retD`, `retFx1`, `retFx2`
- [ ] `SEL` und `monitor` je Bus
- [ ] FX-Preset-Programme `prg_c`, `prg_g`, `prg_d`
- [ ] `vaio` — Bus als eigenes virtuelles Gerät
- [ ] `muc`, `karaoke`, `stripmode`, `greenmode`

## 5. Verpackung und Vertrieb

- [ ] **macOS**: CoreAudio-Backend statt JACK, `.dmg`-Job, Signierung
      (ohne Signatur „Right-click → Open" dokumentieren)
- [ ] **Flathub-Einreichung**: Metainfo ist da, Screenshots im Repo,
      Einreichung als PR gegen `flathub/flathub`
- [ ] **Host-Betrieb ohne Flatpak** ebenfalls nativ (braucht
      `webkit2gtk4.1-devel`; auf Bazzite nur per Layering — dokumentieren oder
      bewusst beim Browser-Fallback bleiben)
- [ ] release-please-Kette einmal komplett durchlaufen lassen
      (Feature-Commit → Release-PR → Merge), bisher wird per Tag ausgelöst

## 6. Aufräumen

- [ ] `docs/04-abgleich-original.md` nach jedem Feature nachziehen
- [ ] Brave-Reste des alten Fensterpfads entfernen (`pkill -f crosspoint-browser`)
- [ ] `tools/fenster_screenshot.py`: CDP-Timeout beheben (Screenshot über
      `Page.captureScreenshot` bricht nach 30 s ab)

## Nicht geplant (bewusste Entscheidung)

- **Eigenes VST-Hosting**: VST3-SDK, Plugin-Scan, GUI-Einbettung, Sandboxing —
  Wochen Arbeit. Carla macht das seit Jahren; wir verdrahten es.
- **Windows-VSTs nativ**: braucht Wine. Nur über Carla, nicht in Crosspoint.
- **1:1-Kopie der Original-Oberfläche**: Marken- und Designrisiko. Layout und
  Bedienlogik bleiben vertraut, Farben, Logo und Namen sind eigen.
