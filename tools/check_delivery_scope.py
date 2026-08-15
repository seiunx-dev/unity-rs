#!/usr/bin/env python3
"""Proves that the shipped workspace has only the requested headless surfaces."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
EXPECTED_TARGET = {
    "assetstudio-cli": "bin",
    "assetstudio-core": "lib",
    "assetstudio-node": "cdylib",
    "assetstudio-python": "cdylib",
}
BINDINGS = ("assetstudio-cli", "assetstudio-node", "assetstudio-python")
FORBIDDEN_PACKAGE_NAMES = {
    "assetstudio-ffi",
    "assetstudio-gui",
    "assetstudiogui",
    "haruki-assetstudio-ffi",
}


def main() -> int:
    metadata: dict[str, Any] = json.loads(
        subprocess.check_output(
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--locked",
            ],
            cwd=ROOT,
            text=True,
        )
    )
    workspace_ids = set(metadata["workspace_members"])
    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package["id"] in workspace_ids
    }
    assert set(packages) == set(EXPECTED_TARGET), (
        "delivery workspace changed; it must remain Core + CLI + Python + optional Node",
        sorted(packages),
    )

    for name, package in packages.items():
        lowered = name.casefold()
        assert lowered not in FORBIDDEN_PACKAGE_NAMES, name
        target_kinds = {
            kind
            for target in package["targets"]
            for kind in target["kind"]
        }
        assert EXPECTED_TARGET[name] in target_kinds, (name, sorted(target_kinds))
        manifest = Path(package["manifest_path"]).resolve()
        assert manifest.is_relative_to(ROOT), (name, manifest)

        normal_dependencies = {
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency["kind"] is None
        }
        forbidden = sorted(
            dependency
            for dependency in normal_dependencies
            if dependency.casefold() in FORBIDDEN_PACKAGE_NAMES
        )
        assert not forbidden, (name, forbidden)
        if name in BINDINGS:
            assert "assetstudio-core" in normal_dependencies, (name, normal_dependencies)
        else:
            assert not (normal_dependencies & set(BINDINGS)), normal_dependencies

    print("delivery scope: Core + CLI + Python + optional Node, no GUI or custom C ABI")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
