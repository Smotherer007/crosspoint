# Abgleich gegen das Original (Parameter- und Funktionsstand)

Stand: 2026-10-28. Quelle der Original-Seite: `docs/02-signalmodell.md`
(Persistenz-Attribute aus `voicemeeter8x64.exe` 3.1.3.0). Quelle meiner Seite:
`src/engine.rs` (`set_strip`, `set_strip_ext`, `set_bus`) und `src/lib.rs`.

Legende: **✔** vorhanden · **~** teilweise/anders benannt · **✘** fehlt

## Streifen (8)

| Original | bei mir | Stand |
|---|---|---|
| `dblevel` (Fader) | `gain` | ✔ |
| `dblimit` | im Modell, nicht setzbar | ~ |
| `mute`, `solo`, `mono` | gleich | ✔ |
| `muc` (Center-Mute) | – | ✘ |
| `karaoke`, `stripmode`, `greenmode` | – | ✘ |
| `prg_c`, `prg_g`, `prg_d` (FX-Presets je Modul) | – | ✘ |
| `busa, busa2..busa5, busb, busb2, busb3` | `send0..send7` | ✔ (anders benannt) |
| `layer1..layer8` | im Modell, ohne Wirkung | ~ |
| `paneltype`, `Panel3Dx/y`, `ColorPanel*`, `ModPanel*` (Intellipan/Voice Color) | – | ✘ |
| `audibility_c`, `audibility_g`, `denoiser_g`, `dn_bypass`, `dn_learn` | `denoiser` (an/aus, floor, threshold) | ~ |
| `EQGain1..3` (Schnellzugriff) | – (dafür 6 Zellen einzeln) | ~ |
| EQ-Zellen: `cell`, `EQtype`, `freq`, `Q`, `dblevel` | `eq<zelle>_on/_type/_freq/_q/_gain` | ✔ |
| `sendR/postR`, `sendD/postD`, `sendFx1/postFx1`, `sendFx2/postFx2` | – | ✘ |
| Comp: `gainin, attack, release, knee, comprate, threshold, automakeup, gainout` | `comp_threshold/_ratio/_attack/_release/_knee`; `gainin`, `automakeup`, `gainout` fehlen | ~ |
| Gate: `thresin, damping, bpsidechain, attack, hold, release` | `gate_threshold/_attack/_hold/_release`; `damping`, `bpsidechain` fehlen | ~ |
| Pitch: `pitchon, drywet, pitchvalue, formantlo/med/high` | nur im Modell | ✘ |

## Busse (8)

| Original | bei mir | Stand |
|---|---|---|
| `dblevel` | `gain` | ✔ |
| `dblimit` | im Modell, nicht setzbar | ~ |
| `mute`, `mono`, `eq`, `limiter` | gleich | ✔ |
| `BusMode` (Normal, Downmix A/B, Stereo-Rep, Composite, TV-Mix, Upmix 2.1/4.1/6.1) | in Arbeit (Subagent, Parameter `mode`) | ~ |
| `cross` | in Arbeit | ~ |
| `SEL`, `monitor` | – | ✘ |
| `trim`, `delay` (je Kanal) | `trim`, `delay` | ✔ |
| `retR, retD, retFx1, retFx2` (Returns) | – | ✘ |
| `greenmode` | – | ✘ |
| `vaio` (Bus als virtuelles Gerät) | – | ✘ |
| C5-Band: `attack, release, knee, ratio, threshold, gainOut, limOn, limit, distorsion, chain, mute, solo` | `c5`, `c5_threshold`, `c5_ceiling` (Bänder intern fix) | ~ |

## Was über die Parameter hinaus fehlt

- **Tape**: Original nimmt auf A1–B3 auf, kann abspielen und MP3; ich nur Bus A1 als WAV (Mehrspur-Aufnahme in Arbeit).
- **VBAN**: Original sendet *und* empfängt (Streams in Streifen), dazu Chat und VBAN2MIDI; ich nur Senden von Bus A1.
- **Remote-API**: Original 31 Exporte (`VBVMR_*`) über Shared Memory; bei mir entsteht eine Rust-Nachbildung (Work in progress), kein DLL-ABI.
- **Geräteebene**: Original WDM/KS/WASAPI/ASIO mit eigenem Treiber und Puffergrößen-Wahl; bei mir PipeWire-Knoten (Sinks für A1–A5, Quellen für Streifen 1–5), Puffergröße kommt von PipeWire.

## Fazit

Der Kern (Routing, Pegel, EQ, Dynamik, Limiter, Aufnahme, Geräte, VBAN-Ausgang,
Macros) steht und ist gemessen. Für echte Parität fehlen vor allem:
Intellipan/Pitch, die Sends R/D/FX1/FX2, Bus-Returns, `SEL`/`monitor`, die
FX-Preset-Programme (`prg_*`), VBAN-Empfang und das DLL-ABI der Remote-API.
