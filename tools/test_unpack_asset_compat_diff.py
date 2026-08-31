#!/usr/bin/env python3
"""Unit tests for the old unpack-asset differential helper."""

from __future__ import annotations

import io
import struct
import unittest
import wave
from pathlib import Path
from types import SimpleNamespace

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

    def test_pcm16_comparison_reports_one_unit_rounding(self) -> None:
        def make_wav(samples: tuple[int, ...]) -> bytes:
            output = io.BytesIO()
            with wave.open(output, "wb") as writer:
                writer.setnchannels(1)
                writer.setsampwidth(2)
                writer.setframerate(44_100)
                writer.writeframes(struct.pack("<{}h".format(len(samples)), *samples))
            return output.getvalue()

        difference = diff.pcm16_difference(
            make_wav((0, 1, -1, 32_767)),
            make_wav((0, 2, -2, 32_767)),
        )
        self.assertIsNotNone(difference)
        assert difference is not None
        self.assertEqual(difference.samples, 4)
        self.assertEqual(difference.differing_samples, 2)
        self.assertEqual(difference.worst_sample, 1)

        larger = diff.pcm16_difference(make_wav((0,)), make_wav((2,)))
        self.assertIsNotNone(larger)
        assert larger is not None
        self.assertEqual(larger.worst_sample, 2)

        changed_header = bytearray(make_wav((0,)))
        changed_header[4] ^= 1
        self.assertIsNone(
            diff.pcm16_difference(bytes(changed_header), make_wav((0,)))
        )

    def test_extracts_shader_export_manifest(self) -> None:
        shader = '''Shader "Example/Shader" {
Properties {
[Toggle(_FLAG)] _Enabled ("Enabled", Float) = 1.0
_MainTex ("Texture", 2D) = "white" { }
}
SubShader {
 Pass { }
 UsePass "Example/Other/PASS"
 GrabPass { }
}
SubShader {
}
}'''
        self.assertEqual(
            diff.exported_shader_manifest(shader),
            diff.ShaderManifest(
                name="Example/Shader",
                properties=("_Enabled", "_MainTex"),
                subshader_pass_counts=(3, 0),
            ),
        )

    def test_arguments_accept_directories_and_repeated_type_filters(self) -> None:
        arguments = diff.parse_arguments(
            ["--type", "Mesh", "--type", "Shader", str(Path.cwd())]
        )
        self.assertEqual(arguments.types, frozenset({"Mesh", "Shader"}))

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
        self.assertEqual(changed.worst_channel, 10)
        self.assertEqual(changed.worst_alpha, 5)
        self.assertGreater(changed.worst_composited, 0)

    def test_classifies_only_bounded_texture_conversion_differences(self) -> None:
        alpha8 = diff.image_difference(
            FakeImage((1, 1), bytes((0, 0, 0, 127))),
            FakeImage((1, 1), bytes((255, 255, 255, 127))),
        )
        self.assertEqual(
            diff.known_texture_difference_kind(1, alpha8),
            "Alpha8 unstored RGB fill",
        )

        rgb565 = diff.image_difference(
            FakeImage((1, 1), bytes((100, 100, 100, 255))),
            FakeImage((1, 1), bytes((101, 99, 100, 255))),
        )
        self.assertEqual(
            diff.known_texture_difference_kind(7, rgb565),
            "RGB565 one-level conversion rounding",
        )
        larger = diff.image_difference(
            FakeImage((1, 1), bytes((100, 100, 100, 255))),
            FakeImage((1, 1), bytes((102, 100, 100, 255))),
        )
        self.assertIsNone(diff.known_texture_difference_kind(7, larger))

    def test_tight_sprite_classification_uses_the_packing_mode_bit(self) -> None:
        self.assertTrue(
            diff.sprite_uses_tight_mask(
                SimpleNamespace(m_RD=SimpleNamespace(settingsRaw=64))
            )
        )
        self.assertFalse(
            diff.sprite_uses_tight_mask(
                SimpleNamespace(m_RD=SimpleNamespace(settingsRaw=66))
            )
        )

    def test_sprite_classification_uses_atlas_render_data(self) -> None:
        render_data = SimpleNamespace(settingsRaw=66)
        key = object()
        atlas = SimpleNamespace(m_RenderDataMap=[(key, render_data)])
        atlas_reader = SimpleNamespace(read=lambda: atlas)
        sprite = SimpleNamespace(
            m_RD=SimpleNamespace(settingsRaw=64),
            m_SpriteAtlas=SimpleNamespace(
                path_id=7, deref=lambda: atlas_reader
            ),
            m_AtlasTags=[],
            m_RenderDataKey=key,
        )

        self.assertIs(diff.effective_sprite_render_data(sprite), render_data)
        self.assertFalse(diff.sprite_uses_tight_mask(sprite))

    def test_rejects_different_image_dimensions(self) -> None:
        with self.assertRaisesRegex(ValueError, "dimensions differ"):
            diff.image_difference(
                FakeImage((1, 1), bytes(4)), FakeImage((2, 1), bytes(8))
            )


if __name__ == "__main__":
    unittest.main()
