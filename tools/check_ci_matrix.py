#!/usr/bin/env python3
"""Verify the six-platform release shape and the artifact-level checks of CI.

The workflows are thin callers of the shared templates in seiunx-dev/ci-templates:
`release.yml` builds the CLI archives (`rust-release`), the abi3 wheels
(`maturin-wheels`) and the Node packages (a job of its own), and `ci.yml` runs
the checks. GitHub still parses and executes the YAML; this gate prevents a
locally plausible edit from silently dropping one target or one of the required
artifact tests while the documentation keeps saying "six".

It deliberately reads the small YAML shape these files use instead of
implementing a general YAML parser. Executable evidence is taken only from
`run:` steps and from the command inputs handed to the templates
(`build-command`, `test-command`, `extra-lint-command`); comments, step names,
environment values and `echo` lines do not count.
"""

from __future__ import annotations

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
RELEASE_WORKFLOW = ROOT / ".github/workflows/release.yml"
ORACLE_SCRIPT = ROOT / "scripts/ci/run-oracles.sh"
NODE_PACKAGE = ROOT / "crates/unity-rs-node/package.json"
EXPECTED_PLATFORMS = [
    ("ubuntu-latest", "linux-x64"),
    ("ubuntu-24.04-arm", "linux-arm64"),
    ("windows-latest", "windows-x64"),
    ("windows-11-arm", "windows-arm64"),
    ("macos-15-intel", "macos-x64"),
    ("macos-latest", "macos-arm64"),
]
BLOCK_INDICATORS = {"|", "|-", "|+", ">", ">-", ">+"}


class AuditError(ValueError):
    """The checked workflow no longer proves the documented release shape."""


def job_block(workflow: str, job_name: str) -> str:
    lines = workflow.splitlines()
    start = next(
        (index for index, line in enumerate(lines) if line == f"  {job_name}:"),
        None,
    )
    if start is None:
        raise AuditError(f"workflow is missing the {job_name!r} job")
    end = next(
        (
            index
            for index in range(start + 1, len(lines))
            if re.fullmatch(r" {2}[A-Za-z0-9_-]+:", lines[index])
        ),
        len(lines),
    )
    return "\n".join(lines[start:end])


def indentation(line: str) -> int:
    return len(line) - len(line.lstrip())


def _key_values(block: str, key: str) -> list[tuple[str, list[str]]]:
    """Every value of ``key`` in the block: (indicator, content lines).

    A scalar value comes back as ("", [value]). A block scalar keeps its raw
    content lines (indentation stripped, blank lines dropped).
    """

    lines = block.splitlines()
    pattern = re.compile(rf"^(\s*)(?:- )?{re.escape(key)}:(.*)$")
    values: list[tuple[str, list[str]]] = []
    index = 0
    while index < len(lines):
        match = pattern.match(lines[index])
        if match is None or lines[index].lstrip().startswith("#"):
            index += 1
            continue
        owner_indent = len(match.group(1))
        if lines[index].lstrip().startswith("- "):
            owner_indent += 2
        value = match.group(2).strip()
        index += 1
        if value not in BLOCK_INDICATORS:
            values.append(("", [value] if value else []))
            continue
        content: list[str] = []
        while index < len(lines):
            child = lines[index]
            if child.strip() and indentation(child) <= owner_indent:
                break
            if child.strip():
                content.append(child.strip())
            index += 1
        values.append((value, content))
    return values


def key_value(block: str, job_name: str, key: str) -> str:
    """The single scalar or folded value of ``key`` (folded lines joined with spaces)."""

    values = _key_values(block, key)
    if len(values) != 1:
        raise AuditError(f"{job_name} must set {key!r} exactly once, found {len(values)}")
    indicator, content = values[0]
    joined = " ".join(content) if indicator.startswith(">") or not indicator else "\n".join(content)
    return joined.strip().strip("'\"")


def commands(block: str, key: str = "run") -> list[str]:
    """Executable command lines of every ``key`` (run / build-command / ...) in the block."""

    result: list[str] = []
    for indicator, content in _key_values(block, key):
        lines = [line for line in content if not line.startswith("#")]
        if indicator.startswith(">"):
            if lines:
                result.append(" ".join(lines))
        else:
            result.extend(lines)
    return [command for command in result if not command.startswith("echo ")]


def command_matches(command: str, required: str) -> bool:
    """Match a complete executable or a required command prefix."""

    return command == required or command.startswith(f"{required} ")


def require_commands(
    available: list[str], job_name: str, required_commands: tuple[str, ...]
) -> None:
    missing = [
        required
        for required in required_commands
        if not any(command_matches(command, required) for command in available)
    ]
    if missing:
        raise AuditError(f"{job_name} is missing required commands: {missing}")


def command_position(available: list[str], job_name: str, required: str) -> int:
    for position, command in enumerate(available):
        if command_matches(command, required):
            return position
    raise AuditError(f"{job_name} is missing required command {required!r}")


def require_fragments(block: str, job_name: str, fragments: tuple[str, ...]) -> None:
    # Only for non-command fields such as a `uses:` pin or an artifact `path`.
    active = "\n".join(line for line in block.splitlines() if not line.lstrip().startswith("#"))
    missing = [fragment for fragment in fragments if fragment not in active]
    if missing:
        raise AuditError(f"{job_name} is missing required workflow text: {missing}")


def target_list(text: str, job_name: str) -> list[dict[str, object]]:
    try:
        targets = json.loads(text)
    except json.JSONDecodeError as error:
        raise AuditError(f"{job_name} targets are not a JSON list: {error}") from error
    if not isinstance(targets, list) or not all(isinstance(t, dict) for t in targets):
        raise AuditError(f"{job_name} targets must be a JSON list of objects")
    return targets


def require_six_platforms(targets: list[dict[str, object]], job_name: str) -> None:
    actual = [(target.get("os"), target.get("label")) for target in targets]
    if actual != EXPECTED_PLATFORMS:
        raise AuditError(
            f"{job_name} platform list differs from the documented six targets: {actual}"
        )
    for target in targets:
        if target.get("test") is False:
            raise AuditError(f"{job_name} must test every target, {target['label']} is skipped")


def matrix_entries(block: str, job_name: str) -> list[dict[str, str]]:
    lines = block.splitlines()
    try:
        include = next(i for i, line in enumerate(lines) if line.strip() == "include:")
    except StopIteration as error:
        raise AuditError(f"{job_name} has no explicit matrix include list") from error
    base = indentation(lines[include])
    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for line in lines[include + 1 :]:
        if line.strip() and indentation(line) <= base:
            break
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped.startswith("- "):
            if current is not None:
                entries.append(current)
            current = {}
            stripped = stripped[2:]
        key, separator, value = stripped.partition(":")
        if current is None or not separator:
            continue
        if key in current:
            raise AuditError(f"{job_name} matrix entry repeats key {key!r}")
        current[key.strip()] = value.strip()
    if current is not None:
        entries.append(current)
    if not entries:
        raise AuditError(f"{job_name} matrix include list is empty")
    return entries


def validate_release(workflow: str) -> None:
    cli = job_block(workflow, "cli")
    require_fragments(cli, "cli", ("uses: seiunx-dev/ci-templates/.github/workflows/rust-release.yml@v1",))
    require_six_platforms(target_list(key_value(cli, "cli", "targets"), "cli"), "cli")
    # flat unity-rs-cli-<version>-<label> archives, as published since 0.5.0
    require_fragments(cli, "cli", ("      name: unity-rs-cli\n", "      layout: flat\n"))
    build = commands(cli, "build-command")
    require_commands(
        build,
        "cli",
        (
            "cargo build --release --locked -p unity-rs-cli",
            '"$py" tools/stage_cli_artifact.py',
            '"./target/cli-stage/unity-rs$ext" --help',
            'cp target/cli-stage/* "$BIN_DIR/"',
        ),
    )
    stage = command_position(build, "cli", '"$py" tools/stage_cli_artifact.py')
    smoke = command_position(build, "cli", '"./target/cli-stage/unity-rs$ext" --help')
    package = command_position(build, "cli", 'cp target/cli-stage/* "$BIN_DIR/"')
    if not stage < smoke < package:
        raise AuditError("cli must stage the binary, smoke-test the staged copy, then package it")

    wheels = job_block(workflow, "wheels")
    validate_wheel_job(wheels, "wheels", key_value(wheels, "wheels", "targets"))

    node = job_block(workflow, "node")
    actual = [(entry.get("os"), entry.get("label")) for entry in matrix_entries(node, "node")]
    if actual != EXPECTED_PLATFORMS:
        raise AuditError(f"node platform matrix differs from the documented six targets: {actual}")
    node_commands = commands(node)
    require_commands(
        node_commands,
        "node",
        ("npm ci --ignore-scripts", "npm run build", "npm test", "npm run test:package", "npm pack"),
    )
    require_fragments(node, "node", ("path: crates/unity-rs-node/out/*.tgz",))

    dist = job_block(workflow, "python-dist")
    require_fragments(dist, "python-dist", ("len(wheels) != 6 or len(sdists) != 1",))
    pypi = job_block(workflow, "pypi")
    require_fragments(
        pypi,
        "pypi",
        (
            "if: needs.gate.outputs.is-tag == 'true'",
            "needs: [gate, cli, node, python-dist]",
            "name: pypi",
            "id-token: write",
            "pattern: wheels-*",
            "merge-multiple: true",
            "uses: pypa/gh-action-pypi-publish@dc37677b2e1c63e2034f94d8a5b11f265b73ba33",
            "packages-dir: dist/",
        ),
    )
    gate = job_block(workflow, "gate")
    require_fragments(
        gate,
        "gate",
        (
            "uses: seiunx-dev/ci-templates/.github/workflows/release-gate.yml@v1",
            "version-extra-paths: crates/unity-rs-node/package.json",
        ),
    )


def validate_wheel_job(block: str, job_name: str, targets_text: str) -> None:
    require_fragments(
        block,
        job_name,
        (
            "uses: seiunx-dev/ci-templates/.github/workflows/maturin-wheels.yml@v1",
            "working-directory: crates/unity-rs-python",
            "smoke-import: unity_rs",
        ),
    )
    targets = target_list(targets_text, job_name)
    require_six_platforms(targets, job_name)
    for target in targets:
        # Linux wheels keep the manylinux_2_28 floor; maturin audits the others on the host.
        linux = str(target["label"]).startswith("linux-")
        if (target.get("manylinux") == "2_28") != linux:
            raise AuditError(f"{job_name} must pin manylinux 2_28 on Linux only: {target}")
    if key_value(block, job_name, "python-version") != "3.14":
        raise AuditError(f"{job_name} must test the abi3 wheel on Python 3.14")
    require_commands(
        commands(block, "test-command"),
        job_name,
        ("python -I tests/installed_wheel.py", "python -I tests/python_api.py"),
    )


def validate_workflow(workflow: str) -> None:
    rust = job_block(workflow, "rust")
    require_commands(
        commands(rust, "extra-lint-command"),
        "rust",
        (
            "python3 tools/test_local_ci.py",
            "python3 tools/check_python_api_surface.py",
            "python3 tools/test_python_api_surface.py",
            "python3 tools/check_node_api_surface.py",
            "python3 tools/test_node_api_surface.py",
            "python3 tools/check_ci_matrix.py",
            "python3 tools/test_ci_matrix.py",
            "cargo doc --workspace --no-deps --locked",
            "cargo package --locked -p unity-rs-core",
            "python3 tools/generate_dependency_licenses.py --check",
            "python3 tools/check_delivery_scope.py",
            "python3 tools/test_delivery_scope.py",
            "python3 tools/check_core_package.py",
        ),
    )
    for job_name in ("rust-windows", "rust-macos"):
        require_fragments(
            job_block(workflow, job_name),
            job_name,
            ("uses: seiunx-dev/ci-templates/.github/workflows/rust-ci.yml@v1",),
        )
    require_commands(
        commands(job_block(workflow, "audit")),
        "audit",
        ("cargo audit --file Cargo.lock --deny unsound --deny yanked",),
    )
    require_commands(
        commands(job_block(workflow, "node")),
        "node",
        ("npm ci --ignore-scripts", "npm run build:debug", "npm test", "npm run test:package"),
    )

    # The main/dispatch half of the expression is the full six-platform list.
    wheels = job_block(workflow, "wheels")
    lists = re.findall(r"'(\[\{.*?\}\])'", key_value(wheels, "wheels", "targets"))
    if len(lists) != 2:
        raise AuditError("wheels must list the PR subset and the six release platforms")
    pr_targets = target_list(lists[0], "wheels (pull requests)")
    validate_wheel_job(wheels, "wheels", lists[1])
    full = {t["label"]: t for t in target_list(lists[1], "wheels")}
    if not pr_targets or any(full.get(t.get("label")) != t for t in pr_targets):
        raise AuditError("wheels pull-request targets must be a subset of the release targets")

    floor = job_block(workflow, "python-floor")
    floor_commands = commands(floor)
    require_commands(
        floor_commands,
        "python-floor",
        (
            "maturin build --release --locked",
            "maturin build --release --sdist",
            "python tests/sdist_contents.py sdist-dist",
            "python -I tests/installed_wheel.py",
            "python -I tests/python_api.py",
            "python -m mypy tests/typecheck_api.py",
        ),
    )
    if sum(command_matches(c, "python -I tests/python_api.py") for c in floor_commands) < 2:
        raise AuditError("python-floor must test both the floor wheel and the sdist-rebuilt wheel")
    require_fragments(floor, "python-floor", ('python-version: "3.9"',))

    require_commands(
        commands(job_block(workflow, "oracles")), "oracles", ("./scripts/ci/run-oracles.sh",)
    )
    require_commands(
        commands(job_block(workflow, "tools")),
        "tools",
        (
            "python -m coverage xml -o coverage/python.xml",
            "for test in tools/test_ci_matrix.py tools/test_delivery_scope.py",
        ),
    )
    ci_ok = job_block(workflow, "ci-ok")
    for job_name in ("rust", "rust-windows", "rust-macos", "audit", "node", "wheels", "python-floor", "oracles", "sonar"):
        if not re.search(rf"needs: \[[^\]]*\b{re.escape(job_name)}\b", ci_ok):
            raise AuditError(f"CI OK must depend on {job_name}")


def validate_oracle_script(script: str) -> None:
    active = [line.strip() for line in script.splitlines() if line.strip() and not line.strip().startswith("#")]
    require_commands(
        active,
        "run-oracles.sh",
        (
            "cargo test -p unity-rs-core --test dotnet_oracle --locked -- --ignored --nocapture",
            "python3 tools/test_monoschema.py",
            'mkdir -p "$HOME/.local/bin"',
            'unzip -o -j vgmstream.zip vgmstream-cli -d "$HOME/.local/bin"',
            "python3 -c \"import json, os, subprocess; r = subprocess.run(['vgmstream-cli', '-V'], check=False, capture_output=True, text=True); assert r.returncode == 1, r; assert json.loads(r.stdout)['version'] == os.environ['VGMSTREAM_VERSION']\"",
            "cargo test -p unity-rs-core --lib --locked -- --ignored",
            "python -I tests/unitypy_oracle.py",
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
    validate_release(RELEASE_WORKFLOW.read_text(encoding="utf-8"))
    validate_oracle_script(ORACLE_SCRIPT.read_text(encoding="utf-8"))
    validate_node_package(NODE_PACKAGE.read_text(encoding="utf-8"))
    print("CI release audit passed (Python, CLI and Node each have six targets)")


if __name__ == "__main__":
    main()
