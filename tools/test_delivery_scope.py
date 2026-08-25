#!/usr/bin/env python3
"""Reverse tests for the headless delivery-scope audit."""

from __future__ import annotations

import shutil
import tempfile
import unittest
from pathlib import Path

import check_delivery_scope


class DeliveryScopeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        for relative in check_delivery_scope.PUBLIC_API_FILES:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("owned Studio API\n", encoding="utf-8")
        for relative in check_delivery_scope.DELIVERY_CONFIGURATION_FILES:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("clean delivery configuration\n", encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_clean_owned_apis_pass(self) -> None:
        check_delivery_scope.check_retired_surfaces(self.root)

    def test_retired_ffi_sources_fail(self) -> None:
        for relative in check_delivery_scope.FORBIDDEN_SOURCE_FILES:
            with self.subTest(relative=relative):
                path = self.root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("[package]\nname = 'unity-rs-ffi'\n", encoding="utf-8")
                with self.assertRaises(AssertionError):
                    check_delivery_scope.check_retired_surfaces(self.root)
                shutil.rmtree(self.root / "crates/unity-rs-ffi")

    def test_retired_ffi_directory_fails_without_source_files(self) -> None:
        retired = self.root / check_delivery_scope.FORBIDDEN_REPOSITORY_PATHS[0]
        cache = retired / "target/cache"
        cache.mkdir(parents=True)
        (cache / "marker").write_text(
            "ignored historical build output\n",
            encoding="utf-8",
        )
        with self.assertRaises(AssertionError):
            check_delivery_scope.check_retired_surfaces(self.root)

    def test_context_handles_fail_each_public_surface(self) -> None:
        markers = (
            "for_each_os_str_char_lossy",
            "StudioContext",
            "context_id",
            "contextId",
        )
        self.assertEqual(len(check_delivery_scope.PUBLIC_API_FILES), len(markers))
        for relative, marker in zip(check_delivery_scope.PUBLIC_API_FILES, markers):
            with self.subTest(relative=relative, marker=marker):
                path = self.root / relative
                original = path.read_text(encoding="utf-8")
                path.write_text(f"{original}{marker}\n", encoding="utf-8")
                with self.assertRaises(AssertionError):
                    check_delivery_scope.check_retired_surfaces(self.root)
                path.write_text(original, encoding="utf-8")

    def test_retired_ffi_configuration_references_fail(self) -> None:
        for relative in check_delivery_scope.DELIVERY_CONFIGURATION_FILES:
            with self.subTest(relative=relative):
                path = self.root / relative
                original = path.read_text(encoding="utf-8")
                path.write_text(f"{original}unity-rs-ffi\n", encoding="utf-8")
                with self.assertRaises(AssertionError):
                    check_delivery_scope.check_retired_surfaces(self.root)
                path.write_text(original, encoding="utf-8")

    def test_public_context_in_non_root_rust_module_fails(self) -> None:
        source = self.root / "crates/unity-rs-core/src/parser_state.rs"
        source.write_text("pub struct ParserContext;\n", encoding="utf-8")
        with self.assertRaises(AssertionError):
            check_delivery_scope.check_retired_surfaces(self.root)

    def test_custom_exported_c_abi_fails(self) -> None:
        source = self.root / "crates/unity-rs-core/src/legacy_abi.rs"
        for declaration in (
            '#[unsafe(no_mangle)]\npub extern "C" fn context_open() {}\n',
            'pub unsafe extern "C" fn context_close() {}\n',
            '#[unsafe(export_name = "context_list")]\nfn exported_context_list() {}\n',
        ):
            with self.subTest(declaration=declaration):
                source.write_text(declaration, encoding="utf-8")
                with self.assertRaises(AssertionError):
                    check_delivery_scope.check_retired_surfaces(self.root)
        source.unlink()

    def test_extra_production_target_fails(self) -> None:
        expected = check_delivery_scope.EXPECTED_PRIMARY_TARGET["unity-rs-core"]
        package = {
            "targets": [
                {
                    "name": expected[0],
                    "kind": list(expected[1]),
                    "crate_types": list(expected[2]),
                }
            ]
        }
        check_delivery_scope.check_package_targets("unity-rs-core", package)
        package["targets"].append(
            {
                "name": "unity-rs-gui",
                "kind": ["bin"],
                "crate_types": ["bin"],
            }
        )
        with self.assertRaises(AssertionError):
            check_delivery_scope.check_package_targets("unity-rs-core", package)


if __name__ == "__main__":
    unittest.main()
