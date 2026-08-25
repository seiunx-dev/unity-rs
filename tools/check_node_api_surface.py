#!/usr/bin/env python3
"""Audit the optional Node binding against Core and its TypeScript consumer.

The generated declaration and the Rust addon can drift together while both
omit a newly added Core capability.  Conversely, a declaration can publish a
method that no ordinary TypeScript consumer ever type-checks.  This gate closes
both directions without loading a platform-specific native addon:

* every stable high-level Core method is mapped to a real Node symbol or has a
  concrete Rust-only ownership reason;
* the ``#[napi]`` Rust class/object fields and ``index.d.ts`` agree; and
* every public ``AssetStudio`` member is used by the strict TypeScript consumer.
"""

from __future__ import annotations

import re
from pathlib import Path

import check_python_api_surface


ROOT = Path(__file__).resolve().parents[1]
CORE_STUDIO = ROOT / "crates/assetstudio-core/src/studio.rs"
NODE_RUST = ROOT / "crates/assetstudio-node/src/lib.rs"
DECLARATIONS = ROOT / "crates/assetstudio-node/index.d.ts"
CONSUMER = ROOT / "crates/assetstudio-node/tests/types.ts"

# This table is intentionally independent of the Python mapping. Updating one
# binding cannot silently classify another. Multiple streaming/materializing
# Core methods may map to one bounded Node byte-returning method.
CORE_TO_NODE = {
    "Studio.open": "AssetStudio.constructor",
    "Studio.open_with_options": "AssetStudio.openWith",
    "Studio.open_region": "AssetStudio.fromBuffer",
    "Studio.open_region_with_options": "AssetStudio.fromBuffer",
    "Studio.open_regions": "AssetStudio.fromBuffers",
    "Studio.open_regions_with_options": "AssetStudio.fromBuffers",
    "Studio.file_count": "AssetStudio.fileCount",
    "Studio.object_count": "AssetStudio.objectCount",
    "Studio.resource_count": "AssetStudio.resourceCount",
    "Studio.load_diagnostics": "AssetStudio.loadDiagnosticPage",
    "Studio.files": "AssetStudio.filePage",
    "Studio.file": "AssetStudio.filePage",
    "Studio.resources": "AssetStudio.resourcePage",
    "Studio.resource": "AssetStudio.readResource",
    "Studio.resource_by_path": "AssetStudio.resourceIndexByPath",
    "Studio.objects": "AssetStudio.objectPage",
    "Studio.object": "AssetStudio.readRaw",
    "Studio.scene_hierarchy": "AssetStudio.sceneWithLimits",
    "Studio.export": "AssetStudio.exportWithOptions",
    "Studio.extract": "AssetStudio.extract",
    "Studio.write_static_fbx": "AssetStudio.readStaticFbx",
    "Studio.read_static_fbx": "AssetStudio.readStaticFbx",
    "Studio.write_fbx": "AssetStudio.readFbx",
    "Studio.write_fbx_with_acl_decoder": "AssetStudio.readFbxWithAclDecoder",
    "Studio.write_static_fbx_binary": "AssetStudio.readStaticFbxBinary",
    "Studio.read_static_fbx_binary": "AssetStudio.readStaticFbxBinary",
    "Studio.write_fbx_binary": "AssetStudio.readFbxBinary",
    "Studio.write_fbx_binary_with_acl_decoder": (
        "AssetStudio.readFbxBinaryWithAclDecoder"
    ),
    "Studio.read_fbx_binary": "AssetStudio.readFbxBinary",
    "Studio.read_fbx_binary_with_acl_decoder": (
        "AssetStudio.readFbxBinaryWithAclDecoder"
    ),
    "Studio.write_fbx_with_textures": "AssetStudio.readFbxWithTextures",
    "Studio.read_model_obj": "AssetStudio.readModelObj",
    "Studio.read_fbx": "AssetStudio.readFbx",
    "Studio.read_fbx_with_acl_decoder": "AssetStudio.readFbxWithAclDecoder",
    "Studio.split_object_fbx_candidates": "AssetStudio.splitObjectFbxCandidates",
    "Studio.animator_fbx_candidates": "AssetStudio.animatorFbxCandidates",
    "Studio.write_game_object_fbx": "AssetStudio.readGameObjectFbx",
    "Studio.write_game_object_fbx_with_acl_decoder": (
        "AssetStudio.readGameObjectFbxWithAclDecoder"
    ),
    "Studio.read_game_object_fbx": "AssetStudio.readGameObjectFbx",
    "Studio.read_game_object_fbx_with_acl_decoder": (
        "AssetStudio.readGameObjectFbxWithAclDecoder"
    ),
    "Studio.live2d_packages": "AssetStudio.live2DPackages",
    "Studio.live2d_packages_with_schema_provider": (
        "AssetStudio.readLive2DPackagesWithSchemas"
    ),
    "Studio.live2d_packages_with_adapters": (
        "AssetStudio.readLive2DPackagesWithAclDecoder"
    ),
    "Studio.read_live2d_packages": "AssetStudio.readLive2DPackages",
    "Studio.read_live2d_packages_with_schema_provider": (
        "AssetStudio.readLive2DPackagesWithSchemas"
    ),
    "Studio.read_live2d_packages_with_adapters": (
        "AssetStudio.readLive2DPackagesWithAclDecoder"
    ),
    "StudioFile.index": "FileInfo.index",
    "StudioFile.path": "FileInfo.path",
    "StudioFile.unity_version": "FileInfo.unityVersion",
    "StudioFile.object_count": "FileInfo.objectCount",
    "StudioResource.index": "ResourceInfo.index",
    "StudioResource.path": "ResourceInfo.path",
    "StudioResource.byte_size": "ResourceInfo.byteSize",
    "StudioResource.write": "AssetStudio.readResource",
    "StudioResource.write_range": "AssetStudio.readResourceRange",
    "StudioResource.read": "AssetStudio.readResource",
    "StudioResource.read_range": "AssetStudio.readResourceRange",
    "StudioObject.file_index": "ObjectInfo.fileIndex",
    "StudioObject.object_index": "ObjectInfo.objectIndex",
    "StudioObject.source_path": "ObjectInfo.sourcePath",
    "StudioObject.path_id": "ObjectInfo.pathId",
    "StudioObject.class_id": "ObjectInfo.classId",
    "StudioObject.byte_size": "ObjectInfo.byteSize",
    "StudioObject.name": "ObjectInfo.name",
    "StudioObject.container": "ObjectInfo.container",
    "StudioObject.write_raw": "AssetStudio.readRaw",
    "StudioObject.read_raw": "AssetStudio.readRaw",
    "StudioObject.read_text_bytes": "AssetStudio.readText",
    "StudioObject.read_shader_text": "AssetStudio.readShader",
    "StudioObject.write_mesh_obj": "AssetStudio.readMeshObj",
    "StudioObject.read_mesh_obj": "AssetStudio.readMeshObj",
    "StudioObject.read_animation_clip": "AssetStudio.readAnimationClipInfo",
    "StudioObject.read_legacy_animation": "AssetStudio.readLegacyAnimation",
    "StudioObject.read_animator_override_controller": (
        "AssetStudio.readAnimatorOverrideController"
    ),
    "StudioObject.read_asset_bundle": "AssetStudio.readAssetBundle",
    "StudioObject.read_resource_manager": "AssetStudio.readResourceManager",
    "StudioObject.read_preload_data": "AssetStudio.readPreloadData",
    "StudioObject.read_animator_controller": "AssetStudio.readAnimatorController",
    "StudioObject.read_avatar": "AssetStudio.readAvatar",
    "StudioObject.read_audio_clip": "AssetStudio.readAudioClip",
    "StudioObject.read_font": "AssetStudio.readFont",
    "StudioObject.read_movie_texture": "AssetStudio.readMovieTexture",
    "StudioObject.read_video_clip": "AssetStudio.readVideoClip",
    "StudioObject.read_material": "AssetStudio.readMaterial",
    "StudioObject.read_mono_script": "AssetStudio.readMonoScript",
    "StudioObject.read_type_tree_json": "AssetStudio.readTypeTreeJson",
    "StudioObject.write_type_tree_dump": "AssetStudio.readTypeTreeDump",
    "StudioObject.read_type_tree_dump": "AssetStudio.readTypeTreeDump",
    "StudioObject.read_mono_behaviour_json": "AssetStudio.readMonoBehaviourJson",
    "StudioObject.decode_texture_mip": "AssetStudio.readTexture",
    "StudioObject.decode_texture_array_mip0": "AssetStudio.readTextureArray",
    "StudioObject.read_sprite_atlas": "AssetStudio.readSpriteAtlas",
    "StudioObject.read_sprite": "AssetStudio.readSpriteMetadata",
    "StudioObject.decode_sprite": "AssetStudio.readSprite",
    "StudioObject.read_build_settings": "AssetStudio.readBuildSettings",
    "StudioObject.read_player_settings": "AssetStudio.readPlayerSettings",
    "StudioObject.read_cubism_expression": "AssetStudio.readCubismExpression",
    "StudioObject.read_cubism_pose_part": "AssetStudio.readCubismPosePart",
    "StudioObject.read_cubism_display_info": "AssetStudio.readCubismDisplayInfo",
    "StudioObject.read_cubism_physics": "AssetStudio.readCubismPhysics",
    "StudioObject.read_cubism_fade_motion": "AssetStudio.readCubismFadeMotion",
    "StudioObject.read_cubism_clip_motion": "AssetStudio.readCubismClipMotion",
    "StudioObject.read_cubism_clip_motion_with_acl_decoder": (
        "AssetStudio.readCubismClipMotionWithAclDecoder"
    ),
}

INTENTIONAL_RUST_ONLY = {
    "Studio.from_collection": "accepts the low-level Rust AssetCollection type",
    "Studio.collection": "borrows the low-level Rust AssetCollection type",
    "Studio.into_collection": "moves the low-level Rust AssetCollection type",
    "Studio.object_by_index": (
        "returns a borrowed Rust StudioObject; Node reads use the stable "
        "file/path key and expose objectIndex as metadata"
    ),
}

MAPPED_OBJECTS = ("FileInfo", "ObjectInfo", "ResourceInfo")


class AuditError(ValueError):
    """The Node public surface is incomplete or internally inconsistent."""


def snake_to_javascript(name: str) -> str:
    """Mirror napi-rs' camel casing, including the ``Live2D`` digit edge."""
    parts = name.split("_")
    result = parts[0] + "".join(part[:1].upper() + part[1:] for part in parts[1:])
    return re.sub(r"(?<=\d)([a-z])", lambda match: match.group(1).upper(), result)


def block_between(source: str, start_marker: str, end_marker: str) -> str:
    start = source.find(start_marker)
    if start < 0:
        raise AuditError(f"source does not contain {start_marker!r}")
    start += len(start_marker)
    end = source.find(end_marker, start)
    if end < 0:
        raise AuditError(f"source does not contain {end_marker!r} after {start_marker!r}")
    return source[start:end]


def rust_node_symbols(source: str) -> set[str]:
    """Extract exported class members and mapped object fields from Rust."""
    implementation = block_between(
        source,
        "#[napi]\nimpl AssetStudio {",
        "\n}\n\nimpl AssetStudio {",
    )
    symbols: set[str] = set()
    for name in re.findall(r"^\s*pub fn ([A-Za-z_][A-Za-z0-9_]*)", implementation, re.M):
        javascript = "constructor" if name == "new" else snake_to_javascript(name)
        symbol = f"AssetStudio.{javascript}"
        if symbol in symbols:
            raise AuditError(f"Rust Node symbol is declared twice: {symbol}")
        symbols.add(symbol)

    for object_name in MAPPED_OBJECTS:
        block = block_between(
            source,
            f"pub struct {object_name} {{",
            "\n}",
        )
        for field in re.findall(r"^\s*pub ([A-Za-z_][A-Za-z0-9_]*):", block, re.M):
            symbols.add(f"{object_name}.{snake_to_javascript(field)}")
    return symbols


def declaration_symbols(source: str) -> tuple[set[str], set[str], set[str]]:
    """Return all mapped symbols plus AssetStudio methods and properties."""
    class_block = block_between(source, "export declare class AssetStudio {", "\n}")
    methods: set[str] = set()
    properties: set[str] = set()
    for line in class_block.splitlines():
        match = re.match(
            r"\s*(?:(static|get) )?([A-Za-z_$][A-Za-z0-9_$]*|constructor)\(",
            line,
        )
        if match is None:
            continue
        kind, name = match.groups()
        if kind == "get":
            properties.add(name)
        else:
            methods.add(name)

    symbols = {f"AssetStudio.{name}" for name in methods | properties}
    for object_name in MAPPED_OBJECTS:
        block = block_between(source, f"export interface {object_name} {{", "\n}")
        for field in re.findall(r"^\s*([A-Za-z_$][A-Za-z0-9_$]*)\??:", block, re.M):
            symbols.add(f"{object_name}.{field}")
    return symbols, methods, properties


def validate_node_declarations(rust_source: str, declaration_source: str) -> tuple[int, int]:
    """Require Rust's public AssetStudio class and generated declarations to agree."""
    rust = rust_node_symbols(rust_source)
    declarations, methods, properties = declaration_symbols(declaration_source)
    rust_class = {symbol for symbol in rust if symbol.startswith("AssetStudio.")}
    declaration_class = {
        symbol for symbol in declarations if symbol.startswith("AssetStudio.")
    }
    missing_declarations = sorted(rust_class - declaration_class)
    stale_declarations = sorted(declaration_class - rust_class)
    if missing_declarations or stale_declarations:
        details = []
        if missing_declarations:
            details.append("missing declarations: " + ", ".join(missing_declarations))
        if stale_declarations:
            details.append("declarations without Rust exports: " + ", ".join(stale_declarations))
        raise AuditError("Rust Node exports and index.d.ts differ (" + "; ".join(details) + ")")
    return len(methods), len(properties)


def validate_core_mapping(
    core_source: str,
    rust_source: str,
    declaration_source: str,
) -> tuple[int, int]:
    """Require every high-level Core method to have a real Node disposition."""
    core = check_python_api_surface.core_studio_methods(core_source)
    classified = set(CORE_TO_NODE) | set(INTENTIONAL_RUST_ONLY)
    missing = sorted(core - classified)
    stale = sorted(classified - core)
    overlap = sorted(set(CORE_TO_NODE) & set(INTENTIONAL_RUST_ONLY))
    rust = rust_node_symbols(rust_source)
    declarations, _, _ = declaration_symbols(declaration_source)
    missing_rust_targets = sorted(
        f"{method} -> {target}"
        for method, target in CORE_TO_NODE.items()
        if target not in rust
    )
    missing_declaration_targets = sorted(
        f"{method} -> {target}"
        for method, target in CORE_TO_NODE.items()
        if target not in declarations
    )
    details = []
    if missing:
        details.append("unclassified Core methods: " + ", ".join(missing))
    if stale:
        details.append("stale Core classifications: " + ", ".join(stale))
    if overlap:
        details.append("methods mapped and Rust-only: " + ", ".join(overlap))
    if missing_rust_targets:
        details.append("missing Rust Node targets: " + ", ".join(missing_rust_targets))
    if missing_declaration_targets:
        details.append(
            "missing TypeScript targets: " + ", ".join(missing_declaration_targets)
        )
    if details:
        raise AuditError("Core-to-Node mapping is incomplete (" + "; ".join(details) + ")")
    return len(core), len(INTENTIONAL_RUST_ONLY)


def without_comments(source: str) -> str:
    source = re.sub(r"/\*.*?\*/", "", source, flags=re.S)
    return re.sub(r"//[^\n]*", "", source)


def consumed_members(source: str) -> tuple[set[str], set[str]]:
    """Collect direct calls/properties on the canonical TypeScript receiver."""
    source = without_comments(source)
    calls = set(
        re.findall(
            r"(?:\bstudio|\bAssetStudio)\.([A-Za-z_$][A-Za-z0-9_$]*)\s*\(",
            source,
        )
    )
    if re.search(r"\bnew\s+AssetStudio\s*\(", source):
        calls.add("constructor")
    attributes = set(
        re.findall(r"\bstudio\.([A-Za-z_$][A-Za-z0-9_$]*)", source)
    )
    return calls, attributes


def validate_surface(declaration_source: str, consumer_source: str) -> tuple[int, int]:
    _, methods, properties = declaration_symbols(declaration_source)
    calls, attributes = consumed_members(consumer_source)
    missing_methods = sorted(methods - calls)
    missing_properties = sorted(properties - attributes)
    if missing_methods or missing_properties:
        details = []
        if missing_methods:
            details.append("methods: " + ", ".join(missing_methods))
        if missing_properties:
            details.append("properties: " + ", ".join(missing_properties))
        raise AuditError(
            "strict TypeScript consumer does not cover every public AssetStudio member ("
            + "; ".join(details)
            + ")"
        )
    return len(methods), len(properties)


def main() -> None:
    core_source = CORE_STUDIO.read_text(encoding="utf-8")
    rust_source = NODE_RUST.read_text(encoding="utf-8")
    declaration_source = DECLARATIONS.read_text(encoding="utf-8")
    try:
        methods, properties = validate_node_declarations(rust_source, declaration_source)
        core_methods, rust_only = validate_core_mapping(
            core_source,
            rust_source,
            declaration_source,
        )
        validate_surface(
            declaration_source,
            CONSUMER.read_text(encoding="utf-8"),
        )
    except AuditError as error:
        raise SystemExit(str(error)) from error
    print(
        "Node API surface audit passed "
        f"({methods} methods, {properties} properties; "
        f"{core_methods} Core methods classified, {rust_only} Rust-only)"
    )


if __name__ == "__main__":
    main()
