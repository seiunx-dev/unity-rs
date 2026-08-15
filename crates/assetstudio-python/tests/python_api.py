import json
import struct
import tempfile
from pathlib import Path
from typing import Optional

from assetstudio import (
    AclCompressedTracks,
    AclDecodedClip,
    AnimationClip,
    AnimatorController,
    AssetStudio,
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
    Live2dPackage,
    Material,
    MonoBehaviourSchema,
    MonoBehaviourSchemas,
    MonoScript,
    PlayerSettings,
    ResourceInfo,
    ResourceIterator,
    extract,
)


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


def synthetic_text_asset() -> bytes:
    payload = bytearray()
    push_aligned_string(payload, "python")
    push_i32(payload, 12)
    payload.extend(b"hello python")

    return finish_v22_asset(49, payload)


def synthetic_unity6_shader() -> bytes:
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
    push_i32(payload, 0)  # object dependencies
    push_i32(payload, 0)  # non-modifiable textures
    payload.append(0)  # baked
    align(payload, 4)
    return finish_v22_asset(48, payload, "6000.2.0f1")


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
        / "assetstudio-core"
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
    push_pptr(payload, 9)
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
    return finish_v22_asset(21, payload)


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


def synthetic_tuanjie_animation_clip() -> bytes:
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
    push_u32(payload, 7)
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
    push_i32(payload, 0)
    push_i32(payload, 0)
    payload.extend((0, 0))
    align(payload, 4)
    push_i32(payload, 0)
    align(payload, 4)
    return finish_v22_asset(74, payload, "2022.3.61t1")


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


def model_renderer(*, tuanjie: bool = False) -> bytearray:
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
    push_i32(output, 0)
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


def main() -> None:
    assert AnimationClip.__name__ == "AnimationClip"
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
    assert FbxCandidate.__name__ == "FbxCandidate"
    assert hasattr(Live2dPackage, "eye_blink_parameters")
    assert hasattr(Live2dPackage, "lip_sync_parameters")
    targets = CubismMotionTargets(parameters=["ParamAngleX"], parts=["PartBody"])
    assert targets.parameters == ["ParamAngleX"]
    assert targets.parts == ["PartBody"]
    with tempfile.TemporaryDirectory(prefix="assetstudio-python-") as directory:
        path = Path(directory) / "fixture.assets"
        path.write_bytes(synthetic_text_asset())

        studio = AssetStudio(path)
        memory_studio = AssetStudio.from_bytes(
            synthetic_text_asset(), name="memory-fixture.assets"
        )
        assert memory_studio.file_count == 1
        assert memory_studio.files()[0].path == "memory-fixture.assets"
        assert memory_studio.read_text(0, 7) == b"hello python"
        try:
            AssetStudio.from_bytes(synthetic_text_asset(), maximum_bytes=1)
        except ValueError:
            pass
        else:
            raise AssertionError("in-memory input limit should be enforced")
        memory_resource = AssetStudio.from_bytes(
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

        oodle_studio = AssetStudio.from_bytes(
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
            AssetStudio(oodle_source_path, oodle_decoder=decode_oodle).read_resource(0)
            == oodle_payload
        )
        oodle_source_path.unlink()
        oodle_calls.clear()
        assert (
            AssetStudio.from_memory_files(
                [("oodle-memory.bundle", oodle_bundle)],
                oodle_decoder=decode_oodle,
            ).read_resource(0)
            == oodle_payload
        )
        try:
            AssetStudio.from_bytes(oodle_bundle, name="oodle.bundle")
        except NotImplementedError:
            pass
        else:
            raise AssertionError("Oodle bundles without a decoder should be rejected")
        try:
            AssetStudio.from_bytes(
                oodle_bundle,
                name="oodle.bundle",
                oodle_decoder=lambda _block, _size: b"short",
            )
        except ValueError:
            pass
        else:
            raise AssertionError("short Python Oodle decoder output should be rejected")
        try:
            AssetStudio.from_bytes(
                oodle_bundle,
                name="oodle.bundle",
                oodle_decoder=object(),
            )
        except TypeError:
            pass
        else:
            raise AssertionError("non-callable Python Oodle decoders should be rejected")
        memory_files = AssetStudio.from_memory_files(
            [
                ("multi.assets", synthetic_text_asset()),
                ("multi.resS", b"multi resource"),
            ]
        )
        assert memory_files.file_count == 1
        assert memory_files.resource_count == 1
        assert memory_files.read_text(0, 7) == b"hello python"
        assert memory_files.read_resource_by_path("MULTI.RESS") == b"multi resource"
        external_video = AssetStudio.from_memory_files(
            [
                ("external.assets", synthetic_external_video_clip()),
                ("external.resS", b"xxvideo-binyy"),
            ]
        ).read_video_clip(0, 7)
        assert external_video.name == "external-video"
        assert external_video.extension == ".mp4"
        assert external_video.data == b"video-bin"
        try:
            AssetStudio.from_memory_files(
                [("a", b"a"), ("b", b"b")], maximum_files=1
            )
        except ValueError:
            pass
        else:
            raise AssertionError("memory file count limit should be enforced")
        try:
            AssetStudio.from_memory_files(
                [("a", b"aa")], maximum_file_bytes=1
            )
        except ValueError:
            pass
        else:
            raise AssertionError("memory per-file byte limit should be enforced")
        try:
            AssetStudio.from_memory_files(
                [("a", b"aa"), ("b", b"bb")], maximum_total_bytes=3
            )
        except ValueError:
            pass
        else:
            raise AssertionError("memory total byte limit should be enforced")
        try:
            AssetStudio(Path(directory), maximum_input_directories=0)
        except ValueError:
            pass
        else:
            raise AssertionError("directory traversal limits should be enforced")

        # A game directory mixes readable assets with containers whose layout
        # has never been verified. By default one of those fails the whole
        # load; skip_unreadable_inputs keeps everything that did parse.
        with tempfile.TemporaryDirectory(prefix="assetstudio-mixed-") as mixed_root:
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
                AssetStudio(mixed)
            except NotImplementedError:
                pass
            else:
                raise AssertionError("an unreadable input should fail the load")
            tolerant = AssetStudio(mixed, skip_unreadable_inputs=True)
            assert tolerant.file_count == 2, tolerant.file_count
            assert tolerant.object_count == 2, tolerant.object_count
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
        assert next(AssetStudio(path).iter_objects()).path_id == 7
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
        resource_studio = AssetStudio(directory)
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

        # Unity changed the serialized shader in 2021 and neither this reader
        # nor the managed one implements the new layout, so a 6000 shader is
        # declined rather than parsed. This used to assert the parsed text,
        # from a fixture built to a layout no Unity writes.
        shader_path = Path(directory) / "unity6-shader.assets"
        shader_path.write_bytes(synthetic_unity6_shader())
        shader_studio = AssetStudio(shader_path)
        try:
            shader_studio.read_shader(0, 7)
        except NotImplementedError as error:
            assert "2021" in str(error), error
        else:
            raise AssertionError("a Unity 6 shader should be declined, not parsed")

        mesh_path = Path(directory) / "mesh.assets"
        mesh_path.write_bytes(synthetic_mesh())
        mesh_studio = AssetStudio(mesh_path)
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
        assert AssetStudio(tuanjie_mesh_path).read_mesh_obj(0, 7) == mesh_obj
        external_mesh = AssetStudio.from_memory_files(
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
        assert oodle_calls == [
            (b"fake-oodle-blocks-info", len(oodle_info)),
            (oodle_data_input, len(oodle_payload)),
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

        try:
            studio.read_raw(0, 8)
        except KeyError:
            pass
        else:
            raise AssertionError("missing path_id should raise KeyError")

        texture_path = Path(directory) / "texture.assets"
        texture_path.write_bytes(synthetic_texture2d())
        texture_studio = AssetStudio(texture_path)
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
        switch_image = AssetStudio(switch_path).read_texture(0, 7)
        assert (switch_image.width, switch_image.height) == (1, 1)
        assert switch_image.rgba == bytes((9, 8, 7, 6))

        array_path = Path(directory) / "texture-array.assets"
        array_path.write_bytes(synthetic_texture2d_array())
        array_images = AssetStudio(array_path).read_texture_array(0, 7)
        assert len(array_images) == 2
        assert repr(array_images[0]) == "RgbaImage(width=1, height=2)"
        assert array_images[0].rgba == bytes((5, 6, 7, 8, 1, 2, 3, 4))
        assert array_images[1].rgba == bytes((15, 16, 17, 18, 11, 12, 13, 14))
        try:
            AssetStudio(array_path).read_texture_array(0, 7, maximum_bytes=15)
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
        sprite_studio = AssetStudio(sprite_path)
        sprite_image = sprite_studio.read_sprite(0, 7)
        assert repr(sprite_image) == "RgbaImage(width=1, height=1)"
        assert sprite_image.rgba == bytes((9, 8, 7, 255))
        try:
            sprite_studio.read_sprite(0, 7, maximum_bytes=3)
        except ValueError:
            pass
        else:
            raise AssertionError("sprite output limit should raise ValueError")

        tight_sprite_path = Path(directory) / "tight-sprite.assets"
        tight_sprite_path.write_bytes(synthetic_tight_sprite())
        tight_sprite = AssetStudio(tight_sprite_path).read_sprite(0, 7)
        assert (tight_sprite.width, tight_sprite.height) == (2, 2)
        assert tight_sprite.rgba == bytes(
            (30, 3, 3, 255, 0, 0, 0, 0, 10, 1, 1, 255, 20, 2, 2, 255)
        )

        webp_output = Path(directory) / "webp-export"
        webp_report = texture_studio.export(webp_output, image_format="webp")
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
        audio_studio = AssetStudio(audio_path)
        audio = audio_studio.read_audio_clip(0, 7)
        assert isinstance(audio, AudioClip)
        assert audio.name == "legacy-pcm"
        assert audio.extension == ".wav"
        assert audio.payload_kind == "audio_wav"
        assert audio.data[:12] == b"RIFF(\0\0\0WAVE"
        assert audio.data[36:44] == b"data\x04\0\0\0"
        assert audio.data[44:] == b"\x01\x02\x03\x04"

        raw_audio = audio_studio.read_audio_clip(0, 7, format="raw")
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

        audio_export = audio_studio.export(Path(directory) / "audio-export")
        assert audio_export.failures == []
        assert len(audio_export.exported) == 1
        assert Path(audio_export.exported[0]).suffix == ".wav"

        fsb5_path = Path(directory) / "fsb5-pcm.assets"
        fsb5_path.write_bytes(synthetic_fsb5_pcm())
        fsb5_audio = AssetStudio(fsb5_path).read_audio_clip(0, 7)
        assert fsb5_audio.name == "fsb5-pcm"
        assert fsb5_audio.extension == ".wav"
        assert fsb5_audio.payload_kind == "audio_wav"
        assert fsb5_audio.data[:12] == b"RIFF(\0\0\0WAVE"
        assert fsb5_audio.data[20:24] == struct.pack("<HH", 1, 2)
        assert fsb5_audio.data[24:28] == struct.pack("<I", 44_100)
        assert fsb5_audio.data[44:] == b"\x01\x02\x03\x04"
        fsb5_raw = AssetStudio(fsb5_path).read_audio_clip(0, 7, format="raw")
        assert fsb5_raw.extension == ".fsb"
        assert fsb5_raw.data[:4] == b"FSB5"

        fsb5_ima_path = Path(directory) / "fsb5-ima.assets"
        fsb5_ima_path.write_bytes(synthetic_fsb5_ima())
        fsb5_ima = AssetStudio(fsb5_ima_path).read_audio_clip(0, 7)
        assert fsb5_ima.name == "fsb5-ima"
        assert fsb5_ima.extension == ".wav"
        assert fsb5_ima.payload_kind == "audio_wav"
        assert fsb5_ima.data[:4] == b"RIFF"
        assert fsb5_ima.data[20:24] == struct.pack("<HH", 1, 1)
        assert len(fsb5_ima.data) == 44 + 64 * 2
        assert struct.unpack("<hh", fsb5_ima.data[44:48]) == (1000, 1002)

        fsb5_dsp_path = Path(directory) / "fsb5-dsp.assets"
        fsb5_dsp_path.write_bytes(synthetic_fsb5_dsp())
        fsb5_dsp = AssetStudio(fsb5_dsp_path).read_audio_clip(0, 7)
        assert fsb5_dsp.name == "fsb5-dsp"
        assert fsb5_dsp.extension == ".wav"
        assert fsb5_dsp.payload_kind == "audio_wav"
        assert fsb5_dsp.data[:4] == b"RIFF"
        assert fsb5_dsp.data[20:24] == struct.pack("<HH", 1, 1)
        assert len(fsb5_dsp.data) == 44 + 14 * 2
        assert struct.unpack("<hh", fsb5_dsp.data[44:48]) == (1, 3)

        fsb5_vag_path = Path(directory) / "fsb5-vag.assets"
        fsb5_vag_path.write_bytes(synthetic_fsb5_vag())
        fsb5_vag = AssetStudio(fsb5_vag_path).read_audio_clip(0, 7)
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
        fsb5_hevag = AssetStudio(fsb5_hevag_path).read_audio_clip(0, 7)
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
        fsb5_fadpcm = AssetStudio(fsb5_fadpcm_path).read_audio_clip(0, 7)
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
        fsb5_mpeg = AssetStudio(fsb5_mpeg_path).read_audio_clip(0, 7)
        assert fsb5_mpeg.name == "fsb5-mpeg"
        assert fsb5_mpeg.extension == ".wav"
        assert fsb5_mpeg.payload_kind == "audio_wav"
        assert fsb5_mpeg.data[:4] == b"RIFF"
        assert fsb5_mpeg.data[20:24] == struct.pack("<HH", 1, 1)
        assert len(fsb5_mpeg.data) == 44 + 2304 * 2
        assert set(fsb5_mpeg.data[44:]) == {0}

        fsb5_opus_path = Path(directory) / "fsb5-opus.assets"
        fsb5_opus_path.write_bytes(synthetic_fsb5_opus())
        fsb5_opus = AssetStudio(fsb5_opus_path).read_audio_clip(0, 7)
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
        fsb5_vorbis = AssetStudio(fsb5_vorbis_path).read_audio_clip(0, 7)
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
        font = AssetStudio(font_path).read_font(0, 7)
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
        movie = AssetStudio(movie_path).read_movie_texture(0, 7)
        assert (movie.name, movie.extension, movie.payload_kind, movie.data) == (
            "python-movie",
            ".ogv",
            "movie_ogv",
            b"OggS",
        )

        video_path = Path(directory) / "video.assets"
        video_path.write_bytes(synthetic_video_clip())
        video_studio = AssetStudio(video_path)
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
        material_studio = AssetStudio(material_path)
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
        scene = AssetStudio(scene_path).scene()
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

        model_path = Path(directory) / "model.assets"
        model_path.write_bytes(synthetic_static_model())
        fbx = AssetStudio(model_path).read_static_fbx(maximum_bytes=128 * 1024)
        # The same scene in the other encoding, which had no binding at all
        # until the writer was wired up. Checked against the format's own magic
        # and version word rather than against bytes this project produced.
        binary_fbx = AssetStudio(model_path).read_static_fbx_binary(
            maximum_bytes=128 * 1024
        )
        assert binary_fbx.startswith(b"Kaydara FBX Binary  \0"), binary_fbx[:32]
        assert struct.unpack_from("<I", binary_fbx, 23)[0] == 7400
        assert b"Geometry" in binary_fbx
        assert binary_fbx != fbx
        model_studio = AssetStudio(model_path)
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
        animated_fbx = AssetStudio(model_path).read_fbx(maximum_bytes=128 * 1024)
        animated_binary = AssetStudio(model_path).read_fbx_binary(
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
        tuanjie_fbx = AssetStudio(tuanjie_model_path).read_fbx(
            maximum_bytes=128 * 1024
        )
        assert b"Model::python model" in tuanjie_fbx
        assert b"Geometry::python triangle" in tuanjie_fbx
        assert b"a: 2,1,-1" in tuanjie_fbx
        try:
            AssetStudio(model_path).read_static_fbx(maximum_bytes=64)
        except ValueError:
            pass
        else:
            raise AssertionError("FBX output limit should raise ValueError")

        build_path = Path(directory) / "build-settings.assets"
        build_path.write_bytes(synthetic_build_settings())
        build = AssetStudio(build_path).read_build_settings(0, 7)
        assert isinstance(build, BuildSettings)
        assert build.path_id == 7
        assert build.levels is None
        assert build.scenes == ["Assets/Intro.unity", "Assets/Game.unity"]

        player_path = Path(directory) / "player-settings.assets"
        player_path.write_bytes(synthetic_player_settings())
        player = AssetStudio(player_path).read_player_settings(0, 7)
        assert isinstance(player, PlayerSettings)
        assert player.path_id == 7
        assert player.company_name == "Haruki"
        assert player.product_name == "Asset Studio"

        animation_path = Path(directory) / "tuanjie-animation.assets"
        animation_path.write_bytes(synthetic_tuanjie_animation_clip())
        animation = AssetStudio(animation_path).read_animation_clip(0, 7)
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
        acl = AssetStudio(animation_path).inspect_acl_tracks(0, 7)
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
        acl_blob, decoder_map = AssetStudio(animation_path).read_acl_decoder_input(0, 7)
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

        decoded = AssetStudio(animation_path).decode_acl_tracks(0, 7, decode_acl)
        assert isinstance(decoded, AclDecodedClip)
        assert len(decoded.times) == 12
        assert decoded.binding_indices == list(range(7))
        assert len(decoded.values) == 84
        assert decoded.values[-1] == 83.0
        assert decoded.following_curve_offset == 7
        try:
            AssetStudio(animation_path).decode_acl_tracks(0, 7, object())
        except TypeError:
            pass
        else:
            raise AssertionError("non-callable ACL decoders should be rejected")
        try:
            AssetStudio(animation_path).decode_acl_tracks(
                0,
                7,
                lambda *_args: ([0.0], [0], [float("nan")], 1),
            )
        except ValueError:
            pass
        else:
            raise AssertionError("invalid ACL decoder output should be rejected")
        try:
            AssetStudio(animation_path).decode_acl_tracks(
                0, 7, decode_acl, maximum_values=83
            )
        except ValueError:
            pass
        else:
            raise AssertionError("ACL decoded output limit should be enforced")
        try:
            AssetStudio(animation_path).inspect_acl_tracks(
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
                AssetStudio(animation_path).read_acl_decoder_input(0, 7, **kwargs)
            except ValueError:
                pass
            else:
                raise AssertionError("ACL decoder input limits should be enforced")
        assert animation.streaming_offset == 0x1020304050607080
        assert animation.streaming_size == 0x1234
        assert animation.streaming_path == "archive:/animation.resS"
        try:
            AssetStudio(animation_path).read_animation_clip(0, 7, maximum_bytes=1)
        except ValueError:
            pass
        else:
            raise AssertionError("AnimationClip object limit should be enforced")

        controller_path = Path(directory) / "tuanjie-controller.assets"
        controller_path.write_bytes(synthetic_tuanjie_animator_controller())
        controller = AssetStudio(controller_path).read_animator_controller(0, 7)
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
        avatar = AssetStudio(avatar_path).read_avatar(0, 7)
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
        stripped = AssetStudio(stripped_path)
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
        decoded = json.loads(stripped.read_mono_behaviour_json(0, 7, schema))
        assert decoded["m_Name"] == "Hero"
        assert decoded["score"] == 123
        exact_decoded = json.loads(
            stripped.read_mono_behaviour_json_with_schemas(0, 7, schemas)
        )
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
        expression = AssetStudio(expression_path).read_cubism_expression(0, 7)
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
        dump_report = AssetStudio(expression_path).export(
            Path(directory) / "dump-text",
            mode="dump_text",
        )
        assert dump_report.failures == []
        assert len(dump_report.exported) == 1
        dump_bytes = Path(dump_report.exported[0]).read_bytes()
        assert dump_bytes.startswith(b"MonoBehaviour Base\r\n")
        assert b'\tstring m_Name = "smile.exp3"\r\n' in dump_bytes
        assert dump_bytes.endswith(b"\r\n")
        direct_dump = AssetStudio(expression_path).read_type_tree_dump(0, 7)
        assert direct_dump.encode("utf-8") == dump_bytes
        try:
            AssetStudio(expression_path).read_type_tree_dump(0, 7, maximum_bytes=8)
        except ValueError:
            pass
        else:
            raise AssertionError("TypeTree dump output limit should be enforced")

        pose_path = Path(directory) / "pose.assets"
        pose_path.write_bytes(synthetic_cubism_pose_part())
        pose = AssetStudio(pose_path).read_cubism_pose_part(0, 7)
        assert isinstance(pose, CubismPosePart)
        assert pose.path_id == 7
        assert pose.group_index == 2
        assert pose.links == ["PartArmL", "PartArmR"]

        display_path = Path(directory) / "display.assets"
        display_path.write_bytes(synthetic_cubism_display_info())
        display = AssetStudio(display_path).read_cubism_display_info(0, 7)
        assert isinstance(display, CubismDisplayInfo)
        assert display.path_id == 7
        assert display.name == "Angle X"
        assert display.display_name == "Face Angle"
        assert display.effective_name == "Face Angle"

        physics_path = Path(directory) / "physics.assets"
        physics_path.write_bytes(synthetic_cubism_physics())
        physics = AssetStudio(physics_path).read_cubism_physics(0, 7, motion_fps=60.0)
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

        motion_path = Path(directory) / "motion.assets"
        motion_path.write_bytes(synthetic_cubism_fade_motion())
        motion = AssetStudio(motion_path).read_cubism_fade_motion(0, 7)
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


if __name__ == "__main__":
    main()
