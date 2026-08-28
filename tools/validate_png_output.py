#!/usr/bin/env python3
"""Decodes the crate's PNG output with an independent implementation.

The PNG encoder here is this project's own -- `flate2` compresses, but the
signature, chunk framing, CRCs, IHDR fields and scanline filters are all
written by hand. The unit test that checks them verifies each chunk's CRC with
`png_crc32`, the project's own CRC routine, so a wrong CRC table would produce
a file the test accepts and every image viewer rejects.

`zlib` in Python's standard library provides both an independent CRC-32 and an
independent inflate. This walks the chunks, checks each CRC against that
implementation, inflates the image data, undoes the per-scanline filters, and
compares the result to the pixels the file was built from. A PNG that survives
all of that is one an unrelated decoder can read.

    python3 tools/validate_png_output.py            # export through the CLI
    python3 tools/validate_png_output.py image.png  # check an existing file
"""

from __future__ import annotations

import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SIGNATURE = b"\x89PNG\r\n\x1a\n"


class Invalid(Exception):
    pass


def unfilter(raw: bytes, width: int, height: int) -> bytes:
    """Reverses PNG's per-scanline filters for 8-bit RGBA."""
    stride = width * 4
    out = bytearray()
    previous = bytearray(stride)
    at = 0
    for row in range(height):
        if at >= len(raw):
            raise Invalid(f"image data ends before scanline {row}")
        kind = raw[at]
        at += 1
        line = bytearray(raw[at : at + stride])
        if len(line) != stride:
            raise Invalid(f"scanline {row} is {len(line)} bytes, expected {stride}")
        at += stride
        for index in range(stride):
            left = line[index - 4] if index >= 4 else 0
            up = previous[index]
            upper_left = previous[index - 4] if index >= 4 else 0
            line[index] = unfilter_byte(
                kind, line[index], left, up, upper_left, row
            )
        out += line
        previous = line
    if at != len(raw):
        raise Invalid(f"{len(raw) - at} bytes remain after the last scanline")
    return bytes(out)


def unfilter_byte(
    kind: int, value: int, left: int, up: int, upper_left: int, row: int
) -> int:
    if kind == 0:
        predictor = 0
    elif kind == 1:
        predictor = left
    elif kind == 2:
        predictor = up
    elif kind == 3:
        predictor = (left + up) // 2
    elif kind == 4:
        predictor = paeth_predictor(left, up, upper_left)
    else:
        raise Invalid(f"scanline {row} uses filter {kind}, which is not 0-4")
    return (value + predictor) & 0xFF


def paeth_predictor(left: int, up: int, upper_left: int) -> int:
    estimate = left + up - upper_left
    distances = (
        abs(estimate - left),
        abs(estimate - up),
        abs(estimate - upper_left),
    )
    if distances[0] <= distances[1] and distances[0] <= distances[2]:
        return left
    if distances[1] <= distances[2]:
        return up
    return upper_left


def read_chunk(data: bytes, at: int) -> tuple[bytes, bytes, int]:
    if at + 8 > len(data):
        raise Invalid(f"a chunk header at {at} runs past the end")
    (length,) = struct.unpack_from(">I", data, at)
    kind = data[at + 4 : at + 8]
    body = data[at + 8 : at + 8 + length]
    if len(body) != length or at + 12 + length > len(data):
        raise Invalid(
            f"chunk {kind.decode('ascii', 'replace')} declares {length} bytes "
            "but the file ends before its data and CRC"
        )
    (stored,) = struct.unpack_from(">I", data, at + 8 + length)
    computed = zlib.crc32(kind + body) & 0xFFFFFFFF
    if stored != computed:
        raise Invalid(
            f"chunk {kind.decode('ascii', 'replace')} has CRC {stored:#010x}; "
            f"zlib computes {computed:#010x}"
        )
    return kind, body, at + 12 + length


def png_header(body: bytes) -> tuple[int, int]:
    width, height, depth, colour, compression, filters, interlace = struct.unpack(
        ">IIBBBBB", body
    )
    if depth != 8 or colour != 6:
        raise Invalid(f"IHDR is depth {depth} colour type {colour}, not 8-bit RGBA")
    if compression or filters or interlace:
        raise Invalid("IHDR declares a non-default compression, filter or interlace")
    return width, height


def decode(data: bytes) -> tuple[int, int, bytes, list[str]]:
    if not data.startswith(SIGNATURE):
        raise Invalid("the file does not start with the PNG signature")

    notes: list[str] = []
    at = len(SIGNATURE)
    header: tuple[int, int] | None = None
    pixels = bytearray()
    chunks: list[str] = []
    while at < len(data):
        kind, body, at = read_chunk(data, at)
        chunks.append(kind.decode("ascii", "replace"))

        if kind == b"IHDR":
            header = png_header(body)
        elif kind == b"IDAT":
            pixels += body
        elif kind == b"IEND":
            if body:
                raise Invalid("IEND is not empty")
            break

    if chunks[0] != "IHDR":
        raise Invalid(f"the first chunk is {chunks[0]}, not IHDR")
    if chunks[-1] != "IEND":
        raise Invalid(f"the last chunk is {chunks[-1]}, not IEND")
    if header is None:
        raise Invalid("the file has no IHDR")
    if at != len(data):
        raise Invalid(f"{len(data) - at} bytes follow IEND")

    width, height = header
    raw = zlib.decompress(bytes(pixels))
    expected = height * (1 + width * 4)
    if len(raw) != expected:
        raise Invalid(f"the image data inflates to {len(raw)} bytes, expected {expected}")
    notes.append(f"{width}x{height} RGBA, {len(chunks)} chunk(s), {chunks.count('IDAT')} IDAT")
    return width, height, unfilter(raw, width, height), notes


def texture_assets(width: int, height: int, pixels: bytes) -> bytes:
    """A v22 file with one RGBA32 Texture2D carrying `pixels`."""

    def pad(value: bytes) -> bytes:
        return value + b"\x00" * (-len(value) % 4)

    def text(value: str) -> bytes:
        return pad(struct.pack("<I", len(value)) + value.encode())

    def i32(value: int) -> bytes:
        return struct.pack("<i", value)

    body = bytearray(text("probe"))
    body += i32(0) + bytes([0, 0])
    body = bytearray(pad(bytes(body)))
    body += i32(width) + i32(height) + struct.pack("<I", len(pixels)) + i32(0)
    body += i32(4) + i32(1)          # RGBA32, one mip
    body += bytes([1, 0, 0])         # readable, preprocessed, ignore mip limit
    body = bytearray(pad(bytes(body)))
    body += text("") + bytes([0])
    body = bytearray(pad(bytes(body)))
    body += i32(0) + i32(1) + i32(2) # priority, image count, dimension
    body += bytes(24) + i32(0) + i32(0) + i32(0)
    body = bytearray(pad(bytes(body)))
    body += struct.pack("<I", len(pixels)) + pixels
    body += struct.pack("<q", 0) + struct.pack("<I", 0) + text("")

    payload = bytes(body)
    metadata = bytearray(b"2022.3.62f1\x00") + i32(13) + b"\x00"
    metadata += i32(1) + i32(28) + b"\x00" + struct.pack("<h", -1) + bytes(16)
    metadata += i32(1)
    while (48 + len(metadata)) % 4:
        metadata += b"\x00"
    metadata += struct.pack("<q", 7) + struct.pack("<q", 0)
    metadata += struct.pack("<I", len(payload)) + i32(0)
    metadata += i32(0) * 3 + b"\x00"

    data_offset = -(-(48 + len(metadata)) // 16) * 16
    header = bytearray(48)
    header[8:12] = struct.pack(">I", 22)
    header[20:24] = struct.pack(">I", len(metadata))
    header[24:32] = struct.pack(">q", data_offset + len(payload))
    header[32:40] = struct.pack(">q", data_offset)
    return bytes(header + metadata + bytes(data_offset - 48 - len(metadata)) + payload)


def export_and_validate() -> int:
    # Every channel differs per pixel, so a swapped component or a mirrored row
    # shows up rather than cancelling out. Unity stores rows bottom-up, so the
    # last row here is the top row of the image.
    width, height = 3, 2
    source = bytes(
        value
        for row in range(height)
        for column in range(width)
        for value in (17 * column + 1, 40 * row + 2, 90 + column, 255 - 30 * row)
    )
    with tempfile.TemporaryDirectory(prefix="unity-rs-png-") as directory:
        work = Path(directory)
        assets = work / "texture.assets"
        assets.write_bytes(texture_assets(width, height, source))
        result = subprocess.run(
            ["cargo", "run", "--quiet", "-p", "unity-rs-cli", "--locked", "--",
             "export", str(assets), str(work / "out")],
            cwd=ROOT, capture_output=True, text=True,
        )
        if result.returncode != 0:
            print(result.stderr.strip(), file=sys.stderr)
            return 1
        pngs = list((work / "out").rglob("*.png"))
        if len(pngs) != 1:
            print(f"expected one PNG, found {pngs}", file=sys.stderr)
            return 1
        try:
            got_width, got_height, decoded, notes = decode(pngs[0].read_bytes())
        except (Invalid, zlib.error) as error:
            print(f"exported PNG is invalid: {error}", file=sys.stderr)
            return 1
        for note in notes:
            print(f"  {note}")
        if (got_width, got_height) != (width, height):
            print(f"PNG is {got_width}x{got_height}, expected {width}x{height}", file=sys.stderr)
            return 1
        # Unity's rows are bottom-up and a PNG's are top-down.
        stride = width * 4
        flipped = b"".join(
            source[row * stride : (row + 1) * stride] for row in reversed(range(height))
        )
        if decoded != flipped:
            print("decoded pixels do not match the source, row order included", file=sys.stderr)
            return 1
    print("exported PNG decodes to the source pixels under an independent reader")
    return 0


def main() -> int:
    if len(sys.argv) == 1:
        return export_and_validate()
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    try:
        width, height, _, notes = decode(Path(sys.argv[1]).read_bytes())
    except (Invalid, zlib.error) as error:
        print(f"{sys.argv[1]}: {error}", file=sys.stderr)
        return 1
    for note in notes:
        print(f"  {note}")
    print(f"{sys.argv[1]}: valid {width}x{height} PNG")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
