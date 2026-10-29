# Phase 2 — was bewusst später kommt

Stand: 2026-10-29. Alles hier ist beschlossen, aber nicht in der laufenden
Runde. Reihenfolge = Nutzen pro Aufwand.

## 1. Makro-Engine (80 Tasten, wie im Original)

Vorbild aus dem Original (Screenshots des Nutzers): *Macro Buttons* mit
Farbe, Typ `PUSH`/`TOGGLE`/`2P`/`3P`, Namen und Untertitel, ein Dialog
*Button Configuration* mit Anforderung für Ein-/Ausgangsbefehl
(`recorder.load = "M3 - NeuTets_Theme.mp3"; Recorder.play = 1;`), MIDI-
Implementierung (CC, Note, Gamepad) und Trigger mit Streifen/Ziel/Out/Hold.

**Umsetzung:** Die Befehlslogik wird nicht als eigener Befehlsparser gebaut,
sondern als **Skript je Taste** — Rhai (reines Rust, keine C-Abhängigkeit,
sandboxbar, kleine Laufzeit) mit einer festen API:

```
set("strip", 0, "gain", -6.0)
toggle("strip", 2, "mute")
preset("load", "abend")
tape("record", true)
send("osc", "/bus/0/mute", 1.0)
```

- Tasten-Zustand (Farbe, Typ, Skript, MIDI-Bindung) in `state.json`
- Ausführung über den **Befehlskanal**, nicht direkt — ein Skript darf den
  Audio-Thread nie blockieren; Zeitlimit je Skript (z. B. 50 ms), sonst Abbruch
- `PUSH`/`TOGGLE`/`2P`/`3P` als Zustandsautomat im Modell, nicht im Skript
- Rückmeldung an MIDI/OSC (LED-Rückkanal) über den vorhandenen OSC-Adapter

**Abnahme:** eine Taste lädt ein Preset, eine schaltet Mute auf Streifen 3,
eine startet die Aufnahme — alle drei per MIDI-CC ausgelöst, Zustand überlebt
einen Neustart.

## 2. Anwendungen einzeln steuern („Apps"-Streifen)

Im Original (Screenshot) gibt es je Anwendung einen eigenen Block mit
`M`-Taste und Buszuordnung — Windows-Anwendungen als eigene Quellen.

**Auf Linux über PipeWire:** jeder `Stream/Input/Audio`-Knoten ist eine
Anwendung. Ziel:

- eine Liste der laufenden Anwendungen im UI (Name, Pegel, `M`, Buszuordnung)
- Anwendung auf einen Streifen legen (statt „Gerät"), inklusive Neustart-
  Erkennung, wenn die Anwendung neu startet
- Regeln: „Anwendung X immer auf Streifen 3", gespeichert in `state.json`

**Grenze, die dokumentiert werden muss:** nicht jede Anwendung lässt sich
umleiten (manche öffnen Geräte exklusiv), und die Zuordnung gilt ab dem
nächsten Verbindungsaufbau der Anwendung.

## 3. Rest aus `docs/04-abgleich-original.md`

Intellipan/Voice Color · Sends `R/D/Fx1/Fx2` mit Pre/Post · Bus-Returns ·
`SEL`/`monitor` · FX-Presets `prg_c/g/d` · `vaio` · `muc`/`karaoke`/
`stripmode`/`greenmode`

## 4. Verpackung

macOS (CoreAudio) · Flathub-Einreichung · release-please-Kette einmal
vollständig · Launcher-Skripte durch das eine Binary ersetzen

## 5. Optimierungen aus dem Audit (laufend)

- Test-Isolation der Konfigurationspfade (ein Testlauf hat schon einmal das
  echte `preset.json` überschrieben)
- `state.json` als einzige Zustandsquelle statt sechs Dateien
- strukturiertes Logging statt 27 `eprintln!`
- Screenshot-Vergleich der Oberfläche als Regressionstest
