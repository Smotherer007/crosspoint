# tape

Tape-Recorder fuer den Mixer: schreibt interleaved f32-Audiobloecke als WAV.

Eigenstaendiges Crate (Rust 2021, **keine** externen Abhaengigkeiten, keine
Typen aus `vm8`). Anbindung spaeter ausschliesslich ueber primitive Typen.

| Eigenschaft | Umsetzung |
| --- | --- |
| Formate | `Format::Pcm24` (Format-Tag 1) und `Format::Float32` (Format-Tag 3) |
| Zielpfad | `<stamm>-YYYYMMDD-HHMMSS-<segment>.wav`, Verzeichnis wird angelegt |
| Rotation | bei `Config::max_file_bytes` (Standard 512 MiB), jedes Segment eigener gueltiger Kopf |
| Header | RIFF-Kopf wird beim Oeffnen als Platzhalter geschrieben und bei `stop`/Rotation per `seek(0)` gepatcht (inkl. Pad-Byte bei ungerader Nutzlast) |
| Puffer | vorallokierter Byte-Puffer + `BufWriter<File>`; `write_block` allokiert nichts (per Test belegt) |
| Zustandsmaschine | `Idle` -> `Recording` -> `Stopped`, `TapeError::WrongState` bei Fehlbedienung |
| Metadaten | `RecordingMeta { sample_rate, channels, format, start_epoch_s, start_time_iso, frames, peak, dateien }`, optional als JSON-Sidecar |

## API

```rust
use tape::{Format, Tape};

let mut tape = Tape::new();
tape.start("aufnahmen/take", 48_000, 2, Format::Pcm24)?;
tape.write_block(&block)?;   // f32 interleaved, beliebige Laenge
let meta = tape.stop()?;     // Kopf finalisieren, Metadaten zurueck
```

## Tests

```sh
cargo test          # 13 Unit- + 14 Integrationstests + 1 Doc-Test
```

Abgedeckt: Kopf-Groessen/Format-Tag/Block-Align, Frame-Zahl == erwartet,
Rotation (inkl. realistischer 1-MiB-Segmente und krummer Blockgroessen),
Header-Gueltigkeit nach `stop` auch gegenpruefen mit `file(1)`,
Pad-Byte bei ungerader Nutzlast, Zustandsmaschine, Metadaten, sowie ein
Allokationszaehler, der 0 Allokationen im Schreib-Hotpath nachweist.
