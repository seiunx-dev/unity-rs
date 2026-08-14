#!/usr/bin/env python3
"""Runs every step the CI workflow runs, on this machine.

CI has not run since the LZMA commit, and the gap was not harmless: the Python
suite had been failing the whole time and nothing surfaced it until the wheel
was built by hand. This exists so that "run what CI runs" is one command on any
platform rather than a sequence someone has to reconstruct from the workflow
file.

It is not a replacement for CI. CI runs this matrix on Linux, Windows and
macOS; this runs it wherever you are. The value is that a contributor on a
platform the maintainer cannot reach can produce the same evidence.

Two groups reach past this machine. `cross` compiles the workspace and its
tests for another target without running them, which catches what fails to
build elsewhere -- a path assumption, a missing `cfg`, a type that is only
`Send` on one platform -- but says nothing about behaviour; it needs a cross C
toolchain because zstd builds from C sources. `linux` goes further and runs the
suite and the Python wheel inside a container on the toolchain CI pins, which
is behaviour rather than compilation. Neither covers Windows or macOS at once,
so CI still has work to do.

The `outputs` group is a second implementation rather than a second run: the
crate's binary FBX reader and writer were built together, so their agreement
shows they share assumptions, not that either matches the format. That group
exports through the CLI and checks the bytes with a parser written from the
format's rules, and does the same for the ASCII form: braces, the counts
`Definitions` declares against the objects actually written, connections
resolving to objects that exist, and arrays holding as many values as they
claim. The WAV containers go to Python's own `wave` module, which was
not written for this project and refuses files whose header fields disagree.

Steps are grouped, and a group that cannot run because a tool is missing is
reported as skipped rather than failed -- the .NET oracle, `vgmstream-cli` and
UnityPy are all optional. Anything that runs and fails is a failure.

    python3 tools/local_ci.py             # everything available
    python3 tools/local_ci.py --list      # what would run
    python3 tools/local_ci.py rust python # only these groups
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
    reason: str = ""


def groups(interpreter: str) -> list[Group]:
    return [
        Group(
            "quality",
            [
                Step("format", ["cargo", "fmt", "--all", "--", "--check"]),
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
                    ["cargo", "package", "--locked", "-p", "assetstudio-core"],
                ),
            ],
        ),
        Group(
            "rust",
            [Step("workspace tests", ["cargo", "test", "--workspace", "--locked"])],
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
                )
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
                )
            ],
            requires="x86_64-unknown-linux-gnu-gcc",
            reason="cross-compiling needs a Linux C toolchain for zstd's C sources",
        ),
        Group(
            "linux",
            [
                # Both architectures CI builds for. Each gets its own target
                # directory so the two do not fight over one cache.
                *(
                    Step(
                        f"workspace tests on Linux {architecture}",
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
                Step(
                    "Python wheel on Linux",
                    [
                        "docker", "run", "--rm", "--platform", "linux/amd64",
                        "-v", f"{ROOT}:/src", "-w", "/src/crates/assetstudio-python",
                        "-e", "CARGO_TARGET_DIR=/tmp/target",
                        LINUX_IMAGE,
                        "sh", "-c", LINUX_WHEEL,
                    ],
                ),
                Step(
                    "Node addon on Linux",
                    [
                        "docker", "run", "--rm", "--platform", "linux/amd64",
                        "-v", f"{ROOT}:/src", "-w", "/src/crates/assetstudio-node",
                        "-e", "CARGO_TARGET_DIR=/tmp/target",
                        LINUX_IMAGE,
                        "sh", "-c", LINUX_NODE,
                    ],
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
                Step("package contents", ["npm", "pack", "--dry-run"], cwd=NODE),
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
                        "--interpreter", interpreter, "--out", "dist",
                    ],
                    cwd=PYTHON,
                ),
                Step("install wheel", [interpreter, "-c", INSTALL_WHEEL], cwd=PYTHON),
                Step("wheel surface", [interpreter, "-I", "tests/installed_wheel.py"], cwd=PYTHON),
                Step("python api", [interpreter, "-I", "tests/python_api.py"], cwd=PYTHON),
            ],
            requires="maturin",
            reason="the Python wheel needs maturin",
        ),
        Group(
            "unitypy",
            [
                Step(
                    "UnityPy differential",
                    [interpreter, "-I", "tests/unitypy_oracle.py"],
                    cwd=PYTHON,
                )
            ],
            requires="maturin",
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
    f"{LINUX_SETUP} && cargo test -p assetstudio-core -p assetstudio-cli --locked"
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
LINUX_NODE = (
    f"{LINUX_SETUP} && apt-get install -y -qq curl xz-utils >/dev/null"
    f" && curl -fsSL https://nodejs.org/dist/v{LINUX_NODE_VERSION}"
    f"/node-v{LINUX_NODE_VERSION}-linux-x64.tar.xz -o /tmp/node.tar.xz"
    " && tar -xJf /tmp/node.tar.xz -C /tmp"
    f" && export PATH=/tmp/node-v{LINUX_NODE_VERSION}-linux-x64/bin:$PATH"
    " && npm ci --silent && npm run build:debug && npm test"
    # The addon lands in the mounted tree; leaving a foreign binary behind
    # would confuse the next host build.
    " && rm -f assetstudio-node.linux-x64-gnu.node"
)

INSTALL_WHEEL = (
    "import glob, subprocess, sys; "
    "wheels = glob.glob('dist/*.whl'); "
    "assert len(wheels) == 1, wheels; "
    "subprocess.check_call([sys.executable, '-m', 'pip', 'install', "
    "'--force-reinstall', '--no-deps', wheels[0]])"
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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("groups", nargs="*", help="only run these groups")
    parser.add_argument("--list", action="store_true", help="list groups and exit")
    parser.add_argument(
        "--interpreter",
        default=sys.executable,
        help="Python used to build and test the wheel; use a virtualenv so the "
        "wheel is not installed into a system interpreter",
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
            note = f"  (needs {group.requires})" if group.requires else ""
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
        if group.requires and shutil.which(group.requires) is None:
            skipped.append(f"{group.name}: {group.reason}")
            print(f"skip {group.name}: {group.requires} not found")
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
    print(f"all steps passed ({len(skipped)} group(s) skipped)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
