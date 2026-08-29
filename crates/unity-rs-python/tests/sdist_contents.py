"""Validate that the Python source distribution is complete and source-only."""

from __future__ import annotations

import sys
import tarfile
from pathlib import Path


FORBIDDEN_COMPONENTS = {"unity-rs-ffi", "unity-rs-gui", "unity-rsgui"}
FORBIDDEN_SUFFIXES = (".cs", ".csproj", ".fsproj", ".sln", ".slnx", ".vbproj")

# Every check here is an `assert`, and `-O` or `PYTHONOPTIMIZE` deletes those
# outright rather than skipping them: an sdist carrying none of the required
# sources and none of the three legal files exits zero under `PYTHONOPTIMIZE=1`.
# Refuse to run instead of reporting a success that checked nothing.
if not __debug__:
    raise SystemExit(
        "refusing to run with assertions disabled (-O / PYTHONOPTIMIZE): "
        "every check in this gate is an assert"
    )


def forbidden_delivery_path(name: str) -> bool:
    lowered = name.casefold().replace("\\", "/")
    return any(part in FORBIDDEN_COMPONENTS for part in lowered.split("/")) or lowered.endswith(
        FORBIDDEN_SUFFIXES
    )


def main() -> None:
    distribution_directory = Path(sys.argv[1]) if len(sys.argv) == 2 else Path("dist")
    archives = sorted(distribution_directory.glob("*.tar.gz"))
    assert len(archives) == 1, archives
    with tarfile.open(archives[0], "r:gz") as archive:
        names = archive.getnames()

    native_binary_suffixes = (".dll", ".dylib", ".exe", ".lib", ".pyd", ".so")
    forbidden = [
        name
        for name in names
        if name.endswith((".whl", *native_binary_suffixes))
        or "/dist/" in f"/{name}/"
        or forbidden_delivery_path(name)
    ]
    assert not forbidden, f"sdist contains build artifacts or out-of-scope files: {forbidden}"

    required_suffixes = (
        "/crates/unity-rs-core/LICENSE",
        "/crates/unity-rs-core/THIRD_PARTY_NOTICES.md",
        "/crates/unity-rs-core/THIRD_PARTY_LICENSES.txt",
        "/crates/unity-rs-core/src/lib.rs",
        "/crates/unity-rs-core/src/fsb_vorbis_headers.bin",
        "/crates/unity-rs-python/src/lib.rs",
        "/python/unity_rs/__init__.py",
        "/python/unity_rs/__init__.pyi",
        "/python/unity_rs/compat/__init__.py",
        "/python/unity_rs/compat/unitypy.py",
        "/python/unity_rs/LICENSE",
        "/python/unity_rs/THIRD_PARTY_NOTICES.md",
        "/python/unity_rs/THIRD_PARTY_LICENSES.txt",
        "/python/unity_rs/py.typed",
    )
    missing = [
        suffix for suffix in required_suffixes if not any(name.endswith(suffix) for name in names)
    ]
    assert not missing, f"sdist is missing required sources: {missing}"


if __name__ == "__main__":
    main()
