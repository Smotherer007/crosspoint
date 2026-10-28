# Referenzprogramm – Ghidra-Tiefenanalyse der Anwendung

Stand: 2026-10-28 · Ghidra 12.1.4 (headless über REA 6.5.0) ·
Ziel: `voicemeeter8x64.exe`, SHA-256 `7f783f03599f704124bedb1e13e8d8a6e91ab58fd74dc37d675f3a718acb81eb`

## 0. Umfang der Analyse

| Messwert | Wert |
|---|---|
| Image-Base | `0x140000000` |
| `.text` | `0x140001000`–`0x14014a400`, 1 348 608 Byte |
| Prozeduren | **2128** |
| Strings (Ghidra-definiert) | **3168** |
| PDB | `Voicemeeter8x64.pdb`, GUID `3c6dd96d-…` (nicht vorhanden → keine Symbolnamen) |

Beleg: `ev_34d69efa81811ac9b61d4ec73b2847d95fcdf2508949ec8a4e46f723b112e4a7`

Untersucht wurden gezielt die Startroutine der Audio-Engine, der MME-Ausgabepfad und
die Geräte-/Puffer-Verwaltung. **Nicht** untersucht: die DSP-Kerne (EQ, Kompressor,
Gate, Denoiser, Limiter) und die Lizenzprüfung.

## 1. Importprofil

Statisch importiert: `WINMM` (MME: `waveIn*`/`waveOut*`), `SETUPAPI` (Geräte-Enumeration),
`WS2_32` (VBAN), `ole32` (`CoInitialize`/`CoCreateInstance`), `ADVAPI32`, `SHELL32`,
`USER32`, `GDI32`, `COMDLG32`, `MSACM32` (ACM/MP3), `VERSION`, `WININET`.

Zur Laufzeit nachgeladen (Strings im Binary): `ksuser.dll` (Kernel-Streaming),
`Avrt.dll` (MMCSS-Thread-Priorität), `Mfplat.dll`/`Mfreadwrite.dll` (Media Foundation für
Datei-I/O), `psapi.dll` (`EnumProcessModules` für die Remote-API-Clientliste),
`iphlpapi.dll`, `Gdiplus.dll`, `SHFOLDER.DLL`.

**ASIO ist im Hauptprogramm nicht importiert** — die virtuellen ASIO-Geräte sind die
separaten `vbvm*_asiodriver{,64}.dll`, die über eine gemeinsame Speicherregion mit dem
Mixer reden.

## 2. Audio-Engine-Start (`FUN_1400f4080`, 0x1400f4080)

Pseudocode aus Ghidra (Beleg `ev_8a851629aeab00fddb8eeb265c51f091147b74058bcb5845be88294a19102422`).

Belegte Konstanten und Grenzen:

```
unterstützte Sampleraten (Vergleichskette im Code):
    32000, 0xAC44 = 44100, 48000, 0x15888 = 88200,
    96000, 0x2B110 = 176400, 0x2EE00 = 192000
    sonst  -> Rückgabe 0xFFFFFFEC (-20)  "Output A1 supports standard SampleRate only"

Puffergröße:    if (0x800 < block) -> Fehler   (0x800 = 2048 Frames)
                "Buffer size cannot be above 2048 samples."
Kanäle:         clamp(2 .. 8), Vorgabe 8   (Objektfeld +0x16DE)
Samplerate:     Objektfeld +0x30C5C
Blockgröße:     Objektfeld +0x618C
Geräte-Slots:   5 Ausgänge, je 0x398 Byte, Basis (obj + (i+0x32)*0x73)*8
Registry:       HKCU\VB-Audio\VoiceMeeter, Wert "AutoUpMixMode" (DWORD, 4 Byte)
VBAN:           Objektfeld +0x5D3FC; bei belegtem UDP-Port eigene Fehlermeldung
```

Produkt-Unterscheidung an `*(int*)(obj + 0x31224)`: `0` → Gerätenamen
`VBAudioVMVAIO` / `VBAudioVMAUXVAIO` / `VBAudio100VMVAIO3` (Standard/Banana),
sonst `VBVoicemeeterVAIO` mit Kanalindizes 6, 7, 8 (Potato).

Damit ist die Geräteebene vollständig erklärt — und für Linux **gegenstandslos**:
PipeWire liefert Knoten und Clock, der Mixer braucht kein Kernelmodul.

## 3. Render-Pfad (MME-Ausgang, exemplarisch für alle Backends)

Kette, jede Stufe belegt:

1. `FUN_14005bd40` (0x14005bd40) — Ausgabe-Thread (Beleg `ev_b7122d41372008c3…`)
   - `SetThreadPriority(GetCurrentThread(), 0xF)` = `THREAD_PRIORITY_TIME_CRITICAL`
   - MMCSS über dynamisch geladenen Zeiger `DAT_140378940` mit Taskname `"Pro Audio"`
     (Rückfall auf einen zweiten String, wenn der erste leer ist)
   - Schleife: `(*(code**)(obj + 0x10B0))(ctx, buf, size)` → **das ist der Mixer-Render**,
     danach `FUN_14005bb30(obj, buf)` zum Schreiben
2. `FUN_14000c580` (0x14000c580) = `memset(buf, 0, size)` — Stille, wenn kein Render
   (Beleg `ev_cea06b763108fa0d…`); `FUN_14000c5e0` = `memcpy(dst, src, size)`
3. `FUN_14005bb30` (0x14005bb30) — Ringpuffer-Verwaltung, Beleg `ev_caceb78379e75ad4…`
   - Ring mit Slots von `0x28` Byte, Anzahl in `obj + 0x1094`
   - Wartet per `WaitForSingleObject` auf `obj + 0x1188` (Buffer-Done-Event),
     Timeout `obj + 0x10C0`
   - Setzt das `WHDR_INQUEUE`-Bit zurück (`and dword [hdr+0x18], 0xFFFFEEEE`)
   - `waveOutWrite(hwo, hdr, 0x30)`, bei Fehler `waveOutGetErrorTextA`

**Wichtig für den Nachbau:** der Mixer wird als **Funktionszeiger** in das Gerät
eingehängt (`obj + 0x10B0`), also backend-unabhängig mit der Signatur
`void render(void *ctx, float *out, uint32_t frames)`. Genau diese Schnittstelle
bildet ein Linux-Nachbau mit dem JACK-Prozess-Callback ab.

Der Ausgabepfad ist **doppelt gepuffert mit Block-Ring**; die Anzahl der Ring-Slots
liegt zur Laufzeit in `obj + 0x1094`. Das erklärt die Latenzformel
`Blockgröße × Ringtiefe`, nicht `Blockgröße × 2`.

## 4. Was die Analyse für den Nachbau hergibt

| Bereich | Status |
|---|---|
| Geräte-/Backend-Schicht | vollständig erklärt, für Linux irrelevant (PipeWire) |
| Render-Schnittstelle | belegt: `render(ctx, float* out, frames)`, 32-Bit-Float |
| Sampleraten/Puffergrenzen | belegt (32 k … 192 k, ≤ 2048 Frames, 2–8 Kanäle) |
| Parametermodell | belegt (XML-Attribute, siehe `02-signalmodell.md`) |
| Bus-/Kanal-Modi, Upmix | Namen belegt, **Verhalten nicht** |
| DSP-Kerne (EQ, Comp, Gate, Denoiser, Pitch, Limiter) | **offen** |
| VBAN-Paketlayout | **offen** (öffentliche Spezifikation ist die bessere Quelle) |
| Lizenzprüfung | bewusst nicht analysiert, wird nicht umgangen |

## 5. Nächste Analyseschritte (noch nicht gelaufen)

1. Render-Zeiger `obj + 0x10B0` auflösen: suchen, wer ihn setzt (Init-Pfad von
   `StartAudioEngine` über `FUN_1400f2970` / `FUN_14005c1a0`), dann den Mixer-Körper
   dekompilieren — erwartet: Schleife Streifen → Matrix → Busse.
2. Von dort in die FX-Aufrufe absteigen (EQ-Zellen, Comp, Gate) und die
   Koeffizientenbildung lesen (Biquad-Typen, Zeitkonstanten).
3. Pegel-/Meter-Berechnung (`dblevel`) und `greenmode`-Pfad abgleichen.
4. Grenzen der Analyse nennen: Pseudocode, keine Symbolnamen, keine Originaltypen.
