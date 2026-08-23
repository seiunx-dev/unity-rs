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
    # Use this only for active non-command fields such as an artifact ``path``.
    # Executable evidence must go through ``require_run_commands`` below so an
    # environment value or an ``echo`` cannot impersonate a command.
    active_block = "\n".join(
        line for line in block.splitlines() if not line.lstrip().startswith("#")
    )
    missing = [fragment for fragment in fragments if fragment not in active_block]
    if missing:
        raise AuditError(f"{job_name} is missing required workflow text: {missing}")


def run_commands(block: str) -> list[str]:
    """Return executable command lines from this job's YAML ``run`` steps.

    The release workflow deliberately uses the small GitHub Actions shape
    handled here: a scalar command or a literal/folded block under a step. The
    scanner observes YAML indentation, so comments, names, environment values
    and other keys are not executable evidence. A folded block is one shell
    line; a literal block keeps its command lines separate.
    """

    lines = block.splitlines()
    commands: list[str] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        match = re.fullmatch(r"        run:\s*(.*)", line)
        if match is None:
            index += 1
            continue

        value = match.group(1).strip()
        if value not in {"|", "|-", "|+", ">", ">-", ">+"}:
            if value and not value.startswith("#"):
                commands.append(value)
            index += 1
            continue

        folded = value.startswith(">")
        block_lines: list[str] = []
        index += 1
        while index < len(lines):
            child = lines[index]
            if child.strip() and len(child) - len(child.lstrip()) <= 8:
                break
            command = child.strip()
            if command and not command.startswith("#"):
                block_lines.append(command)
            index += 1
        if folded:
            if block_lines:
                commands.append(" ".join(block_lines))
        else:
            commands.extend(block_lines)
    return commands


def command_matches(command: str, required: str) -> bool:
    """Match a complete executable or a required command prefix."""

    return command == required or command.startswith(f"{required} ")


def require_run_commands(
    block: str, job_name: str, required_commands: tuple[str, ...]
) -> None:
    commands = run_commands(block)
    missing = [
        required
        for required in required_commands
        if not any(command_matches(command, required) for command in commands)
    ]
    if missing:
        raise AuditError(f"{job_name} is missing required run commands: {missing}")


def run_command_position(block: str, job_name: str, required_command: str) -> int:
    for position, command in enumerate(run_commands(block)):
        if command_matches(command, required_command):
            return position
    raise AuditError(f"{job_name} is missing required run command {required_command!r}")


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

    python_block = job_block(workflow, "python")
    require_run_commands(
        python_block,
        "python",
        (
            "maturin build --release --locked",
            "python -I tests/installed_wheel.py",
            "python -I tests/python_api.py",
            "python -m mypy tests/typecheck_api.py",
        ),
    )
    require_fragments(
        python_block, "python", ("path: crates/assetstudio-python/dist/*.whl",)
    )
    cli_block = job_block(workflow, "package-cli")
    require_run_commands(
        cli_block,
        "package-cli",
        (
            "cargo +1.88.0 build --release --locked -p assetstudio-cli",
            "${{ matrix.smoke }}",
            "python tools/stage_cli_artifact.py",
        ),
    )
    require_fragments(cli_block, "package-cli", ("path: target/release/artifact",))
    stage = run_command_position(
        cli_block, "package-cli", "python tools/stage_cli_artifact.py"
    )
    smoke = run_command_position(cli_block, "package-cli", "${{ matrix.smoke }}")
    if smoke < stage:
        raise AuditError("package-cli must stage the upload artifact before smoke-testing it")
    node_block = job_block(workflow, "package-node")
    require_run_commands(
        node_block,
        "package-node",
        (
            "npm run build",
            "npm test",
            "npm run test:package",
            "npm pack",
        ),
    )
    require_fragments(
        node_block, "package-node", ("path: crates/assetstudio-node/*.tgz",)
    )
    require_run_commands(
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
    audio_block = job_block(workflow, "audio-oracle")
    require_run_commands(
        audio_block,
        "audio-oracle",
        (
            'mkdir -p "$HOME/.local/bin"',
            'unzip -j vgmstream.zip vgmstream-cli -d "$HOME/.local/bin"',
            "python3 -c \"import json, os, subprocess; result = subprocess.run(['vgmstream-cli', '-V'], check=False, capture_output=True, text=True); assert result.returncode == 1, result; assert json.loads(result.stdout)['version'] == os.environ['VGMSTREAM_VERSION']\"",
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
