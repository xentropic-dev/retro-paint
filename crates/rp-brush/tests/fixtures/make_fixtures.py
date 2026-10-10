#!/usr/bin/env python3
"""Regenerate the rp-brush test fixtures in this directory.

Every fixture is synthetic (no GIMP data). Run: python3 make_fixtures.py
"""
import os
import struct

HERE = os.path.dirname(os.path.abspath(__file__))


def write(name, data):
    with open(os.path.join(HERE, name), "wb") as f:
        f.write(data)


def gbr_v1(name, w, h, bpp, pixels):
    n = name.encode() + b"\0"
    return struct.pack(">5I", 20 + len(n), 1, w, h, bpp) + n + bytes(pixels)


def gbr_v2(name, w, h, bpp, spacing, pixels):
    n = name.encode() + b"\0"
    header = struct.pack(">5I", 28 + len(n), 2, w, h, bpp) + b"GIMP"
    return header + struct.pack(">I", spacing) + n + bytes(pixels)


def vbr(*lines):
    return ("GIMP-VBR\n" + "\n".join(str(l) for l in lines) + "\n").encode()


write("v1-gray.gbr", gbr_v1("v1 gray", 3, 2, 1, [0, 128, 255, 10, 20, 30]))
write("v2-gray.gbr", gbr_v2("v2 gray", 4, 3, 1, 75, [i * 20 for i in range(12)]))
write(
    "v2-rgba.gbr",
    gbr_v2(
        "v2 rgba", 2, 2, 4, 40,
        [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 10, 20, 30, 64],
    ),
)
# v1.5: name, shape, spacing, radius, spikes, hardness, aspect ratio, angle
write("star.vbr", vbr("1.5", "test star", "diamond", "50.000000", "25.000000", 5,
                      "1.000000", "2.500000", "17.500000"))
write("one-pixel.vbr", vbr("1.5", "one pixel", "square", "20.000000", "1.000000", 2,
                           "1.000000", "1.000000", "0.000000"))
# v1.0: name, spacing, radius, hardness, aspect ratio, angle
write("soft.vbr", vbr("1.0", "soft", "10.000000", "25.000000", "0.500000",
                      "1.000000", "0.000000"))
