# Infrastruktur auf Bibliotheken umgestellt

Stand: 2026-10-29. Grundlage: `docs/07-bibliotheken-audit.md` (welche Eigenbauten
haben Fehler gekostet) und `docs/TODO.md`. Umgesetzt ist die Reihenfolge aus dem
Audit: **JSON → Tape → HTTP/WebSocket → JACK → OSC/FX → Flatpak**.

Nach jedem Schritt galt: `cargo build --release` und `cargo test --release`
grün, Tests angepasst statt gelöscht.

## Kurzfassung

| Eigenbau vorher | jetzt | Zeilen vorher → nachher |
|---|---|---|
| `src/json.rs` (Parser + Serialisierer, ~270 Z.) | `serde` + `serde_json` | 273 → 291 (davon ~130 Testzeilen) |
| `src/tape_io.rs` SPSC-Ring (eigene `head`/`tail`-Zähler) | `rtrb` | 785 → 969 |
| `crates/tape` WAV-Kopf + Sample-Kodierung | `hound` | 208 + 456 → 197 + 433 |
| `src/http.rs` HTTP-Server, WS-Handshake, SHA-1/Base64, Frames | `tiny_http` + `tungstenite` | 1 082 → 1 171 |
| `src/jack.rs` handdeklariertes ABI | `jack` (RustAudio/rust-jack) | 152 → 90 |

Die Produktlogik (`engine.rs`, `dsp.rs`, `remote_api.rs`, `vban_io.rs`,
`crates/vban`) ist unverändert.

## Abhängigkeiten (`Cargo.toml`)

```
+serde = { version = "1", features = ["derive"] }   # JSON, strukturiert lesen
+serde_json = "1"                                    # JSON lesen/schreiben
+rtrb = "0.4"                                        # SPSC-Ring (Tape-Brücke)
+tiny_http = "0.12"                                  # HTTP, synchron
+tungstenite = "0.30"                                # WebSocket-Frames + Handshake
+jack = "0.13"                                       # JACK-Client, lädt libjack.so.0
```
`crates/tape` bekommt genau eine Abhängigkeit: `hound = "3.5"`.

54 Pakete im `Cargo.lock` (vorher 4 path-Crates), 50 davon eingefroren in
`flatpak/new-sources.json`.

## 1. JSON: `serde_json` statt Eigenbau

* `src/json.rs` behält die **Form** der Werte (`Value` mit geordneter Paarliste),
  weil die Feldreihenfolge Teil der öffentlichen API ist. Gelesen wird über
  `serde_json::from_str`, geschrieben über `Serialize` + eigener `Formatter`
  für Zahlen (`0.700`, `-140`, `1000` – wie die Oberfläche es seit v0.1 liest).
  `serde_json::Map` hätte die Felder alphabetisch sortiert, deshalb keine
  Umwandlung nach `serde_json::Value` beim Schreiben.
* Alle Aufrufer (`engine.rs`, `devices.rs`, `pw.rs`, `http.rs`, `restore.rs`,
  `inputs.rs`, `autostart.rs`, `remote_api.rs`, `vban_io.rs`) blieben
  unverändert – sie benutzen weiter `crate::json::Value`.
* Neue Tests: Zahlenformat, Feldreihenfolge, Maskierung, `pw-dump`-Ausschnitt,
  Fehlertexte mit Zeile/Spalte, `derive` (struct `Beispiel`).

**API-Vergleich** (siehe unten): `/api/state`, `/api/levels`, `/api/devices`,
`/api/vban`, `/ws/levels` zeichengleich.

## 2. Tape: `rtrb` + `hound`

* `Ring` und `MultiRing` liegen auf `rtrb::RingBuffer`. Zwei Ringe je Strecke:
  ein Kopf-Ring mit **genau** `slots` Plätzen (Blockgrenzen, Kanalzahl) und ein
  `f32`-Ring für die Samples. Der Produzent schreibt erst Samples, dann den
  Kopf; der Konsument liest erst den Kopf. Ein Kopf ist damit nie ohne seine
  Samples sichtbar, und die Blockzahl bleibt exakt `slots` – unabhängig davon,
  wie `rtrb` seinen Puffer ausrichtet.
* Der Audio-Callback bleibt allokations- und lockfrei: `write_chunk_uninit` +
  `fill_from_iter` (memcpy) in den vorallokierten Ring, kein `Vec`, kein Mutex.
  Der Test `write_block_allokiert_nicht` zählt weiter Allocationen (0).
* WAV schreibt `hound` (`WavWriter`): RIFF-Kopf, Größen-Patch beim `finalize`,
  Rotation und Metadaten bleiben unser Code. `write_block` schreibt Sample für
  Sample in einen `BufWriter` – ebenfalls ohne Allokation.
* **Format-Änderung (bewusst):** `hound` schreibt bei mehr als 16 bit (also
  immer bei uns: 24 bit und float32) `WAVE_FORMAT_EXTENSIBLE` – 40-Byte-
  `fmt`-Chunk, `data` ab Byte 68, Format-Tag 0xFFFE mit SubFormat-GUID.
  Nebeneffekt: `hound` schreibt kein Füllbyte hinter eine ungerade Nutzlast;
  das trifft nur Mono-24-bit (ungerade Byte-Zahl), unsere Aufnahmen sind immer
  ≥ 2 Kanäle. Dateinamen, Rotation, Sidecar-Metadaten und Peak-Messung sind
  unverändert. `tests/wav_check.py` liest den Kopf deshalb selbst (Pythons
  `wave` kennt EXTENSIBLE nicht).

**Messung 1 kHz** (`tape_io::tests::funktionstest_1khz_pegel_und_frequenz`):
Sinus 1 kHz, Amplitude 0,5 durch Ring + Schreiber → `Peak 4194304 =
-6.02 dBFS, 998 Hz` (Nulldurchgänge über 0,5 s).

**Messung Ende-zu-Ende** (`tools/e2e_1khz.sh`, Engine + PipeWire + `pw-cat`):

| | vorher (Referenz-Binary) | nachher |
|---|---|---|
| Frames | 143 360 | 145 408 |
| Pegel | −6,02 dBFS | −6,02 dBFS |
| Frequenz | 999,8 Hz | 999,8 Hz |
| Kopf | `PCM`, 44 Byte | `PCM (EXTENSIBLE)`, 68 Byte |

Der Unterschied in der Framezahl sind zwei Blöcke Vorlauf (2 × 1024), die der
alte Ring beim Start verworfen hat – der neue schreibt sie mit.

## 3. HTTP: `tiny_http` + `tungstenite`

* `serve()` nimmt `tiny_http::Server`, ein Thread je Anfrage (wie vorher).
  `route()` ist eine **reine Funktion** aus Methode, Pfad und Rumpf auf eine
  `Antwort`; die Tests prüfen die Wege ohne Socket, zwei Tests laufen über
  einen echten Socket (HTTP-Antwort und WebSocket-Handshake + Text-Frame).
* WebSocket: `derive_accept_key` (RFC 6455, rechnet `tungstenite`) ersetzt
  SHA-1/Base64 von Hand; `Request::upgrade("websocket", …)` +
  `WebSocket::from_raw_socket`. Der Frame ist derselbe Text-Frame wie vorher
  (`0x81`, JSON-Rumpf), gesendet wird weiter nur bei neuen Pegeln (60-Hz-Takt
  am Frame-Zähler).
* Gelesen wird auf dem Pegel-Socket nicht: die Oberfläche schickt dort nichts.
  Ein geschlossener Browser fällt über den Schreibfehler auf; das hält den
  Takt frei von blockierenden Leseversuchen.
* Kopfzeilen: `Content-Type`, `Content-Length`, `Cache-Control: no-store`
  unverändert; `Connection: close` entfällt (tiny_http hält die Verbindung
  offen, HTTP/1.1-Standard).

## 4. JACK: Crate `jack`

* `src/jack.rs` ist jetzt eine schmale Hülle um `jack::{Client, Port<AudioIn>,
  Port<AudioOut>, ProcessScope}`: gleiche Portnamen (`strip1_L`, `busA1_L`,
  `vban_L` …), gleicher Prozess-Callback je Block, keine Allokation darin.
* `jack-sys` lädt `libjack.so.0` **zur Laufzeit** (`dynamic_loading`). Beim
  Bauen fragt es trotzdem pkg-config nach `jack`; dafür liegt
  `packaging/pkgconfig/jack.pc` im Repo, bekannt gemacht über
  `.cargo/config.toml` (`[env] PKG_CONFIG_PATH`). `build.rs` linkt nichts mehr.
* Geprüft: Client verbindet (`48000 Hz, Block 1024`), 16 Eingangs- und
  18 Ausgangsports wie vorher, Frame-Zähler läuft (~47 000 Frames/s), und die
  Engine läuft **im Flatpak-Sandkasten** (dort kommt `libjack.so.0` aus der
  GNOME-Runtime).

## 5. OSC und FX eingebunden

`src/osc.rs` und `src/fx.rs` sind als `pub mod` eingetragen; in `main.rs` nach
`wire("up")`:

```rust
let osc_halt = crosspoint::osc::spawn(params.clone());
crosspoint::fx::spawn(params.clone());
```

Beleg: Engine-Log `OSC-Eingang auf 127.0.0.1:9000`, ein Paket
`/strip/0/gain -6` per UDP → `GET /api/state` zeigt `"gain":-6`. `fx: 0 Inserts
geladen …, mod-host Port 5556` und ohne mod-host `kein Effekt-Host erreichbar –
Inserts bleiben aus, der Mixer laeuft weiter`. Beim Beenden wird der OSC-Eingang
über `osc_halt.anhalten()` gestoppt.

## 6. Flatpak: Cargo-Quellen eingefroren

* `flatpak/new-sources.json` mit `flatpak-cargo-generator.py`
  (flatpak-builder-tools) aus `Cargo.lock`: 50 Archiv-Quellen mit SHA-256,
  entpackt nach `cargo/vendor/<name>-<version>` (Layout von `cargo vendor`).
* Das Manifest referenziert die Datei als Quellenliste **und** als Datei:
  flatpak-builder lädt jede Datei, prüft die Prüfsumme und entpackt sie; der
  Modulschritt `cargo-quellen` zählt die Verzeichnisse gegen die Liste
  (`set -eu`, Abbruch bei Abweichung) und meldet
  `Cargo-Quellen: 50 von 50 eingefrorenen Paketen entpackt`.
* Die Engine selbst wird weiter im CI gebaut (`prebuilt/crosspoint-engine`),
  weil die GNOME-Runtime keine Rust-Toolchain mitbringt. Ein Rust-Bau im
  Sandkasten würde dieselben Verzeichnisse als
  `[source.vendored-sources] directory = "vendor"` benutzen.

## Belege

### API-Antworten vorher/nachher

`tools/api_capture.py` schreibt je Endpunkt Rumpf und Kopf weg,
`tools/api_vergleich.py` vergleicht byte-genau (nur `"f":<n>` normalisiert).
Referenz-Binary = derselbe Quellstand mit altem `json.rs`/`http.rs`.

```
ABWEICHEND  post_set_bad  (25 -> 57 Byte)
      erste Abweichung bei Byte 10
      vorher : …{"error":"'\"' erwartet"}'
      nachher: …{"error":"EOF while parsing a value at line 1 column 18"}'

29 Endpunkte gleich, 1 abweichend, 0 fehlend, 0 neu
```

Gleich sind u. a. `/api/state`, `/api/levels`, `/api/devices`,
`/api/devices?monitors=1`, `/api/vban`, `/api/vban/recv`, `/api/vban/config`,
`/api/eq?strip=0`, `/api/inputs`, `/api/restore`, `/api/tape`, `/api/macros`,
`/api/autostart`, `/api/preset`, `/`, `ws_levels` sowie alle POST-Antworten.
Die einzige Abweichung ist der Fehlertext für **syntaktisch kaputtes** JSON:
`serde_json` nennt Zeile und Spalte statt „`'"` erwartet".

### Testzahlen

```
crosspoint  (Hauptcrate)   189   (vorher 138)
macros                      26
tape (lib)                  13
tape (tests/tape.rs)        14
vban (lib + integration)    40
doc-tests                    3
zusammen                   286   alle grün
```

### Flatpak

```
$ flatpak run --command=flatpak-builder org.flatpak.Builder --user --install \
    --force-clean --disable-rofiles-fuse build-dir flatpak/de.weppelmann.Crosspoint.yml
Cargo-Quellen: 50 von 50 eingefrorenen Paketen entpackt
Vendored-Sources: cargo/vendor
...
FLATPAK_EXIT=0

# ohne Netz (nur lokal vorhandene Quellen):
$ ... --disable-download ...
0 Downloads, OFFLINE_EXIT=0
```

Im Sandkasten geprüft: `flatpak run --command=crosspoint-engine … --help` und
ein echter Start (`Client 'crosspoint-fp' verbunden: 48000 Hz, Block 1024
Frames`, `/api/state` antwortet).

## Offene Punkte / bewusste Abweichungen

1. **Fehlertext bei kaputtem JSON** kommt von `serde_json` (Zeile/Spalte).
2. **WAV-Kopf** ist `WAVE_FORMAT_EXTENSIBLE` (siehe oben); `file(1)` nennt
   deshalb keine Bittiefe mehr, `ffprobe` liest `pcm_s24le` / `pcm_f32le`.
3. **`Connection: close`** entfällt (keep-alive).
4. **Pegel-WebSocket liest nicht**; Verbindungsende wird am Schreibfehler
   erkannt.
5. **`pactl`/`pw-link`-Textparsing** (`pw.rs`, `devices.rs`, `inputs.rs`) ist
   noch Eigenbau – der nächste Schritt aus dem Audit (pipewire-rs), hier nicht
   enthalten.
6. `crates/macros` hat weiter einen eigenen JSON-Leser (`Wert`); er gehört zum
   Makro-Format und war nicht Teil dieses Umbaus.
