#!/usr/bin/env python3
"""Decodes the crate's BMP and TGA output against the two formats' rules.

Both encoders are this project's own, and both are the kind of layout where a
wrong field survives its author's tests. Every reader of a 32-bit BMP that
assumes BGRA reads a file whose channel masks say something else and still
produces the right picture; every reader of a TGA that assumes top-down reads a
file whose descriptor says bottom-up and still produces the right picture --
right side up, even, until the day it meets a reader that believes the header.
The redundant fields are the same story: the file size, the pixel offset and
the image size are all derivable, so a wrong one changes nothing until it does.

This decodes both formats the way the specifications say to -- taking the row
order from the header rather than assuming it, and taking the BMP channel
layout from the masks the file declares rather than assuming BGRA -- and
compares the result to the pixels the file was built from.

    python3 tools/validate_image_output.py            # export through the CLI
    python3 tools/validate_image_output.py image.bmp  # check an existing file
"""

from __future__ import annotations

import struct
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from validate_png_output import Invalid, texture_assets  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent

BMP_FILE_HEADER = 14
BMP_V4_HEADER = 108


def shift_and_max(mask: int) -> tuple[int, int]:
    """Returns how far a channel is shifted and how wide it is."""
    if mask == 0:
        raise Invalid("a channel mask is zero, so that channel cannot be read")
    shift = (mask & -mask).bit_length() - 1
    width = (mask >> shift).bit_length()
    if mask >> shift != (1 << width) - 1:
        raise Invalid(f"channel mask {mask:#010x} is not a contiguous run of bits")
    return shift, (1 << width) - 1


def decode_bmp(data: bytes) -> tuple[int, int, bytes, list[str]]:
    if len(data) < BMP_FILE_HEADER + BMP_V4_HEADER:
        raise Invalid(f"the file is {len(data)} bytes, shorter than a V4 BMP header")
    magic, file_size, _, _, pixel_offset = struct.unpack_from("<2sIHHI", data, 0)
    if magic != b"BM":
        raise Invalid("the file does not start with BM")
    if file_size != len(data):
        raise Invalid(f"the header declares {file_size} bytes; the file is {len(data)}")

    (
        header_size,
        width,
        height,
        planes,
        bits,
        compression,
        image_size,
    ) = struct.unpack_from("<IiiHHII", data, BMP_FILE_HEADER)
    if header_size != BMP_V4_HEADER:
        raise Invalid(f"the DIB header is {header_size} bytes, not a V4 header's 108")
    if pixel_offset != BMP_FILE_HEADER + header_size:
        raise Invalid(
            f"pixels start at {pixel_offset}; the headers end at "
            f"{BMP_FILE_HEADER + header_size}"
        )
    if planes != 1 or bits != 32:
        raise Invalid(f"the image declares {planes} plane(s) at {bits} bits, not 1 at 32")
    if compression != 3:
        raise Invalid(f"compression is {compression}, not BI_BITFIELDS")
    if width <= 0:
        raise Invalid(f"width {width} is not positive")

    # A negative height means the rows are stored top-down.
    top_down = height < 0
    height = abs(height)
    stride = width * 4
    if image_size != stride * height:
        raise Invalid(f"the image size field is {image_size}, not {stride * height}")

    masks = struct.unpack_from("<IIII", data, BMP_FILE_HEADER + 40)
    pixels = data[pixel_offset : pixel_offset + image_size]
    if len(pixels) != image_size:
        raise Invalid(f"{len(pixels)} bytes of pixels are present, {image_size} declared")

    # Read the channels the masks describe rather than assuming a layout.
    channels = [shift_and_max(mask) for mask in masks]
    out = bytearray()
    for row in range(height):
        source = row if top_down else height - 1 - row
        line = pixels[source * stride : (source + 1) * stride]
        for at in range(0, stride, 4):
            (value,) = struct.unpack_from("<I", line, at)
            for shift, maximum in channels:
                out.append((value >> shift) & maximum)
    notes = [
        f"{width}x{height} BMP, {'top-down' if top_down else 'bottom-up'}, "
        f"masks {'/'.join(f'{mask:#010x}' for mask in masks)}"
    ]
    return width, height, bytes(out), notes


def decode_tga(data: bytes) -> tuple[int, int, bytes, list[str]]:
    if len(data) < 18:
        raise Invalid(f"the file is {len(data)} bytes, shorter than a TGA header")
    (
        id_length,
        colour_map_type,
        image_type,
        _,
        colour_map_length,
        _,
        x_origin,
        y_origin,
        width,
        height,
        bits,
        descriptor,
    ) = struct.unpack_from("<BBBHHBHHHHBB", data, 0)
    if image_type != 2:
        raise Invalid(f"image type {image_type} is not uncompressed true-colour")
    if colour_map_type or colour_map_length:
        raise Invalid("a true-colour image declares a colour map")
    if bits != 32:
        raise Invalid(f"the image is {bits} bits per pixel, not 32")
    if x_origin or y_origin:
        raise Invalid(f"the image origin is ({x_origin}, {y_origin}), not (0, 0)")
    if descriptor & 0x0F != 8:
        raise Invalid(f"the descriptor declares {descriptor & 0x0F} alpha bits, not 8")

    # Bit 5 of the descriptor is the vertical origin: set means top-down.
    top_down = bool(descriptor & 0x20)
    if descriptor & 0x10:
        raise Invalid("the descriptor declares a right-to-left image")
    stride = width * 4
    start = 18 + id_length
    pixels = data[start : start + stride * height]
    if len(pixels) != stride * height:
        raise Invalid(
            f"{len(pixels)} bytes of pixels are present, {stride * height} implied"
        )
    if start + len(pixels) != len(data):
        raise Invalid(f"{len(data) - start - len(pixels)} bytes follow the pixel data")

    out = bytearray()
    for row in range(height):
        source = row if top_down else height - 1 - row
        line = pixels[source * stride : (source + 1) * stride]
        for at in range(0, stride, 4):
            blue, green, red, alpha = line[at : at + 4]
            out += bytes((red, green, blue, alpha))
    notes = [f"{width}x{height} TGA, {'top-down' if top_down else 'bottom-up'}"]
    return width, height, bytes(out), notes


DECODERS = {".bmp": decode_bmp, ".tga": decode_tga}


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
    stride = width * 4
    flipped = b"".join(
        source[row * stride : (row + 1) * stride] for row in reversed(range(height))
    )

    checked = 0
    with tempfile.TemporaryDirectory(prefix="unity-rs-image-") as directory:
        for extension, decode in DECODERS.items():
            work = Path(directory) / extension[1:]
            work.mkdir()
            assets = work / "texture.assets"
            assets.write_bytes(texture_assets(width, height, source))
            result = subprocess.run(
                ["cargo", "run", "--quiet", "-p", "unity-rs-cli", "--locked", "--",
                 "export", "--image-format", extension[1:], str(assets), str(work / "out")],
                cwd=ROOT, capture_output=True, text=True,
            )
            if result.returncode != 0:
                print(result.stderr.strip(), file=sys.stderr)
                return 1
            written = list((work / "out").rglob(f"*{extension}"))
            if len(written) != 1:
                print(f"expected one {extension} file, found {written}", file=sys.stderr)
                return 1
            try:
                got_width, got_height, decoded, notes = decode(written[0].read_bytes())
            except Invalid as error:
                print(f"exported {extension} is invalid: {error}", file=sys.stderr)
                return 1
            for note in notes:
                print(f"  {note}")
            if (got_width, got_height) != (width, height):
                print(
                    f"{extension} is {got_width}x{got_height}, expected {width}x{height}",
                    file=sys.stderr,
                )
                return 1
            if decoded != flipped:
                print(
                    f"{extension} pixels do not match the source, row order included",
                    file=sys.stderr,
                )
                return 1
            checked += 1
    print(f"{checked} exported image(s) decode to the source pixels under a second reader")
    return 0


def main() -> int:
    if len(sys.argv) == 1:
        return export_and_validate()
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    path = Path(sys.argv[1])
    decode = DECODERS.get(path.suffix.lower())
    if decode is None:
        print(f"{path}: not a .bmp or .tga file", file=sys.stderr)
        return 2
    try:
        width, height, _, notes = decode(path.read_bytes())
    except Invalid as error:
        print(f"{path}: {error}", file=sys.stderr)
        return 1
    for note in notes:
        print(f"  {note}")
    print(f"{path}: valid {width}x{height} image")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
