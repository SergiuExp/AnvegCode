#!/usr/bin/env python3
"""Pack PNG renders of the app icon into a macOS .icns file.

    scripts/make-icns.py <dir with icon_16.png … icon_1024.png> <out.icns>

macOS reads PNG data in these icon types, so no Apple tool is needed
(`iconutil` would give the same result on a Mac). The PNGs come from
assets/icon/AppIcon.svg, rendered at each size (scripts/render-icon.js does
that with Playwright's Chromium; any SVG renderer will do).
"""
import struct
import sys
from pathlib import Path

# icns type -> the PNG size (in pixels) it holds.
TYPES = [
    (b"icp4", 16),
    (b"icp5", 32),
    (b"icp6", 64),
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),  # 512@2x
    (b"ic11", 32),  # 16@2x
    (b"ic12", 64),  # 32@2x
    (b"ic13", 256),  # 128@2x
    (b"ic14", 512),  # 256@2x
]


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    body = b""
    for kind, px in TYPES:
        png = (src / f"icon_{px}.png").read_bytes()
        if png[:8] != b"\x89PNG\r\n\x1a\n":
            print(f"icon_{px}.png is not a PNG", file=sys.stderr)
            return 1
        body += kind + struct.pack(">I", 8 + len(png)) + png
    out.write_bytes(b"icns" + struct.pack(">I", 8 + len(body)) + body)
    print(f"wrote {out} ({out.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
