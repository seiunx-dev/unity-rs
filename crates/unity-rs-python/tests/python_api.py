from __future__ import annotations

import io
import json
import os
import struct
import sys
import tempfile
import threading
import zlib
from collections.abc import Callable
from pathlib import Path
from typing import Optional, TypeVar

from unity_rs import (
    AclCompressedTracks,
    AclDecodedClip,
    AnimationClip,
    AnimatorOverrideController,
    AnimatorController,
    AssetBundle,
    UnityRs,
    Avatar,
    AudioClip,
    BinaryAsset,
    BuildSettings,
    CubismClipMotion,
    CubismMotionTargets,
    CubismDisplayInfo,
    CubismExpression,
    CubismExpressionParameter,
    CubismFadeMotion,
    CubismPosePart,
    CubismPhysics,
    ExportLimits,
    ExtractionLimits,
    FbxCandidate,
    LegacyAnimation,
    Live2dPackage,
    Material,
    ModelTextureLimits,
    MonoBehaviourSchema,
    MonoBehaviourSchemas,
    MonoScript,
    PlayerSettings,
    PreloadData,
    ResourceInfo,
    ResourceIterator,
    ResourceManager,
    SceneLimits,
    SpriteAtlas,
    SpriteAtlasRenderData,
    SpriteAtlasRenderDataKey,
    SpriteAtlasSecondaryTexture,
    SpriteMetadata,
    SpriteMetadataLimits,
    SpriteRenderData,
    SpriteSecondaryTexture,
    SpriteSettings,
    extract,
)
from unity_rs.compat import unitypy as UnityPyCompat

# Every check here is an `assert`, and `-O` or `PYTHONOPTIMIZE` deletes those
# outright rather than skipping them: this suite would import the package,
# build every fixture, call every reader and exit zero without comparing one
# value. A suite that silently stops checking is the failure mode this project
# has already been bitten by twice, so refuse to run instead.
if not __debug__:
    raise SystemExit(
        "refusing to run with assertions disabled (-O / PYTHONOPTIMIZE): "
        "every check in this suite is an assert"
    )

T = TypeVar("T")


def push_i32(output: bytearray, value: int) -> None:
    output.extend(struct.pack("<i", value))


def push_u32(output: bytearray, value: int) -> None:
    output.extend(struct.pack("<I", value))


def push_f32s(output: bytearray, values: tuple[float, ...]) -> None:
    for value in values:
        output.extend(struct.pack("<f", value))


def push_aligned_string(output: bytearray, value: str) -> None:
    encoded = value.encode("utf-8")
    push_i32(output, len(encoded))
    output.extend(encoded)
    while len(output) % 4:
        output.append(0)


def align_with_base(output: bytearray, base: int, alignment: int) -> None:
    while (base + len(output)) % alignment:
        output.append(0)


def align(output: bytearray, alignment: int) -> None:
    while len(output) % alignment:
        output.append(0)


def synthetic_text_asset(external_path: Optional[str] = None) -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python")
    push_i32(payload, 12)
    payload.extend(b"hello python")

    return finish_v22_asset(49, payload, external_path=external_path)


class MemoryFileSystem:
    """Small fsspec-shaped filesystem used without a runtime dependency."""

    sep = "/"

    def __init__(self, files: dict[str, bytes]) -> None:
        self.files = files
        self.opened: list[io.BytesIO] = []

    def isfile(self, path: str) -> bool:
        return path in self.files

    def isdir(self, path: str) -> bool:
        prefix = path.rstrip(self.sep) + self.sep
        return any(name.startswith(prefix) for name in self.files)

    def walk(self, path: str) -> list[tuple[str, list[str], list[str]]]:
        prefix = path.rstrip(self.sep) + self.sep
        files = [
            name[len(prefix) :]
            for name in self.files
            if name.startswith(prefix) and self.sep not in name[len(prefix) :]
        ]
        return [(path, [], files)]

    def open(self, path: str, mode: str) -> io.BytesIO:
        assert mode == "rb"
        stream = io.BytesIO(self.files[path])
        self.opened.append(stream)
        return stream


class MissingReadStream:
    def __init__(self) -> None:
        self.closed = False

    def close(self) -> None:
        self.closed = True


class InvalidStreamFileSystem(MemoryFileSystem):
    def __init__(self) -> None:
        super().__init__({"invalid.assets": b"unused"})
        self.invalid_stream = MissingReadStream()

    def open(self, path: str, mode: str) -> MissingReadStream:
        assert path == "invalid.assets"
        assert mode == "rb"
        return self.invalid_stream


def synthetic_unity6_shader(unity_version: str = "6000.2.0f1") -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "Unity6Object")
    push_i32(payload, 0)  # properties
    push_i32(payload, 0)  # subshaders
    push_i32(payload, 0)  # keyword names
    push_i32(payload, 0)  # keyword flags
    push_aligned_string(payload, "Parsed/Unity6")
    push_aligned_string(payload, "")
    push_aligned_string(payload, "")
    push_i32(payload, 0)  # dependencies
    push_i32(payload, 0)  # render-pipeline custom editors
    payload.append(0)  # disable no-subshaders message
    align(payload, 4)
    push_i32(payload, 0)  # platforms
    push_i32(payload, 0)  # nested chunk offsets
    push_i32(payload, 0)  # nested compressed lengths
    push_i32(payload, 0)  # nested decompressed lengths
    push_i32(payload, 0)  # compressed blob
    # 2022.2 added a per-platform stage count here, and Unity 6 appends the
    # source asset's GUID. Without them this described a file Unity does not
    # write, which is how three missing fields stayed hidden.
    push_i32(payload, 0)  # stage counts
    push_i32(payload, 0)  # object dependencies
    push_i32(payload, 0)  # non-modifiable textures
    payload.append(0)  # baked
    align(payload, 4)
    payload.extend(b"\x00" * 16)  # asset GUID
    return finish_v22_asset(48, payload, unity_version)


def synthetic_mesh(*, external: bool = False, tuanjie: bool = False) -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "tri:mesh")
    push_i32(payload, 1)
    push_u32(payload, 0)
    push_u32(payload, 3)
    push_i32(payload, 0)
    push_u32(payload, 0)
    push_u32(payload, 0)
    push_u32(payload, 3)
    payload.extend(bytes(24))

    for _ in range(4):
        push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_u32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 0)

    payload.extend((0, 1, 0, 0))
    if tuanjie:
        align(payload, 4)
        push_tuanjie_mesh_cluster(payload)
        align(payload, 4)
    else:
        align(payload, 4)
    push_i32(payload, 0)
    push_i32(payload, 6)
    payload.extend(struct.pack("<HHH", 0, 1, 2))
    align(payload, 4)

    push_u32(payload, 3)
    push_i32(payload, 1)
    payload.extend((0, 0, 0, 3))
    vertex_data = synthetic_mesh_vertex_data()
    push_i32(payload, 0 if external else len(vertex_data))
    if not external:
        payload.extend(vertex_data)
    align(payload, 4)

    for _ in range(4):
        push_empty_packed_float(payload)
    for _ in range(3):
        push_empty_packed_int(payload)
    push_empty_packed_float(payload)
    for _ in range(2):
        push_empty_packed_int(payload)
    push_u32(payload, 0)

    payload.extend(bytes(24))
    for _ in range(4):
        push_i32(payload, 0)
    payload.extend(bytes(8))
    align(payload, 4)
    payload.extend(struct.pack("<q", 3 if external else 0))
    push_u32(payload, len(vertex_data) if external else 0)
    push_aligned_string(payload, "python-mesh.resS" if external else "")
    if tuanjie:
        payload.extend((1, 0))
    return finish_v22_asset(
        43,
        payload,
        "2022.3.61t2" if tuanjie else "2022.3.62f1",
    )


def push_tuanjie_mesh_cluster(output: bytearray) -> None:
    push_i32(output, 0)
    output.extend(struct.pack("<f", 1.0))
    push_i32(output, 3)
    output.extend((0xA5, 0x5A, 0xC3))
    push_i32(output, 0)  # hierarchy nodes
    push_i32(output, 0)  # page streaming infos
    push_i32(output, 0)  # page dependency indices
    push_i32(output, 0)  # streamable cluster page


def synthetic_mesh_vertex_data() -> bytes:
    vertices = ((1.0, 0.0, 0.0), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0))
    vertex_data = bytearray()
    for vertex in vertices:
        push_f32s(vertex_data, vertex)
    vertex_data.extend(bytes(48 - len(vertex_data)))
    return bytes(vertex_data)


def synthetic_texture2d() -> bytes:
    return finish_v22_asset(28, texture2d_payload())


def synthetic_texture2d_array() -> bytes:
    pixels = bytes(
        (
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            11,
            12,
            13,
            14,
            15,
            16,
            17,
            18,
        )
    )
    payload = bytearray()
    push_aligned_string(payload, "array")
    push_i32(payload, 0)
    payload.extend((0, 0))
    align(payload, 4)
    push_i32(payload, 0)
    push_i32(payload, 4)
    push_i32(payload, 1)
    push_i32(payload, 2)
    push_i32(payload, 2)
    push_i32(payload, 1)
    push_u32(payload, len(pixels))
    payload.extend(bytes(24))
    push_i32(payload, 7)
    payload.append(1)
    align(payload, 4)
    push_i32(payload, len(pixels))
    payload.extend(pixels)
    return finish_v22_asset(187, payload)


def texture2d_payload(
    name: str = "image",
    width: int = 2,
    height: int = 2,
    pixels: bytes = bytes(
        (
            255,
            0,
            0,
            1,
            0,
            255,
            0,
            2,
            0,
            0,
            255,
            3,
            255,
            255,
            255,
            4,
        )
    ),
    mip_count: int = 1,
    platform_blob: bytes = b"",
) -> bytearray:
    payload = bytearray()
    push_aligned_string(payload, name)
    push_i32(payload, 0)
    payload.extend((0, 0))
    while len(payload) % 4:
        payload.append(0)
    push_i32(payload, width)
    push_i32(payload, height)
    payload.extend(struct.pack("<I", len(pixels)))
    push_i32(payload, 0)
    push_i32(payload, 4)
    push_i32(payload, mip_count)
    payload.extend((0, 0, 0))
    while len(payload) % 4:
        payload.append(0)
    push_aligned_string(payload, "")
    payload.append(0)
    while len(payload) % 4:
        payload.append(0)
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_i32(payload, 2)
    payload.extend(bytes(24))
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, len(platform_blob))
    payload.extend(platform_blob)
    align(payload, 4)
    push_i32(payload, len(pixels))
    payload.extend(pixels)
    return payload


def synthetic_sprite_with_atlas_backfill() -> bytes:
    sprite = bytearray()
    push_aligned_string(sprite, "python sprite")
    push_f32s(sprite, (0.0, 0.0, 1.0, 1.0))
    push_f32s(sprite, (0.0, 0.0))
    push_f32s(sprite, (0.0, 0.0, 0.0, 0.0))
    push_f32s(sprite, (100.0, 0.5, 0.5))
    push_u32(sprite, 0)
    sprite.append(0)
    align(sprite, 4)
    sprite.extend(bytes(16))
    sprite.extend(struct.pack("<q", 0))
    push_i32(sprite, 0)
    push_pptr(sprite, 0)
    push_pptr(sprite, 8)
    push_pptr(sprite, 0)
    for _ in range(3):
        push_i32(sprite, 0)
    align(sprite, 4)
    push_u32(sprite, 0)
    push_i32(sprite, 1)
    sprite.extend((0, 0, 0, 3))
    push_i32(sprite, 0)
    align(sprite, 4)
    push_i32(sprite, 0)
    push_f32s(sprite, (0.0, 0.0, 1.0, 1.0))
    push_f32s(sprite, (0.0, 0.0, 0.0, 0.0))
    push_u32(sprite, 2)
    push_f32s(sprite, (0.0, 0.0, 1.0, 1.0, 1.0))

    atlas = bytearray()
    push_aligned_string(atlas, "python atlas")
    push_i32(atlas, 1)
    push_pptr(atlas, 7)
    push_i32(atlas, 1)
    push_aligned_string(atlas, "python sprite")
    push_i32(atlas, 1)
    atlas.extend(bytes(16))
    atlas.extend(struct.pack("<q", 0))
    push_pptr(atlas, 10)
    push_pptr(atlas, 0)
    push_f32s(atlas, (0.0, 0.0, 1.0, 1.0))
    push_f32s(atlas, (0.0, 0.0, 0.0, 0.0))
    push_f32s(atlas, (0.0, 0.0, 1.0, 1.0, 1.0))
    push_u32(atlas, 2)
    push_i32(atlas, 0)
    align(atlas, 4)
    push_aligned_string(atlas, "python")
    atlas.append(0)
    align(atlas, 4)

    return finish_v22_objects(
        (
            (213, 7, sprite),
            (28, 8, texture2d_payload("resident", 1, 1, bytes((1, 2, 3, 255)))),
            (687_078_895, 9, atlas),
            (28, 10, texture2d_payload("atlas", 1, 1, bytes((9, 8, 7, 255)))),
        )
    )


def synthetic_tight_sprite() -> bytes:
    sprite = bytearray()
    push_aligned_string(sprite, "python tight sprite")
    push_f32s(sprite, (0.0, 0.0, 2.0, 2.0))
    push_f32s(sprite, (0.0, 0.0))
    push_f32s(sprite, (0.0, 0.0, 0.0, 0.0))
    push_f32s(sprite, (1.0, 0.5, 0.5))
    push_u32(sprite, 0)
    sprite.append(0)
    align(sprite, 4)
    sprite.extend(bytes(16))
    sprite.extend(struct.pack("<q", 0))
    push_i32(sprite, 0)
    push_pptr(sprite, 0)
    push_pptr(sprite, 8)
    push_pptr(sprite, 0)
    push_i32(sprite, 0)

    push_i32(sprite, 1)
    push_u32(sprite, 0)
    push_u32(sprite, 3)
    push_i32(sprite, 0)
    push_u32(sprite, 0)
    push_u32(sprite, 0)
    push_u32(sprite, 3)
    # The submesh's localAABB is six floats -- a centre and an extent -- not
    # eight. This fixture encoded eight to match a reader that skipped 32 bytes
    # here, and kept doing so after that was corrected, so every field after it
    # was misread. The managed differential is what found the reader defect; the
    # Python suite kept the old shape because nothing had run it since.
    push_f32s(sprite, (0.0,) * 6)
    push_i32(sprite, 6)
    sprite.extend(struct.pack("<HHH", 0, 1, 2))
    align(sprite, 4)
    push_u32(sprite, 3)
    push_i32(sprite, 1)
    sprite.extend((0, 0, 0, 3))
    push_i32(sprite, 36)
    push_f32s(sprite, (-1.0, -1.0, 0.0, 1.1, -1.0, 0.0, -1.0, 1.1, 0.0))
    align(sprite, 4)
    push_i32(sprite, 0)
    push_f32s(sprite, (0.0, 0.0, 2.0, 2.0))
    push_f32s(sprite, (0.0, 0.0, 0.0, 0.0))
    push_u32(sprite, 0)
    push_f32s(sprite, (0.0, 0.0, 1.0, 1.0, 1.0))

    pixels = bytes((10, 1, 1, 255, 20, 2, 2, 255, 30, 3, 3, 255, 40, 4, 4, 255))
    return finish_v22_objects(((213, 7, sprite), (28, 8, texture2d_payload("tight", 2, 2, pixels))))


def synthetic_switch_mip_chain() -> bytes:
    platform_blob = bytes(12)
    base_mip = bytes((9, 8, 7, 6)) + bytes(508)
    lower_mip_tail = bytes((0xA5,)) * 128
    payload = texture2d_payload(
        "switch-chain",
        1,
        1,
        base_mip + lower_mip_tail,
        mip_count=2,
        platform_blob=platform_blob,
    )
    return finish_v22_asset(28, payload, target_platform=38)


def synthetic_legacy_pcm() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "legacy-pcm")
    push_i32(payload, 2)
    payload.extend(struct.pack("<f", 0.0))
    push_i32(payload, 22_050)
    push_i32(payload, 4)
    payload.extend((1, 2, 3, 4))
    return finish_v22_asset(83, payload, "2.5.0f1")


def synthetic_fsb5_pcm() -> bytes:
    pcm = b"\x01\x02\x03\x04"
    fsb = bytearray(0x3C)
    fsb[:4] = b"FSB5"
    fsb[4:8] = struct.pack("<I", 1)
    fsb[8:12] = struct.pack("<I", 1)
    fsb[12:16] = struct.pack("<I", 8)
    fsb[20:24] = struct.pack("<I", len(pcm))
    fsb[24:28] = struct.pack("<I", 2)
    sample_mode = (1 << 34) | (1 << 5) | (8 << 1)
    fsb.extend(struct.pack("<Q", sample_mode))
    fsb.extend(pcm)

    payload = bytearray()
    push_aligned_string(payload, "fsb5-pcm")
    push_i32(payload, 0)
    push_i32(payload, 2)
    push_i32(payload, 44_100)
    push_i32(payload, 16)
    payload.extend(struct.pack("<f", 0.0))
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(bytes(3))
    align(payload, 4)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", len(fsb)))
    push_i32(payload, 0)
    payload.extend(fsb)
    return finish_v22_asset(83, payload)


def synthetic_fsb5_ima() -> bytes:
    block = bytearray((0x10,)) * 36
    block[:2] = struct.pack("<h", 1000)
    block[2] = 10
    block[3] = 0
    fsb = bytearray(0x3C)
    fsb[:4] = b"FSB5"
    fsb[4:8] = struct.pack("<I", 1)
    fsb[8:12] = struct.pack("<I", 1)
    fsb[12:16] = struct.pack("<I", 8)
    fsb[20:24] = struct.pack("<I", len(block))
    fsb[24:28] = struct.pack("<I", 7)
    sample_mode = (64 << 34) | (8 << 1)
    fsb.extend(struct.pack("<Q", sample_mode))
    fsb.extend(block)

    payload = bytearray()
    push_aligned_string(payload, "fsb5-ima")
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_i32(payload, 44_100)
    push_i32(payload, 16)
    payload.extend(struct.pack("<f", 0.0))
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(bytes(3))
    align(payload, 4)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", len(fsb)))
    push_i32(payload, 0)
    payload.extend(fsb)
    return finish_v22_asset(83, payload)


def synthetic_fsb5_dsp() -> bytes:
    coefficients = bytearray(0x2E)
    coefficients[:2] = struct.pack(">h", 2048)
    sample_mode = (14 << 34) | (8 << 1) | 1
    chunk_header = (7 << 25) | (len(coefficients) << 1)
    headers = struct.pack("<QI", sample_mode, chunk_header) + coefficients
    encoded = bytes((0, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12))
    fsb = bytearray(0x3C)
    fsb[:4] = b"FSB5"
    fsb[4:8] = struct.pack("<I", 1)
    fsb[8:12] = struct.pack("<I", 1)
    fsb[12:16] = struct.pack("<I", len(headers))
    fsb[20:24] = struct.pack("<I", len(encoded))
    fsb[24:28] = struct.pack("<I", 6)
    fsb.extend(headers)
    fsb.extend(encoded)

    payload = bytearray()
    push_aligned_string(payload, "fsb5-dsp")
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_i32(payload, 44_100)
    push_i32(payload, 16)
    payload.extend(struct.pack("<f", 0.0))
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(bytes(3))
    align(payload, 4)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", len(fsb)))
    push_i32(payload, 0)
    payload.extend(fsb)
    return finish_v22_asset(83, payload)


def synthetic_fsb5_vag() -> bytes:
    first = bytearray((0x21,)) * 16
    first[0] = 0x0C
    first[1] = 0
    second = bytearray((0x32,)) * 16
    second[0] = 0x0C
    second[1] = 0
    encoded = first + second
    fsb = bytearray(0x3C)
    fsb[:4] = b"FSB5"
    fsb[4:8] = struct.pack("<I", 1)
    fsb[8:12] = struct.pack("<I", 1)
    fsb[12:16] = struct.pack("<I", 8)
    fsb[20:24] = struct.pack("<I", len(encoded))
    fsb[24:28] = struct.pack("<I", 8)
    sample_mode = (56 << 34) | (8 << 1)
    fsb.extend(struct.pack("<Q", sample_mode))
    fsb.extend(encoded)

    payload = bytearray()
    push_aligned_string(payload, "fsb5-vag")
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_i32(payload, 44_100)
    push_i32(payload, 16)
    payload.extend(struct.pack("<f", 0.0))
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(bytes(3))
    align(payload, 4)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", len(fsb)))
    push_i32(payload, 0)
    payload.extend(fsb)
    return finish_v22_asset(83, payload)


def synthetic_fsb5_hevag() -> bytes:
    first = bytearray((0x21,)) * 16
    first[0] = 0x0C
    first[1] = 0
    second = bytearray((0x32,)) * 16
    second[0] = 0x0C
    second[1] = 0
    encoded = first + second
    fsb = bytearray(0x3C)
    fsb[:4] = b"FSB5"
    fsb[4:8] = struct.pack("<I", 1)
    fsb[8:12] = struct.pack("<I", 1)
    fsb[12:16] = struct.pack("<I", 8)
    fsb[20:24] = struct.pack("<I", len(encoded))
    fsb[24:28] = struct.pack("<I", 9)
    sample_mode = (56 << 34) | (8 << 1)
    fsb.extend(struct.pack("<Q", sample_mode))
    fsb.extend(encoded)

    payload = bytearray()
    push_aligned_string(payload, "fsb5-hevag")
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_i32(payload, 44_100)
    push_i32(payload, 16)
    payload.extend(struct.pack("<f", 0.0))
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(bytes(3))
    align(payload, 4)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", len(fsb)))
    push_i32(payload, 0)
    payload.extend(fsb)
    return finish_v22_asset(83, payload)


def synthetic_fsb5_fadpcm() -> bytes:
    first = bytearray((0x21,)) * 0x8C
    first[:12] = bytes(12)
    second = bytearray((0x32,)) * 0x8C
    second[:12] = bytes(12)
    encoded = first + second
    fsb = bytearray(0x3C)
    fsb[:4] = b"FSB5"
    fsb[4:8] = struct.pack("<I", 1)
    fsb[8:12] = struct.pack("<I", 1)
    fsb[12:16] = struct.pack("<I", 8)
    fsb[20:24] = struct.pack("<I", len(encoded))
    fsb[24:28] = struct.pack("<I", 16)
    sample_mode = (512 << 34) | (8 << 1)
    fsb.extend(struct.pack("<Q", sample_mode))
    fsb.extend(encoded)

    payload = bytearray()
    push_aligned_string(payload, "fsb5-fadpcm")
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_i32(payload, 44_100)
    push_i32(payload, 16)
    payload.extend(struct.pack("<f", 0.0))
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(bytes(3))
    align(payload, 4)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", len(fsb)))
    push_i32(payload, 0)
    payload.extend(fsb)
    return finish_v22_asset(83, payload)


def synthetic_fsb5_mpeg() -> bytes:
    encoded = bytearray(208)
    encoded[:4] = b"\xff\xfb\x10\xc0"
    encoded[104:108] = b"\xff\xfb\x10\xc0"
    fsb = bytearray(0x3C)
    fsb[:4] = b"FSB5"
    fsb[4:8] = struct.pack("<I", 1)
    fsb[8:12] = struct.pack("<I", 1)
    fsb[12:16] = struct.pack("<I", 8)
    fsb[20:24] = struct.pack("<I", len(encoded))
    fsb[24:28] = struct.pack("<I", 11)
    sample_mode = (2304 << 34) | (8 << 1)
    fsb.extend(struct.pack("<Q", sample_mode))
    fsb.extend(encoded)

    payload = bytearray()
    push_aligned_string(payload, "fsb5-mpeg")
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_i32(payload, 44_100)
    push_i32(payload, 16)
    payload.extend(struct.pack("<f", 0.0))
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(bytes(3))
    align(payload, 4)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", len(fsb)))
    push_i32(payload, 0)
    payload.extend(fsb)
    return finish_v22_asset(83, payload)


def synthetic_fsb5_opus() -> bytes:
    packet = bytes.fromhex(
        "f8 6f ed 8a 58 c6 40 44 64 d8 00 00 00 00 00 00 00 00 ad 43 a8"
    ) + bytes(43)
    encoded = struct.pack("<H", len(packet)) + packet + b"\0\0"
    fsb = bytearray(0x3C)
    fsb[:4] = b"FSB5"
    fsb[4:8] = struct.pack("<I", 1)
    fsb[8:12] = struct.pack("<I", 1)
    fsb[12:16] = struct.pack("<I", 8)
    fsb[20:24] = struct.pack("<I", len(encoded))
    fsb[24:28] = struct.pack("<I", 17)
    sample_mode = (648 << 34) | (9 << 1)
    fsb.extend(struct.pack("<Q", sample_mode))
    fsb.extend(encoded)

    payload = bytearray()
    push_aligned_string(payload, "fsb5-opus")
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_i32(payload, 48_000)
    push_i32(payload, 16)
    payload.extend(struct.pack("<f", 0.0))
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(bytes(3))
    align(payload, 4)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", len(fsb)))
    push_i32(payload, 0)
    payload.extend(fsb)
    return finish_v22_asset(83, payload)


def synthetic_fsb5_vorbis() -> bytes:
    fixture = (
        Path(__file__).parents[2]
        / "unity-rs-core"
        / "tests"
        / "fixtures"
        / "audio"
        / "fsb5-vorbis-stereo.fsb"
    ).read_bytes()
    payload = bytearray()
    push_aligned_string(payload, "fsb5-vorbis")
    push_i32(payload, 0)
    push_i32(payload, 2)
    push_i32(payload, 48_000)
    push_i32(payload, 16)
    payload.extend(struct.pack("<f", 0.0))
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(bytes(3))
    align(payload, 4)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", len(fixture)))
    push_i32(payload, 0)
    payload.extend(fixture)
    return finish_v22_asset(83, payload)


def synthetic_oodle_bundle() -> tuple[bytes, bytes, bytes, bytes]:
    payload = b"native-rust-payload"
    info_input = b"fake-oodle-blocks-info"
    data_input = b"fake-oodle-data"
    blocks_info = bytearray(16)
    blocks_info.extend(struct.pack(">i", 1))
    blocks_info.extend(struct.pack(">I", len(payload)))
    blocks_info.extend(struct.pack(">I", len(data_input)))
    blocks_info.extend(struct.pack(">H", 6))
    blocks_info.extend(struct.pack(">i", 1))
    blocks_info.extend(struct.pack(">q", 0))
    blocks_info.extend(struct.pack(">q", len(payload)))
    blocks_info.extend(struct.pack(">I", 0))
    blocks_info.extend(b"folder/data.bin\0")

    bundle = bytearray(b"UnityFS\0")
    bundle.extend(struct.pack(">I", 6))
    bundle.extend(b"5.x.x\0")
    bundle.extend(b"2018.4.0f1\0")
    size_offset = len(bundle)
    bundle.extend(bytes(8))
    bundle.extend(struct.pack(">I", len(info_input)))
    bundle.extend(struct.pack(">I", len(blocks_info)))
    bundle.extend(struct.pack(">I", 0x40 | 6))
    bundle.extend(info_input)
    bundle.extend(data_input)
    bundle[size_offset : size_offset + 8] = struct.pack(">q", len(bundle))
    return bytes(bundle), bytes(blocks_info), payload, data_input


def synthetic_font() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python-font")
    payload.extend(struct.pack("<f", 0.0))
    push_pptr(payload, 0)
    payload.extend(struct.pack("<f", 12.0))
    push_pptr(payload, 0)
    payload.extend(bytes(20))
    push_i32(payload, 0)
    push_i32(payload, 0)
    payload.extend(struct.pack("<f", 1.0))
    push_i32(payload, 8)
    payload.extend(b"OTTOfont")
    return finish_v22_asset(128, payload)


def synthetic_movie_texture() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python-movie")
    payload.extend(bytes(5))
    align(payload, 4)
    payload.append(1)
    align(payload, 4)
    push_pptr(payload, 0)
    push_i32(payload, 4)
    payload.extend(b"OggS")
    return finish_v22_asset(152, payload, "2018.4.36f1")


def synthetic_video_clip() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python-video")
    push_aligned_string(payload, "movies/python.mp4")
    for value in (0, 0, 1920, 1080, 1, 1):
        push_u32(payload, value)
    payload.extend(struct.pack("<d", 30.0))
    payload.extend(struct.pack("<Q", 300))
    push_i32(payload, 0)
    for _ in range(4):
        push_i32(payload, 0)
    push_aligned_string(payload, "")
    payload.extend(struct.pack("<q", 0))
    payload.extend(struct.pack("<q", 9))
    payload.extend((0, 1))
    payload.extend(b"video-bin")
    return finish_v22_asset(329, payload)


def synthetic_external_video_clip() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "external-video")
    push_aligned_string(payload, "movies/external.mp4")
    for value in (0, 0, 1920, 1080, 1, 1):
        push_u32(payload, value)
    payload.extend(struct.pack("<d", 30.0))
    payload.extend(struct.pack("<Q", 300))
    push_i32(payload, 0)
    for _ in range(4):
        push_i32(payload, 0)
    push_aligned_string(payload, "external.resS")
    payload.extend(struct.pack("<q", 2))
    payload.extend(struct.pack("<q", 9))
    payload.extend((0, 1))
    return finish_v22_asset(329, payload)


def synthetic_material() -> bytes:
    return finish_v22_asset(21, material_payload())


def material_payload(texture_path_id: int = 9) -> bytearray:
    payload = bytearray()
    push_aligned_string(payload, "python-material")
    push_i32(payload, 1)
    payload.extend(struct.pack("<q", 42))
    push_i32(payload, 2)
    push_aligned_string(payload, "FOO")
    push_aligned_string(payload, "BAR")
    push_i32(payload, 1)
    push_aligned_string(payload, "OLD")
    push_u32(payload, 3)
    payload.append(1)
    align(payload, 4)
    push_i32(payload, 2450)
    push_i32(payload, 2)
    push_aligned_string(payload, "RenderType")
    push_aligned_string(payload, "Opaque")
    push_aligned_string(payload, "RenderType")
    push_aligned_string(payload, "Cutout")
    push_i32(payload, 1)
    push_aligned_string(payload, "ShadowCaster")
    push_i32(payload, 1)
    push_aligned_string(payload, "_MainTex")
    push_pptr(payload, texture_path_id)
    push_f32s(payload, (2.0, 3.0, 0.25, 0.5))
    push_i32(payload, 2)
    for value in (1, 2):
        push_aligned_string(payload, "_Mode")
        push_i32(payload, value)
    push_i32(payload, 1)
    push_aligned_string(payload, "_Glossiness")
    push_f32s(payload, (0.75,))
    push_i32(payload, 1)
    push_aligned_string(payload, "_Color")
    push_f32s(payload, (1.0, 0.5, 0.25, 1.0))
    push_i32(payload, 0)
    return payload


def synthetic_game_object() -> bytes:
    payload = bytearray()
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_aligned_string(payload, "Python Root")
    return finish_v22_asset(1, payload)


def synthetic_build_settings() -> bytes:
    payload = bytearray()
    push_i32(payload, 2)
    push_aligned_string(payload, "Assets/Intro.unity")
    push_aligned_string(payload, "Assets/Game.unity")
    return finish_v22_asset(141, payload)


def synthetic_player_settings() -> bytes:
    payload = bytearray(bytes(16))
    payload.append(1)
    align(payload, 4)
    push_i32(payload, 1)
    push_i32(payload, 2)
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 60)
    push_aligned_string(payload, "Haruki")
    push_aligned_string(payload, "Asset Studio")
    payload.extend(b"future tail")
    return finish_v22_asset(129, payload)


def synthetic_acl_tracks() -> bytes:
    tracks = bytearray()
    push_u32(tracks, 32)
    push_u32(tracks, 0)
    push_u32(tracks, 0xAC11AC11)
    tracks.extend(struct.pack("<H", 10))
    tracks.extend((0, 12))
    push_u32(tracks, 3)
    push_u32(tracks, 12)
    push_f32s(tracks, (30.0,))
    push_u32(tracks, 0)
    hash_value = 2166136261
    for value in tracks[8:]:
        hash_value = ((hash_value ^ value) * 16777619) & 0xFFFFFFFF
    tracks[4:8] = struct.pack("<I", hash_value)
    return bytes(tracks)


def synthetic_standard_animation_clip() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python-standard-animation")
    payload.extend((0, 0, 0))
    align(payload, 4)
    for _ in range(7):
        push_i32(payload, 0)
    push_f32s(payload, (60.0,))
    push_i32(payload, 2)
    push_f32s(payload, (0.0,) * 6)
    push_u32(payload, 0)

    push_tuanjie_animation_xform(payload)
    push_f32s(payload, (0.0,) * 7)
    push_i32(payload, 0)
    for _ in range(2):
        push_tuanjie_animation_xform(payload)
        push_i32(payload, 0)
        push_f32s(payload, (0.0,) * 4)
    push_i32(payload, 0)
    push_i32(payload, 0)
    for _ in range(4):
        push_tuanjie_animation_xform(payload)
    push_f32s(payload, (0.0,) * 3)
    push_i32(payload, 0)
    payload.extend(struct.pack("<HH", 2, 1))
    push_i32(payload, 0)
    push_u32(payload, 0)
    push_f32s(payload, (30.0, 0.0))
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_f32s(payload, (0.0,) * 6)
    for _ in range(3):
        push_i32(payload, 0)
    payload.extend(bytes(11))
    align(payload, 4)

    push_i32(payload, 0)  # generic bindings
    push_i32(payload, 0)  # PPtr curve mapping
    payload.extend((1, 0))
    align(payload, 4)
    push_i32(payload, 0)  # events
    align(payload, 4)
    return finish_v22_asset(74, payload, "6000.2.0f1")


def synthetic_tuanjie_animation_clip(*, cubism_binding: bool = False) -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python-tuanjie-animation")
    payload.extend((0, 0, 0))
    align(payload, 4)
    for _ in range(4):
        push_i32(payload, 0)
    push_f32s(payload, (60.0,))
    push_i32(payload, 2)
    push_f32s(payload, (0.0,) * 6)
    for _ in range(3):
        push_i32(payload, 0)
    push_u32(payload, 0)

    push_tuanjie_animation_xform(payload)
    push_f32s(payload, (0.0,) * 7)
    push_i32(payload, 0)
    for _ in range(2):
        push_tuanjie_animation_xform(payload)
        push_i32(payload, 0)
        push_f32s(payload, (0.0,) * 4)
    push_i32(payload, 0)
    push_i32(payload, 0)
    for _ in range(4):
        push_tuanjie_animation_xform(payload)
    push_f32s(payload, (0.0,) * 3)
    push_i32(payload, 0)
    payload.extend(struct.pack("<HH", 2, 1))
    push_i32(payload, 0)
    push_u32(payload, 0)
    push_f32s(payload, (30.0, 0.0))
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_u32(payload, 12)
    push_u32(payload, 3)
    push_f32s(payload, (30.0,))
    push_u32(payload, 1 if cubism_binding else 7)
    acl_tracks = synthetic_acl_tracks()
    push_i32(payload, len(acl_tracks))
    payload.extend(acl_tracks)
    align(payload, 4)
    push_i32(payload, 2)
    push_u32(payload, 0x10)
    push_u32(payload, 0x20)
    payload.append(1)
    align(payload, 4)
    push_f32s(payload, (0.0,) * 6)
    for _ in range(3):
        push_i32(payload, 0)
    payload.extend(bytes(11))
    align(payload, 4)
    payload.extend(struct.pack("<q", 0x1020304050607080))
    push_u32(payload, 0x1234)
    push_aligned_string(payload, "archive:/animation.resS")
    push_i32(payload, 1 if cubism_binding else 0)
    if cubism_binding:
        # Unity stores the standard CRC32 of the binding path. One scalar
        # binding is enough to make the ACL callback reach the motion writer.
        push_u32(payload, zlib.crc32(b"Parameters/ParamAngleX") & 0xFFFFFFFF)
        push_u32(payload, 0)  # attribute
        push_pptr(payload, 0)  # script
        push_i32(payload, 0)  # type ID
        payload.extend((0, 0, 0, 0))
        align(payload, 4)
    push_i32(payload, 0)
    payload.extend((0, 0))
    align(payload, 4)
    push_i32(payload, 0)
    align(payload, 4)
    return finish_v22_asset(74, payload, "2022.3.61t1")


def synthetic_legacy_animation_component() -> bytes:
    payload = bytearray()
    push_pptr(payload, 31)  # GameObject
    payload.append(1)  # enabled
    align(payload, 4)
    push_pptr(payload, 70)  # default clip
    push_i32(payload, 2)
    push_pptr(payload, 71)
    push_pptr(payload, 72)
    payload.extend(b"\xaa\xbb")  # unparsed version-dependent tail
    return finish_v22_asset(111, payload)


def synthetic_animator_override_controller() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python override controller")
    push_pptr(payload, 90)  # base AnimatorController
    push_i32(payload, 2)
    push_pptr(payload, 71)
    push_pptr(payload, 73)
    push_pptr(payload, 72)
    push_pptr(payload, 74)
    payload.append(0xCC)  # unparsed version-dependent tail
    return finish_v22_asset(221, payload)


def synthetic_container_metadata_objects() -> bytes:
    asset_bundle = bytearray()
    push_aligned_string(asset_bundle, "root")
    push_i32(asset_bundle, 2)
    push_pptr(asset_bundle, 11)
    push_pptr(asset_bundle, 12)
    push_i32(asset_bundle, 2)
    for key, preload_index, asset_path_id in (
        ("bundle/first", 0, 11),
        ("bundle/second", 1, 12),
    ):
        push_aligned_string(asset_bundle, key)
        push_i32(asset_bundle, preload_index)
        push_i32(asset_bundle, 1)
        push_pptr(asset_bundle, asset_path_id)
    push_i32(asset_bundle, 0)  # main asset preload index
    push_i32(asset_bundle, 0)  # main asset preload size
    push_pptr(asset_bundle, 0)
    push_u32(asset_bundle, 0)  # runtime compatibility
    push_aligned_string(asset_bundle, "python-bundle")
    push_i32(asset_bundle, 2)
    push_aligned_string(asset_bundle, "shared-a")
    push_aligned_string(asset_bundle, "shared-b")
    asset_bundle.append(0)  # ordinary AssetBundle; preload ranges use its local table

    resource_manager = bytearray()
    push_i32(resource_manager, 2)
    push_aligned_string(resource_manager, "resource/first")
    push_pptr(resource_manager, 21)
    push_aligned_string(resource_manager, "resource/second")
    push_pptr(resource_manager, 22)

    preload_data = bytearray()
    push_aligned_string(preload_data, "python-preload")
    push_i32(preload_data, 2)
    push_pptr(preload_data, 31)
    push_pptr(preload_data, 32)

    return finish_v22_objects(
        (
            (142, 7, asset_bundle),
            (147, 8, resource_manager),
            (150, 9, preload_data),
        )
    )


def push_tuanjie_animation_xform(output: bytearray) -> None:
    push_f32s(output, (0.0,) * 10)


def synthetic_tuanjie_animator_controller() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python-tuanjie-controller")
    push_u32(payload, 0)
    for _ in range(9):
        push_i32(payload, 0)
    push_i32(payload, 1)
    push_u32(payload, 0xDEADBEEF)
    push_aligned_string(payload, "Root/Hips")
    push_i32(payload, 1)
    push_i32(payload, 0)
    payload.extend(struct.pack("<q", 74))
    # The state-machine-behaviour tail every real controller ends with: two
    # empty collections, an empty behaviour vector, and the threading flag.
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 0)
    payload.append(1)
    align(payload, 4)
    return finish_v22_asset(91, payload, "2022.3.55t4")


def synthetic_tuanjie_avatar() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python-tuanjie-avatar")
    push_u32(payload, 0)
    push_empty_avatar_skeleton(payload)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_tuanjie_animation_xform(payload)
    push_empty_avatar_skeleton(payload)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_f32s(payload, (1.0, 0.5, 0.5, 0.5, 0.5, 0.05, 0.05, 0.0))
    payload.extend((0, 0, 0))
    align(payload, 4)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, -1)
    push_tuanjie_animation_xform(payload)
    push_empty_avatar_skeleton(payload)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_u32(payload, 0xFEEDBEEF)
    push_aligned_string(payload, "Root/Hips")
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_f32s(payload, (0.5, 0.5, 0.5, 0.5, 0.05, 0.05, 0.0, 1.0))
    push_aligned_string(payload, "Hips")
    payload.extend((1, 0, 1))
    align(payload, 4)
    return finish_v22_asset(90, payload, "2022.3.55t4")


def push_empty_avatar_skeleton(output: bytearray) -> None:
    push_i32(output, 0)
    push_i32(output, 0)
    push_i32(output, 0)


def stripped_mono_schema_nodes() -> list[tuple[str, str, int, bool]]:
    return list(mono_behaviour_nodes()) + [("SInt32", "score", 1, False)]


def synthetic_stripped_mono_behaviour() -> bytes:
    behaviour = bytearray()
    push_pptr(behaviour, 0)
    behaviour.append(1)
    align(behaviour, 4)
    push_pptr(behaviour, 8)
    push_aligned_string(behaviour, "Hero")
    push_i32(behaviour, 123)

    script = bytearray()
    push_aligned_string(script, "Stats script")
    push_i32(script, 0)
    script.extend(bytes(16))
    push_aligned_string(script, "Stats")
    push_aligned_string(script, "Game")
    push_aligned_string(script, "Assembly-CSharp.dll")
    return finish_v22_objects(((114, 7, behaviour), (115, 8, script)))


def mono_behaviour_nodes() -> tuple[tuple[str, str, int, bool], ...]:
    return (
        ("MonoBehaviour", "Base", 0, False),
        ("PPtr<GameObject>", "m_GameObject", 1, False),
        ("int", "m_FileID", 2, False),
        ("SInt64", "m_PathID", 2, False),
        ("UInt8", "m_Enabled", 1, True),
        ("PPtr<MonoScript>", "m_Script", 1, False),
        ("int", "m_FileID", 2, False),
        ("SInt64", "m_PathID", 2, False),
        ("string", "m_Name", 1, False),
        ("Array", "Array", 2, True),
        ("int", "size", 3, False),
        ("char", "data", 3, False),
    )


def synthetic_cubism_expression() -> bytes:
    nodes = mono_behaviour_nodes() + (
        ("string", "Type", 1, False),
        ("Array", "Array", 2, True),
        ("int", "size", 3, False),
        ("char", "data", 3, False),
        ("float", "FadeInTime", 1, False),
        ("float", "FadeOutTime", 1, False),
        ("vector", "Parameters", 1, False),
        ("Array", "Array", 2, True),
        ("int", "size", 3, False),
        ("SerializableExpressionParameter", "data", 3, False),
        ("string", "Id", 4, False),
        ("Array", "Array", 5, True),
        ("int", "size", 6, False),
        ("char", "data", 6, False),
        ("float", "Value", 4, False),
        ("int", "Blend", 4, False),
    )
    payload = bytearray()
    push_pptr(payload, 0)
    payload.append(1)
    align(payload, 4)
    push_pptr(payload, 0)
    push_aligned_string(payload, "smile.exp3")
    push_aligned_string(payload, "Live2D Expression")
    push_f32s(payload, (0.5, 0.75))
    push_i32(payload, 1)
    push_aligned_string(payload, "ParamAngleX")
    push_f32s(payload, (0.25,))
    push_i32(payload, 1)
    align(payload, 4)
    return finish_v22_type_tree_mono(nodes, payload)


def synthetic_cubism_pose_part() -> bytes:
    nodes = mono_behaviour_nodes() + (
        ("int", "GroupIndex", 1, False),
        ("vector", "Link", 1, False),
        ("Array", "Array", 2, True),
        ("int", "size", 3, False),
        ("string", "data", 3, False),
        ("Array", "Array", 4, True),
        ("int", "size", 5, False),
        ("char", "data", 5, False),
    )
    payload = bytearray()
    push_pptr(payload, 0)
    payload.append(1)
    align(payload, 4)
    push_pptr(payload, 0)
    push_aligned_string(payload, "pose")
    push_i32(payload, 2)
    push_i32(payload, 2)
    push_aligned_string(payload, "PartArmL")
    push_aligned_string(payload, "PartArmR")
    align(payload, 4)
    return finish_v22_type_tree_mono(nodes, payload)


def synthetic_cubism_display_info() -> bytes:
    nodes = mono_behaviour_nodes() + (
        ("string", "Name", 1, False),
        ("Array", "Array", 2, True),
        ("int", "size", 3, False),
        ("char", "data", 3, False),
        ("string", "DisplayName", 1, False),
        ("Array", "Array", 2, True),
        ("int", "size", 3, False),
        ("char", "data", 3, False),
    )
    payload = bytearray()
    push_pptr(payload, 0)
    payload.append(1)
    align(payload, 4)
    push_pptr(payload, 0)
    push_aligned_string(payload, "display")
    push_aligned_string(payload, "Angle X")
    push_aligned_string(payload, "Face Angle")
    return finish_v22_type_tree_mono(nodes, payload)


def synthetic_cubism_physics() -> bytes:
    nodes = mono_behaviour_nodes() + (
        ("CubismPhysicsRig", "_rig", 1, False),
        ("vector", "SubRigs", 2, False),
        ("Array", "Array", 3, True),
        ("int", "size", 4, False),
        ("CubismPhysicsSubRig", "data", 4, False),
        ("vector", "Input", 5, False),
        ("Array", "Array", 6, True),
        ("int", "size", 7, False),
        ("CubismPhysicsInput", "data", 7, False),
        ("string", "SourceId", 8, False),
        ("Array", "Array", 9, True),
        ("int", "size", 10, False),
        ("char", "data", 10, False),
        ("float", "Weight", 8, False),
        ("int", "SourceComponent", 8, False),
        ("bool", "IsInverted", 8, True),
        ("vector", "Output", 5, False),
        ("Array", "Array", 6, True),
        ("int", "size", 7, False),
        ("CubismPhysicsOutput", "data", 7, False),
        ("string", "DestinationId", 8, False),
        ("Array", "Array", 9, True),
        ("int", "size", 10, False),
        ("char", "data", 10, False),
        ("int", "ParticleIndex", 8, False),
        ("float", "AngleScale", 8, False),
        ("float", "Weight", 8, False),
        ("int", "SourceComponent", 8, False),
        ("bool", "IsInverted", 8, True),
        ("vector", "Particles", 5, False),
        ("Array", "Array", 6, True),
        ("int", "size", 7, False),
        ("CubismPhysicsParticle", "data", 7, False),
        ("Vector2", "InitialPosition", 8, False),
        ("float", "X", 9, False),
        ("float", "Y", 9, False),
        ("float", "Mobility", 8, False),
        ("float", "Delay", 8, False),
        ("float", "Acceleration", 8, False),
        ("float", "Radius", 8, False),
        ("CubismPhysicsNormalization", "Normalization", 5, False),
        ("CubismPhysicsNormalizationTuplet", "Position", 6, False),
        ("float", "Maximum", 7, False),
        ("float", "Minimum", 7, False),
        ("float", "Default", 7, False),
        ("CubismPhysicsNormalizationTuplet", "Angle", 6, False),
        ("float", "Maximum", 7, False),
        ("float", "Minimum", 7, False),
        ("float", "Default", 7, False),
        ("Vector2", "Gravity", 2, False),
        ("float", "X", 3, False),
        ("float", "Y", 3, False),
        ("Vector2", "Wind", 2, False),
        ("float", "X", 3, False),
        ("float", "Y", 3, False),
        ("float", "Fps", 2, False),
    )
    payload = bytearray()
    push_pptr(payload, 0)
    payload.append(1)
    align(payload, 4)
    push_pptr(payload, 0)
    push_aligned_string(payload, "physics")
    push_i32(payload, 1)
    push_i32(payload, 1)
    push_aligned_string(payload, "ParamAngleX")
    push_f32s(payload, (80.0,))
    push_i32(payload, 0)
    payload.append(0)
    align(payload, 4)
    push_i32(payload, 1)
    push_aligned_string(payload, "ParamHair")
    push_i32(payload, 1)
    push_f32s(payload, (2.5, 90.0))
    push_i32(payload, 2)
    payload.append(1)
    align(payload, 4)
    push_i32(payload, 1)
    push_f32s(
        payload,
        (
            0.0,
            1.0,
            0.8,
            0.2,
            1.0,
            10.0,
            10.0,
            -10.0,
            0.0,
            30.0,
            -30.0,
            0.0,
            0.0,
            -1.0,
            0.5,
            0.0,
            0.0,
        ),
    )
    return finish_v22_type_tree_mono(nodes, payload)


def synthetic_cubism_fade_motion() -> bytes:
    nodes = mono_behaviour_nodes() + (
        ("string", "MotionName", 1, False),
        ("Array", "Array", 2, True),
        ("int", "size", 3, False),
        ("char", "data", 3, False),
        ("float", "FadeInTime", 1, False),
        ("float", "FadeOutTime", 1, False),
        ("vector", "ParameterIds", 1, False),
        ("Array", "Array", 2, False),
        ("int", "size", 3, False),
        ("string", "data", 3, False),
        ("Array", "Array", 4, True),
        ("int", "size", 5, False),
        ("char", "data", 5, False),
        ("vector", "ParameterCurves", 1, False),
        ("Array", "Array", 2, False),
        ("int", "size", 3, False),
        ("AnimationCurve", "data", 3, False),
        ("vector", "m_Curve", 4, False),
        ("Array", "Array", 5, False),
        ("int", "size", 6, False),
        ("Keyframe", "data", 6, False),
        ("float", "time", 7, False),
        ("float", "value", 7, False),
        ("float", "inSlope", 7, False),
        ("float", "outSlope", 7, False),
        ("int", "weightedMode", 7, False),
        ("float", "inWeight", 7, False),
        ("float", "outWeight", 7, False),
        ("int", "m_PreInfinity", 4, False),
        ("int", "m_PostInfinity", 4, False),
        ("int", "m_RotationOrder", 4, False),
        ("vector", "ParameterFadeInTimes", 1, False),
        ("Array", "Array", 2, False),
        ("int", "size", 3, False),
        ("float", "data", 3, False),
        ("vector", "ParameterFadeOutTimes", 1, False),
        ("Array", "Array", 2, False),
        ("int", "size", 3, False),
        ("float", "data", 3, False),
        ("float", "MotionLength", 1, False),
    )
    payload = bytearray()
    push_pptr(payload, 0)
    payload.append(1)
    align(payload, 4)
    push_pptr(payload, 0)
    push_aligned_string(payload, "idle.fade.asset")
    push_aligned_string(payload, "idle")
    push_f32s(payload, (0.2, 0.3))
    push_i32(payload, 1)
    push_aligned_string(payload, "ParamAngleX")
    push_i32(payload, 1)
    push_i32(payload, 2)
    for keyframe in ((0.0, 0.0, 0.0, 0.0), (1.0, 1.0, 0.0, 0.0)):
        push_f32s(payload, keyframe)
        push_i32(payload, 0)
        push_f32s(payload, (0.0, 0.0))
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 0)
    push_i32(payload, 1)
    push_f32s(payload, (0.4,))
    push_i32(payload, 1)
    push_f32s(payload, (0.5, 1.0))
    return finish_v22_type_tree_mono(nodes, payload)


def finish_v22_type_tree_mono(
    nodes: tuple[tuple[str, str, int, bool], ...], payload: bytearray
) -> bytes:

    metadata = bytearray(b"2022.3.62f1\0")
    push_i32(metadata, 13)
    metadata.append(1)
    push_i32(metadata, 1)
    push_i32(metadata, 114)
    metadata.append(0)
    metadata.extend(struct.pack("<h", -1))
    metadata.extend(bytes((0x20,)) * 16)
    metadata.extend(bytes((0x42,)) * 16)
    push_blob_tree(metadata, nodes)
    push_i32(metadata, 0)
    push_i32(metadata, 1)
    align_with_base(metadata, 48, 4)
    metadata.extend(struct.pack("<q", 7))
    metadata.extend(struct.pack("<q", 0))
    push_u32(metadata, len(payload))
    push_i32(metadata, 0)
    for _ in range(3):
        push_i32(metadata, 0)
    metadata.append(0)
    return finish_v22(metadata, payload)


def push_blob_tree(
    output: bytearray, nodes: tuple[tuple[str, str, int, bool], ...]
) -> None:
    strings = bytearray()
    offsets: list[tuple[int, int]] = []
    for type_name, field_name, _, _ in nodes:
        type_offset = len(strings)
        strings.extend(type_name.encode("utf-8"))
        strings.append(0)
        name_offset = len(strings)
        strings.extend(field_name.encode("utf-8"))
        strings.append(0)
        offsets.append((type_offset, name_offset))
    push_i32(output, len(nodes))
    push_i32(output, len(strings))
    for index, ((_, _, level, aligned), (type_offset, name_offset)) in enumerate(
        zip(nodes, offsets)
    ):
        output.extend(struct.pack("<H", 1))
        output.extend((level, 0))
        push_u32(output, type_offset)
        push_u32(output, name_offset)
        push_i32(output, -1)
        push_i32(output, index)
        push_i32(output, 0x4000 if aligned else 0)
        output.extend(bytes(8))
    output.extend(strings)


def synthetic_static_model(*, tuanjie: bool = False) -> bytes:
    return finish_v22_objects(
        (
            (1, 1, model_game_object(tuanjie=tuanjie)),
            (4, 11, model_transform()),
            (33, 21, model_mesh_filter()),
            (23, 31, model_renderer(tuanjie=tuanjie)),
            (43, 51, model_mesh(tuanjie=tuanjie)),
        ),
        "2022.3.61t2" if tuanjie else "2022.3.62f1",
    )


def synthetic_textured_model() -> bytes:
    return finish_v22_objects(
        (
            (1, 1, model_game_object()),
            (4, 11, model_transform()),
            (33, 21, model_mesh_filter()),
            (23, 31, model_renderer(material_path_id=41)),
            (21, 41, material_payload(61)),
            (43, 51, model_mesh()),
            (
                28,
                61,
                texture2d_payload(
                    "python model texture", 1, 1, bytes((9, 8, 7, 255))
                ),
            ),
        )
    )


def push_pptr(output: bytearray, path_id: int) -> None:
    push_i32(output, 0)
    output.extend(struct.pack("<q", path_id))


def model_game_object(*, tuanjie: bool = False) -> bytearray:
    output = bytearray()
    push_i32(output, 3)
    for component in (11, 21, 31):
        push_pptr(output, component)
    push_i32(output, 0)
    if tuanjie:
        output.append(0)
        align(output, 4)
    push_aligned_string(output, "python model")
    return output


def model_transform() -> bytearray:
    output = bytearray()
    push_pptr(output, 1)
    push_f32s(output, (0.0, 0.0, 0.38268343, 0.9238795))
    push_f32s(output, (2.0, 3.0, 4.0))
    push_f32s(output, (2.0, 3.0, 4.0))
    push_i32(output, 0)
    push_pptr(output, 0)
    return output


def model_mesh_filter() -> bytearray:
    output = bytearray()
    push_pptr(output, 1)
    push_pptr(output, 51)
    return output


def model_renderer(*, tuanjie: bool = False, material_path_id: int = 0) -> bytearray:
    output = bytearray()
    push_pptr(output, 1)
    output.extend((1, 2, 1, 0, 0, 0, 0, 0, 0, 0))
    if tuanjie:
        output.extend((7, 8))
        align(output, 4)
        output.extend((9, 10))
    align(output, 4)
    push_u32(output, 0xFFFFFFFF)
    push_i32(output, 0)
    output.extend(bytes(36))
    push_i32(output, int(material_path_id != 0))
    if material_path_id != 0:
        push_pptr(output, material_path_id)
    output.extend(bytes(4))
    for _ in range(3):
        push_pptr(output, 0)
    output.extend(bytes(8))
    align(output, 4)
    return output


def model_mesh(*, tuanjie: bool = False) -> bytearray:
    output = bytearray()
    push_aligned_string(output, "python triangle")
    push_i32(output, 1)
    for value in (0, 3, 0, 0, 0, 3):
        push_u32(output, value)
    output.extend(bytes(24))
    for _ in range(3):
        push_i32(output, 0)
    push_u32(output, 0)
    for _ in range(5):
        push_i32(output, 0)
    output.extend((0, 1, 0, 0))
    if tuanjie:
        align(output, 4)
        push_tuanjie_mesh_cluster(output)
    align(output, 4)
    push_i32(output, 0)
    push_i32(output, 6)
    for index in range(3):
        output.extend(struct.pack("<H", index))
    align(output, 4)
    push_mesh_vertex_data(output)
    push_mesh_tail(output)
    if tuanjie:
        output.extend((1, 0))
    return output


def push_mesh_vertex_data(output: bytearray) -> None:
    push_u32(output, 3)
    push_i32(output, 5)
    output.extend((0, 0, 0, 3))
    output.extend(bytes(16))
    push_i32(output, 36)
    for vertex in ((0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (0.0, 1.0, 0.0)):
        push_f32s(output, vertex)
    align(output, 4)


def push_mesh_tail(output: bytearray) -> None:
    for _ in range(4):
        push_empty_packed_float(output)
    for _ in range(3):
        push_empty_packed_int(output)
    push_empty_packed_float(output)
    for _ in range(2):
        push_empty_packed_int(output)
    push_u32(output, 0)
    output.extend(bytes(24))
    for _ in range(3):
        push_i32(output, 0)
    align(output, 4)
    push_i32(output, 0)
    align(output, 4)
    output.extend(bytes(8))
    align(output, 4)
    output.extend(struct.pack("<q", 0))
    push_u32(output, 0)
    push_aligned_string(output, "")


def push_empty_packed_float(output: bytearray) -> None:
    push_u32(output, 0)
    push_f32s(output, (0.0, 0.0))
    push_i32(output, 0)
    align(output, 4)
    output.append(0)
    align(output, 4)


def push_empty_packed_int(output: bytearray) -> None:
    push_u32(output, 0)
    push_i32(output, 0)
    align(output, 4)
    output.append(0)
    align(output, 4)


def finish_v22_objects(
    objects: tuple[tuple[int, int, bytearray], ...],
    unity_version: str = "2022.3.62f1",
) -> bytes:
    classes = sorted({class_id for class_id, _, _ in objects})
    metadata = bytearray(unity_version.encode("ascii") + b"\0")
    push_i32(metadata, 13)
    metadata.append(0)
    push_i32(metadata, len(classes))
    for class_id in classes:
        push_i32(metadata, class_id)
        metadata.append(0)
        metadata.extend(struct.pack("<h", -1))
        if class_id == 114:
            metadata.extend(bytes(16))
        metadata.extend(bytes(16))

    data = bytearray()
    records: list[tuple[int, int, int, int]] = []
    for class_id, path_id, payload in objects:
        align(data, 4)
        records.append((path_id, len(data), len(payload), classes.index(class_id)))
        data.extend(payload)
    push_i32(metadata, len(records))
    for path_id, offset, size, type_index in records:
        align_with_base(metadata, 48, 4)
        metadata.extend(struct.pack("<q", path_id))
        metadata.extend(struct.pack("<q", offset))
        push_u32(metadata, size)
        push_i32(metadata, type_index)
    for _ in range(3):
        push_i32(metadata, 0)
    metadata.append(0)
    return finish_v22(metadata, data)


def finish_v22(metadata: bytearray, data: bytearray) -> bytes:
    metadata_size = len(metadata)
    data_offset = ((48 + metadata_size + 15) // 16) * 16
    file_size = data_offset + len(data)
    output = bytearray(48)
    output[8:12] = struct.pack(">I", 22)
    output[20:24] = struct.pack(">I", metadata_size)
    output[24:32] = struct.pack(">q", file_size)
    output[32:40] = struct.pack(">q", data_offset)
    output.extend(metadata)
    output.extend(bytes(data_offset - len(output)))
    output.extend(data)
    return bytes(output)


# Meta flag for "align the stream to four bytes after this field".
ALIGN = 0x4000


class TreeBuilder:
    """Accumulates TypeTree nodes in serialized (depth-first) order."""

    def __init__(self) -> None:
        self.nodes: list[dict[str, int | str]] = []

    def push(
        self,
        type_name: str,
        field: str,
        byte_size: int,
        level: int,
        *,
        is_array: int = 0,
        flags: int = 0,
    ) -> None:
        self.nodes.append(
            {
                "type": type_name,
                "name": field,
                "byte_size": byte_size,
                "index": len(self.nodes),
                "is_array": is_array,
                "version": 1,
                "meta_flags": flags,
                "level": level,
            }
        )

    def string(self, field: str, level: int) -> None:
        self.push("string", field, -1, level, flags=ALIGN)
        self.push("Array", "Array", -1, level + 1, is_array=1, flags=ALIGN)
        self.push("int", "size", 4, level + 2)
        self.push("char", "data", 1, level + 2)

    def vector(self, element: str, field: str, level: int) -> None:
        self.push("vector", field, -1, level)
        self.push("Array", "Array", -1, level + 1, is_array=1, flags=ALIGN)
        self.push("int", "size", 4, level + 2)
        self.push(element, "data", -1, level + 2)


def type_tree_probe_tree() -> list[dict[str, int | str]]:
    """A tree covering the shapes a reader can get wrong.

    Alignment after a one-byte field, a length-prefixed string, an array of
    primitives, and an array of structs that themselves contain a string and a
    byte -- the last being where a misplaced align shows up as garbage rather
    than as a wrong number.
    """
    tree = TreeBuilder()
    tree.push("MonoBehaviour", "Base", -1, 0)
    tree.push("PPtr<GameObject>", "m_GameObject", 12, 1)
    tree.push("int", "m_FileID", 4, 2)
    tree.push("SInt64", "m_PathID", 8, 2)
    tree.push("UInt8", "m_Enabled", 1, 1, flags=ALIGN)
    tree.push("PPtr<MonoScript>", "m_Script", 12, 1)
    tree.push("int", "m_FileID", 4, 2)
    tree.push("SInt64", "m_PathID", 8, 2)
    tree.string("m_Name", 1)

    tree.push("SInt8", "Signed8", 1, 1, flags=ALIGN)
    tree.push("UInt16", "Unsigned16", 2, 1, flags=ALIGN)
    tree.push("SInt64", "Signed64", 8, 1)
    tree.push("float", "Single", 4, 1)
    tree.push("double", "Double", 8, 1)
    tree.push("bool", "Flag", 1, 1, flags=ALIGN)
    tree.vector("int", "Numbers", 1)
    tree.string("Label", 1)
    tree.vector("Entry", "Entries", 1)
    tree.string("Id", 4)
    tree.push("bool", "Enabled", 1, 4, flags=ALIGN)
    tree.push("float", "Weight", 4, 4)
    return tree.nodes


def synthetic_type_tree_object() -> bytes:
    """A MonoBehaviour carrying the tree above and the bytes it describes."""
    payload = bytearray()
    push_i32(payload, 0)
    payload.extend(struct.pack("<q", 0))
    payload.append(1)
    align(payload, 4)
    push_i32(payload, 0)
    payload.extend(struct.pack("<q", 0))
    push_aligned_string(payload, "tree-probe")

    payload.extend(struct.pack("<b", -7))
    align(payload, 4)
    payload.extend(struct.pack("<H", 65000))
    align(payload, 4)
    payload.extend(struct.pack("<q", -1234567890123))
    payload.extend(struct.pack("<f", 0.8))
    payload.extend(struct.pack("<d", -0.1))
    payload.append(1)
    align(payload, 4)
    numbers = (5, -6, 7)
    push_i32(payload, len(numbers))
    for number in numbers:
        push_i32(payload, number)
    push_aligned_string(payload, "label with spaces")
    entries = (("first", 1, 0.5), ("second", 0, -2.25))
    push_i32(payload, len(entries))
    for identifier, enabled, weight in entries:
        push_aligned_string(payload, identifier)
        payload.append(enabled)
        align(payload, 4)
        payload.extend(struct.pack("<f", weight))

    return finish_v22_tree_asset(114, type_tree_probe_tree(), payload)


def synthetic_unitypy_type_tree_shapes() -> bytes:
    """A tree that distinguishes UnityPy values from the JSON projection."""
    tree = TreeBuilder()
    tree.push("UnityPyShapeProbe", "Base", -1, 0)
    tree.push("char", "Character", 2, 1)
    tree.push("TypelessData", "Blob", -1, 1)
    tree.push("int", "size", 4, 2)
    tree.push("UInt8", "data", 1, 2)
    tree.push("map", "Pairs", -1, 1)
    tree.push("Array", "Array", -1, 2, is_array=1)
    tree.push("int", "size", 4, 3)
    tree.push("pair", "data", -1, 3)
    tree.push("int", "first", 4, 4)
    tree.push("UInt8", "second", 1, 4)

    payload = bytearray(struct.pack("<H", 0x263A))
    payload.extend(struct.pack("<i", 3))
    payload.extend(b"\x00\xff\x7f")
    payload.extend(struct.pack("<i", 2))
    payload.extend(struct.pack("<iB", 1, 2))
    payload.extend(struct.pack("<iB", 3, 4))
    return finish_v22_tree_asset(9999, tree.nodes, payload)


def push_blob_type_tree(metadata: bytearray, nodes: list[dict[str, int | str]]) -> None:
    """The format 19+ blob encoding: 32-byte nodes, then the string buffer."""
    buffer = bytearray()
    offsets = []
    for node in nodes:
        type_offset = len(buffer)
        buffer.extend(str(node["type"]).encode("ascii") + b"\0")
        name_offset = len(buffer)
        buffer.extend(str(node["name"]).encode("ascii") + b"\0")
        offsets.append((type_offset, name_offset))

    push_i32(metadata, len(nodes))
    push_i32(metadata, len(buffer))
    for node, (type_offset, name_offset) in zip(nodes, offsets):
        metadata.extend(struct.pack("<H", int(node["version"])))
        metadata.append(int(node["level"]))
        metadata.append(int(node["is_array"]))
        push_u32(metadata, type_offset)
        push_u32(metadata, name_offset)
        push_i32(metadata, int(node["byte_size"]))
        push_i32(metadata, int(node["index"]))
        push_i32(metadata, int(node["meta_flags"]))
        metadata.extend(bytes(8))  # reference type hash
    metadata.extend(buffer)


def finish_v22_tree_asset(
    class_id: int,
    nodes: list[dict[str, int | str]],
    payload: bytearray,
    unity_version: str = "2022.3.62f1",
) -> bytes:
    """The same file as `finish_v22_asset`, with the TypeTree embedded."""
    metadata = bytearray(unity_version.encode("ascii") + b"\0")
    push_i32(metadata, 13)
    metadata.append(1)  # the tree is enabled
    push_i32(metadata, 1)
    push_i32(metadata, class_id)
    metadata.append(0)
    metadata.extend(struct.pack("<h", 0))
    if class_id == 114:
        # A MonoBehaviour type record carries the script hash first.
        metadata.extend(bytes(16))
    metadata.extend(bytes(16))
    push_blob_type_tree(metadata, nodes)
    push_i32(metadata, 0)  # no type dependencies

    push_i32(metadata, 1)
    align_with_base(metadata, 48, 4)
    metadata.extend(struct.pack("<q", 7))
    metadata.extend(struct.pack("<q", 0))
    metadata.extend(struct.pack("<I", len(payload)))
    push_i32(metadata, 0)
    for _ in range(3):
        push_i32(metadata, 0)
    metadata.append(0)

    metadata_size = len(metadata)
    data_offset = ((48 + metadata_size + 15) // 16) * 16
    file_size = data_offset + len(payload)
    output = bytearray(48)
    output[8:12] = struct.pack(">I", 22)
    output[20:24] = struct.pack(">I", metadata_size)
    output[24:32] = struct.pack(">q", file_size)
    output[32:40] = struct.pack(">q", data_offset)
    output.extend(metadata)
    output.extend(bytes(data_offset - len(output)))
    output.extend(payload)
    return bytes(output)


def finish_v22_asset(
    class_id: int,
    payload: bytearray,
    unity_version: str = "2022.3.62f1",
    target_platform: int = 13,
    external_path: Optional[str] = None,
) -> bytes:

    metadata = bytearray(unity_version.encode("ascii") + b"\0")
    push_i32(metadata, target_platform)
    metadata.append(0)
    push_i32(metadata, 1)
    push_i32(metadata, class_id)
    metadata.append(0)
    metadata.extend(struct.pack("<h", -1))
    metadata.extend(bytes(16))
    push_i32(metadata, 1)
    align_with_base(metadata, 48, 4)
    metadata.extend(struct.pack("<q", 7))
    metadata.extend(struct.pack("<q", 0))
    metadata.extend(struct.pack("<I", len(payload)))
    push_i32(metadata, 0)
    push_i32(metadata, 0)  # script types
    push_i32(metadata, int(external_path is not None))
    if external_path is not None:
        metadata.append(0)  # empty prefix
        metadata.extend(bytes(16))  # GUID
        push_i32(metadata, 0)  # kind
        metadata.extend(external_path.encode("utf-8") + b"\0")
    push_i32(metadata, 0)  # reference types
    metadata.append(0)

    metadata_size = len(metadata)
    data_offset = ((48 + metadata_size + 15) // 16) * 16
    file_size = data_offset + len(payload)
    output = bytearray(48)
    output[8:12] = struct.pack(">I", 22)
    output[20:24] = struct.pack(">I", metadata_size)
    output[24:32] = struct.pack(">q", file_size)
    output[32:40] = struct.pack(">q", data_offset)
    output.extend(metadata)
    output.extend(bytes(data_offset - len(output)))
    output.extend(payload)
    return bytes(output)


def assert_constructor_releases_gil(
    operation: Callable[[], T], description: str
) -> T:
    ready = threading.Event()
    start = threading.Event()
    ran = threading.Event()

    def worker() -> None:
        ready.set()
        start.wait()
        ran.set()

    thread = threading.Thread(target=worker)
    thread.start()
    assert ready.wait(5), "GIL probe worker did not start"
    previous_interval = sys.getswitchinterval()
    sys.setswitchinterval(1_000.0)
    try:
        start.set()
        assert not ran.is_set(), "GIL probe ran before entering the Rust constructor"
        # Releasing the GIL makes the worker runnable, but it does not force the
        # operating system to schedule that thread before a very fast Rust
        # call returns. Give it several bounded constructor windows. With the
        # switch interval above, a binding which actually holds the GIL cannot
        # pass by switching between these Python loop iterations.
        result: Optional[T] = None
        for _ in range(8):
            result = operation()
            if ran.is_set():
                break
        assert result is not None
        assert ran.is_set(), f"{description} construction held the GIL"
    finally:
        sys.setswitchinterval(previous_interval)
        start.set()
        thread.join(5)
    assert not thread.is_alive(), "GIL probe worker did not finish"
    return result


def assert_schema_construction_releases_gil() -> None:
    """Pure-Rust schema validation and indexing must not monopolize Python."""

    # The binding must check each Python list length before asking PyO3 to
    # convert every element. Invalid entries prove the guards run first rather
    # than merely rejecting after conversion.
    try:
        MonoBehaviourSchema(
            "Probe.dll",
            "Probe",
            [("MonoBehaviour", "Base", 0, False)],
            unity_version="not-a-unity-version",
        )
    except ValueError as error:
        assert "invalid Unity version" in str(error)
    else:
        raise AssertionError("programmatic schemas must reject invalid Unity versions")

    oversized_nodes = [None] * 1_000_001
    try:
        MonoBehaviourSchema("Probe.dll", "Probe", oversized_nodes)
    except ValueError as error:
        assert "1000001 nodes" in str(error)
    else:
        raise AssertionError("oversized schema input must be rejected before conversion")

    oversized_collection = [None] * 100_001
    try:
        MonoBehaviourSchemas(oversized_collection)
    except ValueError as error:
        assert "100001 entries" in str(error)
    else:
        raise AssertionError(
            "oversized schema collection must be rejected before element conversion"
        )

    nodes = [("MonoBehaviour", "Base", 0, False)] + [
        ("SInt32", "value", 1, False)
    ] * 99_999
    schema = assert_constructor_releases_gil(
        lambda: MonoBehaviourSchema("Probe.dll", "Probe", nodes),
        "MonoBehaviourSchema",
    )
    assert schema.node_count == 100_000

    schema_collection = [schema] * 100_000
    schemas = assert_constructor_releases_gil(
        lambda: MonoBehaviourSchemas(schema_collection),
        "MonoBehaviourSchemas",
    )
    assert schemas.schema_count == 100_000


def main() -> None:
    assert_schema_construction_releases_gil()
    assert AnimationClip.__name__ == "AnimationClip"
    assert LegacyAnimation.__name__ == "LegacyAnimation"
    assert AnimatorOverrideController.__name__ == "AnimatorOverrideController"
    assert AssetBundle.__name__ == "AssetBundle"
    assert ResourceManager.__name__ == "ResourceManager"
    assert PreloadData.__name__ == "PreloadData"
    assert AclCompressedTracks.__name__ == "AclCompressedTracks"
    assert AclDecodedClip.__name__ == "AclDecodedClip"
    assert AnimatorController.__name__ == "AnimatorController"
    assert Avatar.__name__ == "Avatar"
    assert CubismClipMotion.__name__ == "CubismClipMotion"
    assert AudioClip.__name__ == "AudioClip"
    assert BinaryAsset.__name__ == "BinaryAsset"
    assert Material.__name__ == "Material"
    assert MonoScript.__name__ == "MonoScript"
    assert ResourceInfo.__name__ == "ResourceInfo"
    assert ResourceIterator.__name__ == "ResourceIterator"
    assert SceneLimits.__name__ == "SceneLimits"
    assert SpriteAtlas.__name__ == "SpriteAtlas"
    assert SpriteAtlasRenderData.__name__ == "SpriteAtlasRenderData"
    assert SpriteAtlasRenderDataKey.__name__ == "SpriteAtlasRenderDataKey"
    assert SpriteAtlasSecondaryTexture.__name__ == "SpriteAtlasSecondaryTexture"
    assert SpriteMetadata.__name__ == "SpriteMetadata"
    assert SpriteMetadataLimits.__name__ == "SpriteMetadataLimits"
    assert SpriteRenderData.__name__ == "SpriteRenderData"
    assert SpriteSecondaryTexture.__name__ == "SpriteSecondaryTexture"
    assert SpriteSettings.__name__ == "SpriteSettings"
    assert FbxCandidate.__name__ == "FbxCandidate"
    assert hasattr(Live2dPackage, "eye_blink_parameters")
    assert hasattr(Live2dPackage, "lip_sync_parameters")
    targets = CubismMotionTargets(parameters=["ParamAngleX"], parts=["PartBody"])
    assert targets.parameters == ["ParamAngleX"]
    assert targets.parts == ["PartBody"]
    with tempfile.TemporaryDirectory(prefix="unity-rs-python-") as directory:
        path = Path(directory) / "fixture.assets"
        missing_path = Path(directory) / "missing.assets"
        try:
            UnityRs(missing_path)
        except FileNotFoundError:
            pass
        else:
            raise AssertionError(
                "a missing input path should preserve FileNotFoundError"
            )
        path.write_bytes(synthetic_text_asset())

        studio = UnityRs(path)
        compat = UnityPyCompat.load(path)
        assert UnityPyCompat.load is UnityPyCompat.Environment
        assert UnityPyCompat.AssetsManager is UnityPyCompat.Environment
        assert len(compat.assets) == 1
        assert compat.file is compat.assets[0]
        assert compat.get("assets") is compat.assets
        assert compat.get("missing", "fallback") == "fallback"
        assert compat.get_cab("FIXTURE.ASSETS") is compat.file
        assert compat.find_file("fixture.assets") is compat.file
        try:
            compat.find_file("missing.assets")
        except FileNotFoundError as error:
            assert "missing.assets" in str(error)
        else:
            raise AssertionError("missing compatibility files must remain explicit")
        assert compat.file.version == 22
        assert compat.file.target_platform == 13
        assert compat.file.unity_version == "2022.3.62f1"
        assert len(compat.file.types) == 1
        text_serialized_type = compat.file.types[0]
        assert text_serialized_type.class_id == int(UnityPyCompat.ClassIDType.TextAsset)
        assert text_serialized_type.script_type_index == -1
        assert text_serialized_type.script_id is None
        assert text_serialized_type.old_type_hash == bytes(16)
        assert text_serialized_type.type_dependencies is None
        assert text_serialized_type.node is None
        assert compat.file.ref_types == []
        assert list(compat.file.objects) == [7]
        compat_reader = compat.file.objects[7]
        assert compat_reader.serialized_type is text_serialized_type
        assert compat_reader.type is UnityPyCompat.ClassIDType.TextAsset
        assert compat_reader.type_id == 0
        assert compat_reader.serialized_type.nodes is None
        assert compat_reader.byte_start >= 0
        assert compat_reader.get("path_id") == 7
        assert compat_reader.get("missing", "fallback") == "fallback"
        assert repr(compat_reader) == "<ObjectReader TextAsset>"
        assert compat_reader.get_raw_data().endswith(b"hello python")
        compat_text = compat_reader.parse_as_object()
        assert isinstance(compat_text, UnityPyCompat.TextAsset)
        assert compat_text.m_Name == "python"
        assert compat_text.m_Script == "hello python"
        assert compat.objects == [compat_reader]
        try:
            compat_reader.parse_as_object(check_read=False)
        except NotImplementedError:
            pass
        else:
            raise AssertionError("compatibility parsing must not weaken layout validation")
        assert not UnityPyCompat.PPtr(compat.file, 0, 0)
        assert UnityPyCompat.PPtr(compat.file, 0, 7).deref() is compat_reader
        try:
            UnityPyCompat.PPtr(compat.file, 1, 7).deref()
        except FileNotFoundError:
            pass
        else:
            raise AssertionError("missing PPtr externals must remain FileNotFoundError")
        try:
            compat_reader.parse_as_dict()
        except UnityPyCompat.TypeTreeError as error:
            assert "no type tree" in str(error)
        else:
            raise AssertionError("stripped TypeTree reads must fail explicitly")
        supplied_text_tree = [
            {"m_Level": 0, "m_Type": "TextAsset", "m_Name": "Base"},
            {
                "m_Level": 1,
                "m_Type": "string",
                "m_Name": "m_Name",
                "m_MetaFlag": ALIGN,
            },
            {
                "m_Level": 2,
                "m_Type": "Array",
                "m_Name": "Array",
                "m_TypeFlags": 1,
                "m_MetaFlag": ALIGN,
            },
            {"m_Level": 3, "m_Type": "int", "m_Name": "size"},
            {"m_Level": 3, "m_Type": "char", "m_Name": "data"},
            {
                "m_Level": 1,
                "m_Type": "TypelessData",
                "m_Name": "m_Script",
            },
            {"m_Level": 2, "m_Type": "int", "m_Name": "size"},
            {"m_Level": 2, "m_Type": "UInt8", "m_Name": "data"},
        ]
        supplied_text = compat_reader.read_typetree(supplied_text_tree)
        assert supplied_text == {"m_Name": "python", "m_Script": b"hello python"}
        supplied_text_object = compat_reader.read_typetree(
            supplied_text_tree, wrap=True
        )
        assert supplied_text_object.__class__.__name__ == "TextAsset"
        assert supplied_text_object.m_Name == "python"
        assert supplied_text_object.m_Script == b"hello python"
        supplied_structure = compat_reader.dump_typetree_structure(
            supplied_text_tree, indent=""
        )
        assert supplied_structure.startswith(
            "TextAsset Base // ByteSize{0}, Index{0}, Version{0}"
        )
        assert "  string m_Name" in supplied_structure
        try:
            compat_reader.read_typetree(supplied_text_tree[:1])
        except UnityPyCompat.TypeTreeError as error:
            assert "does not match this object" in str(error)
        else:
            raise AssertionError("partial caller TypeTrees must not produce partial output")
        try:
            compat.save()
        except NotImplementedError:
            pass
        else:
            raise AssertionError("read-only compatibility save must fail explicitly")
        try:
            UnityPyCompat.load(path, maximum_file_bytes=1)
        except ValueError as error:
            assert "maximum_file_bytes 1" in str(error)
        else:
            raise AssertionError("compatibility path inputs must obey byte limits")
        limited_types = UnityPyCompat.load(path, maximum_compat_types=0)
        try:
            _ = limited_types.file.types
        except MemoryError as error:
            assert "maximum_compat_types 0" in str(error)
        else:
            raise AssertionError("serialized type materialization must obey limits")

        compat_memory = UnityPyCompat.load(synthetic_text_asset())
        assert compat_memory.file.objects[7].read().m_Script == "hello python"
        compat_stream = UnityPyCompat.load(
            io.BytesIO(synthetic_text_asset()), maximum_file_bytes=1024 * 1024
        )
        assert compat_stream.file.objects[7].peek_name() == "python"
        compat_source_path = Path(directory) / "source.assets"
        compat_target_path = Path(directory) / "target.assets"
        compat_source_path.write_bytes(
            synthetic_text_asset(external_path="target.assets")
        )
        compat_target_path.write_bytes(synthetic_text_asset())
        compat_external = UnityPyCompat.load(
            compat_source_path, compat_target_path
        )
        assert compat_external.assets[0].externals[0].path == "target.assets"
        assert compat_external.get_cab("folder\\TARGET.ASSETS") is compat_external.assets[1]
        assert compat_external.find_file("target.assets") is compat_external.assets[1]
        external_reader = UnityPyCompat.PPtr(
            compat_external.assets[0], 1, 7
        ).deref()
        assert external_reader is compat_external.assets[1].objects[7]
        virtual_fs = MemoryFileSystem(
            {
                "virtual/source.assets": synthetic_text_asset(
                    external_path="target.assets"
                ),
                "virtual/target.assets": synthetic_text_asset(),
            }
        )
        compat_virtual = UnityPyCompat.load("virtual", fs=virtual_fs)
        assert compat_virtual.fs is virtual_fs
        assert compat_virtual.path == "virtual"
        assert len(compat_virtual.assets) == 2
        virtual_external = UnityPyCompat.PPtr(
            compat_virtual.assets[0], 1, 7
        ).deref()
        assert virtual_external is compat_virtual.assets[1].objects[7]
        assert virtual_fs.opened and all(stream.closed for stream in virtual_fs.opened)
        try:
            UnityPyCompat.load("virtual", fs=virtual_fs, maximum_files=1)
        except ValueError as error:
            assert "maximum_files 1" in str(error)
        else:
            raise AssertionError("virtual filesystem enumeration must obey file limits")
        try:
            UnityPyCompat.load(
                "virtual/source.assets", fs=virtual_fs, maximum_file_bytes=1
            )
        except ValueError as error:
            assert "maximum_file_bytes 1" in str(error)
        else:
            raise AssertionError("virtual filesystem reads must obey byte limits")
        invalid_fs = InvalidStreamFileSystem()
        try:
            UnityPyCompat.load("invalid.assets", fs=invalid_fs)
        except TypeError as error:
            assert "binary stream" in str(error)
        else:
            raise AssertionError("virtual filesystem streams must be validated")
        assert invalid_fs.invalid_stream.closed
        first_pointer = UnityPyCompat.PPtr(compat.file, 0, 7)
        second_pointer = UnityPyCompat.PPtr(compat.file, 0, 8)
        duplicate_container = UnityPyCompat.ContainerHelper(
            [("assets/shared", first_pointer), ("assets/shared", second_pointer)]
        )
        assert len(duplicate_container) == 2
        assert list(duplicate_container.items()) == [
            ("assets/shared", first_pointer),
            ("assets/shared", second_pointer),
        ]
        assert duplicate_container["assets/shared"] is second_pointer

        tree_compat = UnityPyCompat.load(synthetic_type_tree_object())
        tree_reader = tree_compat.file.objects[7]
        tree_nodes = tree_reader.serialized_type.nodes
        assert tree_nodes is not None
        assert tree_reader.serialized_type is tree_compat.file.types[0]
        assert tree_reader.serialized_type.script_id == bytes(16)
        assert tree_reader.serialized_type.old_type_hash == bytes(16)
        assert tree_reader.serialized_type.node is tree_nodes[0]
        assert list(tree_reader.serialized_type.node.traverse()) == tree_nodes
        assert tree_reader.serialized_type.node.to_dict_list() == [
            node.to_dict() for node in tree_nodes
        ]
        assert (
            tree_reader.serialized_type.node.to_dict()["m_Children"]
            is tree_reader.serialized_type.node.m_Children
        )
        assert tree_nodes[0].m_Type == "MonoBehaviour"
        assert tree_nodes[0].m_Name == "Base"
        caller_node = UnityPyCompat.TypeTreeNode(0, "int", "pass", 4, 1)
        assert caller_node._clean_name == "pass_"
        assert caller_node.to_dict()["m_Children"] == []
        caller_node.m_Children.append(caller_node)
        try:
            caller_node.to_dict_list()
        except ValueError as error:
            assert "cycle or shared node" in str(error)
        else:
            raise AssertionError("TypeTree helpers must reject cyclic caller nodes")
        tree_structure = tree_reader.dump_typetree_structure()
        assert tree_structure.startswith(
            "  MonoBehaviour Base // ByteSize{-1}, Index{0}, Version{1}"
        )
        assert "    string m_Name" in tree_structure
        try:
            tree_compat._native.type_tree_nodes(0, 7, maximum_nodes=1)
        except ValueError as error:
            assert "exceeding limit 1" in str(error)
        else:
            raise AssertionError("TypeTree node materialization limit must be enforced")
        tree_dict = tree_reader.parse_as_dict()
        assert tree_dict["m_Name"] == "tree-probe"
        assert tree_dict["Signed8"] == -7
        assert tree_dict["Unsigned16"] == 65000
        assert tree_dict["Numbers"] == [5, -6, 7]
        supplied_tree_dict = tree_reader.parse_as_dict(tree_nodes)
        assert supplied_tree_dict == tree_dict
        renamed_tree = [
            {
                "m_Level": node.m_Level,
                "m_Type": node.m_Type,
                "m_Name": "CustomSigned8"
                if node.m_Name == "Signed8"
                else node.m_Name,
                "m_ByteSize": node.m_ByteSize,
                "m_Index": node.m_Index,
                "m_TypeFlags": node.m_TypeFlags,
                "m_Version": node.m_Version,
                "m_MetaFlag": node.m_MetaFlag,
                "m_RefTypeHash": node.m_RefTypeHash,
            }
            for node in tree_nodes
        ]
        renamed_tree_dict = tree_reader.read_typetree(renamed_tree)
        assert renamed_tree_dict["CustomSigned8"] == -7
        assert "Signed8" not in renamed_tree_dict
        tree_object = tree_reader.parse_as_object()
        assert tree_object.__class__.__name__ == "MonoBehaviour"
        assert tree_object.m_Name == "tree-probe"
        assert isinstance(tree_object.m_GameObject, UnityPyCompat.PPtr)
        assert not tree_object.m_GameObject
        shape_compat = UnityPyCompat.load(synthetic_unitypy_type_tree_shapes())
        shape_dict = shape_compat.file.objects[7].parse_as_dict()
        assert shape_dict["Character"] == 0x263A
        assert shape_dict["Blob"] == b"\x00\xff\x7f"
        assert shape_dict["Pairs"] == [(1, 2), (3, 4)]
        memory_studio = UnityRs.from_bytes(
            synthetic_text_asset(), name="memory-fixture.assets"
        )
        assert memory_studio.file_count == 1
        assert memory_studio.files()[0].path == "memory-fixture.assets"
        assert memory_studio.read_text(0, 7) == b"hello python"
        try:
            UnityRs.from_bytes(
                b"resource", name="12345", maximum_path_bytes=4
            )
        except ValueError as error:
            assert "path limit 4" in str(error)
        else:
            raise AssertionError("in-memory input names must obey the path limit")
        try:
            UnityRs.from_bytes(
                b"resource", name="12345", maximum_total_path_bytes=4
            )
        except ValueError as error:
            assert "total path limit 4" in str(error)
        else:
            raise AssertionError("one input name must obey the total path limit")
        try:
            UnityRs.from_bytes(synthetic_text_asset(), maximum_bytes=1)
        except ValueError:
            pass
        else:
            raise AssertionError("in-memory input limit should be enforced")
        memory_resource = UnityRs.from_bytes(
            b"not a Unity container", name="memory.resource"
        )
        assert memory_resource.file_count == 0
        assert memory_resource.resource_count == 1
        assert memory_resource.read_resource(0) == b"not a Unity container"
        oodle_bundle, oodle_info, oodle_payload, oodle_data_input = (
            synthetic_oodle_bundle()
        )
        oodle_calls: list[tuple[bytes, int]] = []

        def decode_oodle(block: bytes, expected_size: int) -> bytes:
            oodle_calls.append((block, expected_size))
            if block == b"fake-oodle-blocks-info":
                return oodle_info
            if block == oodle_data_input:
                return oodle_payload
            raise AssertionError(f"unexpected Oodle block {block!r}")

        oodle_studio = UnityRs.from_bytes(
            oodle_bundle,
            name="oodle.bundle",
            oodle_decoder=decode_oodle,
        )
        assert oodle_studio.file_count == 0
        assert oodle_studio.resource_count == 1
        assert oodle_studio.resources()[0].path == "oodle.bundle::folder/data.bin"
        assert oodle_studio.read_resource(0) == oodle_payload
        assert oodle_calls == [
            (b"fake-oodle-blocks-info", len(oodle_info)),
            (oodle_data_input, len(oodle_payload)),
        ]
        oodle_source_path = Path(directory) / "oodle-load.bundle"
        oodle_source_path.write_bytes(oodle_bundle)
        oodle_calls.clear()
        assert (
            UnityRs(oodle_source_path, oodle_decoder=decode_oodle).read_resource(0)
            == oodle_payload
        )
        oodle_source_path.unlink()
        oodle_calls.clear()
        assert (
            UnityRs.from_memory_files(
                [("oodle-memory.bundle", oodle_bundle)],
                oodle_decoder=decode_oodle,
            ).read_resource(0)
            == oodle_payload
        )
        try:
            UnityRs.from_bytes(oodle_bundle, name="oodle.bundle")
        except NotImplementedError:
            pass
        else:
            raise AssertionError("Oodle bundles without a decoder should be rejected")
        try:
            UnityRs.from_bytes(
                oodle_bundle,
                name="oodle.bundle",
                oodle_decoder=lambda _block, _size: b"short",
            )
        except ValueError:
            pass
        else:
            raise AssertionError("short Python Oodle decoder output should be rejected")
        try:
            UnityRs.from_bytes(
                oodle_bundle,
                name="oodle.bundle",
                oodle_decoder=object(),
            )
        except TypeError:
            pass
        else:
            raise AssertionError("non-callable Python Oodle decoders should be rejected")
        memory_files = UnityRs.from_memory_files(
            [
                ("multi.assets", synthetic_text_asset()),
                ("multi.resS", b"multi resource"),
            ]
        )
        assert memory_files.file_count == 1
        assert memory_files.resource_count == 1
        assert memory_files.read_text(0, 7) == b"hello python"
        assert memory_files.read_resource_by_path("MULTI.RESS") == b"multi resource"
        external_video = UnityRs.from_memory_files(
            [
                ("external.assets", synthetic_external_video_clip()),
                ("external.resS", b"xxvideo-binyy"),
            ]
        ).read_video_clip(0, 7)
        assert external_video.name == "external-video"
        assert external_video.extension == ".mp4"
        assert external_video.data == b"video-bin"
        try:
            UnityRs.from_memory_files([None], maximum_files=0)
        except ValueError as error:
            assert "1 files" in str(error)
        else:
            raise AssertionError(
                "memory file count must be checked before tuple conversion"
            )
        empty_key_studio = UnityRs.from_memory_files(
            [], unity_cn_key=b"0123456789abcdef"
        )
        assert empty_key_studio.file_count == 0
        assert (
            UnityRs.from_memory_files(
                [], unity_cn_key="0123456789abcdef"
            ).file_count
            == 0
        )
        for invalid_key in (b"short", "short", [0] * 16):
            try:
                UnityRs.from_memory_files([], unity_cn_key=invalid_key)
            except ValueError:
                pass
            else:
                raise AssertionError(
                    "UnityCN keys must be exactly 16 UTF-8 bytes or raw bytes"
                )
        try:
            UnityRs.from_memory_files(
                [("a", b"a"), ("b", b"b")], maximum_files=1
            )
        except ValueError:
            pass
        else:
            raise AssertionError("memory file count limit should be enforced")
        try:
            UnityRs.from_memory_files(
                [("a", b"aa")], maximum_file_bytes=1
            )
        except ValueError:
            pass
        else:
            raise AssertionError("memory per-file byte limit should be enforced")
        try:
            UnityRs.from_memory_files(
                [("a", b"aa"), ("b", b"bb")], maximum_total_bytes=3
            )
        except ValueError:
            pass
        else:
            raise AssertionError("memory total byte limit should be enforced")
        try:
            UnityRs.from_memory_files(
                [("abc", b"a"), ("de", b"b")], maximum_total_path_bytes=4
            )
        except ValueError as error:
            assert "names total 5 bytes" in str(error)
        else:
            raise AssertionError("memory input names must share one path budget")
        try:
            UnityRs(Path(directory), maximum_input_directories=0)
        except ValueError:
            pass
        else:
            raise AssertionError("directory traversal limits should be enforced")
        try:
            UnityRs(path, maximum_path_bytes=1)
        except ValueError as error:
            assert "asset path" in str(error)
        else:
            raise AssertionError("filesystem input labels must obey the path limit")

        # A game directory mixes readable assets with containers whose layout
        # has never been verified. By default one of those fails the whole
        # load; skip_unreadable_inputs keeps everything that did parse.
        with tempfile.TemporaryDirectory(prefix="unity-rs-mixed-") as mixed_root:
            mixed = Path(mixed_root)
            (mixed / "a-good.assets").write_bytes(synthetic_text_asset())
            archive = (
                b"UnityArchive\0"
                + (5).to_bytes(4, "big")
                + b"5.x.x\0"
                + b"5.0.0f4\0"
            )
            (mixed / "b-archive.unity3d").write_bytes(archive)
            (mixed / "c-good.assets").write_bytes(synthetic_text_asset())
            try:
                UnityRs(mixed)
            except NotImplementedError:
                pass
            else:
                raise AssertionError("an unreadable input should fail the load")
            tolerant = UnityRs(mixed, skip_unreadable_inputs=True)
            assert tolerant.file_count == 2, tolerant.file_count
            assert tolerant.object_count == 2, tolerant.object_count
            assert tolerant.load_diagnostic_count == 1
            diagnostic = tolerant.load_diagnostic_page(limit=1)[0]
            assert "b-archive" in diagnostic.path
            assert "UnityArchive" in diagnostic.message
            assert tolerant.load_diagnostic_page(offset=1) == []
            try:
                UnityRs(
                    mixed,
                    skip_unreadable_inputs=True,
                    maximum_diagnostic_bytes=0,
                )
            except ValueError as error:
                assert "load diagnostics require" in str(error)
            else:
                raise AssertionError("load diagnostic budget should be enforced")
        assert studio.file_count == 1
        assert studio.object_count == 1
        assert studio.resource_count == 0
        assert "files=1, objects=1, resources=0" in repr(studio)

        file_iterator = studio.iter_files()
        assert iter(file_iterator) is file_iterator
        assert [file.path for file in file_iterator] == [str(path)]
        object_iterator = studio.iter_objects()
        assert iter(object_iterator) is object_iterator
        assert [obj.path_id for obj in object_iterator] == [7]
        # The iterator owns a strong reference to its temporary Studio.
        assert next(UnityRs(path).iter_objects()).path_id == 7
        assert [file.index for file in studio.file_page(limit=1)] == [0]
        assert studio.file_page(offset=1) == []
        assert [obj.path_id for obj in studio.object_page(0, limit=1)] == [7]
        assert studio.object_page(0, offset=1) == []
        try:
            studio.object_page(1)
        except KeyError:
            pass
        else:
            raise AssertionError("missing object-page file should raise KeyError")
        try:
            studio.file_page(limit=1_000_001)
        except ValueError:
            pass
        else:
            raise AssertionError("metadata page limit should be enforced")

        files = studio.files()
        assert files[0].path.endswith("fixture.assets")
        assert files[0].unity_version == "2022.3.62f1"
        assert files[0].object_count == 1

        objects = studio.objects()
        assert objects[0].file_index == 0
        assert objects[0].object_index == 0
        assert objects[0].path_id == 7
        assert objects[0].class_id == 49
        assert objects[0].name == "python"
        assert studio.read_text(0, 7) == b"hello python"
        assert studio.read_raw(0, 7).endswith(b"hello python")

        resource_path = Path(directory) / "python.resS"
        resource_path.write_bytes(b"external python payload")
        resource_studio = UnityRs(directory)
        assert resource_studio.resource_count == 1
        resources = resource_studio.resources()
        assert len(resources) == 1
        assert isinstance(resources[0], ResourceInfo)
        assert resources[0].index == 0
        assert resources[0].path == str(resource_path)
        assert resources[0].byte_size == 23
        resource_iterator = resource_studio.iter_resources()
        assert isinstance(resource_iterator, ResourceIterator)
        assert iter(resource_iterator) is resource_iterator
        assert [resource.path for resource in resource_iterator] == [str(resource_path)]
        assert resource_studio.resource_page(limit=1)[0].byte_size == 23
        assert resource_studio.resource_page(offset=1) == []
        assert resource_studio.read_resource(0) == b"external python payload"
        assert resource_studio.read_resource_range(0, 9, 6) == b"python"
        assert (
            resource_studio.read_resource_by_path("PYTHON.RESS")
            == b"external python payload"
        )
        try:
            resource_studio.read_resource(0, maximum_bytes=22)
        except ValueError:
            pass
        else:
            raise AssertionError("external resource byte limit should be enforced")
        try:
            resource_studio.read_resource_range(0, 9, 6, maximum_bytes=5)
        except ValueError:
            pass
        else:
            raise AssertionError("external resource range limit should be enforced")
        try:
            resource_studio.read_resource_range(0, 22, 2)
        except ValueError:
            pass
        else:
            raise AssertionError("external resource range bounds should be enforced")
        try:
            resource_studio.read_resource(1)
        except KeyError:
            pass
        else:
            raise AssertionError("missing external resource index should raise KeyError")

        # Unity changed the serialized shader in 2021 and again in 2022.2. The
        # managed implementation followed neither and refuses the class from
        # 2021 on; this reader implements the additions, so a Unity 6 shader
        # reads.
        shader_path = Path(directory) / "unity6-shader.assets"
        shader_path.write_bytes(synthetic_unity6_shader())
        shader_studio = UnityRs(shader_path)
        shader = shader_studio.read_shader(0, 7)
        shader_header = (
            b"//////////////////////////////////////////\n"
            b"//\n"
            b"// NOTE: This is *not* a valid shader file\n"
            b"//\n"
            b"///////////////////////////////////////////\n"
        )
        assert shader == shader_header + b'Shader "Parsed/Unity6" {\nProperties {\n}\n}'
        try:
            shader_studio.read_shader(0, 7, maximum_bytes=len(shader) - 1)
        except ValueError:
            pass
        else:
            raise AssertionError("Shader output limit should be enforced")

        # Above the verified Unity majors the default is lenient: the newest
        # known layout is attempted. This 6000.2-layout shader relabeled 7000
        # misses the 6000.3 asset-identifier field, so the attempt does not
        # fit and the failure stays in the NotImplementedError family with the
        # attempt recorded. strict_unity_versions restores the version-gate
        # rejection without touching the payload.
        future_path = Path(directory) / "unity7-shader.assets"
        future_path.write_bytes(synthetic_unity6_shader(unity_version="7000.0.0f1"))
        try:
            UnityRs(future_path).read_shader(0, 7)
        except NotImplementedError as error:
            assert "above the verified range" in str(error), error
        else:
            raise AssertionError("the lenient 7000 shader attempt should not fit")
        try:
            UnityRs(future_path, strict_unity_versions=True).read_shader(0, 7)
        except NotImplementedError as error:
            assert "Shader serialization version" in str(error), error
        else:
            raise AssertionError("strict mode should refuse the 7000 shader outright")

        # The keyword is accepted by the in-memory constructors as well, and
        # in-range files are unaffected by strict mode.
        strict_bytes = UnityRs.from_bytes(
            synthetic_text_asset(), strict_unity_versions=True
        )
        assert strict_bytes.object_count == 1
        strict_memory = UnityRs.from_memory_files(
            [("text.assets", synthetic_text_asset())], strict_unity_versions=True
        )
        assert strict_memory.object_count == 1

        mesh_path = Path(directory) / "mesh.assets"
        mesh_path.write_bytes(synthetic_mesh())
        mesh_studio = UnityRs(mesh_path)
        mesh_obj = mesh_studio.read_mesh_obj(0, 7)
        assert mesh_obj == (
            b"g tri:mesh\r\n"
            b"v -1 0 0\r\n"
            b"v -0 1 0\r\n"
            b"v -0 0 1\r\n"
            b"g tri:mesh_0\r\n"
            b"f 3/3/3 2/2/2 1/1/1\r\n"
        )
        tuanjie_mesh_path = Path(directory) / "mesh-tuanjie.assets"
        tuanjie_mesh_path.write_bytes(synthetic_mesh(tuanjie=True))
        assert UnityRs(tuanjie_mesh_path).read_mesh_obj(0, 7) == mesh_obj
        external_mesh = UnityRs.from_memory_files(
            [
                ("mesh-stream.assets", synthetic_mesh(external=True)),
                (
                    "PYTHON-MESH.RESS",
                    b"pad" + synthetic_mesh_vertex_data(),
                ),
            ]
        )
        assert external_mesh.read_mesh_obj(0, 7) == mesh_obj
        try:
            mesh_studio.read_mesh_obj(0, 7, maximum_bytes=8)
        except ValueError:
            pass
        else:
            raise AssertionError("Mesh OBJ output limit should be enforced")
        try:
            studio.read_mesh_obj(0, 7)
        except NotImplementedError:
            pass
        else:
            raise AssertionError("non-Mesh objects should be rejected")

        extraction_output = Path(directory) / "extracted"
        extraction = extract(path, extraction_output)
        assert extraction.failures == []
        assert extraction.skipped_existing == []
        assert len(extraction.extracted) == 1
        extracted_record = extraction.extracted[0]
        assert extracted_record.source == str(path)
        assert extracted_record.bytes == path.stat().st_size
        assert Path(extracted_record.output_path).read_bytes() == path.read_bytes()
        assert extraction.output_bytes == path.stat().st_size
        if sys.platform.startswith("linux"):
            raw_output = os.fsencode(directory) + b"/extracted-\xff"
            invalid_output = Path(os.fsdecode(raw_output))
            invalid_path_report = extract(path, invalid_output)
            assert len(invalid_path_report.extracted) == 1
            assert "\ufffd" in invalid_path_report.extracted[0].output_path
            assert os.path.isdir(raw_output)
        skipped = extract(path, extraction_output)
        assert skipped.extracted == []
        assert len(skipped.skipped_existing) == 1
        assert skipped.failures == []
        replaced = extract(path, extraction_output, overwrite=True)
        assert len(replaced.extracted) == 1
        assert replaced.failures == []
        extraction_limited_output = Path(directory) / "extraction-limited"
        limited_extraction = extract(
            path,
            extraction_limited_output,
            limits=ExtractionLimits(maximum_output_bytes=1),
        )
        assert limited_extraction.extracted == []
        assert len(limited_extraction.failures) == 1
        assert limited_extraction.output_bytes == 0
        assert not any(
            child.is_file() for child in extraction_limited_output.rglob("*")
        )
        metadata_limited_output = Path(directory) / "extraction-metadata-limited"
        metadata_limit = ExtractionLimits(maximum_metadata_bytes=0)
        assert metadata_limit.maximum_metadata_bytes == 0
        try:
            extract(path, metadata_limited_output, limits=metadata_limit)
        except ValueError as error:
            assert "extraction report metadata requires" in str(error)
        else:
            raise AssertionError("extraction report metadata budget should be enforced")
        assert not any(
            child.is_file() for child in metadata_limited_output.rglob("*")
        )
        path_budget_input = Path(directory) / "path-budget-input"
        path_budget_input.mkdir()
        (path_budget_input / "payload.bin").write_bytes(b"payload")
        path_budget = ExtractionLimits(
            maximum_total_path_bytes=len(str(path_budget_input).encode("utf-8"))
        )
        assert path_budget.maximum_total_path_bytes == len(
            str(path_budget_input).encode("utf-8")
        )
        try:
            extract(
                path_budget_input,
                Path(directory) / "path-budget-output",
                limits=path_budget,
            )
        except ValueError as error:
            assert "extraction paths total" in str(error)
        else:
            raise AssertionError("cumulative extraction path budget should be enforced")
        oodle_path = Path(directory) / "oodle.bundle"
        oodle_path.write_bytes(oodle_bundle)
        oodle_extract_output = Path(directory) / "oodle-extracted"
        oodle_calls.clear()
        oodle_extraction = extract(
            oodle_path,
            oodle_extract_output,
            oodle_decoder=decode_oodle,
        )
        assert oodle_extraction.failures == []
        extracted_oodle_files = [
            child for child in oodle_extract_output.rglob("*") if child.is_file()
        ]
        assert len(extracted_oodle_files) == 1
        assert extracted_oodle_files[0].read_bytes() == oodle_payload
        # One data-block decode, not two: the extractor's header probe and
        # write pass share the bundle's decoded-block cache, so the Oodle
        # callback runs once for the blocks info and once for the data block.
        assert oodle_calls == [
            (b"fake-oodle-blocks-info", len(oodle_info)),
            (oodle_data_input, len(oodle_payload)),
        ]
        live2d = studio.read_live2d_packages()
        assert live2d.packages == []
        assert live2d.diagnostics == []
        try:
            studio.read_live2d_packages(acl_decoder=object())
        except TypeError:
            pass
        else:
            raise AssertionError("non-callable Live2D ACL decoders should be rejected")
        try:
            studio.read_static_fbx()
        except NotImplementedError:
            pass
        else:
            raise AssertionError("a collection without Transforms has no static FBX scene")

        output = Path(directory) / "export"
        report = studio.export(output)
        assert report.failures == []
        assert len(report.exported) == 1
        exported = Path(report.exported[0])
        assert exported.name == "python.txt"
        assert exported.read_bytes() == b"hello python"

        no_clobber = studio.export(output)
        assert no_clobber.exported == []
        assert len(no_clobber.failures) == 1
        assert exported.read_bytes() == b"hello python"

        overwritten = studio.export(output, overwrite=True)
        assert overwritten.failures == []
        assert exported.read_bytes() == b"hello python"

        object_limited = Path(directory) / "object-limited"
        try:
            studio.export(object_limited, limits=ExportLimits(maximum_objects=0))
        except ValueError:
            pass
        else:
            raise AssertionError("export object limit should raise ValueError")
        assert not object_limited.exists()

        byte_limited = Path(directory) / "byte-limited"
        limited_report = studio.export(
            byte_limited,
            limits=ExportLimits(maximum_total_output_bytes=5),
        )
        assert limited_report.exported == []
        assert len(limited_report.failures) == 1
        assert not any(path.is_file() for path in byte_limited.rglob("*"))

        metadata_limits = ExportLimits(maximum_metadata_bytes=0)
        assert metadata_limits.maximum_metadata_bytes == 0
        metadata_limited = Path(directory) / "metadata-limited"
        try:
            studio.export(metadata_limited, limits=metadata_limits)
        except ValueError as error:
            assert "export metadata exceeds" in str(error)
        else:
            raise AssertionError("export metadata budget should raise ValueError")
        assert not any(path.is_file() for path in metadata_limited.rglob("*"))

        try:
            studio.read_raw(0, 8)
        except KeyError:
            pass
        else:
            raise AssertionError("missing path_id should raise KeyError")

        texture_path = Path(directory) / "texture.assets"
        texture_path.write_bytes(synthetic_texture2d())
        texture_studio = UnityRs(texture_path)
        image = texture_studio.read_texture(0, 7)
        assert repr(image) == "RgbaImage(width=2, height=2)"
        assert image.width == 2
        assert image.height == 2
        assert image.rgba == bytes(
            (
                0,
                0,
                255,
                3,
                255,
                255,
                255,
                4,
                255,
                0,
                0,
                1,
                0,
                255,
                0,
                2,
            )
        )
        try:
            texture_studio.read_texture(0, 7, maximum_bytes=8)
        except ValueError:
            pass
        else:
            raise AssertionError("texture output limit should raise ValueError")

        switch_path = Path(directory) / "switch-chain.assets"
        switch_path.write_bytes(synthetic_switch_mip_chain())
        switch_image = UnityRs(switch_path).read_texture(0, 7)
        assert (switch_image.width, switch_image.height) == (1, 1)
        assert switch_image.rgba == bytes((9, 8, 7, 6))

        array_path = Path(directory) / "texture-array.assets"
        array_path.write_bytes(synthetic_texture2d_array())
        array_images = UnityRs(array_path).read_texture_array(0, 7)
        assert len(array_images) == 2
        assert repr(array_images[0]) == "RgbaImage(width=1, height=2)"
        assert array_images[0].rgba == bytes((5, 6, 7, 8, 1, 2, 3, 4))
        assert array_images[1].rgba == bytes((15, 16, 17, 18, 11, 12, 13, 14))
        try:
            UnityRs(array_path).read_texture_array(0, 7, maximum_bytes=15)
        except ValueError:
            pass
        else:
            raise AssertionError("texture-array cumulative output limit should raise ValueError")
        try:
            studio.read_texture_array(0, 7)
        except NotImplementedError:
            pass
        else:
            raise AssertionError("non-Texture2DArray objects should be rejected")

        sprite_path = Path(directory) / "sprite.assets"
        sprite_path.write_bytes(synthetic_sprite_with_atlas_backfill())
        sprite_studio = UnityRs(sprite_path)
        sprite_metadata = sprite_studio.read_sprite_metadata(0, 7)
        assert isinstance(sprite_metadata, SpriteMetadata)
        assert sprite_metadata.object_index == 0
        assert sprite_metadata.path_id == 7
        assert sprite_metadata.name == "python sprite"
        assert sprite_metadata.rect == (0.0, 0.0, 1.0, 1.0)
        assert sprite_metadata.offset == (0.0, 0.0)
        assert sprite_metadata.border == (0.0, 0.0, 0.0, 0.0)
        assert sprite_metadata.pixels_to_units == 100.0
        assert sprite_metadata.pivot == (0.5, 0.5)
        assert sprite_metadata.extrude == 0
        assert sprite_metadata.is_polygon is False
        assert sprite_metadata.render_data_key is not None
        assert sprite_metadata.render_data_key.guid_bytes == bytes(16)
        assert sprite_metadata.render_data_key.value == 0
        assert sprite_metadata.atlas_tags == []
        assert sprite_metadata.sprite_atlas == (0, 0)
        sprite_render_data = sprite_metadata.render_data
        assert isinstance(sprite_render_data, SpriteRenderData)
        assert sprite_render_data.texture == (0, 8)
        assert sprite_render_data.alpha_texture == (0, 0)
        assert sprite_render_data.secondary_textures == []
        assert sprite_render_data.texture_rect == (0.0, 0.0, 1.0, 1.0)
        assert sprite_render_data.texture_rect_offset == (0.0, 0.0)
        assert sprite_render_data.atlas_rect_offset == (0.0, 0.0)
        assert sprite_render_data.uv_transform == (0.0, 0.0, 1.0, 1.0)
        assert sprite_render_data.downscale_multiplier == 1.0
        assert sprite_render_data.mesh_triangles == []
        assert isinstance(sprite_render_data.settings, SpriteSettings)
        assert sprite_render_data.settings.raw == 2
        assert sprite_render_data.settings.packed is False
        assert sprite_render_data.settings.packing_mode == "rectangle"
        assert sprite_render_data.settings.packing_rotation == 0
        assert sprite_render_data.settings.mesh_type == "full_rect"
        default_sprite_limits = SpriteMetadataLimits()
        assert default_sprite_limits.maximum_entries == 1_000_000
        assert default_sprite_limits.maximum_string_bytes == 16_777_216
        assert default_sprite_limits.maximum_total_string_bytes == 33_554_432
        assert default_sprite_limits.maximum_mesh_bytes == 536_870_912
        for limits in (
            SpriteMetadataLimits(maximum_entries=0),
            SpriteMetadataLimits(maximum_string_bytes=5),
            SpriteMetadataLimits(maximum_total_string_bytes=5),
            SpriteMetadataLimits(maximum_mesh_bytes=0),
        ):
            try:
                sprite_studio.read_sprite_metadata(0, 7, limits=limits)
            except ValueError:
                pass
            else:
                raise AssertionError("Sprite metadata limits should be enforced")

        sprite_atlas = sprite_studio.read_sprite_atlas(0, 9)
        assert isinstance(sprite_atlas, SpriteAtlas)
        assert sprite_atlas.path_id == 9
        assert sprite_atlas.name == "python atlas"
        assert sprite_atlas.packed_sprites == [(0, 7)]
        assert sprite_atlas.packed_sprite_names == ["python sprite"]
        assert sprite_atlas.tag == "python"
        assert sprite_atlas.is_variant is False
        assert len(sprite_atlas.render_data_entries) == 1
        atlas_entry = sprite_atlas.render_data_entries[0]
        assert isinstance(atlas_entry, SpriteAtlasRenderData)
        assert isinstance(atlas_entry.key, SpriteAtlasRenderDataKey)
        assert atlas_entry.key.guid_bytes == bytes(16)
        assert atlas_entry.key.value == 0
        assert atlas_entry.texture == (0, 10)
        assert atlas_entry.alpha_texture == (0, 0)
        assert atlas_entry.texture_rect == (0.0, 0.0, 1.0, 1.0)
        assert atlas_entry.texture_rect_offset == (0.0, 0.0)
        assert atlas_entry.atlas_rect_offset == (0.0, 0.0)
        assert atlas_entry.uv_transform == (0.0, 0.0, 1.0, 1.0)
        assert atlas_entry.downscale_multiplier == 1.0
        assert atlas_entry.settings_raw == 2
        assert atlas_entry.packed is False
        assert atlas_entry.packing_mode == 1
        assert atlas_entry.packing_rotation == 0
        assert atlas_entry.mesh_type == 0
        assert atlas_entry.secondary_textures == []
        for kwargs in (
            {"maximum_entries": 0},
            {"maximum_string_bytes": 5},
            {"maximum_total_string_bytes": 12},
        ):
            try:
                sprite_studio.read_sprite_atlas(0, 9, **kwargs)
            except ValueError:
                pass
            else:
                raise AssertionError("SpriteAtlas limits should be enforced")

        sprite_image = sprite_studio.read_sprite(0, 7)
        assert repr(sprite_image) == "RgbaImage(width=1, height=1)"
        assert sprite_image.rgba == bytes((9, 8, 7, 255))
        try:
            sprite_studio.read_sprite(0, 7, maximum_bytes=3)
        except ValueError:
            pass
        else:
            raise AssertionError("sprite output limit should raise ValueError")

        # The sprite-page cache counters are observable: a fresh collection
        # starts at zero, the first decode misses, and repeating the same
        # decode turns every miss into a hit.
        stats_studio = UnityRs(sprite_path)
        assert stats_studio.sprite_page_cache_stats() == (0, 0)
        stats_studio.read_sprite(0, 7)
        first_hits, first_misses = stats_studio.sprite_page_cache_stats()
        assert first_hits == 0 and first_misses >= 1
        stats_studio.read_sprite(0, 7)
        second_hits, second_misses = stats_studio.sprite_page_cache_stats()
        assert second_hits == first_misses and second_misses == first_misses

        tight_sprite_path = Path(directory) / "tight-sprite.assets"
        tight_sprite_path.write_bytes(synthetic_tight_sprite())
        tight_sprite_studio = UnityRs(tight_sprite_path)
        tight_metadata = tight_sprite_studio.read_sprite_metadata(0, 7)
        assert tight_metadata.render_data.settings.packing_mode == "tight"
        assert len(tight_metadata.render_data.mesh_triangles) == 1
        tight_sprite = tight_sprite_studio.read_sprite(0, 7)
        assert (tight_sprite.width, tight_sprite.height) == (2, 2)
        assert tight_sprite.rgba == bytes(
            (30, 3, 3, 255, 0, 0, 0, 0, 10, 1, 1, 255, 20, 2, 2, 255)
        )

        # RgbaImage.encode exposes Core's bounded per-image encoders so a
        # caller can save one decoded texture or sprite without the on-disk
        # export layout or a Python-side encoder.
        encoded_png = tight_sprite.encode()
        assert encoded_png[:8] == b"\x89PNG\r\n\x1a\n"
        # IHDR dimensions prove the pixels were encoded as this 2x2 image.
        assert encoded_png[16:24] == (2).to_bytes(4, "big") * 2
        encoded_raw = tight_sprite.encode(" Raw-RGBA ")
        assert encoded_raw[:16] == b"HARUKI_RGBAIR_V1"
        encoded_qoi = tight_sprite.encode("qoi")
        assert encoded_qoi[:4] == b"qoif"
        assert encoded_qoi[4:12] == (2).to_bytes(4, "big") * 2
        # The zlib effort and scanline filter change the compressed stream,
        # never the pixels: every choice stays a valid PNG carrying the same
        # IHDR geometry, and an explicit numeric level is accepted.
        for kwargs in (
            {"compression": " Fast "},
            {"compression": 0},
            {"compression": 9},
            {"png_filter": "auto"},
            {"png_filter": " Adaptive "},
        ):
            encoded_variant = tight_sprite.encode("png", **kwargs)
            assert encoded_variant[:8] == b"\x89PNG\r\n\x1a\n"
            assert encoded_variant[16:24] == encoded_png[16:24]
        # The JPEG knobs produce decodable streams that differ from the
        # baseline, and the background composite accepts an RGB tuple.
        encoded_jpeg = tight_sprite.encode("jpeg")
        assert encoded_jpeg[:2] == b"\xff\xd8"
        for kwargs in (
            {"jpeg_sampling": "4:4:4"},
            {"jpeg_progressive": True},
            {"jpeg_optimized_huffman": True},
            {"jpeg_background": (255, 255, 255)},
        ):
            variant = tight_sprite.encode("jpeg", **kwargs)
            assert variant[:2] == b"\xff\xd8"
            assert variant != encoded_jpeg
        for failing_call in (
            lambda: tight_sprite.encode("gif"),
            lambda: tight_sprite.encode("jpeg", jpeg_quality=0),
            lambda: tight_sprite.encode("jpeg", jpeg_sampling="4:1:1"),
            lambda: tight_sprite.encode("png", compression="turbo"),
            lambda: tight_sprite.encode("png", compression=10),
            lambda: tight_sprite.encode("png", png_filter="paeth"),
        ):
            try:
                failing_call()
            except ValueError:
                pass
            else:
                raise AssertionError("encode should enforce format and quality")
        # A mid-stream PNG budget violation keeps its I/O error family, the
        # same classification the streaming export writer reports.
        try:
            tight_sprite.encode(maximum_bytes=8)
        except OSError as error:
            assert "exceed" in str(error)
        else:
            raise AssertionError("encode should enforce the output budget")

        # Batch export must reach the same PNG effort/filter knobs as
        # RgbaImage.encode: fast selects the fdeflate path and produces a
        # different, still-valid stream than the default effort.
        default_png_output = Path(directory) / "png-default-export"
        default_png = texture_studio.export(default_png_output)
        fast_png_output = Path(directory) / "png-fast-export"
        fast_png = texture_studio.export(fast_png_output, compression="fast")
        assert default_png.failures == [] and fast_png.failures == []
        default_bytes = Path(default_png.exported[0]).read_bytes()
        fast_bytes = Path(fast_png.exported[0]).read_bytes()
        assert default_bytes[:8] == b"\x89PNG\r\n\x1a\n"
        assert fast_bytes[:8] == b"\x89PNG\r\n\x1a\n"
        assert default_bytes != fast_bytes
        try:
            texture_studio.export(
                Path(directory) / "png-bad-export", compression="turbo"
            )
        except ValueError:
            pass
        else:
            raise AssertionError("export should reject unknown PNG compression")

        webp_output = Path(directory) / "webp-export"
        webp_report = texture_studio.export(webp_output, image_format=" WeBp ")
        assert webp_report.failures == []
        assert len(webp_report.exported) == 1
        webp_path = Path(webp_report.exported[0])
        webp_bytes = webp_path.read_bytes()
        assert webp_path.name == "image.webp"
        assert webp_bytes[:4] == b"RIFF"
        assert webp_bytes[8:12] == b"WEBP"
        assert webp_bytes[12:16] == b"VP8L"

        jpeg_output = Path(directory) / "jpeg-export"
        jpeg_report = texture_studio.export(
            jpeg_output,
            image_format="jpeg",
            jpeg_quality=100,
        )
        assert jpeg_report.failures == []
        assert len(jpeg_report.exported) == 1
        jpeg_path = Path(jpeg_report.exported[0])
        jpeg_bytes = jpeg_path.read_bytes()
        assert jpeg_path.name == "image.jpg"
        assert jpeg_bytes[:2] == b"\xff\xd8"
        assert jpeg_bytes[-2:] == b"\xff\xd9"

        for bad_quality in (-1, 0, 101, 1000):
            try:
                texture_studio.export(
                    Path(directory) / f"bad-jpeg-quality-{bad_quality}",
                    image_format="jpeg",
                    jpeg_quality=bad_quality,
                )
            except ValueError:
                pass
            else:
                raise AssertionError(
                    f"JPEG quality {bad_quality} should raise ValueError"
                )

        audio_path = Path(directory) / "audio.assets"
        audio_path.write_bytes(synthetic_legacy_pcm())
        audio_studio = UnityRs(audio_path)
        audio = audio_studio.read_audio_clip(0, 7)
        assert isinstance(audio, AudioClip)
        assert audio.name == "legacy-pcm"
        assert audio.extension == ".wav"
        assert audio.payload_kind == "audio_wav"
        assert audio.data[:12] == b"RIFF(\0\0\0WAVE"
        assert audio.data[36:44] == b"data\x04\0\0\0"
        assert audio.data[44:] == b"\x01\x02\x03\x04"

        raw_audio = audio_studio.read_audio_clip(0, 7, format=" RaW ")
        assert raw_audio.extension == ".AudioClip"
        assert raw_audio.payload_kind == "audio_raw"
        assert raw_audio.data == b"\x01\x02\x03\x04"
        for invalid_format in ("flac", "wav"):
            try:
                if invalid_format == "wav":
                    audio_studio.read_audio_clip(
                        0,
                        7,
                        format=invalid_format,
                        maximum_bytes=47,
                    )
                else:
                    audio_studio.read_audio_clip(0, 7, format=invalid_format)
            except (ValueError, NotImplementedError):
                pass
            else:
                raise AssertionError(
                    f"audio format {invalid_format} should fail in this test"
                )

        oversized_option = "é" * 2048
        try:
            audio_studio.read_audio_clip(0, 7, format=oversized_option)
        except ValueError as error:
            message = str(error)
            assert "unsupported audio format value of 4096 UTF-8 bytes" in message
            assert oversized_option not in message
        else:
            raise AssertionError("an oversized audio format should be rejected")

        audio_export = audio_studio.export(Path(directory) / "audio-export")
        assert audio_export.failures == []
        assert len(audio_export.exported) == 1
        assert Path(audio_export.exported[0]).suffix == ".wav"

        fsb5_path = Path(directory) / "fsb5-pcm.assets"
        fsb5_path.write_bytes(synthetic_fsb5_pcm())
        fsb5_audio = UnityRs(fsb5_path).read_audio_clip(0, 7)
        assert fsb5_audio.name == "fsb5-pcm"
        assert fsb5_audio.extension == ".wav"
        assert fsb5_audio.payload_kind == "audio_wav"
        assert fsb5_audio.data[:12] == b"RIFF(\0\0\0WAVE"
        assert fsb5_audio.data[20:24] == struct.pack("<HH", 1, 2)
        assert fsb5_audio.data[24:28] == struct.pack("<I", 44_100)
        assert fsb5_audio.data[44:] == b"\x01\x02\x03\x04"
        fsb5_raw = UnityRs(fsb5_path).read_audio_clip(0, 7, format="raw")
        assert fsb5_raw.extension == ".fsb"
        assert fsb5_raw.data[:4] == b"FSB5"

        fsb5_ima_path = Path(directory) / "fsb5-ima.assets"
        fsb5_ima_path.write_bytes(synthetic_fsb5_ima())
        fsb5_ima = UnityRs(fsb5_ima_path).read_audio_clip(0, 7)
        assert fsb5_ima.name == "fsb5-ima"
        assert fsb5_ima.extension == ".wav"
        assert fsb5_ima.payload_kind == "audio_wav"
        assert fsb5_ima.data[:4] == b"RIFF"
        assert fsb5_ima.data[20:24] == struct.pack("<HH", 1, 1)
        assert len(fsb5_ima.data) == 44 + 64 * 2
        assert struct.unpack("<hh", fsb5_ima.data[44:48]) == (1000, 1002)

        fsb5_dsp_path = Path(directory) / "fsb5-dsp.assets"
        fsb5_dsp_path.write_bytes(synthetic_fsb5_dsp())
        fsb5_dsp = UnityRs(fsb5_dsp_path).read_audio_clip(0, 7)
        assert fsb5_dsp.name == "fsb5-dsp"
        assert fsb5_dsp.extension == ".wav"
        assert fsb5_dsp.payload_kind == "audio_wav"
        assert fsb5_dsp.data[:4] == b"RIFF"
        assert fsb5_dsp.data[20:24] == struct.pack("<HH", 1, 1)
        assert len(fsb5_dsp.data) == 44 + 14 * 2
        assert struct.unpack("<hh", fsb5_dsp.data[44:48]) == (1, 3)

        fsb5_vag_path = Path(directory) / "fsb5-vag.assets"
        fsb5_vag_path.write_bytes(synthetic_fsb5_vag())
        fsb5_vag = UnityRs(fsb5_vag_path).read_audio_clip(0, 7)
        assert fsb5_vag.name == "fsb5-vag"
        assert fsb5_vag.extension == ".wav"
        assert fsb5_vag.payload_kind == "audio_wav"
        assert fsb5_vag.data[:4] == b"RIFF"
        assert fsb5_vag.data[20:24] == struct.pack("<HH", 1, 1)
        assert len(fsb5_vag.data) == 44 + 56 * 2
        assert struct.unpack("<hh", fsb5_vag.data[44:48]) == (1, 2)
        assert struct.unpack("<h", fsb5_vag.data[100:102]) == (2,)

        fsb5_hevag_path = Path(directory) / "fsb5-hevag.assets"
        fsb5_hevag_path.write_bytes(synthetic_fsb5_hevag())
        fsb5_hevag = UnityRs(fsb5_hevag_path).read_audio_clip(0, 7)
        assert fsb5_hevag.name == "fsb5-hevag"
        assert fsb5_hevag.extension == ".wav"
        assert fsb5_hevag.payload_kind == "audio_wav"
        assert fsb5_hevag.data[:4] == b"RIFF"
        assert fsb5_hevag.data[20:24] == struct.pack("<HH", 1, 1)
        assert len(fsb5_hevag.data) == 44 + 56 * 2
        assert struct.unpack("<hh", fsb5_hevag.data[44:48]) == (1, 2)
        assert struct.unpack("<h", fsb5_hevag.data[100:102]) == (2,)

        fsb5_fadpcm_path = Path(directory) / "fsb5-fadpcm.assets"
        fsb5_fadpcm_path.write_bytes(synthetic_fsb5_fadpcm())
        fsb5_fadpcm = UnityRs(fsb5_fadpcm_path).read_audio_clip(0, 7)
        assert fsb5_fadpcm.name == "fsb5-fadpcm"
        assert fsb5_fadpcm.extension == ".wav"
        assert fsb5_fadpcm.payload_kind == "audio_wav"
        assert fsb5_fadpcm.data[:4] == b"RIFF"
        assert fsb5_fadpcm.data[20:24] == struct.pack("<HH", 1, 1)
        assert len(fsb5_fadpcm.data) == 44 + 512 * 2
        assert struct.unpack("<hh", fsb5_fadpcm.data[44:48]) == (1, 2)
        assert struct.unpack("<h", fsb5_fadpcm.data[556:558]) == (2,)

        fsb5_mpeg_path = Path(directory) / "fsb5-mpeg.assets"
        fsb5_mpeg_path.write_bytes(synthetic_fsb5_mpeg())
        fsb5_mpeg = UnityRs(fsb5_mpeg_path).read_audio_clip(0, 7)
        assert fsb5_mpeg.name == "fsb5-mpeg"
        assert fsb5_mpeg.extension == ".wav"
        assert fsb5_mpeg.payload_kind == "audio_wav"
        assert fsb5_mpeg.data[:4] == b"RIFF"
        assert fsb5_mpeg.data[20:24] == struct.pack("<HH", 1, 1)
        assert len(fsb5_mpeg.data) == 44 + 2304 * 2
        assert set(fsb5_mpeg.data[44:]) == {0}

        fsb5_opus_path = Path(directory) / "fsb5-opus.assets"
        fsb5_opus_path.write_bytes(synthetic_fsb5_opus())
        fsb5_opus = UnityRs(fsb5_opus_path).read_audio_clip(0, 7)
        assert fsb5_opus.name == "fsb5-opus"
        assert fsb5_opus.extension == ".wav"
        assert fsb5_opus.payload_kind == "audio_wav"
        assert fsb5_opus.data[:4] == b"RIFF"
        assert fsb5_opus.data[20:24] == struct.pack("<HH", 1, 1)
        assert fsb5_opus.data[24:28] == struct.pack("<I", 48_000)
        assert len(fsb5_opus.data) == 44 + 648 * 2
        assert set(fsb5_opus.data[44:]) == {0}

        fsb5_vorbis_path = Path(directory) / "fsb5-vorbis.assets"
        fsb5_vorbis_path.write_bytes(synthetic_fsb5_vorbis())
        fsb5_vorbis = UnityRs(fsb5_vorbis_path).read_audio_clip(0, 7)
        assert fsb5_vorbis.name == "fsb5-vorbis"
        assert fsb5_vorbis.extension == ".wav"
        assert fsb5_vorbis.payload_kind == "audio_wav"
        assert fsb5_vorbis.data[:4] == b"RIFF"
        assert fsb5_vorbis.data[20:24] == struct.pack("<HH", 1, 2)
        assert fsb5_vorbis.data[24:28] == struct.pack("<I", 48_000)
        assert len(fsb5_vorbis.data) == 44 + 4800 * 2 * 2
        assert set(fsb5_vorbis.data[44:]) != {0}

        font_path = Path(directory) / "font.assets"
        font_path.write_bytes(synthetic_font())
        font = UnityRs(font_path).read_font(0, 7)
        assert isinstance(font, BinaryAsset)
        assert repr(font) == (
            'BinaryAsset(name="python-font", extension=".otf", '
            'payload_kind="font", bytes=8)'
        )
        assert (font.name, font.extension, font.payload_kind, font.data) == (
            "python-font",
            ".otf",
            "font",
            b"OTTOfont",
        )

        movie_path = Path(directory) / "movie.assets"
        movie_path.write_bytes(synthetic_movie_texture())
        movie = UnityRs(movie_path).read_movie_texture(0, 7)
        assert (movie.name, movie.extension, movie.payload_kind, movie.data) == (
            "python-movie",
            ".ogv",
            "movie_ogv",
            b"OggS",
        )

        video_path = Path(directory) / "video.assets"
        video_path.write_bytes(synthetic_video_clip())
        video_studio = UnityRs(video_path)
        video = video_studio.read_video_clip(0, 7)
        assert (video.name, video.extension, video.payload_kind, video.data) == (
            "python-video",
            ".mp4",
            "video_raw",
            b"video-bin",
        )
        try:
            video_studio.read_video_clip(0, 7, maximum_bytes=8)
        except ValueError:
            pass
        else:
            raise AssertionError("VideoClip payload limit should raise ValueError")

        material_path = Path(directory) / "material.assets"
        material_path.write_bytes(synthetic_material())
        material_studio = UnityRs(material_path)
        material = material_studio.read_material(0, 7)
        assert isinstance(material, Material)
        assert material.path_id == 7
        assert material.name == "python-material"
        assert material.shader == (1, 42)
        assert material.legacy_shader_keywords == []
        assert material.valid_keywords == ["FOO", "BAR"]
        assert material.invalid_keywords == ["OLD"]
        assert material.lightmap_flags == 3
        assert material.enable_instancing_variants is True
        assert material.custom_render_queue == 2450
        assert material.string_tags == [
            ("RenderType", "Opaque"),
            ("RenderType", "Cutout"),
        ]
        assert material.disabled_shader_passes == ["ShadowCaster"]
        assert material.texture_environments == [
            ("_MainTex", (0, 9), (2.0, 3.0), (0.25, 0.5))
        ]
        assert material.integers == [("_Mode", 1), ("_Mode", 2)]
        assert material.floats == [("_Glossiness", 0.75)]
        assert material.colors == [("_Color", (1.0, 0.5, 0.25, 1.0))]
        assert material.trailing_bytes == 4
        try:
            material_studio.read_material(0, 7, maximum_array_elements=0)
        except ValueError:
            pass
        else:
            raise AssertionError("Material array limit should raise ValueError")

        scene_path = Path(directory) / "scene.assets"
        scene_path.write_bytes(synthetic_game_object())
        scene_limits = SceneLimits(maximum_game_objects=1)
        assert scene_limits.maximum_game_objects == 1
        assert scene_limits.maximum_total_components == 10_000_000
        assert scene_limits.maximum_index_bytes == 268_435_456
        scene_studio = UnityRs(scene_path)
        scene = scene_studio.scene(limits=scene_limits)
        assert len(scene) == 1
        assert scene[0].file_index == 0
        assert scene[0].path_id == 7
        assert scene[0].name == "Python Root"
        assert scene[0].parent is None
        assert scene[0].children == []
        assert scene[0].local_position is None
        assert scene[0].local_rotation is None
        assert scene[0].local_scale is None
        assert scene[0].mesh is None
        assert scene[0].materials == []
        assert scene[0].bones == []
        assert scene[0].animator is None
        try:
            scene_studio.scene(limits=SceneLimits(maximum_game_objects=0))
        except ValueError:
            pass
        else:
            raise AssertionError("scene GameObject limit should be enforced")
        try:
            scene_studio.scene(limits=SceneLimits(maximum_index_bytes=0))
        except ValueError:
            pass
        else:
            raise AssertionError("scene index-byte limit should be enforced")

        model_path = Path(directory) / "model.assets"
        model_path.write_bytes(synthetic_static_model())
        fbx = UnityRs(model_path).read_static_fbx(maximum_bytes=128 * 1024)
        # The same scene in the other encoding, which had no binding at all
        # until the writer was wired up. Checked against the format's own magic
        # and version word rather than against bytes this project produced.
        binary_fbx = UnityRs(model_path).read_static_fbx_binary(
            maximum_bytes=128 * 1024
        )
        assert binary_fbx.startswith(b"Kaydara FBX Binary  \0"), binary_fbx[:32]
        assert struct.unpack_from("<I", binary_fbx, 23)[0] == 7400
        assert b"Geometry" in binary_fbx
        assert binary_fbx != fbx
        model_studio = UnityRs(model_path)
        split_candidates = model_studio.split_object_fbx_candidates()
        assert len(split_candidates) == 1
        assert isinstance(split_candidates[0], FbxCandidate)
        assert split_candidates[0].file_index == 0
        assert split_candidates[0].path_id == 1
        assert split_candidates[0].name == "python model"
        assert split_candidates[0].animator is None
        assert model_studio.animator_fbx_candidates() == []
        selected_fbx = model_studio.read_game_object_fbx(
            0,
            1,
            include_animations=False,
            maximum_bytes=128 * 1024,
            acl_decoder=lambda *_args: (_ for _ in ()).throw(
                AssertionError("static FBX must not invoke the ACL decoder")
            ),
        )
        assert selected_fbx.startswith(b"; FBX 7.4.0 project file\n")
        assert fbx.startswith(b"; FBX 7.4.0 project file\n")
        assert b"Model::python model" in fbx
        assert b"Geometry::python triangle" in fbx
        assert b'P: "Lcl Rotation", "Lcl Rotation", "", "A",0,0,-45' in fbx
        assert b'P: "Lcl Scaling", "Lcl Scaling", "", "A",2,3,4' in fbx
        assert b"a: 2,1,-1" in fbx
        # The scene as OBJ, which existed only inside the CLI: a library
        # caller could reach the FBX scene but not this one. The `mtllib` line
        # has to name the library the caller is told to write, or the material
        # reference resolves to nothing.
        model_obj = UnityRs(model_path).read_model_obj(
            material_library_name="python-model.mtl", maximum_bytes=128 * 1024
        )
        assert model_obj.material_library_name == "python-model.mtl"
        assert b"mtllib python-model.mtl" in model_obj.obj
        assert b"v " in model_obj.obj and b"f " in model_obj.obj
        # One `newmtl` per submesh slot, which is what the OBJ's `usemtl`
        # lines refer to; an empty library would leave every face unmaterialed.
        assert model_obj.material_library.startswith(b"newmtl ")
        assert b"usemtl " in model_obj.obj
        exact_model_limit = max(len(model_obj.obj), len(model_obj.material_library))
        exact_model_obj = UnityRs(model_path).read_model_obj(
            material_library_name="python-model.mtl",
            maximum_bytes=exact_model_limit,
        )
        assert exact_model_obj.obj == model_obj.obj
        assert exact_model_obj.material_library == model_obj.material_library
        try:
            UnityRs(model_path).read_model_obj(
                material_library_name="python-model.mtl",
                maximum_bytes=exact_model_limit - 1,
            )
        except ValueError as error:
            assert "exceeds" in str(error)
        else:
            raise AssertionError("model OBJ/MTL output limits must be enforced")
        # This fixture has no material textures, which is an empty list rather
        # than an error, and nothing was skipped for a reason worth reporting.
        assert model_obj.textures == []
        assert model_obj.skipped == []
        # The same scene as FBX plus its textures, which Python could not
        # reach either.
        textured = UnityRs(model_path).read_fbx_with_textures(
            maximum_bytes=128 * 1024
        )
        assert textured.fbx.startswith(b"; FBX 7.4.0 project file\n")
        assert textured.textures == []
        exact_textured = UnityRs(model_path).read_fbx_with_textures(
            maximum_bytes=len(textured.fbx)
        )
        assert exact_textured.fbx == textured.fbx
        try:
            UnityRs(model_path).read_fbx_with_textures(
                maximum_bytes=len(textured.fbx) - 1
            )
        except ValueError as error:
            assert "exceeds" in str(error)
        else:
            raise AssertionError("textured FBX output limits must be enforced")

        # Core and the CLI already accept every image encoding for model
        # textures. The Python model APIs must pass that choice through rather
        # than silently forcing PNG.
        textured_model_path = Path(directory) / "textured-model.assets"
        textured_model_path.write_bytes(synthetic_textured_model())
        raw_model = UnityRs(textured_model_path).read_model_obj(
            texture_format="raw-rgba", maximum_bytes=128 * 1024
        )
        assert len(raw_model.textures) == 1
        assert raw_model.textures[0].file_name.endswith(".rgba")
        assert raw_model.textures[0].data.startswith(b"HARUKI_RGBAIR_V1")
        default_texture_limits = ModelTextureLimits()
        assert default_texture_limits.maximum_texture_references == 1_000_000
        assert default_texture_limits.maximum_textures == 4_096
        assert default_texture_limits.maximum_name_index_bytes == 67_108_864
        assert default_texture_limits.maximum_metadata_bytes == 268_435_456
        assert default_texture_limits.maximum_total_encoded_bytes == 2_147_483_648
        assert default_texture_limits.maximum_single_texture_bytes == 536_870_912
        try:
            UnityRs(textured_model_path).read_model_obj(
                texture_format="raw-rgba",
                maximum_bytes=128 * 1024,
                texture_limits=ModelTextureLimits(maximum_textures=0),
            )
        except ValueError as error:
            assert "more than 0 textures" in str(error)
        else:
            raise AssertionError("the model texture-count budget must be enforced")
        try:
            UnityRs(textured_model_path).read_model_obj(
                texture_format="raw-rgba",
                maximum_bytes=128 * 1024,
                texture_limits=ModelTextureLimits(maximum_texture_references=0),
            )
        except ValueError as error:
            assert "non-null texture references" in str(error)
        else:
            raise AssertionError("the model texture-reference budget must be enforced")
        try:
            UnityRs(textured_model_path).read_model_obj(
                texture_format="raw-rgba",
                maximum_bytes=128 * 1024,
                texture_limits=ModelTextureLimits(maximum_metadata_bytes=7),
            )
        except ValueError as error:
            assert "metadata requires 8 UTF-8 bytes" in str(error)
        else:
            raise AssertionError("the model texture metadata budget must be enforced")
        texture_name_index_bytes = len(raw_model.textures[0].file_name.encode("utf-8")) * 2
        try:
            UnityRs(textured_model_path).read_model_obj(
                texture_format="raw-rgba",
                maximum_bytes=128 * 1024,
                texture_limits=ModelTextureLimits(
                    maximum_name_index_bytes=texture_name_index_bytes - 1
                ),
            )
        except ValueError as error:
            assert (
                f"name indexes require {texture_name_index_bytes} UTF-8 bytes"
                in str(error)
            )
        else:
            raise AssertionError("the model texture name-index budget must be enforced")
        try:
            UnityRs(textured_model_path).read_fbx_with_textures(
                texture_format="raw-rgba",
                maximum_bytes=128 * 1024,
                texture_limits=ModelTextureLimits(
                    maximum_total_encoded_bytes=len(raw_model.textures[0].data) - 1
                ),
            )
        except ValueError as error:
            assert "byte budget" in str(error)
        else:
            raise AssertionError("the aggregate model texture budget must be enforced")
        limited_model = UnityRs(textured_model_path).read_model_obj(
            texture_format="raw-rgba",
            maximum_bytes=128 * 1024,
            texture_limits=ModelTextureLimits(
                maximum_single_texture_bytes=len(raw_model.textures[0].data) - 1
            ),
        )
        assert limited_model.textures == []
        assert len(limited_model.skipped) == 1
        assert "limit" in limited_model.skipped[0].lower()
        tga_model = UnityRs(textured_model_path).read_fbx_with_textures(
            texture_format="tga", maximum_bytes=128 * 1024
        )
        assert len(tga_model.textures) == 1
        assert tga_model.textures[0].file_name.endswith(".tga")
        assert tga_model.textures[0].file_name.encode() in tga_model.fbx
        try:
            UnityRs(textured_model_path).read_model_obj(
                texture_format="not-an-image-format"
            )
        except ValueError:
            pass
        else:
            raise AssertionError("unknown model texture formats must be rejected")

        animated_fbx = UnityRs(model_path).read_fbx(maximum_bytes=128 * 1024)
        animated_binary = UnityRs(model_path).read_fbx_binary(
            maximum_bytes=128 * 1024
        )
        assert animated_binary.startswith(b"Kaydara FBX Binary  \0")
        assert struct.unpack_from("<I", animated_binary, 23)[0] == 7400
        assert animated_fbx.startswith(b"; FBX 7.4.0 project file\n")
        assert b'ObjectType: "AnimationStack" { Count: 0 }' in animated_fbx
        assert b"Geometry::python triangle" in animated_fbx
        for read_fbx in (
            lambda: model_studio.read_fbx(acl_decoder=object()),
            lambda: model_studio.read_game_object_fbx(0, 1, acl_decoder=object()),
        ):
            try:
                read_fbx()
            except TypeError:
                pass
            else:
                raise AssertionError("non-callable FBX ACL decoders should be rejected")
        tuanjie_model_path = Path(directory) / "model-tuanjie.assets"
        tuanjie_model_path.write_bytes(synthetic_static_model(tuanjie=True))
        tuanjie_fbx = UnityRs(tuanjie_model_path).read_fbx(
            maximum_bytes=128 * 1024
        )
        assert b"Model::python model" in tuanjie_fbx
        assert b"Geometry::python triangle" in tuanjie_fbx
        assert b"a: 2,1,-1" in tuanjie_fbx
        try:
            UnityRs(model_path).read_static_fbx(maximum_bytes=64)
        except ValueError:
            pass
        else:
            raise AssertionError("FBX output limit should raise ValueError")

        build_path = Path(directory) / "build-settings.assets"
        build_path.write_bytes(synthetic_build_settings())
        build = UnityRs(build_path).read_build_settings(0, 7)
        assert isinstance(build, BuildSettings)
        assert build.path_id == 7
        assert build.levels is None
        assert build.scenes == ["Assets/Intro.unity", "Assets/Game.unity"]

        player_path = Path(directory) / "player-settings.assets"
        player_path.write_bytes(synthetic_player_settings())
        player = UnityRs(player_path).read_player_settings(0, 7)
        assert isinstance(player, PlayerSettings)
        assert player.path_id == 7
        assert player.company_name == "Haruki"
        assert player.product_name == "Asset Studio"

        animation_path = Path(directory) / "tuanjie-animation.assets"
        animation_path.write_bytes(synthetic_tuanjie_animation_clip())
        animation = UnityRs(animation_path).read_animation_clip(0, 7)
        assert isinstance(animation, AnimationClip)
        assert animation.path_id == 7
        assert animation.name == "python-tuanjie-animation"
        assert not animation.legacy
        assert animation.sample_rate == 60.0
        assert animation.euler_curve_count == 0
        assert animation.muscle_present
        assert animation.streamed_curve_count == 2
        assert animation.acl_present
        assert animation.acl_frame_count == 12
        assert animation.acl_bone_count == 3
        assert animation.acl_sample_rate == 30.0
        assert animation.acl_curve_count == 7
        assert animation.acl_track_byte_count == 32
        assert animation.acl_decoder_count == 2
        assert animation.acl_use_fast_sample_mode is True
        acl = UnityRs(animation_path).inspect_acl_tracks(0, 7)
        assert isinstance(acl, AclCompressedTracks)
        assert acl.declared_size == 32
        assert acl.version == 10
        assert acl.track_type == "qvvf"
        assert acl.num_tracks == 3
        assert acl.num_samples_per_track == 12
        assert acl.sample_rate == 30.0
        assert acl.decompressed_value_count == 360
        assert not acl.has_metadata
        assert not acl.is_wrap_optimized
        assert not acl.has_database
        assert not acl.has_stripped_keyframes
        acl_blob, decoder_map = UnityRs(animation_path).read_acl_decoder_input(0, 7)
        assert acl_blob == synthetic_acl_tracks()
        assert decoder_map == [0x10, 0x20]

        def decode_acl(
            compressed_tracks: bytes,
            map_values: list[int],
            frame_count: int,
            bone_count: int,
            sample_rate: float,
            declared_curve_count: Optional[int],
            fast_sample: Optional[bool],
        ) -> tuple[list[float], list[int], list[float], int]:
            assert compressed_tracks == synthetic_acl_tracks()
            assert map_values == [0x10, 0x20]
            assert frame_count == 12
            assert bone_count == 3
            assert sample_rate == 30.0
            assert declared_curve_count == 7
            assert fast_sample is True
            return (
                [index / sample_rate for index in range(frame_count)],
                list(range(declared_curve_count)),
                [float(index) for index in range(frame_count * declared_curve_count)],
                declared_curve_count,
            )

        decoded = UnityRs(animation_path).decode_acl_tracks(0, 7, decode_acl)
        assert isinstance(decoded, AclDecodedClip)
        assert len(decoded.times) == 12
        assert decoded.binding_indices == list(range(7))
        assert len(decoded.values) == 84
        assert decoded.values[-1] == 83.0
        assert decoded.following_curve_offset == 7
        try:
            UnityRs(animation_path).decode_acl_tracks(0, 7, object())
        except TypeError:
            pass
        else:
            raise AssertionError("non-callable ACL decoders should be rejected")
        try:
            UnityRs(animation_path).decode_acl_tracks(
                0,
                7,
                lambda *_args: ([0.0], [0], [float("nan")], 1),
            )
        except ValueError:
            pass
        else:
            raise AssertionError("invalid ACL decoder output should be rejected")
        try:
            UnityRs(animation_path).decode_acl_tracks(
                0,
                7,
                lambda *_args: ([None] * 13, [None] * 7, [None] * 84, 7),
            )
        except ValueError as error:
            assert "returned 13 times for 12 declared frames" in str(error)
        else:
            raise AssertionError(
                "ACL list lengths must be checked before element conversion"
            )

        acl_limit_decoder_called = False

        def must_not_decode_acl(
            *_args: object,
        ) -> tuple[list[float], list[int], list[float], int]:
            nonlocal acl_limit_decoder_called
            acl_limit_decoder_called = True
            return ([], [], [], 0)

        try:
            UnityRs(animation_path).decode_acl_tracks(
                0, 7, must_not_decode_acl, maximum_values=83
            )
        except ValueError as error:
            assert "requires 84 values, exceeding limit 83" in str(error)
        else:
            raise AssertionError("ACL decoded output limit should be enforced")
        assert not acl_limit_decoder_called
        try:
            UnityRs(animation_path).inspect_acl_tracks(
                0, 7, maximum_decompressed_values=359
            )
        except ValueError:
            pass
        else:
            raise AssertionError("ACL implied output limit should be enforced")
        for kwargs in (
            {"maximum_decoder_map_entries": 1},
            {"maximum_materialized_bytes": 39},
        ):
            try:
                UnityRs(animation_path).read_acl_decoder_input(0, 7, **kwargs)
            except ValueError:
                pass
            else:
                raise AssertionError("ACL decoder input limits should be enforced")
        assert animation.streaming_offset == 0x1020304050607080
        assert animation.streaming_size == 0x1234
        assert animation.streaming_path == "archive:/animation.resS"
        try:
            UnityRs(animation_path).read_animation_clip(0, 7, maximum_bytes=1)
        except ValueError:
            pass
        else:
            raise AssertionError("AnimationClip object limit should be enforced")

        standard_motion_path = Path(directory) / "standard-motion.assets"
        standard_motion_path.write_bytes(synthetic_standard_animation_clip())
        standard_motion_studio = UnityRs(standard_motion_path)
        standard_motion = standard_motion_studio.read_cubism_clip_motion(
            0,
            7,
            targets=CubismMotionTargets(parameters=["ParamAngleX"]),
        )
        assert isinstance(standard_motion, CubismClipMotion)
        assert standard_motion.file_index == 0
        assert standard_motion.path_id == 7
        assert standard_motion.name == "python-standard-animation"
        assert standard_motion.fps == 60.0
        assert standard_motion.curve_count == 0
        assert standard_motion.keyframe_count == 0
        standard_motion_json = json.loads(standard_motion.json)
        assert standard_motion_json["Meta"]["CurveCount"] == 0
        exact_standard_motion = standard_motion_studio.read_cubism_clip_motion(
            0,
            7,
            maximum_output_bytes=len(standard_motion.json),
        )
        assert exact_standard_motion.json == standard_motion.json
        try:
            standard_motion_studio.read_cubism_clip_motion(
                0,
                7,
                maximum_output_bytes=len(standard_motion.json) - 1,
            )
        except ValueError:
            pass
        else:
            raise AssertionError("standard Cubism clip output limits must be enforced")

        acl_motion_path = Path(directory) / "acl-motion.assets"
        acl_motion_path.write_bytes(
            synthetic_tuanjie_animation_clip(cubism_binding=True)
        )
        acl_motion_studio = UnityRs(acl_motion_path)

        def decode_cubism_acl(
            compressed_tracks: bytes,
            map_values: list[int],
            frame_count: int,
            bone_count: int,
            sample_rate: float,
            declared_curve_count: Optional[int],
            fast_sample: Optional[bool],
        ) -> tuple[list[float], list[int], list[float], int]:
            assert compressed_tracks == synthetic_acl_tracks()
            assert map_values == [0x10, 0x20]
            assert frame_count == 12
            assert bone_count == 3
            assert sample_rate == 30.0
            assert declared_curve_count == 1
            assert fast_sample is True
            return (
                [index / sample_rate for index in range(frame_count)],
                [0],
                [float(index) / 10.0 for index in range(frame_count)],
                1,
            )

        acl_motion = acl_motion_studio.read_cubism_acl_clip_motion(
            0,
            7,
            decode_cubism_acl,
            targets=CubismMotionTargets(parameters=["ParamAngleX"]),
        )
        assert isinstance(acl_motion, CubismClipMotion)
        assert acl_motion.name == "python-tuanjie-animation"
        assert acl_motion.curve_count == 1
        assert acl_motion.keyframe_count == 12
        acl_motion_json = json.loads(acl_motion.json)
        assert acl_motion_json["Meta"]["CurveCount"] == 1
        assert acl_motion_json["Curves"][0]["Target"] == "Parameter"
        assert acl_motion_json["Curves"][0]["Id"] == "ParamAngleX"
        exact_acl_motion = acl_motion_studio.read_cubism_acl_clip_motion(
            0,
            7,
            decode_cubism_acl,
            targets=CubismMotionTargets(parameters=["ParamAngleX"]),
            maximum_output_bytes=len(acl_motion.json),
        )
        assert exact_acl_motion.json == acl_motion.json
        try:
            acl_motion_studio.read_cubism_acl_clip_motion(
                0,
                7,
                decode_cubism_acl,
                targets=CubismMotionTargets(parameters=["ParamAngleX"]),
                maximum_output_bytes=len(acl_motion.json) - 1,
            )
        except ValueError:
            pass
        else:
            raise AssertionError("ACL Cubism clip output limits must be enforced")

        legacy_animation_path = Path(directory) / "legacy-animation.assets"
        legacy_animation_path.write_bytes(synthetic_legacy_animation_component())
        legacy_animation = UnityRs(legacy_animation_path).read_legacy_animation(0, 7)
        assert isinstance(legacy_animation, LegacyAnimation)
        assert legacy_animation.path_id == 7
        assert legacy_animation.game_object == (0, 31)
        assert legacy_animation.enabled == 1
        assert legacy_animation.default_clip == (0, 70)
        assert legacy_animation.clips == [(0, 71), (0, 72)]
        assert legacy_animation.trailing_bytes == 2
        try:
            UnityRs(legacy_animation_path).read_legacy_animation(
                0, 7, maximum_bytes=1
            )
        except ValueError:
            pass
        else:
            raise AssertionError("legacy Animation object limit should be enforced")

        override_path = Path(directory) / "animator-override.assets"
        override_path.write_bytes(synthetic_animator_override_controller())
        override = UnityRs(override_path).read_animator_override_controller(0, 7)
        assert isinstance(override, AnimatorOverrideController)
        assert override.path_id == 7
        assert override.name == "python override controller"
        assert override.controller == (0, 90)
        assert override.clip_overrides == [
            ((0, 71), (0, 73)),
            ((0, 72), (0, 74)),
        ]
        assert override.trailing_bytes == 1
        try:
            UnityRs(override_path).read_animator_override_controller(
                0, 7, maximum_bytes=1
            )
        except ValueError:
            pass
        else:
            raise AssertionError(
                "AnimatorOverrideController object limit should be enforced"
            )

        container_path = Path(directory) / "container-metadata.assets"
        container_path.write_bytes(synthetic_container_metadata_objects())
        container_studio = UnityRs(container_path)
        bundle = container_studio.read_asset_bundle(0, 7)
        assert isinstance(bundle, AssetBundle)
        assert bundle.path_id == 7
        assert bundle.name == "python-bundle"
        assert bundle.object_name == "root"
        assert bundle.asset_bundle_name == "python-bundle"
        assert bundle.preload_table == [(0, 11), (0, 12)]
        assert bundle.container == [
            ("bundle/first", 0, 1, (0, 11)),
            ("bundle/second", 1, 1, (0, 12)),
        ]
        assert bundle.dependencies == ["shared-a", "shared-b"]
        assert bundle.is_streamed_scene_asset_bundle is False

        manager = container_studio.read_resource_manager(0, 8)
        assert isinstance(manager, ResourceManager)
        assert manager.path_id == 8
        assert manager.container == [
            ("resource/first", (0, 21)),
            ("resource/second", (0, 22)),
        ]

        preload = container_studio.read_preload_data(0, 9)
        assert isinstance(preload, PreloadData)
        assert preload.path_id == 9
        assert preload.name == "python-preload"
        assert preload.assets == [(0, 31), (0, 32)]

        for method, path_id, kwargs in (
            (container_studio.read_asset_bundle, 7, {"maximum_entries": 1}),
            (
                container_studio.read_asset_bundle,
                7,
                {"maximum_total_string_bytes": 1},
            ),
            (container_studio.read_resource_manager, 8, {"maximum_entries": 1}),
            (container_studio.read_preload_data, 9, {"maximum_entries": 1}),
        ):
            try:
                method(0, path_id, **kwargs)
            except ValueError:
                pass
            else:
                raise AssertionError("container metadata limits should be enforced")

        controller_path = Path(directory) / "tuanjie-controller.assets"
        controller_path.write_bytes(synthetic_tuanjie_animator_controller())
        controller = UnityRs(controller_path).read_animator_controller(0, 7)
        assert isinstance(controller, AnimatorController)
        assert controller.path_id == 7
        assert controller.name == "python-tuanjie-controller"
        assert controller.layer_count == 0
        assert controller.state_machine_count == 0
        assert controller.value_count == 0
        assert controller.entity_id_count is None
        assert controller.tos == [(0xDEADBEEF, "Root/Hips")]
        assert controller.animation_clips == [(0, 74)]

        avatar_path = Path(directory) / "tuanjie-avatar.assets"
        avatar_path.write_bytes(synthetic_tuanjie_avatar())
        avatar = UnityRs(avatar_path).read_avatar(0, 7)
        assert isinstance(avatar, Avatar)
        assert avatar.path_id == 7
        assert avatar.name == "python-tuanjie-avatar"
        assert avatar.skeleton_node_count == 0
        assert avatar.human_skeleton_node_count == 0
        assert avatar.path_count == 1
        assert avatar.paths == [(0xFEEDBEEF, "Root/Hips")]
        assert avatar.has_human_description
        assert avatar.human_bone_count == 0
        assert avatar.skeleton_bone_count == 0
        assert avatar.root_motion_bone_name == "Hips"

        stripped_path = Path(directory) / "stripped-mono.assets"
        stripped_path.write_bytes(synthetic_stripped_mono_behaviour())
        stripped = UnityRs(stripped_path)
        script = stripped.read_mono_script(0, 8)
        assert isinstance(script, MonoScript)
        assert script.path_id == 8
        assert script.name == "Stats script"
        assert script.execution_order == 0
        assert script.class_name == "Stats"
        assert script.namespace == "Game"
        assert script.assembly_name == "Assembly-CSharp.dll"
        assert script.is_editor_script is None
        try:
            stripped.read_mono_script(0, 8, maximum_string_bytes=4)
        except ValueError:
            pass
        else:
            raise AssertionError("MonoScript string limit should be enforced")
        try:
            stripped.read_mono_script(0, 7)
        except NotImplementedError:
            pass
        else:
            raise AssertionError("MonoScript class mismatch should be rejected")
        schema = MonoBehaviourSchema(
            "folder/assembly-csharp.DLL",
            "Stats",
            stripped_mono_schema_nodes(),
            namespace="Game",
        )
        assert schema.assembly_name == "folder/assembly-csharp.DLL"
        assert schema.namespace == "Game"
        assert schema.class_name == "Stats"
        assert schema.unity_version is None
        assert schema.node_count == len(stripped_mono_schema_nodes())
        exact_schema = MonoBehaviourSchema(
            "Assembly-CSharp.dll",
            "Stats",
            list(mono_behaviour_nodes())
            + [("SInt32", "hit_points", 1, False)],
            namespace="Game",
            unity_version="2022.3.62f1",
        )
        schemas = MonoBehaviourSchemas([schema, exact_schema])
        assert schemas.schema_count == 2
        read = stripped.read_mono_behaviour_json(0, 7, schema)
        # The file ships no tree of its own, so this can only have come from
        # the schema, and the read says so rather than leaving it to be guessed.
        assert read.source == "schema"
        decoded = json.loads(read.json)
        assert decoded["m_Name"] == "Hero"
        assert decoded["score"] == 123
        exact_read = stripped.read_mono_behaviour_json_with_schemas(0, 7, schemas)
        assert exact_read.source == "schema"
        exact_decoded = json.loads(exact_read.json)
        assert exact_decoded["m_Name"] == "Hero"
        assert exact_decoded["hit_points"] == 123
        assert "score" not in exact_decoded
        try:
            stripped.read_mono_behaviour_json(0, 7, schema, maximum_bytes=1)
        except ValueError:
            pass
        else:
            raise AssertionError("MonoBehaviour JSON output limit should be enforced")
        schema_packages = stripped.read_live2d_packages(schemas=schemas)
        assert schema_packages.packages == []

        expression_path = Path(directory) / "expression.assets"
        expression_path.write_bytes(synthetic_cubism_expression())
        expression = UnityRs(expression_path).read_cubism_expression(0, 7)
        assert isinstance(expression, CubismExpression)
        assert expression.path_id == 7
        assert expression.source_name == "smile.exp3"
        assert expression.expression_type == "Live2D Expression"
        assert expression.fade_in_time == 0.5
        assert expression.fade_out_time == 0.75
        assert len(expression.parameters) == 1
        parameter = expression.parameters[0]
        assert isinstance(parameter, CubismExpressionParameter)
        assert parameter.id == "ParamAngleX"
        assert parameter.value == 0.25
        assert parameter.blend == "Multiply"
        assert b'"Blend": 1' in expression.json
        exact_expression = UnityRs(expression_path).read_cubism_expression(
            0, 7, maximum_output_bytes=len(expression.json)
        )
        assert exact_expression.json == expression.json
        try:
            UnityRs(expression_path).read_cubism_expression(
                0, 7, maximum_output_bytes=len(expression.json) - 1
            )
        except ValueError:
            pass
        else:
            raise AssertionError("Cubism expression output limits must be enforced")
        dump_report = UnityRs(expression_path).export(
            Path(directory) / "dump-text",
            mode=" DuMp_TeXt ",
        )
        assert dump_report.failures == []
        assert len(dump_report.exported) == 1
        dump_bytes = Path(dump_report.exported[0]).read_bytes()
        assert dump_bytes.startswith(b"MonoBehaviour Base\r\n")
        assert b'\tstring m_Name = "smile.exp3"\r\n' in dump_bytes
        assert dump_bytes.endswith(b"\r\n")
        direct_dump = UnityRs(expression_path).read_type_tree_dump(0, 7)
        assert direct_dump.encode("utf-8") == dump_bytes
        oversized_option = "é" * 2048
        for field, options in (
            ("export mode", {"mode": oversized_option}),
            ("image format", {"image_format": oversized_option}),
        ):
            try:
                UnityRs(expression_path).export(
                    Path(directory) / f"oversized-{field.replace(' ', '-')}",
                    **options,
                )
            except ValueError as error:
                message = str(error)
                assert (
                    f"unsupported {field} value of 4096 UTF-8 bytes" in message
                )
                assert oversized_option not in message
            else:
                raise AssertionError(f"an oversized {field} should be rejected")
        try:
            UnityRs(expression_path).read_type_tree_dump(0, 7, maximum_bytes=8)
        except ValueError:
            pass
        else:
            raise AssertionError("TypeTree dump output limit should be enforced")

        pose_path = Path(directory) / "pose.assets"
        pose_path.write_bytes(synthetic_cubism_pose_part())
        pose = UnityRs(pose_path).read_cubism_pose_part(0, 7)
        assert isinstance(pose, CubismPosePart)
        assert pose.path_id == 7
        assert pose.group_index == 2
        assert pose.links == ["PartArmL", "PartArmR"]

        display_path = Path(directory) / "display.assets"
        display_path.write_bytes(synthetic_cubism_display_info())
        display = UnityRs(display_path).read_cubism_display_info(0, 7)
        assert isinstance(display, CubismDisplayInfo)
        assert display.path_id == 7
        assert display.name == "Angle X"
        assert display.display_name == "Face Angle"
        assert display.effective_name == "Face Angle"

        physics_path = Path(directory) / "physics.assets"
        physics_path.write_bytes(synthetic_cubism_physics())
        physics = UnityRs(physics_path).read_cubism_physics(0, 7, motion_fps=60.0)
        assert isinstance(physics, CubismPhysics)
        assert physics.path_id == 7
        assert physics.fps == 0.0
        assert physics.gravity == (0.0, -1.0)
        assert physics.wind == (0.5, 0.0)
        assert physics.sub_rig_count == 1
        assert physics.input_count == 1
        assert physics.output_count == 1
        assert physics.particle_count == 1
        # physics3.json goes through .NET's "0.###", where an integral value
        # has no decimal point. This read 60.0 until the managed differential
        # established the format.
        assert b'"Fps": 60' in physics.json
        # Newtonsoft's `Formatting.Indented` expands every object, and the
        # managed extractor writes this document through it, so a destination
        # spans four lines rather than one. This read the compact form until
        # the differential started comparing these documents byte for byte.
        assert (
            b'"Destination": {\n            "Target": "Parameter",\n'
            b'            "Id": "ParamHair"\n          }'
        ) in physics.json
        exact_physics = UnityRs(physics_path).read_cubism_physics(
            0, 7, motion_fps=60.0, maximum_output_bytes=len(physics.json)
        )
        assert exact_physics.json == physics.json
        try:
            UnityRs(physics_path).read_cubism_physics(
                0, 7, motion_fps=60.0, maximum_output_bytes=len(physics.json) - 1
            )
        except ValueError:
            pass
        else:
            raise AssertionError("Cubism physics output limits must be enforced")

        motion_path = Path(directory) / "motion.assets"
        motion_path.write_bytes(synthetic_cubism_fade_motion())
        motion = UnityRs(motion_path).read_cubism_fade_motion(0, 7)
        assert isinstance(motion, CubismFadeMotion)
        assert motion.path_id == 7
        assert motion.source_name == "idle.fade.asset"
        assert motion.motion_name == "idle"
        assert motion.fade_in_time > 0.19
        assert motion.fade_out_time > 0.29
        assert motion.motion_length == 1.0
        assert motion.curve_count == 1
        assert motion.keyframe_count == 2
        assert b'"CurveCount": 1' in motion.json
        assert b'"Id": "ParamAngleX"' in motion.json
        exact_motion = UnityRs(motion_path).read_cubism_fade_motion(
            0, 7, maximum_output_bytes=len(motion.json)
        )
        assert exact_motion.json == motion.json
        try:
            UnityRs(motion_path).read_cubism_fade_motion(
                0, 7, maximum_output_bytes=len(motion.json) - 1
            )
        except ValueError:
            pass
        else:
            raise AssertionError("Cubism motion output limits must be enforced")


if __name__ == "__main__":
    main()
