# Referenzprogramm – Signal- und Parametermodell (aus dem Binary)

Stand: 2026-10-28. Quelle: Statik aus `voicemeeter8x64.exe` 3.1.3.0
(PE32+ x64, `.text` 1 349 358 Byte, `.rsrc` 11,7 MB, SHA-256 der EXE
`7f783f03599f704124bedb1e13e8d8a6e91ab58fd74dc37d675f3a718acb81eb`).
**Noch keine Ghidra-Tiefenanalyse** — das hier sind die belegten Fakten aus
Importen, Strings und der mitgelieferten Persistenz-Syntax.

## 1. Abhängigkeiten des Mixers

`WINMM.dll` (waveIn/waveOut = MME), `SETUPAPI.dll` (Geräte-Enumeration),
`WS2_32.dll` (Sockets → VBAN), `WININET.dll`, `VERSION.dll`, `MSACM32.dll`
(ACM-Codecs für MP3-Aufnahme), `ole32`/`ADVAPI32`/`USER32`/`GDI32`/`SHELL32`.
Kein `ksuser.dll`, kein `Avrt.dll` im Hauptprogramm — Kernstreaming und
Realtime-Priorität laufen über die Treiber- und ASIO-Schicht, nicht über KS-API
im Mixer selbst.

## 2. Parametername-Schema (Persistenz)

Alle Einstellungen werden als XML-Attribute geschrieben; die Attributnamen sind
das vollständige Parametermodell. Auszug, wörtlich aus dem Binary:

```
<Strip index='%i' layout='%i' mute='%i' vaio='%i' solo='%i' mono='%i' muc='%i'
       EQon='%i' prg_c='%i' prg_g='%i' prg_d='%i'
       busa='%i' busa2='%i' busa3='%i' busa4='%i' busa5='%i'
       busb='%i' busb2='%i' busb3='%i'
       greenmode='%i' karaoke='%i' stripmode='%i'
       dblevel='%0.2f' dblimit='%0.2f'
       layer1..layer8='%0.2f' />

<Strip index='%i' channel='%i' cell='%i' EQon='%i' EQtype='%i'
       dblevel='%0.2f' freq='%0.2f' Q='%0.2f' />          (6 Zellen je Kanal)

<StripComp index='%i' gainin='%0.2f' attack='%0.1f' release='%0.1f' knee='%0.2f'
       comprate='%0.3f' threshold='%0.2f' automakeup='%i' gainout='%0.2f' />
<StripGate index='%i' thresin='%0.2f' damping='%0.2f' bpsidechain='%0.0f'
       attack='%0.1f' hold='%0.1f' release='%0.1f' />
<StripPitch index='%i' pitchon='%i' drywet='%0.2f' pitchvalue='%0.2f'
       formantlo='%0.2f' formantmed='%0.2f' formanthigh='%0.2f' />
<Strip index='%i' sendR='%0.2f' postR='%i' sendD='%0.2f' postD='%i'
       sendFx1='%0.2f' postFx1='%i' sendFx2='%0.2f' postFx2='%i' />
<Strip index='%i' EQGain1='%0.2f' EQGain2='%0.2f' EQGain3='%0.2f'
       Panel3Dx='%0.3f' Panel3Dy='%0.3f' />
<Strip index='%i' audibility_c='%0.2f' audibility_g='%0.2f'
       denoiser_g='%0.2f' dn_bypass='%i' dn_learn='%i' />

<Bus index='%i' mute='%i' vaio='%i' mono='%i' cross='%i' BusMode='%i'
      EQon='%i' SEL='%i' monitor='%i' dblevel='%0.2f' greenmode='%i'
      dblimit='%0.2f' />
<Bus index='%i' channel='%i' cell='%i' EQon='%i' EQtype='%i'
      dblevel='%0.2f' freq='%0.2f' Q='%0.2f' />
<Bus index='%i' channel='%i' trim='%0.2f' delay='%0.2f' />
<Bus index='%i' retR='%0.2f' retD='%0.2f' retFx1='%0.2f' retFx2='%0.2f' />
```

Daraus folgt der Aufbau, den ein Nachbau abbilden muss:

| Block | Umfang |
|---|---|
| Streifen | 8 (`index 0..7`), je `dblevel` (Fader), `dblimit`, `mute`, `solo`, `mono`, `muc`, `karaoke`, `stripmode`, `greenmode` |
| Routing | 8 Ziele je Streifen: `busa, busa2..busa5, busb, busb2, busb3` (Schalter) |
| Streifen-EQ | je Kanal 6 Zellen (`cell 0..5`) mit `EQtype`, `freq`, `Q`, `dblevel` — dazu 3 Schnellzugriffe `EQGain1..3` |
| Compressor | `gainin, attack, release, knee, comprate, threshold, automakeup, gainout` |
| Gate | `thresin, damping, bpsidechain, attack, hold, release` |
| Denoiser | `denoiser_g, dn_bypass, dn_learn` (+ `audibility_c/g` = Erkennungs-Modus) |
| Pitch | `pitchon, drywet, pitchvalue, formantlo/med/high` |
| Busse | 8 (`a,b,b2,b3` virtuelle, `A1..A5` Hardware): `dblevel, dblimit, mute, mono, cross, BusMode, SEL, monitor, EQon, greenmode` |
| Bus-EQ | je Kanal Zellen mit `EQtype/freq/Q/dblevel`, zusätzlich `trim` und `delay` pro Kanal |
| Bus-Returns | `retR (Reverb), retD (Delay), retFx1, retFx2` |
| Layer | `layer1..layer8` je Streifen (8-Band-Pegel/Gain-Set) |

## 3. FX-Kette (Namen aus dem Binary)

- Strip: Gate → Denoiser → 6-Band-EQ → Compressor → Pitch/Formant → 4 Sends
  (`R`, `D`, `FX1`, `FX2`, je mit `post`-Schalter) → Routing-Matrix.
- Bus: EQ → `C5-Limiter (MultiBand Compressor)` — eigener Dialog, je Band
  `gain, compOn, attack, release, knee, ratio, threshold, gainOut, limOn,
  limit, distorsion, chain, mute, solo` (`<C5LimBand index='%i' …>`).
- `greenmode` bei Streifen und Bus: reduzierter Rechenpfad.
- Es gibt keine frei ladbaren Plugins: FX1/FX2 sind feste Slots (`VoicemeeterBUSGEQ15.exe`,
  `VoicemeeterBUSMatrix8.exe` sind getrennte APAs, keine Hosts).

## 4. Audio-Format und Grenzen

- Unterstützte Raten: 32 kHz bis 192 kHz, `44100`, `48000`, `192000` explizit genannt;
  ASIO-Ausgänge nur mit „standard SampleRate";
  „Buffer size cannot be above 2048 samples", Default 441 Samples (10 ms @ 44,1 kHz).
- Bus-Mix-Modi (`BusMode`): Normal, Downmix A/B, Stereo-Rep, Composite, TV-Mix,
  Upmix 2.1/4.1/6.1; Kanal-Routing einzeln (`Bus_L/R/C/LFE/Rear`).
- Pegel: `dblevel` = Fader in dB, `dblimit` = Limiter-Schwelle.

## 5. VBAN

Sockets über `WS2_32`, eigener UDP-Thread („Failed to Create UDP Thread"),
konfigurierbarer Port, Stream-Namen, In/Out/Frame-Pakete sowie ein Chat-Kanal
(`VBANChat_*`) und `VBAN2MIDI.exe`. Paketlayout nicht rekonstruiert — dafür ist die
öffentliche VBAN-Protokollbeschreibung die richtige Quelle, nicht das Binary.

## 6. Was noch fehlt (nächster Schritt)

- Ghidra-Tiefenanalyse der EXE: DSP-Innenleben (Biquad-Koeffizienten, Kompressor-
  Kennlinie, Limiter-Verhalten), Sample-Genauigkeit der Mischmatrix, Reihenfolge
  der FX-Blöcke im Code verifizieren.
- Lizenz-/Registrierungsprüfung: wird **nicht** analysiert und nicht umgangen.
