#!/usr/bin/env python3
"""UI-Durchlauf über das Chrome DevTools Protocol.

Klickt die Oberfläche wie ein Benutzer (echte Element-Clicks/Events) und prüft
danach den Zustand über die HTTP-API. Kein Browser-Tool, keine Fremdpakete.

Aufruf: python3 tests/ui_test.py [--url http://127.0.0.1:8770] [--cdp http://127.0.0.1:9222]
"""
import argparse
import base64
import json
import os
import socket
import struct
import sys
import time
import urllib.request

ok_all = True
fails = []


def result(name, good, detail=""):
    global ok_all
    ok_all &= good
    mark = "OK  " if good else "FEHL"
    print(f"[{mark}] {name}{(' – ' + detail) if detail else ''}")
    if not good:
        fails.append(name)


class Cdp:
    def __init__(self, ws_url):
        host, port = ws_url.split("/")[2].split(":")
        path = "/" + "/".join(ws_url.split("/")[3:])
        self.s = socket.create_connection((host, int(port)), timeout=15)
        key = base64.b64encode(os.urandom(16)).decode()
        self.s.sendall(
            (
                f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += self.s.recv(4096)
        self.id = 0

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
        self.s.settimeout(15)
        while True:
            h = self.s.recv(2)
            ln = h[1] & 0x7F
            if ln == 126:
                ln = struct.unpack(">H", self.s.recv(2))[0]
            elif ln == 127:
                ln = struct.unpack(">Q", self.s.recv(8))[0]
            data = b""
            while len(data) < ln:
                data += self.s.recv(ln - len(data))
            msg = json.loads(data)
            if msg.get("id") == self.id:
                return msg

    def js(self, expr):
        r = self.call(
            "Runtime.evaluate",
            {"expression": expr, "returnByValue": True, "awaitPromise": True},
        )
        res = r.get("result", {})
        if "exceptionDetails" in res:
            return {"__error": res["exceptionDetails"].get("text", "exception")}
        return res.get("result", {}).get("value")


def get(base, path):
    with urllib.request.urlopen(base + path, timeout=5) as r:
        return json.loads(r.read().decode())


def post(base, path, body):
    req = urllib.request.Request(
        base + path,
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.loads(r.read().decode() or "{}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8770")
    ap.add_argument("--cdp", default="http://127.0.0.1:9222")
    args = ap.parse_args()

    targets = json.load(urllib.request.urlopen(args.cdp + "/json/list"))
    page = [t for t in targets if t.get("url", "").startswith(args.url)]
    if not page:
        print("Kein Browserfenster mit der UI offen – bitte Fenster öffnen.")
        return 2
    cdp = Cdp(page[0]["webSocketDebuggerUrl"])
    cdp.call("Page.reload", {"ignoreCache": True})
    time.sleep(3)

    # Fehlersammler setzen, bevor geklickt wird
    cdp.js("window.__errs=[];window.addEventListener('error',e=>window.__errs.push(String(e.message)));"
           "window.addEventListener('unhandledrejection',e=>window.__errs.push('promise:'+String(e.reason)));1")

    dom = cdp.js("JSON.stringify({strips:document.querySelectorAll('#strips > .strip').length,"
                 "buses:document.querySelectorAll('#buses > *').length,"
                 "knobs:document.querySelectorAll('.knob').length,"
                 "faders:document.querySelectorAll('input[type=range]').length,"
                 "routing:document.querySelectorAll('.send,.rb,.tgt').length,"
                 "rec:!!document.getElementById('tRec'),toast:typeof toast})")
    dom = json.loads(dom) if isinstance(dom, str) else {}
    result("UI aufgebaut", dom.get("strips", 0) == 8 and dom.get("buses", 0) == 8
           and dom.get("faders", 0) >= 16, str(dom))

    # 1) Fader: Streifen 1 auf -9 dB
    got = cdp.js("""(()=>{const f=document.querySelectorAll('.strip .fader')[0];
      f.value=-9; f.dispatchEvent(new Event('input',{bubbles:true})); return f.value;})()""")
    time.sleep(0.6)
    st = get(args.url, "/api/state")
    result("Fader wirkt", abs(st["strips"][0]["gain"] + 9) < 0.6, f"gain={st['strips'][0]['gain']}")

    # 2) Routing-Taste B1 (Index 5) am Streifen 1 - Erwartung aus dem Ist-Zustand
    before_b1 = get(args.url, "/api/state")["strips"][0]["sends"][5] > 0
    cdp.js("""(()=>{const s=document.querySelectorAll('.strip')[0];
      const b=[...s.querySelectorAll('button')].find(x=>x.textContent.trim()==='B1'); b.click();})()""")
    time.sleep(0.6)
    st = get(args.url, "/api/state")
    after_b1 = st["strips"][0]["sends"][5] > 0
    result("Routing B1 schaltet um", after_b1 != before_b1, f"{before_b1} -> {after_b1}")

    # 3) Mute (Button mit Klasse "mute") und Mono (Button "M")
    cdp.js("""(()=>{const s=document.querySelectorAll('.strip')[0];
      s.querySelector('button.sw.mute').click();})()""")
    time.sleep(0.6)
    st = get(args.url, "/api/state")
    result("MUTE-Taste", st["strips"][0]["mute"] is True)
    cdp.js("""(()=>{const s=document.querySelectorAll('.strip')[0];
      s.querySelector('button.sw.mute').click();})()""")
    time.sleep(0.4)
    st = get(args.url, "/api/state")
    result("MUTE wieder aus", st["strips"][0]["mute"] is False)

    cdp.js("""(()=>{const s=document.querySelectorAll('.strip')[0];
      const b=[...s.querySelectorAll('button.sw')].find(x=>x.textContent.trim()==='M'); b.click();})()""")
    time.sleep(0.6)
    st = get(args.url, "/api/state")
    result("Mono-Taste (M)", st["strips"][0]["mono"] is True)
    cdp.js("""(()=>{const s=document.querySelectorAll('.strip')[0];
      const b=[...s.querySelectorAll('button.sw')].find(x=>x.textContent.trim()==='M'); b.click();})()""")

    # 4) Aufnahme starten/stoppen
    post(args.url, "/api/tape", {"action": "stop"})
    time.sleep(0.4)
    cdp.js("document.getElementById('tRec').click();1")
    time.sleep(1.5)
    tape = get(args.url, "/api/tape")
    result("REC startet Aufnahme", tape.get("recording") is True, str(tape))
    cdp.js("document.getElementById('tStop').click();1")
    time.sleep(1.0)
    tape = get(args.url, "/api/tape")
    result("STOP beendet Aufnahme", tape.get("recording") is False and "gestoppt" in tape.get("status", ""),
           tape.get("status", ""))

    # 5) Preset speichern/laden über die UI-Buttons
    before = get(args.url, "/api/state")["strips"][0]["gain"]
    cdp.js("""(()=>{const b=[...document.querySelectorAll('button')].find(x=>/PRESET SPEICHERN/i.test(x.textContent));b.click();})()""")
    time.sleep(0.8)
    cdp.js("""(()=>{const f=document.querySelectorAll('.strip .fader')[0];f.value=0;
      f.dispatchEvent(new Event('input',{bubbles:true}));})()""")
    time.sleep(0.5)
    cdp.js("""(()=>{const b=[...document.querySelectorAll('button')].find(x=>/PRESET LADEN/i.test(x.textContent));b.click();})()""")
    time.sleep(1.2)
    after = get(args.url, "/api/state")["strips"][0]["gain"]
    result("Preset speichern/laden", abs(after - before) < 0.6, f"{before} -> {after}")

    # 6) Knopf ziehen (echte Maus-Events auf dem Gain-Knopf von Streifen 2)
    box = cdp.js("""(()=>{const k=document.querySelectorAll('#strips > .strip')[1].querySelector('.knob');
      if(!k) return null; const r=k.getBoundingClientRect();
      return JSON.stringify({x:Math.round(r.x+r.width/2),y:Math.round(r.y+r.height/2)});})()""")
    before_k = get(args.url, "/api/state")["strips"][1]["gain"]
    if box:
        pt = json.loads(box)
        cdp.call("Input.dispatchMouseEvent", {"type": "mousePressed", "x": pt["x"], "y": pt["y"],
                                              "button": "left", "clickCount": 1})
        cdp.call("Input.dispatchMouseEvent", {"type": "mouseMoved", "x": pt["x"], "y": pt["y"] - 40,
                                              "button": "left", "buttons": 1})
        cdp.call("Input.dispatchMouseEvent", {"type": "mouseReleased", "x": pt["x"], "y": pt["y"] - 40,
                                              "button": "left", "clickCount": 1})
        time.sleep(0.6)
        after_k = get(args.url, "/api/state")["strips"][1]["gain"]
        result("Gain-Knopf dreht", abs(after_k - before_k) > 0.5, f"{before_k} -> {after_k}")
    else:
        result("Gain-Knopf dreht", False, "Knopf nicht gefunden")

    # 7) Bus-Fader (A1) und Master-Mute
    before_bus = get(args.url, "/api/state")["buses"][0]["gain"]
    cdp.js("""(()=>{const f=document.querySelectorAll('#buses .fader')[0];
      f.value=-6; f.dispatchEvent(new Event('input',{bubbles:true}));})()""")
    time.sleep(0.6)
    after_bus = get(args.url, "/api/state")["buses"][0]["gain"]
    result("Bus-Fader A1", abs(after_bus + 6) < 0.6, f"{before_bus} -> {after_bus}")

    muted_before = get(args.url, "/api/state")["buses"][0]["mute"]
    cdp.js("""(()=>{const side=document.querySelector('.side');
      const b=[...side.querySelectorAll('button')].find(x=>x.textContent.trim()==='MUTE'); if(b) b.click();})()""")
    time.sleep(0.6)
    muted_after = get(args.url, "/api/state")["buses"][0]["mute"]
    result("Master-MUTE", muted_after != muted_before, f"{muted_before} -> {muted_after}")
    if muted_after != muted_before:
        cdp.js("""(()=>{const side=document.querySelector('.side');
          const b=[...side.querySelectorAll('button')].find(x=>x.textContent.trim()==='MUTE'); if(b) b.click();})()""")

    # 8) EQ-Zelle und Denominierung
    eq_before = get(args.url, "/api/state")["strips"][0]["eq"]
    cdp.js("""(()=>{const s=document.querySelectorAll('#strips > .strip')[0];
      s.querySelector('.eqled').click();})()""")
    time.sleep(0.6)
    eq_after = get(args.url, "/api/state")["strips"][0]["eq"]
    result("EQ-Taste", eq_after != eq_before, f"{eq_before} -> {eq_after}")

    errs = cdp.js("JSON.stringify(window.__errs||[])")
    result("keine JS-Fehler", errs in ("[]", None), str(errs))
    shot = cdp.call("Page.captureScreenshot", {"format": "png"})
    data = shot.get("result", {}).get("data")
    if data:
        open("/tmp/vm8-ui-test.png", "wb").write(base64.b64decode(data))
        print("Screenshot: /tmp/vm8-ui-test.png")

    print("\nGesamtergebnis:", "bestanden" if ok_all else f"FEHLGESCHLAGEN ({', '.join(fails)})")
    return 0 if ok_all else 1


if __name__ == "__main__":
    sys.exit(main())
