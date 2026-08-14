"""Validate that the Python source distribution is complete and source-only."""

from __future__ import annotations

import sys
import tarfile
from pathlib import Path


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
    ]
    assert not forbidden, f"sdist contains build artifacts: {forbidden}"

    required_suffixes = (
        "/crates/assetstudio-core/src/lib.rs",
        "/crates/assetstudio-core/src/fsb_vorbis_headers.bin",
        "/crates/assetstudio-python/src/lib.rs",
        "/python/assetstudio/__init__.py",
        "/python/assetstudio/__init__.pyi",
        "/python/assetstudio/py.typed",
    )
    missing = [
        suffix for suffix in required_suffixes if not any(name.endswith(suffix) for name in names)
    ]
    assert not missing, f"sdist is missing required sources: {missing}"


if __name__ == "__main__":
    main()
