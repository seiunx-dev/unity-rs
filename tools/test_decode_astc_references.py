#!/usr/bin/env python3
"""Tests for the explicit astcenc executable trust boundary."""

from __future__ import annotations

import os
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


if __name__ == "__main__":
    unittest.main()
