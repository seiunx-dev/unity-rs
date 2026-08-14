"""Differential gate against UnityPy, a second independent implementation.

The managed .NET oracle is this project's primary compatibility reference, but
it is one implementation and it needs a `dotnet` toolchain. UnityPy is a third
reader that installs from PyPI and runs in the same interpreter as this
package's wheel, so it can compare the same inputs with no extra toolchain.

What this proves and what it does not:

* Strong: the serialized-file layer. Object order, path IDs, class IDs, byte
  sizes, resolved names and raw object bytes come from UnityPy's own parser.
* Weak: anything that bottoms out in a shared dependency. UnityPy decodes
  pixels through the same upstream `texture2ddecoder` this workspace links, and
  its mesh and shader helpers are transliterations of AssetStudio, so agreement
  there is not independent evidence. Those rows are deliberately absent.

Run with the wheel and UnityPy installed in the same interpreter:

    python -I tests/unitypy_oracle.py
"""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import python_api as fixtures  # noqa: E402
import UnityPy  # noqa: E402

from assetstudio import AssetStudio  # noqa: E402

FNV64_OFFSET = 0xCBF29CE484222325
FNV64_PRIME = 0x00000100000001B3

# UnityPy's ImportHelper classifies any input shorter than this as a plain
# resource file rather than sniffing it, so a fixture below it is never parsed.
UNITYPY_MINIMUM_SNIFF_BYTES = 128


def fnv1a64(data: bytes) -> str:
    digest = FNV64_OFFSET
    for byte in data:
        digest = ((digest ^ byte) * FNV64_PRIME) & 0xFFFFFFFFFFFFFFFF
    return f"{digest:016x}"


def bytes_manifest(data: bytes) -> dict[str, Any]:
    return {"Size": len(data), "Fnv64": fnv1a64(data)}


def portable_file_name(path: str) -> str:
    """Mirrors the Rust helper: the entry name, without container or directory."""
    tail = path.rsplit("::", 1)[-1]
    for separator in ("/", "\\"):
        tail = tail.rsplit(separator, 1)[-1]
    return tail


def unitypy_manifest(path: Path) -> dict[str, Any]:
    environment = UnityPy.load(str(path))
    files = []
    for name, serialized in environment.files.items():
        objects = []
        # UnityPy keys objects by path ID in a dict, so a file with duplicate
        # path IDs would silently collapse. The Rust reader rejects those
        # outright, so fixtures never contain them and insertion order is file
        # order.
        for object_reader in serialized.objects.values():
            raw = object_reader.get_raw_data()
            objects.append(
                {
                    "PathId": object_reader.path_id,
                    "ClassId": int(object_reader.class_id),
                    "ByteSize": object_reader.byte_size,
                    "Name": peek_name(object_reader),
                    "Raw": bytes_manifest(bytes(raw)),
                }
            )
        files.append(
            {
                "Path": portable_file_name(name),
                "UnityVersion": str(serialized.unity_version),
                "Objects": objects,
            }
        )
    return {"Files": files}


def peek_name(object_reader: Any) -> str | None:
    """Returns the object's name, or None when UnityPy cannot determine one.

    UnityPy resolves names through its bundled TypeTree database, so it raises
    for a class/version pair the database does not cover. That is not the same
    as an object having no name, and collapsing the two would let a real
    disagreement pass as an agreed empty string. Callers skip the name
    comparison for those objects and the run reports how many were skipped.
    """
    try:
        name = object_reader.peek_name()
    except Exception:
        return None
    return name or ""


def wheel_manifest(path: Path) -> dict[str, Any]:
    studio = AssetStudio(path)
    files = []
    for info in studio.files():
        objects = []
        for obj in studio.objects():
            if obj.file_index != info.index:
                continue
            raw = studio.read_raw(obj.file_index, obj.path_id)
            objects.append(
                {
                    "PathId": obj.path_id,
                    "ClassId": obj.class_id,
                    "ByteSize": obj.byte_size,
                    "Name": obj.name or "",
                    "Raw": bytes_manifest(bytes(raw)),
                }
            )
        files.append(
            {
                "Path": portable_file_name(info.path),
                "UnityVersion": info.unity_version,
                "Objects": objects,
            }
        )
    return {"Files": files}


def cases() -> list[tuple[str, bytes]]:
    """Fixtures both readers should agree on.

    Deliberately limited to the serialized-file layer: these exercise object
    tables, version gates, names and raw payload extraction. Decoded-pixel and
    mesh rows are excluded because UnityPy shares this project's decoder or
    transliterates its oracle there, so agreement would prove nothing.
    """
    return [
        ("text-asset.assets", fixtures.synthetic_text_asset()),
        ("texture2d.assets", fixtures.synthetic_texture2d()),
        ("texture2d-array.assets", fixtures.synthetic_texture2d_array()),
        ("mesh.assets", fixtures.synthetic_mesh()),
        ("material.assets", fixtures.synthetic_material()),
        ("game-object.assets", fixtures.synthetic_game_object()),
        ("build-settings.assets", fixtures.synthetic_build_settings()),
        ("player-settings.assets", fixtures.synthetic_player_settings()),
        ("font.assets", fixtures.synthetic_font()),
        ("movie-texture.assets", fixtures.synthetic_movie_texture()),
        ("video-clip.assets", fixtures.synthetic_video_clip()),
        ("shader-unity6.assets", fixtures.synthetic_unity6_shader()),
        ("sprite.assets", fixtures.synthetic_tight_sprite()),
        ("legacy-pcm.assets", fixtures.synthetic_legacy_pcm()),
    ]


def drop_undetermined_names(expected: dict[str, Any], actual: dict[str, Any]) -> int:
    """Removes name rows UnityPy could not resolve, returning how many."""
    skipped = 0
    for expected_file, actual_file in zip(expected["Files"], actual["Files"]):
        for expected_object, actual_object in zip(
            expected_file["Objects"], actual_file["Objects"]
        ):
            if expected_object.get("Name") is None:
                expected_object.pop("Name", None)
                actual_object.pop("Name", None)
                skipped += 1
    return skipped


def compare(name: str, data: bytes, directory: Path) -> tuple[list[str], int]:
    if len(data) < UNITYPY_MINIMUM_SNIFF_BYTES:
        return (
            [
                f"{name}: fixture is {len(data)} bytes, below UnityPy's "
                f"{UNITYPY_MINIMUM_SNIFF_BYTES}-byte sniffing floor, so it would be "
                "read as a resource file rather than parsed"
            ],
            0,
        )
    path = directory / name
    path.write_bytes(data)

    expected = unitypy_manifest(path)
    actual = wheel_manifest(path)
    skipped = drop_undetermined_names(expected, actual)
    if expected == actual:
        return ([], skipped)
    return ([f"{name}:\n  UnityPy: {expected}\n  wheel:   {actual}"], skipped)


def main() -> None:
    failures: list[str] = []
    checked = 0
    skipped_names = 0
    with tempfile.TemporaryDirectory(prefix="assetstudio-unitypy-oracle-") as directory:
        for name, data in cases():
            case_failures, case_skipped = compare(name, data, Path(directory))
            failures.extend(case_failures)
            skipped_names += case_skipped
            checked += 1

    if failures:
        print(f"UnityPy differential: {len(failures)} of {checked} fixtures disagree\n")
        for failure in failures:
            print(failure)
            print()
        raise SystemExit(1)
    summary = f"UnityPy differential: {checked} fixtures agree"
    if skipped_names:
        summary += (
            f" ({skipped_names} name comparison(s) skipped: UnityPy's TypeTree"
            " database does not cover that class and version)"
        )
    print(summary)


if __name__ == "__main__":
    main()
