# Independent re-implementation of the DDT2 wire format as documented in the
# Aether DV Phase A notes (derived from reading D-Rats ddt2.py, yencode.py,
# crc_checksum.py). Analysis aid only; not production code.
import os
import struct
import zlib

BANNED = bytes([0x11, 0x13, 0x1A, 0x00, 0x84, 0xE7, 0xFD, 0xFE, 0xFF, 0xC0, 0xDB, 0x3D])


def crc(data):
    c = 0
    for b in bytes(data) + b"\0\0":
        for _ in range(8):
            b <<= 1
            bit = (b >> 8) & 1
            if c & 0x8000:
                c = ((c << 1) + bit) ^ 0x1021
            else:
                c = (c << 1) + bit
            c &= 0xFFFF
    return c


def yenc(buf):
    return b"".join(bytes([0x3D, (c + 64) % 256]) if c in BANNED else bytes([c]) for c in buf)


def frame(seq, session, typ, src, dst, data, compress):
    magic = 0xDD if compress else 0x22
    payload = zlib.compress(data, 9) if compress else data
    pad = lambda s: s.encode().ljust(8, b"~")
    hdr0 = struct.pack("!BHBBHH8s8s", magic, seq, session, typ, 0, len(payload), pad(src), pad(dst))
    cs = crc(hdr0 + payload)
    raw = struct.pack("!BHBBHH8s8s", magic, seq, session, typ, cs, len(payload), pad(src), pad(dst)) + payload
    return raw, b"[SOB]" + yenc(raw) + b"[EOB]"


def show(name, *a):
    raw, w = frame(*a)
    print(f"== {name}\n  raw {len(raw)} B: {raw.hex(' ')}\n  wire {len(w)} B: {w!r}\n")


show("chat 'Hello' N0CALL->CQCQCQ (sess 1, type 0, uncompressed)", 0, 1, 0, "N0CALL", "CQCQCQ", b"Hello", False)
show("warm-up (sess 0, type 254, '!'->'!', 16x 0x01, uncompressed)", 0, 0, 254, "!", "!", b"\x01" * 16, False)
show("T_REQACK [0,1,2,3] sess 4 (compressed)", 0, 4, 5, "N0CALL", "W1AW", bytes([0, 1, 2, 3]), True)
print("CRC('123456789') = 0x%04X  (CRC-16/XMODEM reference = 0x31C3)" % crc(b"123456789"))
r = zlib.compress(os.urandom(20000))
print("yEnc expansion, incompressible data: %.2f%%" % ((len(yenc(r)) / len(r) - 1) * 100))
for bs in (256, 512, 1024):
    raw, w = frame(1, 4, 4, "N0CALL", "W1AW", os.urandom(bs), True)
    print(f"T_DAT block {bs} B of incompressible data -> {len(w)} B on serial ({len(w)/bs:.3f}x)")
