//! Thin Node-API bindings over the safe high-level Rust API.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::sync::Arc;

use napi::bindgen_prelude::{Array, AsyncTask, BigInt, Buffer, FnArgs, Object};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, Error, JsString, JsValue, Result, Status, Task, Unknown, ValueType};
use napi_derive::napi;
use unity_rs_core::acl::{AclCompressedTracksLimits, AclDecodeLimits};
use unity_rs_core::animation_clip::AnimationClipReadLimits;
use unity_rs_core::animation_component::{AnimationClipOverride, AnimationComponentReadLimits};
use unity_rs_core::animator_controller::AnimatorControllerReadLimits;
use unity_rs_core::avatar::AvatarReadLimits;
use unity_rs_core::export::{
    AudioExportFormat, ExportMode, ExportOptions as CoreExportOptions,
    ExportReport as CoreExportReport, FilenameFormat,
};
use unity_rs_core::extraction::ExtractionOptions;
use unity_rs_core::image_export::{
    DEFAULT_JPEG_QUALITY, ImageFormat, ImageRowOrder, PngCompression,
};
use unity_rs_core::live2d_clip_motion::CubismClipMotionReadLimits;
use unity_rs_core::live2d_motion::{
    CubismFadeMotionReadLimits, CubismMotionTargetIndexLimits, CubismMotionTargetNames,
};
use unity_rs_core::live2d_package::{
    Live2dPackageBytesSet as CoreLive2dPackageBytesSet, Live2dPackageLimits,
    Live2dPackageMaterializeLimits,
};
use unity_rs_core::live2d_physics::CubismPhysicsReadLimits;
use unity_rs_core::live2d_schema::{CubismAuxiliaryReadLimits, CubismExpressionReadLimits};
use unity_rs_core::loader::{
    AssetLoadLimits, AssetLoadOptions, DEFAULT_MAXIMUM_LOAD_PATH_BYTES,
    DEFAULT_MAXIMUM_TOTAL_LOAD_PATH_BYTES, LoadFailurePolicy,
};
use unity_rs_core::material::MaterialReadLimits;
use unity_rs_core::mesh::MeshReadLimits;
use unity_rs_core::model_export::{ModelExportCandidate, ModelExportPlanLimits};
use unity_rs_core::mono_schema::{
    MonoBehaviourSchemaEntry, MonoBehaviourSchemaProvider, MonoBehaviourSchemaRegistry,
    MonoBehaviourSchemaSource, ResolvedMonoBehaviourJson,
};
use unity_rs_core::monobehaviour::MonoBehaviourReadLimits;
use unity_rs_core::project_settings::ProjectSettingsReadLimits;
use unity_rs_core::scene_hierarchy::SceneHierarchyLimits;
use unity_rs_core::scene_hierarchy::SceneObjectKey;
use unity_rs_core::scene_textures::SceneTextureLimits;
use unity_rs_core::serialized::{ContainerMetadataReadLimits, TypeTree, TypeTreeNode};
use unity_rs_core::simple_assets::{
    AudioClipAsset, SimpleAssetReadLimits, SimpleBinaryAsset, direct_wav_output_size,
    write_direct_wav,
};
use unity_rs_core::source::Region;
use unity_rs_core::sprite::{Sprite, SpriteMeshType, SpritePackingMode, SpriteReadLimits};
use unity_rs_core::sprite_atlas::{SpriteAtlas, SpriteAtlasReadLimits};
use unity_rs_core::studio::{Studio, StudioObject};
use unity_rs_core::texture::TextureReadLimits;
use unity_rs_core::texture_array::TextureArrayReadLimits;
use unity_rs_core::unity_cn::UnityCnKey;
use unity_rs_core::unity_version::UnityVersion;

const DEFAULT_PAYLOAD_LIMIT: u64 = 512 * 1024 * 1024;
const DEFAULT_LIVE2D_TOTAL_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
const DEFAULT_PAGE_LIMIT: u32 = 4096;
const MAXIMUM_PAGE_LIMIT: u32 = 1_000_000;
const MAXIMUM_MEMORY_INPUT_NAME_BYTES: usize = DEFAULT_MAXIMUM_LOAD_PATH_BYTES;
const MAXIMUM_TOTAL_MEMORY_INPUT_NAME_BYTES: usize = DEFAULT_MAXIMUM_TOTAL_LOAD_PATH_BYTES;
const MAXIMUM_SCHEMA_ENTRIES: usize = 100_000;
const MAXIMUM_SCHEMA_NODES_PER_ENTRY: usize = 100_000;
const MAXIMUM_TOTAL_SCHEMA_NODES: usize = 1_000_000;
const MAXIMUM_SCHEMA_STRING_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_OPTION_DIAGNOSTIC_BYTES: usize = 64;

#[napi(object)]
pub struct FileInfo {
    pub index: u32,
    pub path: String,
    pub unity_version: String,
    pub object_count: u32,
}

#[napi(object)]
pub struct ObjectInfo {
    pub file_index: u32,
    pub object_index: u32,
    pub source_path: String,
    pub path_id: BigInt,
    pub class_id: i32,
    pub byte_size: BigInt,
    pub name: Option<String>,
    pub container: Option<String>,
}

#[napi(object)]
pub struct ResourceInfo {
    pub index: u32,
    pub path: String,
    pub byte_size: BigInt,
}

#[napi(object)]
pub struct LoadDiagnosticInfo {
    pub path: String,
    pub message: String,
}

/// One Unity serialized-object pointer.
#[napi(object)]
pub struct ObjectReference {
    pub file_id: i32,
    pub path_id: BigInt,
}

/// Stable references from one legacy Unity `Animation` component.
#[napi(object)]
pub struct LegacyAnimationInfo {
    pub path_id: BigInt,
    pub game_object: ObjectReference,
    pub enabled: u32,
    pub default_clip: ObjectReference,
    pub clips: Vec<ObjectReference>,
    pub trailing_bytes: BigInt,
}

/// One original-to-replacement clip entry in an override controller.
#[napi(object)]
pub struct AnimationClipOverrideInfo {
    pub original_clip: ObjectReference,
    pub override_clip: ObjectReference,
}

/// Stable references from one Unity `AnimatorOverrideController`.
#[napi(object)]
pub struct AnimatorOverrideControllerInfo {
    pub path_id: BigInt,
    pub name: String,
    pub controller: ObjectReference,
    pub clip_overrides: Vec<AnimationClipOverrideInfo>,
    pub trailing_bytes: BigInt,
}

/// One named entry in an `AssetBundle` container table.
#[napi(object)]
pub struct AssetBundleContainerEntry {
    pub key: String,
    pub preload_index: u32,
    pub preload_size: u32,
    pub asset: ObjectReference,
}

/// Bounded, ordered metadata from one Unity `AssetBundle`.
#[napi(object)]
pub struct AssetBundleInfo {
    pub path_id: BigInt,
    pub name: String,
    pub object_name: String,
    pub asset_bundle_name: Option<String>,
    pub preload_table: Vec<ObjectReference>,
    pub container: Vec<AssetBundleContainerEntry>,
    pub dependencies: Vec<String>,
    pub is_streamed_scene_asset_bundle: bool,
}

/// One named entry in a `ResourceManager` container table.
#[napi(object)]
pub struct ResourceManagerContainerEntry {
    pub key: String,
    pub asset: ObjectReference,
}

/// Bounded, ordered metadata from one Unity `ResourceManager`.
#[napi(object)]
pub struct ResourceManagerInfo {
    pub path_id: BigInt,
    pub container: Vec<ResourceManagerContainerEntry>,
}

/// Bounded, ordered metadata from one Unity `PreloadData`.
#[napi(object)]
pub struct PreloadDataInfo {
    pub path_id: BigInt,
    pub name: String,
    pub assets: Vec<ObjectReference>,
}

/// Serialized composite key used by one `SpriteAtlas` render-data entry.
#[napi(object)]
pub struct SpriteAtlasRenderDataKey {
    /// GUID bytes in Unity's original serialized order.
    pub guid_bytes: Buffer,
    pub value: BigInt,
}

#[napi(object)]
pub struct SpriteAtlasVector2 {
    pub x: f64,
    pub y: f64,
}

#[napi(object)]
pub struct SpriteAtlasVector4 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

#[napi(object)]
pub struct SpriteAtlasRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Raw and decoded `SpriteSettings` bits.
#[napi(object)]
pub struct SpriteAtlasSettings {
    pub raw: u32,
    pub packed: bool,
    pub packing_mode: u32,
    pub packing_rotation: u32,
    pub mesh_type: u32,
}

#[napi(object)]
pub struct SpriteAtlasSecondaryTexture {
    pub texture: ObjectReference,
    pub name: String,
}

/// Complete crop, texture and packing metadata for one atlas key.
#[napi(object)]
pub struct SpriteAtlasRenderData {
    pub key: SpriteAtlasRenderDataKey,
    pub texture: ObjectReference,
    pub alpha_texture: ObjectReference,
    pub texture_rect: SpriteAtlasRect,
    pub texture_rect_offset: SpriteAtlasVector2,
    pub atlas_rect_offset: SpriteAtlasVector2,
    pub uv_transform: SpriteAtlasVector4,
    pub downscale_multiplier: f64,
    pub settings: SpriteAtlasSettings,
    /// Absent before Unity 2020.2; present and possibly empty afterwards.
    pub secondary_textures: Option<Vec<SpriteAtlasSecondaryTexture>>,
}

/// Complete, bounded metadata from one Unity `SpriteAtlas` object.
#[napi(object)]
pub struct SpriteAtlasInfo {
    pub path_id: BigInt,
    pub name: String,
    pub packed_sprites: Vec<ObjectReference>,
    pub packed_sprite_names: Vec<String>,
    pub render_data_entries: Vec<SpriteAtlasRenderData>,
    pub tag: String,
    pub is_variant: bool,
}

/// Caller-configurable budgets for metadata-only `Sprite` parsing.
#[napi(object)]
pub struct SpriteMetadataLimits {
    pub maximum_entries: Option<u32>,
    pub maximum_string_bytes: Option<i64>,
    pub maximum_total_string_bytes: Option<i64>,
    pub maximum_mesh_bytes: Option<i64>,
}

#[napi(object)]
pub struct SpriteVector2 {
    pub x: f64,
    pub y: f64,
}

#[napi(object)]
pub struct SpriteVector4 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

#[napi(object)]
pub struct SpriteRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Raw and decoded `SpriteSettings` bits from resident render data.
#[napi(object)]
pub struct SpriteSettings {
    pub raw: u32,
    pub packed: bool,
    pub packing_mode: String,
    pub packing_rotation: u32,
    pub mesh_type: String,
}

#[napi(object)]
pub struct SpriteSecondaryTexture {
    pub texture: ObjectReference,
    pub name: String,
}

/// One validated local-space triangle used for tight-mesh masking.
#[napi(object)]
pub struct SpriteTriangle {
    pub first: SpriteVector2,
    pub second: SpriteVector2,
    pub third: SpriteVector2,
}

/// Complete resident render metadata stored directly on one `Sprite`.
#[napi(object)]
pub struct SpriteRenderData {
    pub texture: ObjectReference,
    pub alpha_texture: ObjectReference,
    pub secondary_textures: Vec<SpriteSecondaryTexture>,
    pub texture_rect: SpriteRect,
    pub texture_rect_offset: SpriteVector2,
    pub atlas_rect_offset: SpriteVector2,
    pub settings: SpriteSettings,
    pub uv_transform: SpriteVector4,
    pub downscale_multiplier: f64,
    pub mesh_triangles: Vec<SpriteTriangle>,
}

/// Complete, bounded metadata from one Unity `Sprite` object.
#[napi(object)]
pub struct SpriteMetadata {
    pub object_index: u32,
    pub path_id: BigInt,
    pub name: String,
    pub rect: SpriteRect,
    pub offset: SpriteVector2,
    pub border: SpriteVector4,
    pub pixels_to_units: f64,
    pub pivot: SpriteVector2,
    pub extrude: u32,
    pub is_polygon: bool,
    pub render_data_key: Option<SpriteAtlasRenderDataKey>,
    pub atlas_tags: Vec<String>,
    pub sprite_atlas: ObjectReference,
    pub render_data: SpriteRenderData,
}

/// A decoded RGBA8 image.
#[napi(object)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8 rows in display order: the first row is the top
    /// edge of the image, matching the Python binding and the CLI's encoded
    /// output. `Texture2D` decoders work bottom-up, so these rows have already
    /// been flipped.
    pub pixels: Buffer,
}

/// Options for encoding one decoded RGBA image into a file payload.
#[napi(object)]
pub struct EncodeImageOptions {
    /// `jpeg`, `png`, `bmp`, `tga`, `webp`, or `raw-rgba`. Defaults to `png`.
    pub image_format: Option<String>,
    /// JPEG-only quality from 1 through 100. Defaults to 75.
    pub jpeg_quality: Option<u32>,
    /// PNG-only zlib effort: `fast`, `default`, or `best`. Defaults to
    /// `default`. Every level is lossless; the effort trades encode CPU for
    /// file size.
    pub compression: Option<String>,
    /// Cap on the encoded output length in bytes. Defaults to 512 MiB.
    pub maximum_bytes: Option<i64>,
}

/// One `AudioClip`'s stored payload and the extension its container implies.
#[napi(object)]
pub struct AudioClip {
    pub name: String,
    /// The extension the stored bytes carry, `.fsb` or `.wav` for example.
    pub extension: String,
    /// `audio_raw` for the serialized payload or `audio_wav` for a verified
    /// decoder-free WAV materialization.
    pub payload_kind: String,
    /// True when Core can produce WAV bytes without an external decoder.
    pub is_direct_wav: bool,
    pub data: Buffer,
}

/// One Cubism document produced from a single behaviour.
///
/// The bytes are the document itself; the counts alongside are what a caller
/// would otherwise have to parse the JSON to learn.
#[napi(object)]
pub struct CubismDocument {
    pub name: String,
    /// The document's own JSON text.
    pub json: Buffer,
    /// Sub-rigs for physics, parameters for an expression, curves for a
    /// motion. Zero for a document that has no such notion.
    pub entry_count: u32,
}

/// One embedded-schema `CubismPosePart` component.
#[napi(object)]
pub struct CubismPosePart {
    pub path_id: BigInt,
    pub group_index: i32,
    pub links: Vec<String>,
}

/// One embedded-schema Cubism display-info component.
#[napi(object)]
pub struct CubismDisplayInfo {
    pub path_id: BigInt,
    pub name: String,
    pub display_name: Option<String>,
    /// `displayName` when it is non-empty, otherwise `name`.
    pub effective_name: String,
}

/// Parameter and part identifiers used to resolve `AnimationClip` bindings.
#[napi(object)]
pub struct CubismMotionTargets {
    pub parameters: Option<Vec<String>>,
    pub parts: Option<Vec<String>>,
}

/// One real Unity `AnimationClip` projected to Cubism motion3 JSON.
#[napi(object)]
pub struct CubismClipMotion {
    pub file_index: u32,
    pub path_id: BigInt,
    pub name: String,
    pub duration: f64,
    pub fps: f64,
    pub curve_count: u32,
    pub keyframe_count: u32,
    pub event_count: u32,
    pub json: Buffer,
}

/// One `GameObject` branch that can be exported as its own FBX.
#[napi(object)]
pub struct FbxCandidate {
    pub file_index: u32,
    pub path_id: BigInt,
    /// The Animator that owns this branch, when one does.
    pub animator_file_index: Option<u32>,
    pub animator_path_id: Option<BigInt>,
    pub name: String,
}

/// A resident binary asset: a `Font`, a legacy `MovieTexture`, or a `VideoClip`.
///
/// All three are a name and a blob whose format the file declares, so one shape
/// covers them rather than three that differ only in what they are called.
#[napi(object)]
pub struct BinaryAsset {
    pub name: String,
    /// What the payload is, as the reader classified it.
    pub kind: String,
    /// The extension the stored bytes carry, `.ttf` or `.ogg` for example.
    pub extension: String,
    pub data: Buffer,
}

/// The identity fields of a `MonoScript`, which name the type a
/// `MonoBehaviour` deserializes as.
#[napi(object)]
pub struct MonoScript {
    pub name: String,
    pub class_name: String,
    pub namespace: String,
    pub assembly_name: String,
    pub execution_order: Option<i32>,
}

/// A `Material`'s shader reference and its named property sheets.
#[napi(object)]
pub struct Material {
    pub name: String,
    pub shader_file_id: i32,
    pub shader_path_id: BigInt,
    pub texture_properties: Vec<String>,
    pub float_properties: Vec<String>,
    pub color_properties: Vec<String>,
}

/// The scene lists a `BuildSettings` object records.
#[napi(object)]
pub struct BuildSettings {
    /// Pre-5.x level paths, absent on newer layouts.
    pub levels: Option<Vec<String>>,
    /// 5.x and newer scene paths, absent on older layouts.
    pub scenes: Option<Vec<String>>,
}

/// The identity fields of a `PlayerSettings` object.
#[napi(object)]
pub struct PlayerSettings {
    pub company_name: String,
    pub product_name: String,
}

/// One ordered Avatar TOS path entry.
#[napi(object)]
pub struct AvatarPathEntry {
    pub hash: u32,
    pub path: String,
}

/// Complete stable skeleton, TOS, and human-description metadata from an
/// `Avatar`.
#[napi(object)]
pub struct Avatar {
    pub path_id: BigInt,
    pub name: String,
    /// Compatibility alias retained for callers of the original Node slice.
    pub declared_size: u32,
    /// The size the object declares for its constant block.
    pub declared_avatar_size: u32,
    pub skeleton_node_count: u32,
    pub human_skeleton_node_count: u32,
    /// Bone path entries, retained in order so duplicate hashes keep Unity's
    /// first-hit behaviour.
    pub path_count: u32,
    pub paths: Vec<AvatarPathEntry>,
    pub has_human_description: bool,
    pub human_bone_count: u32,
    pub skeleton_bone_count: u32,
    pub root_motion_bone_name: Option<String>,
}

/// One `GameObject` in the assembled hierarchy.
///
/// The component flags are separate booleans rather than a bitfield because
/// this is a JavaScript-facing shape and a bitfield would need decoding on the
/// other side.
#[allow(clippy::struct_excessive_bools)]
#[napi(object)]
pub struct SceneNode {
    pub file_index: u32,
    pub path_id: BigInt,
    pub name: String,
    /// Absent for a `GameObject` with no `Transform`, which cannot be placed.
    pub parent_path_id: Option<BigInt>,
    pub child_count: u32,
    pub has_transform: bool,
    pub has_mesh_renderer: bool,
    pub has_skinned_mesh_renderer: bool,
    pub has_animator: bool,
}

/// One in-memory input for a multi-file open.
#[napi(object)]
pub struct MemoryInput {
    pub name: String,
    pub data: Buffer,
}

/// One object the exporter wrote.
#[napi(object)]
pub struct ExportRecord {
    pub source: String,
    pub path_id: BigInt,
    pub class_id: i32,
    pub output_path: String,
    /// What the bytes are, `image_png` or `mesh_obj` for example.
    pub payload_kind: String,
}

/// One object the exporter could not write, and why.
#[napi(object)]
pub struct ExportFailure {
    pub source: String,
    pub path_id: BigInt,
    pub class_id: i32,
    pub error: String,
}

/// What an export run produced. Failures are reported rather than thrown so
/// one unreadable object does not cost the whole run.
#[napi(object)]
pub struct ExportReport {
    pub exported: Vec<ExportRecord>,
    pub failures: Vec<ExportFailure>,
    /// Objects declined by design rather than broken, kept separate so a
    /// caller can tell "this build carries shaders we do not read" from "the
    /// export went wrong".
    pub unsupported: Vec<ExportFailure>,
}

/// What an extraction run produced.
#[napi(object)]
pub struct ExtractionReport {
    pub extracted_count: u32,
    pub skipped_existing_count: u32,
    pub failure_count: u32,
    pub output_bytes: BigInt,
}

/// Complete bounded `AnimationClip` shape, muscle, ACL, and streaming metadata.
///
/// Core still parses the complete object and validates ordinary keyframes;
/// their arrays stay in Rust and are summarized by counts rather than copied
/// into JavaScript. Separate booleans rather than a bitfield keep callers from
/// having to reproduce Core's decoding.
#[allow(clippy::struct_excessive_bools)]
#[napi(object)]
pub struct AnimationClipInfo {
    pub path_id: BigInt,
    pub name: String,
    pub sample_rate: f64,
    pub wrap_mode: i32,
    pub legacy: bool,
    pub compressed: bool,
    pub use_high_quality_curve: bool,
    pub rotation_curve_count: u32,
    pub position_curve_count: u32,
    pub scale_curve_count: u32,
    pub euler_curve_count: u32,
    pub float_curve_count: u32,
    pub pptr_curve_count: u32,
    pub muscle_clip_size: u32,
    /// Present when the clip carries muscle (humanoid) data.
    pub has_muscle_clip: bool,
    pub streamed_curve_count: Option<u32>,
    pub dense_curve_count: Option<u32>,
    pub constant_value_count: Option<u32>,
    pub has_acl: bool,
    pub acl_frame_count: Option<u32>,
    pub acl_bone_count: Option<u32>,
    pub acl_sample_rate: Option<f64>,
    pub acl_curve_count: Option<u32>,
    pub acl_track_byte_count: Option<BigInt>,
    pub acl_decoder_count: Option<u32>,
    pub acl_use_fast_sample_mode: Option<bool>,
    /// Present when the clip's samples live in a sibling resource file.
    pub has_streaming_info: bool,
    pub streaming_offset: Option<BigInt>,
    pub streaming_size: Option<u32>,
    pub streaming_path: Option<String>,
}

/// An `AnimatorController`'s identity and the clips it references.
#[napi(object)]
pub struct AnimatorControllerInfo {
    pub name: String,
    /// Transform-path strings the controller's bindings resolve through.
    pub tos_entry_count: u32,
    pub animation_clip_path_ids: Vec<BigInt>,
}

/// One Live2D model discovered in the collection.
#[allow(clippy::struct_excessive_bools)]
#[napi(object)]
pub struct Live2dPackageInfo {
    pub name: String,
    pub directory_name: String,
    pub moc_file_name: String,
    pub texture_count: u32,
    pub expression_count: u32,
    pub motion_count: u32,
    pub has_physics: bool,
    pub has_pose: bool,
    pub has_display_info: bool,
}

/// One file belonging to a materialized Live2D package.
#[napi(object)]
pub struct Live2dFile {
    /// Path relative to the package directory.
    pub file_name: String,
    pub data: Buffer,
}

/// One materialized Live2D model: every file it needs, in memory.
#[napi(object)]
pub struct Live2dPackageFiles {
    pub name: String,
    pub directory_name: String,
    pub files: Vec<Live2dFile>,
}

/// Something a package could not carry, and why.
///
/// A Live2D model routinely resolves partly: a fade-motion list points at a
/// clip in a bundle that was not loaded, or a component's schema is not
/// available. Discovery keeps going, and these say what was left out. Without
/// them a short package looks like a complete one.
#[napi(object)]
pub struct Live2dDiagnostic {
    pub file_index: u32,
    pub path_id: BigInt,
    pub kind: String,
    pub detail: String,
}

/// Materialized packages and what discovery could not include.
#[napi(object)]
pub struct Live2dPackageSet {
    pub packages: Vec<Live2dPackageFiles>,
    pub diagnostics: Vec<Live2dDiagnostic>,
}

/// An FBX plus the texture files it references by name.
#[napi(object)]
pub struct TexturedFbx {
    pub fbx: Buffer,
    /// Each must be written beside the FBX for its reference to resolve.
    pub textures: Vec<Live2dFile>,
    /// Texture references this reader could not resolve or decode, with the
    /// reason. Reported rather than raised so one bad texture does not cost the
    /// model.
    pub skipped: Vec<String>,
}

/// A scene written as Wavefront OBJ, with the files it names.
///
/// The OBJ's `mtllib` line names the material library and the library's
/// `map_*` lines name the textures, all resolved by file name against the
/// OBJ's own directory. They come back rather than being written because this
/// call has no directory of its own, and splitting them across directories
/// breaks the references.
#[napi(object)]
pub struct ModelObj {
    pub obj: Buffer,
    pub material_library_name: String,
    pub material_library: Buffer,
    pub textures: Vec<Live2dFile>,
    /// Texture references this reader could not resolve or decode, with the
    /// reason.
    pub skipped: Vec<String>,
}

/// The header of one ACL compressed-track blob.
///
/// Enough to decide whether a caller's decoder can handle it, without
/// decompressing anything.
///
/// The state flags stay separate booleans rather than the packed bits Core
/// keeps them in: this is a JavaScript-facing shape, and a bitfield would only
/// move the unpacking to the other side.
#[allow(clippy::struct_excessive_bools)]
#[napi(object)]
pub struct AclTracks {
    pub declared_size: u32,
    pub stored_hash: u32,
    pub version: u16,
    pub track_type: String,
    pub track_count: u32,
    pub samples_per_track: u32,
    pub sample_rate: f64,
    pub decompressed_value_count: BigInt,
    pub has_metadata: bool,
    pub is_wrap_optimized: bool,
    pub has_database: bool,
    pub has_stripped_keyframes: bool,
}

/// What an injected ACL decoder is asked to decode.
#[napi(object)]
pub struct AclDecodeRequest {
    pub frame_count: u32,
    pub bone_count: u32,
    pub sample_rate: f64,
    pub declared_curve_count: Option<u32>,
    pub use_fast_sample_mode: Option<bool>,
    /// The validated compressed-track bytes.
    pub compressed_tracks: Buffer,
    /// Tuanjie's decoder map, empty when the clip carries none.
    pub decoder_map: Vec<u32>,
}

/// Frame-major scalar curves an injected ACL decoder returns.
///
/// `bindingIndices[column]` is the absolute Unity binding scalar index for
/// `values[frame * bindingIndices.length + column]`, and the indices must be
/// strictly increasing.
#[napi(object)]
pub struct AclDecodedClip {
    pub times: Vec<f64>,
    pub binding_indices: Vec<u32>,
    pub values: Vec<f64>,
    /// Binding offset for the ordinary streamed curves that follow.
    pub following_curve_offset: u32,
}

/// One node of a trusted managed object schema.
///
/// Reconstructed by an offline tool; nothing here executes asset-controlled
/// code. `align` sets the four-byte alignment flag Unity's own trees carry.
#[napi(object)]
pub struct SchemaNode {
    pub type_name: String,
    pub field_name: String,
    /// Nesting depth, zero for the root.
    pub level: u32,
    pub align: bool,
}

/// A complete object schema for one managed script type.
#[napi(object)]
pub struct MonoBehaviourSchema {
    pub assembly_name: String,
    pub class_name: String,
    pub namespace: Option<String>,
    /// Exact Unity version this schema was generated for. Omit for a schema
    /// that applies to every version.
    pub unity_version: Option<String>,
    pub nodes: Vec<SchemaNode>,
}

/// The name the bindings report for a schema source.
const fn schema_source_name(source: MonoBehaviourSchemaSource) -> &'static str {
    match source {
        MonoBehaviourSchemaSource::Embedded => "embedded",
        MonoBehaviourSchemaSource::External => "schema",
    }
}

/// A `MonoBehaviour` read as JSON, and which tree it was read through.
#[napi(object)]
pub struct MonoBehaviourJson {
    pub json: Buffer,
    /// `"embedded"` when the file carried its own type tree, `"schema"` when
    /// the layout came from a supplied schema. Worth distinguishing: a value
    /// read through a schema is only as good as that schema.
    pub source: String,
}

/// How an input is opened.
///
/// Every field is optional and omitting one keeps Core's default. This exists
/// because the alternatives do not combine: a separate factory per option
/// cannot open a UnityCN-encrypted bundle whose header version was also
/// stripped, which is an ordinary pair of facts about one file.
#[napi(object)]
pub struct OpenOptions {
    /// Parse against this version instead of the one the files declare, for
    /// files whose own version was stripped at build time.
    pub unity_version: Option<String>,
    /// The 16-byte UnityCN key for an encrypted archive.
    ///
    /// Caller-supplied only: this project ships no key material, and none is
    /// recovered from anything. The key is never printed, including in error
    /// text.
    pub unity_cn_key: Option<Buffer>,
    /// Keep the inputs that parsed instead of refusing the whole load over one
    /// that did not. A game directory routinely mixes readable assets with
    /// encrypted or not-yet-supported containers.
    pub skip_unreadable_inputs: Option<bool>,
    /// Reject classes whose standard-Unity version is above the verified
    /// ceiling instead of attempting the newest known layout (the default).
    pub strict_unity_versions: Option<bool>,
    pub maximum_input_files: Option<u32>,
    pub maximum_input_directories: Option<u32>,
    pub maximum_directory_entries: Option<u32>,
    /// Maximum UTF-8 bytes in one root label or fully qualified nested path.
    pub maximum_path_bytes: Option<u32>,
    /// Maximum cumulative UTF-8 bytes of paths discovered during one load.
    pub maximum_total_path_bytes: Option<u32>,
    /// Maximum cumulative UTF-8 bytes retained by skipped-input diagnostics.
    pub maximum_diagnostic_bytes: Option<i64>,
}

/// Complete caller-configurable Core export policy.
///
/// Every field is optional and preserves Core's default when omitted. The
/// older `export(outputRoot, overwrite?)` method remains available as the
/// compact compatibility call.
#[napi(object)]
pub struct ExportConfiguration {
    /// `auto`, `raw`, `typetree-json`, or `dump-text`.
    pub mode: Option<String>,
    /// `asset-name`, `asset-name-path-id`, or `path-id`.
    pub filename_format: Option<String>,
    /// `jpeg`, `png`, `bmp`, `tga`, `webp`, or `raw-rgba`.
    pub image_format: Option<String>,
    pub jpeg_quality: Option<u32>,
    /// `auto`, `raw`, or `wav`.
    pub audio_format: Option<String>,
    pub overwrite_existing: Option<bool>,
    pub restore_text_asset_extension: Option<bool>,
    pub pretty_json: Option<bool>,
    pub maximum_objects: Option<u32>,
    pub maximum_total_output_bytes: Option<i64>,
    pub maximum_metadata_bytes: Option<i64>,
    pub maximum_raw_object_bytes: Option<i64>,
    pub maximum_type_tree_json_bytes: Option<i64>,
    pub maximum_type_tree_dump_bytes: Option<i64>,
    pub maximum_text_asset_bytes: Option<i64>,
    pub maximum_simple_asset_bytes: Option<i64>,
    pub maximum_audio_output_bytes: Option<i64>,
    pub maximum_texture_output_bytes: Option<i64>,
    pub maximum_texture_array_output_bytes: Option<i64>,
    pub maximum_texture_array_bundle_bytes: Option<i64>,
    pub maximum_sprite_output_bytes: Option<i64>,
    pub maximum_shader_output_bytes: Option<i64>,
    pub maximum_monobehaviour_json_bytes: Option<i64>,
    pub maximum_mesh_object_bytes: Option<i64>,
    pub maximum_mesh_output_bytes: Option<i64>,
}

/// Caller-configurable collection-wide scene assembly budgets.
#[napi(object)]
pub struct SceneLimits {
    pub maximum_game_objects: Option<u32>,
    pub maximum_total_components: Option<u32>,
    pub maximum_total_transform_child_references: Option<u32>,
    pub maximum_total_material_references: Option<u32>,
    pub maximum_total_bone_references: Option<u32>,
    pub maximum_hierarchy_edges: Option<u32>,
    pub maximum_index_bytes: Option<u32>,
}

/// Caller-configurable budgets for textures returned beside a model.
#[napi(object)]
pub struct ModelTextureLimits {
    pub maximum_texture_references: Option<u32>,
    pub maximum_textures: Option<u32>,
    pub maximum_name_index_bytes: Option<i64>,
    pub maximum_metadata_bytes: Option<i64>,
    pub maximum_total_encoded_bytes: Option<i64>,
    pub maximum_single_texture_bytes: Option<i64>,
}

fn load_options(
    options: Option<OpenOptions>,
    oodle: Option<Arc<dyn unity_rs_core::bundle::OodleDecoder>>,
) -> Result<AssetLoadOptions> {
    let Some(options) = options else {
        return Ok(AssetLoadOptions {
            oodle_decoder: oodle,
            ..AssetLoadOptions::default()
        });
    };
    let unity_version_override = match options.unity_version {
        None => None,
        Some(text) => Some(
            text.parse::<UnityVersion>()
                .map_err(|_| invalid_arg(format!("unsupported Unity version: {text}")))?,
        ),
    };
    let unity_cn_key = match options.unity_cn_key {
        None => None,
        Some(buffer) => {
            let key: [u8; 16] = buffer.as_ref().try_into().map_err(|_| {
                // The length is the caller's own input, not key material.
                invalid_arg(format!(
                    "unityCnKey must be exactly 16 bytes; got {}",
                    buffer.len()
                ))
            })?;
            Some(UnityCnKey::new(key))
        }
    };
    let defaults = AssetLoadLimits::default();
    Ok(AssetLoadOptions {
        limits: AssetLoadLimits {
            maximum_input_files: count_limit(
                options.maximum_input_files,
                defaults.maximum_input_files,
            ),
            maximum_input_directories: count_limit(
                options.maximum_input_directories,
                defaults.maximum_input_directories,
            ),
            maximum_directory_entries: count_limit(
                options.maximum_directory_entries,
                defaults.maximum_directory_entries,
            ),
            maximum_path_bytes: count_limit(
                options.maximum_path_bytes,
                defaults.maximum_path_bytes,
            ),
            maximum_total_path_bytes: count_limit(
                options.maximum_total_path_bytes,
                defaults.maximum_total_path_bytes,
            ),
            maximum_diagnostic_bytes: usize_non_negative_limit(
                options.maximum_diagnostic_bytes,
                defaults.maximum_diagnostic_bytes,
                "maximumDiagnosticBytes",
            )?,
            ..defaults
        },
        unity_version_override,
        oodle_decoder: oodle,
        unity_cn_key,
        failure_policy: if options.skip_unreadable_inputs.unwrap_or(false) {
            LoadFailurePolicy::SkipInput
        } else {
            LoadFailurePolicy::Abort
        },
        strict_unity_versions: options.strict_unity_versions.unwrap_or(false),
    })
}

fn count_limit(value: Option<u32>, default: usize) -> usize {
    value.map_or(default, |value| value as usize)
}

fn export_configuration(options: Option<ExportConfiguration>) -> Result<CoreExportOptions> {
    let defaults = CoreExportOptions::default();
    let Some(options) = options else {
        return Ok(defaults);
    };
    let jpeg_quality = options
        .jpeg_quality
        .unwrap_or(u32::from(defaults.jpeg_quality));
    if !(1..=100).contains(&jpeg_quality) {
        return Err(invalid_arg(format!(
            "jpegQuality {jpeg_quality} is outside the supported range 1 through 100"
        )));
    }
    let jpeg_quality = u8::try_from(jpeg_quality)
        .map_err(|_| invalid_arg("jpegQuality does not fit in one byte"))?;
    let audio_format = match options.audio_format.as_deref() {
        Some(value) => parse_audio_format(value)?,
        None => defaults.audio_format,
    };
    let mut configured = defaults;
    apply_export_limits(&options, &mut configured)?;
    configured.mode = parse_export_mode(options.mode.as_deref())?;
    configured.filename_format = parse_filename_format(options.filename_format.as_deref())?;
    configured.image_format = parse_image_format(options.image_format)?;
    configured.jpeg_quality = jpeg_quality;
    configured.audio_format = audio_format;
    configured.overwrite_existing = options
        .overwrite_existing
        .unwrap_or(defaults.overwrite_existing);
    configured.restore_text_asset_extension = options
        .restore_text_asset_extension
        .unwrap_or(defaults.restore_text_asset_extension);
    configured.pretty_json = options.pretty_json.unwrap_or(defaults.pretty_json);
    Ok(configured)
}

fn apply_export_limits(
    options: &ExportConfiguration,
    configured: &mut CoreExportOptions,
) -> Result<()> {
    configured.maximum_objects = count_limit(options.maximum_objects, configured.maximum_objects);
    configured.maximum_total_output_bytes = non_negative_limit(
        options.maximum_total_output_bytes,
        configured.maximum_total_output_bytes,
        "maximumTotalOutputBytes",
    )?;
    configured.maximum_metadata_bytes = non_negative_limit(
        options.maximum_metadata_bytes,
        configured.maximum_metadata_bytes,
        "maximumMetadataBytes",
    )?;
    configured.maximum_raw_object_bytes = non_negative_limit(
        options.maximum_raw_object_bytes,
        configured.maximum_raw_object_bytes,
        "maximumRawObjectBytes",
    )?;
    configured.maximum_type_tree_json_bytes = non_negative_limit(
        options.maximum_type_tree_json_bytes,
        configured.maximum_type_tree_json_bytes,
        "maximumTypeTreeJsonBytes",
    )?;
    configured.maximum_type_tree_dump_bytes = non_negative_limit(
        options.maximum_type_tree_dump_bytes,
        configured.maximum_type_tree_dump_bytes,
        "maximumTypeTreeDumpBytes",
    )?;
    configured.maximum_text_asset_bytes = usize_non_negative_limit(
        options.maximum_text_asset_bytes,
        configured.maximum_text_asset_bytes,
        "maximumTextAssetBytes",
    )?;
    configured.maximum_simple_asset_bytes = non_negative_limit(
        options.maximum_simple_asset_bytes,
        configured.maximum_simple_asset_bytes,
        "maximumSimpleAssetBytes",
    )?;
    configured.maximum_audio_output_bytes = non_negative_limit(
        options.maximum_audio_output_bytes,
        configured.maximum_audio_output_bytes,
        "maximumAudioOutputBytes",
    )?;
    configured.maximum_texture_output_bytes = non_negative_limit(
        options.maximum_texture_output_bytes,
        configured.maximum_texture_output_bytes,
        "maximumTextureOutputBytes",
    )?;
    configured.maximum_texture_array_output_bytes = non_negative_limit(
        options.maximum_texture_array_output_bytes,
        configured.maximum_texture_array_output_bytes,
        "maximumTextureArrayOutputBytes",
    )?;
    configured.maximum_texture_array_bundle_bytes = non_negative_limit(
        options.maximum_texture_array_bundle_bytes,
        configured.maximum_texture_array_bundle_bytes,
        "maximumTextureArrayBundleBytes",
    )?;
    configured.maximum_sprite_output_bytes = non_negative_limit(
        options.maximum_sprite_output_bytes,
        configured.maximum_sprite_output_bytes,
        "maximumSpriteOutputBytes",
    )?;
    configured.maximum_shader_output_bytes = non_negative_limit(
        options.maximum_shader_output_bytes,
        configured.maximum_shader_output_bytes,
        "maximumShaderOutputBytes",
    )?;
    configured.maximum_monobehaviour_json_bytes = usize_non_negative_limit(
        options.maximum_monobehaviour_json_bytes,
        configured.maximum_monobehaviour_json_bytes,
        "maximumMonobehaviourJsonBytes",
    )?;
    configured.maximum_mesh_object_bytes = non_negative_limit(
        options.maximum_mesh_object_bytes,
        configured.maximum_mesh_object_bytes,
        "maximumMeshObjectBytes",
    )?;
    configured.maximum_mesh_output_bytes = non_negative_limit(
        options.maximum_mesh_output_bytes,
        configured.maximum_mesh_output_bytes,
        "maximumMeshOutputBytes",
    )?;
    Ok(())
}

fn export_report(report: CoreExportReport) -> Result<ExportReport> {
    let mut exported = reserve(report.exported.len(), "export records")?;
    for record in report.exported {
        exported.push(ExportRecord {
            source: record.source,
            path_id: BigInt::from(record.path_id),
            class_id: record.class_id,
            output_path: copy_path_string(&record.output_path, "exported output path")?,
            payload_kind: record.payload_kind.to_owned(),
        });
    }
    let mut failures = reserve(report.failures.len(), "export failures")?;
    for failure in report.failures {
        failures.push(ExportFailure {
            source: failure.source,
            path_id: BigInt::from(failure.path_id),
            class_id: failure.class_id,
            error: failure.error,
        });
    }
    let mut unsupported = reserve(report.unsupported.len(), "unsupported exports")?;
    for declined in report.unsupported {
        unsupported.push(ExportFailure {
            source: declined.source,
            path_id: BigInt::from(declined.path_id),
            class_id: declined.class_id,
            error: declined.error,
        });
    }
    Ok(ExportReport {
        exported,
        failures,
        unsupported,
    })
}

fn scene_limits(options: Option<SceneLimits>) -> SceneHierarchyLimits {
    let defaults = SceneHierarchyLimits::default();
    let Some(options) = options else {
        return defaults;
    };
    SceneHierarchyLimits {
        maximum_game_objects: count_limit(
            options.maximum_game_objects,
            defaults.maximum_game_objects,
        ),
        maximum_total_components: count_limit(
            options.maximum_total_components,
            defaults.maximum_total_components,
        ),
        maximum_total_transform_child_references: count_limit(
            options.maximum_total_transform_child_references,
            defaults.maximum_total_transform_child_references,
        ),
        maximum_total_material_references: count_limit(
            options.maximum_total_material_references,
            defaults.maximum_total_material_references,
        ),
        maximum_total_bone_references: count_limit(
            options.maximum_total_bone_references,
            defaults.maximum_total_bone_references,
        ),
        maximum_hierarchy_edges: count_limit(
            options.maximum_hierarchy_edges,
            defaults.maximum_hierarchy_edges,
        ),
        maximum_index_bytes: count_limit(options.maximum_index_bytes, defaults.maximum_index_bytes),
        ..defaults
    }
}

fn model_texture_limits(options: Option<ModelTextureLimits>) -> Result<SceneTextureLimits> {
    let defaults = SceneTextureLimits::default();
    let Some(options) = options else {
        return Ok(defaults);
    };
    let maximum_single_texture_bytes = non_negative_limit(
        options.maximum_single_texture_bytes,
        defaults.texture.maximum_output_bytes,
        "maximumSingleTextureBytes",
    )?;
    Ok(SceneTextureLimits {
        maximum_texture_references: count_limit(
            options.maximum_texture_references,
            defaults.maximum_texture_references,
        ),
        maximum_textures: count_limit(options.maximum_textures, defaults.maximum_textures),
        maximum_name_index_bytes: non_negative_limit(
            options.maximum_name_index_bytes,
            defaults.maximum_name_index_bytes,
            "maximumNameIndexBytes",
        )?,
        maximum_metadata_bytes: non_negative_limit(
            options.maximum_metadata_bytes,
            defaults.maximum_metadata_bytes,
            "maximumMetadataBytes",
        )?,
        maximum_total_encoded_bytes: non_negative_limit(
            options.maximum_total_encoded_bytes,
            defaults.maximum_total_encoded_bytes,
            "maximumTotalEncodedBytes",
        )?,
        texture: TextureReadLimits {
            maximum_payload_bytes: maximum_single_texture_bytes,
            maximum_output_bytes: maximum_single_texture_bytes,
            maximum_decoder_working_bytes: maximum_single_texture_bytes,
            ..defaults.texture
        },
    })
}

/// One opened collection. All format work is delegated to `unity-rs-core`.
#[napi]
pub struct UnityRs {
    studio: Arc<Studio>,
}

#[napi]
impl UnityRs {
    #[napi(constructor)]
    pub fn new(path: String) -> Result<Self> {
        Studio::open(path)
            .map(|studio| Self {
                studio: Arc::new(studio),
            })
            .map_err(core_error)
    }

    /// Opens a path, parsing against `unityVersion` instead of the version the
    /// files declare.
    ///
    /// Needed for files whose own version was stripped at build time, where a
    /// reader has nothing to key its layout decisions on.
    // napi marshals a JavaScript string by value; a reference does not expand.
    #[allow(clippy::needless_pass_by_value)]
    #[napi(factory)]
    pub fn open_with_version(path: String, unity_version: String) -> Result<Self> {
        let version: UnityVersion = unity_version
            .parse()
            .map_err(|_| invalid_arg(format!("unsupported Unity version: {unity_version}")))?;
        Studio::open_with_options(
            path,
            AssetLoadOptions {
                unity_version_override: Some(version),
                ..AssetLoadOptions::default()
            },
        )
        .map(|studio| Self {
            studio: Arc::new(studio),
        })
        .map_err(core_error)
    }

    /// Writes the animated FBX, decoding ACL tracks through a caller-supplied
    /// decoder.
    ///
    /// Core ships no ACL decoder. Without one a clip whose samples are ACL
    /// compressed contributes nothing to the FBX; with one its tracks are
    /// validated on the way back in -- shape, ordering and budgets are Core's
    /// checks, not the caller's promises.
    ///
    /// Asynchronous for the same reason as the Oodle entry point: the callback
    /// runs on the event loop while a worker waits for it.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_fbx_with_acl_decoder(
        &self,
        #[napi(ts_arg_type = "(request: AclDecodeRequest) => AclDecodedClip")] decoder: AclCallback,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<FbxWithAclTask>> {
        Ok(AsyncTask::new(FbxWithAclTask {
            studio: Arc::clone(&self.studio),
            maximum: byte_limit(maximum_bytes)?,
            decoder: Arc::new(JsAclDecoder {
                callback: decoder,
                limits: AclDecodeLimits::default(),
            }),
            request: FbxWithAclRequest::SceneAscii,
        }))
    }

    /// Writes the animated scene as binary FBX 7.4, decoding ACL tracks
    /// through a caller-supplied decoder on a worker.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_fbx_binary_with_acl_decoder(
        &self,
        #[napi(ts_arg_type = "(request: AclDecodeRequest) => AclDecodedClip")] decoder: AclCallback,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<FbxWithAclTask>> {
        Ok(AsyncTask::new(FbxWithAclTask {
            studio: Arc::clone(&self.studio),
            maximum: byte_limit(maximum_bytes)?,
            decoder: Arc::new(JsAclDecoder {
                callback: decoder,
                limits: AclDecodeLimits::default(),
            }),
            request: FbxWithAclRequest::SceneBinary,
        }))
    }

    /// Opens a path with any combination of the load options.
    ///
    /// The single-option factories remain for the cases they already cover;
    /// this is the one that can express two facts about the same file.
    #[napi(factory)]
    pub fn open_with(path: String, options: Option<OpenOptions>) -> Result<Self> {
        Studio::open_with_options(path, load_options(options, None)?)
            .map(|studio| Self {
                studio: Arc::new(studio),
            })
            .map_err(core_error)
    }

    /// Opens a path whose bundles are Oodle-compressed, using a
    /// caller-supplied decoder.
    ///
    /// Asynchronous by necessity, not for convenience: the decoder runs on the
    /// JavaScript event loop while a worker waits for it, so calling this
    /// synchronously would block the loop that has to run the callback.
    ///
    /// The callback receives the compressed bytes and the exact expected output
    /// length, and must return precisely that many bytes.
    #[must_use]
    #[napi(ts_return_type = "Promise<UnityRs>")]
    pub fn open_with_oodle(
        path: String,
        #[napi(ts_arg_type = "(input: Buffer, expectedLength: number) => Buffer")]
        decoder: OodleCallback,
        options: Option<OpenOptions>,
    ) -> AsyncTask<OpenWithOodleTask> {
        AsyncTask::new(OpenWithOodleTask {
            path,
            decoder: Arc::new(JsOodleDecoder { callback: decoder }),
            options,
        })
    }

    /// Opens a path on a libuv worker so container discovery does not block
    /// the JavaScript event loop. The optional settings are identical to
    /// `openWith`; existing one-argument calls remain valid.
    #[must_use]
    #[napi(ts_return_type = "Promise<UnityRs>")]
    pub fn open_async(path: String, options: Option<OpenOptions>) -> AsyncTask<OpenPathTask> {
        AsyncTask::new(OpenPathTask { path, options })
    }

    /// Opens one in-memory asset, bundle, or resource after copying the Node
    /// buffer into Rust-owned immutable storage.
    ///
    /// The final options argument exposes the same version override, UnityCN
    /// key, failure policy, and discovery budgets as `openWith`. It is last so
    /// existing `(data, name, maximumBytes)` calls remain source-compatible.
    #[napi(factory)]
    pub fn from_buffer(
        data: &[u8],
        name: Option<String>,
        maximum_bytes: Option<i64>,
        options: Option<OpenOptions>,
    ) -> Result<Self> {
        let maximum = byte_limit(maximum_bytes)?;
        let actual =
            u64::try_from(data.len()).map_err(|_| invalid_arg("buffer length does not fit u64"))?;
        if actual > maximum {
            return Err(invalid_arg(format!(
                "input buffer is {actual} bytes, exceeding limit {maximum}"
            )));
        }
        let bytes = copy_slice(data, "input buffer")?;
        Studio::open_region_with_options(
            name.unwrap_or_else(|| "memory.assets".to_owned()),
            Region::from_bytes(bytes),
            load_options(options, None)?,
        )
        .map(|studio| Self {
            studio: Arc::new(studio),
        })
        .map_err(core_error)
    }

    /// Copies a Node buffer once, then parses it on a libuv worker. The final
    /// options argument matches the synchronous `fromBuffer` entry point.
    #[napi(ts_return_type = "Promise<UnityRs>")]
    pub fn from_buffer_async(
        data: &[u8],
        name: Option<String>,
        maximum_bytes: Option<i64>,
        options: Option<OpenOptions>,
    ) -> Result<AsyncTask<OpenBufferTask>> {
        let maximum = byte_limit(maximum_bytes)?;
        let actual =
            u64::try_from(data.len()).map_err(|_| invalid_arg("buffer length does not fit u64"))?;
        if actual > maximum {
            return Err(invalid_arg(format!(
                "input buffer is {actual} bytes, exceeding limit {maximum}"
            )));
        }
        let bytes = copy_slice(data, "input buffer")?;
        Ok(AsyncTask::new(OpenBufferTask {
            bytes,
            name: name.unwrap_or_else(|| "memory.assets".to_owned()),
            options,
        }))
    }

    #[napi(getter)]
    pub fn file_count(&self) -> Result<u32> {
        count_u32(self.studio.file_count(), "file count")
    }

    #[napi(getter)]
    pub fn object_count(&self) -> Result<u32> {
        count_u32(self.studio.object_count(), "object count")
    }

    #[napi(getter)]
    pub fn resource_count(&self) -> Result<u32> {
        count_u32(self.studio.resource_count(), "resource count")
    }

    #[napi(getter)]
    pub fn load_diagnostic_count(&self) -> Result<u32> {
        count_u32(
            self.studio.load_diagnostics().len(),
            "load diagnostic count",
        )
    }

    #[napi]
    pub fn load_diagnostic_page(
        &self,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Vec<LoadDiagnosticInfo>> {
        let (offset, limit) = page(offset, limit)?;
        let diagnostics = self.studio.load_diagnostics();
        let available = diagnostics.len().saturating_sub(offset);
        let count = available.min(limit);
        let mut output = reserve(count, "load diagnostic page")?;
        for diagnostic in diagnostics.iter().skip(offset).take(count) {
            output.push(LoadDiagnosticInfo {
                path: copy_string(&diagnostic.path, "load diagnostic path")?,
                message: copy_string(&diagnostic.message, "load diagnostic message")?,
            });
        }
        Ok(output)
    }

    #[napi]
    pub fn file_page(&self, offset: Option<u32>, limit: Option<u32>) -> Result<Vec<FileInfo>> {
        let (offset, limit) = page(offset, limit)?;
        let available = self.studio.file_count().saturating_sub(offset);
        let count = available.min(limit);
        let mut output = reserve(count, "file metadata page")?;
        for file in self.studio.files().skip(offset).take(count) {
            output.push(FileInfo {
                index: count_u32(file.index(), "file index")?,
                path: copy_string(file.path(), "file path")?,
                unity_version: copy_string(file.unity_version(), "Unity version")?,
                object_count: count_u32(file.object_count(), "file object count")?,
            });
        }
        Ok(output)
    }

    #[napi]
    pub fn object_page(
        &self,
        file_index: u32,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Vec<ObjectInfo>> {
        let file_index = usize::try_from(file_index).expect("u32 fits usize");
        let file = self
            .studio
            .file(file_index)
            .ok_or_else(|| invalid_arg(format!("file index {file_index} was not found")))?;
        let (offset, limit) = page(offset, limit)?;
        let available = file.object_count().saturating_sub(offset);
        let count = available.min(limit);
        let mut output = reserve(count, "object metadata page")?;
        for object_index in offset..offset + count {
            let object = self
                .studio
                .object_by_index(file_index, object_index)
                .ok_or_else(|| Error::from_reason("validated object index vanished"))?;
            output.push(object_info(object)?);
        }
        Ok(output)
    }

    #[napi]
    pub fn resource_page(
        &self,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Vec<ResourceInfo>> {
        let (offset, limit) = page(offset, limit)?;
        let available = self.studio.resource_count().saturating_sub(offset);
        let count = available.min(limit);
        let mut output = reserve(count, "resource metadata page")?;
        for resource in self.studio.resources().skip(offset).take(count) {
            output.push(ResourceInfo {
                index: count_u32(resource.index(), "resource index")?,
                path: copy_string(resource.path(), "resource path")?,
                byte_size: resource.byte_size().into(),
            });
        }
        Ok(output)
    }

    #[napi]
    pub fn read_resource(&self, resource_index: u32, maximum_bytes: Option<i64>) -> Result<Buffer> {
        let index = usize::try_from(resource_index).expect("u32 fits usize");
        self.studio
            .resource(index)
            .ok_or_else(|| invalid_arg(format!("resource index {index} was not found")))?
            .read(byte_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi]
    pub fn read_raw(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_raw(byte_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_raw_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(file_index, path_id, maximum_bytes, ByteReadKind::Raw)
    }

    #[napi]
    pub fn read_text(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        let maximum = usize_limit(maximum_bytes)?;
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_text_bytes(maximum)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_text_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(file_index, path_id, maximum_bytes, ByteReadKind::Text)
    }

    #[napi]
    pub fn read_type_tree_json(
        &self,
        file_index: u32,
        path_id: BigInt,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_type_tree_json(pretty.unwrap_or(false), usize_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_type_tree_json_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(
            file_index,
            path_id,
            maximum_bytes,
            ByteReadKind::TypeTreeJson {
                pretty: pretty.unwrap_or(false),
            },
        )
    }

    #[napi]
    pub fn read_type_tree_dump(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_type_tree_dump(byte_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_type_tree_dump_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(
            file_index,
            path_id,
            maximum_bytes,
            ByteReadKind::TypeTreeDump,
        )
    }

    #[napi]
    pub fn read_shader(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_shader_text(byte_limit(maximum_bytes)?)
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_shader_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(file_index, path_id, maximum_bytes, ByteReadKind::Shader)
    }

    #[napi]
    pub fn read_mesh_obj(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        let maximum = byte_limit(maximum_bytes)?;
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_mesh_obj(MeshReadLimits {
                maximum_object_bytes: maximum,
                maximum_output_bytes: maximum,
                ..MeshReadLimits::default()
            })
            .map(Buffer::from)
            .map_err(core_error)
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_mesh_obj_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        self.read_bytes_task(file_index, path_id, maximum_bytes, ByteReadKind::MeshObj)
    }

    #[napi]
    pub fn read_texture(
        &self,
        file_index: u32,
        path_id: BigInt,
        mip_level: Option<u32>,
        maximum_bytes: Option<i64>,
    ) -> Result<RgbaImage> {
        let maximum = byte_limit(maximum_bytes)?;
        let image = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .decode_texture_mip(
                mip_level.unwrap_or(0),
                TextureReadLimits {
                    maximum_payload_bytes: maximum,
                    maximum_output_bytes: maximum,
                    maximum_decoder_working_bytes: maximum,
                    ..TextureReadLimits::default()
                },
            )
            .map_err(core_error)?;
        convert_decoded_image(image)
    }

    #[napi(ts_return_type = "Promise<RgbaImage>")]
    pub fn read_texture_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        mip_level: Option<u32>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadTextureTask>> {
        Ok(AsyncTask::new(ReadTextureTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            mip_level: mip_level.unwrap_or(0),
            maximum: byte_limit(maximum_bytes)?,
        }))
    }

    #[napi]
    pub fn read_texture_array(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Vec<RgbaImage>> {
        let maximum = byte_limit(maximum_bytes)?;
        let images = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .decode_texture_array_mip0(texture_array_limits(maximum))
            .map_err(core_error)?;
        convert_decoded_images(images)
    }

    #[napi(ts_return_type = "Promise<Array<RgbaImage>>")]
    pub fn read_texture_array_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadTextureArrayTask>> {
        Ok(AsyncTask::new(ReadTextureArrayTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            maximum: byte_limit(maximum_bytes)?,
        }))
    }

    /// Reads one complete, bounded Unity `SpriteAtlas` metadata table.
    #[napi]
    pub fn read_sprite_atlas(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_entries: Option<u32>,
        maximum_string_bytes: Option<i64>,
        maximum_total_string_bytes: Option<i64>,
    ) -> Result<SpriteAtlasInfo> {
        let atlas = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_sprite_atlas(sprite_atlas_limits(
                maximum_entries,
                maximum_string_bytes,
                maximum_total_string_bytes,
            )?)
            .map_err(core_error)?;
        convert_sprite_atlas(atlas)
    }

    /// Reads one complete, bounded Unity `Sprite` metadata object without
    /// resolving or decoding its texture references.
    #[napi]
    pub fn read_sprite_metadata(
        &self,
        file_index: u32,
        path_id: BigInt,
        limits: Option<SpriteMetadataLimits>,
    ) -> Result<SpriteMetadata> {
        let sprite = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_sprite(sprite_metadata_limits(limits)?)
            .map_err(core_error)?;
        convert_sprite_metadata(sprite)
    }

    #[napi]
    pub fn read_sprite(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<RgbaImage> {
        let maximum = byte_limit(maximum_bytes)?;
        let image = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .decode_sprite(sprite_limits(maximum), texture_limits(maximum))
            .map_err(core_error)?;
        Ok(convert_image(image))
    }

    #[napi(ts_return_type = "Promise<RgbaImage>")]
    pub fn read_sprite_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadSpriteTask>> {
        Ok(AsyncTask::new(ReadSpriteTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            maximum: byte_limit(maximum_bytes)?,
        }))
    }

    /// Encodes one decoded RGBA image into a complete file payload using the
    /// same bounded Core encoders as `exportWithOptions`: `png` (the
    /// default), `jpeg`, `bmp`, `tga`, `webp`, or `raw-rgba`.
    ///
    /// The pixels must be display-order rows exactly as `readTexture`,
    /// `readTextureArray`, and `readSprite` return them.
    // napi marshals JavaScript objects by value; references do not expand.
    #[allow(clippy::needless_pass_by_value)]
    #[napi]
    pub fn encode_image(image: RgbaImage, options: Option<EncodeImageOptions>) -> Result<Buffer> {
        let plan = encode_image_plan(&image, options)?;
        plan.encode().map(Into::into)
    }

    /// The worker-thread variant of `encodeImage` for pipelines that keep
    /// pixel-proportional encoding off the event loop.
    // napi marshals JavaScript objects by value; references do not expand.
    #[allow(clippy::needless_pass_by_value)]
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn encode_image_async(
        image: RgbaImage,
        options: Option<EncodeImageOptions>,
    ) -> Result<AsyncTask<EncodeImageTask>> {
        Ok(AsyncTask::new(EncodeImageTask {
            plan: encode_image_plan(&image, options)?,
        }))
    }

    /// Reads an `AudioClip`'s stored payload without transcoding it.
    #[napi]
    pub fn read_audio(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AudioClip> {
        let maximum = byte_limit(maximum_bytes)?;
        let audio = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_audio_clip(SimpleAssetReadLimits {
                maximum_payload_bytes: maximum,
                ..SimpleAssetReadLimits::default()
            })
            .map_err(core_error)?;
        materialize_audio_clip(audio, AudioExportFormat::Raw, maximum)
    }

    /// Reads an `AudioClip` using the same `auto`, `raw`, or `wav` policy as
    /// Core, Python and the CLI exporter.
    ///
    /// `auto` writes a WAV only when Core has verified a decoder-free path;
    /// otherwise it preserves the source container. `wav` requires such a
    /// path and refuses compressed codecs rather than returning mislabeled
    /// bytes. The older `readAudio` method remains a raw-only compatibility
    /// alias.
    #[napi]
    pub fn read_audio_clip(
        &self,
        file_index: u32,
        path_id: BigInt,
        format: Option<String>,
        maximum_bytes: Option<i64>,
    ) -> Result<AudioClip> {
        let maximum = byte_limit(maximum_bytes)?;
        let format = match format {
            Some(value) => parse_audio_format(&value)?,
            None => AudioExportFormat::Auto,
        };
        let audio = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_audio_clip(SimpleAssetReadLimits {
                maximum_payload_bytes: maximum,
                ..SimpleAssetReadLimits::default()
            })
            .map_err(core_error)?;
        materialize_audio_clip(audio, format, maximum)
    }

    /// Reads the identity of a `MonoScript`.
    #[napi]
    pub fn read_mono_script(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<MonoScript> {
        let maximum = byte_limit(maximum_bytes)?;
        let script = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_mono_script(MonoBehaviourReadLimits {
                maximum_string_bytes: usize::try_from(maximum).unwrap_or(usize::MAX),
                ..MonoBehaviourReadLimits::default()
            })
            .map_err(core_error)?;
        Ok(MonoScript {
            name: script.name,
            class_name: script.class_name,
            namespace: script.namespace,
            assembly_name: script.assembly_name,
            execution_order: script.execution_order,
        })
    }

    /// Reads a `Material`'s shader reference and the names of its properties.
    ///
    /// Property values are deliberately not flattened here: they are typed
    /// per sheet and a caller that needs them is better served by the Rust or
    /// Python API than by a lossy JavaScript projection.
    #[napi]
    pub fn read_material(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Material> {
        let maximum = byte_limit(maximum_bytes)?;
        let material = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_material(MaterialReadLimits {
                maximum_object_bytes: maximum,
                ..MaterialReadLimits::default()
            })
            .map_err(core_error)?;
        Ok(Material {
            name: material.name,
            shader_file_id: material.shader.file_id,
            shader_path_id: BigInt::from(material.shader.path_id),
            texture_properties: property_names(&material.saved_properties.texture_environments)?,
            float_properties: property_names(&material.saved_properties.floats)?,
            color_properties: property_names(&material.saved_properties.colors)?,
        })
    }

    /// Reads the scene list a `BuildSettings` object records.
    #[napi]
    pub fn read_build_settings(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<BuildSettings> {
        let settings = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_build_settings(settings_limits(byte_limit(maximum_bytes)?))
            .map_err(core_error)?;
        Ok(BuildSettings {
            levels: settings.levels,
            scenes: settings.scenes,
        })
    }

    /// Reads the company and product names from a `PlayerSettings` object.
    #[napi]
    pub fn read_player_settings(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<PlayerSettings> {
        let settings = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_player_settings(settings_limits(byte_limit(maximum_bytes)?))
            .map_err(core_error)?;
        Ok(PlayerSettings {
            company_name: settings.company_name,
            product_name: settings.product_name,
        })
    }

    /// Reads complete stable skeleton, TOS, and human-description metadata
    /// from one bounded `Avatar`.
    #[napi]
    pub fn read_avatar(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Avatar> {
        let maximum = byte_limit(maximum_bytes)?;
        let avatar = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_avatar(avatar_limits(maximum)?)
            .map_err(core_error)?;
        convert_avatar(avatar)
    }

    /// Opens several in-memory inputs as one collection.
    ///
    /// A serialized file and the `.resS` its textures and audio stream from are
    /// separate files; opening them one at a time leaves every streamed payload
    /// unresolvable, so a caller holding both in memory needs to pass them
    /// together. The final options argument matches `openWith` and is placed
    /// after the existing aggregate byte limit to preserve old calls.
    #[napi(factory)]
    pub fn from_buffers(
        env: Env,
        #[napi(ts_arg_type = "Array<MemoryInput>")] inputs: Array<'_>,
        maximum_bytes: Option<i64>,
        options: Option<OpenOptions>,
    ) -> Result<Self> {
        let maximum = byte_limit(maximum_bytes)?;
        let options = load_options(options, None)?;
        let input_count = usize::try_from(inputs.len()).expect("u32 fits usize");
        let maximum_files = options.limits.maximum_input_files;
        if input_count > maximum_files {
            return Err(invalid_arg(format!(
                "memory input has {input_count} files, exceeding limit {maximum_files}"
            )));
        }
        let mut total = 0_u64;
        let mut total_name_bytes = 0_usize;
        let mut regions = reserve(input_count, "memory input regions")?;
        for index in 0..inputs.len() {
            let owner = format!("memory input {index}");
            let input: Object<'_> = inputs
                .get(index)?
                .ok_or_else(|| invalid_arg(format!("{owner} is missing")))?;
            let name: JsString<'_> = required_object_field(&input, "name", &owner)?;
            let name_length = name.utf8_len()?;
            if name_length > MAXIMUM_MEMORY_INPUT_NAME_BYTES {
                return Err(invalid_arg(format!(
                    "{owner} name has {name_length} bytes, exceeding limit {MAXIMUM_MEMORY_INPUT_NAME_BYTES}"
                )));
            }
            total_name_bytes = total_name_bytes
                .checked_add(name_length)
                .ok_or_else(|| invalid_arg("memory input name byte count overflowed"))?;
            if total_name_bytes > MAXIMUM_TOTAL_MEMORY_INPUT_NAME_BYTES {
                return Err(invalid_arg(format!(
                    "memory input names exceed {MAXIMUM_TOTAL_MEMORY_INPUT_NAME_BYTES} bytes"
                )));
            }
            let data: Buffer = required_object_field(&input, "data", &owner)?;
            let length = u64::try_from(data.len())
                .map_err(|_| invalid_arg("buffer length does not fit u64"))?;
            total = total
                .checked_add(length)
                .ok_or_else(|| invalid_arg("input buffer sizes overflowed"))?;
            if total > maximum {
                return Err(invalid_arg(format!(
                    "input buffers total {total} bytes, exceeding limit {maximum}"
                )));
            }
            let name = copy_js_string(env.raw(), name, name_length, "memory input name")?;
            let bytes = copy_slice(data.as_ref(), "memory input buffer")?;
            regions.push((name, Region::from_bytes(bytes)));
        }
        Studio::open_regions_with_options(regions, options)
            .map(|studio| Self {
                studio: Arc::new(studio),
            })
            .map_err(core_error)
    }

    /// Reads one checked byte range of a resource without materializing the
    /// rest, which is how a caller pulls a single texture out of a large
    /// `.resS`.
    #[napi]
    pub fn read_resource_range(
        &self,
        resource_index: u32,
        offset: BigInt,
        length: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        let index = usize::try_from(resource_index).expect("u32 fits usize");
        let resource = self.studio.resource(index).ok_or_else(|| {
            invalid_arg(format!("resource index {resource_index} is out of range"))
        })?;
        let offset = bigint_u64(offset, "offset")?;
        let length = bigint_u64(length, "length")?;
        resource
            .read_range(offset, length, byte_limit(maximum_bytes)?)
            .map(Into::into)
            .map_err(core_error)
    }

    /// Finds a resource by the path a serialized file references it through.
    // napi marshals a JavaScript string by value; a reference does not expand.
    #[allow(clippy::needless_pass_by_value)]
    #[must_use]
    #[napi]
    pub fn resource_index_by_path(&self, path: String) -> Option<u32> {
        self.studio
            .resource_by_path(&path)
            .and_then(|resource| u32::try_from(resource.index()).ok())
    }

    /// Assembles the `GameObject` hierarchy across every loaded file.
    #[napi]
    pub fn scene(&self, maximum_game_objects: Option<u32>) -> Result<Vec<SceneNode>> {
        let mut limits = SceneHierarchyLimits::default();
        if let Some(maximum) = maximum_game_objects {
            limits.maximum_game_objects = usize::try_from(maximum).expect("u32 fits usize");
        }
        build_scene(&self.studio, limits)
    }

    /// Assembles the same hierarchy with all collection-wide budgets exposed.
    ///
    /// `scene(maximumGameObjects)` remains available for compatibility; this
    /// method adds component, child, material, bone, and hierarchy-edge limits.
    #[napi]
    pub fn scene_with_limits(&self, limits: Option<SceneLimits>) -> Result<Vec<SceneNode>> {
        build_scene(&self.studio, scene_limits(limits))
    }

    /// Writes the whole collection as static ASCII FBX 7.4.
    ///
    /// Ordinary and skinned renderer geometry, direct and hash-recovered bones
    /// and static blend shapes. Animation and textures are separate concerns
    /// and are not included.
    #[napi]
    pub fn read_static_fbx(&self, maximum_bytes: Option<i64>) -> Result<Buffer> {
        self.studio
            .read_static_fbx(byte_limit(maximum_bytes)?)
            .map(Into::into)
            .map_err(core_error)
    }

    /// The same static scene in FBX 7.4's binary encoding.
    ///
    /// Some importers accept only the binary form, and it is smaller and faster
    /// to parse. The scene is identical to `readStaticFbx`.
    #[napi]
    pub fn read_static_fbx_binary(&self, maximum_bytes: Option<i64>) -> Result<Buffer> {
        self.studio
            .read_static_fbx_binary(byte_limit(maximum_bytes)?)
            .map(Into::into)
            .map_err(core_error)
    }

    /// Reads a `CubismPhysicsController` and writes its physics3.json.
    ///
    /// `motionFps` is the fallback the converter uses when the rig carries no
    /// frame rate of its own, matching the managed extractor's argument.
    #[napi]
    pub fn read_cubism_physics(
        &self,
        file_index: u32,
        path_id: BigInt,
        motion_fps: Option<f64>,
        maximum_bytes: Option<i64>,
    ) -> Result<CubismDocument> {
        let maximum = byte_limit(maximum_bytes)?;
        let rig = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_cubism_physics(CubismPhysicsReadLimits::default())
            .map_err(core_error)?;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "physics3.json's fps field is a float"
        )]
        let fallback = motion_fps.unwrap_or(30.0) as f32;
        let json = materialize_core_bytes(maximum, "physics3 JSON", |output| {
            rig.write_physics3_json(fallback, output, maximum)
        })?;
        Ok(CubismDocument {
            name: String::new(),
            json: json.into(),
            entry_count: count_u32(rig.sub_rigs.len(), "Cubism physics sub-rig count")?,
        })
    }

    /// Reads a `CubismExpressionData` and writes its exp3.json.
    #[napi]
    pub fn read_cubism_expression(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<CubismDocument> {
        let maximum = byte_limit(maximum_bytes)?;
        let expression = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_cubism_expression(CubismExpressionReadLimits::default())
            .map_err(core_error)?;
        let json = materialize_core_bytes(maximum, "exp3 JSON", |output| {
            expression.write_exp3_json(output, maximum)
        })?;
        let entry_count = count_u32(
            expression.parameters.len(),
            "Cubism expression parameter count",
        )?;
        Ok(CubismDocument {
            name: expression.source_name,
            json: json.into(),
            entry_count,
        })
    }

    /// Reads a `CubismFadeMotionData` and writes its motion3.json.
    #[napi]
    pub fn read_cubism_fade_motion(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<CubismDocument> {
        let maximum = byte_limit(maximum_bytes)?;
        let motion = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_cubism_fade_motion(CubismFadeMotionReadLimits::default())
            .map_err(core_error)?;
        let json = materialize_core_bytes(maximum, "motion3 JSON", |output| {
            motion.write_motion3_json(&CubismMotionTargetNames::default(), false, output, maximum)
        })?;
        let entry_count = count_u32(motion.curves.len(), "Cubism fade-motion curve count")?;
        Ok(CubismDocument {
            name: motion.source_name,
            json: json.into(),
            entry_count,
        })
    }

    /// Reads one embedded-schema `CubismPosePart` component.
    #[napi]
    pub fn read_cubism_pose_part(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<CubismPosePart> {
        let limits = cubism_auxiliary_limits(byte_limit(maximum_bytes)?)?;
        let pose = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_cubism_pose_part(limits)
            .map_err(core_error)?;
        Ok(CubismPosePart {
            path_id: pose.path_id.into(),
            group_index: pose.group_index,
            links: pose.links,
        })
    }

    /// Reads one embedded-schema Cubism display-info component.
    #[napi]
    pub fn read_cubism_display_info(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<CubismDisplayInfo> {
        let limits = cubism_auxiliary_limits(byte_limit(maximum_bytes)?)?;
        let info = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_cubism_display_info(limits)
            .map_err(core_error)?;
        let effective_name = copy_string(info.effective_name(), "Cubism effective display name")?;
        Ok(CubismDisplayInfo {
            path_id: info.path_id.into(),
            name: info.name,
            display_name: info.display_name,
            effective_name,
        })
    }

    /// Projects one real Unity `AnimationClip` to Cubism motion3 JSON.
    #[napi]
    pub fn read_cubism_clip_motion(
        &self,
        file_index: u32,
        path_id: BigInt,
        #[napi(ts_arg_type = "CubismMotionTargets | undefined | null")] targets: Option<Object<'_>>,
        force_bezier: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<CubismClipMotion> {
        let maximum = byte_limit(maximum_bytes)?;
        let limits = cubism_clip_motion_limits(maximum)?;
        let target_names = cubism_motion_targets(targets, &limits)?;
        let motion = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_cubism_clip_motion(&target_names, limits)
            .map_err(core_error)?;
        build_cubism_clip_motion(motion, force_bezier.unwrap_or(false), maximum)
            .map(CubismClipMotionOutput::into_js)
    }

    /// Projects one ACL-backed Tuanjie `AnimationClip` on a worker.
    ///
    /// The JavaScript decoder receives only Core-validated, owned ACL input.
    /// Core then validates every returned time, binding index and value before
    /// the motion3 document is built. The worker is required so the callback
    /// can execute on the JavaScript event loop without deadlocking it.
    #[napi(ts_return_type = "Promise<CubismClipMotion>")]
    pub fn read_cubism_clip_motion_with_acl_decoder(
        &self,
        file_index: u32,
        path_id: BigInt,
        #[napi(ts_arg_type = "(request: AclDecodeRequest) => AclDecodedClip")] decoder: AclCallback,
        #[napi(ts_arg_type = "CubismMotionTargets | undefined | null")] targets: Option<Object<'_>>,
        force_bezier: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<CubismClipMotionWithAclTask>> {
        let maximum = byte_limit(maximum_bytes)?;
        let limits = cubism_clip_motion_limits(maximum)?;
        Ok(AsyncTask::new(CubismClipMotionWithAclTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            targets: cubism_motion_targets(targets, &limits)?,
            force_bezier: force_bezier.unwrap_or(false),
            maximum,
            decoder: Arc::new(JsAclDecoder {
                callback: decoder,
                limits: AclDecodeLimits::default(),
            }),
        }))
    }

    /// Enumerates the `GameObject` branches a split-objects export would write.
    ///
    /// The whole-collection FBX calls put every model in one file; these name
    /// the branches so a caller can write one file each, which is what the
    /// CLI's `split-objects` does.
    #[napi]
    pub fn split_object_fbx_candidates(&self) -> Result<Vec<FbxCandidate>> {
        let candidates = self
            .studio
            .split_object_fbx_candidates(ModelExportPlanLimits::default())
            .map_err(core_error)?;
        convert_fbx_candidates(candidates, "split-object FBX candidates")
    }

    /// Enumerates the branches an Animator owns.
    #[napi]
    pub fn animator_fbx_candidates(&self) -> Result<Vec<FbxCandidate>> {
        let candidates = self
            .studio
            .animator_fbx_candidates(ModelExportPlanLimits::default())
            .map_err(core_error)?;
        convert_fbx_candidates(candidates, "Animator FBX candidates")
    }

    /// Writes one selected `GameObject` branch as FBX.
    #[napi]
    pub fn read_game_object_fbx(
        &self,
        file_index: u32,
        path_id: BigInt,
        include_animations: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<Buffer> {
        let key = SceneObjectKey {
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
        };
        self.studio
            .read_game_object_fbx_with_acl_decoder(
                key,
                include_animations.unwrap_or(true),
                byte_limit(maximum_bytes)?,
                None,
            )
            .map(Into::into)
            .map_err(core_error)
    }

    /// Writes one selected `GameObject` branch as animated ASCII FBX while a
    /// worker delegates Tuanjie ACL decompression to JavaScript.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_game_object_fbx_with_acl_decoder(
        &self,
        file_index: u32,
        path_id: BigInt,
        #[napi(ts_arg_type = "(request: AclDecodeRequest) => AclDecodedClip")] decoder: AclCallback,
        include_animations: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<FbxWithAclTask>> {
        Ok(AsyncTask::new(FbxWithAclTask {
            studio: Arc::clone(&self.studio),
            maximum: byte_limit(maximum_bytes)?,
            decoder: Arc::new(JsAclDecoder {
                callback: decoder,
                limits: AclDecodeLimits::default(),
            }),
            request: FbxWithAclRequest::GameObject {
                key: SceneObjectKey {
                    file_index: usize::try_from(file_index).expect("u32 fits usize"),
                    path_id: bigint_i64(path_id, "pathId")?,
                },
                include_animations: include_animations.unwrap_or(true),
            },
        }))
    }

    /// Reads a `Font`'s resident payload.
    #[napi]
    pub fn read_font(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<BinaryAsset> {
        self.binary_asset(file_index, path_id, maximum_bytes, |object, limits| {
            object.read_font(limits)
        })
    }

    /// Reads the resident Ogg payload from a legacy `MovieTexture`.
    #[napi]
    pub fn read_movie_texture(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<BinaryAsset> {
        self.binary_asset(file_index, path_id, maximum_bytes, |object, limits| {
            object.read_movie_texture(limits)
        })
    }

    /// Reads a `VideoClip`'s resident or external payload.
    #[napi]
    pub fn read_video_clip(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<BinaryAsset> {
        self.binary_asset(file_index, path_id, maximum_bytes, |object, limits| {
            object.read_video_clip(limits)
        })
    }

    /// Exports every supported object into `outputRoot`.
    ///
    /// The Core exporter writes atomically and never overwrites unless asked,
    /// and a failure on one object is recorded rather than raised so a single
    /// unreadable asset does not cost the run.
    // napi marshals JavaScript strings by value; references do not expand.
    #[allow(clippy::needless_pass_by_value)]
    #[napi]
    pub fn export(&self, output_root: String, overwrite: Option<bool>) -> Result<ExportReport> {
        let options = CoreExportOptions {
            overwrite_existing: overwrite.unwrap_or(false),
            ..CoreExportOptions::default()
        };
        let report = self
            .studio
            .export(&output_root, options)
            .map_err(core_error)?;
        export_report(report)
    }

    /// Exports every supported object with the complete Core policy surface.
    ///
    /// This is additive to `export`: existing callers keep the compact
    /// overwrite flag, while callers that need deterministic names, raw/dump
    /// modes, image/audio selection or aggregate budgets can express them
    /// without dropping to the CLI.
    #[allow(clippy::needless_pass_by_value)]
    #[napi]
    pub fn export_with_options(
        &self,
        output_root: String,
        options: Option<ExportConfiguration>,
    ) -> Result<ExportReport> {
        let options = export_configuration(options)?;
        let report = self
            .studio
            .export(&output_root, options)
            .map_err(core_error)?;
        export_report(report)
    }

    /// Recursively extracts one file or directory tree without loading it.
    ///
    /// Child symlinks are never followed and every archive path is made
    /// relative before it is joined to the output root, so a hostile entry
    /// cannot escape it.
    // napi marshals JavaScript strings by value; references do not expand.
    #[allow(clippy::needless_pass_by_value)]
    #[napi]
    pub fn extract(
        input: String,
        output_root: String,
        overwrite: Option<bool>,
    ) -> Result<ExtractionReport> {
        let options = ExtractionOptions {
            overwrite_existing: overwrite.unwrap_or(false),
            ..ExtractionOptions::default()
        };
        let report = Studio::extract(&input, &output_root, options).map_err(core_error)?;
        Ok(ExtractionReport {
            extracted_count: u32::try_from(report.extracted.len())
                .map_err(|_| invalid_arg("extracted count does not fit u32"))?,
            skipped_existing_count: u32::try_from(report.skipped_existing.len())
                .map_err(|_| invalid_arg("skipped count does not fit u32"))?,
            failure_count: u32::try_from(report.failures.len())
                .map_err(|_| invalid_arg("failure count does not fit u32"))?,
            output_bytes: BigInt::from(report.output_bytes),
        })
    }

    /// Reads complete bounded `AnimationClip` shape, muscle, ACL, and external
    /// streaming metadata without copying parsed keyframe arrays into
    /// JavaScript.
    #[napi]
    pub fn read_animation_clip_info(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AnimationClipInfo> {
        let maximum = byte_limit(maximum_bytes)?;
        let clip = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_animation_clip(animation_clip_limits(maximum)?)
            .map_err(core_error)?;
        convert_animation_clip_info(clip)
    }

    /// Reads the stable references from one legacy Unity `Animation` component.
    #[napi]
    pub fn read_legacy_animation(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<LegacyAnimationInfo> {
        let limits = animation_component_limits(maximum_bytes)?;
        let animation = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_legacy_animation(limits)
            .map_err(core_error)?;
        let mut clips = reserve(animation.clips.len(), "legacy Animation clip references")?;
        for reference in animation.clips {
            clips.push(object_reference(reference));
        }
        Ok(LegacyAnimationInfo {
            path_id: BigInt::from(animation.path_id),
            game_object: object_reference(animation.behaviour.component.game_object),
            enabled: u32::from(animation.behaviour.enabled),
            default_clip: object_reference(animation.default_clip),
            clips,
            trailing_bytes: BigInt::from(animation.trailing_bytes),
        })
    }

    /// Reads one bounded Unity `AnimatorOverrideController` substitution table.
    #[napi]
    pub fn read_animator_override_controller(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AnimatorOverrideControllerInfo> {
        let limits = animation_component_limits(maximum_bytes)?;
        let controller = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_animator_override_controller(limits)
            .map_err(core_error)?;
        let mut clip_overrides = reserve(
            controller.clips.len(),
            "AnimatorOverrideController clip overrides",
        )?;
        for pair in controller.clips {
            clip_overrides.push(AnimationClipOverrideInfo {
                original_clip: object_reference(pair.original_clip),
                override_clip: object_reference(pair.override_clip),
            });
        }
        Ok(AnimatorOverrideControllerInfo {
            path_id: BigInt::from(controller.path_id),
            name: controller.name,
            controller: object_reference(controller.controller),
            clip_overrides,
            trailing_bytes: BigInt::from(controller.trailing_bytes),
        })
    }

    /// Reads inherited/effective names, dependencies and ordered tables from
    /// one bounded Unity `AssetBundle` object.
    #[napi]
    pub fn read_asset_bundle(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_entries: Option<u32>,
        maximum_string_bytes: Option<i64>,
        maximum_total_string_bytes: Option<i64>,
    ) -> Result<AssetBundleInfo> {
        let path_id = bigint_i64(path_id, "pathId")?;
        let bundle = self
            .object(file_index, path_id)?
            .read_asset_bundle(container_metadata_limits(
                maximum_entries,
                maximum_string_bytes,
                maximum_total_string_bytes,
            )?)
            .map_err(core_error)?;
        let mut preload_table = reserve(bundle.preload_table.len(), "AssetBundle preload table")?;
        for reference in bundle.preload_table {
            preload_table.push(object_reference(reference));
        }
        let mut container = reserve(bundle.container.len(), "AssetBundle container")?;
        for entry in bundle.container {
            container.push(AssetBundleContainerEntry {
                key: entry.key,
                preload_index: count_u32(entry.preload_index, "preload index")?,
                preload_size: count_u32(entry.preload_size, "preload size")?,
                asset: object_reference(entry.asset),
            });
        }
        Ok(AssetBundleInfo {
            path_id: BigInt::from(path_id),
            name: bundle.name,
            object_name: bundle.object_name,
            asset_bundle_name: bundle.asset_bundle_name,
            preload_table,
            container,
            dependencies: bundle.dependencies,
            is_streamed_scene_asset_bundle: bundle.is_streamed_scene_asset_bundle,
        })
    }

    /// Reads one bounded Unity `ResourceManager` named-container table.
    #[napi]
    pub fn read_resource_manager(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_entries: Option<u32>,
        maximum_string_bytes: Option<i64>,
        maximum_total_string_bytes: Option<i64>,
    ) -> Result<ResourceManagerInfo> {
        let path_id = bigint_i64(path_id, "pathId")?;
        let manager = self
            .object(file_index, path_id)?
            .read_resource_manager(container_metadata_limits(
                maximum_entries,
                maximum_string_bytes,
                maximum_total_string_bytes,
            )?)
            .map_err(core_error)?;
        let mut container = reserve(manager.container.len(), "ResourceManager container")?;
        for entry in manager.container {
            container.push(ResourceManagerContainerEntry {
                key: entry.key,
                asset: object_reference(entry.asset),
            });
        }
        Ok(ResourceManagerInfo {
            path_id: BigInt::from(path_id),
            container,
        })
    }

    /// Reads one bounded Unity `PreloadData` object-reference table.
    #[napi]
    pub fn read_preload_data(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_entries: Option<u32>,
        maximum_string_bytes: Option<i64>,
        maximum_total_string_bytes: Option<i64>,
    ) -> Result<PreloadDataInfo> {
        let path_id = bigint_i64(path_id, "pathId")?;
        let preload = self
            .object(file_index, path_id)?
            .read_preload_data(container_metadata_limits(
                maximum_entries,
                maximum_string_bytes,
                maximum_total_string_bytes,
            )?)
            .map_err(core_error)?;
        let mut assets = reserve(preload.assets.len(), "PreloadData asset references")?;
        for reference in preload.assets {
            assets.push(object_reference(reference));
        }
        Ok(PreloadDataInfo {
            path_id: BigInt::from(path_id),
            name: preload.name,
            assets,
        })
    }

    /// Reads an `AnimatorController`'s identity and the clips it references.
    #[napi]
    pub fn read_animator_controller(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AnimatorControllerInfo> {
        let maximum = byte_limit(maximum_bytes)?;
        let controller = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_animator_controller(AnimatorControllerReadLimits {
                maximum_object_bytes: maximum,
                ..AnimatorControllerReadLimits::default()
            })
            .map_err(core_error)?;
        let mut animation_clip_path_ids = reserve(
            controller.animation_clips.len(),
            "AnimatorController clip path IDs",
        )?;
        animation_clip_path_ids.extend(
            controller
                .animation_clips
                .iter()
                .map(|reference| BigInt::from(reference.path_id)),
        );
        Ok(AnimatorControllerInfo {
            name: controller.name,
            tos_entry_count: u32::try_from(controller.tos.len())
                .map_err(|_| invalid_arg("TOS entry count does not fit u32"))?,
            animation_clip_path_ids,
        })
    }

    /// Discovers the Live2D models in the collection.
    ///
    /// Only the shape of each package is returned. Materializing the files --
    /// the MOC, textures and JSON -- is a separate concern and belongs behind
    /// an explicit output budget.
    #[napi]
    pub fn live2d_packages(&self) -> Result<Vec<Live2dPackageInfo>> {
        let set = self
            .studio
            .live2d_packages(Live2dPackageLimits::default())
            .map_err(core_error)?;
        let mut packages = reserve(set.packages.len(), "Live2D package metadata")?;
        for package in set.packages {
            packages.push(Live2dPackageInfo {
                name: package.name,
                directory_name: package.directory_name,
                moc_file_name: package.moc_file_name,
                texture_count: count_u32(package.textures.len(), "Live2D texture count")?,
                expression_count: count_u32(package.expressions.len(), "Live2D expression count")?,
                motion_count: count_u32(package.motions.len(), "Live2D motion count")?,
                has_physics: package.physics.is_some(),
                has_pose: package.pose.is_some(),
                has_display_info: package.display_info.is_some(),
            });
        }
        Ok(packages)
    }

    /// Writes the collection as ASCII FBX 7.4 including its animation tracks.
    ///
    /// The static variant omits animation deliberately; this is the one a
    /// caller wants for a rigged model.
    #[napi]
    pub fn read_fbx(&self, maximum_bytes: Option<i64>) -> Result<Buffer> {
        self.studio
            .read_fbx(byte_limit(maximum_bytes)?)
            .map(Into::into)
            .map_err(core_error)
    }

    /// The same animated scene in FBX 7.4's binary encoding.
    #[napi]
    pub fn read_fbx_binary(&self, maximum_bytes: Option<i64>) -> Result<Buffer> {
        self.studio
            .read_fbx_binary(byte_limit(maximum_bytes)?)
            .map(Into::into)
            .map_err(core_error)
    }

    /// Materializes every Live2D package: the MOC, the model3 manifest, the
    /// mip-zero texture PNGs, and the expression, motion, physics, pose and
    /// display-info JSON where their verified fields are present.
    ///
    /// Returned in memory rather than written, so the caller decides where the
    /// files land and stays inside whatever budget it set.
    #[napi]
    pub fn read_live2d_packages(&self, maximum_bytes: Option<i64>) -> Result<Live2dPackageSet> {
        let (planning_limits, materialize_limits) = live2d_package_limits(None, maximum_bytes)?;
        let set = self
            .studio
            .read_live2d_packages(planning_limits, materialize_limits)
            .map_err(core_error)?;
        convert_live2d_package_set(set)
    }

    /// Materializes every verified Live2D package using trusted external
    /// MonoBehaviour schemas when a shipped build stripped its type trees.
    ///
    /// Schemas are inert data produced by an offline tool. Embedded type trees
    /// retain priority, matching the Core and Python surfaces.
    #[napi]
    pub fn read_live2d_packages_with_schemas(
        &self,
        env: Env,
        #[napi(ts_arg_type = "Array<MonoBehaviourSchema>")] schemas: Array<'_>,
        maximum_file_bytes: Option<i64>,
        maximum_total_bytes: Option<i64>,
    ) -> Result<Live2dPackageSet> {
        let registry = build_schema_registry(env, schemas)?;
        let (planning_limits, materialize_limits) =
            live2d_package_limits(maximum_file_bytes, maximum_total_bytes)?;
        let set = self
            .studio
            .read_live2d_packages_with_schema_provider(
                planning_limits,
                materialize_limits,
                &registry,
            )
            .map_err(core_error)?;
        convert_live2d_package_set(set)
    }

    /// Materializes every verified Live2D package on a worker while a
    /// JavaScript callback decodes Tuanjie ACL animation tracks.
    ///
    /// `schemas` is optional so the same call handles embedded trees, stripped
    /// managed layouts, or both. Core validates all decoded curves and output
    /// budgets before JavaScript receives the package bytes.
    #[napi(ts_return_type = "Promise<Live2DPackageSet>")]
    pub fn read_live2d_packages_with_acl_decoder(
        &self,
        env: Env,
        #[napi(ts_arg_type = "(request: AclDecodeRequest) => AclDecodedClip")] decoder: AclCallback,
        #[napi(ts_arg_type = "Array<MonoBehaviourSchema> | undefined | null")] schemas: Option<
            Array<'_>,
        >,
        maximum_file_bytes: Option<i64>,
        maximum_total_bytes: Option<i64>,
    ) -> Result<AsyncTask<Live2dPackagesWithAclTask>> {
        let schemas = schemas
            .map(|schemas| parse_schema_entries(env, schemas))
            .transpose()?;
        let (planning_limits, materialize_limits) =
            live2d_package_limits(maximum_file_bytes, maximum_total_bytes)?;
        Ok(AsyncTask::new(Live2dPackagesWithAclTask {
            studio: Arc::clone(&self.studio),
            planning_limits,
            materialize_limits,
            schemas,
            decoder: Arc::new(JsAclDecoder {
                callback: decoder,
                limits: AclDecodeLimits::default(),
            }),
        }))
    }

    /// Writes the whole scene as one Wavefront OBJ, with the material library
    /// it names and that library's textures.
    ///
    /// Distinct from `readMeshObj`, which writes one mesh the way the managed
    /// exporter does. This is the scene: every renderer placed in world space.
    ///
    /// `materialLibraryName` is what the OBJ's `mtllib` line will say, so it
    /// has to be the name the library is actually written under.
    /// `textureFormat` defaults to PNG and accepts the same format names as
    /// the Core and Python surfaces. `textureLimits` independently bounds the
    /// texture count, total encoded bytes and each texture's payload, decoded
    /// output and decoder workspace.
    #[napi]
    pub fn read_model_obj(
        &self,
        material_library_name: Option<String>,
        maximum_bytes: Option<i64>,
        texture_format: Option<String>,
        texture_limits: Option<ModelTextureLimits>,
    ) -> Result<ModelObj> {
        let maximum = byte_limit(maximum_bytes)?;
        let texture_format = parse_image_format(texture_format)?;
        let texture_limits = model_texture_limits(texture_limits)?;
        let name = material_library_name.unwrap_or_else(|| "model.mtl".to_owned());
        let model = self
            .studio
            .read_model_obj(&name, maximum, texture_format, texture_limits)
            .map_err(core_error)?;
        let textures =
            convert_scene_texture_files(model.textures.textures, "model OBJ texture files")?;
        let skipped =
            convert_scene_texture_skips(model.textures.skipped, "model OBJ skipped textures")?;
        Ok(ModelObj {
            obj: model.obj.into(),
            material_library_name: model.material_library_name,
            material_library: model.material_library.into(),
            textures,
            skipped,
        })
    }

    /// Writes the collection as ASCII FBX with its animations and returns the
    /// material textures it references.
    ///
    /// The FBX names each texture by file name, so the returned files have to
    /// be written beside it for those references to resolve. They come back
    /// rather than being written because this call has no directory of its own
    /// and where they land is the caller's decision. `textureFormat` defaults
    /// to PNG and accepts the same format names as the Core and Python surfaces.
    /// `textureLimits` has the same meaning as on `readModelObj`.
    #[napi]
    pub fn read_fbx_with_textures(
        &self,
        maximum_bytes: Option<i64>,
        texture_format: Option<String>,
        texture_limits: Option<ModelTextureLimits>,
    ) -> Result<TexturedFbx> {
        let maximum = byte_limit(maximum_bytes)?;
        let texture_format = parse_image_format(texture_format)?;
        let texture_limits = model_texture_limits(texture_limits)?;
        let (fbx, textures) =
            materialize_core_output(maximum, "ASCII FBX with textures", |output| {
                self.studio
                    .write_fbx_with_textures(output, maximum, texture_format, texture_limits)
            })?;
        let texture_files = convert_scene_texture_files(textures.textures, "FBX texture files")?;
        let skipped = convert_scene_texture_skips(textures.skipped, "FBX skipped textures")?;
        Ok(TexturedFbx {
            fbx: fbx.into(),
            textures: texture_files,
            skipped,
        })
    }

    /// Inspects an `AnimationClip`'s ACL blob without decompressing it.
    ///
    /// Core ships no ACL decoder, so this is what a caller needs to decide
    /// whether its own decoder can handle the blob before asking for it.
    #[napi]
    pub fn read_acl_tracks(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
    ) -> Result<AclTracks> {
        let maximum = byte_limit(maximum_bytes)?;
        let clip = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_animation_clip(animation_clip_limits(maximum)?)
            .map_err(core_error)?;
        let acl = clip
            .muscle_clip
            .as_ref()
            .and_then(|muscle| muscle.clip.acl.as_ref())
            .ok_or_else(|| invalid_arg("AnimationClip does not contain ACL tracks"))?;
        let tracks = acl
            .inspect_compressed_tracks(AclCompressedTracksLimits {
                maximum_compressed_bytes: maximum,
                ..AclCompressedTracksLimits::default()
            })
            .map_err(core_error)?;
        Ok(AclTracks {
            declared_size: tracks.declared_size,
            stored_hash: tracks.stored_hash,
            version: tracks.version,
            track_type: tracks.track_type.name().to_owned(),
            track_count: tracks.num_tracks,
            samples_per_track: tracks.num_samples_per_track,
            sample_rate: f64::from(tracks.sample_rate()),
            decompressed_value_count: BigInt::from(tracks.decompressed_value_count),
            has_metadata: tracks.has_metadata(),
            is_wrap_optimized: tracks.is_wrap_optimized(),
            has_database: tracks.has_database(),
            has_stripped_keyframes: tracks.has_stripped_keyframes(),
        })
    }

    /// Reads a `MonoBehaviour` as JSON through the type tree embedded in the
    /// serialized file.
    ///
    /// A stripped build has no embedded managed layout and is refused rather
    /// than guessed; use `readMonoBehaviourJsonWithSchemas` for that case.
    #[napi]
    pub fn read_mono_behaviour_json(
        &self,
        file_index: u32,
        path_id: BigInt,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<MonoBehaviourJson> {
        self.mono_behaviour_json(
            file_index,
            path_id,
            &MonoBehaviourSchemaRegistry::new(),
            pretty,
            maximum_bytes,
        )
    }

    /// Worker-backed form of `readMonoBehaviourJson` for large or untrusted
    /// embedded type trees.
    #[napi(ts_return_type = "Promise<MonoBehaviourJson>")]
    pub fn read_mono_behaviour_json_async(
        &self,
        file_index: u32,
        path_id: BigInt,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadMonoBehaviourJsonTask>> {
        self.mono_behaviour_json_task(
            file_index,
            path_id,
            PendingMonoBehaviourSchemas::default(),
            pretty,
            maximum_bytes,
        )
    }

    /// Reads a `MonoBehaviour` as JSON, resolving its stripped managed fields
    /// through caller-supplied schemas.
    ///
    /// A shipped build strips the managed type layout, so without a schema only
    /// the engine-owned prefix can be read. The schemas are data: they are
    /// matched by assembly, namespace, class and optionally Unity version, and
    /// nothing in them is executed.
    #[napi]
    pub fn read_mono_behaviour_json_with_schemas(
        &self,
        env: Env,
        file_index: u32,
        path_id: BigInt,
        #[napi(ts_arg_type = "Array<MonoBehaviourSchema>")] schemas: Array<'_>,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<MonoBehaviourJson> {
        let registry = build_schema_registry(env, schemas)?;
        self.mono_behaviour_json(file_index, path_id, &registry, pretty, maximum_bytes)
    }

    /// Worker-backed form of `readMonoBehaviourJsonWithSchemas`. JavaScript
    /// values are count-checked and copied before the task is queued; schema
    /// identity validation, registry indexing, parsing and JSON
    /// materialization happen on the worker.
    #[napi(ts_return_type = "Promise<MonoBehaviourJson>")]
    pub fn read_mono_behaviour_json_with_schemas_async(
        &self,
        env: Env,
        file_index: u32,
        path_id: BigInt,
        #[napi(ts_arg_type = "Array<MonoBehaviourSchema>")] schemas: Array<'_>,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadMonoBehaviourJsonTask>> {
        self.mono_behaviour_json_task(
            file_index,
            path_id,
            parse_schema_entries(env, schemas)?,
            pretty,
            maximum_bytes,
        )
    }
}

impl UnityRs {
    /// Shared bounded body for embedded and externally supplied
    /// `MonoBehaviour` type trees.
    fn mono_behaviour_json(
        &self,
        file_index: u32,
        path_id: BigInt,
        registry: &MonoBehaviourSchemaRegistry,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<MonoBehaviourJson> {
        let maximum = byte_limit(maximum_bytes)?;
        let limits = MonoBehaviourReadLimits {
            maximum_json_bytes: usize::try_from(maximum).unwrap_or(usize::MAX),
            ..MonoBehaviourReadLimits::default()
        };
        let resolved = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_mono_behaviour_json(registry, pretty.unwrap_or(false), limits)
            .map_err(core_error)?;
        Ok(MonoBehaviourJson {
            json: resolved.json.into(),
            source: schema_source_name(resolved.source).to_owned(),
        })
    }

    fn mono_behaviour_json_task(
        &self,
        file_index: u32,
        path_id: BigInt,
        schemas: PendingMonoBehaviourSchemas,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<ReadMonoBehaviourJsonTask>> {
        Ok(AsyncTask::new(ReadMonoBehaviourJsonTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            schemas,
            pretty: pretty.unwrap_or(false),
            maximum: byte_limit(maximum_bytes)?,
        }))
    }

    /// Shared body for the three binary-asset readers, which differ only in
    /// which Core method they call.
    fn binary_asset(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
        read: impl FnOnce(
            &StudioObject<'_>,
            SimpleAssetReadLimits,
        ) -> unity_rs_core::Result<SimpleBinaryAsset>,
    ) -> Result<BinaryAsset> {
        let maximum = byte_limit(maximum_bytes)?;
        let object = self.object(file_index, bigint_i64(path_id, "pathId")?)?;
        let asset = read(
            &object,
            SimpleAssetReadLimits {
                maximum_payload_bytes: maximum,
                ..SimpleAssetReadLimits::default()
            },
        )
        .map_err(core_error)?;
        let data = asset.payload.read_to_vec(maximum).map_err(core_error)?;
        Ok(BinaryAsset {
            name: asset.name,
            kind: asset.payload_kind.to_owned(),
            extension: asset.suggested_extension,
            data: data.into(),
        })
    }

    fn object(&self, file_index: u32, path_id: i64) -> Result<StudioObject<'_>> {
        let file_index = usize::try_from(file_index).expect("u32 fits usize");
        studio_object(&self.studio, file_index, path_id)
    }

    fn read_bytes_task(
        &self,
        file_index: u32,
        path_id: BigInt,
        maximum_bytes: Option<i64>,
        kind: ByteReadKind,
    ) -> Result<AsyncTask<ReadBytesTask>> {
        Ok(AsyncTask::new(ReadBytesTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            maximum: byte_limit(maximum_bytes)?,
            kind,
        }))
    }
}

/// The JavaScript ACL decoder callback.
type AclCallback =
    ThreadsafeFunction<AclDecodeRequest, Unknown<'static>, AclDecodeRequest, Status, false>;

/// Bridges a JavaScript ACL decoder into Core's synchronous animation build.
///
/// Same threading constraint as the Oodle bridge: the callback runs on the
/// event loop while a worker waits for it, so this is only reachable from the
/// asynchronous entry points.
struct JsAclDecoder {
    callback: AclCallback,
    limits: AclDecodeLimits,
}

#[derive(Clone, Copy)]
struct JsAclOutputExpectation {
    frame_count: u32,
    declared_curve_count: Option<u32>,
    limits: AclDecodeLimits,
}

impl unity_rs_core::acl::AclDecoder for JsAclDecoder {
    fn decode(
        &self,
        request: &unity_rs_core::acl::AclDecodeRequest<'_>,
    ) -> unity_rs_core::Result<unity_rs_core::acl::AclDecodedClip> {
        let compressed_tracks = copy_core_slice(
            &request.input.compressed_tracks,
            "ACL callback compressed tracks",
        )?;
        let decoder_map = copy_core_slice(&request.input.decoder_map, "ACL callback decoder map")?;
        let payload = AclDecodeRequest {
            frame_count: request.frame_count,
            bone_count: request.bone_count,
            sample_rate: f64::from(request.sample_rate()),
            declared_curve_count: request.declared_curve_count,
            use_fast_sample_mode: request.use_fast_sample_mode,
            compressed_tracks: Buffer::from(compressed_tracks),
            decoder_map,
        };
        let expected = JsAclOutputExpectation {
            frame_count: request.frame_count,
            declared_curve_count: request.declared_curve_count,
            limits: self.limits,
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        self.callback.call_with_return_value(
            payload,
            ThreadsafeFunctionCallMode::Blocking,
            move |result: Result<Unknown<'static>>, _env| {
                let _ = sender.send(result.and_then(|value| parse_js_acl_output(value, expected)));
                Ok(())
            },
        );
        receiver
            .recv()
            .map_err(|_| {
                unity_rs_core::Error::invalid_data("the ACL decoder callback never answered")
            })?
            .map_err(|error| {
                unity_rs_core::Error::invalid_data(format!("the ACL decoder failed: {error}"))
            })
    }
}

fn parse_js_acl_output(
    value: Unknown<'static>,
    expected: JsAclOutputExpectation,
) -> Result<unity_rs_core::acl::AclDecodedClip> {
    if value.get_type()? != ValueType::Object {
        return Err(invalid_arg("ACL decoder must return an object"));
    }
    // `get_type` above establishes the precondition for `Unknown::cast`.
    let object = unsafe { value.cast::<Object<'static>>()? };
    let times: Array<'_> = required_object_field(&object, "times", "ACL decoder output")?;
    let binding_indices: Array<'_> =
        required_object_field(&object, "bindingIndices", "ACL decoder output")?;
    let values: Array<'_> = required_object_field(&object, "values", "ACL decoder output")?;
    let following_curve_offset: u32 =
        required_object_field(&object, "followingCurveOffset", "ACL decoder output")?;

    let frame_count = usize::try_from(expected.frame_count).expect("u32 fits usize");
    let returned_frames = usize::try_from(times.len()).expect("u32 fits usize");
    if returned_frames != frame_count {
        return Err(invalid_arg(format!(
            "ACL decoder returned {returned_frames} times for {frame_count} declared frames"
        )));
    }
    let curve_count = usize::try_from(binding_indices.len()).expect("u32 fits usize");
    if curve_count > expected.limits.maximum_curves {
        return Err(invalid_arg(format!(
            "ACL decoder returned {curve_count} curves, exceeding limit {}",
            expected.limits.maximum_curves
        )));
    }
    if let Some(declared) = expected.declared_curve_count {
        let declared = usize::try_from(declared).expect("u32 fits usize");
        if curve_count != declared {
            return Err(invalid_arg(format!(
                "ACL decoder returned {curve_count} curves for {declared} declared curves"
            )));
        }
    }
    let expected_values = frame_count
        .checked_mul(curve_count)
        .ok_or_else(|| invalid_arg("ACL decoded value count overflowed"))?;
    if expected_values > expected.limits.maximum_values {
        return Err(invalid_arg(format!(
            "ACL decoder output requires {expected_values} values, exceeding limit {}",
            expected.limits.maximum_values
        )));
    }
    let returned_values = usize::try_from(values.len()).expect("u32 fits usize");
    if returned_values != expected_values {
        return Err(invalid_arg(format!(
            "ACL decoder returned {returned_values} values; {expected_values} are required"
        )));
    }

    Ok(unity_rs_core::acl::AclDecodedClip {
        times: copy_js_f32_array(&times, "ACL decoded times")?,
        binding_indices: copy_js_u32_array(&binding_indices, "ACL decoded binding indices")?,
        values: copy_js_f32_array(&values, "ACL decoded values")?,
        following_curve_offset,
    })
}

fn required_object_field<T: napi::bindgen_prelude::FromNapiValue>(
    object: &Object<'_>,
    field: &str,
    owner: &str,
) -> Result<T> {
    object
        .get(field)?
        .ok_or_else(|| invalid_arg(format!("{owner} is missing {field}")))
}

fn optional_object_field<T: napi::bindgen_prelude::FromNapiValue>(
    object: &Object<'_>,
    field: &str,
) -> Result<Option<T>> {
    object.get::<Option<T>>(field).map(Option::flatten)
}

/// Copies one JavaScript string after its UTF-8 length has already passed the
/// caller's budget. napi-rs' ordinary `String` conversion allocates with
/// `vec![0; len]`; using the raw N-API copy here lets the binding reserve
/// fallibly before setting the vector length.
fn copy_js_string(
    env: napi::sys::napi_env,
    value: JsString<'_>,
    length: usize,
    field: &str,
) -> Result<String> {
    let capacity = length
        .checked_add(1)
        .ok_or_else(|| invalid_arg(format!("{field} length overflowed")))?;
    let mut bytes: Vec<u8> = reserve(capacity, field)?;
    bytes.resize(capacity, 0);
    let mut written = 0_usize;
    // SAFETY: `bytes` owns `capacity` initialized bytes, `value` and `env`
    // belong to this active N-API callback, and N-API writes at most the
    // supplied capacity including its trailing NUL.
    napi::check_status!(
        unsafe {
            napi::sys::napi_get_value_string_utf8(
                env,
                value.raw(),
                bytes.as_mut_ptr().cast(),
                capacity,
                &raw mut written,
            )
        },
        "failed to copy {field}"
    )?;
    if written != length {
        return Err(invalid_arg(format!(
            "{field} changed length while being copied: expected {length}, wrote {written}"
        )));
    }
    bytes.truncate(written);
    String::from_utf8(bytes).map_err(|error| invalid_arg(format!("{field} is not UTF-8: {error}")))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "ACL samples are stored as f32 after JavaScript transports them as numbers"
)]
fn copy_js_f32_array(values: &Array<'_>, field: &'static str) -> Result<Vec<f32>> {
    let mut copied = reserve(
        usize::try_from(values.len()).expect("u32 fits usize"),
        field,
    )?;
    for index in 0..values.len() {
        let value = values
            .get::<f64>(index)?
            .ok_or_else(|| invalid_arg(format!("{field} is missing index {index}")))?;
        copied.push(value as f32);
    }
    Ok(copied)
}

fn copy_js_u32_array(values: &Array<'_>, field: &'static str) -> Result<Vec<u32>> {
    let mut copied = reserve(
        usize::try_from(values.len()).expect("u32 fits usize"),
        field,
    )?;
    for index in 0..values.len() {
        copied.push(
            values
                .get::<u32>(index)?
                .ok_or_else(|| invalid_arg(format!("{field} is missing index {index}")))?,
        );
    }
    Ok(copied)
}

/// The JavaScript decoder callback: compressed bytes and the exact expected
/// output length in, exactly that many bytes out.
type OodleCallback =
    ThreadsafeFunction<FnArgs<(Buffer, u32)>, Buffer, FnArgs<(Buffer, u32)>, Status, false>;

/// Bridges a JavaScript Oodle decoder into Core's synchronous decompression.
///
/// Core never ships an Oodle decoder: the format is licensed, so a caller has
/// to supply one. Core asks for bytes in and bytes out, which means the
/// JavaScript function has to be called from whichever thread is decompressing
/// and its result waited for.
///
/// That is only safe off the main thread. The worker blocks on a channel while
/// the JavaScript callback runs on the event loop; doing the same from the main
/// thread would block the very loop that has to run the callback and deadlock.
/// This is why the only way to reach it is the asynchronous factory.
struct JsOodleDecoder {
    callback: OodleCallback,
}

impl unity_rs_core::bundle::OodleDecoder for JsOodleDecoder {
    fn decompress(&self, input: &[u8], output: &mut [u8]) -> unity_rs_core::Result<usize> {
        let expected = u32::try_from(output.len()).map_err(|_| {
            unity_rs_core::Error::invalid_data("Oodle output length does not fit u32")
        })?;
        let input = copy_core_slice(input, "Oodle callback input")?;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.callback.call_with_return_value(
            (Buffer::from(input), expected).into(),
            ThreadsafeFunctionCallMode::Blocking,
            move |result: Result<Buffer>, _env| {
                let copied =
                    result.and_then(|buffer| copy_slice(buffer.as_ref(), "Oodle decoder output"));
                let _ = sender.send(copied);
                Ok(())
            },
        );
        let decoded = receiver
            .recv()
            .map_err(|_| {
                unity_rs_core::Error::invalid_data("the Oodle decoder callback never answered")
            })?
            .map_err(|error| {
                unity_rs_core::Error::invalid_data(format!("the Oodle decoder failed: {error}"))
            })?;
        if decoded.len() != output.len() {
            return Err(unity_rs_core::Error::invalid_data(format!(
                "the Oodle decoder returned {} bytes, expected {}",
                decoded.len(),
                output.len()
            )));
        }
        output.copy_from_slice(&decoded);
        Ok(decoded.len())
    }
}

/// Builds the animated FBX on a worker so the ACL callback can run on the
/// event loop while this waits for it.
pub struct FbxWithAclTask {
    studio: Arc<Studio>,
    maximum: u64,
    decoder: Arc<JsAclDecoder>,
    request: FbxWithAclRequest,
}

#[derive(Clone, Copy)]
enum FbxWithAclRequest {
    SceneAscii,
    SceneBinary,
    GameObject {
        key: SceneObjectKey,
        include_animations: bool,
    },
}

impl Task for FbxWithAclTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Self::Output> {
        match self.request {
            FbxWithAclRequest::SceneAscii => self
                .studio
                .read_fbx_with_acl_decoder(self.maximum, Some(self.decoder.as_ref())),
            FbxWithAclRequest::SceneBinary => self
                .studio
                .read_fbx_binary_with_acl_decoder(self.maximum, Some(self.decoder.as_ref())),
            FbxWithAclRequest::GameObject {
                key,
                include_animations,
            } => self.studio.read_game_object_fbx_with_acl_decoder(
                key,
                include_animations,
                self.maximum,
                Some(self.decoder.as_ref()),
            ),
        }
        .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, bytes: Self::Output) -> Result<Self::JsValue> {
        Ok(bytes.into())
    }
}

/// Builds one ACL-backed Cubism motion on a worker while its decoder callback
/// is serviced by the JavaScript event loop.
pub struct CubismClipMotionWithAclTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    targets: CubismMotionTargetNames,
    force_bezier: bool,
    maximum: u64,
    decoder: Arc<JsAclDecoder>,
}

impl Task for CubismClipMotionWithAclTask {
    type Output = CubismClipMotionOutput;
    type JsValue = CubismClipMotion;

    fn compute(&mut self) -> Result<Self::Output> {
        let limits = cubism_clip_motion_limits(self.maximum)?;
        let motion = studio_object(&self.studio, self.file_index, self.path_id)?
            .read_cubism_clip_motion_with_acl_decoder(&self.targets, limits, self.decoder.as_ref())
            .map_err(core_error)?;
        build_cubism_clip_motion(motion, self.force_bezier, self.maximum)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into_js())
    }
}

/// Builds complete `Live2D` packages on a worker while JavaScript services ACL
/// decode callbacks on the event loop.
pub struct Live2dPackagesWithAclTask {
    studio: Arc<Studio>,
    planning_limits: Live2dPackageLimits,
    materialize_limits: Live2dPackageMaterializeLimits,
    schemas: Option<PendingMonoBehaviourSchemas>,
    decoder: Arc<JsAclDecoder>,
}

impl Task for Live2dPackagesWithAclTask {
    type Output = Live2dPackageSet;
    type JsValue = Live2dPackageSet;

    fn compute(&mut self) -> Result<Self::Output> {
        let schemas = self
            .schemas
            .take()
            .map(PendingMonoBehaviourSchemas::into_registry)
            .transpose()?;
        let provider = schemas
            .as_ref()
            .map(|value| value as &dyn MonoBehaviourSchemaProvider);
        let set = self
            .studio
            .read_live2d_packages_with_adapters(
                self.planning_limits,
                self.materialize_limits,
                provider,
                Some(self.decoder.as_ref()),
            )
            .map_err(core_error)?;
        convert_live2d_package_set(set)
    }

    fn resolve(&mut self, _env: Env, set: Self::Output) -> Result<Self::JsValue> {
        Ok(set)
    }
}

pub struct OpenWithOodleTask {
    path: String,
    decoder: Arc<JsOodleDecoder>,
    options: Option<OpenOptions>,
}

impl Task for OpenWithOodleTask {
    type Output = Studio;
    type JsValue = UnityRs;

    fn compute(&mut self) -> Result<Self::Output> {
        let oodle = Arc::clone(&self.decoder) as Arc<dyn unity_rs_core::bundle::OodleDecoder>;
        let options = load_options(self.options.take(), Some(oodle))?;
        Studio::open_with_options(&self.path, options).map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, studio: Self::Output) -> Result<Self::JsValue> {
        Ok(UnityRs {
            studio: Arc::new(studio),
        })
    }
}

pub struct OpenPathTask {
    path: String,
    options: Option<OpenOptions>,
}

impl Task for OpenPathTask {
    type Output = Studio;
    type JsValue = UnityRs;

    fn compute(&mut self) -> Result<Self::Output> {
        Studio::open_with_options(&self.path, load_options(self.options.take(), None)?)
            .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, studio: Self::Output) -> Result<Self::JsValue> {
        Ok(UnityRs {
            studio: Arc::new(studio),
        })
    }
}

pub struct OpenBufferTask {
    bytes: Vec<u8>,
    name: String,
    options: Option<OpenOptions>,
}

impl Task for OpenBufferTask {
    type Output = Studio;
    type JsValue = UnityRs;

    fn compute(&mut self) -> Result<Self::Output> {
        let bytes = std::mem::take(&mut self.bytes);
        let name = std::mem::take(&mut self.name);
        Studio::open_region_with_options(
            name,
            Region::from_bytes(bytes),
            load_options(self.options.take(), None)?,
        )
        .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, studio: Self::Output) -> Result<Self::JsValue> {
        Ok(UnityRs {
            studio: Arc::new(studio),
        })
    }
}

#[derive(Clone, Copy)]
enum ByteReadKind {
    Raw,
    Text,
    TypeTreeJson { pretty: bool },
    TypeTreeDump,
    Shader,
    MeshObj,
}

pub struct ReadBytesTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    maximum: u64,
    kind: ByteReadKind,
}

impl Task for ReadBytesTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Self::Output> {
        let object = studio_object(&self.studio, self.file_index, self.path_id)?;
        match self.kind {
            ByteReadKind::Raw => object.read_raw(self.maximum),
            ByteReadKind::Text => object.read_text_bytes(
                usize::try_from(self.maximum)
                    .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?,
            ),
            ByteReadKind::TypeTreeJson { pretty } => object.read_type_tree_json(
                pretty,
                usize::try_from(self.maximum)
                    .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?,
            ),
            ByteReadKind::TypeTreeDump => object.read_type_tree_dump(self.maximum),
            ByteReadKind::Shader => object.read_shader_text(self.maximum),
            ByteReadKind::MeshObj => object.read_mesh_obj(MeshReadLimits {
                maximum_object_bytes: self.maximum,
                maximum_output_bytes: self.maximum,
                ..MeshReadLimits::default()
            }),
        }
        .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output.into())
    }
}

pub struct ReadMonoBehaviourJsonTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    schemas: PendingMonoBehaviourSchemas,
    pretty: bool,
    maximum: u64,
}

impl Task for ReadMonoBehaviourJsonTask {
    type Output = ResolvedMonoBehaviourJson;
    type JsValue = MonoBehaviourJson;

    fn compute(&mut self) -> Result<Self::Output> {
        let registry = std::mem::take(&mut self.schemas).into_registry()?;
        studio_object(&self.studio, self.file_index, self.path_id)?
            .read_mono_behaviour_json(
                &registry,
                self.pretty,
                MonoBehaviourReadLimits {
                    maximum_json_bytes: usize::try_from(self.maximum)
                        .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?,
                    ..MonoBehaviourReadLimits::default()
                },
            )
            .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(MonoBehaviourJson {
            json: output.json.into(),
            source: schema_source_name(output.source).to_owned(),
        })
    }
}

pub struct ReadTextureTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    mip_level: u32,
    maximum: u64,
}

impl Task for ReadTextureTask {
    type Output = DisplayRowImage;
    type JsValue = RgbaImage;

    fn compute(&mut self) -> Result<Self::Output> {
        let image = studio_object(&self.studio, self.file_index, self.path_id)?
            .decode_texture_mip(
                self.mip_level,
                TextureReadLimits {
                    maximum_payload_bytes: self.maximum,
                    maximum_output_bytes: self.maximum,
                    maximum_decoder_working_bytes: self.maximum,
                    ..TextureReadLimits::default()
                },
            )
            .map_err(core_error)?;
        DisplayRowImage::from_decoded(image)
    }

    fn resolve(&mut self, _env: Env, image: Self::Output) -> Result<Self::JsValue> {
        Ok(image.into_node())
    }
}

pub struct ReadTextureArrayTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    maximum: u64,
}

impl Task for ReadTextureArrayTask {
    type Output = DisplayRowImages;
    type JsValue = Vec<RgbaImage>;

    fn compute(&mut self) -> Result<Self::Output> {
        let images = studio_object(&self.studio, self.file_index, self.path_id)?
            .decode_texture_array_mip0(texture_array_limits(self.maximum))
            .map_err(core_error)?;
        DisplayRowImages::from_decoded(images)
    }

    fn resolve(&mut self, _env: Env, images: Self::Output) -> Result<Self::JsValue> {
        Ok(images.into_nodes())
    }
}

pub struct ReadSpriteTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    maximum: u64,
}

impl Task for ReadSpriteTask {
    type Output = unity_rs_core::texture::RgbaImage;
    type JsValue = RgbaImage;

    fn compute(&mut self) -> Result<Self::Output> {
        studio_object(&self.studio, self.file_index, self.path_id)?
            .decode_sprite(sprite_limits(self.maximum), texture_limits(self.maximum))
            .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, image: Self::Output) -> Result<Self::JsValue> {
        Ok(convert_image(image))
    }
}

/// Fully validated, JavaScript-independent inputs for one image encode. The
/// pixel bytes were length-checked against the declared dimensions and copied
/// out of the caller's `Buffer` with a fallible reservation, so the worker
/// thread never touches JavaScript-owned memory.
pub struct EncodeImagePlan {
    image: unity_rs_core::texture::RgbaImage,
    format: ImageFormat,
    jpeg_quality: u8,
    png_compression: PngCompression,
    maximum_bytes: u64,
}

impl EncodeImagePlan {
    fn encode(&self) -> Result<Vec<u8>> {
        unity_rs_core::image_export::encode_rgba_image(
            &self.image,
            self.format,
            ImageRowOrder::Display,
            self.jpeg_quality,
            self.png_compression,
            self.maximum_bytes,
        )
        .map_err(core_error)
    }
}

fn encode_image_plan(
    image: &RgbaImage,
    options: Option<EncodeImageOptions>,
) -> Result<EncodeImagePlan> {
    let (format, jpeg_quality, png_compression, maximum_bytes) = match options {
        None => (
            ImageFormat::Png,
            DEFAULT_JPEG_QUALITY,
            PngCompression::default(),
            DEFAULT_PAYLOAD_LIMIT,
        ),
        Some(options) => {
            let format = parse_image_format(options.image_format)?;
            let jpeg_quality = options
                .jpeg_quality
                .unwrap_or(u32::from(DEFAULT_JPEG_QUALITY));
            if !(1..=100).contains(&jpeg_quality) {
                return Err(invalid_arg(format!(
                    "jpegQuality {jpeg_quality} is outside the supported range 1 through 100"
                )));
            }
            let jpeg_quality = u8::try_from(jpeg_quality)
                .map_err(|_| invalid_arg("jpegQuality does not fit in one byte"))?;
            (
                format,
                jpeg_quality,
                parse_png_compression(options.compression)?,
                byte_limit(options.maximum_bytes)?,
            )
        }
    };
    let expected_bytes = u64::from(image.width)
        .checked_mul(4)
        .and_then(|stride| stride.checked_mul(u64::from(image.height)))
        .ok_or_else(|| invalid_arg("image pixel byte length overflowed"))?;
    let actual_bytes = u64::try_from(image.pixels.len())
        .map_err(|_| invalid_arg("image pixel buffer length does not fit in u64"))?;
    if actual_bytes != expected_bytes {
        return Err(invalid_arg(format!(
            "image pixel buffer holds {actual_bytes} bytes, but {}x{} RGBA8 requires {expected_bytes}",
            image.width, image.height
        )));
    }
    let pixels = copy_slice(image.pixels.as_ref(), "image pixels")?;
    Ok(EncodeImagePlan {
        image: unity_rs_core::texture::RgbaImage {
            width: image.width,
            height: image.height,
            pixels,
        },
        format,
        jpeg_quality,
        png_compression,
        maximum_bytes,
    })
}

pub struct EncodeImageTask {
    plan: EncodeImagePlan,
}

impl Task for EncodeImageTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Self::Output> {
        self.plan.encode()
    }

    fn resolve(&mut self, _env: Env, bytes: Self::Output) -> Result<Self::JsValue> {
        Ok(bytes.into())
    }
}

fn studio_object(studio: &Studio, file_index: usize, path_id: i64) -> Result<StudioObject<'_>> {
    studio.object(file_index, path_id).ok_or_else(|| {
        invalid_arg(format!(
            "object path ID {path_id} was not found in file index {file_index}"
        ))
    })
}

fn build_scene(studio: &Studio, limits: SceneHierarchyLimits) -> Result<Vec<SceneNode>> {
    let hierarchy = studio.scene_hierarchy(limits).map_err(core_error)?;
    let mut nodes = reserve(hierarchy.nodes.len(), "scene nodes")?;
    for node in &hierarchy.nodes {
        nodes.push(SceneNode {
            file_index: u32::try_from(node.object.file_index)
                .map_err(|_| invalid_arg("file index does not fit u32"))?,
            path_id: BigInt::from(node.object.path_id),
            name: copy_string(&node.name, "scene node name")?,
            parent_path_id: node.parent.map(|parent| BigInt::from(parent.path_id)),
            child_count: u32::try_from(node.children.len())
                .map_err(|_| invalid_arg("child count does not fit u32"))?,
            has_transform: node.transform.is_some(),
            has_mesh_renderer: node.mesh_renderer.is_some(),
            has_skinned_mesh_renderer: node.skinned_mesh_renderer.is_some(),
            has_animator: node.animator.is_some(),
        });
    }
    Ok(nodes)
}

fn live2d_package_limits(
    maximum_file_bytes: Option<i64>,
    maximum_total_bytes: Option<i64>,
) -> Result<(Live2dPackageLimits, Live2dPackageMaterializeLimits)> {
    let maximum_file_bytes = non_negative_limit(
        maximum_file_bytes,
        DEFAULT_PAYLOAD_LIMIT,
        "maximumFileBytes",
    )?;
    let maximum_total_bytes = non_negative_limit(
        maximum_total_bytes,
        DEFAULT_LIVE2D_TOTAL_LIMIT,
        "maximumTotalBytes",
    )?;
    let texture = TextureReadLimits {
        maximum_output_bytes: maximum_file_bytes,
        ..TextureReadLimits::default()
    };
    let planning = Live2dPackageLimits {
        maximum_total_moc_bytes: maximum_total_bytes,
        maximum_total_texture_payload_bytes: maximum_total_bytes,
        maximum_total_manifest_bytes: maximum_total_bytes,
        texture,
        ..Live2dPackageLimits::default()
    };
    let materialize = Live2dPackageMaterializeLimits {
        maximum_file_bytes,
        maximum_total_bytes,
        texture,
        motion_target_index: CubismMotionTargetIndexLimits::default(),
    };
    Ok((planning, materialize))
}

fn convert_live2d_package_set(set: CoreLive2dPackageBytesSet) -> Result<Live2dPackageSet> {
    let mut packages = reserve(set.packages.len(), "Live2D packages")?;
    for package in set.packages {
        let auxiliary_count = usize::from(package.physics.is_some())
            .checked_add(usize::from(package.pose.is_some()))
            .and_then(|count| count.checked_add(usize::from(package.display_info.is_some())))
            .ok_or_else(|| invalid_arg("Live2D auxiliary file count overflowed"))?;
        let file_count = 2_usize
            .checked_add(package.textures.len())
            .and_then(|count| count.checked_add(package.expressions.len()))
            .and_then(|count| count.checked_add(package.motions.len()))
            .and_then(|count| count.checked_add(auxiliary_count))
            .ok_or_else(|| invalid_arg("Live2D package file count overflowed"))?;
        let mut files = reserve(file_count, "Live2D package files")?;
        files.push(Live2dFile {
            file_name: package.moc_file_name,
            data: package.moc.into(),
        });
        files.push(Live2dFile {
            file_name: package.manifest_file_name,
            data: package.manifest.into(),
        });
        for texture in package.textures {
            files.push(Live2dFile {
                file_name: texture.file_name,
                data: texture.png.into(),
            });
        }
        for expression in package.expressions {
            files.push(Live2dFile {
                file_name: expression.file_name,
                data: expression.json.into(),
            });
        }
        for motion in package.motions {
            files.push(Live2dFile {
                file_name: motion.file_name,
                data: motion.json.into(),
            });
        }
        for auxiliary in [package.physics, package.pose, package.display_info]
            .into_iter()
            .flatten()
        {
            files.push(Live2dFile {
                file_name: auxiliary.file_name,
                data: auxiliary.bytes.into(),
            });
        }
        debug_assert_eq!(files.len(), file_count);
        packages.push(Live2dPackageFiles {
            name: package.name,
            directory_name: package.directory_name,
            files,
        });
    }
    let mut diagnostics = reserve(set.diagnostics.len(), "Live2D diagnostics")?;
    for diagnostic in set.diagnostics {
        diagnostics.push(Live2dDiagnostic {
            file_index: count_u32(diagnostic.object.file_index, "Live2D diagnostic file index")?,
            path_id: BigInt::from(diagnostic.object.path_id),
            kind: format!("{:?}", diagnostic.kind),
            detail: diagnostic.message,
        });
    }
    Ok(Live2dPackageSet {
        packages,
        diagnostics,
    })
}

/// Converts caller-supplied schema descriptions into a lookup registry.
///
/// The raw arrays stay in JavaScript until their counts have passed the
/// binding budgets. This is intentional: napi-rs' automatic `Vec<T>` input
/// conversion allocates with `Vec::with_capacity` before this function could
/// inspect either the schema or node count.
fn build_schema_registry(env: Env, schemas: Array<'_>) -> Result<MonoBehaviourSchemaRegistry> {
    parse_schema_entries(env, schemas)?.into_registry()
}

#[derive(Default)]
struct PendingMonoBehaviourSchemas {
    entries: Vec<MonoBehaviourSchemaEntry>,
}

impl PendingMonoBehaviourSchemas {
    fn into_registry(self) -> Result<MonoBehaviourSchemaRegistry> {
        let mut registry = MonoBehaviourSchemaRegistry::new();
        for entry in self.entries {
            registry.push(entry).map_err(core_error)?;
        }
        Ok(registry)
    }
}

fn parse_schema_entries(env: Env, schemas: Array<'_>) -> Result<PendingMonoBehaviourSchemas> {
    let schema_count = usize::try_from(schemas.len()).expect("u32 fits usize");
    if schema_count > MAXIMUM_SCHEMA_ENTRIES {
        return Err(invalid_arg(format!(
            "MonoBehaviour schema collection has {schema_count} entries, exceeding limit {MAXIMUM_SCHEMA_ENTRIES}"
        )));
    }
    let mut entries = reserve(schema_count, "MonoBehaviour schema entries")?;
    let mut budget = JsSchemaBudget::default();
    let raw_env = env.raw();
    for schema_index in 0..schemas.len() {
        let owner = format!("MonoBehaviour schema {schema_index}");
        let schema: Object<'_> = schemas
            .get(schema_index)?
            .ok_or_else(|| invalid_arg(format!("{owner} is missing")))?;
        entries.push(parse_schema_entry(raw_env, &schema, &owner, &mut budget)?);
    }
    Ok(PendingMonoBehaviourSchemas { entries })
}

#[derive(Default)]
struct JsSchemaBudget {
    nodes: usize,
    string_bytes: usize,
}

fn parse_schema_entry(
    env: napi::sys::napi_env,
    schema: &Object<'_>,
    owner: &str,
    budget: &mut JsSchemaBudget,
) -> Result<MonoBehaviourSchemaEntry> {
    let assembly_name: JsString<'_> = required_object_field(schema, "assemblyName", owner)?;
    let class_name: JsString<'_> = required_object_field(schema, "className", owner)?;
    let namespace: Option<JsString<'_>> = optional_object_field(schema, "namespace")?;
    let unity_version: Option<JsString<'_>> = optional_object_field(schema, "unityVersion")?;
    let node_values: Array<'_> = required_object_field(schema, "nodes", owner)?;
    charge_schema_nodes(node_values.len(), owner, budget)?;

    let assembly_name = copy_schema_string(
        env,
        assembly_name,
        budget,
        "MonoBehaviour schema assemblyName",
    )?;
    let class_name = copy_schema_string(env, class_name, budget, "MonoBehaviour schema className")?;
    let namespace = namespace
        .map(|value| copy_schema_string(env, value, budget, "MonoBehaviour schema namespace"))
        .transpose()?
        .unwrap_or_default();
    let unity_version = unity_version
        .map(|value| copy_schema_string(env, value, budget, "MonoBehaviour schema unityVersion"))
        .transpose()?;
    let nodes = parse_schema_nodes(env, node_values, owner, budget)?;
    Ok(MonoBehaviourSchemaEntry {
        assembly_name,
        namespace,
        class_name,
        unity_version,
        tree: TypeTree {
            nodes,
            string_buffer: Vec::new(),
        },
    })
}

fn charge_schema_nodes(count: u32, owner: &str, budget: &mut JsSchemaBudget) -> Result<()> {
    let count = usize::try_from(count).expect("u32 fits usize");
    if count == 0 {
        return Err(invalid_arg("a MonoBehaviour schema needs a root node"));
    }
    if count > MAXIMUM_SCHEMA_NODES_PER_ENTRY {
        return Err(invalid_arg(format!(
            "{owner} has {count} nodes; the maximum is {MAXIMUM_SCHEMA_NODES_PER_ENTRY}"
        )));
    }
    budget.nodes = budget
        .nodes
        .checked_add(count)
        .ok_or_else(|| invalid_arg("MonoBehaviour schema node count overflowed"))?;
    if budget.nodes > MAXIMUM_TOTAL_SCHEMA_NODES {
        return Err(invalid_arg(format!(
            "MonoBehaviour schemas contain {} nodes, exceeding limit {MAXIMUM_TOTAL_SCHEMA_NODES}",
            budget.nodes
        )));
    }
    Ok(())
}

fn parse_schema_nodes(
    env: napi::sys::napi_env,
    values: Array<'_>,
    owner: &str,
    budget: &mut JsSchemaBudget,
) -> Result<Vec<TypeTreeNode>> {
    let count = usize::try_from(values.len()).expect("u32 fits usize");
    let mut nodes = reserve(count, "MonoBehaviour schema nodes")?;
    for index in 0..values.len() {
        let node_owner = format!("{owner} node {index}");
        let node: Object<'_> = values
            .get(index)?
            .ok_or_else(|| invalid_arg(format!("{node_owner} is missing")))?;
        let type_name: JsString<'_> = required_object_field(&node, "typeName", &node_owner)?;
        let field_name: JsString<'_> = required_object_field(&node, "fieldName", &node_owner)?;
        let level: u32 = required_object_field(&node, "level", &node_owner)?;
        let align: bool = required_object_field(&node, "align", &node_owner)?;
        nodes.push(TypeTreeNode {
            type_name: copy_schema_string(
                env,
                type_name,
                budget,
                "MonoBehaviour schema node typeName",
            )?,
            field_name: copy_schema_string(
                env,
                field_name,
                budget,
                "MonoBehaviour schema node fieldName",
            )?,
            byte_size: -1,
            index: i32::try_from(index)
                .map_err(|_| invalid_arg("schema node index exceeds i32"))?,
            type_flags: 0,
            version: 1,
            meta_flags: if align { 0x4000 } else { 0 },
            level,
            type_string_offset: None,
            name_string_offset: None,
            reference_type_hash: 0,
        });
    }
    Ok(nodes)
}

fn copy_schema_string(
    env: napi::sys::napi_env,
    value: JsString<'_>,
    budget: &mut JsSchemaBudget,
    field: &'static str,
) -> Result<String> {
    let length = value.utf8_len()?;
    budget.string_bytes = budget
        .string_bytes
        .checked_add(length)
        .ok_or_else(|| invalid_arg("MonoBehaviour schema strings overflowed"))?;
    if budget.string_bytes > MAXIMUM_SCHEMA_STRING_BYTES {
        return Err(invalid_arg(format!(
            "MonoBehaviour schema strings exceed {MAXIMUM_SCHEMA_STRING_BYTES} bytes"
        )));
    }
    copy_js_string(env, value, length, field)
}

/// Settings objects are small; one budget covers the payload and its strings.
fn settings_limits(maximum: u64) -> ProjectSettingsReadLimits {
    ProjectSettingsReadLimits {
        maximum_object_bytes: maximum,
        ..ProjectSettingsReadLimits::default()
    }
}

fn cubism_auxiliary_limits(maximum: u64) -> Result<CubismAuxiliaryReadLimits> {
    let maximum_usize = usize::try_from(maximum)
        .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?;
    let defaults = CubismAuxiliaryReadLimits::default();
    Ok(CubismAuxiliaryReadLimits {
        maximum_object_bytes: defaults.maximum_object_bytes.min(maximum),
        maximum_string_bytes: defaults.maximum_string_bytes.min(maximum_usize),
        maximum_total_string_bytes: defaults.maximum_total_string_bytes.min(maximum_usize),
        ..defaults
    })
}

fn cubism_motion_targets(
    targets: Option<Object<'_>>,
    limits: &CubismClipMotionReadLimits,
) -> Result<CubismMotionTargetNames> {
    let Some(targets) = targets else {
        return Ok(CubismMotionTargetNames::default());
    };
    let env = targets.value().env;
    let parameters: Option<Array<'_>> = optional_object_field(&targets, "parameters")?;
    let parts: Option<Array<'_>> = optional_object_field(&targets, "parts")?;
    let parameter_count = parameters.as_ref().map_or(0_u32, Array::len);
    let part_count = parts.as_ref().map_or(0_u32, Array::len);
    let total_count = usize::try_from(parameter_count)
        .expect("u32 fits usize")
        .checked_add(usize::try_from(part_count).expect("u32 fits usize"))
        .ok_or_else(|| invalid_arg("Cubism motion target count overflowed"))?;
    if total_count > limits.maximum_curves {
        return Err(invalid_arg(format!(
            "Cubism motion targets contain {total_count} names, exceeding limit {}",
            limits.maximum_curves
        )));
    }
    let mut total_string_bytes = 0_usize;
    Ok(CubismMotionTargetNames {
        parameters: copy_js_string_array(
            env,
            parameters,
            &mut total_string_bytes,
            limits,
            "Cubism parameter target",
        )?,
        parts: copy_js_string_array(
            env,
            parts,
            &mut total_string_bytes,
            limits,
            "Cubism part target",
        )?,
    })
}

fn copy_js_string_array(
    env: napi::sys::napi_env,
    values: Option<Array<'_>>,
    total_string_bytes: &mut usize,
    limits: &CubismClipMotionReadLimits,
    field: &'static str,
) -> Result<Vec<String>> {
    let Some(values) = values else {
        return Ok(Vec::new());
    };
    let count = usize::try_from(values.len()).expect("u32 fits usize");
    let mut copied = reserve(count, field)?;
    for index in 0..values.len() {
        let value: JsString<'_> = values
            .get(index)?
            .ok_or_else(|| invalid_arg(format!("{field} is missing index {index}")))?;
        let length = value.utf8_len()?;
        if length > limits.maximum_string_bytes {
            return Err(invalid_arg(format!(
                "{field} has {length} bytes, exceeding limit {}",
                limits.maximum_string_bytes
            )));
        }
        *total_string_bytes = total_string_bytes
            .checked_add(length)
            .ok_or_else(|| invalid_arg("Cubism motion target string bytes overflowed"))?;
        if *total_string_bytes > limits.maximum_total_string_bytes {
            return Err(invalid_arg(format!(
                "Cubism motion target strings exceed {} bytes",
                limits.maximum_total_string_bytes
            )));
        }
        copied.push(copy_js_string(env, value, length, field)?);
    }
    Ok(copied)
}

fn cubism_clip_motion_limits(maximum: u64) -> Result<CubismClipMotionReadLimits> {
    let maximum_usize = usize::try_from(maximum)
        .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?;
    let defaults = CubismClipMotionReadLimits::default();
    Ok(CubismClipMotionReadLimits {
        maximum_string_bytes: defaults.maximum_string_bytes.min(maximum_usize),
        maximum_total_string_bytes: defaults.maximum_total_string_bytes.min(maximum_usize),
        maximum_output_bytes: maximum,
        clip: AnimationClipReadLimits {
            maximum_object_bytes: defaults.clip.maximum_object_bytes.min(maximum),
            maximum_string_bytes: defaults.clip.maximum_string_bytes.min(maximum_usize),
            maximum_total_string_bytes: defaults.clip.maximum_total_string_bytes.min(maximum_usize),
            maximum_total_allocation_bytes: defaults
                .clip
                .maximum_total_allocation_bytes
                .min(maximum),
            ..defaults.clip
        },
        ..defaults
    })
}

pub struct CubismClipMotionOutput {
    file_index: u32,
    path_id: i64,
    name: String,
    duration: f64,
    fps: f64,
    curve_count: u32,
    keyframe_count: u32,
    event_count: u32,
    json: Vec<u8>,
}

impl CubismClipMotionOutput {
    fn into_js(self) -> CubismClipMotion {
        CubismClipMotion {
            file_index: self.file_index,
            path_id: self.path_id.into(),
            name: self.name,
            duration: self.duration,
            fps: self.fps,
            curve_count: self.curve_count,
            keyframe_count: self.keyframe_count,
            event_count: self.event_count,
            json: self.json.into(),
        }
    }
}

fn build_cubism_clip_motion(
    motion: unity_rs_core::live2d_clip_motion::CubismClipMotion,
    force_bezier: bool,
    maximum: u64,
) -> Result<CubismClipMotionOutput> {
    let keyframe_count = motion.curves.iter().try_fold(0_usize, |total, curve| {
        total.checked_add(curve.keyframes.len())
    });
    let keyframe_count = keyframe_count
        .ok_or_else(|| invalid_arg("Cubism clip-motion keyframe count overflowed"))?;
    let mut json = reserve(
        usize::try_from(maximum.min(64 * 1024)).expect("64 KiB fits usize"),
        "Cubism clip-motion JSON",
    )?;
    motion
        .write_motion3_json(force_bezier, &mut json, maximum)
        .map_err(core_error)?;
    Ok(CubismClipMotionOutput {
        file_index: count_u32(motion.object.file_index, "Cubism clip-motion file index")?,
        path_id: motion.object.path_id,
        name: motion.name,
        duration: f64::from(motion.duration),
        fps: f64::from(motion.fps),
        curve_count: count_u32(motion.curves.len(), "Cubism clip-motion curve count")?,
        keyframe_count: count_u32(keyframe_count, "Cubism clip-motion keyframe count")?,
        event_count: count_u32(motion.events.len(), "Cubism clip-motion event count")?,
        json,
    })
}

/// The names of one material property sheet, in serialized order.
fn property_names<T>(
    properties: &[unity_rs_core::material::NamedMaterialProperty<T>],
) -> Result<Vec<String>> {
    let mut names = reserve(properties.len(), "material property names")?;
    for property in properties {
        names.push(copy_string(&property.name, "material property name")?);
    }
    Ok(names)
}

fn texture_limits(maximum: u64) -> TextureReadLimits {
    TextureReadLimits {
        maximum_payload_bytes: maximum,
        maximum_output_bytes: maximum,
        maximum_decoder_working_bytes: maximum,
        ..TextureReadLimits::default()
    }
}

fn texture_array_limits(maximum: u64) -> TextureArrayReadLimits {
    TextureArrayReadLimits {
        maximum_payload_bytes: maximum,
        maximum_output_bytes: maximum,
        maximum_decoder_working_bytes: maximum,
        maximum_bundle_bytes: maximum,
        ..TextureArrayReadLimits::default()
    }
}

fn sprite_limits(maximum: u64) -> SpriteReadLimits {
    SpriteReadLimits {
        maximum_mesh_bytes: maximum,
        maximum_output_pixels: maximum / 4,
        maximum_output_bytes: maximum,
        maximum_working_bytes: maximum,
        maximum_raster_operations: maximum,
        ..SpriteReadLimits::default()
    }
}

fn sprite_atlas_limits(
    maximum_entries: Option<u32>,
    maximum_string_bytes: Option<i64>,
    maximum_total_string_bytes: Option<i64>,
) -> Result<SpriteAtlasReadLimits> {
    let defaults = SpriteAtlasReadLimits::default();
    let maximum_entries = count_limit(maximum_entries, defaults.maximum_render_data_entries);
    Ok(SpriteAtlasReadLimits {
        maximum_string_bytes: usize_non_negative_limit(
            maximum_string_bytes,
            defaults.maximum_string_bytes,
            "maximumStringBytes",
        )?,
        maximum_total_string_bytes: usize_non_negative_limit(
            maximum_total_string_bytes,
            defaults.maximum_total_string_bytes,
            "maximumTotalStringBytes",
        )?,
        maximum_packed_sprites: maximum_entries,
        maximum_packed_sprite_names: maximum_entries,
        maximum_render_data_entries: maximum_entries,
        maximum_secondary_textures: maximum_entries,
    })
}

fn sprite_metadata_limits(options: Option<SpriteMetadataLimits>) -> Result<SpriteReadLimits> {
    let defaults = SpriteReadLimits::default();
    let Some(options) = options else {
        return Ok(defaults);
    };
    Ok(SpriteReadLimits {
        maximum_string_bytes: usize_non_negative_limit(
            options.maximum_string_bytes,
            defaults.maximum_string_bytes,
            "maximumStringBytes",
        )?,
        maximum_total_string_bytes: usize_non_negative_limit(
            options.maximum_total_string_bytes,
            defaults.maximum_total_string_bytes,
            "maximumTotalStringBytes",
        )?,
        maximum_array_elements: count_limit(
            options.maximum_entries,
            defaults.maximum_array_elements,
        ),
        maximum_mesh_bytes: non_negative_limit(
            options.maximum_mesh_bytes,
            defaults.maximum_mesh_bytes,
            "maximumMeshBytes",
        )?,
        ..defaults
    })
}

fn convert_sprite_atlas(atlas: SpriteAtlas) -> Result<SpriteAtlasInfo> {
    let mut packed_sprites = reserve(atlas.packed_sprites.len(), "SpriteAtlas packed sprites")?;
    for reference in atlas.packed_sprites {
        packed_sprites.push(object_reference(reference));
    }

    let mut render_data_entries = reserve(
        atlas.render_data_entries.len(),
        "SpriteAtlas render-data entries",
    )?;
    for entry in atlas.render_data_entries {
        let mut guid_bytes = reserve(16, "SpriteAtlas GUID bytes")?;
        guid_bytes.extend_from_slice(&entry.key.guid_bytes);
        let settings = entry.data.settings;
        let secondary_textures = entry
            .data
            .secondary_textures
            .map(|textures| {
                let mut output = reserve(textures.len(), "SpriteAtlas secondary textures")?;
                for texture in textures {
                    output.push(SpriteAtlasSecondaryTexture {
                        texture: object_reference(texture.texture),
                        name: texture.name,
                    });
                }
                Ok::<_, Error>(output)
            })
            .transpose()?;
        render_data_entries.push(SpriteAtlasRenderData {
            key: SpriteAtlasRenderDataKey {
                guid_bytes: guid_bytes.into(),
                value: BigInt::from(entry.key.value),
            },
            texture: object_reference(entry.data.texture),
            alpha_texture: object_reference(entry.data.alpha_texture),
            texture_rect: SpriteAtlasRect {
                x: f64::from(entry.data.texture_rect.x),
                y: f64::from(entry.data.texture_rect.y),
                width: f64::from(entry.data.texture_rect.width),
                height: f64::from(entry.data.texture_rect.height),
            },
            texture_rect_offset: SpriteAtlasVector2 {
                x: f64::from(entry.data.texture_rect_offset.x),
                y: f64::from(entry.data.texture_rect_offset.y),
            },
            atlas_rect_offset: SpriteAtlasVector2 {
                x: f64::from(entry.data.atlas_rect_offset.x),
                y: f64::from(entry.data.atlas_rect_offset.y),
            },
            uv_transform: SpriteAtlasVector4 {
                x: f64::from(entry.data.uv_transform.x),
                y: f64::from(entry.data.uv_transform.y),
                z: f64::from(entry.data.uv_transform.z),
                w: f64::from(entry.data.uv_transform.w),
            },
            downscale_multiplier: f64::from(entry.data.downscale_multiplier),
            settings: SpriteAtlasSettings {
                raw: settings.raw,
                packed: settings.packed(),
                packing_mode: u32::from(settings.packing_mode()),
                packing_rotation: u32::from(settings.packing_rotation()),
                mesh_type: u32::from(settings.mesh_type()),
            },
            secondary_textures,
        });
    }

    Ok(SpriteAtlasInfo {
        path_id: BigInt::from(atlas.path_id),
        name: atlas.name,
        packed_sprites,
        packed_sprite_names: atlas.packed_sprite_names,
        render_data_entries,
        tag: atlas.tag,
        is_variant: atlas.is_variant,
    })
}

fn convert_sprite_metadata(sprite: Sprite) -> Result<SpriteMetadata> {
    let Sprite {
        object_index,
        path_id,
        name,
        rect,
        offset,
        border,
        pixels_to_units,
        pivot,
        extrude,
        is_polygon,
        render_data_key,
        atlas_tags,
        sprite_atlas,
        render_data,
    } = sprite;
    let render_data_key = render_data_key
        .map(|(guid_bytes, value)| {
            let mut bytes = reserve(16, "Sprite GUID bytes")?;
            bytes.extend_from_slice(&guid_bytes);
            Ok::<_, Error>(SpriteAtlasRenderDataKey {
                guid_bytes: bytes.into(),
                value: BigInt::from(value),
            })
        })
        .transpose()?;
    Ok(SpriteMetadata {
        object_index: count_u32(object_index, "Sprite object index")?,
        path_id: BigInt::from(path_id),
        name,
        rect: SpriteRect {
            x: f64::from(rect.x),
            y: f64::from(rect.y),
            width: f64::from(rect.width),
            height: f64::from(rect.height),
        },
        offset: sprite_vector2(offset),
        border: SpriteVector4 {
            x: f64::from(border.x),
            y: f64::from(border.y),
            z: f64::from(border.z),
            w: f64::from(border.w),
        },
        pixels_to_units: f64::from(pixels_to_units),
        pivot: sprite_vector2(pivot),
        extrude,
        is_polygon,
        render_data_key,
        atlas_tags,
        sprite_atlas: sprite_object_reference(sprite_atlas),
        render_data: convert_sprite_render_data(render_data)?,
    })
}

fn convert_sprite_render_data(
    render_data: unity_rs_core::sprite::SpriteRenderData,
) -> Result<SpriteRenderData> {
    let unity_rs_core::sprite::SpriteRenderData {
        texture,
        alpha_texture,
        secondary_textures,
        texture_rect,
        texture_rect_offset,
        atlas_rect_offset,
        settings,
        uv_transform,
        downscale_multiplier,
        mesh_triangles,
    } = render_data;
    let mut node_secondary = reserve(secondary_textures.len(), "Sprite secondary textures")?;
    for secondary in secondary_textures {
        node_secondary.push(SpriteSecondaryTexture {
            texture: sprite_object_reference(secondary.texture),
            name: secondary.name,
        });
    }
    let mut node_triangles = reserve(mesh_triangles.len(), "Sprite mesh triangles")?;
    for [first, second, third] in mesh_triangles {
        node_triangles.push(SpriteTriangle {
            first: sprite_vector2(first),
            second: sprite_vector2(second),
            third: sprite_vector2(third),
        });
    }
    Ok(SpriteRenderData {
        texture: sprite_object_reference(texture),
        alpha_texture: sprite_object_reference(alpha_texture),
        secondary_textures: node_secondary,
        texture_rect: SpriteRect {
            x: f64::from(texture_rect.x),
            y: f64::from(texture_rect.y),
            width: f64::from(texture_rect.width),
            height: f64::from(texture_rect.height),
        },
        texture_rect_offset: sprite_vector2(texture_rect_offset),
        atlas_rect_offset: sprite_vector2(atlas_rect_offset),
        settings: SpriteSettings {
            raw: settings.raw,
            packed: settings.packed,
            packing_mode: match settings.packing_mode {
                SpritePackingMode::Tight => "tight",
                SpritePackingMode::Rectangle => "rectangle",
            }
            .to_owned(),
            packing_rotation: u32::from(settings.packing_rotation),
            mesh_type: match settings.mesh_type {
                SpriteMeshType::FullRect => "full_rect",
                SpriteMeshType::Tight => "tight",
            }
            .to_owned(),
        },
        uv_transform: SpriteVector4 {
            x: f64::from(uv_transform.x),
            y: f64::from(uv_transform.y),
            z: f64::from(uv_transform.z),
            w: f64::from(uv_transform.w),
        },
        downscale_multiplier: f64::from(downscale_multiplier),
        mesh_triangles: node_triangles,
    })
}

fn sprite_vector2(value: unity_rs_core::sprite::Vector2) -> SpriteVector2 {
    SpriteVector2 {
        x: f64::from(value.x),
        y: f64::from(value.y),
    }
}

/// Converts an image Core already returns in display row order, such as a
/// decoded `Sprite`.
fn convert_image(image: unity_rs_core::texture::RgbaImage) -> RgbaImage {
    RgbaImage {
        width: image.width,
        height: image.height,
        pixels: image.pixels.into(),
    }
}

/// A decoded texture whose pixels already use the top-down row order exposed
/// to JavaScript. Keeping this as the asynchronous task output makes the
/// worker/event-loop boundary explicit: `resolve` can only wrap the owned
/// bytes in a Node `Buffer`; it cannot accidentally inherit the O(pixel bytes)
/// row flip again.
#[doc(hidden)]
pub struct DisplayRowImage(unity_rs_core::texture::RgbaImage);

impl DisplayRowImage {
    fn from_decoded(mut image: unity_rs_core::texture::RgbaImage) -> Result<Self> {
        unity_rs_core::image_export::flip_rgba_rows(&mut image).map_err(core_error)?;
        Ok(Self(image))
    }

    fn into_node(self) -> RgbaImage {
        convert_image(self.0)
    }
}

/// The same worker-completed row-order invariant for a `Texture2DArray`.
#[doc(hidden)]
pub struct DisplayRowImages(Vec<RgbaImage>);

impl DisplayRowImages {
    fn from_decoded(images: Vec<unity_rs_core::texture::RgbaImage>) -> Result<Self> {
        let mut output = reserve(images.len(), "Texture2DArray images")?;
        for mut image in images {
            unity_rs_core::image_export::flip_rgba_rows(&mut image).map_err(core_error)?;
            output.push(convert_image(image));
        }
        Ok(Self(output))
    }

    fn into_nodes(self) -> Vec<RgbaImage> {
        self.0
    }
}

/// Converts a `Texture2D` decoder result, whose rows run bottom-up, into the
/// top-down order every other surface hands to callers.
fn convert_decoded_image(image: unity_rs_core::texture::RgbaImage) -> Result<RgbaImage> {
    DisplayRowImage::from_decoded(image).map(DisplayRowImage::into_node)
}

fn convert_decoded_images(
    images: Vec<unity_rs_core::texture::RgbaImage>,
) -> Result<Vec<RgbaImage>> {
    DisplayRowImages::from_decoded(images).map(DisplayRowImages::into_nodes)
}

fn convert_animation_clip_info(
    mut clip: unity_rs_core::animation_clip::AnimationClip,
) -> Result<AnimationClipInfo> {
    let streaming_info = clip.streaming_info.take();
    let muscle = clip.muscle_clip.as_ref();
    let acl = muscle.and_then(|value| value.clip.acl.as_ref());
    let constant_value_count = muscle
        .map(|value| {
            count_u32(
                value.clip.constant.values.count,
                "AnimationClip constant value count",
            )
        })
        .transpose()?;
    let acl_decoder_count = acl
        .map(|value| count_u32(value.decoder_map.count, "AnimationClip ACL decoder count"))
        .transpose()?;
    let (streaming_offset, streaming_size, streaming_path) = match streaming_info {
        Some(value) => (
            Some(BigInt::from(value.offset)),
            Some(value.size),
            Some(value.path),
        ),
        None => (None, None, None),
    };
    Ok(AnimationClipInfo {
        path_id: BigInt::from(clip.path_id),
        name: clip.name,
        sample_rate: f64::from(clip.sample_rate),
        wrap_mode: clip.wrap_mode,
        legacy: clip.legacy,
        compressed: clip.compressed,
        use_high_quality_curve: clip.use_high_quality_curve,
        rotation_curve_count: count_u32(
            clip.rotation_curves.len(),
            "AnimationClip rotation curve count",
        )?,
        position_curve_count: count_u32(
            clip.position_curves.len(),
            "AnimationClip position curve count",
        )?,
        scale_curve_count: count_u32(clip.scale_curves.len(), "AnimationClip scale curve count")?,
        euler_curve_count: count_u32(clip.euler_curves.len(), "AnimationClip Euler curve count")?,
        float_curve_count: count_u32(clip.float_curves.len(), "AnimationClip float curve count")?,
        pptr_curve_count: count_u32(clip.pptr_curves.len(), "AnimationClip PPtr curve count")?,
        muscle_clip_size: clip.muscle_clip_size,
        has_muscle_clip: muscle.is_some(),
        streamed_curve_count: muscle.map(|value| value.clip.streamed.curve_count),
        dense_curve_count: muscle.map(|value| value.clip.dense.curve_count),
        constant_value_count,
        has_acl: acl.is_some(),
        acl_frame_count: acl.map(|value| value.frame_count),
        acl_bone_count: acl.map(|value| value.bone_count),
        acl_sample_rate: acl.map(|value| f64::from(value.sample_rate())),
        acl_curve_count: acl.and_then(|value| value.curve_count),
        acl_track_byte_count: acl.map(|value| BigInt::from(value.tracks.byte_length)),
        acl_decoder_count,
        acl_use_fast_sample_mode: acl.and_then(|value| value.use_fast_sample_mode),
        has_streaming_info: streaming_path.is_some(),
        streaming_offset,
        streaming_size,
        streaming_path,
    })
}

fn convert_avatar(avatar: unity_rs_core::avatar::Avatar) -> Result<Avatar> {
    let skeleton_node_count = count_u32(
        avatar.constant.avatar_skeleton.nodes.len(),
        "Avatar skeleton node count",
    )?;
    let human_skeleton_node_count = count_u32(
        avatar.constant.human.skeleton.nodes.len(),
        "Avatar human skeleton node count",
    )?;
    let (has_human_description, human_bone_count, skeleton_bone_count, root_motion_bone_name) =
        match avatar.human_description {
            Some(description) => (
                true,
                count_u32(description.human_bones.len(), "Avatar human bone count")?,
                count_u32(
                    description.skeleton_bones.len(),
                    "Avatar skeleton bone count",
                )?,
                Some(description.root_motion_bone_name),
            ),
            None => (false, 0, 0, None),
        };
    let path_count = count_u32(avatar.paths.len(), "Avatar path count")?;
    let mut paths = reserve(avatar.paths.len(), "Avatar paths")?;
    for entry in avatar.paths {
        paths.push(AvatarPathEntry {
            hash: entry.hash,
            path: entry.path,
        });
    }
    Ok(Avatar {
        path_id: BigInt::from(avatar.path_id),
        name: avatar.name,
        declared_size: avatar.declared_avatar_size,
        declared_avatar_size: avatar.declared_avatar_size,
        skeleton_node_count,
        human_skeleton_node_count,
        path_count,
        paths,
        has_human_description,
        human_bone_count,
        skeleton_bone_count,
        root_motion_bone_name,
    })
}

fn object_info(object: StudioObject<'_>) -> Result<ObjectInfo> {
    Ok(ObjectInfo {
        file_index: count_u32(object.file_index(), "object file index")?,
        object_index: count_u32(object.object_index(), "object index")?,
        source_path: copy_string(object.source_path(), "object source path")?,
        path_id: object.path_id().into(),
        class_id: object.class_id(),
        byte_size: object.byte_size().into(),
        name: object
            .name()
            .map(|value| copy_string(value, "object name"))
            .transpose()?,
        container: object
            .container()
            .map(|value| copy_string(value, "object container"))
            .transpose()?,
    })
}

fn page(offset: Option<u32>, limit: Option<u32>) -> Result<(usize, usize)> {
    let offset = usize::try_from(offset.unwrap_or(0)).expect("u32 fits usize");
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    if limit > MAXIMUM_PAGE_LIMIT {
        return Err(invalid_arg(format!(
            "page limit {limit} exceeds {MAXIMUM_PAGE_LIMIT}"
        )));
    }
    Ok((offset, usize::try_from(limit).expect("u32 fits usize")))
}

fn byte_limit(value: Option<i64>) -> Result<u64> {
    non_negative_limit(value, DEFAULT_PAYLOAD_LIMIT, "maximumBytes")
}

fn unsupported_option(field: &str, value: &str, expected: &str) -> Error {
    if value.len() <= MAXIMUM_OPTION_DIAGNOSTIC_BYTES {
        invalid_arg(format!(
            "unsupported {field} {value:?}; expected {expected}"
        ))
    } else {
        invalid_arg(format!(
            "unsupported {field} value of {} UTF-8 bytes; expected {expected}",
            value.len()
        ))
    }
}

fn parse_export_mode(value: Option<&str>) -> Result<ExportMode> {
    let Some(value) = value else {
        return Ok(ExportMode::Auto);
    };
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") {
        Ok(ExportMode::Auto)
    } else if value.eq_ignore_ascii_case("raw") {
        Ok(ExportMode::Raw)
    } else if value.eq_ignore_ascii_case("typetree-json")
        || value.eq_ignore_ascii_case("typetree_json")
        || value.eq_ignore_ascii_case("json")
    {
        Ok(ExportMode::TypeTreeJson)
    } else if value.eq_ignore_ascii_case("dump-text")
        || value.eq_ignore_ascii_case("dump_text")
        || value.eq_ignore_ascii_case("dump")
    {
        Ok(ExportMode::DumpText)
    } else {
        Err(unsupported_option(
            "export mode",
            value,
            "auto, raw, typetree-json, or dump-text",
        ))
    }
}

fn parse_filename_format(value: Option<&str>) -> Result<FilenameFormat> {
    let Some(value) = value else {
        return Ok(FilenameFormat::AssetName);
    };
    let value = value.trim();
    if value.eq_ignore_ascii_case("asset-name") || value.eq_ignore_ascii_case("asset_name") {
        Ok(FilenameFormat::AssetName)
    } else if value.eq_ignore_ascii_case("asset-name-path-id")
        || value.eq_ignore_ascii_case("asset_name_path_id")
    {
        Ok(FilenameFormat::AssetNamePathId)
    } else if value.eq_ignore_ascii_case("path-id") || value.eq_ignore_ascii_case("path_id") {
        Ok(FilenameFormat::PathId)
    } else {
        Err(unsupported_option(
            "filename format",
            value,
            "asset-name, asset-name-path-id, or path-id",
        ))
    }
}

fn parse_audio_format(value: &str) -> Result<AudioExportFormat> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") {
        Ok(AudioExportFormat::Auto)
    } else if value.eq_ignore_ascii_case("raw") || value.eq_ignore_ascii_case("none") {
        Ok(AudioExportFormat::Raw)
    } else if value.eq_ignore_ascii_case("wav") || value.eq_ignore_ascii_case("wave") {
        Ok(AudioExportFormat::Wav)
    } else {
        Err(unsupported_option(
            "audio format",
            value,
            "auto, raw, or wav",
        ))
    }
}

fn materialize_audio_clip(
    audio: AudioClipAsset,
    format: AudioExportFormat,
    maximum: u64,
) -> Result<AudioClip> {
    let AudioClipAsset {
        name,
        payload,
        raw_extension,
        direct_wav,
        ..
    } = audio;
    let is_direct_wav = direct_wav.is_some();
    let wav_kind = match format {
        AudioExportFormat::Auto => direct_wav,
        AudioExportFormat::Raw => None,
        AudioExportFormat::Wav => Some(direct_wav.ok_or_else(|| {
            Error::new(
                Status::GenericFailure,
                "AudioClip uses a compressed or unsupported audio codec and cannot be exported directly as WAV",
            )
        })?),
    };

    if let Some(kind) = wav_kind {
        let expected = direct_wav_output_size(&payload, kind).map_err(core_error)?;
        if expected > maximum {
            return Err(invalid_arg(format!(
                "WAV output is {expected} bytes, exceeding maximumBytes {maximum}"
            )));
        }
        let expected_usize = usize::try_from(expected)
            .map_err(|_| invalid_arg("WAV output is too large for this platform"))?;
        let mut data = reserve(expected_usize, "AudioClip WAV output")?;
        let written = write_direct_wav(&payload, kind, maximum, &mut data).map_err(core_error)?;
        if written != expected || data.len() != expected_usize {
            return Err(Error::from_reason(format!(
                "AudioClip WAV writer produced {written} bytes, expected {expected}"
            )));
        }
        Ok(AudioClip {
            name,
            extension: ".wav".to_owned(),
            payload_kind: "audio_wav".to_owned(),
            is_direct_wav,
            data: data.into(),
        })
    } else {
        let data = payload.read_to_vec(maximum).map_err(core_error)?;
        Ok(AudioClip {
            name,
            extension: raw_extension,
            payload_kind: "audio_raw".to_owned(),
            is_direct_wav,
            data: data.into(),
        })
    }
}

fn animation_clip_limits(maximum: u64) -> Result<AnimationClipReadLimits> {
    let maximum_usize = usize::try_from(maximum)
        .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?;
    let defaults = AnimationClipReadLimits::default();
    Ok(AnimationClipReadLimits {
        maximum_object_bytes: defaults.maximum_object_bytes.min(maximum),
        maximum_string_bytes: defaults.maximum_string_bytes.min(maximum_usize),
        maximum_total_string_bytes: defaults.maximum_total_string_bytes.min(maximum_usize),
        maximum_packed_bytes: defaults.maximum_packed_bytes.min(maximum),
        maximum_total_packed_bytes: defaults.maximum_total_packed_bytes.min(maximum),
        maximum_reference_bytes: defaults.maximum_reference_bytes.min(maximum),
        maximum_total_allocation_bytes: defaults.maximum_total_allocation_bytes.min(maximum),
        ..defaults
    })
}

fn avatar_limits(maximum: u64) -> Result<AvatarReadLimits> {
    let maximum_usize = usize::try_from(maximum)
        .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?;
    let defaults = AvatarReadLimits::default();
    Ok(AvatarReadLimits {
        maximum_object_bytes: defaults.maximum_object_bytes.min(maximum),
        maximum_string_bytes: defaults.maximum_string_bytes.min(maximum_usize),
        maximum_total_string_bytes: defaults.maximum_total_string_bytes.min(maximum_usize),
        maximum_total_allocation_bytes: defaults.maximum_total_allocation_bytes.min(maximum),
        maximum_reference_bytes: defaults.maximum_reference_bytes.min(maximum),
        ..defaults
    })
}

fn animation_component_limits(value: Option<i64>) -> Result<AnimationComponentReadLimits> {
    let maximum = byte_limit(value)?;
    let maximum_usize = usize::try_from(maximum)
        .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))?;
    let maximum_clips = maximum_usize / std::mem::size_of::<AnimationClipOverride>();
    Ok(AnimationComponentReadLimits {
        maximum_object_bytes: maximum,
        maximum_string_bytes: maximum_usize,
        maximum_clips: maximum_clips.min(AnimationComponentReadLimits::default().maximum_clips),
        maximum_reference_bytes: maximum,
    })
}

fn container_metadata_limits(
    maximum_entries: Option<u32>,
    maximum_string_bytes: Option<i64>,
    maximum_total_string_bytes: Option<i64>,
) -> Result<ContainerMetadataReadLimits> {
    let defaults = ContainerMetadataReadLimits::default();
    let maximum_entries = count_limit(maximum_entries, defaults.maximum_container_entries);
    Ok(ContainerMetadataReadLimits {
        maximum_preload_references: maximum_entries,
        maximum_container_entries: maximum_entries,
        maximum_dependencies: maximum_entries,
        maximum_class_version_entries: maximum_entries,
        maximum_string_bytes: usize_non_negative_limit(
            maximum_string_bytes,
            defaults.maximum_string_bytes,
            "maximumStringBytes",
        )?,
        maximum_total_string_bytes: usize_non_negative_limit(
            maximum_total_string_bytes,
            defaults.maximum_total_string_bytes,
            "maximumTotalStringBytes",
        )?,
    })
}

fn usize_non_negative_limit(value: Option<i64>, default: usize, field: &str) -> Result<usize> {
    let default = u64::try_from(default).expect("a usize limit fits u64");
    let value = non_negative_limit(value, default, field)?;
    usize::try_from(value).map_err(|_| invalid_arg(format!("{field} does not fit this platform")))
}

fn non_negative_limit(value: Option<i64>, default: u64, field: &str) -> Result<u64> {
    match value {
        None => Ok(default),
        Some(value) if value >= 0 => u64::try_from(value)
            .map_err(|_| invalid_arg(format!("{field} does not fit an unsigned 64-bit integer"))),
        Some(value) => Err(invalid_arg(format!(
            "{field} must be non-negative, received {value}"
        ))),
    }
}

fn parse_image_format(value: Option<String>) -> Result<ImageFormat> {
    let Some(value) = value else {
        return Ok(ImageFormat::Png);
    };
    let value = value.trim();
    if value.eq_ignore_ascii_case("jpg") || value.eq_ignore_ascii_case("jpeg") {
        Ok(ImageFormat::Jpeg)
    } else if value.eq_ignore_ascii_case("png") {
        Ok(ImageFormat::Png)
    } else if value.eq_ignore_ascii_case("bmp") {
        Ok(ImageFormat::Bmp)
    } else if value.eq_ignore_ascii_case("tga") {
        Ok(ImageFormat::Tga)
    } else if value.eq_ignore_ascii_case("webp") {
        Ok(ImageFormat::Webp)
    } else if value.eq_ignore_ascii_case("raw_rgba")
        || value.eq_ignore_ascii_case("raw-rgba")
        || value.eq_ignore_ascii_case("rgba")
    {
        Ok(ImageFormat::RawRgba)
    } else {
        Err(unsupported_option(
            "image format",
            value,
            "jpeg, png, bmp, tga, webp, or raw-rgba",
        ))
    }
}

fn parse_png_compression(value: Option<String>) -> Result<PngCompression> {
    let Some(value) = value else {
        return Ok(PngCompression::default());
    };
    let value = value.trim();
    if value.eq_ignore_ascii_case("fast") {
        Ok(PngCompression::Fast)
    } else if value.eq_ignore_ascii_case("default") {
        Ok(PngCompression::Default)
    } else if value.eq_ignore_ascii_case("best") {
        Ok(PngCompression::Best)
    } else {
        Err(unsupported_option(
            "PNG compression",
            value,
            "fast, default, or best",
        ))
    }
}

fn usize_limit(value: Option<i64>) -> Result<usize> {
    usize::try_from(byte_limit(value)?)
        .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))
}

fn count_u32(value: usize, field: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::from_reason(format!("{field} does not fit u32")))
}

fn object_reference(reference: unity_rs_core::serialized::ObjectReference) -> ObjectReference {
    ObjectReference {
        file_id: reference.file_id,
        path_id: BigInt::from(reference.path_id),
    }
}

fn sprite_object_reference(reference: unity_rs_core::sprite::ObjectReference) -> ObjectReference {
    ObjectReference {
        file_id: reference.file_id,
        path_id: BigInt::from(reference.path_id),
    }
}

/// A non-negative `BigInt`, for offsets and lengths.
fn bigint_u64(value: BigInt, field: &str) -> Result<u64> {
    let BigInt { sign_bit, words } = value;
    if sign_bit {
        return Err(invalid_arg(format!("{field} must not be negative")));
    }
    match words.len() {
        0 => Ok(0),
        1 => Ok(words[0]),
        _ => Err(invalid_arg(format!(
            "{field} does not fit unsigned 64 bits"
        ))),
    }
}

fn bigint_i64(value: BigInt, field: &str) -> Result<i64> {
    let BigInt { sign_bit, words } = value;
    if words.len() != 1 {
        return Err(invalid_arg(format!("{field} does not fit signed 64 bits")));
    }
    let magnitude = words[0];
    if sign_bit {
        if magnitude == i64::MIN.unsigned_abs() {
            Ok(i64::MIN)
        } else {
            i64::try_from(magnitude)
                .map(|number| -number)
                .map_err(|_| invalid_arg(format!("{field} does not fit signed 64 bits")))
        }
    } else {
        i64::try_from(magnitude)
            .map_err(|_| invalid_arg(format!("{field} does not fit signed 64 bits")))
    }
}

fn reserve<T>(count: usize, field: &str) -> Result<Vec<T>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|error| Error::from_reason(format!("cannot allocate {field}: {error}")))?;
    Ok(output)
}

fn copy_slice<T: Copy>(source: &[T], field: &str) -> Result<Vec<T>> {
    let mut output = reserve(source.len(), field)?;
    output.extend_from_slice(source);
    Ok(output)
}

fn copy_core_slice<T: Copy>(source: &[T], field: &'static str) -> unity_rs_core::Result<Vec<T>> {
    let mut output = Vec::new();
    output.try_reserve_exact(source.len()).map_err(|error| {
        unity_rs_core::Error::invalid_data(format!("cannot allocate {field}: {error}"))
    })?;
    output.extend_from_slice(source);
    Ok(output)
}

struct FallibleOutput {
    bytes: Vec<u8>,
    maximum: usize,
    field: &'static str,
    limit_exceeded: bool,
}

impl FallibleOutput {
    const fn new(maximum: usize, field: &'static str) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            field,
            limit_exceeded: false,
        }
    }
}

impl Write for FallibleOutput {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(input.len())
            .ok_or_else(|| io::Error::other(format!("{} length overflowed", self.field)))?;
        if next > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::other(format!(
                "{} exceeds {} bytes",
                self.field, self.maximum
            )));
        }
        self.bytes.try_reserve(input.len()).map_err(|error| {
            io::Error::other(format!("cannot allocate {}: {error}", self.field))
        })?;
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn materialize_core_output<T>(
    maximum: u64,
    field: &'static str,
    write: impl FnOnce(&mut FallibleOutput) -> unity_rs_core::Result<(u64, T)>,
) -> Result<(Vec<u8>, T)> {
    let maximum = usize::try_from(maximum)
        .map_err(|_| invalid_arg(format!("{field} limit does not fit this platform")))?;
    let mut output = FallibleOutput::new(maximum, field);
    let write_result = write(&mut output);
    if output.limit_exceeded {
        return Err(invalid_arg(format!("{field} exceeds {maximum} bytes")));
    }
    let (written, value) = write_result.map_err(core_error)?;
    let actual = u64::try_from(output.bytes.len())
        .map_err(|_| invalid_arg(format!("{field} length does not fit u64")))?;
    if written != actual {
        return Err(Error::from_reason(format!(
            "{field} writer reported {written} bytes but produced {actual}"
        )));
    }
    Ok((output.bytes, value))
}

fn materialize_core_bytes(
    maximum: u64,
    field: &'static str,
    write: impl FnOnce(&mut FallibleOutput) -> unity_rs_core::Result<u64>,
) -> Result<Vec<u8>> {
    materialize_core_output(maximum, field, |output| {
        write(output).map(|written| (written, ()))
    })
    .map(|(bytes, ())| bytes)
}

fn copy_string(value: &str, field: &str) -> Result<String> {
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|error| Error::from_reason(format!("cannot allocate {field}: {error}")))?;
    output.push_str(value);
    Ok(output)
}

#[cfg(windows)]
fn for_each_path_char_lossy(
    value: &std::ffi::OsStr,
    mut visitor: impl FnMut(char) -> Result<()>,
) -> Result<()> {
    use std::char::decode_utf16;
    use std::os::windows::ffi::OsStrExt;

    for character in decode_utf16(value.encode_wide()) {
        visitor(character.unwrap_or(char::REPLACEMENT_CHARACTER))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn for_each_path_char_lossy(
    value: &std::ffi::OsStr,
    mut visitor: impl FnMut(char) -> Result<()>,
) -> Result<()> {
    let mut input = value.as_encoded_bytes();
    while !input.is_empty() {
        match std::str::from_utf8(input) {
            Ok(valid) => {
                for character in valid.chars() {
                    visitor(character)?;
                }
                return Ok(());
            }
            Err(error) => {
                let valid_length = error.valid_up_to();
                if valid_length != 0 {
                    let valid = std::str::from_utf8(&input[..valid_length]).map_err(|_| {
                        invalid_arg("valid filesystem UTF-8 prefix could not be decoded")
                    })?;
                    for character in valid.chars() {
                        visitor(character)?;
                    }
                }
                visitor(char::REPLACEMENT_CHARACTER)?;
                let invalid_length = error
                    .error_len()
                    .unwrap_or_else(|| input.len() - valid_length);
                input = &input[valid_length + invalid_length..];
            }
        }
    }
    Ok(())
}

fn path_lossy_utf8_length(value: &std::ffi::OsStr, field: &'static str) -> Result<usize> {
    let mut length = 0_usize;
    for_each_path_char_lossy(value, |character| {
        length = length
            .checked_add(character.len_utf8())
            .ok_or_else(|| invalid_arg(format!("{field} replacement length overflowed")))?;
        Ok(())
    })?;
    Ok(length)
}

fn copy_path_string(path: &std::path::Path, field: &'static str) -> Result<String> {
    let value = path.as_os_str();
    let utf8_length = path_lossy_utf8_length(value, field)?;
    let mut output = String::new();
    output
        .try_reserve_exact(utf8_length)
        .map_err(|error| Error::from_reason(format!("cannot allocate {field}: {error}")))?;
    for_each_path_char_lossy(value, |character| {
        output.push(character);
        Ok(())
    })?;
    if output.len() != utf8_length {
        return Err(Error::from_reason(format!(
            "{field} changed while converting the filesystem path"
        )));
    }
    Ok(output)
}

fn convert_fbx_candidates(
    source: Vec<ModelExportCandidate>,
    field: &'static str,
) -> Result<Vec<FbxCandidate>> {
    let mut output = reserve(source.len(), field)?;
    output.extend(source.into_iter().map(into_candidate));
    Ok(output)
}

fn convert_scene_texture_files(
    source: Vec<unity_rs_core::scene_textures::SceneTexture>,
    field: &'static str,
) -> Result<Vec<Live2dFile>> {
    let mut output = reserve(source.len(), field)?;
    for texture in source {
        output.push(Live2dFile {
            file_name: texture.file_name,
            data: texture.encoded.into(),
        });
    }
    Ok(output)
}

fn convert_scene_texture_skips(
    source: Vec<unity_rs_core::scene_textures::SceneTextureSkip>,
    field: &'static str,
) -> Result<Vec<String>> {
    let mut output = reserve(source.len(), field)?;
    for skip in source {
        let capacity = skip
            .property
            .len()
            .checked_add(skip.reason.len())
            .and_then(|length| length.checked_add(2))
            .ok_or_else(|| invalid_arg("skipped texture description length overflowed"))?;
        let mut description = String::new();
        description.try_reserve_exact(capacity).map_err(|error| {
            Error::from_reason(format!(
                "cannot allocate skipped texture description: {error}"
            ))
        })?;
        write!(description, "{}: {}", skip.property, skip.reason)
            .map_err(|error| Error::from_reason(format!("cannot format texture skip: {error}")))?;
        output.push(description);
    }
    Ok(output)
}

fn invalid_arg(message: impl Into<String>) -> Error {
    Error::new(Status::InvalidArg, message.into())
}

/// Converts a Core export candidate into the shape JavaScript receives.
fn into_candidate(candidate: ModelExportCandidate) -> FbxCandidate {
    FbxCandidate {
        file_index: u32::try_from(candidate.game_object.file_index).expect("a file index fits u32"),
        path_id: BigInt::from(candidate.game_object.path_id),
        animator_file_index: candidate
            .animator
            .map(|animator| u32::try_from(animator.file_index).expect("a file index fits u32")),
        animator_path_id: candidate
            .animator
            .map(|animator| BigInt::from(animator.path_id)),
        name: candidate.name,
    }
}

fn core_error(error: unity_rs_core::Error) -> Error {
    match error {
        unity_rs_core::Error::Unsupported(message) => Error::new(Status::GenericFailure, message),
        other => Error::new(Status::InvalidArg, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DisplayRowImage, DisplayRowImages, ModelTextureLimits, OpenOptions, copy_path_string,
        load_options, materialize_core_bytes, model_texture_limits, parse_audio_format,
        parse_export_mode, parse_filename_format, parse_image_format,
    };
    use std::io::Write;
    use unity_rs_core::export::{AudioExportFormat, ExportMode, FilenameFormat};
    use unity_rs_core::image_export::ImageFormat;
    use unity_rs_core::loader::LoadFailurePolicy;

    fn decoded_test_image(first: u8, second: u8) -> unity_rs_core::texture::RgbaImage {
        unity_rs_core::texture::RgbaImage {
            width: 1,
            height: 2,
            pixels: vec![first, 0, 0, 255, second, 0, 0, 255],
        }
    }

    #[test]
    fn worker_texture_outputs_already_use_display_row_order() {
        let DisplayRowImage(image) =
            DisplayRowImage::from_decoded(decoded_test_image(1, 2)).expect("one decoded texture");
        assert_eq!(image.pixels, [2, 0, 0, 255, 1, 0, 0, 255]);

        let DisplayRowImages(images) = DisplayRowImages::from_decoded(vec![
            decoded_test_image(3, 4),
            decoded_test_image(5, 6),
        ])
        .expect("decoded texture array");
        assert_eq!(&images[0].pixels[..], &[4, 0, 0, 255, 3, 0, 0, 255]);
        assert_eq!(&images[1].pixels[..], &[6, 0, 0, 255, 5, 0, 0, 255]);
    }

    #[test]
    fn maps_the_model_texture_reference_budget() {
        let defaults = model_texture_limits(None).expect("default texture limits");
        assert_eq!(defaults.maximum_texture_references, 1_000_000);
        assert_eq!(defaults.maximum_name_index_bytes, 64 * 1024 * 1024);
        assert_eq!(defaults.maximum_metadata_bytes, 256 * 1024 * 1024);

        let configured = model_texture_limits(Some(ModelTextureLimits {
            maximum_texture_references: Some(0),
            maximum_textures: None,
            maximum_name_index_bytes: None,
            maximum_metadata_bytes: None,
            maximum_total_encoded_bytes: None,
            maximum_single_texture_bytes: None,
        }))
        .expect("configured texture limits");
        assert_eq!(configured.maximum_texture_references, 0);
        assert_eq!(configured.maximum_textures, defaults.maximum_textures);
        assert_eq!(
            configured.maximum_name_index_bytes,
            defaults.maximum_name_index_bytes
        );
        assert_eq!(
            configured.maximum_metadata_bytes,
            defaults.maximum_metadata_bytes
        );
    }

    #[test]
    fn maps_the_load_diagnostic_budget() {
        let defaults = load_options(None, None).unwrap();
        assert_eq!(defaults.limits.maximum_diagnostic_bytes, 256 * 1024 * 1024);
        let configured = load_options(
            Some(OpenOptions {
                unity_version: None,
                unity_cn_key: None,
                skip_unreadable_inputs: Some(true),
                strict_unity_versions: Some(true),
                maximum_input_files: None,
                maximum_input_directories: None,
                maximum_directory_entries: None,
                maximum_path_bytes: None,
                maximum_total_path_bytes: None,
                maximum_diagnostic_bytes: Some(0),
            }),
            None,
        )
        .unwrap();
        assert_eq!(configured.limits.maximum_diagnostic_bytes, 0);
        assert_eq!(configured.failure_policy, LoadFailurePolicy::SkipInput);
        assert!(configured.strict_unity_versions);
        assert!(!defaults.strict_unity_versions);
    }

    fn assert_bounded_option_error(error: &napi::Error, field: &str, oversized: &str) {
        let message = error.to_string();
        assert!(
            message.contains(&format!("unsupported {field} value of 4096 UTF-8 bytes")),
            "unexpected option error: {message}"
        );
        assert!(!message.contains(oversized));
    }

    #[test]
    fn option_parsers_preserve_aliases_without_echoing_large_values() {
        assert_eq!(
            parse_export_mode(Some(" DuMp_TeXt ")).expect("dump alias"),
            ExportMode::DumpText
        );
        assert_eq!(
            parse_filename_format(Some(" PaTh_Id ")).expect("path alias"),
            FilenameFormat::PathId
        );
        assert_eq!(
            parse_image_format(Some(" RaW-RgBa ".to_owned())).expect("RGBA alias"),
            ImageFormat::RawRgba
        );
        assert_eq!(
            parse_audio_format(" WaVe ").expect("wave alias"),
            AudioExportFormat::Wav
        );
        assert_eq!(
            parse_image_format(None).expect("default image format"),
            ImageFormat::Png
        );

        let oversized = "é".repeat(2048);
        assert_bounded_option_error(
            &parse_export_mode(Some(&oversized)).expect_err("oversized export mode"),
            "export mode",
            &oversized,
        );
        assert_bounded_option_error(
            &parse_filename_format(Some(&oversized)).expect_err("oversized filename format"),
            "filename format",
            &oversized,
        );
        assert_bounded_option_error(
            &parse_image_format(Some(oversized.clone())).expect_err("oversized image format"),
            "image format",
            &oversized,
        );
        assert_bounded_option_error(
            &parse_audio_format(&oversized).expect_err("oversized audio format"),
            "audio format",
            &oversized,
        );
    }

    #[test]
    fn fallible_output_enforces_the_byte_limit() {
        let error = materialize_core_bytes(3, "test output", |output| {
            output.write_all(b"four")?;
            Ok(4)
        })
        .expect_err("four bytes must not fit a three-byte output budget");
        assert!(error.to_string().contains("test output exceeds 3 bytes"));
    }

    #[test]
    fn fallible_output_checks_the_writer_count() {
        let error = materialize_core_bytes(2, "test output", |output| {
            output.write_all(b"ok")?;
            Ok(1)
        })
        .expect_err("a writer's byte count must match its output");
        assert!(
            error
                .to_string()
                .contains("writer reported 1 bytes but produced 2")
        );
    }

    #[cfg(unix)]
    #[test]
    fn report_paths_preserve_platform_lossy_replacement_without_a_temporary_string() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let path = PathBuf::from(OsString::from_vec(vec![b'a', 0xff, b'b']));
        assert_eq!(
            copy_path_string(&path, "test report path").expect("fallible path copy"),
            "a\u{fffd}b"
        );
    }
}
