# vm8-linux

Nachbau der VoiceMeeter-Signalkette in Rust: 8 Streifen × 8 Busse, auf PipeWire/JACK,
ohne Kernelmodul und ohne Fremd-Crates. Kein Code, keine Ressourcen, keine Strings aus
dem Original.

## Signalkette

```
Streifen 1..8:  Eingang → 6-Band-EQ → Compressor → Gate → Denoiser → Gain → Pan → Routing
Busse A1..A5 / B1..B3:  Summe → Limiter → Delay je Kanal → Trim → Gain/Mute → Ausgang
```

Herleitung und Belege: `docs/01-installer-analyse.md` (Paketformat),
`docs/02-signalmodell.md` (Parameternamen aus dem Binary),
`docs/03-ghidra-app.md` (Render-Pfad, Grenzwerte, Geräteebene).

## Stand (2026-10-28)

| Teil | Zustand |
|---|---|
| Ghidra-Analyse der Anwendung | 2128 Prozeduren, Render-Pfad und Grenzwerte belegt |
| Parametermodell (`src/lib.rs`) | vollständig, 8 Streifen/8 Busse, EQ-Zellen, Comp, Gate, Denoiser, Pitch, Sends, Layer, Bus-Modi |
| Echtzeit-Kern (`src/engine.rs`) | 8×8-Matrix, EQ, Comp, Gate, Denoiser, Limiter, Delay, Pegel |
| DSP-Bausteine (`src/dsp.rs`) | RBJ-Biquads, Kompressor, Gate, Limiter, Delay |
| Audio-I/O (`src/jack.rs`) | JACK-ABI über PipeWire, ohne `-devel`-Pakete |
| Steuer-API + UI (`src/http.rs`, `web/`) | JSON-API, Web-Oberfläche, Presets |
| Tests | `cargo test` 18/18, `tests/e2e.py` 5/5 akustisch |

Gemessen (nicht behauptet): 1-kHz-Sinus −6 dBFS durch Streifen 1 → Bus A1 ergibt
−6,00 dBFS; Gain −12 dB → −18,00 dBFS; Mute → Stille; Routing A1→A2 wandert;
EQ-Boost +12 dB bei 1 kHz → exakt +12 dB.

## Bauen und starten

Voraussetzungen: `pipewire`, `pipewire-jack`, `pactl`, `cargo`. Keine `-devel`-Pakete nötig —
`build.rs` linkt gegen `/usr/lib64/pipewire-0.3/jack/libjack.so.0`.

```bash
cargo build --release
./target/release/vm8-engine                 # UI auf http://127.0.0.1:8770/
VM8_A1=<pipewire-sink> ./scripts/vm8-wire.sh up
```

`vm8-wire.sh up` legt `vm8_in_1..3` (Anwendungen spielen hinein → Streifen 6..8) und
`vm8_out_B1..3` (Anwendungen nehmen auf ← Busse B1..B3) als `null-sink` an. Die
Hardware-Busse A1..A5 kommen über `VM8_A1` … `VM8_A5`.

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

## Grenzen (ehrlich)

- **DSP-Koeffizienten sind nicht aus dem Original extrahiert.** Die Analyse belegt
  Parameterliste, Render-Signatur und Grenzwerte, nicht die Innenrechnung. EQ (RBJ),
  Compressor, Gate, Limiter und Denoiser sind Standard-Implementierungen mit den
  Original-Parametern — klanglich nah, nicht bitgleich.
- Pitch/Formant ist im Modell vorhanden, aber **nicht** verarbeitet (kein Pitch-Shifter).
- Bus-Mix-Modi (Upmix/Downmix/Composite) sind als Modell vorhanden, aber nicht umgesetzt.
- VBAN und Tape-Recorder fehlen vollständig.
- Keine Latenz-Optimierung: die Engine erbt die PipeWire-Quantum (hier 1024 Frames ≈ 21 ms).
- Nicht getestet: PipeWire-Neustart im Betrieb, mehrere Instanzen, Ratenwechsel.
