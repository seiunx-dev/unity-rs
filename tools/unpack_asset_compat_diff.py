#!/usr/bin/env python3
"""Differentially checks the old ``unpack_asset`` export contract.

This runner compares a locally built ``unity_rs.compat.unitypy`` facade with
UnityPy 1.25.x on caller-supplied bundles or extracted asset directories.
Private game data is never copied into the repository. It follows the useful
parts of the former Lambda:

* enumerate the AssetBundle container;
* materialize Texture2D and Sprite images;
* recover TextAsset bytes without losing surrogate-escaped input;
* read embedded MonoBehaviour TypeTrees, otherwise compare raw bytes;
* also cover the Mesh, Shader, Font, and AudioClip conveniences added later.

Texture2D pixels, payload bytes, TypeTree values, font bytes, and audio files
must be exact by default. Mesh OBJ rows are compared at their represented
``f32`` values. Shader output is checked against UnityPy's independently parsed
name, property order, and per-SubShader Pass/UsePass/GrabPass counts instead of
UnityPy's less complete text writer. Tight Sprite pixels are exact by default;
known rasterizer differences on tight Sprites require an explicit reporting
flag after their source textures have been checked. Alpha8 unstored-channel and
RGB565 one-level conversion differences, plus the independently established
one-unit PCM16 vgmstream rounding boundary, likewise require explicit flags.
No flag hides dimensions, counts, or difference metrics.

Both packages and Pillow must be installed in the running interpreter::

    python tools/unpack_asset_compat_diff.py \
      --unity-version 2022.3.21f1 bundle-a bundle-b
"""

from __future__ import annotations

import argparse
import hashlib
import io
import re
import struct
import sys
import wave
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
    worst_channel: int
    worst_alpha: int
    worst_composited: float


@dataclass(frozen=True)
class Pcm16Difference:
    samples: int
    differing_samples: int
    worst_sample: int
    rms: float


@dataclass(frozen=True)
class ShaderManifest:
    name: str
    properties: tuple[str, ...]
    subshader_pass_counts: tuple[int, ...]


@dataclass(frozen=True)
class SpriteSourceComparison:
    exact: bool
    known_texture_differences: tuple[str, ...]


@dataclass
class ComparisonStats:
    bundles: int = 0
    container_entries: int = 0
    skipped: int = 0
    exact: int = 0
    known_texture_differences: int = 0
    known_sprite_differences: int = 0
    known_audio_differences: int = 0
    oracle_failures: int = 0


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


def _riff_data_range(value: bytes) -> Optional[tuple[int, int]]:
    if len(value) < 12 or value[:4] != b"RIFF" or value[8:12] != b"WAVE":
        return None
    offset = 12
    while offset <= len(value) - 8:
        size = struct.unpack_from("<I", value, offset + 4)[0]
        start = offset + 8
        end = start + size
        if end > len(value):
            return None
        if value[offset : offset + 4] == b"data":
            return (start, end)
        offset = end + (size & 1)
    return None


def pcm16_difference(left: bytes, right: bytes) -> Optional[Pcm16Difference]:
    left_data_range = _riff_data_range(left)
    right_data_range = _riff_data_range(right)
    if left_data_range is None or right_data_range is None:
        return None
    left_start, left_end = left_data_range
    right_start, right_end = right_data_range
    if (
        left_start != right_start
        or left_end != right_end
        or left[:left_start] != right[:right_start]
        or left[left_end:] != right[right_end:]
    ):
        return None
    try:
        with wave.open(io.BytesIO(left), "rb") as left_wave:
            left_params = left_wave.getparams()
            left_frames = left_wave.readframes(left_params.nframes)
        with wave.open(io.BytesIO(right), "rb") as right_wave:
            right_params = right_wave.getparams()
            right_frames = right_wave.readframes(right_params.nframes)
    except (EOFError, wave.Error):
        return None
    if (
        left_params != right_params
        or left_params.sampwidth != 2
        or left_params.comptype != "NONE"
        or len(left_frames) != len(right_frames)
        or len(left_frames) % 2 != 0
    ):
        return None
    samples = len(left_frames) // 2
    differing_samples = 0
    worst_sample = 0
    squared_difference = 0
    for (left_sample,), (right_sample,) in zip(
        struct.iter_unpack("<h", left_frames),
        struct.iter_unpack("<h", right_frames),
    ):
        difference = abs(left_sample - right_sample)
        if difference:
            differing_samples += 1
            worst_sample = max(worst_sample, difference)
            squared_difference += difference * difference
    rms = (squared_difference / samples) ** 0.5 if samples else 0.0
    return Pcm16Difference(samples, differing_samples, worst_sample, rms)


def _keyword_blocks(value: str, keyword: str) -> list[str]:
    blocks: list[str] = []
    pattern = re.compile(r"\b{}\s*\{{".format(re.escape(keyword)))
    for match in pattern.finditer(value):
        opening = value.find("{", match.start(), match.end())
        depth = 0
        quote = False
        escaped = False
        closing: Optional[int] = None
        for index in range(opening, len(value)):
            character = value[index]
            if quote:
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == '"':
                    quote = False
                continue
            if character == '"':
                quote = True
            elif character == "{":
                depth += 1
            elif character == "}":
                depth -= 1
                if depth == 0:
                    closing = index
                    break
        if closing is None:
            raise ValueError("{} block has no closing brace".format(keyword))
        blocks.append(value[opening + 1 : closing])
    return blocks


def unitypy_shader_manifest(value: Any) -> ShaderManifest:
    parsed = value.m_ParsedForm
    return ShaderManifest(
        name=str(parsed.m_Name),
        properties=tuple(str(prop.m_Name) for prop in parsed.m_PropInfo.m_Props),
        subshader_pass_counts=tuple(
            len(subshader.m_Passes) for subshader in parsed.m_SubShaders
        ),
    )


def exported_shader_manifest(value: str) -> ShaderManifest:
    normalized = normalized_text(value)
    name_match = re.search(r'\bShader\s+"((?:\\.|[^"\\])*)"', normalized)
    if name_match is None:
        raise ValueError("exported shader has no Shader name")
    property_blocks = _keyword_blocks(normalized, "Properties")
    if len(property_blocks) != 1:
        raise ValueError(
            "exported shader has {} Properties blocks".format(len(property_blocks))
        )
    property_pattern = re.compile(
        r"^\s*(?:\[[^\]\r\n]*\]\s*)*([A-Za-z_][A-Za-z0-9_]*)\s*\(",
        re.MULTILINE,
    )
    properties = tuple(property_pattern.findall(property_blocks[0]))
    subshader_blocks = _keyword_blocks(normalized, "SubShader")
    pass_pattern = re.compile(
        r"^\s*(?:Pass\s*\{|UsePass\b|GrabPass\s*\{)", re.MULTILINE
    )
    return ShaderManifest(
        name=name_match.group(1),
        properties=properties,
        subshader_pass_counts=tuple(
            len(pass_pattern.findall(block)) for block in subshader_blocks
        ),
    )


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
    worst_channel = 0
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
        worst_channel = max(
            worst_channel,
            *(abs(left_pixel[channel] - right_pixel[channel]) for channel in range(4)),
        )
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
        worst_channel=worst_channel,
        worst_alpha=worst_alpha,
        worst_composited=worst_composited,
    )


def known_texture_difference_kind(
    format_code: int, difference: ImageDifference
) -> Optional[str]:
    if format_code == 1 and difference.alpha_differences == 0:
        return "Alpha8 unstored RGB fill"
    if (
        format_code == 7
        and difference.alpha_differences == 0
        and difference.worst_channel <= 1
    ):
        return "RGB565 one-level conversion rounding"
    return None


def effective_sprite_render_data(sprite: Any) -> Any:
    atlas_reader = None
    atlas_pointer = getattr(sprite, "m_SpriteAtlas", None)
    if atlas_pointer is not None and int(atlas_pointer.path_id) != 0:
        atlas_reader = atlas_pointer.deref()
    elif getattr(sprite, "m_AtlasTags", None):
        atlas_name = sprite.m_AtlasTags[0]
        for reader in sprite.assets_file.objects.values():
            if reader.type.name == "SpriteAtlas" and reader.peek_name() == atlas_name:
                atlas_reader = reader
                break
    if atlas_reader is None:
        return sprite.m_RD

    atlas = atlas_reader.read()
    render_data_key = sprite.m_RenderDataKey
    for key, render_data in atlas.m_RenderDataMap:
        if key == render_data_key:
            return render_data
    raise ValueError("SpriteAtlas does not contain the Sprite render-data key")


def compare_sprite_sources(
    left_sprite: Any, right_pointer: Any
) -> SpriteSourceComparison:
    render_data = effective_sprite_render_data(left_sprite)
    source_pointers = [render_data.texture]
    alpha_texture = getattr(render_data, "alphaTexture", None)
    if alpha_texture is not None and int(alpha_texture.path_id) != 0:
        source_pointers.append(alpha_texture)

    right_sprite_reader = right_pointer.deref()
    if right_sprite_reader is None:
        raise ValueError("unity-rs Sprite pointer did not resolve")
    right_sprite_file = right_sprite_reader.assets_file
    right_environment = right_sprite_file.environment
    known_differences: list[str] = []
    compared = 0
    for source_pointer in source_pointers:
        if int(source_pointer.path_id) == 0:
            continue
        left_reader = source_pointer.deref()
        if left_reader is None:
            continue
        source_name = getattr(left_reader.assets_file, "name", None)
        if source_name is None:
            source_name = left_reader.assets_file.path
        if (
            int(source_pointer.file_id) == 0
            and left_reader.assets_file is left_sprite.assets_file
        ):
            right_file = right_sprite_file
        else:
            try:
                right_file = right_environment.find_file(str(source_name))
            except FileNotFoundError as error:
                raise ValueError(
                    "unity-rs could not find Sprite source file {!r}".format(
                        source_name
                    )
                ) from error
            if right_file is None:
                raise ValueError(
                    "unity-rs could not find Sprite source file {!r}".format(
                        source_name
                    )
                )
        right_reader = right_file.objects.get(int(left_reader.path_id))
        if right_reader is None or right_reader.type.name != "Texture2D":
            raise ValueError(
                "unity-rs could not resolve Sprite Texture2D {}:{}".format(
                    source_name, left_reader.path_id
                )
            )
        left_texture = left_reader.read()
        right_texture = right_reader.read()
        difference = image_difference(left_texture.image, right_texture.image)
        compared += 1
        if difference.differing_pixels == 0:
            continue
        try:
            format_code = int(left_texture.m_TextureFormat)
        except (AttributeError, TypeError, ValueError):
            format_code = -1
        kind = known_texture_difference_kind(format_code, difference)
        if kind is None:
            raise ValueError(
                "Sprite source Texture2D {}:{} has an unexplained pixel difference".format(
                    source_name, left_reader.path_id
                )
            )
        known_differences.append(kind)
    if compared == 0:
        raise ValueError("Sprite has no resolvable source Texture2D")
    return SpriteSourceComparison(
        exact=not known_differences,
        known_texture_differences=tuple(known_differences),
    )


def sprite_uses_tight_mask(value: Any) -> bool:
    settings = int(effective_sprite_render_data(value).settingsRaw)
    return ((settings >> 1) & 1) == 0


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


def compare_pointer(
    bundle: Path,
    path: str,
    left_pointer: Any,
    right_pointer: Any,
    args: argparse.Namespace,
    stats: ComparisonStats,
    problems: list[str],
    texture_notes: list[str],
    sprite_notes: list[str],
    audio_notes: list[str],
    oracle_notes: list[str],
) -> None:
    kind = left_pointer.type.name
    label = "{}:{!r}:{} {}".format(
        bundle.name, path, left_pointer.path_id, kind
    )
    if kind not in args.types:
        stats.skipped += 1
        return
    try:
        left_value = left_pointer.read()
    except Exception as error:  # noqa: BLE001
        stats.oracle_failures += 1
        oracle_notes.append("{} UnityPy read failed: {}".format(label, error))
        return
    try:
        right_value = right_pointer.read()
    except Exception as error:  # noqa: BLE001
        problems.append("{} unity-rs read failed: {}".format(label, error))
        return

    if kind in ("Texture2D", "Sprite"):
        try:
            left_image = left_value.image
        except (AttributeError, ImportError, OSError, ValueError) as error:
            stats.oracle_failures += 1
            oracle_notes.append(
                "{} UnityPy image conversion failed: {}".format(label, error)
            )
            return
        try:
            right_image = right_value.image
        except (AttributeError, ImportError, OSError, ValueError) as error:
            problems.append(
                "{} unity-rs image conversion failed: {}".format(label, error)
            )
            return
        difference = image_difference(left_image, right_image)
        if difference.differing_pixels == 0:
            stats.exact += 1
            return
        message = (
            "{} differs in {}/{} pixels; alpha differs in {}, worst alpha {}, "
            "worst channel {}, worst composited contribution {:.6g}".format(
                label,
                difference.differing_pixels,
                difference.width * difference.height,
                difference.alpha_differences,
                difference.worst_alpha,
                difference.worst_channel,
                difference.worst_composited,
            )
        )
        texture_difference_kind: Optional[str] = None
        if kind == "Texture2D":
            try:
                format_code = int(left_value.m_TextureFormat)
            except (AttributeError, TypeError, ValueError):
                format_code = -1
            texture_difference_kind = known_texture_difference_kind(
                format_code, difference
            )
        if (
            texture_difference_kind is not None
            and args.allow_known_texture_conversion_differences
        ):
            stats.known_texture_differences += 1
            texture_notes.append("{}: {}".format(texture_difference_kind, message))
        elif kind == "Sprite":
            try:
                source_comparison = compare_sprite_sources(left_value, right_pointer)
            except (AttributeError, ImportError, OSError, TypeError, ValueError) as error:
                problems.append(
                    "{} could not classify its Sprite source: {}".format(label, error)
                )
                return
            tight_mask = sprite_uses_tight_mask(left_value)
            if (
                source_comparison.exact
                and tight_mask
                and args.allow_known_sprite_mask_differences
            ):
                stats.known_sprite_differences += 1
                sprite_notes.append(message)
            elif (
                source_comparison.known_texture_differences
                and args.allow_known_texture_conversion_differences
                and (not tight_mask or args.allow_known_sprite_mask_differences)
            ):
                stats.known_texture_differences += 1
                texture_notes.append(
                    "Sprite source {}: {}".format(
                        ", ".join(source_comparison.known_texture_differences),
                        message,
                    )
                )
            elif not tight_mask:
                problems.append(
                    "{} differs although its source texture is exact and it does "
                    "not use a tight mask".format(label)
                )
            else:
                problems.append(message)
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
        left_shader = unitypy_shader_manifest(left_value)
        right_shader = exported_shader_manifest(right_value.export())
        exact = left_shader == right_shader
        if not exact:
            problems.append(
                "{} shader manifest differs: {!r} against {!r}".format(
                    label, left_shader, right_shader
                )
            )
    elif kind == "Font":
        exact = compare_bytes(
            label, bytes(left_value.m_FontData), bytes(right_value.m_FontData), problems
        )
    elif kind == "AudioClip":
        left_samples = left_value.samples
        right_samples = right_value.samples
        if set(left_samples) != set(right_samples):
            problems.append(
                "{} names differ: {} against {}".format(
                    label, sorted(left_samples), sorted(right_samples)
                )
            )
            return
        differences = [
            (name, pcm16_difference(left_samples[name], right_samples[name]))
            for name in sorted(left_samples)
            if left_samples[name] != right_samples[name]
        ]
        if not differences:
            exact = True
        elif (
            args.allow_known_audio_rounding_differences
            and all(difference is not None for _name, difference in differences)
            and all(
                difference.worst_sample <= 1
                for _name, difference in differences
                if difference is not None
            )
        ):
            stats.known_audio_differences += 1
            for name, difference in differences:
                if difference is None:
                    raise AssertionError("validated PCM16 difference disappeared")
                audio_notes.append(
                    "{} {!r} differs in {}/{} PCM16 samples; worst {}, RMS {:.6g}".format(
                        label,
                        name,
                        difference.differing_samples,
                        difference.samples,
                        difference.worst_sample,
                        difference.rms,
                    )
                )
            exact = False
        else:
            exact = False
            for name in sorted(left_samples):
                compare_bytes(
                    "{} {!r}".format(label, name),
                    left_samples[name],
                    right_samples[name],
                    problems,
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
    texture_notes: list[str],
    sprite_notes: list[str],
    audio_notes: list[str],
    oracle_notes: list[str],
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
            texture_notes,
            sprite_notes,
            audio_notes,
            oracle_notes,
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
    texture_notes: list[str],
    sprite_notes: list[str],
    audio_notes: list[str],
    oracle_notes: list[str],
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
            texture_notes,
            sprite_notes,
            audio_notes,
            oracle_notes,
        )
    if args.include_uncontained:
        compare_uncontained(
            bundle,
            left,
            right,
            args,
            stats,
            problems,
            texture_notes,
            sprite_notes,
            audio_notes,
            oracle_notes,
        )


def compare_uncontained(
    bundle: Path,
    left: Any,
    right: Any,
    args: argparse.Namespace,
    stats: ComparisonStats,
    problems: list[str],
    texture_notes: list[str],
    sprite_notes: list[str],
    audio_notes: list[str],
    oracle_notes: list[str],
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
            if reader.type.name in args.types
            and reader_identity(reader) not in left_contained
        ),
        key=lambda item: item[0],
    )
    right_readers = sorted(
        (
            (reader_identity(reader), reader)
            for reader in right.objects
            if reader.type.name in args.types
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
            texture_notes,
            sprite_notes,
            audio_notes,
            oracle_notes,
        )


def parse_arguments(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "bundles",
        nargs="+",
        type=Path,
        help="Unity bundle/file or extracted asset directory",
    )
    parser.add_argument("--unity-version", default=None)
    parser.add_argument(
        "--type",
        dest="types",
        action="append",
        choices=sorted(SUPPORTED_TYPES),
        help="compare only this export type; repeat to select multiple types",
    )
    parser.add_argument("--maximum-tree-values", type=int, default=1_000_000)
    parser.add_argument("--maximum-tree-depth", type=int, default=256)
    parser.add_argument(
        "--allow-known-texture-conversion-differences",
        action="store_true",
        help=(
            "report Alpha8 unstored-RGB and one-level RGB565 conversion "
            "differences without failing"
        ),
    )
    parser.add_argument(
        "--allow-known-sprite-mask-differences",
        action="store_true",
        help="report tight Sprite rasterizer differences without failing",
    )
    parser.add_argument(
        "--allow-known-audio-rounding-differences",
        action="store_true",
        help=(
            "report matching PCM16 WAVs whose samples differ by at most one "
            "unit without failing"
        ),
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
    args.types = frozenset(args.types or SUPPORTED_TYPES)
    for bundle in args.bundles:
        if not bundle.exists() or not (bundle.is_file() or bundle.is_dir()):
            parser.error(
                "input does not exist or is not a file/directory: {}".format(bundle)
            )
    return args


def report(
    stats: ComparisonStats,
    problems: list[str],
    texture_notes: list[str],
    sprite_notes: list[str],
    audio_notes: list[str],
    oracle_notes: list[str],
) -> int:
    print(
        "checked {} input(s), {} container entries: {} exact, {} known Texture "
        "difference(s), {} known Sprite difference(s), {} known audio "
        "difference(s), {} oracle failure(s), {} skipped".format(
            stats.bundles,
            stats.container_entries,
            stats.exact,
            stats.known_texture_differences,
            stats.known_sprite_differences,
            stats.known_audio_differences,
            stats.oracle_failures,
            stats.skipped,
        )
    )
    if texture_notes:
        print("\nknown Texture conversion differences:")
        for note in texture_notes:
            print("  " + note)
    if sprite_notes:
        print("\nknown Sprite rasterizer differences:")
        for note in sprite_notes:
            print("  " + note)
    if audio_notes:
        print("\nknown PCM16 decoder-rounding differences:")
        for note in audio_notes:
            print("  " + note)
    if oracle_notes:
        print("\nUnityPy oracle failures (not compared):")
        for note in oracle_notes:
            print("  " + note)
    if problems:
        print("\n{} unexplained difference(s):".format(len(problems)), file=sys.stderr)
        for problem in problems:
            print("  " + problem, file=sys.stderr)
        return 1
    if (
        stats.exact
        + stats.known_texture_differences
        + stats.known_sprite_differences
        + stats.known_audio_differences
        == 0
    ):
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
    texture_notes: list[str] = []
    sprite_notes: list[str] = []
    audio_notes: list[str] = []
    oracle_notes: list[str] = []
    for bundle in args.bundles:
        compare_bundle(
            bundle,
            UnityPy,
            unity_rs_compat,
            args,
            stats,
            problems,
            texture_notes,
            sprite_notes,
            audio_notes,
            oracle_notes,
        )
    return report(
        stats,
        problems,
        texture_notes,
        sprite_notes,
        audio_notes,
        oracle_notes,
    )


if __name__ == "__main__":
    raise SystemExit(main())
