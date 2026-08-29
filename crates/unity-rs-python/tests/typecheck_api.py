"""Static consumer of the public Python surface.

This file is checked by mypy and is not executed. Runtime/wheel checks live in
``installed_wheel.py``; this one proves that the shipped annotations can be
used by an ordinary strict Python 3.9 caller, including the decoder aliases.
"""

from pathlib import Path
from typing import Any, Optional

from unity_rs import (
    AclCompressedTracks,
    AclDecodedClip,
    AclDecoder,
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
    CubismDisplayInfo,
    CubismExpression,
    CubismFadeMotion,
    CubismMotionTargets,
    CubismPhysics,
    CubismPosePart,
    ExportLimits,
    ExportReport,
    ExtractionLimits,
    ExtractionReport,
    FbxCandidate,
    FileInfo,
    LegacyAnimation,
    Live2dPackageSet,
    LoadDiagnostic,
    Material,
    ModelObj,
    ModelTextureLimits,
    MonoBehaviourJson,
    MonoBehaviourSchema,
    MonoBehaviourSchemas,
    MonoScript,
    ObjectInfo,
    OodleDecoder,
    PlayerSettings,
    PreloadData,
    ResourceInfo,
    ResourceManager,
    RgbaImage,
    SceneLimits,
    SceneNode,
    SpriteAtlas,
    SpriteAtlasRenderData,
    SpriteAtlasRenderDataKey,
    SpriteAtlasSecondaryTexture,
    SpriteMetadata,
    SpriteMetadataLimits,
    SpriteRenderData,
    SpriteSecondaryTexture,
    SpriteSettings,
    TexturedFbx,
    extract,
)
from unity_rs.compat import unitypy as UnityPyCompat


def decode_oodle(data: bytes, expected: int) -> bytes:
    return data[:expected]


def decode_acl(
    compressed_tracks: bytes,
    decoder_map: list[int],
    frame_count: int,
    bone_count: int,
    sample_rate: float,
    declared_curve_count: Optional[int],
    use_fast_sample_mode: Optional[bool],
) -> tuple[list[float], list[int], list[float], int]:
    del (
        compressed_tracks,
        decoder_map,
        frame_count,
        bone_count,
        sample_rate,
        declared_curve_count,
        use_fast_sample_mode,
    )
    return ([], [], [], 0)


oodle_decoder: OodleDecoder = decode_oodle
acl_decoder: AclDecoder = decode_acl


def consume_public_api(
    studio: UnityRs,
    schema: MonoBehaviourSchema,
    schemas: MonoBehaviourSchemas,
) -> None:
    path = Path("fixture.assets")
    opened: UnityRs = UnityRs(
        path,
        maximum_path_bytes=1_048_576,
        maximum_total_path_bytes=67_108_864,
        maximum_diagnostic_bytes=268_435_456,
        maximum_expanded_bytes=4_294_967_296,
        maximum_single_entry_bytes=536_870_912,
        oodle_decoder=oodle_decoder,
    )
    memory: UnityRs = UnityRs.from_bytes(
        b"",
        maximum_path_bytes=1_048_576,
        maximum_total_path_bytes=67_108_864,
        oodle_decoder=oodle_decoder,
    )
    memory_files: UnityRs = UnityRs.from_memory_files(
        [("fixture.assets", b"")],
        maximum_path_bytes=1_048_576,
        maximum_total_path_bytes=67_108_864,
        maximum_diagnostic_bytes=268_435_456,
        oodle_decoder=oodle_decoder,
    )
    compat: UnityPyCompat.Environment = UnityPyCompat.load(Path("fixture.assets"))
    compat_assets: list[UnityPyCompat.SerializedFile] = compat.assets
    compat_objects: list[UnityPyCompat.ObjectReader] = compat.objects
    if compat_assets:
        compat_file_objects: dict[int, UnityPyCompat.ObjectReader] = (
            compat_assets[0].objects
        )
    if compat_objects:
        compat_reader = compat_objects[0]
        compat_class: UnityPyCompat.ClassIDType = compat_reader.type
        compat_raw: bytes = compat_reader.get_raw_data()
        compat_dict: dict[str, Any] = compat_reader.parse_as_dict()
        compat_object: UnityPyCompat.Object = compat_reader.parse_as_object()
        compat_nodes: Optional[list[UnityPyCompat.TypeTreeNode]] = (
            compat_reader.serialized_type.nodes
        )
        compat_ptr = UnityPyCompat.PPtr(
            compat_reader.assets_file, 0, compat_reader.path_id
        )
        compat_resolved: Optional[UnityPyCompat.ObjectReader] = compat_ptr.deref()

    file_count: int = studio.file_count
    object_count: int = studio.object_count
    resource_count: int = studio.resource_count
    load_diagnostic_count: int = studio.load_diagnostic_count
    load_diagnostics: list[LoadDiagnostic] = studio.load_diagnostic_page(limit=1)
    cache_stats: tuple[int, int] = studio.sprite_page_cache_stats()
    files: list[FileInfo] = studio.files()
    objects: list[ObjectInfo] = studio.objects()
    resources: list[ResourceInfo] = studio.resources()
    file_iterator = studio.iter_files()
    object_iterator = studio.iter_objects()
    resource_iterator = studio.iter_resources()
    file_page: list[FileInfo] = studio.file_page()
    object_page: list[ObjectInfo] = studio.object_page(0)
    resource_page: list[ResourceInfo] = studio.resource_page()
    if files:
        file_info = files[0]
        file_effective_version: str = file_info.effective_unity_version
        file_format_version: int = file_info.format_version
        file_target_platform: int = file_info.target_platform
        file_endianness: int = file_info.endianness
        file_type_tree_enabled: bool = file_info.type_tree_enabled
        file_external_paths: list[str] = file_info.external_paths
    if objects:
        object_info = objects[0]
        object_byte_start: int = object_info.byte_start
        object_type_id: int = object_info.type_id
        object_serialized_type_index: Optional[int] = object_info.serialized_type_index
        object_destroyed: int = object_info.destroyed
        object_stripped: int = object_info.stripped
        object_script_type_index: Optional[int] = object_info.script_type_index
    scene_limits = SceneLimits(
        maximum_game_objects=100_000,
        maximum_index_bytes=64 * 1024 * 1024,
    )
    model_texture_limits = ModelTextureLimits(
        maximum_texture_references=1_024,
        maximum_textures=128,
        maximum_name_index_bytes=8 * 1024 * 1024,
        maximum_metadata_bytes=32 * 1024 * 1024,
        maximum_total_encoded_bytes=256 * 1024 * 1024,
        maximum_single_texture_bytes=64 * 1024 * 1024,
    )
    scene: list[SceneNode] = studio.scene(limits=scene_limits)
    candidates: list[FbxCandidate] = studio.split_object_fbx_candidates()
    animator_candidates: list[FbxCandidate] = studio.animator_fbx_candidates()

    resource: bytes = studio.read_resource(0)
    resource_range: bytes = studio.read_resource_range(0, 0, 1)
    resource_by_path: bytes = studio.read_resource_by_path("fixture.resS")
    raw: bytes = studio.read_raw(0, 1)
    text: bytes = studio.read_text(0, 1)
    shader: bytes = studio.read_shader(0, 1)
    mesh: bytes = studio.read_mesh_obj(0, 1)
    static_fbx: bytes = studio.read_static_fbx()
    static_binary_fbx: bytes = studio.read_static_fbx_binary()
    animated_fbx: bytes = studio.read_fbx(acl_decoder=acl_decoder)
    binary_fbx: bytes = studio.read_fbx_binary(acl_decoder=acl_decoder)
    branch_fbx: bytes = studio.read_game_object_fbx(0, 1, acl_decoder=acl_decoder)
    model_obj: ModelObj = studio.read_model_obj(
        texture_format="raw-rgba", texture_limits=model_texture_limits
    )
    textured_fbx: TexturedFbx = studio.read_fbx_with_textures(
        texture_format="tga", texture_limits=model_texture_limits
    )

    animation: AnimationClip = studio.read_animation_clip(0, 1)
    legacy_animation: LegacyAnimation = studio.read_legacy_animation(0, 1)
    override_controller: AnimatorOverrideController = (
        studio.read_animator_override_controller(0, 1)
    )
    asset_bundle: AssetBundle = studio.read_asset_bundle(0, 1)
    resource_manager: ResourceManager = studio.read_resource_manager(0, 1)
    preload_data: PreloadData = studio.read_preload_data(0, 1)
    acl_header: AclCompressedTracks = studio.inspect_acl_tracks(0, 1)
    acl_decoder_input: tuple[bytes, list[int]] = studio.read_acl_decoder_input(0, 1)
    acl_values: AclDecodedClip = studio.decode_acl_tracks(0, 1, acl_decoder)
    controller: AnimatorController = studio.read_animator_controller(0, 1)
    avatar: Avatar = studio.read_avatar(0, 1)
    rgba: RgbaImage = studio.read_texture(0, 1)
    array: list[RgbaImage] = studio.read_texture_array(0, 1)
    atlas: SpriteAtlas = studio.read_sprite_atlas(0, 1)
    atlas_entry: SpriteAtlasRenderData = atlas.render_data_entries[0]
    atlas_key: SpriteAtlasRenderDataKey = atlas_entry.key
    atlas_secondary: Optional[list[SpriteAtlasSecondaryTexture]] = (
        atlas_entry.secondary_textures
    )
    sprite_metadata_limits = SpriteMetadataLimits(maximum_entries=1_000)
    sprite_metadata: SpriteMetadata = studio.read_sprite_metadata(
        0, 1, limits=sprite_metadata_limits
    )
    sprite_render_data: SpriteRenderData = sprite_metadata.render_data
    sprite_settings: SpriteSettings = sprite_render_data.settings
    sprite_secondary: list[SpriteSecondaryTexture] = (
        sprite_render_data.secondary_textures
    )
    sprite: RgbaImage = studio.read_sprite(0, 1)
    encoded_sprite: bytes = sprite.encode(
        "png",
        jpeg_quality=90,
        compression="fast",
        png_filter="adaptive",
        maximum_bytes=1_048_576,
    )
    encoded_level: bytes = sprite.encode(compression=3)
    encoded_jpeg: bytes = sprite.encode(
        "jpeg",
        jpeg_sampling="4:4:4",
        jpeg_progressive=True,
        jpeg_optimized_huffman=True,
        jpeg_background=(255, 255, 255),
    )
    encoded_default: bytes = sprite.encode()
    audio: AudioClip = studio.read_audio_clip(0, 1)
    font: BinaryAsset = studio.read_font(0, 1)
    movie: BinaryAsset = studio.read_movie_texture(0, 1)
    video: BinaryAsset = studio.read_video_clip(0, 1)
    material: Material = studio.read_material(0, 1)
    script: MonoScript = studio.read_mono_script(0, 1)
    settings: BuildSettings = studio.read_build_settings(0, 1)
    player: PlayerSettings = studio.read_player_settings(0, 1)
    mono: MonoBehaviourJson = studio.read_mono_behaviour_json(0, 1, schema)
    mono_registry: MonoBehaviourJson = studio.read_mono_behaviour_json_with_schemas(
        0, 1, schemas
    )
    type_tree_json: str = studio.read_type_tree_json(0, 1)
    type_tree: Any = studio.read_type_tree(0, 1)
    supplied_type_tree: Any = studio.read_type_tree_with_nodes(
        0,
        1,
        [("TextAsset", "Base", -1, 0, 0, 1, 0, 0, 0)],
    )
    resolved_pptr: Optional[tuple[int, int, int]] = studio.resolve_pptr(0, 0, 1)
    type_tree_nodes: list[
        tuple[str, str, int, int, int, int, int, int, int]
    ] = studio.type_tree_nodes(0, 1)
    type_tree_dump: str = studio.read_type_tree_dump(0, 1)

    expression: CubismExpression = studio.read_cubism_expression(0, 1)
    pose: CubismPosePart = studio.read_cubism_pose_part(0, 1)
    display: CubismDisplayInfo = studio.read_cubism_display_info(0, 1)
    physics: CubismPhysics = studio.read_cubism_physics(0, 1)
    fade: CubismFadeMotion = studio.read_cubism_fade_motion(0, 1)
    targets = CubismMotionTargets(parameters=["ParamAngleX"], parts=["PartArmL"])
    motion: CubismClipMotion = studio.read_cubism_clip_motion(0, 1, targets=targets)
    acl_motion: CubismClipMotion = studio.read_cubism_acl_clip_motion(
        0, 1, acl_decoder, targets=targets
    )
    packages: Live2dPackageSet = studio.read_live2d_packages(
        schemas=schemas,
        acl_decoder=acl_decoder,
    )

    export_report: ExportReport = studio.export(
        path,
        compression="fast",
        png_filter="auto",
        limits=ExportLimits(maximum_metadata_bytes=268_435_456),
    )
    extraction_limits = ExtractionLimits(
        maximum_total_path_bytes=67_108_864,
        maximum_metadata_bytes=268_435_456,
    )
    extraction_metadata_bytes: int = extraction_limits.maximum_metadata_bytes
    extraction_report: ExtractionReport = extract(
        path,
        path,
        limits=extraction_limits,
        oodle_decoder=oodle_decoder,
    )

    del (
        opened,
        memory,
        memory_files,
        file_count,
        object_count,
        resource_count,
        load_diagnostic_count,
        load_diagnostics,
        files,
        objects,
        resources,
        file_iterator,
        object_iterator,
        resource_iterator,
        file_page,
        object_page,
        resource_page,
        scene,
        candidates,
        animator_candidates,
        resource,
        resource_range,
        resource_by_path,
        raw,
        text,
        shader,
        mesh,
        static_fbx,
        static_binary_fbx,
        animated_fbx,
        binary_fbx,
        branch_fbx,
        model_obj,
        textured_fbx,
        animation,
        legacy_animation,
        override_controller,
        asset_bundle,
        resource_manager,
        preload_data,
        acl_header,
        acl_decoder_input,
        acl_values,
        controller,
        avatar,
        rgba,
        array,
        atlas,
        atlas_entry,
        atlas_key,
        atlas_secondary,
        sprite_metadata,
        sprite_metadata_limits,
        sprite_render_data,
        sprite_settings,
        sprite_secondary,
        sprite,
        cache_stats,
        encoded_sprite,
        encoded_level,
        encoded_jpeg,
        encoded_default,
        audio,
        font,
        movie,
        video,
        material,
        script,
        settings,
        player,
        mono,
        mono_registry,
        type_tree_json,
        type_tree_dump,
        expression,
        pose,
        display,
        physics,
        fade,
        motion,
        acl_motion,
        packages,
        export_report,
        extraction_report,
        extraction_metadata_bytes,
    )
