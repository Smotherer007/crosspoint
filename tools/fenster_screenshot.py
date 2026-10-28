#!/usr/bin/env python3
"""Screenshot des Mixer-Fensters ueber CDP (umgeht Sperrbildschirm/Screensaver)."""
import base64
import json
import socket
import sys
import urllib.request


class Cdp:
    def __init__(self, ws_url):
        ohne = ws_url.split("://", 1)[1]
        hostport, pfad = ohne.split("/", 1)
        host, port = hostport.split(":")
        self.s = socket.create_connection((host, int(port)), timeout=15)
        key = base64.b64encode(b"0123456789abcdef").decode()
        req = (
            f"GET /{pfad} HTTP/1.1\r\nHost: {hostport}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        self.s.sendall(req.encode())
        self.s.recv(4096)
        self.n = 0

    def call(self, method, params=None):
        self.n += 1
        nutz = json.dumps({"id": self.n, "method": method, "params": params or {}}).encode()
        kopf = bytearray([0x81])
        l = len(nutz)
        if l < 126:
            kopf.append(0x80 | l)
        elif l < 65536:
            kopf.append(0x80 | 126)
            kopf += l.to_bytes(2, "big")
        else:
            kopf.append(0x80 | 127)
            kopf += l.to_bytes(8, "big")
        maske = b"\x11\x22\x33\x44"
        kopf += maske
        self.s.sendall(bytes(kopf) + bytes(b ^ maske[i % 4] for i, b in enumerate(nutz)))
        while True:
            daten = self.s.recv(1 << 20)
            if len(daten) < 3:
                continue
            start = 2
            ln = daten[1] & 0x7F
            if ln == 126:
                ln = int.from_bytes(daten[2:4], "big")
                start = 4
            elif ln == 127:
                ln = int.from_bytes(daten[2:10], "big")
                start = 10
            text = daten[start:start + ln].decode("utf-8", "replace")
            try:
                return json.loads(text)
            except Exception:
                continue


def main():
    port = sys.argv[1] if len(sys.argv) > 1 else "9223"
    ziel = sys.argv[2] if len(sys.argv) > 2 else "/tmp/fenster.png"
    tabs = json.load(urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list"))
    tab = next((t for t in tabs if "8770" in (t.get("url") or "")), None)
    if not tab:
        print("kein Fenster auf Port", port)
        return 1
    cdp = Cdp(tab["webSocketDebuggerUrl"])
    antwort = cdp.call("Page.captureScreenshot", {"format": "png"})
    daten = base64.b64decode(antwort["result"]["data"])
    with open(ziel, "wb") as f:
        f.write(daten)
    print(f"{ziel} ({len(daten)} Byte) Titel: {tab.get('title')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
