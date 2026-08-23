#!/usr/bin/env python3
"""Regression tests for the specialized CI release-matrix audit."""

from __future__ import annotations

import unittest

import check_ci_matrix


class CiMatrixAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workflow = check_ci_matrix.WORKFLOW.read_text(encoding="utf-8")

    def test_current_workflow_is_complete(self) -> None:
        check_ci_matrix.validate_workflow(self.workflow)
        check_ci_matrix.validate_node_package(
            check_ci_matrix.NODE_PACKAGE.read_text(encoding="utf-8")
        )

    def test_missing_platform_is_rejected(self) -> None:
        altered = self.workflow.replace(
            "          - os: windows-11-arm\n            artifact: windows-arm64\n"
            "            build_python: \"3.14\"\n",
            "",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_duplicate_matrix_key_is_rejected(self) -> None:
        altered = self.workflow.replace(
            "            artifact: windows-arm64\n            binary:",
            "            artifact: windows-arm64\n"
            "            artifact: duplicate\n            binary:",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_missing_artifact_smoke_is_rejected(self) -> None:
        altered = self.workflow.replace("      - name: Smoke-test the staged CLI artifact", "")
        altered = altered.replace("        run: ${{ matrix.smoke }}", "", 1)
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_cli_smoke_must_name_the_staged_binary(self) -> None:
        altered = self.workflow.replace(
            "            smoke: ./target/release/artifact/assetstudio --help\n",
            "            smoke: ./target/release/assetstudio --help\n",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_cli_smoke_must_run_after_staging(self) -> None:
        stage = (
            "      - name: Stage binary with license and notices\n"
            "        run: python tools/stage_cli_artifact.py \"${{ matrix.binary }}\" target/release/artifact\n"
        )
        smoke = (
            "      - name: Smoke-test the staged CLI artifact\n"
            "        run: ${{ matrix.smoke }}\n"
        )
        self.assertIn(stage + smoke, self.workflow)
        altered = self.workflow.replace(stage + smoke, smoke + stage, 1)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_missing_python_surface_audit_tests_are_rejected(self) -> None:
        altered = self.workflow.replace(
            "        run: python3 tools/test_python_api_surface.py\n",
            "",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_missing_node_surface_audit_tests_are_rejected(self) -> None:
        altered = self.workflow.replace(
            "        run: python3 tools/test_node_api_surface.py\n",
            "",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_missing_local_ci_policy_tests_are_rejected(self) -> None:
        altered = self.workflow.replace(
            "        run: python3 tools/test_local_ci.py\n",
            "",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_missing_delivery_scope_tests_are_rejected(self) -> None:
        altered = self.workflow.replace(
            "        run: python3 tools/test_delivery_scope.py\n",
            "",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_missing_rustsec_audit_is_rejected(self) -> None:
        altered = self.workflow.replace(
            "        run: cargo audit --file Cargo.lock --deny unsound --deny yanked\n",
            "",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_commented_rustsec_audit_is_rejected(self) -> None:
        altered = self.workflow.replace(
            "        run: cargo audit --file Cargo.lock --deny unsound --deny yanked\n",
            "        # run: cargo audit --file Cargo.lock --deny unsound --deny yanked\n",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_missing_installed_node_tarball_test_is_rejected(self) -> None:
        package_json = check_ci_matrix.NODE_PACKAGE.read_text(encoding="utf-8")
        altered = package_json.replace(
            " && node tests/installed_package.cjs",
            "",
            1,
        )
        self.assertNotEqual(altered, package_json)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_node_package(altered)


if __name__ == "__main__":
    unittest.main()
