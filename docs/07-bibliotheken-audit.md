# Bibliotheks-Audit: was haben wir selbst gebaut, was gehört einer Bibliothek?

Stand: 2026-10-29. Grundlage: `wc -l` über den Code (19 592 Zeilen gesamt) und
die Fehler der letzten Tage. Die Frage ist nicht „was könnte man ersetzen",
sondern **welcher Eigenbau hat uns Fehler gekostet, die eine Bibliothek nicht
hätte**.

## Kurzfassung

Der Eigenbau von Infrastruktur (JSON, HTTP/WebSocket, PipeWire-Zugriff, WAV,
DBus) war eine Folge der Regel „keine externen Crates" aus der ersten Stunde —
damit der Build ohne Netz läuft. Diese Regel kostet inzwischen mehr, als sie
spart: **drei der vier schwersten Fehler der letzten Tage lagen in Eigenbau-
Infrastruktur, nicht in der Mischpult-Logik.**

Die Produktlogik (Mischer-Modell, Routing-Matrix, DSP, VBAN-Semantik,
`VBVMR_*`-Nachbildung) bleibt unser Code — dafür gibt es nichts Fertiges.

## Eigenbau → Bibliothek

| Eigenbau | Umfang | Ersetzen durch | Warum (belegter Schmerz) |
|---|---|---|---|
| `src/http.rs` — HTTP-Server, WebSocket-Handshake, SHA-1/Base64 von Hand | 1 082 Z. | `axum` + `tokio-tungstenite` (oder `tiny_http` für den kleinen Fall) | WS-Handshake war buggy (Key aus kleingeschriebenen Headern gelesen), Pegel-Polling musste durch WS ersetzt werden, `Content-Length`/Keep-Alive selbst gebaut |
| JSON-Parser/Serializer (in mehreren Modulen) | ~400 Z. verteilt | `serde_json` | Antworten von `pw-dump` per Hand geparst; Feldfehler fallen erst zur Laufzeit auf |
| `src/pw.rs` + `devices.rs`/`inputs.rs` — Steuerung über `pactl`/`pw-link`-Textausgaben | 831 + 764 + 631 Z. | **`pipewire` (pipewire-rs, offizielle Bindings)** | Mono-Mikrofon nicht verbunden (`capture_FL` angenommen), Monitore als Geräte gezählt, Beschreibungen am Leerzeichen abgeschnitten, VBAN-Port blieb liegen |
| `src/jack.rs` — JACK-ABI von Hand deklariert | ~120 Z. | `jack` (RustAudio/rust-jack) | `unsafe`, keine Typprüfung, `clippy::not_unsafe_ptr_arg_deref` musste mit `unsafe fn` entschärft werden |
| `src/tape_io.rs` + `crates/tape` — SPSC-Ring und WAV-Schreiber | 785 + 456 Z. | `rtrb` (lock-freier Ring) + `hound` (WAV) | Ringpuffer und RIFF-Kopf sind gelöst; eigener Code hier ist Risiko ohne Erkenntnis |
| `fenster/fenster.c` — Tray als StatusNotifierItem über GDBus | ~420 Z. in C | `ksni` (Rust, SNI) — oder C behalten | funktioniert, ist aber viel DBus-Boilerplate; `ksni` ist die gepflegte Variante |
| Eigenbau-VBAN (`crates/vban` + `vban_io`) | 692 + 456 + 1 614 Z. | **behalten** | Spezifikationskonform und in beide Richtungen belegt (375 Pakete, 0 verworfen). `quiniouben/vban` nur als Gegenprobe für Randfälle |

## Bewusst behalten

- `src/engine.rs`, `src/dsp.rs`, `src/lib.rs` — Mischer, Routing-Matrix, EQ,
  Kompressor, Gate, C5-Limiter. Das ist das Produkt.
- `src/remote_api.rs` — `VBVMR_*`-Nachbildung, absichtlich an das Original
  angelehnt, damit bestehende Steuerungen funktionieren.
- `web/index.html` — die Oberfläche ist unser Entwurf; ein Framework würde den
  Original-Look nicht liefern.

## Was der Umstieg kostet

1. **Crates müssen in den Flatpak-Build.** Mit `flatpak-cargo-generator`
   (flatpak-builder-tools) werden die Abhängigkeiten als Quellen eingefroren —
   der Build bleibt reproduzierbar und offline, das ist der Standardweg.
2. **CI muss einmal online ziehen** (danach Cache).
3. **API-Änderungen**: `pipewire` ist asynchron (tokio) — die Geräte- und
   Eingangsschicht bekommt einen kleinen Runtime-Layer. Das ist der eigentliche
   Aufwand, nicht das Ersetzen der Dateien.
4. **Zwei Schritte, nicht einer**: erst JSON + PipeWire (größter Nutzen, keine
   Async-Umstellung nötig für `serde_json`), dann HTTP/WS und JACK.

## Empfohlene Reihenfolge

1. `serde_json` — klein, sofort Gewinn, keine Architekturänderung.
2. `pipewire` für Geräte, Ports und Links (ersetzt `pactl`/`pw-link`-Parsing).
3. `rtrb` + `hound` für Tape.
4. `axum`/`tonic`-frei: HTTP + WebSocket über eine etablierte Bibliothek.
5. `jack`-Crate statt handdeklariertem ABI (optional, funktioniert aktuell).
6. `ksni` nur, wenn das Tray ohnehin angefasst wird.
