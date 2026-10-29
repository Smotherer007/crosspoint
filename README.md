# Crosspoint

8×8-Mixer für PipeWire/JACK in Rust: 8 Streifen, 8 Busse (A1–A5 Hardware, B1–B3
virtuell), ohne Kernelmodul. Die Infrastruktur kommt aus etablierten Crates
(JSON: `serde_json`, HTTP/WebSocket: `tiny_http` + `tungstenite`, WAV: `hound`,
Ring: `rtrb`, JACK: `jack`) - siehe `docs/08-refactor-bibliotheken.md`. Die Engine ist ein
JACK-Client; die Oberfläche läuft als Web-App auf `127.0.0.1:8770` und wird als
eigenes Fenster gestartet.

## Signalkette

```
Streifen 1..8:  Eingang → 6-Band-EQ → Compressor → Gate → Denoiser → Gain → Pan → Routing
Busse A1..A5 / B1..B3:  Summe → Limiter → Delay je Kanal → Trim → Gain/Mute → Ausgang
```

Herleitung und Belege der Parameternamen: `docs/02-signalmodell.md`,
`docs/03-ghidra-app.md`, `docs/04-abgleich-original.md`. Die dort untersuchte
Fremdsoftware wird in diesem Repo nicht als Produktname geführt; Dateinamen und
Hashes sind reine Analysebelege.

## Stand (2026-10-28)

| Teil | Zustand |
|---|---|
| Parametermodell (`src/lib.rs`) | vollständig: 8 Streifen/8 Busse, EQ-Zellen, Comp, Gate, Denoiser, Pitch, Sends, Layer, Bus-Modi |
| Echtzeit-Kern (`src/engine.rs`) | 8×8-Matrix, EQ, Comp, Gate, Denoiser, Limiter, Delay, Pegel |
| DSP-Bausteine (`src/dsp.rs`) | RBJ-Biquads, Kompressor, Gate, Limiter, Delay |
| Audio-I/O (`src/jack.rs`) | JACK-ABI über PipeWire, ohne `-devel`-Pakete |
| Steuer-API + UI (`src/http.rs`, `web/`) | JSON-API, Web-Oberfläche, Presets |
| VBAN (`crates/vban`, `src/vban_io.rs`) | Sender/Empfänger, VBAN-kompatibel in beiden Richtungen |
| Tape (`crates/tape`, `src/tape_io.rs`) | Bandmaschine mit WAV-Mitschnitt |
| Tests | `cargo test --release --workspace`: 189 im Hauptcrate, 94 in den path-Crates, alle grün |

Gemessen (nicht behauptet): 1-kHz-Sinus −6 dBFS durch Streifen 1 → Bus A1 ergibt
−6,00 dBFS; Gain −12 dB → −18,00 dBFS; Mute → Stille; Routing A1→A2 wandert;
EQ-Boost +12 dB bei 1 kHz → exakt +12 dB.

## Bauen und starten

Voraussetzungen: `pipewire` (mit `pipewire-jack`), `pactl`, `cargo`. Keine
`-devel`-Pakete nötig: `build.rs` linkt gegen die vorhandene
`libjack.so.0` (`/usr/lib64/pipewire-0.3/jack` auf Fedora/Bazzite,
`/usr/lib/x86_64-linux-gnu` im Freedesktop-Runtime).

```bash
cargo build --release
./target/release/crosspoint-engine                 # UI auf http://127.0.0.1:8770/
CROSSPOINT_A1=<pipewire-sink> ./scripts/crosspoint-wire.sh up
./scripts/crosspoint-launch.sh                     # Engine + Fenster, Ende bei Fensterschluss
```

`crosspoint-wire.sh up` legt `crosspoint_in_1..3` (Anwendungen spielen hinein →
Streifen 6..8, Anzeigename `CROSSPOINT-Input-1..3`) und `crosspoint_out_B1..3`
(Anwendungen nehmen auf ← Busse B1..B3, Anzeigename `CROSSPOINT-Output-B1..B3`)
als `null-sink` an; die Hardware-Busse A1..A5 kommen über `CROSSPOINT_A1` …
`CROSSPOINT_A5`. Die Engine ruft das Skript beim Start selbst auf
(`CROSSPOINT_NO_WIRE=1` schaltet das ab).

Der JACK-Client heißt `crosspoint`, seine Ports sind `crosspoint:strip1_L` …
`crosspoint:busB3_R` sowie `crosspoint:vban_L/_R`.

## Konfiguration

Alles liegt in `~/.config/crosspoint` (bzw. `$XDG_CONFIG_HOME/crosspoint`):

| Datei | Inhalt |
|---|---|
| `preset.json` | aktueller Mischpultzustand |
| `devices.json` | Hardware-Bus → PipeWire-Gerät |
| `inputs.conf` | Streifen 1..5 → Aufnahmequelle |
| `macros.json` | Makro-Tasten |
| `tape/` | Mitschnitte der Bandmaschine |

Beim ersten Start wird ein vorhandenes `~/.config/vm8-linux` einmalig nach
`~/.config/crosspoint` umbenannt (nur wenn das neue Verzeichnis noch fehlt) –
Presets, Gerätewahl und Makros bleiben damit erhalten.

Im **Flatpak** zeigt `XDG_CONFIG_HOME` in den Sandbox-Ordner; die Konfiguration
liegt dort unter `~/.var/app/de.weppelmann.Crosspoint/config/crosspoint`. Der
Autostart-Eintrag dagegen gehört dem Host und landet immer in
`~/.config/autostart`.

## Zustand beim Start, Autosave, Autostart, Tray

Beim Start wird der gemerkte Zustand **wiederhergestellt**, nicht nur gelesen:

1. `devices.json` – jeder Bus wird über denselben Weg wie `POST /api/devices`
   neu verdrahtet (`pw-link` setzen, alte Links lösen).
2. `inputs.conf` – Streifen-Eingänge über `inputs::assign`.
3. `preset.json` – das vollständige Parameterbild.

Das läuft, nachdem die virtuellen Geräte stehen (`crosspoint-wire.sh up`), und
steht danach im Engine-Protokoll:
`Zustand wiederhergestellt: 1 Bus(se), 1 Streifen-Eingang, Preset geladen`.
Geräte, die gerade nicht da sind, werden gezählt und gemeldet, aber nicht als
Fehler behandelt.

**Autosave:** jede Änderung über `POST /api/set` setzt nur einen Zeitstempel;
2 s nach der letzten Änderung schreibt ein eigener Thread das komplette
Parameterbild nach `preset.json` (kein Lock, keine Datei-Arbeit im
Audio-Callback). Eine Faderfahrt erzeugt so genau einen Schreibvorgang.

`CROSSPOINT_NO_RESTORE=1` schaltet Wiederherstellen **und** Autosave ab.

**Autostart:** `POST /api/autostart {"enabled":true}` schreibt
`~/.config/autostart/de.weppelmann.Crosspoint.desktop`, `false` entfernt sie
wieder. Der Startbefehl ist im Flatpak
`flatpak run de.weppelmann.Crosspoint --minimized`, auf dem Host
`<repo>/scripts/crosspoint-launch.sh --minimized`. Beide starten ohne Fenster,
nur mit Tray-Icon.

**Tray:** `crosspoint-fenster` meldet ein StatusNotifierItem über GDBus an
(keine Zusatzbibliothek, `gio` kommt mit GTK) samt `com.canonical.dbusmenu`:

| Menüpunkt | Wirkung |
|---|---|
| Fenster zeigen | Fenster einblenden |
| Alles stumm | alle Streifen-Mutes umschalten (`POST /api/set`) |
| Letztes Preset laden | `POST /api/preset {"action":"load"}` |
| Autostart ein/aus | `POST /api/autostart`, Haken zeigt den Zustand |
| Beenden | Programm und Engine beenden |

Wird das Fenster bei laufendem Tray geschlossen, verschwindet es nur ins Icon
(„Beenden“ bleibt im Menü). Ohne Tray beendet das Schließen wie bisher das
Programm.

## Steuer-API

```bash
curl -X POST http://127.0.0.1:8770/api/set \
     -d '{"target":"strip","index":0,"param":"gain","value":-12}'
curl http://127.0.0.1:8770/api/state     # kompletter Zustand
curl http://127.0.0.1:8770/api/levels    # Pegel in dB
curl -X POST http://127.0.0.1:8770/api/preset -d '{"action":"save"}'
curl http://127.0.0.1:8770/api/restore   # {"devices":n,"inputs":n,"preset":bool}
curl -X POST http://127.0.0.1:8770/api/restore -d '{"preset":true,"devices":true,"inputs":false}'
curl http://127.0.0.1:8770/api/autostart # {"enabled":…,"path":…,"exec":…}
curl -X POST http://127.0.0.1:8770/api/autostart -d '{"enabled":true}'
curl http://127.0.0.1:8770/api/vban/config   # Sende-/Empfangsweg + beide Tabellen
curl -X POST http://127.0.0.1:8770/api/vban/config \
     -d '{"senden":{"an":true,"host":"192.168.1.255","port":6980,"stream":"CROSSPOINT"},"empfangen":{"an":true,"port":6980}}'
curl -X POST http://127.0.0.1:8770/api/vban/config -d '{"action":"save"}'   # auch "load" und "reset"
```

Parameter: `gain`, `pan`, `mute`, `solo`, `mono`, `eq`, `comp`, `gate`, `denoiser`,
`send0..send7`, `eq<zelle>_on|type|freq|q|gain`, `comp_threshold|ratio|attack|release|knee`,
`gate_threshold|attack|hold|release`; Busse: `gain`, `mute`, `mono`, `eq`, `limiter`,
`trim`, `delay`. Die Engine hat keine Authentifizierung und lauscht nur auf `127.0.0.1`.

**Anzeigenamen:** Streifen und Busse tragen auf Wunsch einen eigenen Namen. Der
Wert ist Text, nicht Zahl; ein leerer Text setzt wieder `HW In 1`/`A1`:

```bash
curl -X POST http://127.0.0.1:8770/api/set \
     -d '{"target":"strip","index":0,"param":"label","value":"Gesang"}'
curl -X POST http://127.0.0.1:8770/api/set \
     -d '{"target":"bus","index":0,"param":"label","value":"Monitor"}'
```

In der Oberfläche genügt ein Doppelklick auf den Namen im Kanalzug. Der Name
steht im Preset (`label` je Streifen/Bus) und kommt beim Wiederherstellen zurück.
Erlaubt sind bis zu 24 Zeichen ohne Steuerzeichen.

## Flatpak

Einmalig die Bauwerkzeuge (auf Bazzite ohne `flatpak-builder` im System):

```bash
flatpak install --user flathub org.flatpak.Builder
flatpak install --system flathub org.gnome.Sdk//51
```

Das SDK muss **systemweit** liegen: `flatpak-builder` läuft selbst im Builder-Sandbox
und fragt die SDKs über `flatpak info` in der Systeminstallation ab – ein
`--user`-SDK ist für ihn unsichtbar (`error: org.gnome.Sdk/x86_64/51 not installed`).
Die Runtime `org.gnome.Platform//51` bringt WebKitGTK mit, der GNOME-SDK zusätzlich
den Rust-Toolchain unter `/usr/lib/sdk/rust-stable` (keine rust-stable-Extension nötig).

Dann bauen und starten:

```bash
flatpak run --command=flatpak-builder org.flatpak.Builder \
    --user --install --force-clean --disable-rofiles-fuse \
    build-dir flatpak/de.weppelmann.Crosspoint.yml
flatpak run de.weppelmann.Crosspoint
```

`--disable-rofiles-fuse` ist im Builder-Flatpak nötig (kein `/dev/fuse` im Sandbox).
Das Manifest baut ausschließlich aus diesem Repo (nur path-Abhängigkeiten, daher
`cargo build --offline`) und installiert Engine, Startskript, Icon-Satz
(`/app/share/icons/hicolor/<n>x<n>/apps/`) und Desktop-Eintrag
(`/app/share/applications`). Ein zweites Modul (`fenster/`) baut
`crosspoint-fenster`, das GTK3-Fenster mit WebKitGTK.

Finish-Args und warum sie da sind:

| Recht | Grund |
|---|---|
| `--socket=pulseaudio` | `pactl` legt die virtuellen Knoten (`CROSSPOINT-Input-*`, `CROSSPOINT-Output-*`) an |
| `--filesystem=xdg-run/pipewire-0` | nativer PipeWire-Zugriff für JACK-Client und `pw-link`/`pw-dump` |
| `--share=ipc`, `--share=network` | Steuer-API auf `127.0.0.1:8770` bleibt von außen erreichbar (VBAN, Steuerung) |
| `--socket=wayland`, `--socket=fallback-x11` | das Fenster ist ein Wayland-Client, X11 als Rückfall |
| `--device=dri` | WebKit zeichnet die Oberfläche beschleunigt |
| `--socket=session-bus` | Tray-Icon (StatusNotifierItem + dbusmenu) am Sitzungsbus |
| `--filesystem=~/.config/crosspoint:create` | Konfiguration/Presets |
| `--filesystem=xdg-config/autostart:create` | Autostart-Datei in `~/.config/autostart` schreiben/entfernen |

Die Oberfläche ist im Flatpak ein eigenes Fenster (`crosspoint-fenster`, GTK3 +
WebKitGTK, 1760×980, Titel `Crosspoint`), das dieselbe Seite von
`http://127.0.0.1:8770/` lädt. Ein Browser – im Sandbox oder auf dem Host – ist
dafür nicht mehr nötig; `flatpak-spawn` und `--talk-name=org.freedesktop.Flatpak`
sind entfallen. Auf dem Host startet `scripts/crosspoint-launch.sh` weiterhin den
Browser der Wahl; im Flatpak übernimmt `scripts/crosspoint-start.sh` (Engine →
auf HTTP 200 warten → Fenster → nach Fensterschluss Engine und Geräte aufräumen).

Die Engine linkt im Sandbox gegen das `libjack.so.0` des GNOME-Runtime
(`/usr/lib/x86_64-linux-gnu/libjack.so.0`, pipewire-jack) – es ist dort
vorhanden, ein Rückfall auf PulseAudio ist nicht nötig.

## Grenzen (ehrlich)

- **DSP-Koeffizienten sind nicht aus dem Original extrahiert.** Belegt sind
  Parameterliste, Render-Signatur und Grenzwerte, nicht die Innenrechnung. EQ (RBJ),
  Compressor, Gate, Limiter und Denoiser sind Standard-Implementierungen mit den
  Original-Parametern — klanglich nah, nicht bitgleich.
- Pitch/Formant ist im Modell vorhanden, aber **nicht** verarbeitet (kein Pitch-Shifter).
- Bus-Mix-Modi (Upmix/Downmix/Composite) sind als Modell vorhanden, aber nicht umgesetzt.
- Keine Latenz-Optimierung: die Engine erbt die PipeWire-Quantum (hier 1024 Frames ≈ 21 ms).
- Nicht getestet: PipeWire-Neustart im Betrieb, mehrere Instanzen, Ratenwechsel.
