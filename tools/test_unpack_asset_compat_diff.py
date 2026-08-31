#!/usr/bin/env python3
"""Unit tests for the old unpack-asset differential helper."""

from __future__ import annotations

import unittest

import unpack_asset_compat_diff as diff


class FakePointer:
    m_FileID = 2
    m_PathID = -7


class FakeTextAsset:
    m_Script = "A\udcffB"


class FakeType:
    name = "Mesh"


class FakeAssetsFile:
    name = "CAB-Test"


class FakeReader:
    assets_file = FakeAssetsFile()
    path_id = -9
    type = FakeType()


class FakeImage:
    def __init__(self, size: tuple[int, int], rgba: bytes) -> None:
        self.size = size
        self._rgba = rgba

    def convert(self, mode: str) -> FakeImage:
        self.assert_mode(mode)
        return self

    @staticmethod
    def assert_mode(mode: str) -> None:
        if mode != "RGBA":
            raise AssertionError(mode)

    def tobytes(self) -> bytes:
        return self._rgba


class UnpackAssetCompatDiffTests(unittest.TestCase):
    def test_restores_surrogate_escaped_text_asset_bytes(self) -> None:
        self.assertEqual(diff.text_asset_bytes(FakeTextAsset()), b"A\xffB")

    def test_normalizes_pointer_and_binary_values_with_a_budget(self) -> None:
        value = {"pointer": FakePointer(), "payload": b"data"}
        normalized = diff.normalize_type_tree(value, diff.ValueBudget(8, 4))
        self.assertEqual(normalized["pointer"], ("PPtr", 2, -7))
        self.assertEqual(normalized["payload"][:2], ("bytes", 4))

    def test_reader_identity_does_not_evaluate_a_missing_path_fallback(self) -> None:
        self.assertEqual(diff.reader_identity(FakeReader()), ("cab-test", -9, "Mesh"))

    def test_obj_comparison_ignores_line_endings_and_float_spelling(self) -> None:
        left = "v 1.000000000 0 -0\nf 1/1/1 2/2/2 3/3/3\n"
        right = "v 1 0 -0\r\nf 1/1/1 2/2/2 3/3/3\r\n"
        self.assertEqual(diff.obj_values(left), diff.obj_values(right))

    def test_rejects_type_tree_values_beyond_the_budget(self) -> None:
        with self.assertRaisesRegex(ValueError, "maximum_tree_values"):
            diff.normalize_type_tree([1, 2], diff.ValueBudget(2, 4))

    def test_reports_exact_and_visible_image_differences(self) -> None:
        left = FakeImage((1, 1), bytes((100, 50, 25, 255)))
        exact = diff.image_difference(left, left)
        self.assertEqual(exact.differing_pixels, 0)

        right = FakeImage((1, 1), bytes((90, 50, 25, 250)))
        changed = diff.image_difference(left, right)
        self.assertEqual(changed.differing_pixels, 1)
        self.assertEqual(changed.alpha_differences, 1)
        self.assertEqual(changed.worst_alpha, 5)
        self.assertGreater(changed.worst_composited, 0)

    def test_rejects_different_image_dimensions(self) -> None:
        with self.assertRaisesRegex(ValueError, "dimensions differ"):
            diff.image_difference(
                FakeImage((1, 1), bytes(4)), FakeImage((2, 1), bytes(8))
            )


if __name__ == "__main__":
    unittest.main()
