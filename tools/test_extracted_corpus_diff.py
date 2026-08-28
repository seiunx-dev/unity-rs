#!/usr/bin/env python3
"""Tests for corpus staging and native CLI argument boundaries."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import extracted_corpus_diff


class ExtractedCorpusBoundaryTests(unittest.TestCase):
    def test_stage_copies_without_aliasing_the_source_inode(self) -> None:
        with tempfile.TemporaryDirectory(prefix="unity-rs-corpus-stage-") as directory:
            root = Path(directory)
            source = root / "source.bundle"
            target = root / "target.bundle"
            source.write_bytes(b"bundle")
            extracted_corpus_diff.stage(source, target)
            self.assertEqual(target.read_bytes(), b"bundle")
            self.assertNotEqual(source.stat().st_ino, target.stat().st_ino)

    def test_accepts_bounded_ascii_unity_versions(self) -> None:
        self.assertEqual(
            extracted_corpus_diff.validated_unity_version("2022.3.62f2"),
            "2022.3.62f2",
        )

    def test_rejects_command_shaped_unity_versions(self) -> None:
        for value in ("", "2022.3;touch", "2022.3\nnext", "x" * 65):
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    extracted_corpus_diff.validated_unity_version(value)


if __name__ == "__main__":
    unittest.main()
