#!/usr/bin/env python3
"""Regression tests for the strict Node API surface audit."""

from __future__ import annotations

import unittest

import check_node_api_surface


class NodeApiSurfaceAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.core = check_node_api_surface.CORE_STUDIO.read_text(encoding="utf-8")
        cls.rust = check_node_api_surface.NODE_RUST.read_text(encoding="utf-8")
        cls.declarations = check_node_api_surface.DECLARATIONS.read_text(encoding="utf-8")
        cls.consumer = check_node_api_surface.CONSUMER.read_text(encoding="utf-8")

    def test_current_core_rust_declarations_and_consumer_are_complete(self) -> None:
        check_node_api_surface.validate_live2d_worker_projection(self.rust)
        check_node_api_surface.validate_texture_array_worker_projection(self.rust)
        self.assertEqual(
            check_node_api_surface.validate_node_declarations(
                self.rust,
                self.declarations,
            ),
            (85, 4),
        )
        self.assertEqual(
            check_node_api_surface.validate_core_mapping(
                self.core,
                self.rust,
                self.declarations,
            ),
            (107, 4),
        )
        self.assertEqual(
            check_node_api_surface.validate_surface(
                self.declarations,
                self.consumer,
            ),
            (85, 4),
        )

    def test_live2d_worker_must_return_the_projected_table(self) -> None:
        altered = self.rust.replace(
            "    type Output = Live2dPackageSet;\n",
            "    type Output = CoreLive2dPackageBytesSet;\n",
            1,
        )
        self.assertNotEqual(altered, self.rust)
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"must return the projected Live2dPackageSet",
        ):
            check_node_api_surface.validate_live2d_worker_projection(altered)

    def test_live2d_projection_cannot_move_back_to_resolve(self) -> None:
        altered = self.rust.replace(
            "        convert_live2d_package_set(set)\n"
            "    }\n\n"
            "    fn resolve(&mut self, _env: Env, set: Self::Output) -> Result<Self::JsValue> {\n"
            "        Ok(set)\n",
            "        Ok(set)\n"
            "    }\n\n"
            "    fn resolve(&mut self, _env: Env, set: Self::Output) -> Result<Self::JsValue> {\n"
            "        convert_live2d_package_set(set)\n",
            1,
        )
        self.assertNotEqual(altered, self.rust)
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"inside compute",
        ):
            check_node_api_surface.validate_live2d_worker_projection(altered)

    def test_texture_array_task_must_return_the_worker_projection(self) -> None:
        altered = self.rust.replace(
            "    type Output = DisplayRowImages;\n",
            "    type Output = Vec<RgbaImage>;\n",
            1,
        )
        self.assertNotEqual(altered, self.rust)
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"must return the worker-projected DisplayRowImages",
        ):
            check_node_api_surface.validate_texture_array_worker_projection(altered)

    def test_texture_array_projection_cannot_move_back_to_resolve(self) -> None:
        altered = self.rust.replace(
            "    fn into_nodes(self) -> Vec<RgbaImage> {\n"
            "        self.0\n",
            "    fn into_nodes(self) -> Vec<RgbaImage> {\n"
            "        let _event_loop_regression = convert_image;\n"
            "        self.0\n",
            1,
        )
        self.assertNotEqual(altered, self.rust)
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"must not allocate or project layers on the event loop",
        ):
            check_node_api_surface.validate_texture_array_worker_projection(altered)

    def test_new_core_method_must_be_classified(self) -> None:
        altered = self.core.replace(
            "impl Studio {",
            "impl Studio {\n    pub fn newly_public(&self) {}",
            1,
        )
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"unclassified Core methods: Studio.newly_public",
        ):
            check_node_api_surface.validate_core_mapping(
                altered,
                self.rust,
                self.declarations,
            )

    def test_missing_rust_mapping_target_is_rejected(self) -> None:
        altered = self.rust.replace("    pub fn read_shader(\n", "    fn read_shader(\n", 1)
        self.assertNotEqual(altered, self.rust)
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"StudioObject.read_shader_text -> UnityRs.readShader",
        ):
            check_node_api_surface.validate_core_mapping(
                self.core,
                altered,
                self.declarations,
            )

    def test_missing_typescript_mapping_target_is_rejected(self) -> None:
        altered = self.declarations.replace(
            "  readShader(fileIndex:",
            "  removedShader(fileIndex:",
            1,
        )
        self.assertNotEqual(altered, self.declarations)
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"StudioObject.read_shader_text -> UnityRs.readShader",
        ):
            check_node_api_surface.validate_core_mapping(
                self.core,
                self.rust,
                altered,
            )

    def test_rust_and_generated_class_declarations_must_agree(self) -> None:
        altered = self.rust.replace(
            "#[napi]\nimpl UnityRs {",
            "#[napi]\nimpl UnityRs {\n    #[napi]\n    pub fn extra_export(&self) {}",
            1,
        )
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"missing declarations: UnityRs.extraExport",
        ):
            check_node_api_surface.validate_node_declarations(
                altered,
                self.declarations,
            )

    def test_unconsumed_public_method_is_rejected(self) -> None:
        call = "  void studio.readMaterial(0, 1n, 1024);\n"
        altered = self.consumer.replace(call, "", 1)
        self.assertNotEqual(altered, self.consumer)
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"methods: readMaterial",
        ):
            check_node_api_surface.validate_surface(self.declarations, altered)

    def test_comments_cannot_impersonate_a_typescript_consumer(self) -> None:
        call = "  void studio.readMaterial(0, 1n, 1024);\n"
        altered = self.consumer.replace(
            call,
            "  // void studio.readMaterial(0, 1n, 1024);\n",
            1,
        )
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"methods: readMaterial",
        ):
            check_node_api_surface.validate_surface(self.declarations, altered)

    def test_mapped_object_fields_must_exist_in_both_surfaces(self) -> None:
        altered = self.declarations.replace("  sourcePath: string\n", "", 1)
        self.assertNotEqual(altered, self.declarations)
        with self.assertRaisesRegex(
            check_node_api_surface.AuditError,
            r"StudioObject.source_path -> ObjectInfo.sourcePath",
        ):
            check_node_api_surface.validate_core_mapping(
                self.core,
                self.rust,
                altered,
            )


if __name__ == "__main__":
    unittest.main()
