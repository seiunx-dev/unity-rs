#!/usr/bin/env python3
"""Differentially checks the old ``unpack_asset`` export contract.

This runner compares a locally built ``unity_rs.compat.unitypy`` facade with
UnityPy 1.25.x on caller-supplied bundles. Private game data is never copied
into the repository. It follows the useful parts of the former Lambda:

* enumerate the AssetBundle container;
* materialize Texture2D and Sprite images;
* recover TextAsset bytes without losing surrogate-escaped input;
* read embedded MonoBehaviour TypeTrees, otherwise compare raw bytes;
* also cover the Mesh, Shader, Font, and AudioClip conveniences added later.

Texture2D pixels, payload bytes, TypeTree values, and textual exports must be
exact. Tight Sprite pixels are also checked exactly by default. UnityPy uses
Pillow polygon coverage while unity-rs follows the managed exporter's
pixel-center rasterizer, so known edge-only differences can be reported without
failing by passing ``--allow-known-sprite-mask-differences``. The flag never
hides dimensions, counts, or difference metrics.

Both packages and Pillow must be installed in the running interpreter::

    python tools/unpack_asset_compat_diff.py \
      --unity-version 2022.3.21f1 bundle-a bundle-b
"""

from __future__ import annotations

import argparse
import hashlib
import struct
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional, Sequence


OLD_UNPACK_TYPES = {
    "Texture2D",
    "Sprite",
    "TextAsset",
    "MonoBehaviour",
}
ADDITIONAL_EXPORT_TYPES = {"Mesh", "Shader", "Font", "AudioClip"}
SUPPORTED_TYPES = OLD_UNPACK_TYPES | ADDITIONAL_EXPORT_TYPES


@dataclass(frozen=True)
class ImageDifference:
    width: int
    height: int
    differing_pixels: int
    alpha_differences: int
    worst_alpha: int
    worst_composited: float


@dataclass
class ComparisonStats:
    bundles: int = 0
    container_entries: int = 0
    compared: int = 0
    skipped: int = 0
    exact: int = 0
    known_sprite_differences: int = 0


class ValueBudget:
    def __init__(self, maximum_values: int, maximum_depth: int) -> None:
        self.maximum_values = maximum_values
        self.maximum_depth = maximum_depth
        self.values = 0

    def charge(self, depth: int) -> None:
        if depth > self.maximum_depth:
            raise ValueError(
                "TypeTree value nesting exceeds maximum_tree_depth {}".format(
                    self.maximum_depth
                )
            )
        self.values += 1
        if self.values > self.maximum_values:
            raise ValueError(
                "TypeTree contains more than maximum_tree_values {} values".format(
                    self.maximum_values
                )
            )


class ReaderHandle:
    """PPtr-shaped adapter for an uncontained ObjectReader."""

    def __init__(self, reader: Any) -> None:
        self.reader = reader
        self.path_id = reader.path_id
        self.file_id = 0
        self.type = reader.type

    def read(self) -> Any:
        return self.reader.read()

    def read_typetree(self) -> Any:
        return self.reader.read_typetree()

    def deref(self) -> Any:
        return self.reader


def text_asset_bytes(value: Any) -> bytes:
    script = value.m_Script
    if isinstance(script, str):
        return script.encode("utf-8", errors="surrogateescape")
    return bytes(script)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def normalized_text(value: str) -> str:
    return value.replace("\r\n", "\n").replace("\r", "\n")


def obj_values(value: str) -> list[list[Any]]:
    rows: list[list[Any]] = []
    for line in normalized_text(value).splitlines():
        parts = line.split()
        if not parts:
            continue
        row: list[Any] = [parts[0]]
        for token in parts[1:]:
            try:
                narrowed = struct.unpack("<f", struct.pack("<f", float(token)))[0]
            except (OverflowError, ValueError):
                row.append(token)
            else:
                row.append(narrowed)
        rows.append(row)
    return rows


def normalize_type_tree(
    value: Any,
    budget: ValueBudget,
    depth: int = 0,
) -> Any:
    budget.charge(depth)
    if hasattr(value, "m_FileID") and hasattr(value, "m_PathID"):
        return ("PPtr", int(value.m_FileID), int(value.m_PathID))
    if isinstance(value, dict):
        return {
            str(key): normalize_type_tree(child, budget, depth + 1)
            for key, child in value.items()
        }
    if isinstance(value, (list, tuple)):
        return [normalize_type_tree(child, budget, depth + 1) for child in value]
    if isinstance(value, bytes):
        return ("bytes", len(value), sha256(value))
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    raise TypeError("unsupported TypeTree value {}".format(type(value).__name__))


def image_difference(left: Any, right: Any) -> ImageDifference:
    left_rgba = left.convert("RGBA")
    right_rgba = right.convert("RGBA")
    if left_rgba.size != right_rgba.size:
        raise ValueError(
            "image dimensions differ: {} against {}".format(
                left_rgba.size, right_rgba.size
            )
        )
    width, height = left_rgba.size
    left_bytes = left_rgba.tobytes()
    right_bytes = right_rgba.tobytes()
    expected = width * height * 4
    if len(left_bytes) != expected or len(right_bytes) != expected:
        raise ValueError("RGBA image length does not match its dimensions")

    differing_pixels = 0
    alpha_differences = 0
    worst_alpha = 0
    worst_composited = 0.0
    for offset in range(0, expected, 4):
        left_pixel = left_bytes[offset : offset + 4]
        right_pixel = right_bytes[offset : offset + 4]
        if left_pixel == right_pixel:
            continue
        differing_pixels += 1
        left_alpha = left_pixel[3]
        right_alpha = right_pixel[3]
        alpha_delta = abs(left_alpha - right_alpha)
        if alpha_delta:
            alpha_differences += 1
            worst_alpha = max(worst_alpha, alpha_delta)
        for channel in range(3):
            contribution = abs(
                left_pixel[channel] * left_alpha
                - right_pixel[channel] * right_alpha
            ) / 255
            worst_composited = max(worst_composited, contribution)
    return ImageDifference(
        width=width,
        height=height,
        differing_pixels=differing_pixels,
        alpha_differences=alpha_differences,
        worst_alpha=worst_alpha,
        worst_composited=worst_composited,
    )


def container_rows(environment: Any) -> list[tuple[str, int, int, str]]:
    return [
        (
            path,
            int(pointer.path_id),
            int(pointer.file_id),
            pointer.type.name,
        )
        for path, pointer in environment.container.items()
    ]


def reader_identity(reader: Any) -> tuple[str, int, str]:
    source = getattr(reader.assets_file, "name", None)
    if source is None:
        source = reader.assets_file.path
    return (str(source).replace("\\", "/").lower(), reader.path_id, reader.type.name)


def compare_bytes(
    label: str,
    left: bytes,
    right: bytes,
    problems: list[str],
) -> bool:
    if left == right:
        return True
    problems.append(
        "{} differs: {} bytes sha256={} against {} bytes sha256={}".format(
            label,
            len(left),
            sha256(left),
            len(right),
            sha256(right),
        )
    )
    return False


def compare_mapping_bytes(
    label: str,
    left: dict[str, bytes],
    right: dict[str, bytes],
    problems: list[str],
) -> bool:
    if set(left) != set(right):
        problems.append(
            "{} names differ: {} against {}".format(
                label, sorted(left), sorted(right)
            )
        )
        return False
    exact = True
    for name in sorted(left):
        exact &= compare_bytes(
            "{} {!r}".format(label, name), left[name], right[name], problems
        )
    return exact


def compare_pointer(
    bundle: Path,
    path: str,
    left_pointer: Any,
    right_pointer: Any,
    args: argparse.Namespace,
    stats: ComparisonStats,
    problems: list[str],
    sprite_notes: list[str],
) -> None:
    kind = left_pointer.type.name
    label = "{}:{!r}:{} {}".format(
        bundle.name, path, left_pointer.path_id, kind
    )
    if kind not in SUPPORTED_TYPES:
        stats.skipped += 1
        return
    stats.compared += 1
    left_value = left_pointer.read()
    right_value = right_pointer.read()

    if kind in ("Texture2D", "Sprite"):
        try:
            difference = image_difference(left_value.image, right_value.image)
        except (AttributeError, ImportError, OSError, ValueError) as error:
            problems.append("{} image comparison failed: {}".format(label, error))
            return
        if difference.differing_pixels == 0:
            stats.exact += 1
            return
        message = (
            "{} differs in {}/{} pixels; alpha differs in {}, worst alpha {}, "
            "worst composited contribution {:.6g}".format(
                label,
                difference.differing_pixels,
                difference.width * difference.height,
                difference.alpha_differences,
                difference.worst_alpha,
                difference.worst_composited,
            )
        )
        if kind == "Sprite" and args.allow_known_sprite_mask_differences:
            stats.known_sprite_differences += 1
            sprite_notes.append(message)
        else:
            problems.append(message)
        return

    if kind == "TextAsset":
        exact = compare_bytes(
            label,
            text_asset_bytes(left_value),
            text_asset_bytes(right_value),
            problems,
        )
    elif kind == "MonoBehaviour":
        left_reader = left_pointer.deref()
        right_reader = right_pointer.deref()
        if bool(left_reader.serialized_type.nodes) != bool(
            right_reader.serialized_type.nodes
        ):
            problems.append("{} embedded TypeTree availability differs".format(label))
            return
        if left_reader.serialized_type.nodes:
            left_tree = normalize_type_tree(
                left_pointer.read_typetree(),
                ValueBudget(args.maximum_tree_values, args.maximum_tree_depth),
            )
            right_tree = normalize_type_tree(
                right_pointer.read_typetree(),
                ValueBudget(args.maximum_tree_values, args.maximum_tree_depth),
            )
            exact = left_tree == right_tree
            if not exact:
                problems.append("{} TypeTree values differ".format(label))
        else:
            exact = compare_bytes(
                label,
                left_reader.get_raw_data(),
                right_reader.get_raw_data(),
                problems,
            )
    elif kind == "Mesh":
        left_obj = obj_values(left_value.export())
        right_obj = obj_values(right_value.export())
        exact = left_obj == right_obj
        if not exact:
            problems.append("{} OBJ values differ".format(label))
    elif kind == "Shader":
        left_shader = normalized_text(left_value.export())
        right_shader = normalized_text(right_value.export())
        exact = left_shader == right_shader
        if not exact:
            problems.append("{} normalized shader text differs".format(label))
    elif kind == "Font":
        exact = compare_bytes(
            label, bytes(left_value.m_FontData), bytes(right_value.m_FontData), problems
        )
    elif kind == "AudioClip":
        exact = compare_mapping_bytes(
            label, left_value.samples, right_value.samples, problems
        )
    else:
        raise AssertionError("unhandled supported type {}".format(kind))
    if exact:
        stats.exact += 1


def compare_pointer_safely(
    bundle: Path,
    path: str,
    left_pointer: Any,
    right_pointer: Any,
    args: argparse.Namespace,
    stats: ComparisonStats,
    problems: list[str],
    sprite_notes: list[str],
) -> None:
    try:
        compare_pointer(
            bundle,
            path,
            left_pointer,
            right_pointer,
            args,
            stats,
            problems,
            sprite_notes,
        )
    except Exception as error:  # noqa: BLE001
        problems.append(
            "{}:{} {} comparison failed: {}".format(
                bundle.name,
                left_pointer.path_id,
                left_pointer.type.name,
                error,
            )
        )


def compare_bundle(
    bundle: Path,
    unitypy: Any,
    unity_rs_compat: Any,
    args: argparse.Namespace,
    stats: ComparisonStats,
    problems: list[str],
    sprite_notes: list[str],
) -> None:
    try:
        left = unitypy.load(str(bundle))
        right = unity_rs_compat.load(str(bundle))
    except Exception as error:  # noqa: BLE001
        problems.append("{} failed to load: {}".format(bundle, error))
        return
    stats.bundles += 1
    left_rows = container_rows(left)
    right_rows = container_rows(right)
    if left_rows != right_rows:
        problems.append(
            "{} container differs:\n  UnityPy: {!r}\n  unity-rs: {!r}".format(
                bundle.name, left_rows, right_rows
            )
        )
        return
    stats.container_entries += len(left_rows)
    right_entries = list(right.container.items())
    for (path, left_pointer), (right_path, right_pointer) in zip(
        left.container.items(), right_entries
    ):
        if path != right_path:
            raise AssertionError("equal container rows produced unequal paths")
        compare_pointer_safely(
            bundle,
            path,
            left_pointer,
            right_pointer,
            args,
            stats,
            problems,
            sprite_notes,
        )
    if args.include_uncontained:
        compare_uncontained(
            bundle,
            left,
            right,
            args,
            stats,
            problems,
            sprite_notes,
        )


def compare_uncontained(
    bundle: Path,
    left: Any,
    right: Any,
    args: argparse.Namespace,
    stats: ComparisonStats,
    problems: list[str],
    sprite_notes: list[str],
) -> None:
    left_contained = {
        reader_identity(pointer.deref()) for _path, pointer in left.container.items()
    }
    right_contained = {
        reader_identity(pointer.deref()) for _path, pointer in right.container.items()
    }
    left_readers = sorted(
        (
            (reader_identity(reader), reader)
            for reader in left.objects
            if reader.type.name in SUPPORTED_TYPES
            and reader_identity(reader) not in left_contained
        ),
        key=lambda item: item[0],
    )
    right_readers = sorted(
        (
            (reader_identity(reader), reader)
            for reader in right.objects
            if reader.type.name in SUPPORTED_TYPES
            and reader_identity(reader) not in right_contained
        ),
        key=lambda item: item[0],
    )
    left_identities = [identity for identity, _reader in left_readers]
    right_identities = [identity for identity, _reader in right_readers]
    if left_identities != right_identities:
        problems.append(
            "{} uncontained supported objects differ:\n  UnityPy: {!r}\n  "
            "unity-rs: {!r}".format(
                bundle.name, left_identities, right_identities
            )
        )
        return
    for (identity, left_reader), (_right_identity, right_reader) in zip(
        left_readers, right_readers
    ):
        compare_pointer_safely(
            bundle,
            "<uncontained:{}>".format(identity[0]),
            ReaderHandle(left_reader),
            ReaderHandle(right_reader),
            args,
            stats,
            problems,
            sprite_notes,
        )


def parse_arguments(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundles", nargs="+", type=Path)
    parser.add_argument("--unity-version", default=None)
    parser.add_argument("--maximum-tree-values", type=int, default=1_000_000)
    parser.add_argument("--maximum-tree-depth", type=int, default=256)
    parser.add_argument(
        "--allow-known-sprite-mask-differences",
        action="store_true",
        help="report tight Sprite rasterizer differences without failing",
    )
    parser.add_argument(
        "--include-uncontained",
        action="store_true",
        help="also compare supported objects absent from the AssetBundle container",
    )
    args = parser.parse_args(argv)
    if args.maximum_tree_values < 1:
        parser.error("--maximum-tree-values must be positive")
    if args.maximum_tree_depth < 1:
        parser.error("--maximum-tree-depth must be positive")
    for bundle in args.bundles:
        if not bundle.is_file():
            parser.error("bundle does not exist or is not a file: {}".format(bundle))
    return args


def report(
    stats: ComparisonStats,
    problems: list[str],
    sprite_notes: list[str],
) -> int:
    print(
        "checked {} bundle(s), {} container entries: {} exact, {} known Sprite "
        "difference(s), {} skipped".format(
            stats.bundles,
            stats.container_entries,
            stats.exact,
            stats.known_sprite_differences,
            stats.skipped,
        )
    )
    if sprite_notes:
        print("\nknown Sprite rasterizer differences:")
        for note in sprite_notes:
            print("  " + note)
    if problems:
        print("\n{} unexplained difference(s):".format(len(problems)), file=sys.stderr)
        for problem in problems:
            print("  " + problem, file=sys.stderr)
        return 1
    if stats.compared == 0:
        print("no supported exports were compared", file=sys.stderr)
        return 1
    return 0


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_arguments(argv)
    try:
        import UnityPy  # noqa: PLC0415
        from unity_rs.compat import unitypy as unity_rs_compat  # noqa: PLC0415
    except ImportError as error:
        print(
            "UnityPy, unity-rs, and Pillow must be installed: {}".format(error),
            file=sys.stderr,
        )
        return 2
    if args.unity_version:
        UnityPy.config.FALLBACK_UNITY_VERSION = args.unity_version
        unity_rs_compat.config.FALLBACK_UNITY_VERSION = args.unity_version

    stats = ComparisonStats()
    problems: list[str] = []
    sprite_notes: list[str] = []
    for bundle in args.bundles:
        compare_bundle(
            bundle,
            UnityPy,
            unity_rs_compat,
            args,
            stats,
            problems,
            sprite_notes,
        )
    return report(stats, problems, sprite_notes)


if __name__ == "__main__":
    raise SystemExit(main())
