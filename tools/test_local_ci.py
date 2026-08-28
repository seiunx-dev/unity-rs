#!/usr/bin/env python3
"""Regression tests for the local CI runner's completion policy."""

from __future__ import annotations

import contextlib
import io
import sys
import unittest
from unittest import mock

import local_ci


class LocalCiResultTests(unittest.TestCase):
    def invoke(
        self,
        arguments: list[str],
        available: list[local_ci.Group],
        tools: dict[str, str | None],
        versions: dict[str, str | None] | None = None,
    ) -> tuple[int, str]:
        versions = versions or {}
        with (
            mock.patch.object(sys, "argv", ["local_ci.py", *arguments]),
            mock.patch.object(local_ci, "groups", return_value=available),
            mock.patch.object(
                local_ci.shutil,
                "which",
                side_effect=lambda name: tools.get(name),
            ),
            mock.patch.object(
                local_ci,
                "executable_version",
                side_effect=lambda executable: versions.get(executable),
            ),
            contextlib.redirect_stdout(io.StringIO()) as output,
            contextlib.redirect_stderr(io.StringIO()),
        ):
            return local_ci.main(), output.getvalue()

    def test_optional_missing_tool_remains_a_success_by_default(self) -> None:
        optional = local_ci.Group(
            "oracle",
            [],
            requires="dotnet",
            reason="needs the managed oracle",
        )
        status, output = self.invoke(
            ["oracle"],
            [optional],
            {"cargo": "/tool/cargo", "dotnet": None},
        )
        self.assertEqual(status, 0)
        self.assertIn("all steps passed (1 group(s) skipped)", output)

    def test_fail_on_skip_rejects_the_same_missing_tool(self) -> None:
        optional = local_ci.Group(
            "oracle",
            [],
            requires="dotnet",
            reason="needs the managed oracle",
        )
        status, output = self.invoke(
            ["--fail-on-skip", "oracle"],
            [optional],
            {"cargo": "/tool/cargo", "dotnet": None},
        )
        self.assertEqual(status, 1)
        self.assertIn("1 group(s) skipped under --fail-on-skip", output)

    def test_fail_on_skip_accepts_a_runnable_empty_group(self) -> None:
        required = local_ci.Group("quality", [])
        status, output = self.invoke(
            ["--fail-on-skip", "quality"],
            [required],
            {"cargo": "/tool/cargo"},
        )
        self.assertEqual(status, 0)
        self.assertIn("all steps passed (0 group(s) skipped)", output)

    def test_fail_on_skip_rejects_a_missing_additional_tool(self) -> None:
        cross = local_ci.Group(
            "cross",
            [],
            requires="linux-gcc",
            additional_requires=("windows-gcc",),
            reason="needs both cross C toolchains",
        )
        status, output = self.invoke(
            ["--fail-on-skip", "cross"],
            [cross],
            {
                "cargo": "/tool/cargo",
                "linux-gcc": "/tool/linux-gcc",
                "windows-gcc": None,
            },
        )
        self.assertEqual(status, 1)
        self.assertIn("windows-gcc not found", output)
        self.assertIn("1 group(s) skipped under --fail-on-skip", output)

    def test_pinned_tool_version_is_enforced(self) -> None:
        security = local_ci.Group(
            "security",
            [],
            requires="cargo-audit",
            required_version_output="cargo-audit 0.22.2",
            reason="needs cargo-audit 0.22.2",
        )
        status, output = self.invoke(
            ["--fail-on-skip", "security"],
            [security],
            {"cargo": "/tool/cargo", "cargo-audit": "/tool/cargo-audit"},
            {"/tool/cargo-audit": "cargo-audit 0.21.2"},
        )
        self.assertEqual(status, 1)
        self.assertIn("expected cargo-audit 0.22.2", output)
        self.assertIn("found cargo-audit 0.21.2", output)

    def test_pinned_tool_version_accepts_an_exact_match(self) -> None:
        security = local_ci.Group(
            "security",
            [],
            requires="cargo-audit",
            required_version_output="cargo-audit 0.22.2",
            reason="needs cargo-audit 0.22.2",
        )
        status, output = self.invoke(
            ["--fail-on-skip", "security"],
            [security],
            {"cargo": "/tool/cargo", "cargo-audit": "/tool/cargo-audit"},
            {"/tool/cargo-audit": "cargo-audit 0.22.2"},
        )
        self.assertEqual(status, 0)
        self.assertIn("all steps passed (0 group(s) skipped)", output)

    def test_unknown_group_is_still_a_usage_error(self) -> None:
        status, _ = self.invoke(
            ["--fail-on-skip", "missing"],
            [local_ci.Group("quality", [])],
            {"cargo": "/tool/cargo"},
        )
        self.assertEqual(status, 2)

    def test_linux_node_setup_hardens_download_and_install(self) -> None:
        command = local_ci.linux_node_command("x64", "x64")
        self.assertIn("curl --proto '=https' --proto-redir '=https'", command)
        self.assertIn("npm ci --silent --ignore-scripts", command)


if __name__ == "__main__":
    unittest.main()
