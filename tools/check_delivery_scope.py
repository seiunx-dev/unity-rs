#!/usr/bin/env python3
"""Proves that the shipped workspace has only the requested headless surfaces."""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
EXPECTED_PRIMARY_TARGET = {
    "unity-rs-cli": ("unity-rs", ("bin",), ("bin",)),
    "unity-rs-core": ("unity_rs_core", ("lib",), ("lib",)),
    "unity-rs-node": ("unity_rs_node", ("cdylib",), ("cdylib",)),
    "unity-rs-python": ("_native", ("cdylib",), ("cdylib",)),
}
NON_DELIVERY_TARGET_KINDS = {"bench", "custom-build", "example", "test"}
BINDINGS = ("unity-rs-cli", "unity-rs-node", "unity-rs-python")
FORBIDDEN_PACKAGE_NAMES = {
    "unity-rs-ffi",
    "unity-rs-gui",
    "unity-rsgui",
    "haruki-unity-rs-ffi",
}
FORBIDDEN_SOURCE_FILES = (
    Path("crates/unity-rs-ffi/Cargo.toml"),
    Path("crates/unity-rs-ffi/src/lib.rs"),
)
FORBIDDEN_REPOSITORY_PATHS = (Path("crates/unity-rs-ffi"),)
DELIVERY_CONFIGURATION_FILES = (Path("Cargo.toml"), Path(".gitignore"))
PUBLIC_API_FILES = (
    Path("crates/unity-rs-core/src/lib.rs"),
    Path("crates/unity-rs-core/src/studio.rs"),
    Path("crates/unity-rs-python/python/unity_rs/__init__.pyi"),
    Path("crates/unity-rs-node/index.d.ts"),
)
FORBIDDEN_PUBLIC_API_PATTERN = re.compile(
    r"\b(?:UnityRs|Studio)?Context\b|\bcontext_id\b|\bcontextId\b|"
    r"\bContextOpen\b|\bContextClose\b|\bfor_each_os_str_char_lossy\b|"
    r"\blossy_os_str_utf8_length\b"
)
FORBIDDEN_PUBLIC_RUST_DECLARATION_PATTERN = re.compile(
    r"(?m)^\s*pub\s+(?:(?:async|const|unsafe)\s+|extern\s+\"[^\"]+\"\s+)*"
    r"(?:fn|struct|enum|union|trait|type|mod|use|const|static)\b[^\n]*"
    r"(?:\bContext\b|\b[A-Za-z0-9_]*Context\b|\bcontext_id\b|\bcontextId\b)"
)
FORBIDDEN_CUSTOM_C_ABI_PATTERN = re.compile(
    r"#\s*\[\s*(?:unsafe\s*\(\s*)?(?:no_mangle|export_name)\b|"
    r"^\s*pub\s+(?:unsafe\s+)?extern\s+"
    r"\"(?:C|cdecl|stdcall|system|win64)\"\s+fn\b",
    re.MULTILINE,
)

# Every check here is an `assert`, and `-O` or `PYTHONOPTIMIZE` deletes those
# outright rather than skipping them: a workspace that had grown a GUI crate
# would print the reassuring line below and exit zero. Refuse to run instead of
# reporting a success that checked nothing.
if not __debug__:
    raise SystemExit(
        "refusing to run with assertions disabled (-O / PYTHONOPTIMIZE): "
        "every check in this gate is an assert"
    )


def check_retired_surfaces(root: Path = ROOT) -> None:
    for relative in FORBIDDEN_REPOSITORY_PATHS:
        retired_path = root / relative
        assert not retired_path.exists(), (
            "the retired custom C ABI directory must be absent, including ignored caches",
            relative,
        )
    for relative in FORBIDDEN_SOURCE_FILES:
        source = root / relative
        assert not source.exists(), (
            "the retired custom C ABI/context source must not be kept in the delivery repository",
            relative,
        )
    for relative in DELIVERY_CONFIGURATION_FILES:
        configuration = (root / relative).read_text(encoding="utf-8")
        assert "unity-rs-ffi" not in configuration.casefold(), (
            "the retired custom C ABI crate must not remain as a workspace or ignore rule",
            relative,
        )
    for relative in PUBLIC_API_FILES:
        public_api = root / relative
        source = public_api.read_text(encoding="utf-8")
        match = FORBIDDEN_PUBLIC_API_PATTERN.search(source)
        assert match is None, (
            "public Rust/Python/Node APIs must expose owned Studio values, "
            "not context handles or binding-internal helpers",
            relative,
            match.group(0) if match else None,
        )
    rust_source_roots = (
        root / "crates/unity-rs-core/src",
        root / "crates/unity-rs-python/src",
        root / "crates/unity-rs-node/src",
    )
    for source_root in rust_source_roots:
        for rust_source in source_root.rglob("*.rs"):
            source = rust_source.read_text(encoding="utf-8")
            match = FORBIDDEN_PUBLIC_RUST_DECLARATION_PATTERN.search(source)
            assert match is None, (
                "public Rust declarations must not expose context handles",
                rust_source.relative_to(root),
                match.group(0) if match else None,
            )
    first_party_rust_roots = (*rust_source_roots, root / "crates/unity-rs-cli/src")
    for source_root in first_party_rust_roots:
        for rust_source in source_root.rglob("*.rs"):
            source = rust_source.read_text(encoding="utf-8")
            match = FORBIDDEN_CUSTOM_C_ABI_PATTERN.search(source)
            assert match is None, (
                "first-party Rust code must not reintroduce a custom exported C ABI",
                rust_source.relative_to(root),
                match.group(0) if match else None,
            )


def check_package_targets(name: str, package: dict[str, Any]) -> None:
    delivery_targets: list[tuple[str, tuple[str, ...], tuple[str, ...]]] = []
    for target in package["targets"]:
        kinds = tuple(target["kind"])
        if kinds and all(kind in NON_DELIVERY_TARGET_KINDS for kind in kinds):
            continue
        delivery_targets.append(
            (target["name"], kinds, tuple(target["crate_types"]))
        )
    assert delivery_targets == [EXPECTED_PRIMARY_TARGET[name]], (
        "delivery package gained an unexpected production target",
        name,
        delivery_targets,
    )


def main() -> int:
    check_retired_surfaces()

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
    assert set(packages) == set(EXPECTED_PRIMARY_TARGET), (
        "delivery workspace changed; it must remain Core + CLI + Python + optional Node",
        sorted(packages),
    )

    for name, package in packages.items():
        lowered = name.casefold()
        assert lowered not in FORBIDDEN_PACKAGE_NAMES, name
        check_package_targets(name, package)
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
            assert "unity-rs-core" in normal_dependencies, (name, normal_dependencies)
        else:
            assert not (normal_dependencies & set(BINDINGS)), normal_dependencies

    print(
        "delivery scope: Core + CLI + Python + optional Node, "
        "no GUI, custom C ABI source, or public context handles"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
