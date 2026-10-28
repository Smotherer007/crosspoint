#!/usr/bin/env python3
"""Prüft eine Tape-Aufnahme: Format und Pegel."""
import sys
import math
import wave

p = sys.argv[1]
w = wave.open(p)
n, ch, sw = w.getnframes(), w.getnchannels(), w.getsampwidth()
data = w.readframes(n)
if sw == 3:
    peak = 0
    for i in range(0, len(data), 3):
        v = int.from_bytes(data[i:i + 3], "little", signed=True)
        peak = max(peak, abs(v))
    full = 8388608
elif sw == 2:
    peak = max(abs(int.from_bytes(data[i:i + 2], "little", signed=True))
               for i in range(0, len(data), 2))
    full = 32768
else:
    raise SystemExit(f"unerwartete Bittiefe: {sw*8}")
db = 20 * math.log10(peak / full) if peak else -140.0
print(f"{n} Frames, {ch} Kanäle, {sw*8} bit, Peak {db:.2f} dBFS")
