#!/usr/bin/env python3
"""Runs every step the CI workflow runs, on this machine.

CI has not executed repository steps since the LZMA commit, and the gap was not
harmless: the Python suite had been failing the whole time and nothing surfaced
it until the wheel was built by hand. This exists so that "run what CI runs" is
one command on any platform rather than a sequence someone has to reconstruct
from the workflow file.

It is not a replacement for CI. CI runs this matrix on Linux, Windows and
macOS; this runs it wherever you are. The value is that a contributor on a
platform the maintainer cannot reach can produce the same evidence.

Two groups reach past this machine. `cross` compiles the full workspace and
tests for Linux x86-64, then Core/CLI/Python for Windows x86-64 without running
them, which catches what fails to build elsewhere -- a path assumption, a
missing `cfg`, a type that is only `Send` on one platform -- but says nothing
about behaviour; it needs both cross C toolchains because zstd builds from C
sources. The Node addon is left to a real Windows runner because a Unix cross
environment has no `libnode.dll`. `linux` goes further and runs the
Core/CLI suite, release CLI, Python wheel and release Node package for amd64 and
arm64 inside containers on the toolchain CI pins, which is behaviour and
artifact validation rather than compilation. Neither covers Windows or macOS
at once, so CI still has work to do.

The `outputs` group is a second implementation rather than a second run: the
crate's binary FBX reader and writer were built together, so their agreement
shows they share assumptions, not that either matches the format. That group
exports through the CLI and checks the bytes with a parser written from the
format's rules, and does the same for the ASCII form: braces, the counts
`Definitions` declares against the objects actually written, connections
resolving to objects that exist, and arrays holding as many values as they
claim. The WAV containers go to Python's own `wave` module, which was
not written for this project and refuses files whose header fields disagree.
The PNG check is the same idea one layer down: the encoder's chunk framing,
CRCs and scanline filters are all this project's own, and its unit test checks
the CRCs with the project's own CRC routine, so a wrong table would produce a
file the test accepts and every viewer rejects. `zlib` supplies an independent
CRC-32 and inflate, and the decoded pixels are compared to the source the file
was built from, bottom-up-to-top-down row flip included. BMP and TGA are the
same problem again and are checked the same way, decoded from the two formats'
rules: the row order comes from the header rather than being assumed, and the
BMP channel layout comes from the masks the file declares rather than from the
convention every reader falls back on, so a file whose header disagrees with
its own pixels is caught rather than silently read correctly.

Steps are grouped, and a group that cannot run because a tool is missing is
reported as skipped rather than failed -- the .NET oracle, `vgmstream-cli` and
UnityPy are all optional. `security` similarly needs the exact `cargo-audit`
version pinned in the workflow. Anything that runs and fails is a failure. Use
`--fail-on-skip` for release or close-out evidence, where a missing tool must
make the command fail instead of weakening the result.

    python3 tools/local_ci.py             # everything available
    python3 tools/local_ci.py --list      # what would run
    python3 tools/local_ci.py rust python # only these groups
    python3 tools/local_ci.py --fail-on-skip quality rust security python
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NODE = ROOT / "crates" / "assetstudio-node"
PYTHON = ROOT / "crates" / "assetstudio-python"
# The wheel is installed here rather than into whatever interpreter happens to
# be on PATH. A Homebrew or distribution Python refuses `pip install` outright
# under PEP 668, and one that does not would be left carrying a build of this
# project afterwards. System site packages stay visible so an already-installed
# UnityPy is still importable for the differential.
VENV = PYTHON / ".ci-venv"
VENV_314 = PYTHON / ".ci-venv-314"
MYPY_VERSION = "1.18.2"
CLI_BINARY = ROOT / "target" / "release" / (
    "assetstudio.exe" if os.name == "nt" else "assetstudio"
)

# Several steps below verify with `assert` -- the inline `-c` snippets here and
# the Python gates they invoke -- and `-O` or `PYTHONOPTIMIZE` deletes those
# outright rather than skipping them. Children inherit this interpreter's
# environment, so one such run would report every group green having checked
# nothing. Each gate refuses on its own too; this catches it once, up front.
if not __debug__:
    raise SystemExit(
        "refusing to run with assertions disabled (-O / PYTHONOPTIMIZE): "
        "the steps below verify with asserts"
    )


@dataclass
class Step:
    name: str
    command: list[str]
    cwd: Path = ROOT
    env: dict[str, str] = field(default_factory=dict)


@dataclass
class Group:
    name: str
    steps: list[Step]
    # A tool that must exist for the group to mean anything. None means the
    # group only needs cargo, which the runner requires up front.
    requires: str | None = None
    # Further tools needed by a compound group. Checking them before the first
    # step avoids spending time on one platform and then failing halfway only
    # because the next platform's C compiler was absent.
    additional_requires: tuple[str, ...] = ()
    # Exact stdout from ``<requires> --version`` when reproducible evidence
    # depends on a pinned tool release. A different installed version is a
    # missing prerequisite, not permission to silently produce weaker proof.
    required_version_output: str | None = None
    # Optional semantic prerequisite, such as an import inside a virtualenv.
    # A failed probe skips the group just like a missing executable does.
    probe: list[str] | None = None
    reason: str = ""


def venv_interpreter() -> str:
    """The interpreter inside `VENV`, wherever this platform puts it."""
    if os.name == "nt":
        return str(VENV / "Scripts" / "python.exe")
    return str(VENV / "bin" / "python")


def python314_interpreter() -> str:
    """The Python 3.14 interpreter inside `VENV_314`."""
    if os.name == "nt":
        return str(VENV_314 / "Scripts" / "python.exe")
    return str(VENV_314 / "bin" / "python")


def groups(interpreter: str) -> list[Group]:
    return [
        Group(
            "quality",
            [
                Step("format", ["cargo", "fmt", "--all", "--", "--check"]),
                Step(
                    "local CI runner tests",
                    [sys.executable, "tools/test_local_ci.py"],
                ),
                Step(
                    "Python API surface audit",
                    [sys.executable, "tools/check_python_api_surface.py"],
                ),
                Step(
                    "Python API surface audit tests",
                    [sys.executable, "tools/test_python_api_surface.py"],
                ),
                Step(
                    "Node API surface audit",
                    [sys.executable, "tools/check_node_api_surface.py"],
                ),
                Step(
                    "Node API surface audit tests",
                    [sys.executable, "tools/test_node_api_surface.py"],
                ),
                Step(
                    "CI release matrix audit",
                    [sys.executable, "tools/check_ci_matrix.py"],
                ),
                Step(
                    "CI release matrix audit tests",
                    [sys.executable, "tools/test_ci_matrix.py"],
                ),
                Step(
                    "clippy",
                    [
                        "cargo", "clippy", "--workspace", "--all-targets",
                        "--locked", "--", "-D", "warnings",
                    ],
                ),
                Step(
                    "rustdoc",
                    ["cargo", "doc", "--workspace", "--no-deps", "--locked"],
                    env={"RUSTDOCFLAGS": "-D warnings"},
                ),
                Step(
                    "package the core crate",
                    [
                        "cargo", "package", "--allow-dirty", "--locked",
                        "-p", "assetstudio-core",
                    ],
                ),
                Step(
                    "locked dependency licenses",
                    [
                        "python3", "tools/generate_dependency_licenses.py",
                        "--check",
                    ],
                ),
                Step(
                    "headless delivery scope",
                    ["python3", "tools/check_delivery_scope.py"],
                ),
                Step(
                    "headless delivery scope tests",
                    ["python3", "tools/test_delivery_scope.py"],
                ),
                Step(
                    "Core crate legal files",
                    ["python3", "tools/check_core_package.py"],
                ),
            ],
        ),
        Group(
            "rust",
            [
                Step("build workspace", ["cargo", "build", "--workspace", "--locked"]),
                Step("workspace tests", ["cargo", "test", "--workspace", "--locked"]),
            ],
        ),
        Group(
            "security",
            [
                Step(
                    "RustSec dependency audit",
                    [
                        "cargo-audit", "audit", "--file", "Cargo.lock",
                        "--deny", "unsound", "--deny", "yanked",
                    ],
                ),
            ],
            requires="cargo-audit",
            required_version_output="cargo-audit 0.22.2",
            reason="needs cargo-audit 0.22.2",
        ),
        Group(
            "cli-package",
            [
                Step(
                    "build release CLI",
                    [
                        "cargo", "build", "--release", "--locked",
                        "-p", "assetstudio-cli",
                    ],
                ),
                Step("smoke exact release CLI", [str(CLI_BINARY), "--help"]),
                Step(
                    "stage and smoke release CLI artifact",
                    [sys.executable, "-c", STAGE_CLI_ARTIFACT, str(CLI_BINARY)],
                ),
            ],
        ),
        Group(
            "oracle",
            [
                Step(
                    "managed differential",
                    [
                        "cargo", "test", "-p", "assetstudio-core", "--test",
                        "dotnet_oracle", "--locked", "--", "--ignored",
                    ],
                ),
                Step(
                    "MonoBehaviour schema generator",
                    ["python3", "tools/test_monoschema.py"],
                ),
            ],
            requires="dotnet",
            reason="the managed differential needs the .NET SDK and an AssetStudio checkout",
        ),
        Group(
            "audio",
            [
                Step(
                    "audio differential",
                    [
                        "cargo", "test", "-p", "assetstudio-core", "--lib",
                        "--locked", "--", "--ignored",
                    ],
                )
            ],
            requires="vgmstream-cli",
            reason="the audio differential decodes with vgmstream-cli",
        ),
        Group(
            "outputs",
            [
                Step(
                    "binary FBX verifier regressions",
                    ["python3", "tools/test_validate_fbx_binary.py"],
                ),
                Step(
                    "binary FBX validity",
                    ["python3", "tools/validate_fbx_binary.py", "--cli"],
                ),
                Step(
                    "ASCII FBX consistency",
                    ["python3", "tools/validate_fbx_ascii.py", "--cli"],
                ),
                Step(
                    "WAV containers",
                    ["python3", "tools/validate_wav_output.py"],
                ),
                Step(
                    "PNG containers",
                    ["python3", "tools/validate_png_output.py"],
                ),
                Step(
                    "BMP and TGA containers",
                    ["python3", "tools/validate_image_output.py"],
                ),
            ],
        ),
        Group(
            "cross",
            [
                Step(
                    "compile for Linux x86-64",
                    [
                        "cargo", "check", "--workspace", "--all-targets",
                        "--locked", "--target", "x86_64-unknown-linux-gnu",
                    ],
                    env={
                        "CC_x86_64_unknown_linux_gnu": "x86_64-unknown-linux-gnu-gcc",
                        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER": (
                            "x86_64-unknown-linux-gnu-gcc"
                        ),
                    },
                ),
                Step(
                    "compile Core/CLI/Python for Windows x86-64",
                    [
                        "cargo", "check", "-p", "assetstudio-core",
                        "-p", "assetstudio-cli", "-p", "assetstudio-python",
                        "--all-targets", "--locked", "--target",
                        "x86_64-pc-windows-gnu",
                    ],
                    env={
                        "CC_x86_64_pc_windows_gnu": "x86_64-w64-mingw32-gcc",
                        "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER": (
                            "x86_64-w64-mingw32-gcc"
                        ),
                    },
                ),
            ],
            requires="x86_64-unknown-linux-gnu-gcc",
            additional_requires=("x86_64-w64-mingw32-gcc",),
            reason=(
                "cross-compiling needs Linux and Windows C toolchains for "
                "zstd's C sources"
            ),
        ),
        Group(
            "linux",
            [
                # Both architectures CI builds for. Each gets its own target
                # directory so the two do not fight over one cache.
                *(
                    Step(
                        f"Core/CLI tests and release CLI on Linux {architecture}",
                        [
                            "docker", "run", "--rm",
                            "--platform", f"linux/{architecture}",
                            "-v", f"{ROOT}:/src", "-w", "/src",
                            "-e", f"CARGO_TARGET_DIR=/tmp/target-{architecture}",
                            LINUX_IMAGE,
                            "sh", "-c", LINUX_TESTS,
                        ],
                    )
                    for architecture in ("amd64", "arm64")
                ),
                *(
                    Step(
                        f"Python wheel on Linux {architecture}",
                        [
                            "docker", "run", "--rm",
                            "--platform", f"linux/{architecture}",
                            "-v", f"{ROOT}:/src", "-w", "/src/crates/assetstudio-python",
                            "-e", f"CARGO_TARGET_DIR=/tmp/target-python-{architecture}",
                            LINUX_IMAGE,
                            "sh", "-c", LINUX_WHEEL,
                        ],
                    )
                    for architecture in ("amd64", "arm64")
                ),
                *(
                    Step(
                        f"Node addon on Linux {architecture}",
                        [
                            "docker", "run", "--rm",
                            "--platform", f"linux/{architecture}",
                            "-v", f"{ROOT}:/src:ro", "-w", "/tmp",
                            "-e", f"CARGO_TARGET_DIR=/tmp/target-node-{architecture}",
                            LINUX_IMAGE,
                            "sh", "-c", linux_node_command(node_architecture, addon_architecture),
                        ],
                    )
                    for architecture, node_architecture, addon_architecture in (
                        ("amd64", "x64", "x64"),
                        ("arm64", "arm64", "arm64"),
                    )
                ),
            ],
            requires="docker",
            reason="running the suite on Linux needs a container runtime",
        ),
        Group(
            "node",
            [
                Step("install", ["npm", "ci"], cwd=NODE),
                Step("build addon", ["npm", "run", "build:debug"], cwd=NODE),
                Step("addon tests", ["npm", "test"], cwd=NODE),
                Step("package contents", ["npm", "run", "test:package"], cwd=NODE),
                Step("build release addon", ["npm", "run", "build"], cwd=NODE),
                Step("release addon tests", ["npm", "test"], cwd=NODE),
                Step(
                    "release package contents",
                    ["npm", "run", "test:package"],
                    cwd=NODE,
                ),
                Step(
                    "package tarball",
                    [sys.executable, "-c", PACKAGE_NODE_TARBALL],
                    cwd=NODE,
                ),
            ],
            requires="npm",
            reason="the Node addon needs npm",
        ),
        Group(
            "python",
            [
                Step(
                    "build wheel",
                    [
                        "maturin", "build", "--release", "--locked",
                        "--compatibility", "pypi", "--interpreter", interpreter,
                        "--out", "dist",
                    ],
                    cwd=PYTHON,
                ),
                Step(
                    "build source distribution",
                    [
                        "maturin", "build", "--release", "--sdist",
                        "--compatibility", "pypi", "--interpreter", interpreter,
                        "--out", "sdist-dist",
                    ],
                    cwd=PYTHON,
                ),
                Step(
                    "source distribution contents",
                    [interpreter, "tests/sdist_contents.py", "sdist-dist"],
                    cwd=PYTHON,
                ),
                Step(
                    "create venv",
                    [interpreter, "-m", "venv", "--clear",
                     "--system-site-packages", str(VENV)],
                ),
                Step(
                    "install source-distribution wheel",
                    [venv_interpreter(), "-c", INSTALL_SDIST_WHEEL],
                    cwd=PYTHON,
                ),
                Step(
                    "source-distribution wheel surface",
                    [venv_interpreter(), "-I", "tests/installed_wheel.py"],
                    cwd=PYTHON,
                ),
                Step(
                    "source-distribution Python API",
                    [venv_interpreter(), "-I", "tests/python_api.py"],
                    cwd=PYTHON,
                ),
                Step("install wheel", [venv_interpreter(), "-c", INSTALL_WHEEL], cwd=PYTHON),
                Step(
                    "wheel surface",
                    [venv_interpreter(), "-I", "tests/installed_wheel.py"],
                    cwd=PYTHON,
                ),
                Step("python api", [venv_interpreter(), "-I", "tests/python_api.py"], cwd=PYTHON),
            ],
            requires="maturin",
            reason="the Python wheel and source distribution need maturin",
        ),
        Group(
            "python314",
            [
                Step(
                    "create Python 3.14 venv",
                    ["python3.14", "-m", "venv", "--clear", str(VENV_314)],
                ),
                Step(
                    "install abi3 wheel",
                    [python314_interpreter(), "-c", INSTALL_WHEEL],
                    cwd=PYTHON,
                ),
                Step(
                    "Python 3.14 wheel surface",
                    [python314_interpreter(), "-I", "tests/installed_wheel.py"],
                    cwd=PYTHON,
                ),
                Step(
                    "Python 3.14 API",
                    [python314_interpreter(), "-I", "tests/python_api.py"],
                    cwd=PYTHON,
                ),
            ],
            requires="python3.14",
            reason="abi3 forward-compatibility needs a Python 3.14 interpreter",
        ),
        Group(
            "typing",
            [
                Step(
                    "strict Python 3.9 API consumer",
                    [
                        "uvx", "--from", f"mypy=={MYPY_VERSION}",
                        "mypy", "tests/typecheck_api.py",
                    ],
                    cwd=PYTHON,
                )
            ],
            requires="uvx",
            reason="the Python typing surface needs the pinned mypy checker",
        ),
        Group(
            "unitypy",
            [
                Step(
                    "UnityPy differential",
                    [venv_interpreter(), "-I", "tests/unitypy_oracle.py"],
                    cwd=PYTHON,
                )
            ],
            probe=[venv_interpreter(), "-c", "import assetstudio, UnityPy"],
            reason="needs the built wheel and UnityPy in the same interpreter",
        ),
    ]


# The toolchain CI pins, so a failure here is a failure there.
LINUX_IMAGE = "rust:1.88-slim-bookworm"

# zstd builds from C sources, so the image needs a compiler.
LINUX_SETUP = "apt-get update -qq >/dev/null && apt-get install -y -qq gcc >/dev/null"

# The Node addon has its own step, since it needs a Node runtime the image
# does not carry.
LINUX_TESTS = (
    f"{LINUX_SETUP} && apt-get install -y -qq python3 >/dev/null"
    " && cargo test -p assetstudio-core -p assetstudio-cli --locked"
    " && cargo build --release --locked -p assetstudio-cli"
    ' && "$CARGO_TARGET_DIR/release/assetstudio" --help >/dev/null'
    " && python3 tools/stage_cli_artifact.py"
    ' "$CARGO_TARGET_DIR/release/assetstudio" /tmp/cli-artifact'
    ' && test "$(find /tmp/cli-artifact -maxdepth 1 -type f | wc -l)" -eq 4'
    " && /tmp/cli-artifact/assetstudio --help >/dev/null"
)

LINUX_WHEEL = (
    f"{LINUX_SETUP} && apt-get install -y -qq python3 python3-venv >/dev/null"
    " && python3 -m venv /tmp/venv"
    " && /tmp/venv/bin/pip install --quiet maturin"
    " && /tmp/venv/bin/maturin build --release --locked"
    " --interpreter /tmp/venv/bin/python --out /tmp/dist"
    " && /tmp/venv/bin/pip install --quiet --force-reinstall --no-deps /tmp/dist/*.whl"
    " && /tmp/venv/bin/python -I tests/installed_wheel.py"
    " && /tmp/venv/bin/python -I tests/python_api.py"
)

# The image has no Node, and Debian's is older than the one CI uses, so the
# official build is fetched instead of installed from a package manager.
LINUX_NODE_VERSION = "24.0.0"


def linux_node_command(node_architecture: str, addon_architecture: str) -> str:
    archive = f"node-v{LINUX_NODE_VERSION}-linux-{node_architecture}"
    return (
        f"{LINUX_SETUP} && apt-get install -y -qq curl xz-utils >/dev/null"
        f" && curl -fsSL https://nodejs.org/dist/v{LINUX_NODE_VERSION}"
        f"/{archive}.tar.xz -o /tmp/node.tar.xz"
        " && tar -xJf /tmp/node.tar.xz -C /tmp"
        f" && export PATH=/tmp/{archive}/bin:$PATH"
        # Build from a clean copy: the read-only mounted host tree may already
        # contain its own platform addon, which must not leak into this package.
        " && mkdir /tmp/repository"
        " && tar --exclude=.git --exclude=target --exclude=node_modules"
        " --exclude='*.node' -C /src -cf - . | tar -C /tmp/repository -xf -"
        " && cd /tmp/repository/crates/assetstudio-node"
        " && npm ci --silent && npm run build"
        f" && test -f assetstudio-node.linux-{addon_architecture}-gnu.node"
        " && npm test && npm run test:package"
        " && mkdir /tmp/node-pack"
        " && npm pack --silent --pack-destination /tmp/node-pack >/dev/null"
        " && test \"$(find /tmp/node-pack -maxdepth 1 -name '*.tgz' | wc -l)\" -eq 1"
    )

INSTALL_WHEEL = (
    "import glob, subprocess, sys; "
    "wheels = glob.glob('dist/*.whl'); "
    "assert len(wheels) == 1, wheels; "
    "subprocess.check_call([sys.executable, '-m', 'pip', 'install', "
    "'--force-reinstall', '--no-deps', wheels[0]])"
)

INSTALL_SDIST_WHEEL = (
    "import glob, subprocess, sys; "
    "wheels = glob.glob('sdist-dist/*.whl'); "
    "assert len(wheels) == 1, wheels; "
    "subprocess.check_call([sys.executable, '-m', 'pip', 'install', "
    "'--force-reinstall', '--no-deps', wheels[0]])"
)

STAGE_CLI_ARTIFACT = (
    "import pathlib, subprocess, sys, tempfile; "
    "temporary = tempfile.TemporaryDirectory(); "
    "output = pathlib.Path(temporary.name) / 'artifact'; "
    "subprocess.check_call([sys.executable, 'tools/stage_cli_artifact.py', "
    "sys.argv[1], str(output)]); "
    "expected = {'LICENSE', 'THIRD_PARTY_NOTICES.md', "
    "'THIRD_PARTY_LICENSES.txt', pathlib.Path(sys.argv[1]).name}; "
    "actual = {path.name for path in output.iterdir()}; "
    "assert actual == expected, (actual, expected); "
    "subprocess.check_call([str(output / pathlib.Path(sys.argv[1]).name), '--help']); "
    "temporary.cleanup()"
)

PACKAGE_NODE_TARBALL = (
    "import pathlib, subprocess, tempfile; "
    "temporary = tempfile.TemporaryDirectory(); "
    "subprocess.check_call(['npm', 'pack', '--pack-destination', temporary.name]); "
    "archives = list(pathlib.Path(temporary.name).glob('*.tgz')); "
    "assert len(archives) == 1, archives; "
    "temporary.cleanup()"
)


def run(step: Step) -> tuple[bool, str]:
    environment = dict(os.environ)
    environment.update(step.env)
    result = subprocess.run(
        step.command,
        cwd=step.cwd,
        env=environment,
        capture_output=True,
        text=True,
    )
    if result.returncode == 0:
        return True, ""
    tail = (result.stderr or result.stdout).strip().splitlines()
    return False, "\n".join(tail[-25:])


def executable_version(executable: str) -> str | None:
    """Return a tool's normalized ``--version`` output, or None on failure."""
    try:
        result = subprocess.run(
            [executable, "--version"],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
    except OSError:
        return None
    if result.returncode != 0:
        return None
    return result.stdout.strip()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("groups", nargs="*", help="only run these groups")
    parser.add_argument("--list", action="store_true", help="list groups and exit")
    parser.add_argument(
        "--fail-on-skip",
        action="store_true",
        help="return failure if a requested group cannot run",
    )
    parser.add_argument(
        "--interpreter",
        default=sys.executable,
        help="Python the wheel is built against and the throwaway virtualenv is "
        "created from; the wheel itself is never installed into it",
    )
    arguments = parser.parse_args()

    available = groups(arguments.interpreter)
    if arguments.groups:
        names = set(arguments.groups)
        unknown = names - {group.name for group in available}
        if unknown:
            print(f"unknown group(s): {', '.join(sorted(unknown))}", file=sys.stderr)
            return 2
        available = [group for group in available if group.name in names]

    if arguments.list:
        for group in available:
            if group.requires or group.additional_requires:
                requirement = group.required_version_output or group.requires
                requirements = tuple(
                    value
                    for value in (requirement, *group.additional_requires)
                    if value is not None
                )
                note = f"  (needs {', '.join(requirements)})"
            elif group.probe:
                note = f"  (optional: {group.reason})"
            else:
                note = ""
            print(f"{group.name}{note}")
            for step in group.steps:
                print(f"    {' '.join(step.command)}")
        return 0

    if shutil.which("cargo") is None:
        print("cargo is required", file=sys.stderr)
        return 2

    failures: list[str] = []
    skipped: list[str] = []
    for group in available:
        if group.requires:
            executable = shutil.which(group.requires)
            if executable is None:
                skipped.append(f"{group.name}: {group.reason}")
                print(f"skip {group.name}: {group.requires} not found")
                continue
            if group.required_version_output is not None:
                actual_version = executable_version(executable)
                if actual_version != group.required_version_output:
                    actual = actual_version or "unavailable"
                    skipped.append(f"{group.name}: {group.reason}; found {actual}")
                    print(
                        f"skip {group.name}: expected {group.required_version_output}, "
                        f"found {actual}"
                    )
                    continue
        missing_additional = next(
            (
                requirement
                for requirement in group.additional_requires
                if shutil.which(requirement) is None
            ),
            None,
        )
        if missing_additional is not None:
            skipped.append(f"{group.name}: {group.reason}")
            print(f"skip {group.name}: {missing_additional} not found")
            continue
        if group.probe:
            try:
                probe = subprocess.run(
                    group.probe,
                    cwd=ROOT,
                    capture_output=True,
                    text=True,
                )
            except OSError:
                probe = None
            if probe is None or probe.returncode != 0:
                skipped.append(f"{group.name}: {group.reason}")
                print(f"skip {group.name}: {group.reason}")
                continue
        for step in group.steps:
            label = f"{group.name}/{step.name}"
            print(f"run  {label}", flush=True)
            ok, tail = run(step)
            if not ok:
                failures.append(label)
                print(f"FAIL {label}\n{tail}\n", file=sys.stderr)
                # Later steps in a group generally depend on earlier ones.
                break

    print()
    for note in skipped:
        print(f"skipped {note}")
    if failures:
        print(f"\n{len(failures)} step(s) failed: {', '.join(failures)}")
        return 1
    if skipped and arguments.fail_on_skip:
        print(f"\n{len(skipped)} group(s) skipped under --fail-on-skip")
        return 1
    print(f"all steps passed ({len(skipped)} group(s) skipped)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
