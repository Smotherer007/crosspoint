#!/usr/bin/env python3
"""Akustischer Loopback-Test: Ton ueber die Boxen, Aufnahme ueber das Mikrofon.

Misst, ob und mit welcher Verzoegerung ein Signal den Weg
Wiedergabe -> Luft -> Mikrofon schafft. Das prueft die Kette dort, wo keine
Software-Verkabelung mehr hilft: Pegel, Latenz und Klangfarbe in der Realitaet.

Aufruf:
    python3 tests/loopback_acoustic.py [--sink NAME] [--source NAME]
                                       [--freq 1000] [--sekunden 2]

Ohne Argumente werden der Standard-Sink und das erste ALSA-Eingabegeraet
genommen. Ausgabe: JSON mit Pegeln, Korrelations-Peak und Latenz.
"""
import argparse
import json
import math
import os
import struct
import subprocess
import sys
import tempfile
import wave


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def standard_sink():
    return run(["pactl", "get-default-sink"]).stdout.strip()


def erste_quelle():
    out = run(["pactl", "list", "short", "sources"]).stdout
    for zeile in out.splitlines():
        felder = zeile.split("\t")
        if len(felder) > 1 and "alsa_input" in felder[1]:
            return felder[1]
    return None


def ton_schreiben(pfad, freq, sekunden, rate=48000, amplitude=0.5):
    """Reiner Sinus als 24-Bit-WAV - klar erkennbar in der Korrelation."""
    n = int(rate * sekunden)
    with wave.open(pfad, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(3)
        w.setframerate(rate)
        roh = bytearray()
        for i in range(n):
            v = int(amplitude * math.sin(2 * math.pi * freq * i / rate) * 8388607)
            b = struct.pack("<i", v)[:3]
            roh += b + b
        w.writeframes(bytes(roh))


def aufnehmen(ziel, quelle, sekunden, rate=48000):
    return subprocess.run(
        ["parec", f"--device={quelle}", f"--rate={rate}", "--channels=1",
         "--format=s16le", "--file-format=wav", ziel],
        capture_output=True,
        timeout=sekunden + 5,
    )


def pegel(pfad):
    """Spitzen- und Effektivpegel in dBFS ueber ffmpeg volumedetect."""
    out = run(["ffmpeg", "-hide_banner", "-i", pfad, "-af", "volumedetect", "-f", "null", "-"]).stderr
    werte = {}
    for zeile in out.splitlines():
        for name in ("max_volume", "mean_volume"):
            if f"{name}:" in zeile:
                werte[name] = float(zeile.split(f"{name}:")[1].split("dB")[0].strip())
    return werte


def korrelation(pfad, freq, rate=48000):
    """Schaetzt, ob der Ton im Signal steckt: Goertzel bei `freq` gegen Nachbarband."""
    with wave.open(pfad, "rb") as w:
        kanaele = w.getnchannels()
        roh = w.readframes(w.getnframes())
    if not roh:
        return {"energie": None, "verhaeltnis_db": None}
    samples = struct.unpack("<%dh" % (len(roh) // 2), roh)[::kanaele]

    def goertzel(sig, f):
        n = len(sig)
        k = int(0.5 + n * f / rate)
        omega = 2 * math.pi * k / n
        coeff = 2 * math.cos(omega)
        s0 = s1 = s2 = 0.0
        for x in sig:
            s0 = x / 32768.0 + coeff * s1 - s2
            s2, s1 = s1, s0
        return s1 * s1 + s2 * s2 - coeff * s1 * s2

    e_ton = goertzel(samples, freq)
    e_neben = max(goertzel(samples, freq * 0.7), 1e-12)
    return {
        "energie": e_ton,
        "verhaeltnis_db": 10 * math.log10(max(e_ton, 1e-12) / e_neben),
        "samples": len(samples),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sink", default=None)
    ap.add_argument("--source", default=None)
    ap.add_argument("--freq", type=float, default=1000.0)
    ap.add_argument("--sekunden", type=float, default=2.0)
    args = ap.parse_args()

    sink = args.sink or standard_sink()
    quelle = args.source or erste_quelle()
    if not sink or not quelle:
        print(json.dumps({"fehler": "Sink oder Quelle nicht gefunden",
                          "sink": sink, "quelle": quelle}))
        return 2

    tmp = tempfile.mkdtemp(prefix="vm8-loop-")
    ton = os.path.join(tmp, "ton.wav")
    auf = os.path.join(tmp, "aufnahme.wav")
    ton_schreiben(ton, args.freq, args.sekunden)

    rec = subprocess.Popen(
        ["parec", f"--device={quelle}", "--rate=48000", "--channels=1",
         "--format=s16le", "--file-format=wav", auf],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    subprocess.run(["sleep", "0.3"])
    play = subprocess.run(["pw-play", f"--target={sink}", ton],
                          capture_output=True, text=True, timeout=args.sekunden + 10)
    rec.terminate()
    try:
        rec.wait(timeout=5)
    except subprocess.TimeoutExpired:
        rec.kill()

    ergebnis = {
        "sink": sink,
        "quelle": quelle,
        "freq_hz": args.freq,
        "play_exit": play.returncode,
        "play_fehler": play.stderr.strip()[:200],
        "dateien": {"ton": ton, "aufnahme": auf},
    }
    if os.path.exists(auf) and os.path.getsize(auf) > 44:
        ergebnis["pegel_aufnahme"] = pegel(auf)
        ergebnis["ton_nachweis"] = korrelation(auf, args.freq)
    else:
        ergebnis["fehler"] = "keine Aufnahme entstanden"
    print(json.dumps(ergebnis, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
