# GTK-Oberfläche: Abgleich gegen den Web-Stand

Stand: 2026-10-29, geprüft durch Vergleich der API-Nutzung in `web/index.html`
(9 Bereiche) mit dem Stand in `crates/ui-gtk/` (5 317 Zeilen, Phase 1).

Legende: ✔ vorhanden · ◐ teilweise · ✘ fehlt

## Bereiche

| Bereich | Web | GTK | Anmerkung |
|---|---|---|---|
| Kanalzüge 1–8 mit Fader, Pan, Pegel | ✔ | ◐ | Fader/Pegel gezeichnet, Pan und echte Pegeldaten offen |
| EQ-Bänder je Streifen | ✔ | ◐ | Raster gezeichnet, **Editor fehlt** (`eqOverlay` im Web) |
| Mute / Solo / Mono je Streifen | ✔ | ✔ | |
| **Zuordnungstasten A1–B3** je Streifen | ✔ | ✘ | beauftragt |
| **Gerätezuordnung** je Streifen (`/api/inputs`) | ✔ | ✘ | beauftragt |
| **Gerätezuordnung** je Bus (`/api/devices`) | ✔ | ✘ | beauftragt |
| Busspalte A1–A5, B1–B3 mit Fader/Pegel | ✔ | ◐ | |
| Master mit Fader, Pegel, Mute, Mono, Limiter | ✔ | ◐ | Limiter/Mono offen |
| **Tape-Leiste** (`/api/tape`: Aufnahme, Stopp, Zeit, Pegel, Datei) | ✔ | ✘ | **fehlt — als nächstes** |
| **Preset-Verwaltung** (`/api/preset`: laden, speichern, Namen) | ✔ | ✘ | **fehlt — als nächstes** |
| **Makros** (`/api/macros`: 80 Tasten, Seiten `mPage`/`mPrev`/`mNext`, Zustand, PUSH/TOGGLE) | ✔ | ◐ | Raster als Platzhalter, ohne Funktion |
| VBAN-Dialog (`/api/vban/config`) | ✔ | ◐ | Tabelle vorhanden, Beispieldaten |
| MIDI-Mapping-Dialog | ✘ | ◐ | im Web gar nicht vorhanden, in GTK begonnen |
| Echtzeitpegel (`/api/levels`, WebSocket) | ✔ | ✘ | Attrappe, Kern braucht öffentliche Pegel-Atomics |
| Zustand (`/api/state`) | ✔ | ✔ | direkt über `Params` |
| Sends / Routing-Matrix (`/api/set` mit `send`) | ✔ | ✘ | Phase 2 (`docs/04`) |

## Reihenfolge bis zur Abnahme

1. **Tape-Leiste** — Aufnahme starten/stoppen, Laufzeit, Spitzenpegel, letzte
   Datei, Zustand aus `/api/tape` (bzw. in-process `tape_io`)
2. **Gerätezuordnung** Streifen und Busse (beauftragt)
3. **Zuordnungstasten** A1–B3 je Streifen (beauftragt)
4. **Preset-Verwaltung** — Liste, Laden, Speichern, „zuletzt geladen"
5. **EQ-Editor** als Overlay wie `eqOverlay` im Web
6. **Makros** mit Funktion: 80 Tasten, Seiten, PUSH/TOGGLE, Farben
7. **Echte Pegel** — dafür fehlen im Kern öffentliche Pegel-Atomics je Streifen
   und Bus; ich ergänze sie, sobald die Oberfläche sie braucht
8. **Sends/Routing** — Phase 2

## Regel für die Abnahme

Ein Bereich gilt erst als ✔, wenn er **bedienbar** ist: Klick ändert den
Zustand, der Zustand überlebt einen Neustart, und es gibt einen Beleg
(Screenshot oder Test). Angelegt heißt nicht fertig.
