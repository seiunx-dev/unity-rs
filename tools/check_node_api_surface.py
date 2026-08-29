#!/usr/bin/env python3
"""Audit the optional Node binding against Core and its TypeScript consumer.

The generated declaration and the Rust addon can drift together while both
omit a newly added Core capability.  Conversely, a declaration can publish a
method that no ordinary TypeScript consumer ever type-checks.  This gate closes
both directions without loading a platform-specific native addon:

* every stable high-level Core method is mapped to a real Node symbol or has a
  concrete Rust-only ownership reason;
* the ``#[napi]`` Rust class/object fields and ``index.d.ts`` agree; and
* every public ``UnityRs`` member is used by the strict TypeScript consumer.
"""

from __future__ import annotations

import re
from pathlib import Path

import check_python_api_surface


ROOT = Path(__file__).resolve().parents[1]
CORE_STUDIO = ROOT / "crates/unity-rs-core/src/studio.rs"
NODE_RUST = ROOT / "crates/unity-rs-node/src/lib.rs"
DECLARATIONS = ROOT / "crates/unity-rs-node/index.d.ts"
CONSUMER = ROOT / "crates/unity-rs-node/tests/types.ts"
NODE_READ_RESOURCE = "UnityRs.readResource"
NODE_READ_RAW = "UnityRs.readRaw"

# This table is intentionally independent of the Python mapping. Updating one
# binding cannot silently classify another. Multiple streaming/materializing
# Core methods may map to one bounded Node byte-returning method.
CORE_TO_NODE = {
    "Studio.open": "UnityRs.constructor",
    "Studio.open_with_options": "UnityRs.openWith",
    "Studio.open_region": "UnityRs.fromBuffer",
    "Studio.open_region_with_options": "UnityRs.fromBuffer",
    "Studio.open_regions": "UnityRs.fromBuffers",
    "Studio.open_regions_with_options": "UnityRs.fromBuffers",
    "Studio.file_count": "UnityRs.fileCount",
    "Studio.object_count": "UnityRs.objectCount",
    "Studio.resource_count": "UnityRs.resourceCount",
    "Studio.load_diagnostics": "UnityRs.loadDiagnosticPage",
    "Studio.sprite_page_cache_stats": "UnityRs.spritePageCacheStats",
    "Studio.files": "UnityRs.filePage",
    "Studio.file": "UnityRs.filePage",
    "Studio.resources": "UnityRs.resourcePage",
    "Studio.resource": NODE_READ_RESOURCE,
    "Studio.resource_by_path": "UnityRs.resourceIndexByPath",
    "Studio.objects": "UnityRs.objectPage",
    "Studio.object": NODE_READ_RAW,
    "Studio.scene_hierarchy": "UnityRs.sceneWithLimits",
    "Studio.export": "UnityRs.exportWithOptions",
    "Studio.extract": "UnityRs.extract",
    "Studio.write_static_fbx": "UnityRs.readStaticFbx",
    "Studio.read_static_fbx": "UnityRs.readStaticFbx",
    "Studio.write_fbx": "UnityRs.readFbx",
    "Studio.write_fbx_with_acl_decoder": "UnityRs.readFbxWithAclDecoder",
    "Studio.write_static_fbx_binary": "UnityRs.readStaticFbxBinary",
    "Studio.read_static_fbx_binary": "UnityRs.readStaticFbxBinary",
    "Studio.write_fbx_binary": "UnityRs.readFbxBinary",
    "Studio.write_fbx_binary_with_acl_decoder": (
        "UnityRs.readFbxBinaryWithAclDecoder"
    ),
    "Studio.read_fbx_binary": "UnityRs.readFbxBinary",
    "Studio.read_fbx_binary_with_acl_decoder": (
        "UnityRs.readFbxBinaryWithAclDecoder"
    ),
    "Studio.write_fbx_with_textures": "UnityRs.readFbxWithTextures",
    "Studio.read_model_obj": "UnityRs.readModelObj",
    "Studio.read_fbx": "UnityRs.readFbx",
    "Studio.read_fbx_with_acl_decoder": "UnityRs.readFbxWithAclDecoder",
    "Studio.split_object_fbx_candidates": "UnityRs.splitObjectFbxCandidates",
    "Studio.animator_fbx_candidates": "UnityRs.animatorFbxCandidates",
    "Studio.write_game_object_fbx": "UnityRs.readGameObjectFbx",
    "Studio.write_game_object_fbx_with_acl_decoder": (
        "UnityRs.readGameObjectFbxWithAclDecoder"
    ),
    "Studio.read_game_object_fbx": "UnityRs.readGameObjectFbx",
    "Studio.read_game_object_fbx_with_acl_decoder": (
        "UnityRs.readGameObjectFbxWithAclDecoder"
    ),
    "Studio.live2d_packages": "UnityRs.live2DPackages",
    "Studio.live2d_packages_with_schema_provider": (
        "UnityRs.readLive2DPackagesWithSchemas"
    ),
    "Studio.live2d_packages_with_adapters": (
        "UnityRs.readLive2DPackagesWithAclDecoder"
    ),
    "Studio.read_live2d_packages": "UnityRs.readLive2DPackages",
    "Studio.read_live2d_packages_with_schema_provider": (
        "UnityRs.readLive2DPackagesWithSchemas"
    ),
    "Studio.read_live2d_packages_with_adapters": (
        "UnityRs.readLive2DPackagesWithAclDecoder"
    ),
    "StudioFile.index": "FileInfo.index",
    "StudioFile.path": "FileInfo.path",
    "StudioFile.unity_version": "FileInfo.unityVersion",
    "StudioFile.object_count": "FileInfo.objectCount",
    "StudioResource.index": "ResourceInfo.index",
    "StudioResource.path": "ResourceInfo.path",
    "StudioResource.byte_size": "ResourceInfo.byteSize",
    "StudioResource.write": NODE_READ_RESOURCE,
    "StudioResource.write_range": "UnityRs.readResourceRange",
    "StudioResource.read": NODE_READ_RESOURCE,
    "StudioResource.read_range": "UnityRs.readResourceRange",
    "StudioObject.file_index": "ObjectInfo.fileIndex",
    "StudioObject.object_index": "ObjectInfo.objectIndex",
    "StudioObject.source_path": "ObjectInfo.sourcePath",
    "StudioObject.path_id": "ObjectInfo.pathId",
    "StudioObject.class_id": "ObjectInfo.classId",
    "StudioObject.byte_size": "ObjectInfo.byteSize",
    "StudioObject.name": "ObjectInfo.name",
    "StudioObject.container": "ObjectInfo.container",
    "StudioObject.write_raw": NODE_READ_RAW,
    "StudioObject.read_raw": NODE_READ_RAW,
    "StudioObject.read_text_bytes": "UnityRs.readText",
    "StudioObject.read_shader_text": "UnityRs.readShader",
    "StudioObject.write_mesh_obj": "UnityRs.readMeshObj",
    "StudioObject.read_mesh_obj": "UnityRs.readMeshObj",
    "StudioObject.read_animation_clip": "UnityRs.readAnimationClipInfo",
    "StudioObject.read_legacy_animation": "UnityRs.readLegacyAnimation",
    "StudioObject.read_animator_override_controller": (
        "UnityRs.readAnimatorOverrideController"
    ),
    "StudioObject.read_asset_bundle": "UnityRs.readAssetBundle",
    "StudioObject.read_resource_manager": "UnityRs.readResourceManager",
    "StudioObject.read_preload_data": "UnityRs.readPreloadData",
    "StudioObject.read_animator_controller": "UnityRs.readAnimatorController",
    "StudioObject.read_avatar": "UnityRs.readAvatar",
    "StudioObject.read_audio_clip": "UnityRs.readAudioClip",
    "StudioObject.read_font": "UnityRs.readFont",
    "StudioObject.read_movie_texture": "UnityRs.readMovieTexture",
    "StudioObject.read_video_clip": "UnityRs.readVideoClip",
    "StudioObject.read_material": "UnityRs.readMaterial",
    "StudioObject.read_mono_script": "UnityRs.readMonoScript",
    "StudioObject.read_type_tree_json": "UnityRs.readTypeTreeJson",
    "StudioObject.write_type_tree_dump": "UnityRs.readTypeTreeDump",
    "StudioObject.read_type_tree_dump": "UnityRs.readTypeTreeDump",
    "StudioObject.read_mono_behaviour_json": "UnityRs.readMonoBehaviourJson",
    "StudioObject.decode_texture_mip": "UnityRs.readTexture",
    "StudioObject.decode_texture_array_mip0": "UnityRs.readTextureArray",
    "StudioObject.read_sprite_atlas": "UnityRs.readSpriteAtlas",
    "StudioObject.read_sprite": "UnityRs.readSpriteMetadata",
    "StudioObject.decode_sprite": "UnityRs.readSprite",
    "StudioObject.read_build_settings": "UnityRs.readBuildSettings",
    "StudioObject.read_player_settings": "UnityRs.readPlayerSettings",
    "StudioObject.read_cubism_expression": "UnityRs.readCubismExpression",
    "StudioObject.read_cubism_pose_part": "UnityRs.readCubismPosePart",
    "StudioObject.read_cubism_display_info": "UnityRs.readCubismDisplayInfo",
    "StudioObject.read_cubism_physics": "UnityRs.readCubismPhysics",
    "StudioObject.read_cubism_fade_motion": "UnityRs.readCubismFadeMotion",
    "StudioObject.read_cubism_clip_motion": "UnityRs.readCubismClipMotion",
    "StudioObject.read_cubism_clip_motion_with_acl_decoder": (
        "UnityRs.readCubismClipMotionWithAclDecoder"
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
    "StudioObject.read_type_tree_value_with_tree": (
        "accepts a caller-owned Rust TypeTree for the Python UnityPy facade; "
        "Node has no public TypeTree-node input contract"
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


def braced_block(source: str, marker: str) -> str:
    """Return one source block beginning at ``marker`` through its closing brace."""
    start = source.find(marker)
    if start < 0:
        raise AuditError(f"source does not contain {marker!r}")
    opening = source.find("{", start + len(marker))
    if opening < 0:
        raise AuditError(f"source does not open a block after {marker!r}")
    depth = 0
    for index in range(opening, len(source)):
        character = source[index]
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    raise AuditError(f"source does not close the block after {marker!r}")


def associated_type(block: str, name: str) -> str | None:
    """Return a simple Rust associated type without regex backtracking."""
    expected = f"type {name}"
    for line in block.splitlines():
        left, separator, right = line.strip().partition("=")
        if not separator or left.strip() != expected:
            continue
        value = right.strip()
        if value.endswith(";"):
            return value.removesuffix(";").strip()
    return None


def validate_live2d_worker_projection(rust_source: str) -> None:
    """Keep unbounded package-table projection off the Node event loop."""
    task = braced_block(rust_source, "impl Task for Live2dPackagesWithAclTask")
    output = associated_type(task, "Output")
    if output != "Live2dPackageSet":
        raise AuditError(
            "Live2dPackagesWithAclTask must return the projected Live2dPackageSet "
            "from its worker"
        )
    compute = braced_block(task, "fn compute")
    resolve = braced_block(task, "fn resolve")
    if "convert_live2d_package_set(set)" not in compute:
        raise AuditError(
            "Live2dPackagesWithAclTask must project package files and diagnostics "
            "inside compute"
        )
    if "convert_live2d_package_set" in resolve:
        raise AuditError(
            "Live2dPackagesWithAclTask must not project package files or diagnostics "
            "inside resolve"
        )


def validate_texture_array_worker_projection(rust_source: str) -> None:
    """Keep the fallible multi-layer result projection off the Node event loop."""
    task = braced_block(rust_source, "impl Task for ReadTextureArrayTask")
    output = associated_type(task, "Output")
    if output != "DisplayRowImages":
        raise AuditError(
            "ReadTextureArrayTask must return the worker-projected DisplayRowImages"
        )
    compute = braced_block(task, "fn compute")
    if "DisplayRowImages::from_decoded(images)" not in compute:
        raise AuditError(
            "ReadTextureArrayTask must project Texture2DArray layers inside compute"
        )
    wrapper = braced_block(rust_source, "impl DisplayRowImages")
    from_decoded = braced_block(wrapper, "fn from_decoded")
    into_nodes = braced_block(wrapper, "fn into_nodes")
    if "reserve(images.len(), \"Texture2DArray images\")" not in from_decoded:
        raise AuditError(
            "DisplayRowImages must reserve its final Node-facing layer table on the worker"
        )
    if "convert_image(image)" not in from_decoded:
        raise AuditError(
            "DisplayRowImages must build each final Node-facing image on the worker"
        )
    forbidden_resolve_work = ("reserve(", "convert_image", "for ")
    if any(fragment in into_nodes for fragment in forbidden_resolve_work):
        raise AuditError(
            "DisplayRowImages::into_nodes must not allocate or project layers on the event loop"
        )


def rust_node_symbols(source: str) -> set[str]:
    """Extract exported class members and mapped object fields from Rust."""
    implementation = block_between(
        source,
        "#[napi]\nimpl UnityRs {",
        "\n}\n\nimpl UnityRs {",
    )
    symbols: set[str] = set()
    for name in re.findall(r"^\s*pub fn ([A-Za-z_]\w*)", implementation, re.M | re.ASCII):
        javascript = "constructor" if name == "new" else snake_to_javascript(name)
        symbol = f"UnityRs.{javascript}"
        if symbol in symbols:
            raise AuditError(f"Rust Node symbol is declared twice: {symbol}")
        symbols.add(symbol)

    for object_name in MAPPED_OBJECTS:
        block = block_between(
            source,
            f"pub struct {object_name} {{",
            "\n}",
        )
        for field in re.findall(r"^\s*pub ([A-Za-z_]\w*):", block, re.M | re.ASCII):
            symbols.add(f"{object_name}.{snake_to_javascript(field)}")
    return symbols


def declaration_symbols(source: str) -> tuple[set[str], set[str], set[str]]:
    """Return all mapped symbols plus UnityRs methods and properties."""
    class_block = block_between(source, "export declare class UnityRs {", "\n}")
    methods: set[str] = set()
    properties: set[str] = set()
    for line in class_block.splitlines():
        match = re.match(
            r"[ \t]*(?:(static|get) )?([A-Za-z_$][A-Za-z0-9_$]*)\(", line
        )
        if match is None:
            continue
        kind, name = match.groups()
        if kind == "get":
            properties.add(name)
        else:
            methods.add(name)

    symbols = {f"UnityRs.{name}" for name in methods | properties}
    for object_name in MAPPED_OBJECTS:
        block = block_between(source, f"export interface {object_name} {{", "\n}")
        for field in re.findall(r"^\s*([A-Za-z_$][A-Za-z0-9_$]*)\??:", block, re.M):
            symbols.add(f"{object_name}.{field}")
    return symbols, methods, properties


def validate_node_declarations(rust_source: str, declaration_source: str) -> tuple[int, int]:
    """Require Rust's public UnityRs class and generated declarations to agree."""
    rust = rust_node_symbols(rust_source)
    declarations, methods, properties = declaration_symbols(declaration_source)
    rust_class = {symbol for symbol in rust if symbol.startswith("UnityRs.")}
    declaration_class = {
        symbol for symbol in declarations if symbol.startswith("UnityRs.")
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
            r"(?:\bstudio|\bUnityRs)\.([A-Za-z_$][A-Za-z0-9_$]*)\s*\(",
            source,
        )
    )
    if re.search(r"\bnew\s+UnityRs\s*\(", source):
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
            "strict TypeScript consumer does not cover every public UnityRs member ("
            + "; ".join(details)
            + ")"
        )
    return len(methods), len(properties)


def main() -> None:
    core_source = CORE_STUDIO.read_text(encoding="utf-8")
    rust_source = NODE_RUST.read_text(encoding="utf-8")
    declaration_source = DECLARATIONS.read_text(encoding="utf-8")
    try:
        validate_live2d_worker_projection(rust_source)
        validate_texture_array_worker_projection(rust_source)
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
