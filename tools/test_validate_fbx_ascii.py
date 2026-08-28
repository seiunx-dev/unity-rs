#!/usr/bin/env python3
"""Regression tests for the independent ASCII FBX validator."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import validate_fbx_ascii


DOCUMENT = """\
FBXHeaderExtension: {
}
GlobalSettings: {
}
Definitions: {
ObjectType: "Model" { Count: 1 }
}
Objects: {
Model: 1, "Model::Root", "Null" {
Values: *2 {
    a: 1, 2
}
}
}
Connections: {
C: "OO", 1, 0
}
"""


class AsciiFbxValidatorTests(unittest.TestCase):
    def validate(self, document: str) -> list[str]:
        with tempfile.TemporaryDirectory(prefix="unity-rs-ascii-validator-") as directory:
            path = Path(directory) / "model.fbx"
            path.write_text(document, encoding="utf-8")
            return validate_fbx_ascii.validate(path)

    def test_valid_document_reports_resolved_objects(self) -> None:
        self.assertEqual(
            self.validate(DOCUMENT),
            ["1 object(s) across 1 type(s)", "1 connection(s), all resolving"],
        )

    def test_array_count_mismatch_is_rejected(self) -> None:
        altered = DOCUMENT.replace("    a: 1, 2", "    a: 1", 1)
        with self.assertRaisesRegex(
            validate_fbx_ascii.Invalid,
            r"Values declares \*2 but holds 1 values",
        ):
            self.validate(altered)

    def test_nonempty_zero_length_array_is_rejected(self) -> None:
        altered = DOCUMENT.replace("Values: *2", "Values: *0", 1)
        with self.assertRaisesRegex(
            validate_fbx_ascii.Invalid,
            r"Values declares \*0 but carries values",
        ):
            self.validate(altered)

    def test_missing_values_line_is_rejected(self) -> None:
        altered = DOCUMENT.replace("    a: 1, 2\n", "", 1)
        with self.assertRaisesRegex(
            validate_fbx_ascii.Invalid,
            r"Values declares \*2 but has no values line",
        ):
            self.validate(altered)


if __name__ == "__main__":
    unittest.main()
