#!/usr/bin/env python3
"""Fenster hoeflich schliessen: WM_DELETE_WINDOW senden (wie der Schliessen-Knopf).

Aufruf: wmclose.py <window-id> [...]

Ohne Fenstermanager gibt es keine Titelleiste und damit keinen Schliessen-
Knopf; `xdotool windowclose` zerstoert das Fenster hart (XKillClient), was GTK
nur als "GdkSurface unexpectedly destroyed" sieht. Was ein Fenstermanager beim
Klick auf "Schliessen" tut, ist eine ClientMessage WM_DELETE_WINDOW an den
Client - genau das macht dieses Skript, ueber libX11 per ctypes (kein Paket
noetig).

Exit 0, wenn mindestens eine Nachricht abgesetzt wurde.
"""
import ctypes
import sys

NoEventMask = 0
ClientMessage = 33
PropModeReplace = 0


class XClientMessageEvent(ctypes.Structure):
    _fields_ = [
        ("type", ctypes.c_int),
        ("serial", ctypes.c_ulong),
        ("send_event", ctypes.c_int),
        ("display", ctypes.c_void_p),
        ("window", ctypes.c_ulong),
        ("message_type", ctypes.c_ulong),
        ("format", ctypes.c_int),
        ("data", ctypes.c_long * 5),
    ]


class XEvent(ctypes.Union):
    _fields_ = [("xclient", XClientMessageEvent), ("pad", ctypes.c_long * 24)]


def main() -> int:
    ids = [int(a, 0) for a in sys.argv[1:]]
    if not ids:
        print("Aufruf: wmclose.py <window-id> [...]", file=sys.stderr)
        return 2
    x = ctypes.CDLL("libX11.so.6")
    x.XOpenDisplay.restype = ctypes.c_void_p
    x.XOpenDisplay.argtypes = [ctypes.c_char_p]
    dpy = x.XOpenDisplay(None)
    if not dpy:
        print("kein Display", file=sys.stderr)
        return 1
    x.XInternAtom.restype = ctypes.c_ulong
    x.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    wm_protocols = x.XInternAtom(dpy, b"WM_PROTOCOLS", False)
    wm_delete = x.XInternAtom(dpy, b"WM_DELETE_WINDOW", False)
    x.XSendEvent.argtypes = [
        ctypes.c_void_p,
        ctypes.c_ulong,
        ctypes.c_int,
        ctypes.c_long,
        ctypes.POINTER(XEvent),
    ]
    x.XFlush.argtypes = [ctypes.c_void_p]
    gesendet = 0
    for wid in ids:
        ev = XEvent()
        ev.xclient.type = ClientMessage
        ev.xclient.serial = 0
        ev.xclient.send_event = 1
        ev.xclient.display = dpy
        ev.xclient.window = wid
        ev.xclient.message_type = wm_protocols
        ev.xclient.format = 32
        ev.xclient.data[0] = wm_delete
        ev.xclient.data[1] = 0  # CurrentTime
        if x.XSendEvent(dpy, wid, False, NoEventMask, ctypes.byref(ev)):
            gesendet += 1
            print(f"WM_DELETE_WINDOW -> {wid}")
    x.XFlush(dpy)
    return 0 if gesendet else 1


if __name__ == "__main__":
    sys.exit(main())
