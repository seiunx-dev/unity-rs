#!/usr/bin/env python3
"""Tests for the astcenc trust boundary and the HDR reference conversion."""

from __future__ import annotations

import os
import struct
import tempfile
import unittest
from pathlib import Path

import decode_astc_references


class TrustedAstcencTests(unittest.TestCase):
    def test_accepts_an_executable_with_the_official_name(self) -> None:
        with tempfile.TemporaryDirectory(prefix="unity-rs-astcenc-") as directory:
            executable = Path(directory) / "astcenc-avx2"
            executable.write_bytes(b"test executable")
            executable.chmod(executable.stat().st_mode | 0o100)
            self.assertEqual(
                decode_astc_references.trusted_astcenc(str(executable)),
                executable.resolve(),
            )

    def test_rejects_an_unexpected_executable_name(self) -> None:
        with tempfile.TemporaryDirectory(prefix="unity-rs-astcenc-") as directory:
            executable = Path(directory) / "other-decoder"
            executable.write_bytes(b"test executable")
            executable.chmod(executable.stat().st_mode | 0o100)
            with self.assertRaisesRegex(ValueError, "unexpected name"):
                decode_astc_references.trusted_astcenc(str(executable))

    @unittest.skipIf(os.name == "nt", "Windows executable permission is not a mode bit")
    def test_rejects_a_non_executable_file(self) -> None:
        with tempfile.TemporaryDirectory(prefix="unity-rs-astcenc-") as directory:
            executable = Path(directory) / "astcenc"
            executable.write_bytes(b"not executable")
            executable.chmod(0o600)
            with self.assertRaisesRegex(ValueError, "not executable"):
                decode_astc_references.trusted_astcenc(str(executable))


def half_float_dds(
    width: int, height: int, values: list[float], *, fourcc: bytes = b"DX10", dxgi: int = 10
) -> bytes:
    """A minimal RGBA half-float DDS, laid out the way astcenc writes one."""
    header = bytearray(124)
    struct.pack_into("<I", header, 0, 124)
    struct.pack_into("<II", header, 8, height, width)
    header[80:84] = fourcc
    extension = struct.pack("<5I", dxgi, 3, 0, 1, 0) if fourcc == b"DX10" else b""
    return b"DDS " + bytes(header) + extension + struct.pack(f"<{len(values)}e", *values)


class HdrReferenceConversionTests(unittest.TestCase):
    # Half floats whose 255ths have a fraction above one half: truncating
    # instead of rounding would put each of them one step low.
    ROUNDING = [0.30098, 0.70098, 0.10098, 0.50098]
    ROUNDED = bytes((77, 179, 26, 128))
    # Out-of-range values clamp; one and zero are exact.
    CLAMPED = [2.5, -0.25, 1.0, 0.0]

    def test_rounds_each_channel_of_a_dx10_dds(self) -> None:
        data = half_float_dds(2, 1, self.ROUNDING + self.CLAMPED)
        self.assertEqual(
            decode_astc_references.read_dds(data, 2, 1),
            self.ROUNDED + bytes((255, 0, 255, 0)),
        )

    def test_reads_the_legacy_half_float_code(self) -> None:
        data = half_float_dds(1, 1, self.ROUNDING, fourcc=struct.pack("<I", 113))
        self.assertEqual(decode_astc_references.read_dds(data, 1, 1), self.ROUNDED)

    def test_rejects_another_pixel_format(self) -> None:
        data = half_float_dds(1, 1, self.ROUNDING, dxgi=2)
        with self.assertRaisesRegex(ValueError, "half-float"):
            decode_astc_references.read_dds(data, 1, 1)
        data = half_float_dds(1, 1, self.ROUNDING, fourcc=b"DXT5")
        with self.assertRaisesRegex(ValueError, "four-character code"):
            decode_astc_references.read_dds(data, 1, 1)

    def test_rejects_mismatched_or_truncated_images(self) -> None:
        data = half_float_dds(1, 1, self.ROUNDING)
        with self.assertRaisesRegex(ValueError, "dimensions"):
            decode_astc_references.read_dds(data, 2, 1)
        with self.assertRaisesRegex(ValueError, "truncated"):
            decode_astc_references.read_dds(data[:-2], 1, 1)
        with self.assertRaisesRegex(ValueError, "not a DDS"):
            decode_astc_references.read_dds(b"XXXX" + data[4:], 1, 1)

    def test_rejects_the_nan_of_an_error_block(self) -> None:
        data = half_float_dds(1, 1, [float("nan")] * 4)
        with self.assertRaisesRegex(ValueError, "error block"):
            decode_astc_references.read_dds(data, 1, 1)


if __name__ == "__main__":
    unittest.main()
