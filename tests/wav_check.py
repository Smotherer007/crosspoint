#!/usr/bin/env python3
"""Prüft eine Tape-Aufnahme: Format und Pegel.

Liest den RIFF-Kopf selbst statt über `wave`: seit dem Wechsel auf `hound`
schreibt die Engine bei 24 bit (und bei mehr als zwei Kanälen)
`WAVE_FORMAT_EXTENSIBLE`, und Pythons `wave`-Modul kennt nur
`WAVE_FORMAT_PCM`.

Aufruf: wav_check.py <datei.wav> [--freq]
  --freq  schätzt zusätzlich die Frequenz aus den Nulldurchgängen (Kanal 0)
"""
import math
import struct
import sys


def kopf(b: bytes):
    if b[:4] != b"RIFF" or b[8:12] != b"WAVE":
        raise SystemExit("kein RIFF/WAVE")
    i = 12
    fmt = None
    daten = None
    while i + 8 <= len(b):
        kennung = b[i : i + 4]
        laenge = struct.unpack_from("<I", b, i + 4)[0]
        rumpf = b[i + 8 : i + 8 + laenge]
        if kennung == b"fmt ":
            roh_tag, kanaele, rate, _, align, bits = struct.unpack_from("<HHIIHH", rumpf, 0)
            tag = roh_tag
            if roh_tag == 0xFFFE:
                # WAVEFORMATEXTENSIBLE: SubFormat-GUID ab Byte 24 des Chunks
                tag = struct.unpack_from("<H", rumpf, 24)[0]
            fmt = dict(tag=tag, extensible=roh_tag == 0xFFFE, kanaele=kanaele,
                       rate=rate, bits=bits, align=align)
        elif kennung == b"data":
            daten = rumpf
        i += 8 + laenge + (laenge & 1)
    if fmt is None or daten is None:
        raise SystemExit("fmt- oder data-Chunk fehlt")
    return fmt, daten


def samples(daten: bytes, bits: int):
    if bits == 24:
        for i in range(0, len(daten) - 2, 3):
            yield int.from_bytes(daten[i : i + 3], "little", signed=True), 8388608
    elif bits == 16:
        for i in range(0, len(daten) - 1, 2):
            yield int.from_bytes(daten[i : i + 2], "little", signed=True), 32768
    elif bits == 32:
        for i in range(0, len(daten) - 3, 4):
            yield struct.unpack_from("<f", daten, i)[0], 1.0
    else:
        raise SystemExit(f"unerwartete Bittiefe: {bits}")


def main() -> int:
    pfad = sys.argv[1]
    mit_freq = "--freq" in sys.argv
    b = open(pfad, "rb").read()
    fmt, daten = kopf(b)
    werte = list(samples(daten, fmt["bits"]))
    frames = len(werte) // fmt["kanaele"]
    voll = werte[0][1] if werte else 1.0
    peak = max((abs(v) for v, _ in werte), default=0.0)
    verhaeltnis = peak / voll
    db = 20 * math.log10(verhaeltnis) if verhaeltnis > 0 else -140.0
    format_text = "PCM" if fmt["tag"] == 1 else "IEEE-float"
    print(
        f"{frames} Frames, {fmt['kanaele']} Kanäle, {fmt['bits']} bit, "
        f"{format_text}{' (EXTENSIBLE)' if fmt['extensible'] else ''}, "
        f"{fmt['rate']} Hz, Peak {db:.2f} dBFS"
    )
    if mit_freq:
        kanal = [v for i, (v, _) in enumerate(werte) if i % fmt["kanaele"] == 0]
        # Nur den Bereich mit Signal auswerten: am Anfang/Ende stehen Blöcke
        # Stille (Aufnahme laeuft vor bzw. nach dem Ton weiter), sonst zieht
        # die Stille die Frequenz nach unten.
        schwelle = voll * 0.01
        laut = [i for i, v in enumerate(kanal) if abs(v) > schwelle]
        if not laut:
            print("kein Signal")
            return 0
        von, bis = laut[0], laut[-1]
        teil = kanal[von : bis + 1]
        nullen = sum(1 for a, c in zip(teil, teil[1:]) if a < 0 <= c)
        dauer = len(teil) / fmt["rate"]
        print(f"{nullen / dauer:.1f} Hz (aus {nullen} Nulldurchgängen, {dauer:.3f} s Signal)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
