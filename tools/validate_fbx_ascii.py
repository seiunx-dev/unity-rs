#!/usr/bin/env python3
"""Checks an ASCII FBX 7.4 file for internal consistency, independently.

Written from the format rather than from the crate's writer, for the same
reason as the binary validator: a document that only its author checks is a
document nobody has checked. The managed exporter goes through the FBX SDK, so
there is no byte-level oracle here; what can be verified without one is that
the file does not contradict itself.

Four things, each of which an importer relies on and none of which the writer's
own tests would notice going wrong:

* braces balance, and the sections a 7.4 file must carry are present;
* every `ObjectType: "X" { Count: N }` in `Definitions` matches the number of
  `X:` objects actually written;
* every id referenced by a `C:` connection exists as an object, apart from the
  root's 0;
* every `*N { a: ... }` array holds exactly N values.

    python3 tools/validate_fbx_ascii.py model.fbx
    python3 tools/validate_fbx_ascii.py --cli
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REQUIRED_SECTIONS = ("FBXHeaderExtension", "GlobalSettings", "Definitions", "Objects", "Connections")

OBJECT_TYPE = re.compile(r'^\s*ObjectType:\s*"([^"]+)"\s*\{\s*Count:\s*(\d+)\s*\}')
OBJECT_ENTRY = re.compile(r"^\s*([A-Za-z]+):\s*(-?\d+),\s*\"")
CONNECTION = re.compile(r'^\s*C:\s*"(\w+)",\s*(-?\d+),\s*(-?\d+)')
ARRAY_HEADER = re.compile(r"^\s*([A-Za-z]+):\s*\*(\d+)\s*\{")


class Invalid(Exception):
    pass


def array_values(line: str) -> str | None:
    """Return an FBX array payload without an ambiguous whitespace regex."""
    stripped = line.lstrip()
    if not stripped.startswith("a:"):
        return None
    return stripped.removeprefix("a:").lstrip()


def validate(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    validate_braces(lines)
    validate_sections(lines)
    declared, written, ids, connections = scan_records(lines)
    validate_declared_counts(declared, written)
    validate_connections(connections, ids)
    return [
        f"{sum(written.values())} object(s) across {len(written)} type(s)",
        f"{len(connections)} connection(s), all resolving",
    ]


def validate_braces(lines: list[str]) -> None:
    depth = 0
    for number, line in enumerate(lines, 1):
        depth += line.count("{") - line.count("}")
        if depth < 0:
            raise Invalid(f"line {number} closes a brace that was never opened")
    if depth != 0:
        raise Invalid(f"{depth} brace(s) left open at end of file")


def validate_sections(lines: list[str]) -> None:
    for section in REQUIRED_SECTIONS:
        if not any(line.startswith(f"{section}:") for line in lines):
            raise Invalid(f"the file has no {section} section")


def scan_records(
    lines: list[str],
) -> tuple[dict[str, int], dict[str, int], set[int], list[tuple[int, int]]]:
    declared: dict[str, int] = {}
    written: dict[str, int] = {}
    ids: set[int] = {0}
    connections: list[tuple[int, int]] = []
    in_objects = False

    index = 0
    while index < len(lines):
        line = lines[index]
        in_objects = update_object_section(line, in_objects)
        record_declared_type(line, declared)
        record_object(line, in_objects, written, ids)
        record_connection(line, connections)
        validate_array_at(lines, index, line)
        index += 1
    return declared, written, ids, connections


def update_object_section(line: str, in_objects: bool) -> bool:
    if line.startswith("Objects:"):
        return True
    if line.startswith("Connections:"):
        return False
    return in_objects


def record_declared_type(line: str, declared: dict[str, int]) -> None:
    match = OBJECT_TYPE.match(line)
    if match:
        declared[match.group(1)] = int(match.group(2))


def record_object(
    line: str, in_objects: bool, written: dict[str, int], ids: set[int]
) -> None:
    if not in_objects:
        return
    match = OBJECT_ENTRY.match(line)
    if not match:
        return
    kind, identifier = match.group(1), int(match.group(2))
    written[kind] = written.get(kind, 0) + 1
    if identifier in ids:
        raise Invalid(f"object id {identifier} is used more than once")
    ids.add(identifier)


def record_connection(line: str, connections: list[tuple[int, int]]) -> None:
    match = CONNECTION.match(line)
    if match:
        connections.append((int(match.group(2)), int(match.group(3))))


def validate_array_at(lines: list[str], index: int, line: str) -> None:
    match = ARRAY_HEADER.match(line)
    if match:
        validate_array(lines, index, match)


def validate_array(lines: list[str], index: int, match: re.Match[str]) -> None:
    name, count = match.group(1), int(match.group(2))
    values_line = lines[index + 1] if index + 1 < len(lines) else ""
    values = array_values(values_line)
    if count == 0:
        # An empty array may omit its `a:` line entirely.
        if values is not None and values.strip():
            raise Invalid(f"{name} declares *0 but carries values")
        return
    if values is None:
        raise Invalid(f"{name} declares *{count} but has no values line")
    present = len([value for value in values.split(",") if value.strip()])
    if present != count:
        raise Invalid(f"{name} declares *{count} but holds {present} values")


def validate_declared_counts(declared: dict[str, int], written: dict[str, int]) -> None:
    for kind, count in declared.items():
        actual = written.get(kind, 0)
        if actual != count:
            raise Invalid(
                f'Definitions says Count {count} for "{kind}" but Objects holds {actual}'
            )


def validate_connections(connections: list[tuple[int, int]], ids: set[int]) -> None:
    for child, parent in connections:
        for identifier, role in ((child, "child"), (parent, "parent")):
            if identifier not in ids:
                raise Invalid(f"connection {role} id {identifier} is not an object in this file")


def export_and_validate() -> int:
    import subprocess
    import tempfile

    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from validate_fbx_binary import synthetic_model  # noqa: PLC0415

    root = Path(__file__).resolve().parent.parent
    with tempfile.TemporaryDirectory(prefix="unity-rs-fbx-ascii-") as directory:
        assets = Path(directory) / "model.assets"
        assets.write_bytes(synthetic_model())
        output = Path(directory) / "model.fbx"
        result = subprocess.run(
            ["cargo", "run", "--quiet", "-p", "unity-rs-cli", "--locked", "--",
             "fbx", str(assets), str(output)],
            cwd=root, capture_output=True, text=True,
        )
        if result.returncode != 0:
            print(result.stderr.strip(), file=sys.stderr)
            return 1
        try:
            for note in validate(output):
                print(f"  {note}")
        except Invalid as error:
            print(f"exported FBX is inconsistent: {error}", file=sys.stderr)
            return 1
    print("exported ASCII FBX is internally consistent")
    return 0


def main() -> int:
    if len(sys.argv) == 2 and sys.argv[1] == "--cli":
        return export_and_validate()
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    path = Path(sys.argv[1])
    try:
        for note in validate(path):
            print(f"  {note}")
    except Invalid as error:
        print(f"{path}: {error}", file=sys.stderr)
        return 1
    print(f"{path}: internally consistent ASCII FBX 7.4")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
