#!/usr/bin/env python3
"""Regression tests for the independent binary FBX container verifier."""

from __future__ import annotations

import struct
import tempfile
import zlib
from pathlib import Path

from validate_fbx_binary import (
    FOOTER_ID,
    FOOTER_MAGIC,
    MAGIC,
    Invalid,
    VerificationLimits,
    validate,
)


def finish_body(body: bytes, version: int = 7400) -> tuple[bytes, int, int]:
    padding = 16 - ((len(body) + len(FOOTER_ID)) % 16)
    footer = (
        FOOTER_ID
        + bytes(padding)
        + bytes(4)
        + struct.pack("<I", version)
        + bytes(120)
        + FOOTER_MAGIC
    )
    return body + footer, len(body), padding


def minimal_fbx(
    name: bytes,
    properties: bytes = b"",
    property_count: int = 0,
) -> tuple[bytes, int, int]:
    """Builds one leaf record without using the Rust writer under test."""
    version = 7400
    node_end = len(MAGIC) + 4 + 13 + len(name) + len(properties)
    node = (
        struct.pack("<IIIB", node_end, property_count, len(properties), len(name))
        + name
        + properties
    )
    body = MAGIC + struct.pack("<I", version) + node + bytes(13)
    return finish_body(body, version)


def nested_fbx() -> bytes:
    """Builds root -> child with independently calculated absolute offsets."""
    version = 7400
    root_name = b"root"
    child_name = b"child"
    root_start = len(MAGIC) + 4
    child_start = root_start + 13 + len(root_name)
    child_end = child_start + 13 + len(child_name)
    child = struct.pack("<IIIB", child_end, 0, 0, len(child_name)) + child_name
    root_end = child_end + 13
    root = (
        struct.pack("<IIIB", root_end, 0, 0, len(root_name))
        + root_name
        + child
        + bytes(13)
    )
    body = MAGIC + struct.pack("<I", version) + root + bytes(13)
    return finish_body(body, version)[0]


def validate_bytes(
    data: bytes, limits: VerificationLimits = VerificationLimits()
) -> list[str]:
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "fixture.fbx"
        path.write_bytes(data)
        return validate(path, limits)


def assert_rejected(data: bytes, limits: VerificationLimits, expected: str) -> None:
    try:
        validate_bytes(data, limits)
    except Invalid as error:
        assert expected in str(error), error
    else:
        raise AssertionError(f"verification limit did not reject {expected}")


def main() -> None:
    ordinary, ordinary_body, ordinary_padding = minimal_fbx(b"root")
    assert ordinary_padding == 7
    assert f"body {ordinary_body} bytes" in validate_bytes(ordinary)[-1]

    boundary, boundary_body, boundary_padding = minimal_fbx(b"elevenbytes")
    assert boundary_body % 16 == 0
    assert boundary_padding == 16
    assert f"body {boundary_body} bytes" in validate_bytes(boundary)[-1]

    bool_property = b"b" + struct.pack("<III", 3, 0, 3) + bytes([0, 1, 255])
    bool_array, _, _ = minimal_fbx(b"bools", bool_property, 1)
    assert "1 top-level record(s)" in validate_bytes(bool_array)

    bool_values = bytes([0, 1, 255]) * 100
    compressed = zlib.compress(bool_values)
    compressed_property = (
        b"b"
        + struct.pack("<III", len(bool_values), 1, len(compressed))
        + compressed
    )
    compressed_array, _, _ = minimal_fbx(b"compressed-bools", compressed_property, 1)
    assert "1 top-level record(s)" in validate_bytes(compressed_array)

    assert_rejected(
        ordinary,
        VerificationLimits(maximum_input_bytes=len(ordinary) - 1),
        "input is",
    )
    assert_rejected(
        ordinary,
        VerificationLimits(maximum_nodes=0),
        "exceeds 0 nodes",
    )
    assert_rejected(
        bool_array,
        VerificationLimits(maximum_properties=0),
        "exceeds 0 properties",
    )
    assert_rejected(
        nested_fbx(),
        VerificationLimits(maximum_depth=1),
        "nesting exceeds 1",
    )
    assert_rejected(
        bool_array,
        VerificationLimits(maximum_array_elements=2),
        "array has 3 elements",
    )
    assert_rejected(
        bool_array,
        VerificationLimits(maximum_total_expanded_array_bytes=2),
        "expanded-data budget",
    )

    for label, count, payload, expected in [
        ("truncated", len(bool_values), compressed[:-1], "ends before"),
        ("trailing", len(bool_values), compressed + b"junk", "bytes after"),
        ("wrong-count", len(bool_values) + 1, compressed, "expands to"),
    ]:
        property_bytes = (
            b"b" + struct.pack("<III", count, 1, len(payload)) + payload
        )
        malformed, _, _ = minimal_fbx(label.encode(), property_bytes, 1)
        try:
            validate_bytes(malformed)
        except Invalid as error:
            assert expected in str(error), (label, error)
        else:
            raise AssertionError(f"{label} compressed array was accepted")

    corrupted = bytearray(boundary)
    corrupted[boundary_body + len(FOOTER_ID)] = 1
    try:
        validate_bytes(bytes(corrupted))
    except Invalid as error:
        assert "padding" in str(error)
    else:
        raise AssertionError("non-zero full-block padding was accepted")

    try:
        validate_bytes(boundary[:-1])
    except Invalid as error:
        assert "footer" in str(error)
    else:
        raise AssertionError("a truncated footer was accepted")

    print("binary FBX verifier regressions passed")


if __name__ == "__main__":
    main()
