#!/usr/bin/env python3
"""Akustische Ende-zu-Ende-Prüfung der vm8-Engine.

Spielt einen 1-kHz-Sinus (-6 dBFS) in einen Test-Sink, lässt ihn durch Streifen 1
und Bus A1 laufen und misst den Bus-Ausgang. Erwartet:

  0 dB Gain       -> -6,00 dBFS
  Gain -12 dB     -> -18,00 dBFS
  Mute            -> Stille
  Routing A1->A2  -> Pegel wandert (API-Pegel A1 weg, A2 da)
  EQ-Boost +12 dB -> lauter als trocken

Voraussetzung: ./target/release/vm8-engine läuft.
"""
import json
import math
import os
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request
import wave

API = os.environ.get("VM8_API", "http://127.0.0.1:8770")
CLIENT = os.environ.get("VM8_CLIENT", "vm8")
SR, HZ, DB = 48000, 1000.0, -6.0
TMP = tempfile.mkdtemp(prefix="vm8-e2e-")
ok_all = True


def sh(cmd):
    return subprocess.run(cmd, shell=True, capture_output=True, text=True)


def api(path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(API + path, data=data, method="POST" if data else "GET")
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.loads(r.read().decode() or "{}")


def setp(target, index, param, value):
    return api("/api/set", {"target": target, "index": index, "param": param, "value": value})


def make_sink(name, desc):
    sh(f"pactl load-module module-null-sink sink_name={name} "
       f"sink_properties=device.description='{desc}'")
    time.sleep(0.4)


def write_sine(path, secs=2.5, db=DB):
    amp = 10 ** (db / 20)
    with wave.open(path, "w") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(SR)
        buf = bytearray()
        for i in range(int(SR * secs)):
            v = int(amp * 32767 * math.sin(2 * math.pi * HZ * i / SR))
            buf += struct.pack("<hh", v, v)
        w.writeframes(bytes(buf))


def peak_dbfs(path):
    with wave.open(path) as w:
        frames = w.readframes(w.getnframes())
    if not frames:
        return -140.0
    vals = struct.unpack("<%dh" % (len(frames) // 2), frames)
    pk = max(abs(v) for v in vals) / 32768.0
    return 20 * math.log10(pk) if pk > 1e-7 else -140.0


def measure(sink_in, sink_out, secs=2.4, wav=None):
    rec = os.path.join(TMP, "rec.wav")
    if os.path.exists(rec):
        os.unlink(rec)
    sh(f"pw-record --target 0 -P 'node.name=vm8cap' --rate {SR} --channels 2 {rec} >/dev/null 2>&1 &")
    time.sleep(0.8)
    sh(f"pw-link {sink_out}:monitor_FL vm8cap:input_FL")
    sh(f"pw-link {sink_out}:monitor_FR vm8cap:input_FR")
    sh(f"pw-play --target {sink_in} --rate {SR} {wav or os.path.join(TMP, 'sine.wav')} >/dev/null 2>&1 &")
    time.sleep(secs)
    sh("pkill -x pw-record")
    time.sleep(0.3)
    return peak_dbfs(rec)


def check(name, got, want, tol=0.7):
    global ok_all
    good = abs(got - want) <= tol
    ok_all &= good
    print(f"[{'OK  ' if good else 'FEHL'}] {name}: {got:+.2f} dBFS (erwartet {want:+.2f} ±{tol})")


def main():
    global ok_all
    try:
        api("/api/state")
    except Exception as e:
        print(f"Engine nicht erreichbar ({API}): {e}")
        return 2

    write_sine(os.path.join(TMP, "sine.wav"))
    make_sink("vm8_e2e_in", "VM8 E2E In")
    make_sink("vm8_e2e_out", "VM8 E2E Out")
    sh(f"pw-link vm8_e2e_in:monitor_FL {CLIENT}:strip1_L")
    sh(f"pw-link vm8_e2e_in:monitor_FR {CLIENT}:strip1_R")
    sh(f"pw-link {CLIENT}:busA1_L vm8_e2e_out:playback_FL")
    sh(f"pw-link {CLIENT}:busA1_R vm8_e2e_out:playback_FR")
    time.sleep(0.4)

    # Grundzustand: nur Streifen 1 auf A1, alles andere aus
    for i in range(8):
        setp("strip", i, "gain", 0)
        for p in ("mute", "solo", "mono", "eq", "comp", "gate", "denoiser"):
            setp("strip", i, p, 0)
        for b in range(8):
            setp("strip", i, f"send{b}", 1 if (i == 0 and b == 0) else 0)
    for b in range(8):
        setp("bus", b, "gain", 0)
        setp("bus", b, "mute", 0)
    setp("bus", 0, "limiter", 0)

    print("1) Durchgang 0 dB")
    check("Bus A1", measure("vm8_e2e_in", "vm8_e2e_out"), DB)

    print("2) Streifen-Gain -12 dB")
    setp("strip", 0, "gain", -12)
    time.sleep(0.2)
    check("Bus A1", measure("vm8_e2e_in", "vm8_e2e_out"), DB - 12)

    print("3) Mute")
    setp("strip", 0, "gain", 0)
    setp("strip", 0, "mute", 1)
    time.sleep(0.2)
    check("Bus A1", measure("vm8_e2e_in", "vm8_e2e_out"), -140.0, tol=1.0)

    print("4) Routing A1 -> A2")
    setp("strip", 0, "mute", 0)
    setp("strip", 0, "send0", 0)
    setp("strip", 0, "send1", 1)
    time.sleep(0.2)
    sh(f"pw-play --target vm8_e2e_in --rate {SR} {os.path.join(TMP, 'sine.wav')} >/dev/null 2>&1 &")
    time.sleep(1.2)
    lv = api("/api/levels")
    sh("pkill -x pw-play")
    good = lv["buses"][0] < -100 and abs(lv["buses"][1] - DB) < 1.5
    ok_all &= good
    print(f"[{'OK  ' if good else 'FEHL'}] Routing: A1 {lv['buses'][0]:+.1f} dB, A2 {lv['buses'][1]:+.1f} dB")

    print("5) EQ-Boost bei 1 kHz (+12 dB)")
    setp("strip", 0, "send0", 1)
    setp("strip", 0, "send1", 0)
    setp("strip", 0, "eq", 1)
    setp("strip", 0, "eq0_type", 0)      # Peak
    setp("strip", 0, "eq0_freq", HZ)
    setp("strip", 0, "eq0_q", 1.0)
    setp("strip", 0, "eq0_gain", 12.0)
    setp("strip", 0, "eq0_on", 1)
    time.sleep(0.2)
    # leiserer Sinus, sonst clippt der Boost bei voller Aussteuerung
    quiet = os.path.join(TMP, "sine_q.wav")
    write_sine(quiet, db=-24.0)
    check("Bus A1 mit EQ", measure("vm8_e2e_in", "vm8_e2e_out", wav=quiet), -12.0, tol=1.5)
    setp("strip", 0, "eq", 0)

    print("\nGesamtergebnis:", "bestanden" if ok_all else "FEHLGESCHLAGEN")
    print("Aufräumen: for i in $(pactl list short modules | awk '/vm8_e2e/{print $1}'); do pactl unload-module $i; done")
    return 0 if ok_all else 1


if __name__ == "__main__":
    sys.exit(main())
