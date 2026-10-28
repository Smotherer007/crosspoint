#!/usr/bin/env python3
"""Messung der Pegel-Latenz und der Bildrate im Browser (Chrome DevTools Protocol).

Ein Sprung am Streifen-Gain (-30 dB <-> 0 dB) wird per POST /api/set ausgeloest;
im Browser laeuft eine rAF-Schleife, die misst, wann sich die Hoehe des
Meter-Balkens (getBoundingClientRect, also das, was gezeichnet wird) und wann
sich der vom JS gesetzte Wert (style.height, also die Datenankunft) aendert.

Aufruf:
  python3 tests/latency_test.py --n 20 --strip 1
Ausgabe: Median/Max je Richtung (Latenz in ms) und Frames/s ueber 10 s.
Voraussetzung: Engine laeuft, UI im Browser offen (CDP auf 9222), Dauer-Signal
auf dem gemessenen Streifen (sonst aendert sich nichts).
"""
import argparse
import base64
import json
import os
import socket
import statistics
import struct
import sys
import time
import urllib.parse
import urllib.request

MESS_JS = r"""
(async (zielDb) => {
  const strip = document.querySelectorAll('#strips > .strip')[%(strip)d];
  if (!strip) return {fehler: 'kein Streifen'};
  const box = strip.querySelector('.meter');
  const fill = box.querySelector('.fill');
  if (!box || !fill) return {fehler: 'kein Meter'};
  const h = box.getBoundingClientRect().height || 1;
  const schwelle = Math.max(1.0, h * 0.01);
  const basisPx = fill.getBoundingClientRect().height;
  const basisStil = parseFloat(fill.style.height) || 0;
  let t0 = 0, tPx = null, tStil = null, n = 0;
  t0 = performance.now();
  const p = fetch('/api/set', {
    method: 'POST', headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({target: 'strip', index: %(strip)d, param: 'gain', value: zielDb})
  });
  await p;
  await new Promise((fertig) => {
    const tick = () => {
      const jetzt = performance.now();
      if (tStil === null) {
        const s = parseFloat(fill.style.height) || 0;
        if (Math.abs(s - basisStil) > 0.15) tStil = jetzt;
      }
      if (tPx === null) {
        const r = fill.getBoundingClientRect().height;
        if (Math.abs(r - basisPx) > schwelle) tPx = jetzt;
      }
      n++;
      if ((tPx !== null && tStil !== null) || jetzt - t0 > 400) return fertig();
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  return {px: tPx === null ? null : tPx - t0,
          daten: tStil === null ? null : tStil - t0,
          basis: basisPx, hoehe: h, frames: n};
})
""" 

FRAMES_JS = r"""(() => {
  const a = window.__wsFrames || 0;
  const t = performance.now();
  return new Promise((fertig) => setTimeout(() => {
    const b = window.__wsFrames || 0;
    fertig({frames: b - a, ms: performance.now() - t, gesamt: b,
            modus: window.__wsMode, fps: window.__wsFps || null});
  }, %(ms)d));
})()"""


class Cdp:
    def __init__(self, ws_url):
        u = urllib.parse.urlsplit(ws_url)
        host, port = u.hostname, u.port or 80
        netloc = u.netloc or f"{host}:{port}"
        path = u.path + ("?" + u.query if u.query else "")
        self.s = socket.create_connection((host, port), timeout=20)
        key = base64.b64encode(os.urandom(16)).decode()
        self.s.sendall(
            (
                f"GET {path} HTTP/1.1\r\nHost: {netloc}\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += self.s.recv(4096)
        self.id = 0

    def _recv_exact(self, n):
        data = b""
        while len(data) < n:
            teil = self.s.recv(n - len(data))
            if not teil:
                raise IOError("CDP zu")
            data += teil
        return data

    def _frame(self):
        h = self._recv_exact(2)
        ln = h[1] & 0x7F
        if ln == 126:
            ln = struct.unpack(">H", self._recv_exact(2))[0]
        elif ln == 127:
            ln = struct.unpack(">Q", self._recv_exact(8))[0]
        return self._recv_exact(ln)

    def call(self, method, params=None):
        self.id += 1
        payload = json.dumps({"id": self.id, "method": method, "params": params or {}}).encode()
        mask = os.urandom(4)
        head = bytes([0x81])
        if len(payload) < 126:
            head += bytes([0x80 | len(payload)])
        else:
            head += bytes([0x80 | 126]) + struct.pack(">H", len(payload))
        self.s.sendall(head + mask + bytes(c ^ mask[i % 4] for i, c in enumerate(payload)))
        self.s.settimeout(20)
        while True:
            msg = json.loads(self._frame())
            if msg.get("id") == self.id:
                return msg

    def js(self, expr):
        r = self.call(
            "Runtime.evaluate",
            {"expression": expr, "returnByValue": True, "awaitPromise": True},
        )
        res = r.get("result", {})
        if "exceptionDetails" in res:
            return {"__error": res["exceptionDetails"]}
        return res.get("result", {}).get("value")


def stats(werte):
    werte = [w for w in werte if w is not None]
    if not werte:
        return None
    return {
        "n": len(werte),
        "median": round(statistics.median(werte), 1),
        "max": round(max(werte), 1),
        "min": round(min(werte), 1),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8770")
    ap.add_argument("--cdp", default="http://127.0.0.1:9222")
    ap.add_argument("--n", type=int, default=20)
    ap.add_argument("--strip", type=int, default=1)
    ap.add_argument("--json", default="")
    ap.add_argument("--frames-sekunden", type=int, default=10)
    args = ap.parse_args()

    targets = json.load(urllib.request.urlopen(args.cdp + "/json/list"))
    page = [t for t in targets if t.get("url", "").startswith(args.url)]
    if not page:
        print("Kein Browserfenster mit der UI offen – bitte Fenster öffnen.")
        return 2
    cdp = Cdp(page[0]["webSocketDebuggerUrl"])
    cdp.call("Page.reload", {"ignoreCache": True})
    time.sleep(3.0)

    modus = cdp.js("window.__wsMode")
    print(f"Pegelweg: {modus}")
    if modus != "ws":
        print("WARNUNG: kein WebSocket – Messung liefe über Polling")

    pegel = cdp.js("fetch('/api/levels').then(r=>r.json())")
    print(f"Pegel vor der Messung: Strips {pegel['strips'] if pegel else '?'}")

    runter, hoch, daten = [], [], []
    for i in range(args.n):
        for ziel, topf in ((-30.0, runter), (0.0, hoch)):
            time.sleep(0.45)
            m = cdp.js(MESS_JS % {"strip": args.strip} + f"({ziel})")
            if not isinstance(m, dict) or m.get("fehler"):
                print(f"  Messfehler: {m}")
                continue
            topf.append(m["px"])
            daten.append(m["daten"])
        if (i + 1) % 5 == 0:
            print(f"  {i + 1}/{args.n} Durchlaeufe")

    fr = cdp.js(FRAMES_JS % {"ms": args.frames_sekunden * 1000})
    fps = fr["frames"] / (fr["ms"] / 1000.0) if fr else 0

    alle = [x for x in runter + hoch if x is not None]
    bericht = {
        "modus": modus,
        "sprung_runter": stats(runter),
        "sprung_hoch": stats(hoch),
        "pixel_gesamt": stats(alle),
        "datenankunft": stats(daten),
        "frames_pro_s": round(fps, 1),
        "frames_gesamt": fr["gesamt"] if fr else None,
        "ui_fps": (fr or {}).get("fps"),
    }
    print(json.dumps(bericht, indent=2, ensure_ascii=False))
    if args.json:
        open(args.json, "w").write(json.dumps(bericht, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
