#!/usr/bin/env python3
"""Regression tests for the strict Python API surface audit."""

from __future__ import annotations

import unittest

import check_python_api_surface


MINIMAL_STUB = """
class AssetStudio:
    @classmethod
    def open(cls, path: str) -> AssetStudio: ...

    @property
    def object_count(self) -> int: ...

    def read_text(self, index: int) -> bytes: ...
"""

COMPLETE_CONSUMER = """
def consume_public_api(studio: AssetStudio) -> None:
    alias: AssetStudio = studio
    AssetStudio.open("fixture.assets")
    studio.read_text(0)
    count = alias.object_count
"""


class PythonApiSurfaceAuditTests(unittest.TestCase):
    def test_current_stub_and_consumer_are_complete(self) -> None:
        methods, properties = check_python_api_surface.validate_surface(
            check_python_api_surface.STUB.read_text(encoding="utf-8"),
            check_python_api_surface.CONSUMER.read_text(encoding="utf-8"),
        )
        self.assertEqual((methods, properties), (66, 4))
        core_methods, rust_only = check_python_api_surface.validate_core_mapping(
            check_python_api_surface.CORE_STUDIO.read_text(encoding="utf-8"),
            check_python_api_surface.STUB.read_text(encoding="utf-8"),
        )
        self.assertEqual((core_methods, rust_only), (107, 4))
        check_python_api_surface.validate_texture_gil_boundary(
            check_python_api_surface.PYTHON_BINDING.read_text(encoding="utf-8")
        )

    def test_texture_row_conversion_must_stay_detached(self) -> None:
        binding = check_python_api_surface.PYTHON_BINDING.read_text(encoding="utf-8")
        altered = binding.replace(
            "            DisplayRowPyImage::from_decoded(image)\n        })?;",
            "            Ok(image)\n        })?;\n"
            "        let image = DisplayRowPyImage::from_decoded(image)?;",
            1,
        )
        self.assertNotEqual(altered, binding)
        with self.assertRaisesRegex(
            check_python_api_surface.AuditError,
            r"read_texture.*outside py\.detach",
        ):
            check_python_api_surface.validate_texture_gil_boundary(altered)

        altered = binding.replace(
            "            DisplayRowPyImages::from_decoded(images)\n        })?;",
            "            Ok(images)\n        })?;\n"
            "        let images = DisplayRowPyImages::from_decoded(images)?;",
            1,
        )
        self.assertNotEqual(altered, binding)
        with self.assertRaisesRegex(
            check_python_api_surface.AuditError,
            r"read_texture_array.*outside py\.detach",
        ):
            check_python_api_surface.validate_texture_gil_boundary(altered)

    def test_instance_method_omission_is_rejected(self) -> None:
        consumer = COMPLETE_CONSUMER.replace("    studio.read_text(0)\n", "")
        with self.assertRaisesRegex(
            check_python_api_surface.AuditError,
            r"methods: read_text",
        ):
            check_python_api_surface.validate_surface(MINIMAL_STUB, consumer)

    def test_property_omission_is_rejected(self) -> None:
        consumer = COMPLETE_CONSUMER.replace("    count = alias.object_count\n", "")
        with self.assertRaisesRegex(
            check_python_api_surface.AuditError,
            r"properties: object_count",
        ):
            check_python_api_surface.validate_surface(MINIMAL_STUB, consumer)

    def test_missing_named_classes_are_diagnostic(self) -> None:
        with self.assertRaisesRegex(
            check_python_api_surface.AuditError,
            r"stub does not define AssetStudio",
        ):
            check_python_api_surface.validate_surface("class Other: pass", COMPLETE_CONSUMER)
        with self.assertRaisesRegex(
            check_python_api_surface.AuditError,
            r"strict consumer does not define consume_public_api",
        ):
            check_python_api_surface.validate_surface(MINIMAL_STUB, "def other(): pass")

    def test_new_core_method_must_be_classified(self) -> None:
        core = check_python_api_surface.CORE_STUDIO.read_text(encoding="utf-8")
        altered = core.replace(
            "impl Studio {",
            "impl Studio {\n    pub fn newly_public(&self) {}",
            1,
        )
        with self.assertRaisesRegex(
            check_python_api_surface.AuditError,
            r"unclassified Core methods: Studio.newly_public",
        ):
            check_python_api_surface.validate_core_mapping(
                altered,
                check_python_api_surface.STUB.read_text(encoding="utf-8"),
            )

    def test_missing_python_mapping_target_is_rejected(self) -> None:
        stub = check_python_api_surface.STUB.read_text(encoding="utf-8")
        altered = stub.replace("    def read_shader(\n", "    def removed_shader(\n", 1)
        self.assertNotEqual(altered, stub)
        with self.assertRaisesRegex(
            check_python_api_surface.AuditError,
            r"StudioObject.read_shader_text -> AssetStudio.read_shader",
        ):
            check_python_api_surface.validate_core_mapping(
                check_python_api_surface.CORE_STUDIO.read_text(encoding="utf-8"),
                altered,
            )


if __name__ == "__main__":
    unittest.main()
