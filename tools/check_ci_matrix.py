#!/usr/bin/env python3
"""Verify the six-platform release matrix and its artifact-level checks.

This deliberately validates the small workflow shape we publish instead of
implementing a general YAML parser.  GitHub still parses and executes the YAML;
this gate prevents a locally plausible edit from silently dropping one target
or one of the required tests while the progress document keeps saying "six".
"""

from __future__ import annotations

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
NODE_PACKAGE = ROOT / "crates/assetstudio-node/package.json"
EXPECTED_PLATFORMS = [
    ("ubuntu-latest", "linux-x64"),
    ("ubuntu-24.04-arm", "linux-arm64"),
    ("windows-latest", "windows-x64"),
    ("windows-11-arm", "windows-arm64"),
    ("macos-15-intel", "macos-x64"),
    ("macos-latest", "macos-arm64"),
]


class AuditError(ValueError):
    """The checked workflow no longer proves the documented release shape."""


def job_block(workflow: str, job_name: str) -> str:
    lines = workflow.splitlines()
    start = next(
        (index for index, line in enumerate(lines) if line == f"  {job_name}:"),
        None,
    )
    if start is None:
        raise AuditError(f"CI workflow is missing the {job_name!r} job")
    end = next(
        (
            index
            for index in range(start + 1, len(lines))
            if re.fullmatch(r"  [A-Za-z0-9_-]+:", lines[index])
        ),
        len(lines),
    )
    return "\n".join(lines[start:end])


def matrix_entries(block: str, job_name: str) -> list[dict[str, str]]:
    lines = block.splitlines()
    try:
        include = lines.index("        include:")
    except ValueError as error:
        raise AuditError(f"{job_name} has no explicit matrix include list") from error

    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for line in lines[include + 1 :]:
        if line == "    steps:":
            break
        first = re.fullmatch(r"          - ([A-Za-z0-9_-]+):\s*(.+)", line)
        member = re.fullmatch(r"            ([A-Za-z0-9_-]+):\s*(.+)", line)
        if first:
            if current is not None:
                entries.append(current)
            current = {first.group(1): first.group(2)}
        elif member and current is not None:
            key, value = member.groups()
            if key in current:
                raise AuditError(f"{job_name} matrix entry repeats key {key!r}")
            current[key] = value
    if current is not None:
        entries.append(current)
    if not entries:
        raise AuditError(f"{job_name} matrix include list is empty")
    return entries


def require_fragments(block: str, job_name: str, fragments: tuple[str, ...]) -> None:
    # A disabled step remains literal text in YAML. Counting comments would let
    # ``# run: cargo audit ...`` satisfy the audit even though GitHub executes
    # nothing, which is precisely the silent evidence loss this gate prevents.
    active_block = "\n".join(
        line for line in block.splitlines() if not line.lstrip().startswith("#")
    )
    missing = [fragment for fragment in fragments if fragment not in active_block]
    if missing:
        raise AuditError(f"{job_name} is missing required workflow text: {missing}")


def validate_platform_job(workflow: str, job_name: str) -> None:
    block = job_block(workflow, job_name)
    entries = matrix_entries(block, job_name)
    actual = [(entry.get("os"), entry.get("artifact")) for entry in entries]
    if actual != EXPECTED_PLATFORMS:
        raise AuditError(
            f"{job_name} platform matrix differs from the documented six targets: {actual}"
        )
    required_entry_keys = {
        "python": {"os", "artifact", "build_python"},
        "package-cli": {"os", "artifact", "binary", "smoke"},
        "package-node": {"os", "artifact"},
    }[job_name]
    for entry in entries:
        if set(entry) != required_entry_keys:
            raise AuditError(
                f"{job_name} matrix entry has unexpected keys: {sorted(entry)}"
            )
        if job_name == "package-cli":
            windows = entry["artifact"].startswith("windows-")
            expected_binary = (
                "target/release/assetstudio.exe"
                if windows
                else "target/release/assetstudio"
            )
            expected_smoke = (
                ".\\target\\release\\artifact\\assetstudio.exe --help"
                if windows
                else "./target/release/artifact/assetstudio --help"
            )
            if entry["binary"] != expected_binary or entry["smoke"] != expected_smoke:
                raise AuditError(
                    "package-cli must smoke-test the staged binary for "
                    f"{entry['artifact']}: {entry}"
                )


def validate_workflow(workflow: str) -> None:
    for job_name in ("python", "package-cli", "package-node"):
        validate_platform_job(workflow, job_name)

    require_fragments(
        job_block(workflow, "python"),
        "python",
        (
            "maturin build --release --locked",
            "python -I tests/installed_wheel.py",
            "python -I tests/python_api.py",
            "python -m mypy tests/typecheck_api.py",
            "path: crates/assetstudio-python/dist/*.whl",
        ),
    )
    require_fragments(
        job_block(workflow, "package-cli"),
        "package-cli",
        (
            "cargo +1.88.0 build --release --locked -p assetstudio-cli",
            "run: ${{ matrix.smoke }}",
            "python tools/stage_cli_artifact.py",
            "path: target/release/artifact",
        ),
    )
    cli_block = job_block(workflow, "package-cli")
    stage = cli_block.index("run: python tools/stage_cli_artifact.py")
    smoke = cli_block.index("run: ${{ matrix.smoke }}")
    if smoke < stage:
        raise AuditError("package-cli must stage the upload artifact before smoke-testing it")
    require_fragments(
        job_block(workflow, "package-node"),
        "package-node",
        (
            "run: npm run build",
            "run: npm test",
            "run: npm run test:package",
            "run: npm pack",
            "path: crates/assetstudio-node/*.tgz",
        ),
    )
    require_fragments(
        job_block(workflow, "quality"),
        "quality",
        (
            "python3 tools/test_local_ci.py",
            "python3 tools/check_ci_matrix.py",
            "python3 tools/test_ci_matrix.py",
            "python3 tools/check_python_api_surface.py",
            "python3 tools/test_python_api_surface.py",
            "python3 tools/check_node_api_surface.py",
            "python3 tools/test_node_api_surface.py",
            "cargo install cargo-audit --version 0.22.2 --locked --no-default-features",
            "cargo audit --file Cargo.lock --deny unsound --deny yanked",
            "python3 tools/check_delivery_scope.py",
            "python3 tools/test_delivery_scope.py",
        ),
    )
    require_fragments(
        job_block(workflow, "audio-oracle"),
        "audio-oracle",
        (
            'mkdir -p "$HOME/.local/bin"',
            'unzip -j vgmstream.zip vgmstream-cli -d "$HOME/.local/bin"',
            "run: vgmstream-cli -h > /dev/null",
        ),
    )


def validate_node_package(package_json: str) -> None:
    package = json.loads(package_json)
    package_test = package.get("scripts", {}).get("test:package", "")
    required = (
        "node tests/package_contents.cjs",
        "node tests/installed_package.cjs",
    )
    missing = [command for command in required if command not in package_test]
    if missing:
        raise AuditError(
            "Node test:package no longer checks both tarball contents and an installed package: "
            f"{missing}"
        )


def main() -> None:
    validate_workflow(WORKFLOW.read_text(encoding="utf-8"))
    validate_node_package(NODE_PACKAGE.read_text(encoding="utf-8"))
    print("CI release audit passed (Python, CLI and Node each have six targets)")


if __name__ == "__main__":
    main()
