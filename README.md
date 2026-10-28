# Crosspoint

8×8-Mixer für PipeWire/JACK in Rust: 8 Streifen, 8 Busse (A1–A5 Hardware, B1–B3
virtuell), ohne Kernelmodul und ohne Fremd-Crates. Die Engine ist ein
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
| Tests | `cargo test --release --workspace`: 124 im Hauptcrate, 94 in den path-Crates, alle grün |

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

## Steuer-API

```bash
curl -X POST http://127.0.0.1:8770/api/set \
     -d '{"target":"strip","index":0,"param":"gain","value":-12}'
curl http://127.0.0.1:8770/api/state     # kompletter Zustand
curl http://127.0.0.1:8770/api/levels    # Pegel in dB
curl -X POST http://127.0.0.1:8770/api/preset -d '{"action":"save"}'
```

Parameter: `gain`, `pan`, `mute`, `solo`, `mono`, `eq`, `comp`, `gate`, `denoiser`,
`send0..send7`, `eq<zelle>_on|type|freq|q|gain`, `comp_threshold|ratio|attack|release|knee`,
`gate_threshold|attack|hold|release`; Busse: `gain`, `mute`, `mono`, `eq`, `limiter`,
`trim`, `delay`. Die Engine hat keine Authentifizierung und lauscht nur auf `127.0.0.1`.

## Flatpak

Einmalig die Bauwerkzeuge (auf Bazzite ohne `flatpak-builder` im System):

```bash
flatpak install --user flathub org.flatpak.Builder \
    org.freedesktop.Sdk//25.08 org.freedesktop.Sdk.Extension.rust-stable//25.08
```

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
(`/app/share/applications`).

Finish-Args und warum sie da sind:

| Recht | Grund |
|---|---|
| `--socket=pulseaudio` | `pactl` legt die virtuellen Knoten (`CROSSPOINT-Input-*`, `CROSSPOINT-Output-*`) an |
| `--filesystem=xdg-run/pipewire-0` | nativer PipeWire-Zugriff für JACK-Client und `pw-link`/`pw-dump` |
| `--share=ipc`, `--share=network` | Steuer-API auf `127.0.0.1:8770` muss vom Host-Browser erreichbar sein |
| `--filesystem=~/.config/crosspoint:create` | Konfiguration/Presets |
| `--talk-name=org.freedesktop.Flatpak` | das Fenster öffnet der Host-Browser (`flatpak-spawn --host`) |

Die Engine linkt im Sandbox gegen das `libjack.so.0` des Freedesktop-Runtime
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
