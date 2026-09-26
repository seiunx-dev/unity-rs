#!/usr/bin/env python3
"""Decodes the ASTC fixtures with Khronos `astcenc` into reference blobs.

The `astc-<variant>-<N>x<N>-astcenc.rgba` blobs are the normative reference
for this crate's ASTC decoding: they are what the ASTC specification says
the payloads decode to, produced by the specification's own reference codec
rather than by another port of the AssetStudio decoder lineage.
`ldr_astc_decodes_exactly_like_the_khronos_reference` and
`hdr_astc_decodes_exactly_like_the_khronos_reference` in `texture.rs` pin the
crate's output against them byte for byte, and the managed differential
re-earns the LDR ones against the crate's live output on every run.

This script regenerates the blobs from the committed `.bin` payloads. It wraps
each payload in the `.astc` container `astcenc` reads. LDR payloads go through
`astcenc -dl` to a TGA, which is normalized (BGRA to RGBA, origin to top-down)
into the raw pixel order `decode_mip_rgba8` returns. HDR payloads go through
`astcenc -dH` to a half-float DDS, whose top-down rows are already in that
order; each channel is clamped to [0, 1] and rounded to eight bits, the
conversion this crate applies to decoded HDR values. It needs the official
`astcenc` command-line codec, 4.x or newer; the committed blobs came from
astcenc 5.7.0.

Usage, from the repository root:

    python3 tools/decode_astc_references.py /path/to/astcenc
"""

from __future__ import annotations

import math
import os
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

FIXTURES = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "unity-rs-core"
    / "tests"
    / "fixtures"
    / "astc"
)


def trusted_astcenc(argument: str) -> Path:
    """Resolve an explicitly trusted official astcenc executable."""
    try:
        executable = Path(argument).expanduser().resolve(strict=True)
    except OSError as error:
        raise ValueError(f"astcenc binary cannot be resolved: {argument}: {error}") from error
    if not executable.is_file():
        raise ValueError(f"astcenc binary is not a regular file: {executable}")
    if not executable.name.startswith("astcenc"):
        raise ValueError(f"astcenc binary has an unexpected name: {executable.name}")
    if not os.access(executable, os.X_OK):
        raise ValueError(f"astcenc binary is not executable: {executable}")
    return executable

# Unity's six ASTC footprints. Only square blocks appear in its texture formats.
BLOCK_SIZES = (4, 5, 6, 8, 10, 12)

ASTC_MAGIC = b"\x13\xab\xa1\x5c"

# (variant, astcenc decode mode, output suffix). The suffix selects the
# reader: TGA for the eight-bit LDR decode, DDS for the half-float HDR one.
VARIANTS = (
    ("rgb", "-dl", ".tga"),
    ("rgba", "-dl", ".tga"),
    ("hdr", "-dH", ".dds"),
    ("hdr-glow", "-dH", ".dds"),
)

DDS_MAGIC = b"DDS "
DDS_HEADER_SIZE = 124
DDS_DX10_HEADER_SIZE = 20
DDS_FOURCC_DX10 = b"DX10"
# D3DFMT_A16B16G16R16F, the legacy four-character code for RGBA half floats.
DDS_FOURCC_RGBA16F = struct.pack("<I", 113)
DXGI_FORMAT_R16G16B16A16_FLOAT = 10


def wrap_astc(payload: bytes, block: int, width: int, height: int) -> bytes:
    """The `.astc` container: magic, block footprint, 24-bit dimensions."""
    return (
        ASTC_MAGIC
        + struct.pack("<BBB", block, block, 1)
        + struct.pack("<I", width)[:3]
        + struct.pack("<I", height)[:3]
        + struct.pack("<I", 1)[:3]
        + payload
    )


def decode_tga_payload(payload: bytes, kind: int, want: int) -> bytes:
    """Decode the raw or RLE-compressed TGA pixel payload."""
    if kind == 2:
        return payload[:want]
    out = bytearray()
    offset = 0
    while len(out) < want:
        packet = payload[offset]
        offset += 1
        count = (packet & 0x7F) + 1
        if packet & 0x80:
            out += payload[offset : offset + 4] * count
            offset += 4
        else:
            out += payload[offset : offset + count * 4]
            offset += count * 4
    return bytes(out[:want])


def bgra_rows_to_rgba(rows: list[bytes], width: int) -> bytes:
    """Convert top-down BGRA rows to a contiguous RGBA image."""
    pixels = bytearray(len(rows) * width * 4)
    for y, row in enumerate(rows):
        for x in range(width):
            b, g, r, a = row[x * 4 : x * 4 + 4]
            start = (y * width + x) * 4
            pixels[start : start + 4] = bytes((r, g, b, a))
    return bytes(pixels)


def read_tga(data: bytes, width: int, height: int) -> bytes:
    """Normalize a 32-bit truecolor TGA to top-down RGBA."""
    if data[0] != 0 or data[1] != 0:
        sys.exit("unexpected TGA id/colormap fields")
    kind = data[2]
    if kind not in (2, 10):
        sys.exit(f"expected a truecolor TGA, got image type {kind}")
    if (
        struct.unpack_from("<H", data, 12)[0] != width
        or struct.unpack_from("<H", data, 14)[0] != height
        or data[16] != 32
    ):
        sys.exit("TGA dimensions or depth do not match the fixture")
    top_down = bool(data[17] & 0x20)
    payload = data[18:]
    want = width * height * 4
    decoded = decode_tga_payload(payload, kind, want)
    rows = [decoded[y * width * 4 : (y + 1) * width * 4] for y in range(height)]
    if not top_down:
        rows.reverse()
    return bgra_rows_to_rgba(rows, width)


def half_to_unorm8(value: float) -> int:
    """Clamp a decoded HDR channel to [0, 1] and round it to eight bits."""
    if math.isnan(value):
        raise ValueError("astcenc produced a NaN channel: the payload holds an error block")
    return math.floor(min(max(value, 0.0), 1.0) * 255.0 + 0.5)


def read_dds(data: bytes, width: int, height: int) -> bytes:
    """Convert a top-down RGBA half-float DDS to eight-bit RGBA."""
    if len(data) < 4 + DDS_HEADER_SIZE or data[:4] != DDS_MAGIC:
        raise ValueError("not a DDS file")
    if struct.unpack_from("<I", data, 4)[0] != DDS_HEADER_SIZE:
        raise ValueError("unexpected DDS header size")
    dds_height, dds_width = struct.unpack_from("<II", data, 12)
    if (dds_width, dds_height) != (width, height):
        raise ValueError("DDS dimensions do not match the fixture")
    fourcc = data[84:88]
    offset = 4 + DDS_HEADER_SIZE
    if fourcc == DDS_FOURCC_DX10:
        if len(data) < offset + DDS_DX10_HEADER_SIZE:
            raise ValueError("DDS DX10 header is truncated")
        if struct.unpack_from("<I", data, offset)[0] != DXGI_FORMAT_R16G16B16A16_FLOAT:
            raise ValueError("expected an RGBA half-float DDS")
        offset += DDS_DX10_HEADER_SIZE
    elif fourcc != DDS_FOURCC_RGBA16F:
        raise ValueError(f"expected an RGBA half-float DDS, got four-character code {fourcc!r}")
    count = width * height * 4
    if len(data) - offset < count * 2:
        raise ValueError("DDS pixel data is truncated")
    values = struct.unpack_from(f"<{count}e", data, offset)
    return bytes(half_to_unorm8(value) for value in values)


def decode_fixture(
    astcenc: Path, scratch: Path, name: str, block: int, mode: str, suffix: str
) -> bytes:
    """Decode one committed payload with astcenc into raw RGBA8 pixels."""
    payload = (FIXTURES / f"{name}.bin").read_bytes()
    size = block * 2
    wrapped = scratch / f"{name}.astc"
    wrapped.write_bytes(wrap_astc(payload, block, size, size))
    decoded = scratch / f"{name}{suffix}"
    subprocess.run(
        [str(astcenc), mode, str(wrapped), str(decoded)],
        check=True,
        capture_output=True,
        shell=False,
    )
    if suffix == ".tga":
        return read_tga(decoded.read_bytes(), size, size)
    return read_dds(decoded.read_bytes(), size, size)


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(__doc__.strip())
    try:
        astcenc = trusted_astcenc(sys.argv[1])
    except ValueError as error:
        sys.exit(str(error))

    with tempfile.TemporaryDirectory() as scratch_name:
        scratch = Path(scratch_name)
        for block in BLOCK_SIZES:
            for variant, mode, suffix in VARIANTS:
                name = f"astc-{variant}-{block}x{block}"
                try:
                    pixels = decode_fixture(astcenc, scratch, name, block, mode, suffix)
                except ValueError as error:
                    sys.exit(f"{name}: {error}")
                out = FIXTURES / f"{name}-astcenc.rgba"
                out.write_bytes(pixels)
                print(f"{out.name}: {len(pixels)} bytes")


if __name__ == "__main__":
    main()
