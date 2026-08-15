#!/usr/bin/env python3
"""Stages one native CLI binary with the legal files it must ship beside."""

from __future__ import annotations

import shutil
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
LEGAL_FILES = ("LICENSE", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES.txt")


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: stage_cli_artifact.py <binary> <new-output-directory>", file=sys.stderr)
        return 2
    binary = Path(sys.argv[1]).resolve()
    output = Path(sys.argv[2]).resolve()
    if not binary.is_file() or binary.is_symlink():
        print(f"{binary}: not a regular CLI binary", file=sys.stderr)
        return 1
    if output.exists():
        print(f"{output}: output already exists; refusing stale artifact contents", file=sys.stderr)
        return 1

    legal_sources = []
    for name in LEGAL_FILES:
        source = ROOT / name
        if not source.is_file() or source.is_symlink():
            print(f"{source}: required legal file is missing or not regular", file=sys.stderr)
            return 1
        legal_sources.append((source, name))

    output.mkdir(parents=True)
    shutil.copy2(binary, output / binary.name)
    for source, name in legal_sources:
        shutil.copy2(source, output / name)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
