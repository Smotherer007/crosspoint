# VoiceMeeter 8 (v3.1.3.0) – Installer-Analyse

Stand: 2026-10-28 · Ghidra 12.1.4 (headless via REA 6.5.0) · Ziel: `Voicemeeter8Setup_v3130.zip` → `Voicemeeter8Setup.exe`

## 1. Äußere Form

| Merkmal | Wert |
|---|---|
| Datei | `Voicemeeter8Setup.exe`, 75 099 168 Byte |
| SHA-256 | `3cf143a67a89ff29dd19f6534676c3dc3ec89a3e97443b51d97bd68a7a7ee7a5` |
| Typ | PE32 (i386), GUI-Subsystem, `requireAdministrator` |
| FileVersion | 3.1.3.0, TimeDateStamp 2026-09-10 08:06:48 |
| Code | `.text` 161 792 Byte (350 Prozeduren, 562 definierte Strings) |
| Fracht | `.rsrc` 74 828 288 Byte – **eine** RCDATA-Ressource (Typ 10, ID 100) mit 73 658 023 Byte |
| PDB | `Voicemeeter8Setup.pdb`, GUID `399f2d8f-6488-4166-aa31-53ebf9e93383` (nicht vorhanden) |

Der Installer ist ein Eigenbau von VB-Audio (kein Inno/NSIS/MSI): 160 KB Code, 99 % der Datei ist Fracht.
Belege: `ev_5eaffd4494dd369fe3486b4954c7a848d208e00e05c61f89662efd8e78af3611` (Overview/Segmente).

## 2. Fracht-Format (RCDATA 10/100)

Verzeichnistabelle ab Blob-Offset 0, Einträge zu 160 Byte:

```
0x00  char[128]  Dateiname ('\' als Trenner)
0x80  u32        Datenoffset im Blob
0x84  u32        Datengröße
0x88  byte[16]   MD5 des Klartextes   (Prüffeld, im Installer verifiziert)
0x98  u64        Zusatzfeld, in 3.1.3.0 konstant 0x00CA740C, unbenutzt
```

Die Tabelle endet bei 135 Einträgen (0x5460 Byte); die Nutzdaten schließen lückenlos an und enden exakt
an der Blob-Grenze (`0x4644307`). Der Installer kopiert genau `0x5460` Byte und iteriert `0x87` = 135-mal
(`FUN_00417580`, `FUN_00417610`: `local_834 = 0x87`, Schrittweite `0xa0`).

## 3. „Verschlüsselung"

Alle 135 Nutzlasten sind **gleich lang wie der Klartext** und haben Entropie ≈ 8,0 bit/Byte – also
XOR-Strom, keine Blockchiffre (keine AES-S-Box, keine SHA/MD5-Konstanten im Code außer der MD5-Instanz).

Rekonstruiert aus `FUN_00417610`, Schleife `0x4177e6`–`0x417803`:

```
key_0   = 16 Nullbytes
acc_0   = 0x564A5D6F
je Datei (Tabellenreihenfolge):
    sq      = int32(acc + (size - i))²          # imul eax,eax, 32-Bit
    pt[i]  ^= ( key[(size-i) & 0xF] + ((sq>>8)&0xFF) + (sq&0xFF) ) & 0xFF
    acc     = int32(sq) >> 8                    # arithmetisch
    key'    = MD5(ciphertext)                   # nächste Datei
    acc'    = ((acc² & 0xFFFFFFFF) << 8 | key'[0]) & 0xFFFFFFFF
```

Damit ist der „Schutz" nur eine Verkettung: der Schlüssel für Datei *n* ist das MD5 der **Chiffre** von
Datei *n−1*; das Prüffeld `0x88` ist das MD5 des **Klartextes**. Beide Hashes stehen im selben Datensatz –
die Datei ist selbstverifizierend, ein Angriff ist nicht nötig.

Belege: `ev_ea487c836b9cdffb7fca8dfa883d9c20f439d2d013e27c770207cb4789af295f` (FUN_00417610),
`ev_19938b9a2f97191467af24caa2d2af9877998f57a1401aee185e580823de1d0f` (FUN_0040fd20 = MD5-thiscall),
`ev_a82ba5330326328b840ee2da6bd2ad666cac9f327d42676fb8301bc3f18b286d` (FUN_00401050 = memcmp 16 Byte),
`ev_5829556ede35067d91da26e5065a0ff275ca295151851c9d8067cae95e02f48f` (FUN_00401000 = memcpy).

**Ergebnis:** `tools/vbunpack.py` entpackt 135/135 Dateien mit stimmender Klartext-Prüfsumme
(Exit 0, Laufzeit ~40 s). Verkettung ist damit bewiesen, nicht vermutet.

## 4. Inhalt des Pakets (135 Dateien, 73,7 MB)

| Gruppe | Dateien | Zweck |
|---|---|---|
| Anwendungen | `voicemeeter8.exe`, `voicemeeter8x64.exe` (je ~13,4 MB), `voicemeeter.exe`, `voicemeeter_x64.exe`, `voicemeeterpro.exe`, `voicemeeterpro_x64.exe` | Standard / Banana / Potato / VM8 |
| Werkzeuge | `VoicemeeterMacroButtons.exe`, `VBAN2MIDI.exe`, `VoicemeeterBUSGEQ15.exe`, `VoicemeeterBUSMatrix8.exe`, `VMStreamerView.exe`, `VBDeviceCheck.exe` | Zusatz-APAs |
| Remote-API | `VoicemeeterRemote.dll` / `…64.dll` | 31 Exporte `VBVMR_*` (Login, Get/SetParameter*, Level, MIDI, MacroButton, AudioCallback) |
| ASIO-Treiber | 5× `vbvm*_asiodriver{,64}.dll` | virtuelle ASIO-Geräte (VM, AUX, INS, VMVAIO3, PINS) |
| Kernel-Treiber | `vbaudio_vm*.sys` / `vbvoicemeetervaio64*.sys` (x86, x64, ARM64; XP…Win10) + `.cat` + `.inf` | PortCls/WDM-Audiogerät (`portcls.sys`, `ntoskrnl.exe`) |
| Bibliothek | `mp3lame/lame_enc{,64}.dll` | MP3-Aufnahme |
| Dokumentation | `Voicemeeter{,Banana,Potato}_Help.xml`, `readme.txt` | Hilfe + EULA |

Installationsablauf (`FUN_0041a150`, Pseudocode): Paket entpacken → Zielordner → Dateien kopieren →
nested Setups `VBCABLE_Setup`, `VBVoicemeeterVAIO_Setup`, `VBVMAUX_Setup`, `VBVMVAIO3_Setup` starten →
ASIO-DLLs über `vbregsvr64.exe` registrieren → Verknüpfungen → Registry `…\CurrentVersion\Uninstall\VB:Voicemeeter {17359A74-1236-5467}`.
Beleg: `ev_184abcf977a0f201cd972450683a7e4704b4be20756d71097583c5c3a06d6bf4`.

## 5. Funktionsmodell (aus `VoicemeeterPotato_Help.xml`, 133 HelpIDs)

- **8 Streifen**: `STRIP0`–`STRIP7`, je: Fader, Mute, Solo, Mono, Pan/PanMode, A1–A5-/B1–B3-Sends,
  Comp, Gate, Denoiser, 3-Band-EQ (Lo/Mid/Hi), Pitch/Formant, PostFX-Returns (R, D, FX1, FX2), Karaoke, MC.
- **8 Busse**: `BUSA1`–`A5` (Hardware), `BUSB1`–`B3` (virtuell), je: Fader, Mute, Mono, EQ, Limiter,
  Mix-Modi (Normal / Downmix A/B / Stereo-Rep / Composite / TV-Mix / Upmix 2.1·4.1·6.1),
  Kanal-Routing (L, R, C, LFE, Rear), Returns, Tape-Sends.
- **Tape-Recorder** mit denselben 8 Zielen, **VBAN** In/Out (UDP, konfigurierbarer Port, Stream-Namen),
  **VBAN-Chat**, **VBAN2MIDI**.

Das ist die Parameterliste, an der sich ein Nachbau messen muss – nicht die Optik.

## 6. Rechtlicher Rahmen (wichtig für den Nachbau)

Die mitgelieferte EULA verbietet ausdrücklich Dekompilieren/Disassemblieren. Nach deutschem Recht
(§ 69d UrhG) ist Dekompilation zum Zweck der Interoperabilität eines unabhängig erstellten Programms
zulässig, soweit die nötigen Informationen nicht anderweitig verfügbar sind; eine EULA-Klausel, die das
pauschal ausschließt, ist insoweit unwirksam. Belastbar bleibt:

- **zulässig**: Verhalten analysieren, Schnittstellen/Formate verstehen, ein eigenständiges Programm schreiben.
- **nicht zulässig**: Code übernehmen, Assets (Logo, PNG, Help-XML) mitliefern, Name/Marke „VoiceMeeter"
  für das eigene Produkt verwenden, Setup-Paket weiterverbreiten.

Der Nachbau in diesem Repo enthält **keinen** Code, keine Ressourcen und keine Strings aus dem Original.

## 7. Offene Punkte / nicht geprüft

- `voicemeeter8x64.exe` (13,4 MB) wurde **nicht** in Ghidra analysiert – nur Strings gesichtet (nicht gepackt,
  PE32+ x64, 8 Sektionen). DSP-Details (Filterkoeffizienten, Limiter-Kennlinie) sind damit unbelegt.
- VBAN-Paketlayout nicht aus dem Binary rekonstruiert; die Protokollbeschreibung von VB-Audio ist öffentlich
  und wäre die belastbare Quelle.
- Die WDM-Treiber (`.sys`) wurden nicht analysiert; für Linux irrelevant, weil PipeWire die Geräteebene
  im Userspace bereitstellt.
