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

    def test_linux_wheels_must_keep_the_manylinux_posture(self) -> None:
        altered = self.workflow.replace(
            "            wheel_compatibility: manylinux_2_28\n"
            "            wheel_flags: --zig\n",
            "            wheel_compatibility: pypi\n"
            "            wheel_flags: \"\"\n",
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
            "            smoke: ./target/release/artifact/unity-rs --help\n",
            "            smoke: ./target/release/unity-rs --help\n",
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

    def test_commented_stage_cannot_hide_wrong_active_order(self) -> None:
        stage = (
            "      - name: Stage binary with license and notices\n"
            "        run: python tools/stage_cli_artifact.py \"${{ matrix.binary }}\" target/release/artifact\n"
        )
        smoke = (
            "      - name: Smoke-test the staged CLI artifact\n"
            "        run: ${{ matrix.smoke }}\n"
        )
        commented_stage = (
            "      # - name: Disabled staging decoy\n"
            "      #   run: python tools/stage_cli_artifact.py decoy artifact\n"
        )
        self.assertIn(stage + smoke, self.workflow)
        altered = self.workflow.replace(
            stage + smoke, commented_stage + smoke + stage, 1
        )
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_merged_run_block_preserves_command_order(self) -> None:
        stage = (
            "      - name: Stage binary with license and notices\n"
            "        run: python tools/stage_cli_artifact.py \"${{ matrix.binary }}\" target/release/artifact\n"
        )
        smoke = (
            "      - name: Smoke-test the staged CLI artifact\n"
            "        run: ${{ matrix.smoke }}\n"
        )
        reversed_block = (
            "      - name: Reversed combined publication check\n"
            "        run: |\n"
            "          ${{ matrix.smoke }}\n"
            "          python tools/stage_cli_artifact.py \"${{ matrix.binary }}\" target/release/artifact\n"
        )
        self.assertIn(stage + smoke, self.workflow)
        altered = self.workflow.replace(stage + smoke, reversed_block, 1)
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

    def test_echoed_rustsec_audit_is_rejected(self) -> None:
        command = "cargo audit --file Cargo.lock --deny unsound --deny yanked"
        altered = self.workflow.replace(
            f"        run: {command}\n",
            f"        run: echo '{command}'\n",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_environment_value_cannot_impersonate_rustsec_audit(self) -> None:
        command = "cargo audit --file Cargo.lock --deny unsound --deny yanked"
        altered = self.workflow.replace(
            f"        run: {command}\n",
            f"        env:\n          AUDIT_COMMAND: {command}\n"
            '        run: echo "$AUDIT_COMMAND"\n',
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_echoed_multiline_python_test_is_rejected(self) -> None:
        altered = self.workflow.replace(
            "          python -I tests/python_api.py\n",
            "          echo python -I tests/python_api.py\n",
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_vgmstream_install_creates_destination_directory(self) -> None:
        altered = self.workflow.replace(
            '          mkdir -p "$HOME/.local/bin"\n',
            "",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_vgmstream_smoke_checks_its_documented_info_status(self) -> None:
        altered = self.workflow.replace(
            "assert result.returncode == 1, result; ",
            "",
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

    def test_missing_pypi_trusted_publisher_is_rejected(self) -> None:
        altered = self.workflow.replace(
            "        uses: pypa/gh-action-pypi-publish@dc37677b2e1c63e2034f94d8a5b11f265b73ba33 # release/v1\n",
            "",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_node_installs_must_disable_lifecycle_scripts(self) -> None:
        altered = self.workflow.replace(
            "        run: npm ci --ignore-scripts\n",
            "        run: npm ci\n",
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_missing_pypi_oidc_permission_is_rejected(self) -> None:
        altered = self.workflow.replace("      id-token: write\n", "", 1)
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_python_publish_must_validate_the_tag_version(self) -> None:
        altered = self.workflow.replace(
            '          python3 - "${GITHUB_REF_NAME#v}" <<\'PY\'\n',
            "",
            1,
        )
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)


if __name__ == "__main__":
    unittest.main()
