# Architektur: ein Prozess, ein Zustand, dünne Ränder

Stand: 2026-10-29. Diese Notiz beantwortet die Frage „was ist der sauberste Weg,
ohne technische Brüche" — und zwar als Zielbild, nicht als Wunschliste.

## Was heute bricht

Ein Bruch ist jede Stelle, an der eine Information neu verpackt oder ein
Zustand doppelt geführt wird. Gezählt am aktuellen Stand:

| Bruch | Wo | Symptom, das schon aufgetreten ist |
|---|---|---|
| Oberfläche ↔ Engine über HTTP + JSON | `web/` → `src/http.rs` | 2 445 Byte JSON je Abfrage, WS-Frames 60×/s, weiße `<select>`-Felder, Fenster- und Gerätezustand laufen auseinander |
| Launcher-Skripte ↔ echter Prozess | `scripts/crosspoint-launch.sh`, Fensterbeobachtung | Engine wurde vom Aufräum-Trap gekillt, Geräte blieben liegen, Brave-Fenster tauchten wieder auf |
| Host-Weg und Flatpak-Weg | Launcher-Verzweigung, SDK-Abhängigkeit | zwei Startpfade, zwei Fehlerquellen |
| Eigenbau-Infrastruktur | JSON, HTTP, WS, JACK-ABI, Ringpuffer | behoben (serde, tiny_http/tungstenite, jack, rtrb/hound) |
| Textausgaben von `pactl`/`pw-link` | `pw.rs`, `devices.rs`, `inputs.rs` | Mono-Mikro nicht verbunden, Monitore als Geräte, Beschreibungen abgeschnitten — in Arbeit (pipewire-Crate) |
| Sechs Konfigurationsdateien in drei Formaten | `devices.json`, `inputs.conf`, `preset.json`, `macros.json`, `vban.json`, `fx.json` | Reihenfolge und Besitz unklar, Autosave schreibt quer |
| Fremdprozesse ohne gemeinsame Grenze | `mod-host`, `Carla`, `MIDIMonster`, VBAN, OSC | jeder mit eigenem Protokoll direkt im Kern verdrahtet |

## Zielbild

```
                     ┌──────────────────────────────────┐
   Tastatur/Maus ───▶│  GTK4-Fenster (im Prozess)       │
   MIDI ────────────▶│                                  │
                     │        Arc<Params>  (Atomics)    │──▶ Audio-Thread (JACK/PipeWire)
                     │        Befehlskanal (mpsc)       │
                     │        Pegelring (lock-frei)     │
                     └───────────┬──────────────────────┘
                                 │ dieselben Befehle
        ┌────────────────────────┼─────────────────────────┐
        ▼                        ▼                         ▼
  HTTP-Adapter (127.0.0.1)  OSC-Adapter (UDP)      VBAN-Adapter (UDP)
   Fernbedienung/Tests       MIDI über MIDIMonster   Netzwerk-Audio
        │
        ▼
  Effekt-Adapter (mod-host / Carla) — eigener Prozess, weil Plugins abstürzen dürfen
```

Vier Regeln, aus denen alles folgt:

1. **Ein Prozess für Bedienung und Audio.** Die Oberfläche liest und schreibt
   `Params` direkt. Kein JSON, kein Socket, keine Serialisierung auf dem Weg
   vom Fader zum Ton.
2. **Ein Befehlskanal für alles, was nicht Atomics ist.** Gerätezuordnung,
   Datei-Zugriff, Fremdprozesse laufen über `mpsc` an einen Worker; die
   Oberfläche blockiert nie. Die HTTP-Routen rufen **dieselbe** Funktion.
3. **Protokolle sind Ränder, nicht Wege.** HTTP, OSC und VBAN sind Adapter auf
   dem Befehlskanal. Fällt ein Adapter weg, ändert sich am Kern nichts.
4. **Ein Zustand, ein Schreiber.** Eine Datei `state.json` mit Abschnitten
   (`devices`, `inputs`, `preset`, `macros`, `vban`, `fx`, `labels`), ein
   Autosave, eine Migration beim ersten Start. Alte Dateien werden gelesen und
   danach nicht mehr geschrieben.

## Was das konkret bedeutet

- **Kein Launcher-Skript mehr als Startweg.** Das Flatpak führt `crosspoint`
  aus; Tray, Autostart, Fenster und Engine sind derselbe Prozess. Skripte
  bleiben nur als Entwicklerhilfen.
- **Kein zweiter Startpfad.** Der Host-Betrieb ist derselbe Prozess mit anderem
  Datenverzeichnis, nicht ein anderer Weg über einen Browser.
- **Fremdprozesse hinter einer Grenze.** Ein Modul `exthost` mit Start,
  Health-Check und Abschaltung; `mod-host` und `Carla` sind Implementierungen
  davon. Fehlt der Host, ist das Feature „aus" und wird als solches gemeldet —
  kein Sonderpfad im Kern.
- **PipeWire nativ, in-process.** Die Registry läuft im selben Prozess, die
  Oberfläche liest den Snapshot; keine Textausgaben, keine Shell.

## Was ausdrücklich ein Bruch bleibt (und warum)

- **Flatpak-Sandbox ↔ Host-PipeWire**: eine OS-Grenze. Nicht auflösbar, nur
  sauber dokumentiert (`--socket=pulseaudio`, `--filesystem=xdg-run/pipewire-0`).
- **Plugin-Hosts in eigenem Prozess**: ein abstürzendes VST darf den Mixer
  nicht mitnehmen. Das ist Absicht, kein Provisorium.
- **HTTP-Adapter**: bleibt für Fernbedienung, Tests und die `VBVMR_*`-Schnitt-
  stelle. Er ist Rand, nicht Bedienweg — abschaltbar mit `CROSSPOINT_HTTP=0`.

## Reihenfolge der Umsetzung

1. **Oberfläche in den Prozess** (GTK4, `Arc<Params>`, Pegelring) — größter
   Bruch fällt, danach ist HTTP nur noch Adapter.
2. **Zustand zusammenführen** (`state.json`, ein Autosave, Migration).
3. **Launcher und Startpfade einebnen** (ein Binary, ein Weg).
4. **`exthost`-Grenze** für Effekte, danach MIDIMonster als Adapter dokumentieren.
5. **PipeWire-Umbau abschließen** (läuft bereits).

Jeder Schritt muss einzeln lauffähig sein; nach jedem Schritt Tests grün und
die API-Antworten unverändert. Kein Schritt darf einen neuen Adapter
einführen, ohne dass ein alter wegfällt.

## Darstellung: zwei getrennte Modelle

Oberfläche ist konfigurierbar, aber in **zwei** Modellen — sie zu vermischen
war der klassische Fehler bei „Theme-Engines", die dann halb funktionieren:

| Modell | Beantwortet | Enthält | Datei |
|---|---|---|---|
| **Token-Satz** | „Wie sieht es aus?" | Grund, Panel, Linie, Akzent, Schrift, Gedämpft, Warnung, Schriftfamilie, drei Größen | `~/.config/crosspoint/ui.json` → `theme` |
| **Layout** | „Wo steht was?" | Reihenfolge und Sichtbarkeit der Bereiche (Kopfzeile, Tape, Streifen, Busse, Master, Makros), Seite der Busspalte, Master-Position, Dichte, Streifenreihenfolge | dieselbe Datei → `layout` |

Regeln, die daraus folgen:

- Farben werden **zur Laufzeit in CSS gerendert** und als `GtkCssProvider`
  geladen — ein Wechsel wirkt ohne Neustart.
- **Auch selbst gezeichnete Fader und Pegel** lesen ihre Farben aus dem
  Token-Satz. Ein Theme, das überall greift außer dort, wo man hinsieht, ist
  kein Theme.
- Das Fenster wird **aus der Layout-Beschreibung zusammengesetzt**; keine
  fest verdrahtete Anordnung im Code. Jeder Bereich ist ein eigenes Widget,
  das je nach Layout eingesetzt, verschoben oder ausgeblendet wird.
- Ungültige Datei → Standard (Theme `crosspoint`, Layout `klassisch`) plus
  Logzeile, nie ein Startfehler.
- Der Token-Satz ist ein **Datenformat**, kein GTK-Detail: das Web-UI kann
  dieselbe Datei später lesen, damit es nicht wieder zwei Wahrheiten gibt.

