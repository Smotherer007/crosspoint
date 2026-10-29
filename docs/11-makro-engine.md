# Makro-Engine — Feinentwurf

Stand: 2026-10-29. Ergänzt `docs/10-phase2.md`, Abschnitt 1. Grundlage sind die
Screenshots des Originals (*Macro Buttons* + *Button Configuration*).

## Was eine Taste im Original alles kann

Aus dem Dialog *Button Configuration (Logical ID: 3)*:

| Feld | Bedeutung |
|---|---|
| `Button Name` / `Button Sub Name` | Beschriftung; Untertitel wird im Kachelformat gezeigt |
| `Button Color` | Farbe der Kachel (`Color #4` usw.) |
| `Button Type` | `PUSH`, `TOGGLE`, `2P`, `3P` — Schaltverhalten |
| `Keyboard Shortcut` + `Exclusive Key` | globale Tastenkombination |
| `Request for Initial State` | Skript, das beim Start den Ist-Zustand abfragt (`Recorder.play = 1`) |
| `Request for Button ON / Trigger IN` | was beim Drücken passiert |
| `Request for Button OFF / Trigger OUT` | was beim Loslassen passiert |
| `MIDI Implementation` | `Learn (From MIDI mapping device)`, `GainPad Button`, Midimap-Dialog |
| Trigger-Block | `Streife`, `In`, `Out`, `Hold`, `Level Option` — Pegel- oder Zustandsauslöser |

Alle Felder gehören ins Modell. Eine Taste ist damit **kein** Knopf mit einem
Befehl, sondern ein kleiner Automat mit Ein-/Ausgang und optionaler
Rückmeldung.

## Die Sprache

Rhai als Unterbau (reines Rust, sandboxbar, Zeitlimit je Skript), aber **nicht**
Rhai-Syntax zum Anfassen. Der Skript-Autor sieht eine kleine, gesprochene
Sprache mit Objekten, Eigenschaften und Methoden:

```
# Kanalzug
strip(1).gain = -6            # Streifen 1 auf -6 dB (1-basiert wie im Pult)
strip(1).mute = on
strip(1).pan  = 0.5
strip(1).eq(2) = 3            # drittes EQ-Band +3 dB

# Bus
bus("A1").gain = -3
bus("A1").mute = on

# Streifenzuordnung
strip(2).to("A1")             # auf Bus A1 legen
strip(2).remove("B1")

# Zustand und Dateien
preset.load("abend")
preset.save()
tape.record = on
macro(7).push()               # andere Taste auslösen (ohne Rückkopplung)

# Anwendungen (Phase 2)
app("Firefox").to(3)
```

Regeln, die die Sprache intuitiv halten:

1. **Eigenschaften statt Befehle:** `strip(1).gain = -6` liest sich wie die
   Beschriftung im Pult. Zuweisung ist der Normalfall.
2. **Zahlen wie im Original:** Streifen 1–8, Busse als `"A1".."A5","B1".."B3"`,
   Pegel in dB (nicht in Rohwerten). Keine 0-Basis, keine Skalen-Umrechnung
   im Kopf.
3. **`on` / `off`** statt `true`/`false`.
4. **Jede Zeile eine Wirkung**, Semikolon optional — wer will, kann es setzen.
5. **Fehler nennen Zeile und Feld** (`Zeile 2: Bus "A9" gibt es nicht`), nicht
   „Syntax error".

## Hilfe, die nicht veraltet

Die Befehlsliste wird **aus dem Code erzeugt**, nicht als Text gepflegt: jedes
Objekt registriert seine Eigenschaften und Methoden mit Kurzbeschreibung und
Beispiel. Daraus entstehen

- ein **Hilfe-Knopf im Button-Configuration-Dialog** (Liste links, Beispiel
  rechts, „in den Editor übernehmen"),
- `docs/makro-sprache.md` als generierte Datei im Repo,
- die Autovervollständigung im Editor (Eigenschaften des angesprochenen
  Objekts).

Damit kann die Hilfe nicht auseinanderlaufen — der häufigste Grund, warum
Makro-Sprachen unbenutzbar werden.

## MIDI, OSC, Tastatur

Eine Taste hat **mehrere Auslöser**, alle optional:

| Auslöser | Weg |
|---|---|
| MIDI CC / Note | CC-Nummer + Kanal, `Learn`-Knopf nimmt den nächsten eintreffenden Wert; gespeichert im Tastenmodell |
| OSC | fester Pfad `/macro/<n>` (1-basiert), damit MIDIMonster oder TouchOSC direkt binden können |
| Tastatur | Kombination + `Exclusive Key` |
| Pegel/Zustand | Trigger-Block: Streifen, In/Out, Hold in ms, Level |
| Andere Taste | `macro(n).push()` im Skript |

Rückmeldung (LED, Motorfader, OSC-Echo, MIDI-Out) geht über denselben
Adapter-Weg wie die Steuerung — eine Taste kann ihren Zustand melden, ohne dass
der Kern etwas über MIDI weiß.

## Ausführung

- Skripte laufen **nie** im Audio-Thread: Auftrag über den Befehlskanal an den
  Worker, Zeitlimit 50 ms, danach Abbruch mit Logzeile.
- Ein fehlerhaftes Skript blockiert die Taste, nicht die Anwendung: Fehler wird
  an der Taste angezeigt (roter Rand, Tooltip mit Zeile).
- Zustandsautomat `PUSH`/`TOGGLE`/`2P`/`3P` im Modell; das Skript beschreibt
  nur die Wirkung von Ein und Aus.
- `Request for Initial State` läuft beim Start einmal je Taste, um die
  Anzeige mit der Wirklichkeit abzugleichen.

## Abnahme

1. Taste 1: `preset.load("abend")` — per MIDI-CC 20 ausgelöst, wirkt.
2. Taste 2: `strip(3).mute = on` als `TOGGLE` — LED/OSC-Rückmeldung folgt.
3. Taste 3: `tape.record = on` als `PUSH` — Aufnahme startet, zweiter Druck
   stoppt.
4. Ein Skript mit Tippfehler zeigt Zeile und Feld im Dialog, Anwendung läuft
   weiter.
5. Hilfe-Knopf zeigt für `strip().gain` ein lauffähiges Beispiel, das per Klick
   in den Editor geht.
6. Alle vier Tastentypen verhalten sich nach einem Neustart identisch.
