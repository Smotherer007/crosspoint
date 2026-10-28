#!/usr/bin/env python3
"""Zeichnet Icon und Markenblatt im Blueprint-Stil (dunkel, Faderraster, Signalgrün).

Erzeugt:
  icon.png       1024x1024  App-Icon (Flatpak, Desktop)
  marke.png      1800x1150  Markenblatt mit drei Wortmarken-Varianten
"""
from PIL import Image, ImageDraw, ImageFont

BG      = (6, 10, 18)      # blauschwarz
GRID    = (14, 22, 36)
LINIE   = (24, 52, 78)     # blaustichige Rasterlinie
GRUEN   = (57, 221, 147)
GRUEN_D = (26, 120, 86)
INK     = (214, 226, 240)
DIM     = (124, 147, 168)
MONO    = "/usr/share/fonts/google-noto/NotoSansMono-Bold.ttf"
MONO_R  = "/usr/share/fonts/google-noto/NotoSansMono-Regular.ttf"


def font(pfad, groesse):
    return ImageFont.truetype(pfad, groesse)


def hintergrund(w, h, raster=28):
    bild = Image.new("RGB", (w, h), BG)
    d = ImageDraw.Draw(bild)
    for x in range(0, w, raster):
        d.line([(x, 0), (x, h)], fill=GRID, width=1)
    for y in range(0, h, raster):
        d.line([(0, y), (w, y)], fill=GRID, width=1)
    return bild, d


def faderbank(d, x0, y0, breite, hoehe, n=8, pegel=None, rand=4):
    """Acht Fader mit unterschiedlichen Stellungen - die Signatur des Icons."""
    pegel = pegel or [0.18, 0.34, 0.26, 0.52, 0.44, 0.68, 0.58, 0.80]
    schritt = breite / n
    spalte = schritt * 0.46
    for i in range(n):
        cx = x0 + schritt * (i + 0.5)
        links = cx - spalte / 2
        rechts = cx + spalte / 2
        d.rounded_rectangle([links, y0, rechts, y0 + hoehe], radius=rand, fill=(18, 28, 22), outline=LINIE)
        # Signal-Füllung von unten
        h = hoehe * pegel[i]
        if h > 6:
            d.rounded_rectangle([links + 2, y0 + hoehe - h, rechts - 2, y0 + hoehe - 2],
                                radius=max(1, rand - 1), fill=GRUEN_D)
        # Knopf
        ky = y0 + hoehe - h
        rand_x = schritt * 0.16
        d.rounded_rectangle([links - rand_x, ky - 11, rechts + rand_x, ky + 11],
                            radius=3, fill=INK)
        d.line([(links - rand_x, ky + 4), (rechts + rand_x, ky + 4)], fill=(150, 170, 190), width=2)


def icon(pfad):
    s = 1024
    bild, d = hintergrund(s, s, raster=32)
    # Aussenrahmen
    d.rounded_rectangle([48, 48, s - 48, s - 48], radius=96, outline=LINIE, width=3)
    faderbank(d, 150, 250, s - 300, 470, pegel=[0.20, 0.36, 0.28, 0.55, 0.47, 0.70, 0.60, 0.86])
    # Signallinie
    d.line([(150, 200), (s - 150, 200)], fill=GRUEN, width=6)
    d.ellipse([150 - 9, 200 - 9, 150 + 9, 200 + 9], fill=GRUEN)
    d.ellipse([s - 150 - 9, 200 - 9, s - 150 + 9, 200 + 9], fill=GRUEN)
    # 8 Kanäle als Ziffern
    f = font(MONO_R, 44)
    for i in range(8):
        t = str(i + 1)
        w = d.textlength(t, font=f)
        cx = 150 + (s - 300) / 8 * (i + 0.5)
        d.text((cx - w / 2, 760), t, font=f, fill=DIM)
    bild.save(pfad, "PNG")
    return pfad


def marke(pfad):
    w, h = 1800, 1150
    bild, d = hintergrund(w, h, raster=36)
    kopf = font(MONO_R, 30)
    d.text((80, 60), "MARKE · KONZEPT 01", font=kopf, fill=DIM)
    d.text((80, 100), "BLUEPRINT / FADERBANK / SIGNALGRÜN", font=kopf, fill=GRUEN)

    # Icon links
    ic = Image.open("/home/patrickweppelmann/.hermes/profiles/re/cache/scratch/icon.png").resize((420, 420))
    bild.paste(ic, (80, 180))

    # Wortmarken
    varianten = [("CROSSPOINT", "8 KANÄLE · 8 BUSSE · EIN RASTER")]
    y = 200
    for name, unter in varianten:
        f = font(MONO, 108)
        d.text((580, y), name, font=f, fill=INK)
        fu = font(MONO_R, 34)
        d.text((586, y + 128), unter, font=fu, fill=DIM)
        d.line([(580, y + 186), (1720, y + 186)], fill=LINIE, width=2)
        y += 300

    fuss = font(MONO_R, 28)
    d.text((80, h - 90), "PipeWire · JACK · VBAN   |   #060A12  #0E1624  #18344E  #39DD93  #D6E2F0",
           font=fuss, fill=DIM)
    bild.save(pfad, "PNG")
    return pfad


if __name__ == "__main__":
    ziel = "/home/patrickweppelmann/.hermes/profiles/re/cache/scratch"
    print(icon(f"{ziel}/icon.png"))
    print(marke(f"{ziel}/marke.png"))
