#!/usr/bin/env python3
"""Fragt alle HTTP-Endpunkte der Engine ab und legt die Antworten als Datei ab.

Aufruf: api_capture.py <basis-url> <zielverzeichnis>

Je Endpunkt entstehen zwei Dateien: `<name>.body` (Rumpf, byte-genau) und
`<name>.head` (Statuszeile, Content-Type, Content-Length, Cache-Control,
Connection). Zusaetzlich wird `/ws/levels` mit einem rohen WebSocket-
Handshake gelesen und der erste Text-Frame als `<name>.body` abgelegt.

Nur Standardbibliothek, damit der Lauf ohne Zusatzpakete funktioniert.
"""
import base64
import hashlib
import json
import os
import socket
import sys
import urllib.error
import urllib.parse
import urllib.request

GETS = [
    "/",
    "/index.html",
    "/api/state",
    "/api/levels",
    "/api/devices",
    "/api/devices?monitors=1",
    "/api/vban",
    "/api/vban/recv",
    "/api/vban/config",
    "/api/eq?strip=0",
    "/api/eq?strip=99",
    "/api/inputs",
    "/api/inputs?monitors=1",
    "/api/restore",
    "/api/macros",
    "/api/tape",
    "/api/autostart",
    "/api/preset",
    "/api/gibtsnicht",
]

POSTS = [
    ("set_ok", "/api/set", b'{"target":"strip","index":0,"param":"gain","value":-6}'),
    ("set_bad", "/api/set", b'{"target":"strip",'),
    ("set_unbekannt", "/api/set", b'{"target":"strip","index":0,"param":"quatsch","value":1}'),
    ("vban_bad", "/api/vban", b'{"action":"nope"}'),
    ("tape_bad", "/api/tape", b'{"action":"nope"}'),
    ("macros_bad", "/api/macros", b'{"action":"nope"}'),
    ("preset_save", "/api/preset", b'{"action":"save"}'),
    ("preset_load", "/api/preset", b'{"action":"load"}'),
    ("autostart_bad", "/api/autostart", b'{}'),
    ("vban_config_reset", "/api/vban/config", b'{"action":"reset"}'),
]


def safe(name: str) -> str:
    return (
        name.strip("/")
        .replace("/", "_")
        .replace("?", "_")
        .replace("=", "-")
        .replace("&", "_")
        or "root"
    )


def ablegen(ziel: str, name: str, body: bytes, kopf: str) -> None:
    with open(os.path.join(ziel, name + ".body"), "wb") as f:
        f.write(body)
    with open(os.path.join(ziel, name + ".head"), "w", encoding="utf-8") as f:
        f.write(kopf)
    print(f"{name:24s} {len(body):7d} Byte  {hashlib.sha256(body).hexdigest()[:16]}")


def hole(url: str) -> None:
    try:
        with urllib.request.urlopen(url, timeout=10) as r:
            body = r.read()
            kopf = f"HTTP {r.status}\n" + "".join(
                f"{k}: {v}\n"
                for k, v in r.headers.items()
                if k.lower() in ("content-type", "content-length", "cache-control", "connection")
            )
    except urllib.error.HTTPError as e:
        body = e.read()
        kopf = f"HTTP {e.code}\n" + "".join(
            f"{k}: {v}\n"
            for k, v in e.headers.items()
            if k.lower() in ("content-type", "content-length", "cache-control", "connection")
        )
    ablegen(ZIEL, safe(url), body, kopf)


def post(name: str, pfad: str, daten: bytes) -> None:
    req = urllib.request.Request(
        BASIS + pfad, data=daten, method="POST", headers={"Content-Type": "application/json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            body = r.read()
            kopf = f"HTTP {r.status}\n" + "".join(
                f"{k}: {v}\n"
                for k, v in r.headers.items()
                if k.lower() in ("content-type", "content-length", "cache-control", "connection")
            )
    except urllib.error.HTTPError as e:
        body = e.read()
        kopf = f"HTTP {e.code}\n" + "".join(
            f"{k}: {v}\n"
            for k, v in e.headers.items()
            if k.lower() in ("content-type", "content-length", "cache-control", "connection")
        )
    ablegen(ZIEL, "post_" + name, body, kopf)


def ws_frame() -> None:
    """Roher Handshake + erster Text-Frame von /ws/levels."""
    host, port = urllib.parse.urlsplit(BASIS).hostname, urllib.parse.urlsplit(BASIS).port
    s = socket.create_connection((host, port), timeout=10)
    key = base64.b64encode(b"0123456789abcdef").decode()
    s.sendall(
        (
            "GET /ws/levels HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        ).encode()
    )
    puffer = b""
    while b"\r\n\r\n" not in puffer:
        puffer += s.recv(4096)
    kopf, rest = puffer.split(b"\r\n\r\n", 1)
    zeilen = kopf.decode("latin-1").split("\r\n")
    akzeptiert = [z for z in zeilen if z.lower().startswith("sec-websocket-accept:")]

    def lies(n: int) -> bytes:
        nonlocal rest
        while len(rest) < n:
            rest += s.recv(65536)
        out, rest = rest[:n], rest[n:]
        return out

    def frame() -> tuple[int, bytes]:
        b0, b1 = lies(2)
        opcode = b0 & 0x0F
        ln = b1 & 0x7F
        if ln == 126:
            ln = int.from_bytes(lies(2), "big")
        elif ln == 127:
            ln = int.from_bytes(lies(8), "big")
        return opcode, lies(ln)

    opcode, payload = frame()
    s.close()
    kopf_text = "HTTP 101\n" + "\n".join(akzeptiert) + f"\nopcode: {opcode}\n"
    ablegen(ZIEL, "ws_levels", payload, kopf_text)


if __name__ == "__main__":
    BASIS = sys.argv[1].rstrip("/")
    ZIEL = sys.argv[2]
    os.makedirs(ZIEL, exist_ok=True)
    for p in GETS:
        hole(BASIS + p)
    for name, pfad, daten in POSTS:
        post(name, pfad, daten)
    ws_frame()
