#!/usr/bin/env python3
"""Vergleicht zwei Aufnahmen von tools/api_capture.py byte-genau.

Aufruf: api_vergleich.py <vorher-verzeichnis> <nachher-verzeichnis> [--verbose]

Fluechtige Felder werden vor dem Vergleich normalisiert, sonst waere jeder
Lauf anders:
  * `"f":<n>`            Frame-Zaehler in /api/levels und im WS-Frame
  * `"bis":<n>`          Zeitstempel des Autosave-Threads
Ein Unterschied wird mit Position und Kontext gemeldet; Exit 1, wenn einer
bleibt.
"""
import glob
import os
import re
import sys

FLAECHTIG = [
    (re.compile(rb'"f":-?\d+'), b'"f":X'),
    (re.compile(rb'"bis":-?\d+'), b'"bis":X'),
    (re.compile(rb'"pid":\d+'), b'"pid":X'),
]


def normalisieren(b: bytes) -> bytes:
    for pat, ersatz in FLAECHTIG:
        b = pat.sub(ersatz, b)
    return b


def dateien(d: str) -> dict:
    out = {}
    for p in glob.glob(os.path.join(d, "*")):
        name = os.path.basename(p)
        if name.endswith(".body"):
            out[name[: -len(".body")]] = p
    return out


def zeige_diff(a: bytes, b: bytes) -> None:
    i = 0
    while i < min(len(a), len(b)) and a[i] == b[i]:
        i += 1
    von = max(0, i - 60)
    print(f"      erste Abweichung bei Byte {i}")
    print(f"      vorher : …{a[von:i + 80]!r}")
    print(f"      nachher: …{b[von:i + 80]!r}")


if __name__ == "__main__":
    vor, nach = sys.argv[1], sys.argv[2]
    ausfuehrlich = "--verbose" in sys.argv
    av, bv = dateien(vor), dateien(nach)
    fehlend = sorted(set(av) - set(bv))
    neu = sorted(set(bv) - set(av))
    gleich = abweichend = 0
    for name in sorted(set(av) & set(bv)):
        a = normalisieren(open(av[name], "rb").read())
        b = normalisieren(open(bv[name], "rb").read())
        if a == b:
            gleich += 1
            if ausfuehrlich:
                print(f"  gleich      {name}")
        else:
            abweichend += 1
            print(f"  ABWEICHEND  {name}  ({len(a)} -> {len(b)} Byte)")
            zeige_diff(a, b)
    if fehlend:
        print(f"  fehlt nachher: {fehlend}")
    if neu:
        print(f"  neu nachher:   {neu}")
    print(
        f"\n{gleich} Endpunkte gleich, {abweichend} abweichend, "
        f"{len(fehlend)} fehlend, {len(neu)} neu"
    )
    sys.exit(0 if abweichend == 0 and not fehlend else 1)
