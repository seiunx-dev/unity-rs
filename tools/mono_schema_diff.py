#!/usr/bin/env python3
"""Checks a generated MonoBehaviour schema against Unity's own type trees.

A schema document reconstructed from a game's assemblies is the only way to
read a `MonoBehaviour` out of a release build, and it is also unverifiable on
its own: the reader has nothing to disagree with, so a wrong layout produces
confident nonsense. The check here is to run it against a build that *does*
still carry type trees. Unity wrote those; the schema came from Cecil walking a
DLL. Reading the same object both ways and comparing is a real differential.

    python3 tools/mono_schema_diff.py <bundle-directory> <schema.json> [limit] [unity-version]

Two properties are checked, and they are deliberately different strengths:

* the **values** must match exactly, in order. This is the real claim: the
  schema describes the same bytes in the same layout Unity did. A missing
  four-byte field shifts everything after it, so this bites hard;
* the **field names** may differ, and are reported rather than failed. A
  reconstructed tree names fields as the C# source does, and Unity does not
  always agree -- `UnityEngine.Rect` serializes as `x, y, width, height` while
  its fields are `m_XMin, m_YMin, m_Width, m_Height`. That is cosmetic, and
  making it an error would only invite papering over the value check.

An object read through the embedded tree in both runs proves nothing, so those
are excluded and the run fails if nothing at all went through a schema.

Bundles are processed in batches, each batch exported twice and deleted before
the next, and each export asks for class 114 alone, because a full corpus does
not fit on disk twice. A batch is staged
with hard links rather than symlinks: the loader ignores a symlink on purpose,
so a symlinked batch loads as nothing at all and every count comes out zero.

The batch always includes every bundle whose name mentions monoscripts: a
`MonoBehaviour` names its script through a cross-file reference, and without
the file holding it nothing resolves and every object is declined.

Needs a corpus of real bundles and a schema document, which is why this is a
tool rather than a test. `tools/monoschema` writes the document.
"""

from __future__ import annotations

import collections
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BATCH = 24


def export(
    inputs: Path, output: Path, schema: Path | None, unity_version: str | None
) -> tuple[dict[Path, str], str]:
    """Exports every object as type-tree JSON, returning path -> payload kind."""
    command = ["cargo", "run", "--release", "--quiet", "-p", "unity-rs-cli", "--locked", "--"]
    if unity_version:
        command += ["--unity-version", unity_version]
    if schema:
        command += ["--mono-schema", str(schema), "--mono-schema-override"]
    # Only MonoBehaviour: the cost of an export is whatever is largest in it,
    # and dumping one game's Live2D bundles in full writes gigabytes of mesh
    # per batch for a check that never looks at any of it.
    command += ["export", "--mode", "typetree-json", "--class", "114",
                "--filename", "path-id", str(inputs), str(output)]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    kinds: dict[Path, str] = {}
    for line in result.stdout.splitlines():
        if not line.startswith("exported "):
            continue
        head, _, target = line.rpartition(" -> ")
        _, _, tail = head.rpartition("(class ")
        class_id, _, kind = tail.partition(", ")
        if class_id == "114":
            # resolve() on both sides: on macOS the temporary directory the CLI
            # prints back is /private/var/... while tempfile handed out /var/...
            kinds[Path(target).resolve().relative_to(output.resolve())] = kind.rstrip(")")
    return kinds, result.stdout


def stage(source: Path, target: Path) -> None:
    """Puts a bundle where the batch can see it, without copying if possible."""
    try:
        os.link(source, target)
    except OSError:
        # A different filesystem, or a corpus mounted read-only.
        shutil.copy2(source, target)


def flatten(value: object) -> list[object]:
    """Every leaf in reading order, with field names discarded."""
    if isinstance(value, dict):
        return [leaf for item in value.values() for leaf in flatten(item)]
    if isinstance(value, list):
        return [leaf for item in value for leaf in flatten(item)]
    return [value]


def main() -> int:
    if len(sys.argv) not in (3, 4, 5):
        print(__doc__)
        return 2
    directory = Path(sys.argv[1])
    schema = Path(sys.argv[2])
    limit = int(sys.argv[3]) if len(sys.argv) >= 4 else 0
    unity_version = sys.argv[4] if len(sys.argv) == 5 else None

    everything = sorted(p for p in directory.iterdir() if p.is_file())
    # Present in every batch: the file a MonoScript reference points at.
    always = [p for p in everything if "monoscript" in p.name.lower()]
    rest = [p for p in everything if p not in always]
    if limit:
        rest = rest[:limit]
    if not always:
        print(f"{directory}: no monoscripts bundle, so no m_Script will resolve", file=sys.stderr)
        return 2

    totals = collections.Counter()
    problems: list[str] = []

    with tempfile.TemporaryDirectory(prefix="unity-rs-schemadiff-") as work:
        root = Path(work).resolve()
        for start in range(0, len(rest), BATCH):
            batch = rest[start : start + BATCH]
            staged = root / "input"
            staged.mkdir()
            for source in always + batch:
                stage(source, staged / source.name)

            plain, _ = export(staged, root / "plain", None, unity_version)
            with_schema, _ = export(staged, root / "schema", schema, unity_version)

            for relative, kind in with_schema.items():
                if kind != "typetree_json_schema":
                    totals["read through the file's own tree"] += 1
                    continue
                embedded = root / "plain" / relative
                if not embedded.exists():
                    # The file carried no tree either, so there is nothing to
                    # compare against -- the schema is the only reading.
                    totals["no tree in the file to compare against"] += 1
                    continue
                mine = (root / "schema" / relative).read_text()
                theirs = embedded.read_text()
                totals["compared"] += 1
                if mine == theirs:
                    totals["identical"] += 1
                elif flatten(json.loads(mine)) == flatten(json.loads(theirs)):
                    totals["same values, different field names"] += 1
                else:
                    totals["different values"] += 1
                    if len(problems) < 20:
                        problems.append(str(relative))

            for name in ("input", "plain", "schema"):
                subprocess.run(["rm", "-rf", str(root / name)], check=True)
            print(
                f"  {min(start + BATCH, len(rest))}/{len(rest)} bundles, "
                f"{totals['compared']} compared",
                file=sys.stderr,
            )

    for label, count in sorted(totals.items()):
        print(f"{count:8}  {label}")
    if problems:
        print(f"\n{totals['different values']} object(s) read differently:", file=sys.stderr)
        for line in problems:
            print(f"  {line}", file=sys.stderr)
        return 1
    if totals["compared"] == 0:
        print("nothing was read through a schema, so nothing was checked", file=sys.stderr)
        return 1
    print("every object read through a schema holds the values Unity's own tree gives")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
