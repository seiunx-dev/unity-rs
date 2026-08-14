#!/usr/bin/env python3
"""Encodes the ASTC payloads the texture differential compares.

ASTC was the one block format held out of the managed differential. The other
formats are fed pseudorandom bytes, which works because every bit pattern is a
valid encoding of *something*; ASTC is not like that. Random bytes hit
reserved block encodings that no encoder emits, and the two implementations
handle those differently by design -- the managed decoder substitutes an error
colour, this crate rejects the block -- so a random-payload comparison would
report a disagreement that says nothing about either decoder.

Real encoder output has no reserved blocks in it, which makes the comparison
meaningful. These payloads come from ARM's `astcenc` via the `astc-encoder-py`
binding, encoding a deterministic gradient at each of Unity's six ASTC block
sizes.

The source image is generated here rather than committed: it is a formula, and
a formula that both this script and the reader can check beats an opaque PNG.

Usage, from the repository root, with `astc-encoder-py` installed:

    python3 tools/generate_astc_fixtures.py
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

try:
    import astc_encoder as astc
except ImportError:
    sys.exit("astc-encoder-py is required: pip install astc-encoder-py")

FIXTURES = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "assetstudio-core"
    / "tests"
    / "fixtures"
    / "astc"
)

# Unity's six ASTC footprints. Only square blocks appear in its texture formats.
BLOCK_SIZES = (4, 5, 6, 8, 10, 12)


def gradient(width: int, height: int, *, opaque: bool) -> bytes:
    """A smooth RGBA gradient with a diagonal alpha ramp.

    Smooth is deliberate: ASTC is a block codec, and a gradient makes each
    block's endpoint pair and weight grid do real work, where flat colour would
    collapse to a single endpoint and compare almost nothing.
    """
    pixels = bytearray()
    for y in range(height):
        for x in range(width):
            red = (x * 255) // max(width - 1, 1)
            green = (y * 255) // max(height - 1, 1)
            blue = ((x + y) * 255) // max(width + height - 2, 1)
            alpha = 255 if opaque else 255 - ((x + y) * 255) // max(width + height - 2, 1)
            pixels += bytes((red, green, blue, alpha))
    return bytes(pixels)


def gradient_hdr(width: int, height: int) -> bytes:
    """The same ramp in half floats, spanning a range LDR cannot hold."""
    pixels = bytearray()
    for y in range(height):
        for x in range(width):
            red = 4.0 * x / max(width - 1, 1)
            green = 4.0 * y / max(height - 1, 1)
            blue = 0.25 + (x + y) / max(width + height - 2, 1)
            pixels += struct.pack("<4e", red, green, blue, 1.0)
    return bytes(pixels)


def encode(profile: int, block: int, image_type: int, width: int,
           height: int, pixels: bytes) -> bytes:
    config = astc.ASTCConfig(profile, block, block, 1,
                             astc.ASTCQualityPreset.THOROUGH)
    context = astc.ASTCContext(config)
    image = astc.ASTCImage(image_type, width, height, 1, pixels)
    return context.compress(image, astc.ASTCSwizzle.from_str("RGBA"))


def main() -> None:
    FIXTURES.mkdir(parents=True, exist_ok=True)
    written = []
    for block in BLOCK_SIZES:
        # Two blocks each way, so a fixture covers block-to-block placement and
        # not just the decode of a single block.
        width = height = block * 2
        variants = (
            ("rgb", astc.ASTCProfile.LDR, astc.ASTCType.U8,
             gradient(width, height, opaque=True)),
            ("rgba", astc.ASTCProfile.LDR, astc.ASTCType.U8,
             gradient(width, height, opaque=False)),
            ("hdr", astc.ASTCProfile.HDR, astc.ASTCType.F16,
             gradient_hdr(width, height)),
        )
        for name, profile, image_type, pixels in variants:
            payload = encode(profile, block, image_type, width, height, pixels)
            expected = (width // block) * (height // block) * 16
            if len(payload) != expected:
                raise ValueError(
                    f"{name} {block}x{block}: {len(payload)} bytes, expected {expected}"
                )
            path = FIXTURES / f"astc-{name}-{block}x{block}.bin"
            path.write_bytes(payload)
            written.append(f"{path.name} ({width}x{height}, {len(payload)} bytes)")
    for line in written:
        print(f"wrote {line}")


if __name__ == "__main__":
    main()
