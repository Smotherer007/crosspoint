# crates/ui-gtk — native GTK4-Oberflaeche (Phase 1)

Ein Prozess, ein Zustand: die Oberflaeche liest und schreibt `Arc<Params>` aus
`crosspoint::engine` direkt. Kein HTTP, kein JSON auf dem Bedienweg.

## Bauen

Eigenes Paket mit eigener `[workspace]`-Tabelle, damit es unabhaengig baubar ist.
GTK4-Header fehlen auf dem Host, deshalb im GNOME-SDK (GTK 4.24):

```
flatpak run --system --devel --filesystem=home --filesystem=host \
  --command=bash org.gnome.Sdk//51 -c '
    export PATH=/usr/lib/sdk/rust-stable/bin:$PATH
    export LIBCLANG_PATH=/run/host/usr/lib64
    export LD_LIBRARY_PATH=/run/host/usr/lib64
    export BINDGEN_EXTRA_CLANG_ARGS="-resource-dir /run/host/usr/lib/clang/22 -I/run/host/usr/lib/clang/22/include"
    cd <repo> && cargo build --manifest-path crates/ui-gtk/Cargo.toml'
```

- `gtk` = Crate `gtk4` 0.9.7 (Feature `v4_6`), GTK 4.24.1 im SDK.
- `LIBCLANG_PATH`/`BINDGEN_EXTRA_CLANG_ARGS`: `pipewire-sys` im Kern braucht
  bindgen; das SDK hat kein libclang, der Host schon.

## Starten

```
cargo run --manifest-path crates/ui-gtk/Cargo.toml
crosspoint-gtk --theme crosspoint-hell --layout breit
crosspoint-gtk --dialog midi|vban|darstellung
```

## Konfiguration

Eine Datei, zwei getrennte Modelle: `~/.config/crosspoint/ui.json`

```json
{
  "theme":  { "aktiv": "crosspoint-hell", "themes":  { "eigen": { "akzent": "#00ff88" } } },
  "layout": { "aktiv": "breit",           "layouts": { "eigen": { "dichte": "kompakt" } } }
}
```

- Themes: `crosspoint`, `crosspoint-hell`, `mono` (eingebaut).
- Layouts: `klassisch`, `breit`, `kompakt`, `nur-streifen`.
- Fehlende Felder fallen auf das Standard-Theme/die Vorgabe zurueck, unbekannte
  Namen auf `crosspoint`/`klassisch`, kaputte Dateien auf die Vorgaben - jeweils
  mit Logzeile, nie mit Absturz.
- Das CSS wird zur Laufzeit aus den Tokens gerendert (`GtkCssProvider::load_from_data`);
  ein Theme-Wechsel tauscht nur den Provider.

## Breiten

Die Spaltenbreiten kommen **aus der Fensterbreite**, nicht aus dem Inhalt
(`layout::spaltenbreiten`): Streifen 88..150 px, Busse 64..120 px, Master
96..140 px. Ein laenger Geraetename oder die Zuordnungstastenreihe darf das
Pult nicht mehr breiter machen - was nicht in die Spalte passt, wird gekuerzt
(Ellipse) und steht vollstaendig im Tooltip.

```
fest      = 2*8 Rand + 2*4 Blockrand + 8 Luecke + 2*4 Blockrand
          + 7*4 Streifenabstaende + 8*3 Busabstaende              = 92 px
benoetigt = fest + 8*streifen + 8*bus + master
```

Der Rest wird im Verhaeltnis 1,0 (Streifen) : 0,62 (Bus) : 0,85 (Master)
verteilt; faellt eine Spalte unter ihre Untergrenze oder ueber ihre Obergrenze,
wird sie geklemmt und der Rest neu verteilt. Das Pult braucht mindestens
**1404 px** (8*88 + 8*64 + 96 + 92). Ist das Fenster schmaler, bleiben die
Spalten auf ihren Mindestbreiten und der Inhalt **scrollt waagerecht**
(`GtkScrolledWindow`, Leiste sichtbar, keine Ueberlagerung) - gezeichnet wird
nie ausserhalb des Fensters.

| Fenster | Streifen | Busse | Master | benoetigt | Leiste |
|---|---|---|---|---|---|
| 1200 | 88 | 64 | 96 | 1404 | ja |
| 1400 | 88 | 64 | 96 | 1404 | ja |
| 1440 | 92 | 64 | 96 | 1436 | nein |
| 1600 | 109 | 67 | 96 | 1596 | nein |
| 1920 | 132 | 82 | 112 | 1916 | nein |
| 2560 | 150 | 120 | 140 | 2392 | nein |

Tabelle erzeugen: `cargo test --lib breiten_tabelle -- --ignored --nocapture`.

## Layout-Bereiche

`kopf`, `tape`, `streifen`, `zuordnung`, `busse`, `master`, `makros` - jeder
einzeln sichtbar/unsichtbar, Busspalte links/rechts, Master in der Busspalte /
eigene Spalte / rechts aussen, Dichte kompakt/normal/breit (aendert nur Masse und
Abstaende, **nie** die Schrift), Streifenreihenfolge als Liste.

## Belege

`../../docs/ui-gtk-*.png` - Hauptfenster, helles Theme, Layouts, Zuordnungstasten,
MIDI-Dialog, 1600x900 und 2560x1440 mit offenem MIDI-Dialog. Breiten-Belege:
`ui-gtk-1600x1000.png` (alles sichtbar, keine Leiste), `ui-gtk-1400x900.png`
(waagerechte Leiste), `ui-gtk-2560x1440-breiten.png` (breitere Streifen).

Screenshots ohne Bildschirm: `./screenshot.sh ziel.png [--theme …] [--layout …]`
(startet ein eigenes Xwayland auf :99 und liest das Fenster mit ImageMagick aus).

## Tests

`cargo test --manifest-path crates/ui-gtk/Cargo.toml` - 45 Tests: dB-Umrechnung,
Label-Kuerzung, Theme-Rundlauf/Fallback, Layout-Rundlauf/Nur-Streifen/Busseite/
Dichte/Streifenreihenfolge, Geraetezuordnung (leer = loeschen), Send-Umschaltung,
midimonster.cfg-Rundlauf und Reset.
