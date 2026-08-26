#!/usr/bin/env python3
"""Validates a binary FBX 7.4 file against the format, independently.

The crate's binary FBX reader and writer were written together, so a round trip
through them proves they agree -- not that either is right. A record's header
carries the absolute offset of its end, and the writer back-patches those after
encoding the body; a reader built from the same understanding would accept a
wrong offset as readily as a right one.

This parser is written from the format's rules rather than from that code, so
it disagrees when the code is wrong about the format rather than merely
inconsistent with itself. It checks:

* the 23-byte magic and the version word;
* every node record's end offset landing exactly where the record ends;
* the property-count and property-list-length fields matching the properties
  actually present;
* raw and deflated arrays expanding to exactly their declared shape, with one
  complete zlib member and no trailing compressed bytes;
* nested lists terminating with a null record, and only where one is expected;
* the footer's id, its alignment padding, the version repeated, and the closing
  magic.

It deliberately does not check that the *scene* is meaningful -- that a Model
has a Geometry, say. The managed exporter writes FBX through the FBX SDK, so
there is no byte-level oracle for scene content; what can be checked without one
is that the container is well formed.

    python3 tools/validate_fbx_binary.py model.fbx   # validate a file
    python3 tools/validate_fbx_binary.py --cli          # export one first
"""

from __future__ import annotations

import struct
import sys
import zlib
from dataclasses import dataclass
from pathlib import Path

MAGIC = b"Kaydara FBX Binary  \x00\x1a\x00"
FOOTER_ID = bytes(
    [0xFA, 0xBC, 0xAB, 0x09, 0xD0, 0xC8, 0xD4, 0x66,
     0xB1, 0x76, 0xFB, 0x83, 0x1C, 0xF7, 0x26, 0x7E]
)
FOOTER_MAGIC = bytes(
    [0xF8, 0x5A, 0x8C, 0x6A, 0xDE, 0xF5, 0xD9, 0x7E,
     0xEC, 0xE9, 0x0C, 0xE3, 0x75, 0x8F, 0x29, 0x0B]
)

# Property type codes and the fixed width of the scalar ones.
SCALAR_WIDTHS = {ord("Y"): 2, ord("C"): 1, ord("I"): 4, ord("F"): 4, ord("D"): 8, ord("L"): 8}
ARRAY_WIDTHS = {ord("f"): 4, ord("d"): 8, ord("l"): 8, ord("i"): 4, ord("b"): 1}
RAW_CODES = {ord("S"), ord("R")}
MAXIMUM_SAFE_DEPTH = 256


class Invalid(Exception):
    pass


@dataclass(frozen=True)
class VerificationLimits:
    maximum_input_bytes: int = 1024 * 1024 * 1024
    maximum_nodes: int = 1_000_000
    maximum_properties: int = 4_000_000
    maximum_depth: int = MAXIMUM_SAFE_DEPTH
    maximum_array_elements: int = 128_000_000
    maximum_total_expanded_array_bytes: int = 1024 * 1024 * 1024


class Budget:
    def __init__(self, limits: VerificationLimits) -> None:
        values = vars(limits)
        if any(value < 0 for value in values.values()):
            raise Invalid("binary FBX verification limits cannot be negative")
        if limits.maximum_depth > MAXIMUM_SAFE_DEPTH:
            raise Invalid(
                f"binary FBX verification depth cannot exceed {MAXIMUM_SAFE_DEPTH}"
            )
        if limits.maximum_input_bytes >= sys.maxsize:
            raise Invalid(
                f"binary FBX input limit must be smaller than {sys.maxsize} bytes"
            )
        self.limits = limits
        self.nodes = 0
        self.properties = 0
        self.expanded_array_bytes = 0

    def charge_node(self, depth: int) -> None:
        if depth >= self.limits.maximum_depth:
            raise Invalid(
                f"binary FBX nesting exceeds {self.limits.maximum_depth} records"
            )
        self.nodes += 1
        if self.nodes > self.limits.maximum_nodes:
            raise Invalid(f"binary FBX exceeds {self.limits.maximum_nodes} nodes")

    def charge_properties(self, count: int) -> None:
        self.properties += count
        if self.properties > self.limits.maximum_properties:
            raise Invalid(
                f"binary FBX exceeds {self.limits.maximum_properties} properties"
            )

    def charge_array(self, count: int, width: int) -> int:
        if count > self.limits.maximum_array_elements:
            raise Invalid(
                f"binary FBX array has {count} elements, exceeding limit "
                f"{self.limits.maximum_array_elements}"
            )
        expanded = count * width
        self.expanded_array_bytes += expanded
        if (
            self.expanded_array_bytes
            > self.limits.maximum_total_expanded_array_bytes
        ):
            raise Invalid(
                "binary FBX arrays exceed the "
                f"{self.limits.maximum_total_expanded_array_bytes} byte "
                "expanded-data budget"
            )
        return expanded


class Reader:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.at = 0

    def take(self, count: int) -> bytes:
        if self.at + count > len(self.data):
            raise Invalid(f"read of {count} at {self.at} runs past the {len(self.data)}-byte file")
        chunk = self.data[self.at : self.at + count]
        self.at += count
        return chunk

    def u32(self) -> int:
        return struct.unpack("<I", self.take(4))[0]


def read_property(reader: Reader, budget: Budget) -> None:
    code = reader.take(1)[0]
    if code in SCALAR_WIDTHS:
        reader.take(SCALAR_WIDTHS[code])
    elif code in RAW_CODES:
        reader.take(reader.u32())
    elif code in ARRAY_WIDTHS:
        count = reader.u32()
        encoding = reader.u32()
        compressed_length = reader.u32()
        expected = budget.charge_array(count, ARRAY_WIDTHS[code])
        payload = reader.take(compressed_length)
        if encoding == 0:
            if compressed_length != expected:
                raise Invalid(
                    f"uncompressed array of {count} {chr(code)} declares "
                    f"{compressed_length} bytes, not {expected}"
                )
        elif encoding == 1:
            validate_deflated_array(payload, expected, chr(code))
        else:
            raise Invalid(f"array encoding {encoding} is neither raw nor deflate")
    else:
        raise Invalid(f"property type {chr(code)!r} ({code}) is not an FBX 7.4 code")


def validate_deflated_array(payload: bytes, expected: int, type_code: str) -> None:
    """Checks one exact zlib member without materialising an unbounded array."""
    decoder = zlib.decompressobj()
    pending = payload
    produced = 0
    while pending:
        maximum = min(64 * 1024, expected - produced + 1)
        try:
            decoded = decoder.decompress(pending, maximum)
        except zlib.error as error:
            raise Invalid(f"cannot inflate {type_code} array: {error}") from error
        produced += len(decoded)
        if produced > expected:
            raise Invalid(
                f"deflated {type_code} array expands beyond its declared "
                f"{expected} bytes"
            )
        remaining = decoder.unconsumed_tail
        if remaining and len(remaining) == len(pending) and not decoded:
            raise Invalid(f"cannot make progress inflating {type_code} array")
        pending = remaining

    if not decoder.eof:
        raise Invalid(f"deflated {type_code} array ends before its zlib stream")
    if decoder.unused_data:
        raise Invalid(f"deflated {type_code} array has bytes after its zlib stream")
    if produced != expected:
        raise Invalid(
            f"deflated {type_code} array expands to {produced} bytes, not {expected}"
        )


def read_node(reader: Reader, depth: int, budget: Budget) -> bool:
    """Reads one record. Returns False for the null record ending a list."""
    start = reader.at
    end_offset = reader.u32()
    property_count = reader.u32()
    property_list_length = reader.u32()
    name_length = reader.take(1)[0]

    if end_offset == 0:
        if property_count or property_list_length or name_length:
            raise Invalid(f"null record at {start} is not all zeros")
        return False
    budget.charge_node(depth)
    budget.charge_properties(property_count)
    name = reader.take(name_length).decode("utf-8", "replace")

    properties_at = reader.at
    for _ in range(property_count):
        read_property(reader, budget)
    consumed = reader.at - properties_at
    if consumed != property_list_length:
        raise Invalid(
            f"node {name!r} declares a {property_list_length}-byte property list "
            f"but its {property_count} properties occupy {consumed}"
        )

    # Anything left before the declared end is a nested list, which must be
    # terminated by its own null record.
    if reader.at < end_offset:
        while read_node(reader, depth + 1, budget):
            pass
    if reader.at != end_offset:
        raise Invalid(
            f"node {name!r} ends at {reader.at} but its header says {end_offset}"
        )
    return True


def validate(
    path: Path, limits: VerificationLimits = VerificationLimits()
) -> list[str]:
    budget = Budget(limits)
    declared_size = path.stat().st_size
    if declared_size > limits.maximum_input_bytes:
        raise Invalid(
            f"binary FBX input is {declared_size} bytes, exceeding limit "
            f"{limits.maximum_input_bytes}"
        )
    # Bound the read itself as well as checking metadata. A regular file could
    # grow after stat(), and Path.read_bytes() would otherwise allocate the new
    # unbounded size before the post-read check had a chance to reject it.
    with path.open("rb") as source:
        data = source.read(limits.maximum_input_bytes + 1)
    if len(data) > limits.maximum_input_bytes:
        raise Invalid(
            f"binary FBX input is {len(data)} bytes, exceeding limit "
            f"{limits.maximum_input_bytes}"
        )
    notes: list[str] = []
    if not data.startswith(MAGIC):
        raise Invalid("the file does not start with the binary FBX magic")

    reader = Reader(data)
    reader.at = len(MAGIC)
    version = reader.u32()
    if version != 7400:
        raise Invalid(f"version {version} is not 7400")

    roots = 0
    while read_node(reader, 0, budget):
        roots += 1
    if roots == 0:
        raise Invalid("the file has no top-level records")
    notes.append(f"{roots} top-level record(s)")

    body_end = reader.at
    footer = data[body_end:]
    if not footer.startswith(FOOTER_ID):
        raise Invalid("the footer does not begin with the footer id")
    # The id plus its padding aligns the whole body to 16 bytes.
    # Reference writers use `16 - (position % 16)`, so an already aligned
    # position receives a complete 16-byte zero block rather than no padding.
    padding = 16 - ((body_end + len(FOOTER_ID)) % 16)
    at = len(FOOTER_ID) + padding
    if any(footer[len(FOOTER_ID) : at]):
        raise Invalid("the footer's alignment padding is not zero")
    tail = footer[at:]
    # Four zero bytes, the version again, 120 reserved zeros, then the magic.
    if len(tail) < 4 + 4 + 120 + len(FOOTER_MAGIC):
        raise Invalid(f"the footer is {len(footer)} bytes, too short to be complete")
    if any(tail[:4]):
        raise Invalid("the four bytes before the footer's version are not zero")
    if struct.unpack_from("<I", tail, 4)[0] != version:
        raise Invalid("the footer does not repeat the version")
    if any(tail[8 : 8 + 120]):
        raise Invalid("the footer's 120-byte reserved block is not zero")
    if tail[8 + 120 :] != FOOTER_MAGIC:
        raise Invalid("the file does not end with the closing footer magic")
    notes.append(f"body {body_end} bytes, footer {len(footer)} bytes")
    return notes


def synthetic_model() -> bytes:
    """A v22 serialized file with one game object, transform, mesh and renderer.

    Built here rather than committed so the input has a readable origin, the
    same reason the audio and ASTC fixtures have generators.
    """

    def pad(value: bytes) -> bytes:
        return value + b"\x00" * (-len(value) % 4)

    def text(value: str) -> bytes:
        return pad(struct.pack("<I", len(value)) + value.encode())

    def i32(value: int) -> bytes:
        return struct.pack("<i", value)

    def floats(values: list[float]) -> bytes:
        return b"".join(struct.pack("<f", value) for value in values)

    def pptr(path_id: int) -> bytes:
        return i32(0) + struct.pack("<q", path_id)

    def packed_float() -> bytes:
        return pad(struct.pack("<I", 0) + floats([0.0, 0.0]) + i32(0)) + pad(b"\x00")

    def packed_int() -> bytes:
        return pad(struct.pack("<I", 0) + i32(0)) + pad(b"\x00")

    game_object = i32(3) + pptr(11) + pptr(21) + pptr(31) + i32(0) + text("root")
    transform = (
        pptr(1) + floats([0, 0, 0, 1]) + floats([2, 3, 4]) + floats([1, 1, 1])
        + i32(0) + pptr(0)
    )
    mesh_filter = pptr(1) + pptr(51)
    renderer = pad(pptr(1) + bytes([1, 2, 1, 0, 0, 0, 0, 0, 0, 0]))
    renderer = pad(
        renderer + struct.pack("<I", 0xFFFFFFFF) + i32(0) + bytes(36) + i32(0)
        + bytes(4) + pptr(0) * 3 + bytes(8)
    )

    mesh = text("tri") + i32(1) + b"".join(struct.pack("<I", v) for v in [0, 3, 0, 0, 0, 3])
    mesh += bytes(24) + i32(0) * 3 + struct.pack("<I", 0) + i32(0) * 5
    mesh = pad(mesh + bytes([0, 1, 0, 0])) + i32(0) + i32(6)
    mesh += b"".join(struct.pack("<H", index) for index in range(3))
    mesh = pad(mesh)
    mesh += struct.pack("<I", 3) + i32(5) + bytes([0, 0, 0, 3]) + bytes(16) + i32(36)
    mesh += floats([0, 0, 0, 1, 0, 0, 0, 1, 0])
    mesh = pad(mesh)
    mesh += packed_float() * 4 + packed_int() * 3 + packed_float() + packed_int() * 2
    mesh += struct.pack("<I", 0) + bytes(24) + i32(0) * 3
    mesh = pad(mesh) + i32(0)
    mesh = pad(mesh) + bytes(8)
    mesh = pad(mesh) + struct.pack("<q", 0) + struct.pack("<I", 0) + text("")

    objects = [
        (1, 1, game_object), (4, 11, transform), (33, 21, mesh_filter),
        (23, 31, renderer), (43, 51, mesh),
    ]
    classes: list[int] = []
    for class_id, _, _ in objects:
        if class_id not in classes:
            classes.append(class_id)

    metadata = bytearray(b"2022.3.62f1\x00") + i32(13) + b"\x00" + i32(len(classes))
    for class_id in classes:
        metadata += i32(class_id) + b"\x00" + struct.pack("<h", -1) + bytes(16)

    body = bytearray()
    records = []
    for class_id, path_id, payload in objects:
        while len(body) % 4:
            body += b"\x00"
        records.append((path_id, len(body), len(payload), classes.index(class_id)))
        body += payload
    metadata += i32(len(records))
    for path_id, offset, size, type_index in records:
        while (48 + len(metadata)) % 4:
            metadata += b"\x00"
        metadata += struct.pack("<q", path_id) + struct.pack("<q", offset)
        metadata += struct.pack("<I", size) + i32(type_index)
    metadata += i32(0) * 3 + b"\x00"

    data_offset = -(-(48 + len(metadata)) // 16) * 16
    header = bytearray(48)
    header[8:12] = struct.pack(">I", 22)
    header[20:24] = struct.pack(">I", len(metadata))
    header[24:32] = struct.pack(">q", data_offset + len(body))
    header[32:40] = struct.pack(">q", data_offset)
    return bytes(header + metadata + bytes(data_offset - 48 - len(metadata)) + body)


def export_and_validate() -> int:
    """Exports a binary FBX through the CLI, then validates what came out."""
    import subprocess
    import tempfile

    root = Path(__file__).resolve().parent.parent
    with tempfile.TemporaryDirectory(prefix="unity-rs-fbx-") as directory:
        assets = Path(directory) / "model.assets"
        assets.write_bytes(synthetic_model())
        output = Path(directory) / "model.fbx"
        result = subprocess.run(
            ["cargo", "run", "--quiet", "-p", "unity-rs-cli", "--locked", "--",
             "fbx", "--binary", str(assets), str(output)],
            cwd=root, capture_output=True, text=True,
        )
        if result.returncode != 0:
            print(result.stderr.strip(), file=sys.stderr)
            return 1
        try:
            for note in validate(output):
                print(f"  {note}")
        except Invalid as error:
            print(f"exported FBX is invalid: {error}", file=sys.stderr)
            return 1
    print("exported binary FBX is valid 7.4")
    return 0


def main() -> int:
    if len(sys.argv) == 2 and sys.argv[1] == "--cli":
        return export_and_validate()
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    path = Path(sys.argv[1])
    try:
        for note in validate(path):
            print(f"  {note}")
    except Invalid as error:
        print(f"{path}: {error}", file=sys.stderr)
        return 1
    print(f"{path}: valid binary FBX 7.4")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
