#!/usr/bin/env python3
"""Require every public ``AssetStudio`` member in the stub to be type-consumed.

The installed-wheel gate proves that the PyO3 runtime and ``.pyi`` agree.  This
source-level gate closes the other direction: every method and property that we
publish must also appear in the strict Python 3.9 mypy consumer.  Otherwise an
API can be present on both runtime and stub while its annotation is never used
by an ordinary caller.
"""

from __future__ import annotations

import ast
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
STUB = ROOT / "crates/assetstudio-python/python/assetstudio/__init__.pyi"
CONSUMER = ROOT / "crates/assetstudio-python/tests/typecheck_api.py"
CORE_STUDIO = ROOT / "crates/assetstudio-core/src/studio.rs"

# Every public high-level Rust method must either name the Python symbol that
# represents it or be listed in INTENTIONAL_RUST_ONLY with a concrete ownership
# reason.  Several Rust streaming ``write_*`` methods intentionally map to one
# bounded Python byte-returning method; this table audits capabilities, not a
# spelling convention.
CORE_TO_PYTHON = {
    "Studio.open": "AssetStudio.__new__",
    "Studio.open_with_options": "AssetStudio.__new__",
    "Studio.open_region": "AssetStudio.from_bytes",
    "Studio.open_region_with_options": "AssetStudio.from_bytes",
    "Studio.open_regions": "AssetStudio.from_memory_files",
    "Studio.open_regions_with_options": "AssetStudio.from_memory_files",
    "Studio.file_count": "AssetStudio.file_count",
    "Studio.object_count": "AssetStudio.object_count",
    "Studio.resource_count": "AssetStudio.resource_count",
    "Studio.load_diagnostics": "AssetStudio.load_diagnostic_page",
    "Studio.files": "AssetStudio.files",
    "Studio.file": "AssetStudio.file_page",
    "Studio.resources": "AssetStudio.resources",
    "Studio.resource": "AssetStudio.read_resource",
    "Studio.resource_by_path": "AssetStudio.read_resource_by_path",
    "Studio.objects": "AssetStudio.objects",
    "Studio.object": "AssetStudio.read_raw",
    "Studio.scene_hierarchy": "AssetStudio.scene",
    "Studio.export": "AssetStudio.export",
    "Studio.extract": "extract",
    "Studio.write_static_fbx": "AssetStudio.read_static_fbx",
    "Studio.read_static_fbx": "AssetStudio.read_static_fbx",
    "Studio.write_fbx": "AssetStudio.read_fbx",
    "Studio.write_fbx_with_acl_decoder": "AssetStudio.read_fbx",
    "Studio.write_static_fbx_binary": "AssetStudio.read_static_fbx_binary",
    "Studio.read_static_fbx_binary": "AssetStudio.read_static_fbx_binary",
    "Studio.write_fbx_binary": "AssetStudio.read_fbx_binary",
    "Studio.write_fbx_binary_with_acl_decoder": "AssetStudio.read_fbx_binary",
    "Studio.read_fbx_binary": "AssetStudio.read_fbx_binary",
    "Studio.read_fbx_binary_with_acl_decoder": "AssetStudio.read_fbx_binary",
    "Studio.write_fbx_with_textures": "AssetStudio.read_fbx_with_textures",
    "Studio.read_model_obj": "AssetStudio.read_model_obj",
    "Studio.read_fbx": "AssetStudio.read_fbx",
    "Studio.read_fbx_with_acl_decoder": "AssetStudio.read_fbx",
    "Studio.split_object_fbx_candidates": "AssetStudio.split_object_fbx_candidates",
    "Studio.animator_fbx_candidates": "AssetStudio.animator_fbx_candidates",
    "Studio.write_game_object_fbx": "AssetStudio.read_game_object_fbx",
    "Studio.write_game_object_fbx_with_acl_decoder": "AssetStudio.read_game_object_fbx",
    "Studio.read_game_object_fbx": "AssetStudio.read_game_object_fbx",
    "Studio.read_game_object_fbx_with_acl_decoder": "AssetStudio.read_game_object_fbx",
    "Studio.live2d_packages": "AssetStudio.read_live2d_packages",
    "Studio.live2d_packages_with_schema_provider": "AssetStudio.read_live2d_packages",
    "Studio.live2d_packages_with_adapters": "AssetStudio.read_live2d_packages",
    "Studio.read_live2d_packages": "AssetStudio.read_live2d_packages",
    "Studio.read_live2d_packages_with_schema_provider": "AssetStudio.read_live2d_packages",
    "Studio.read_live2d_packages_with_adapters": "AssetStudio.read_live2d_packages",
    "StudioFile.index": "FileInfo.index",
    "StudioFile.path": "FileInfo.path",
    "StudioFile.unity_version": "FileInfo.unity_version",
    "StudioFile.object_count": "FileInfo.object_count",
    "StudioResource.index": "ResourceInfo.index",
    "StudioResource.path": "ResourceInfo.path",
    "StudioResource.byte_size": "ResourceInfo.byte_size",
    "StudioResource.write": "AssetStudio.read_resource",
    "StudioResource.write_range": "AssetStudio.read_resource_range",
    "StudioResource.read": "AssetStudio.read_resource",
    "StudioResource.read_range": "AssetStudio.read_resource_range",
    "StudioObject.file_index": "ObjectInfo.file_index",
    "StudioObject.object_index": "ObjectInfo.object_index",
    "StudioObject.source_path": "ObjectInfo.source_path",
    "StudioObject.path_id": "ObjectInfo.path_id",
    "StudioObject.class_id": "ObjectInfo.class_id",
    "StudioObject.byte_size": "ObjectInfo.byte_size",
    "StudioObject.name": "ObjectInfo.name",
    "StudioObject.container": "ObjectInfo.container",
    "StudioObject.write_raw": "AssetStudio.read_raw",
    "StudioObject.read_raw": "AssetStudio.read_raw",
    "StudioObject.read_text_bytes": "AssetStudio.read_text",
    "StudioObject.read_shader_text": "AssetStudio.read_shader",
    "StudioObject.write_mesh_obj": "AssetStudio.read_mesh_obj",
    "StudioObject.read_mesh_obj": "AssetStudio.read_mesh_obj",
    "StudioObject.read_animation_clip": "AssetStudio.read_animation_clip",
    "StudioObject.read_legacy_animation": "AssetStudio.read_legacy_animation",
    "StudioObject.read_animator_override_controller": (
        "AssetStudio.read_animator_override_controller"
    ),
    "StudioObject.read_asset_bundle": "AssetStudio.read_asset_bundle",
    "StudioObject.read_resource_manager": "AssetStudio.read_resource_manager",
    "StudioObject.read_preload_data": "AssetStudio.read_preload_data",
    "StudioObject.read_animator_controller": "AssetStudio.read_animator_controller",
    "StudioObject.read_avatar": "AssetStudio.read_avatar",
    "StudioObject.read_audio_clip": "AssetStudio.read_audio_clip",
    "StudioObject.read_font": "AssetStudio.read_font",
    "StudioObject.read_movie_texture": "AssetStudio.read_movie_texture",
    "StudioObject.read_video_clip": "AssetStudio.read_video_clip",
    "StudioObject.read_material": "AssetStudio.read_material",
    "StudioObject.read_mono_script": "AssetStudio.read_mono_script",
    "StudioObject.read_type_tree_json": "AssetStudio.read_type_tree_json",
    "StudioObject.write_type_tree_dump": "AssetStudio.read_type_tree_dump",
    "StudioObject.read_type_tree_dump": "AssetStudio.read_type_tree_dump",
    "StudioObject.read_mono_behaviour_json": "AssetStudio.read_mono_behaviour_json",
    "StudioObject.decode_texture_mip": "AssetStudio.read_texture",
    "StudioObject.decode_texture_array_mip0": "AssetStudio.read_texture_array",
    "StudioObject.read_sprite_atlas": "AssetStudio.read_sprite_atlas",
    "StudioObject.read_sprite": "AssetStudio.read_sprite_metadata",
    "StudioObject.decode_sprite": "AssetStudio.read_sprite",
    "StudioObject.read_build_settings": "AssetStudio.read_build_settings",
    "StudioObject.read_player_settings": "AssetStudio.read_player_settings",
    "StudioObject.read_cubism_expression": "AssetStudio.read_cubism_expression",
    "StudioObject.read_cubism_pose_part": "AssetStudio.read_cubism_pose_part",
    "StudioObject.read_cubism_display_info": "AssetStudio.read_cubism_display_info",
    "StudioObject.read_cubism_physics": "AssetStudio.read_cubism_physics",
    "StudioObject.read_cubism_fade_motion": "AssetStudio.read_cubism_fade_motion",
    "StudioObject.read_cubism_clip_motion": "AssetStudio.read_cubism_clip_motion",
    "StudioObject.read_cubism_clip_motion_with_acl_decoder": (
        "AssetStudio.read_cubism_acl_clip_motion"
    ),
}

INTENTIONAL_RUST_ONLY = {
    "Studio.from_collection": "accepts the low-level Rust AssetCollection type",
    "Studio.collection": "borrows the low-level Rust AssetCollection type",
    "Studio.into_collection": "moves the low-level Rust AssetCollection type",
    "Studio.object_by_index": (
        "returns a borrowed Rust StudioObject; Python exposes object_index as "
        "metadata but reads use the managed-compatible file/path key"
    ),
}

CORE_IMPL_RANGES = (
    ("Studio", "impl Studio {", "/// Borrowed metadata for one discovered serialized file."),
    ("StudioFile", "impl StudioFile<'_> {", "/// Borrowed, source-bound access"),
    ("StudioResource", "impl StudioResource<'_> {", "/// Borrowed handle to one real"),
    ("StudioObject", "impl StudioObject<'_> {", "#[cfg(test)]"),
)


class AuditError(ValueError):
    """The stub is not completely exercised by the strict consumer."""


def has_decorator(function: ast.FunctionDef, name: str) -> bool:
    return any(
        isinstance(decorator, ast.Name) and decorator.id == name
        for decorator in function.decorator_list
    )


def public_stub_members(source: str) -> tuple[set[str], set[str]]:
    tree = ast.parse(source, feature_version=(3, 9))
    studio = next(
        (
            node
            for node in tree.body
            if isinstance(node, ast.ClassDef) and node.name == "AssetStudio"
        ),
        None,
    )
    if studio is None:
        raise AuditError("stub does not define AssetStudio")
    methods: set[str] = set()
    properties: set[str] = set()
    for member in studio.body:
        if not isinstance(member, ast.FunctionDef) or member.name.startswith("_"):
            continue
        if has_decorator(member, "property"):
            properties.add(member.name)
        else:
            methods.add(member.name)
    return methods, properties


def public_stub_symbols(source: str) -> set[str]:
    """Return qualified class members and module functions from the stub."""
    tree = ast.parse(source, feature_version=(3, 9))
    symbols: set[str] = set()
    for node in tree.body:
        if isinstance(node, ast.FunctionDef) and not node.name.startswith("_"):
            symbols.add(node.name)
        elif isinstance(node, ast.ClassDef):
            for member in node.body:
                if isinstance(member, ast.FunctionDef):
                    symbols.add(f"{node.name}.{member.name}")
    return symbols


def core_studio_methods(source: str) -> set[str]:
    """Extract public methods from the four high-level impl blocks."""
    methods: set[str] = set()
    for type_name, start_marker, end_marker in CORE_IMPL_RANGES:
        start = source.find(start_marker)
        if start < 0:
            raise AuditError(f"Core source does not contain {start_marker!r}")
        end = source.find(end_marker, start + len(start_marker))
        if end < 0:
            raise AuditError(
                f"Core source does not contain {end_marker!r} after {start_marker!r}"
            )
        block = source[start + len(start_marker) : end]
        for line in block.splitlines():
            stripped = line.strip()
            prefix = "pub fn "
            const_prefix = "pub const fn "
            if stripped.startswith(prefix):
                remainder = stripped[len(prefix) :]
            elif stripped.startswith(const_prefix):
                remainder = stripped[len(const_prefix) :]
            else:
                continue
            name = remainder.split("(", 1)[0]
            if not name.isidentifier():
                raise AuditError(f"could not parse Core method declaration: {line!r}")
            qualified = f"{type_name}.{name}"
            if qualified in methods:
                raise AuditError(f"Core method is declared twice: {qualified}")
            methods.add(qualified)
    return methods


def validate_core_mapping(core_source: str, stub_source: str) -> tuple[int, int]:
    """Require every Rust high-level method to have a Python disposition."""
    core = core_studio_methods(core_source)
    classified = set(CORE_TO_PYTHON) | set(INTENTIONAL_RUST_ONLY)
    missing = sorted(core - classified)
    stale = sorted(classified - core)
    overlap = sorted(set(CORE_TO_PYTHON) & set(INTENTIONAL_RUST_ONLY))
    stub_symbols = public_stub_symbols(stub_source)
    missing_targets = sorted(
        f"{method} -> {target}"
        for method, target in CORE_TO_PYTHON.items()
        if target not in stub_symbols
    )
    details = []
    if missing:
        details.append("unclassified Core methods: " + ", ".join(missing))
    if stale:
        details.append("stale Core classifications: " + ", ".join(stale))
    if overlap:
        details.append("methods mapped and Rust-only: " + ", ".join(overlap))
    if missing_targets:
        details.append("missing Python targets: " + ", ".join(missing_targets))
    if details:
        raise AuditError("Core-to-Python mapping is incomplete (" + "; ".join(details) + ")")
    return len(core), len(INTENTIONAL_RUST_ONLY)


def consumed_members(source: str) -> tuple[set[str], set[str]]:
    tree = ast.parse(source, feature_version=(3, 9))
    consumer = next(
        (
            node
            for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == "consume_public_api"
        ),
        None,
    )
    if consumer is None:
        raise AuditError("strict consumer does not define consume_public_api")
    receivers = {"studio"}
    for node in ast.walk(consumer):
        if (
            isinstance(node, ast.AnnAssign)
            and isinstance(node.target, ast.Name)
            and isinstance(node.annotation, ast.Name)
            and node.annotation.id == "AssetStudio"
        ):
            receivers.add(node.target.id)

    calls: set[str] = set()
    attributes: set[str] = set()
    for node in ast.walk(consumer):
        if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
            if node.value.id in receivers or node.value.id == "AssetStudio":
                attributes.add(node.attr)
        if (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and (
                node.func.value.id in receivers
                or node.func.value.id == "AssetStudio"
            )
        ):
            calls.add(node.func.attr)
    return calls, attributes


def validate_surface(stub_source: str, consumer_source: str) -> tuple[int, int]:
    methods, properties = public_stub_members(stub_source)
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
            "strict Python consumer does not cover every public AssetStudio member ("
            + "; ".join(details)
            + ")"
        )
    return len(methods), len(properties)


def main() -> None:
    try:
        methods, properties = validate_surface(
            STUB.read_text(encoding="utf-8"),
            CONSUMER.read_text(encoding="utf-8"),
        )
        core_methods, rust_only = validate_core_mapping(
            CORE_STUDIO.read_text(encoding="utf-8"),
            STUB.read_text(encoding="utf-8"),
        )
    except AuditError as error:
        raise SystemExit(str(error)) from error
    print(
        "Python API surface audit passed "
        f"({methods} methods, {properties} properties; "
        f"{core_methods} Core methods classified, {rust_only} Rust-only)"
    )


if __name__ == "__main__":
    main()
