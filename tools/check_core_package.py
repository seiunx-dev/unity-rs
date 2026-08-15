#!/usr/bin/env python3
"""Checks that the publishable Core crate carries current legal documents."""

from __future__ import annotations

import tarfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
LEGAL_FILES = ("LICENSE", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES.txt")
FORBIDDEN_COMPONENTS = {"assetstudio-ffi", "assetstudio-gui", "assetstudiogui"}
FORBIDDEN_SUFFIXES = (".cs", ".csproj", ".fsproj", ".sln", ".slnx", ".vbproj")

# Every check here is an `assert`, and `-O` or `PYTHONOPTIMIZE` deletes those
# outright rather than skipping them: a crate carrying a stale NOTICE, or none
# at all, would print the reassuring line below and exit zero. That is the
# defect this gate was written to catch, so refuse to run instead.
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


def main() -> int:
    archives = sorted((ROOT / "target" / "package").glob("assetstudio-core-*.crate"))
    assert len(archives) == 1, archives
    with tarfile.open(archives[0], "r:gz") as archive:
        forbidden = [member.name for member in archive.getmembers() if forbidden_delivery_path(member.name)]
        assert not forbidden, f"Core crate contains out-of-scope GUI/C ABI/.NET files: {forbidden}"
        members = {Path(member.name).name: member for member in archive.getmembers()}
        for name in LEGAL_FILES:
            member = members.get(name)
            assert member is not None and member.isfile(), f"Core crate is missing {name}"
            packaged = archive.extractfile(member)
            assert packaged is not None
            assert packaged.read() == (ROOT / name).read_bytes(), f"stale Core {name}"
    print("Core crate: license and notices ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
