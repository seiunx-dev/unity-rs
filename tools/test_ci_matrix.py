#!/usr/bin/env python3
"""Regression tests for the specialized CI release-matrix audit."""

from __future__ import annotations

import unittest

import check_ci_matrix


class CiMatrixAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workflow = check_ci_matrix.WORKFLOW.read_text(encoding="utf-8")
        cls.release = check_ci_matrix.RELEASE_WORKFLOW.read_text(encoding="utf-8")
        cls.oracles = check_ci_matrix.ORACLE_SCRIPT.read_text(encoding="utf-8")

    def assert_ci_rejected(self, old: str, new: str = "") -> None:
        altered = self.workflow.replace(old, new, 1)
        self.assertNotEqual(altered, self.workflow, old)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def assert_release_rejected(self, old: str, new: str = "") -> None:
        altered = self.release.replace(old, new, 1)
        self.assertNotEqual(altered, self.release, old)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_release(altered)

    def test_current_workflows_are_complete(self) -> None:
        check_ci_matrix.validate_workflow(self.workflow)
        check_ci_matrix.validate_release(self.release)
        check_ci_matrix.validate_oracle_script(self.oracles)
        check_ci_matrix.validate_node_package(
            check_ci_matrix.NODE_PACKAGE.read_text(encoding="utf-8")
        )

    # --- release.yml: six platforms per artifact family ---------------------------------

    def test_missing_cli_platform_is_rejected(self) -> None:
        self.assert_release_rejected(
            '         {"label":"windows-arm64","os":"windows-11-arm","target":"aarch64-pc-windows-msvc"},\n'
            '         {"label":"macos-x64","os":"macos-15-intel","target":"x86_64-apple-darwin"},\n'
            '         {"label":"macos-arm64","os":"macos-latest","target":"aarch64-apple-darwin"}]\n\n'
            "  wheels:",
            '         {"label":"macos-x64","os":"macos-15-intel","target":"x86_64-apple-darwin"},\n'
            '         {"label":"macos-arm64","os":"macos-latest","target":"aarch64-apple-darwin"}]\n\n'
            "  wheels:",
        )

    def test_missing_release_wheel_platform_is_rejected(self) -> None:
        self.assert_release_rejected(
            '         {"label":"windows-arm64","os":"windows-11-arm","target":"aarch64-pc-windows-msvc"},\n'
            '         {"label":"macos-x64","os":"macos-15-intel","target":"x86_64-apple-darwin"},\n'
            '         {"label":"macos-arm64","os":"macos-latest","target":"aarch64-apple-darwin"}]\n'
            '      python-version: "3.14"',
            '         {"label":"macos-x64","os":"macos-15-intel","target":"x86_64-apple-darwin"},\n'
            '         {"label":"macos-arm64","os":"macos-latest","target":"aarch64-apple-darwin"}]\n'
            '      python-version: "3.14"',
        )

    def test_untested_wheel_target_is_rejected(self) -> None:
        self.assert_release_rejected(
            '{"label":"macos-x64","os":"macos-15-intel","target":"x86_64-apple-darwin"},\n'
            '         {"label":"macos-arm64","os":"macos-latest","target":"aarch64-apple-darwin"}]\n'
            '      python-version',
            '{"label":"macos-x64","os":"macos-15-intel","target":"x86_64-apple-darwin","test":false},\n'
            '         {"label":"macos-arm64","os":"macos-latest","target":"aarch64-apple-darwin"}]\n'
            '      python-version',
        )

    def test_linux_wheels_must_keep_the_manylinux_floor(self) -> None:
        self.assert_release_rejected(
            '{"label":"linux-arm64","os":"ubuntu-24.04-arm","target":"aarch64","manylinux":"2_28"}',
            '{"label":"linux-arm64","os":"ubuntu-24.04-arm","target":"aarch64"}',
        )

    def test_missing_node_platform_is_rejected(self) -> None:
        self.assert_release_rejected("          - os: windows-11-arm\n            label: windows-arm64\n")

    def test_duplicate_node_matrix_key_is_rejected(self) -> None:
        self.assert_release_rejected(
            "            label: windows-arm64\n",
            "            label: windows-arm64\n            label: duplicate\n",
        )

    def test_missing_cli_smoke_is_rejected(self) -> None:
        self.assert_release_rejected('        "./target/cli-stage/unity-rs$ext" --help > /dev/null\n')

    def test_commented_cli_smoke_is_rejected(self) -> None:
        self.assert_release_rejected(
            '        "./target/cli-stage/unity-rs$ext" --help > /dev/null\n',
            '        # "./target/cli-stage/unity-rs$ext" --help > /dev/null\n',
        )

    def test_echoed_cli_staging_is_rejected(self) -> None:
        self.assert_release_rejected(
            '        "$py" tools/stage_cli_artifact.py',
            '        echo "$py" tools/stage_cli_artifact.py',
        )

    def test_cli_must_smoke_the_staged_copy_before_packaging(self) -> None:
        smoke = '        "./target/cli-stage/unity-rs$ext" --help > /dev/null\n'
        package = '        cp target/cli-stage/* "$BIN_DIR/"\n'
        altered = self.release.replace(smoke + package, package + smoke, 1)
        self.assertNotEqual(altered, self.release)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_release(altered)

    def test_cli_archives_must_stay_flat(self) -> None:
        self.assert_release_rejected("      layout: flat\n", "      layout: dir\n")

    def test_missing_installed_wheel_test_is_rejected(self) -> None:
        self.assert_release_rejected("        python -I tests/installed_wheel.py\n")

    def test_abi3_wheel_must_be_tested_on_python_314(self) -> None:
        self.assert_release_rejected('      python-version: "3.14"\n', '      python-version: "3.13"\n')

    def test_missing_npm_pack_is_rejected(self) -> None:
        self.assert_release_rejected("          npm pack\n")

    def test_node_package_test_is_required(self) -> None:
        self.assert_release_rejected("      - run: npm run test:package\n")

    def test_distribution_count_check_is_required(self) -> None:
        self.assert_release_rejected("len(wheels) != 6 or len(sdists) != 1", "len(wheels) < 1")

    def test_missing_pypi_oidc_permission_is_rejected(self) -> None:
        self.assert_release_rejected("      id-token: write\n")

    def test_pypi_must_wait_for_the_distribution_check(self) -> None:
        self.assert_release_rejected(
            "    needs: [gate, cli, node, python-dist]\n    if: needs.gate.outputs.is-tag == 'true'\n    runs-on: ubuntu-latest\n    timeout-minutes: 15\n    environment:",
            "    needs: [gate, cli, node]\n    if: needs.gate.outputs.is-tag == 'true'\n    runs-on: ubuntu-latest\n    timeout-minutes: 15\n    environment:",
        )

    def test_gate_must_check_the_node_package_version(self) -> None:
        self.assert_release_rejected("      version-extra-paths: crates/unity-rs-node/package.json\n")

    # --- ci.yml: checks ------------------------------------------------------------------

    def test_missing_structural_audit_self_test_is_rejected(self) -> None:
        self.assert_ci_rejected("        python3 tools/test_ci_matrix.py\n")

    def test_missing_python_surface_audit_tests_are_rejected(self) -> None:
        self.assert_ci_rejected("        python3 tools/test_python_api_surface.py\n")

    def test_missing_node_surface_audit_tests_are_rejected(self) -> None:
        self.assert_ci_rejected("        python3 tools/test_node_api_surface.py\n")

    def test_missing_local_ci_policy_tests_are_rejected(self) -> None:
        self.assert_ci_rejected("        python3 tools/test_local_ci.py\n")

    def test_missing_delivery_scope_tests_are_rejected(self) -> None:
        self.assert_ci_rejected("        python3 tools/test_delivery_scope.py\n")

    def test_missing_core_package_legal_check_is_rejected(self) -> None:
        self.assert_ci_rejected("        python3 tools/check_core_package.py\n")

    def test_missing_rustsec_audit_is_rejected(self) -> None:
        self.assert_ci_rejected("      - run: cargo audit --file Cargo.lock --deny unsound --deny yanked\n")

    def test_echoed_rustsec_audit_is_rejected(self) -> None:
        self.assert_ci_rejected(
            "      - run: cargo audit --file Cargo.lock --deny unsound --deny yanked\n",
            "      - run: echo cargo audit --file Cargo.lock --deny unsound --deny yanked\n",
        )

    def test_environment_value_cannot_impersonate_rustsec_audit(self) -> None:
        self.assert_ci_rejected(
            "      - run: cargo audit --file Cargo.lock --deny unsound --deny yanked\n",
            "      - env:\n          AUDIT: cargo audit --file Cargo.lock --deny unsound --deny yanked\n"
            "        run: true\n",
        )

    def test_node_installs_must_disable_lifecycle_scripts(self) -> None:
        self.assert_ci_rejected("      - run: npm ci --ignore-scripts\n", "      - run: npm ci\n")

    def test_missing_ci_wheel_platform_is_rejected(self) -> None:
        self.assert_ci_rejected(
            '{"label":"windows-arm64","os":"windows-11-arm","target":"aarch64-pc-windows-msvc"},'
        )

    def test_pr_wheels_must_be_release_targets(self) -> None:
        self.assert_ci_rejected(
            '\'[{"label":"linux-x64","os":"ubuntu-latest","target":"x86_64","manylinux":"2_28"},{"label":"macos-arm64"',
            '\'[{"label":"linux-x64","os":"ubuntu-22.04","target":"x86_64","manylinux":"2_28"},{"label":"macos-arm64"',
        )

    def test_missing_mypy_consumer_is_rejected(self) -> None:
        self.assert_ci_rejected("        run: python -m mypy tests/typecheck_api.py\n", "        run: true\n")

    def test_missing_sdist_contents_check_is_rejected(self) -> None:
        self.assert_ci_rejected("          python tests/sdist_contents.py sdist-dist\n")

    def test_sdist_rebuilt_wheel_must_run_the_api_suite(self) -> None:
        marker = "          python tests/sdist_contents.py sdist-dist\n"
        head, tail = self.workflow.split(marker, 1)
        altered = head + marker + tail.replace("          python -I tests/python_api.py\n", "", 1)
        self.assertNotEqual(altered, self.workflow)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_workflow(altered)

    def test_python_floor_must_stay_on_39(self) -> None:
        self.assert_ci_rejected('          python-version: "3.9"\n', '          python-version: "3.10"\n')

    def test_ci_ok_must_wait_for_the_oracles(self) -> None:
        self.assert_ci_rejected("python-floor, oracles, tools", "python-floor, tools")

    def test_tool_coverage_must_include_the_matrix_tests(self) -> None:
        self.assert_ci_rejected("for test in tools/test_ci_matrix.py ", "for test in ")

    # --- oracle script and Node package -------------------------------------------------

    def test_vgmstream_install_creates_destination_directory(self) -> None:
        altered = self.oracles.replace('mkdir -p "$HOME/.local/bin"\n', "", 1)
        self.assertNotEqual(altered, self.oracles)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_oracle_script(altered)

    def test_commented_unitypy_oracle_is_rejected(self) -> None:
        altered = self.oracles.replace(
            "  python -I tests/unitypy_oracle.py\n", "  # python -I tests/unitypy_oracle.py\n", 1
        )
        self.assertNotEqual(altered, self.oracles)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_oracle_script(altered)

    def test_missing_installed_node_tarball_test_is_rejected(self) -> None:
        package = check_ci_matrix.NODE_PACKAGE.read_text(encoding="utf-8")
        altered = package.replace(" && node tests/installed_package.cjs", "", 1)
        self.assertNotEqual(altered, package)
        with self.assertRaises(check_ci_matrix.AuditError):
            check_ci_matrix.validate_node_package(altered)


if __name__ == "__main__":
    unittest.main()
