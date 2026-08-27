#!/usr/bin/env python3
"""Require every public ``UnityRs`` member in the stub to be type-consumed.

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
STUB = ROOT / "crates/unity-rs-python/python/unity_rs/__init__.pyi"
CONSUMER = ROOT / "crates/unity-rs-python/tests/typecheck_api.py"
CORE_STUDIO = ROOT / "crates/unity-rs-core/src/studio.rs"
PYTHON_BINDING = ROOT / "crates/unity-rs-python/src/lib.rs"

# Every public high-level Rust method must either name the Python symbol that
# represents it or be listed in INTENTIONAL_RUST_ONLY with a concrete ownership
# reason.  Several Rust streaming ``write_*`` methods intentionally map to one
# bounded Python byte-returning method; this table audits capabilities, not a
# spelling convention.
CORE_TO_PYTHON = {
    "Studio.open": "UnityRs.__new__",
    "Studio.open_with_options": "UnityRs.__new__",
    "Studio.open_region": "UnityRs.from_bytes",
    "Studio.open_region_with_options": "UnityRs.from_bytes",
    "Studio.open_regions": "UnityRs.from_memory_files",
    "Studio.open_regions_with_options": "UnityRs.from_memory_files",
    "Studio.file_count": "UnityRs.file_count",
    "Studio.object_count": "UnityRs.object_count",
    "Studio.resource_count": "UnityRs.resource_count",
    "Studio.load_diagnostics": "UnityRs.load_diagnostic_page",
    "Studio.files": "UnityRs.files",
    "Studio.file": "UnityRs.file_page",
    "Studio.resources": "UnityRs.resources",
    "Studio.resource": "UnityRs.read_resource",
    "Studio.resource_by_path": "UnityRs.read_resource_by_path",
    "Studio.objects": "UnityRs.objects",
    "Studio.object": "UnityRs.read_raw",
    "Studio.scene_hierarchy": "UnityRs.scene",
    "Studio.export": "UnityRs.export",
    "Studio.extract": "extract",
    "Studio.write_static_fbx": "UnityRs.read_static_fbx",
    "Studio.read_static_fbx": "UnityRs.read_static_fbx",
    "Studio.write_fbx": "UnityRs.read_fbx",
    "Studio.write_fbx_with_acl_decoder": "UnityRs.read_fbx",
    "Studio.write_static_fbx_binary": "UnityRs.read_static_fbx_binary",
    "Studio.read_static_fbx_binary": "UnityRs.read_static_fbx_binary",
    "Studio.write_fbx_binary": "UnityRs.read_fbx_binary",
    "Studio.write_fbx_binary_with_acl_decoder": "UnityRs.read_fbx_binary",
    "Studio.read_fbx_binary": "UnityRs.read_fbx_binary",
    "Studio.read_fbx_binary_with_acl_decoder": "UnityRs.read_fbx_binary",
    "Studio.write_fbx_with_textures": "UnityRs.read_fbx_with_textures",
    "Studio.read_model_obj": "UnityRs.read_model_obj",
    "Studio.read_fbx": "UnityRs.read_fbx",
    "Studio.read_fbx_with_acl_decoder": "UnityRs.read_fbx",
    "Studio.split_object_fbx_candidates": "UnityRs.split_object_fbx_candidates",
    "Studio.animator_fbx_candidates": "UnityRs.animator_fbx_candidates",
    "Studio.write_game_object_fbx": "UnityRs.read_game_object_fbx",
    "Studio.write_game_object_fbx_with_acl_decoder": "UnityRs.read_game_object_fbx",
    "Studio.read_game_object_fbx": "UnityRs.read_game_object_fbx",
    "Studio.read_game_object_fbx_with_acl_decoder": "UnityRs.read_game_object_fbx",
    "Studio.live2d_packages": "UnityRs.read_live2d_packages",
    "Studio.live2d_packages_with_schema_provider": "UnityRs.read_live2d_packages",
    "Studio.live2d_packages_with_adapters": "UnityRs.read_live2d_packages",
    "Studio.read_live2d_packages": "UnityRs.read_live2d_packages",
    "Studio.read_live2d_packages_with_schema_provider": "UnityRs.read_live2d_packages",
    "Studio.read_live2d_packages_with_adapters": "UnityRs.read_live2d_packages",
    "StudioFile.index": "FileInfo.index",
    "StudioFile.path": "FileInfo.path",
    "StudioFile.unity_version": "FileInfo.unity_version",
    "StudioFile.object_count": "FileInfo.object_count",
    "StudioResource.index": "ResourceInfo.index",
    "StudioResource.path": "ResourceInfo.path",
    "StudioResource.byte_size": "ResourceInfo.byte_size",
    "StudioResource.write": "UnityRs.read_resource",
    "StudioResource.write_range": "UnityRs.read_resource_range",
    "StudioResource.read": "UnityRs.read_resource",
    "StudioResource.read_range": "UnityRs.read_resource_range",
    "StudioObject.file_index": "ObjectInfo.file_index",
    "StudioObject.object_index": "ObjectInfo.object_index",
    "StudioObject.source_path": "ObjectInfo.source_path",
    "StudioObject.path_id": "ObjectInfo.path_id",
    "StudioObject.class_id": "ObjectInfo.class_id",
    "StudioObject.byte_size": "ObjectInfo.byte_size",
    "StudioObject.name": "ObjectInfo.name",
    "StudioObject.container": "ObjectInfo.container",
    "StudioObject.write_raw": "UnityRs.read_raw",
    "StudioObject.read_raw": "UnityRs.read_raw",
    "StudioObject.read_text_bytes": "UnityRs.read_text",
    "StudioObject.read_shader_text": "UnityRs.read_shader",
    "StudioObject.write_mesh_obj": "UnityRs.read_mesh_obj",
    "StudioObject.read_mesh_obj": "UnityRs.read_mesh_obj",
    "StudioObject.read_animation_clip": "UnityRs.read_animation_clip",
    "StudioObject.read_legacy_animation": "UnityRs.read_legacy_animation",
    "StudioObject.read_animator_override_controller": (
        "UnityRs.read_animator_override_controller"
    ),
    "StudioObject.read_asset_bundle": "UnityRs.read_asset_bundle",
    "StudioObject.read_resource_manager": "UnityRs.read_resource_manager",
    "StudioObject.read_preload_data": "UnityRs.read_preload_data",
    "StudioObject.read_animator_controller": "UnityRs.read_animator_controller",
    "StudioObject.read_avatar": "UnityRs.read_avatar",
    "StudioObject.read_audio_clip": "UnityRs.read_audio_clip",
    "StudioObject.read_font": "UnityRs.read_font",
    "StudioObject.read_movie_texture": "UnityRs.read_movie_texture",
    "StudioObject.read_video_clip": "UnityRs.read_video_clip",
    "StudioObject.read_material": "UnityRs.read_material",
    "StudioObject.read_mono_script": "UnityRs.read_mono_script",
    "StudioObject.read_type_tree_json": "UnityRs.read_type_tree_json",
    "StudioObject.write_type_tree_dump": "UnityRs.read_type_tree_dump",
    "StudioObject.read_type_tree_dump": "UnityRs.read_type_tree_dump",
    "StudioObject.read_mono_behaviour_json": "UnityRs.read_mono_behaviour_json",
    "StudioObject.decode_texture_mip": "UnityRs.read_texture",
    "StudioObject.decode_texture_array_mip0": "UnityRs.read_texture_array",
    "StudioObject.read_sprite_atlas": "UnityRs.read_sprite_atlas",
    "StudioObject.read_sprite": "UnityRs.read_sprite_metadata",
    "StudioObject.decode_sprite": "UnityRs.read_sprite",
    "StudioObject.read_build_settings": "UnityRs.read_build_settings",
    "StudioObject.read_player_settings": "UnityRs.read_player_settings",
    "StudioObject.read_cubism_expression": "UnityRs.read_cubism_expression",
    "StudioObject.read_cubism_pose_part": "UnityRs.read_cubism_pose_part",
    "StudioObject.read_cubism_display_info": "UnityRs.read_cubism_display_info",
    "StudioObject.read_cubism_physics": "UnityRs.read_cubism_physics",
    "StudioObject.read_cubism_fade_motion": "UnityRs.read_cubism_fade_motion",
    "StudioObject.read_cubism_clip_motion": "UnityRs.read_cubism_clip_motion",
    "StudioObject.read_cubism_clip_motion_with_acl_decoder": (
        "UnityRs.read_cubism_acl_clip_motion"
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


def rust_braced_block(source: str, marker: str) -> str:
    """Return one Rust block starting at the first brace after ``marker``."""
    marker_offset = source.find(marker)
    if marker_offset < 0:
        raise AuditError(f"Python binding does not contain {marker!r}")
    opening = source.find("{", marker_offset + len(marker))
    if opening < 0:
        raise AuditError(f"Python binding has no body after {marker!r}")
    depth = 0
    for offset in range(opening, len(source)):
        character = source[offset]
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return source[opening + 1 : offset]
    raise AuditError(f"Python binding has an unterminated body after {marker!r}")


def rust_parenthesized_call(source: str, marker: str) -> str:
    """Return one Rust call body, ignoring parentheses inside strings."""
    marker_offset = source.find(marker)
    if marker_offset < 0:
        raise AuditError(f"Python binding does not contain {marker!r}")
    opening = source.find("(", marker_offset + len(marker))
    if opening < 0:
        raise AuditError(f"Python binding has no call after {marker!r}")
    depth = 0
    in_string = False
    escaped = False
    for offset in range(opening, len(source)):
        character = source[offset]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
            continue
        if character == '"':
            in_string = True
        elif character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
            if depth == 0:
                return source[opening + 1 : offset]
    raise AuditError(f"Python binding has an unterminated call after {marker!r}")


def validate_texture_gil_boundary(source: str) -> None:
    """Keep texture row conversion inside the GIL-detached Rust closure."""
    expectations = (
        ("fn read_texture(", "DisplayRowPyImage::from_decoded(image)"),
        ("fn read_texture_array(", "DisplayRowPyImages::from_decoded(images)"),
    )
    for method_marker, conversion in expectations:
        method = rust_braced_block(source, method_marker)
        detach_offset = method.find("py.detach")
        if detach_offset < 0:
            raise AuditError(f"{method_marker[:-1]} does not release the GIL")
        detached = rust_braced_block(method[detach_offset:], "py.detach")
        if conversion not in detached:
            raise AuditError(
                f"{method_marker[:-1]} performs display-row conversion outside py.detach"
            )
        if "flip_rgba_rows" in method:
            raise AuditError(
                f"{method_marker[:-1]} directly flips rows while the GIL may be held"
            )


def validate_sprite_atlas_gil_boundary(source: str) -> None:
    """Keep input-amplifiable SpriteAtlas table projection outside the GIL."""
    method_marker = "fn read_sprite_atlas("
    method = rust_braced_block(source, method_marker)
    detach_offset = method.find("py.detach")
    if detach_offset < 0:
        raise AuditError("read_sprite_atlas does not release the GIL")
    detached = rust_braced_block(method[detach_offset:], "py.detach")
    preparation = "prepare_sprite_atlas(atlas)"
    if preparation not in detached:
        raise AuditError(
            "read_sprite_atlas performs metadata projection outside py.detach"
        )
    wrapping = "python_sprite_atlas(py, atlas)"
    if wrapping not in method or wrapping in detached:
        raise AuditError(
            "read_sprite_atlas does not keep Python object wrapping after py.detach"
        )


PAYLOAD_GIL_EXPECTATIONS = (
    ("fn read_audio_clip(", "materialize_audio_clip(audio, format, maximum_bytes)"),
    ("fn read_font(", "materialize_binary_asset(asset, maximum_bytes)"),
    ("fn read_movie_texture(", "materialize_binary_asset(asset, maximum_bytes)"),
    ("fn read_video_clip(", "materialize_binary_asset(asset, maximum_bytes)"),
)


def validate_image_encode_gil_boundary(source: str) -> None:
    """Keep pixel-proportional image encoding inside the detached closure."""
    method = rust_braced_block(source, "fn encode<")
    detach_offset = method.find("py.detach")
    if detach_offset < 0:
        raise AuditError("RgbaImage.encode does not release the GIL")
    detached = rust_braced_block(method[detach_offset:], "py.detach")
    if "encode_rgba_image(" not in detached:
        raise AuditError("RgbaImage.encode encodes pixels outside py.detach")


def validate_payload_gil_boundary(source: str) -> None:
    """Keep source-bound byte materialization inside the detached closure."""
    for method_marker, materialization in PAYLOAD_GIL_EXPECTATIONS:
        method = rust_braced_block(source, method_marker)
        detach_offset = method.find("py.detach")
        if detach_offset < 0:
            raise AuditError(f"{method_marker[:-1]} does not release the GIL")
        detached = rust_braced_block(method[detach_offset:], "py.detach")
        if materialization not in detached:
            raise AuditError(
                f"{method_marker[:-1]} materializes its payload outside py.detach"
            )


CUBISM_JSON_GIL_EXPECTATIONS = (
    ("fn read_cubism_expression(", "prepare_cubism_expression("),
    ("fn read_cubism_physics(", "python_cubism_physics("),
    ("fn read_cubism_fade_motion(", "python_cubism_fade_motion("),
    ("fn read_cubism_clip_motion(", "python_cubism_clip_motion("),
    ("fn read_cubism_acl_clip_motion(", "python_cubism_clip_motion("),
)


def validate_cubism_json_gil_boundary(source: str) -> None:
    """Keep bounded Cubism JSON production inside the detached closure."""
    for method_marker, preparation in CUBISM_JSON_GIL_EXPECTATIONS:
        method = rust_braced_block(source, method_marker)
        detach_offset = method.find("py.detach")
        if detach_offset < 0:
            raise AuditError(f"{method_marker[:-1]} does not release the GIL")
        detached = rust_braced_block(method[detach_offset:], "py.detach")
        if preparation not in detached:
            raise AuditError(
                f"{method_marker[:-1]} materializes Cubism JSON outside py.detach"
            )


METADATA_PROJECTION_GIL_EXPECTATIONS = (
    ("fn read_legacy_animation(", "prepare_legacy_animation("),
    (
        "fn read_animator_override_controller(",
        "prepare_animator_override_controller(",
    ),
    ("fn read_asset_bundle(", "prepare_asset_bundle("),
    ("fn read_resource_manager(", "prepare_resource_manager("),
    ("fn read_preload_data(", "prepare_preload_data("),
    ("fn read_animator_controller(", "prepare_animator_controller("),
    ("fn read_avatar(", "prepare_avatar("),
)


def validate_metadata_projection_gil_boundary(source: str) -> None:
    """Keep million-entry pure-Rust metadata projection outside the GIL."""
    for method_marker, preparation in METADATA_PROJECTION_GIL_EXPECTATIONS:
        method = rust_braced_block(source, method_marker)
        detach_offset = method.find("py.detach")
        if detach_offset < 0:
            raise AuditError(f"{method_marker[:-1]} does not release the GIL")
        detached = rust_braced_block(method[detach_offset:], "py.detach")
        if preparation not in detached:
            raise AuditError(
                f"{method_marker[:-1]} projects metadata outside py.detach"
            )


TABLE_PROJECTION_GIL_EXPECTATIONS = (
    ("fn load_diagnostic_page(", "prepare_load_diagnostic_page("),
    ("fn files(", "prepare_files("),
    ("fn objects(", "prepare_objects("),
    ("fn resources(", "prepare_resources("),
    ("fn file_page(", "prepare_file_page("),
    ("fn object_page(", "prepare_object_page("),
    ("fn resource_page(", "prepare_resource_page("),
    ("fn scene(", "prepare_scene_nodes("),
    ("fn split_object_fbx_candidates(", "python_fbx_candidates("),
    ("fn animator_fbx_candidates(", "python_fbx_candidates("),
    ("fn read_model_obj(", "skipped_textures("),
    ("fn read_fbx_with_textures(", "skipped_textures("),
    ("fn read_material(", "convert_material("),
    ("fn export(", "prepare_export_report("),
    ("fn extract(", "convert_extraction_report("),
)


def validate_table_projection_gil_boundary(source: str) -> None:
    """Keep collection/report table projection outside the GIL."""
    for method_marker, preparation in TABLE_PROJECTION_GIL_EXPECTATIONS:
        method = rust_braced_block(source, method_marker)
        detach_offset = method.find("py.detach")
        if detach_offset < 0:
            raise AuditError(f"{method_marker[:-1]} does not release the GIL")
        detached = rust_parenthesized_call(method[detach_offset:], "py.detach")
        if preparation not in detached:
            raise AuditError(
                f"{method_marker[:-1]} projects its result table outside py.detach"
            )


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
            if isinstance(node, ast.ClassDef) and node.name == "UnityRs"
        ),
        None,
    )
    if studio is None:
        raise AuditError("stub does not define UnityRs")
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
            and node.annotation.id == "UnityRs"
        ):
            receivers.add(node.target.id)

    calls: set[str] = set()
    attributes: set[str] = set()
    for node in ast.walk(consumer):
        if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
            if node.value.id in receivers or node.value.id == "UnityRs":
                attributes.add(node.attr)
        if (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and (
                node.func.value.id in receivers
                or node.func.value.id == "UnityRs"
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
            "strict Python consumer does not cover every public UnityRs member ("
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
        validate_texture_gil_boundary(PYTHON_BINDING.read_text(encoding="utf-8"))
        validate_sprite_atlas_gil_boundary(
            PYTHON_BINDING.read_text(encoding="utf-8")
        )
        validate_payload_gil_boundary(PYTHON_BINDING.read_text(encoding="utf-8"))
        validate_image_encode_gil_boundary(PYTHON_BINDING.read_text(encoding="utf-8"))
        validate_cubism_json_gil_boundary(PYTHON_BINDING.read_text(encoding="utf-8"))
        validate_metadata_projection_gil_boundary(
            PYTHON_BINDING.read_text(encoding="utf-8")
        )
        validate_table_projection_gil_boundary(
            PYTHON_BINDING.read_text(encoding="utf-8")
        )
    except AuditError as error:
        raise SystemExit(str(error)) from error
    print(
        "Python API surface audit passed "
        f"({methods} methods, {properties} properties; "
        f"{core_methods} Core methods classified, {rust_only} Rust-only; "
        "texture rows, SpriteAtlas tables, binary payloads, Cubism JSON and "
        "result tables detached)"
    )


if __name__ == "__main__":
    main()
