#!/usr/bin/env python3
"""vbunpack.py - Entpacker fuer VB-Audio-Installer-Pakete (RCDATA type 10 / id 100).

Aufruf:
    python3 tools/vbunpack.py <setup.exe|blob.bin> <zielordner>

Beides wird akzeptiert: die Setup-EXE direkt (ressource wird gesucht) oder der
bereits extrahierte RCDATA-Blob.

Nachgebaut aus Voicemeeter8Setup.exe 3.1.3.0, Ghidra 12.1.4
(FUN_00417610 Datei-Schleife, FUN_0040fd20 MD5-thiscall, FUN_00401050 memcmp,
 XOR-Schleife @0x4177e6..0x417803). Das Verfahren ist ein reiner
XOR-Stromschluessel mit MD5-Verkettung - keine Blockchiffre.

Format des Blobs
----------------
Verzeichnistabelle ab Blob-Offset 0, Eintraege zu je 160 Byte:

    0x00  char[128]  Dateiname (NUL-terminiert, Rueckwaertsschraege als Trenner)
    0x80  u32        Offset der Nutzdaten im Blob (ab Blob-Anfang)
    0x84  u32        Groesse der Nutzdaten
    0x88  byte[16]   MD5 des KLARTEXTES dieser Datei
    0x98  u64        Zusatzfeld (in 3.1.3.0 konstant, vom Installer unbenutzt)

Die Tabelle endet beim ersten Eintrag ohne druckbaren Namen; die Nutzdaten
folgen lueckenlos. Der Installer liest genau 0x5460 Byte (135 Eintraege) und
ignoriert den Rest.

Schluesselkette
---------------
    key_0   = 16 Nullbytes
    acc_0   = 0x564A5D6F
    fuer jede Datei (in Tabellenreihenfolge):
        ct      = Nutzdaten (identische Laenge wie der Klartext)
        pt[i]  ^= ( key[i_size & 0xF] + ((sq>>8)&0xFF) + (sq&0xFF) ) & 0xFF
                  mit  sq = int32(acc + (size - i))^2   (32-Bit-Produkt)
                       acc = int32(sq) >> 8              (arithmetisch)
        key_next = MD5(ct)                 <- Pruelfeld 0x88 ist MD5(pt)
        acc_next = ((acc*acc & 0xFFFFFFFF) << 8 | key_next[0]) & 0xFFFFFFFF
"""
import struct
import hashlib
import os
import sys

SEED0 = 0x564A5D6F
M32 = 0xFFFFFFFF
RCDATA_ID = 100          # FindResourceA(h, 0x64, 0xa)
RCDATA_TYPE = 10
TABLE_LEN = 0x5460       # 135 * 160


def i32(v):
    v &= M32
    return v - (1 << 32) if v & 0x80000000 else v


def decrypt(buf, key, seed):
    out = bytearray(buf)
    acc = seed
    n = len(out)
    for p in range(n):
        c = n - p
        sq = (i32(acc) + c) ** 2
        ecx = i32(sq) >> 8
        out[p] ^= (key[c & 0xF] + (ecx & 0xFF) + (sq & 0xFF)) & 0xFF
        acc = ecx & M32
    return bytes(out)


def pe_sections(data):
    e = struct.unpack_from('<I', data, 0x3C)[0]
    nsec = struct.unpack_from('<H', data, e + 6)[0]
    opt = e + 24
    sec = opt + struct.unpack_from('<H', data, e + 20)[0]
    out = []
    for i in range(nsec):
        o = sec + i * 40
        vs, va, rs, ra = struct.unpack_from('<IIII', data, o + 8)
        out.append((va, vs, ra, rs))
    return out, opt


def rsrc_blob(data):
    """Liefert den RCDATA-Blob (type 10, id 100) aus einer PE-Datei."""
    secs, opt = pe_sections(data)

    def rva2off(rva):
        for va, vs, ra, rs in secs:
            if va <= rva < va + max(vs, rs):
                return ra + (rva - va)
        return None

    base = rva2off(struct.unpack_from('<I', data, opt + 96 + 2 * 8)[0])
    if base is None:
        raise SystemExit('keine Ressourcen gefunden')

    def entries(off):
        n = struct.unpack_from('<H', data, base + off + 12)[0] + \
            struct.unpack_from('<H', data, base + off + 14)[0]
        out = []
        for i in range(n):
            eo = base + off + 16 + i * 8
            nm, tgt = struct.unpack_from('<II', data, eo)
            out.append((nm, tgt & 0x7FFFFFFF, bool(tgt & 0x80000000)))
        return out

    for name, tgt, isdir in entries(0):
        if name != RCDATA_TYPE or not isdir:
            continue
        for sub, sub_tgt, _sub_dir in entries(tgt):
            if sub != RCDATA_ID:
                continue
            for _lang, leaf, leaf_dir in entries(sub_tgt):
                if leaf_dir:
                    continue
                drva, dsz = struct.unpack_from('<II', data, base + leaf)
                o = rva2off(drva)
                return data[o:o + dsz]
    raise SystemExit('RCDATA 10/100 nicht gefunden')


def parse_dir(blob):
    off, ents = 0, []
    while off + 160 <= len(blob) and off < TABLE_LEN:
        raw = blob[off:off + 128]
        name = raw.split(b'\0')[0]
        if not name or not all(32 <= c < 127 for c in name):
            break
        d_off, size = struct.unpack_from('<II', blob, off + 0x80)
        md5 = blob[off + 0x88:off + 0x98]
        ents.append((name.decode('ascii'), d_off, size, md5))
        off += 160
    return ents


def main(argv):
    if len(argv) != 3:
        print(__doc__)
        return 2
    src, dst = argv[1], argv[2]
    data = open(src, 'rb').read()
    blob = rsrc_blob(data) if data[:2] == b'MZ' else data
    ents = parse_dir(blob)
    print(f'{len(ents)} Eintraege, Nutzdaten {len(blob)-TABLE_LEN} Byte')
    key, seed, bad = bytes(16), SEED0, []
    for name, d_off, size, md5_plain in ents:
        ct = blob[d_off:d_off + size]
        pt = decrypt(ct, key, seed)
        ok = hashlib.md5(pt).digest() == md5_plain
        if not ok:
            bad.append(name)
        path = os.path.join(dst, *name.replace('/', '\\').split('\\'))
        os.makedirs(os.path.dirname(path) or dst, exist_ok=True)
        with open(path, 'wb') as f:
            f.write(pt)
        print(f"{'ok ' if ok else 'BAD'}  {size:>9}  {name}")
        key = hashlib.md5(ct).digest()
        seed = ((((seed * seed) & M32) << 8) & M32) | key[0]
    print(f'\nPruefsummen OK: {len(ents)-len(bad)}/{len(ents)}')
    return 0 if not bad else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv))
