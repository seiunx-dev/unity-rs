//! Thin Node-API bindings over the safe high-level Rust API.

use std::sync::Arc;

use assetstudio_core::acl::AclCompressedTracksLimits;
use assetstudio_core::animation_clip::AnimationClipReadLimits;
use assetstudio_core::animator_controller::AnimatorControllerReadLimits;
use assetstudio_core::avatar::AvatarReadLimits;
use assetstudio_core::export::ExportOptions;
use assetstudio_core::extraction::ExtractionOptions;
use assetstudio_core::image_export::ImageFormat;
use assetstudio_core::live2d_clip_motion::CubismClipMotionReadLimits;
use assetstudio_core::live2d_motion::{CubismFadeMotionReadLimits, CubismMotionTargetNames};
use assetstudio_core::live2d_package::{Live2dPackageLimits, Live2dPackageMaterializeLimits};
use assetstudio_core::live2d_physics::CubismPhysicsReadLimits;
use assetstudio_core::live2d_schema::{CubismAuxiliaryReadLimits, CubismExpressionReadLimits};
use assetstudio_core::loader::{AssetLoadLimits, AssetLoadOptions, LoadFailurePolicy};
use assetstudio_core::material::MaterialReadLimits;
use assetstudio_core::mesh::MeshReadLimits;
use assetstudio_core::model_export::{ModelExportCandidate, ModelExportPlanLimits};
use assetstudio_core::mono_schema::{
    MonoBehaviourSchemaEntry, MonoBehaviourSchemaRegistry, MonoBehaviourSchemaSource,
};
use assetstudio_core::monobehaviour::MonoBehaviourReadLimits;
use assetstudio_core::project_settings::ProjectSettingsReadLimits;
use assetstudio_core::scene_hierarchy::SceneHierarchyLimits;
use assetstudio_core::scene_hierarchy::SceneObjectKey;
use assetstudio_core::scene_textures::SceneTextureLimits;
use assetstudio_core::serialized::{TypeTree, TypeTreeNode};
use assetstudio_core::simple_assets::{SimpleAssetReadLimits, SimpleBinaryAsset};
use assetstudio_core::source::Region;
use assetstudio_core::sprite::SpriteReadLimits;
use assetstudio_core::studio::{Studio, StudioObject};
use assetstudio_core::texture::TextureReadLimits;
use assetstudio_core::texture_array::TextureArrayReadLimits;
use assetstudio_core::unity_cn::UnityCnKey;
use assetstudio_core::unity_version::UnityVersion;
use napi::bindgen_prelude::{AsyncTask, BigInt, Buffer, FnArgs};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, Error, Result, Status, Task};
use napi_derive::napi;

const DEFAULT_PAYLOAD_LIMIT: u64 = 512 * 1024 * 1024;
const DEFAULT_PAGE_LIMIT: u32 = 4096;
const MAXIMUM_PAGE_LIMIT: u32 = 1_000_000;

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

/// One `AudioClip`'s stored payload and the extension its container implies.
#[napi(object)]
pub struct AudioClip {
    pub name: String,
    /// The extension the stored bytes carry, `.fsb` or `.wav` for example.
    pub extension: String,
    /// True when the payload is already a playable RIFF/WAVE stream.
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

/// An `Avatar`'s skeleton summary.
#[napi(object)]
pub struct Avatar {
    pub name: String,
    /// The size the object declares for its constant block.
    pub declared_size: u32,
    /// Bone path entries, retained in order so duplicate hashes keep Unity's
    /// first-hit behaviour.
    pub path_count: u32,
    pub has_human_description: bool,
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

/// An `AnimationClip`'s shape, without materializing its keyframes.
///
/// Separate booleans rather than a bitfield: this is a JavaScript-facing shape
/// and a bitfield would only move the decoding to the other side.
#[allow(clippy::struct_excessive_bools)]
#[napi(object)]
pub struct AnimationClipInfo {
    pub name: String,
    pub sample_rate: f64,
    pub wrap_mode: i32,
    pub legacy: bool,
    pub compressed: bool,
    pub rotation_curve_count: u32,
    pub position_curve_count: u32,
    pub scale_curve_count: u32,
    pub euler_curve_count: u32,
    pub float_curve_count: u32,
    /// Present when the clip carries muscle (humanoid) data.
    pub has_muscle_clip: bool,
    /// Present when the clip's samples live in a sibling resource file.
    pub has_streaming_info: bool,
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
    pub maximum_input_files: Option<u32>,
    pub maximum_input_directories: Option<u32>,
    pub maximum_directory_entries: Option<u32>,
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
}

/// Caller-configurable budgets for textures returned beside a model.
#[napi(object)]
pub struct ModelTextureLimits {
    pub maximum_textures: Option<u32>,
    pub maximum_total_encoded_bytes: Option<i64>,
    pub maximum_single_texture_bytes: Option<i64>,
}

fn load_options(
    options: Option<OpenOptions>,
    oodle: Option<Arc<dyn assetstudio_core::bundle::OodleDecoder>>,
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
    })
}

fn count_limit(value: Option<u32>, default: usize) -> usize {
    value.map_or(default, |value| value as usize)
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
        maximum_textures: count_limit(options.maximum_textures, defaults.maximum_textures),
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

/// One opened collection. All format work is delegated to `assetstudio-core`.
#[napi]
pub struct AssetStudio {
    studio: Arc<Studio>,
}

#[napi]
impl AssetStudio {
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
            decoder: Arc::new(JsAclDecoder { callback: decoder }),
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
    #[napi(ts_return_type = "Promise<AssetStudio>")]
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
    /// the JavaScript event loop.
    #[must_use]
    #[napi(ts_return_type = "Promise<AssetStudio>")]
    pub fn open_async(path: String) -> AsyncTask<OpenPathTask> {
        AsyncTask::new(OpenPathTask { path })
    }

    /// Opens one in-memory asset, bundle, or resource after copying the Node
    /// buffer into Rust-owned immutable storage.
    #[napi(factory)]
    pub fn from_buffer(
        data: &[u8],
        name: Option<String>,
        maximum_bytes: Option<i64>,
    ) -> Result<Self> {
        let maximum = byte_limit(maximum_bytes)?;
        let actual =
            u64::try_from(data.len()).map_err(|_| invalid_arg("buffer length does not fit u64"))?;
        if actual > maximum {
            return Err(invalid_arg(format!(
                "input buffer is {actual} bytes, exceeding limit {maximum}"
            )));
        }
        Studio::open_region(
            name.unwrap_or_else(|| "memory.assets".to_owned()),
            Region::from_bytes(data.to_vec()),
        )
        .map(|studio| Self {
            studio: Arc::new(studio),
        })
        .map_err(core_error)
    }

    /// Copies a Node buffer once, then parses it on a libuv worker.
    #[napi(ts_return_type = "Promise<AssetStudio>")]
    pub fn from_buffer_async(
        data: &[u8],
        name: Option<String>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<OpenBufferTask>> {
        let maximum = byte_limit(maximum_bytes)?;
        let actual =
            u64::try_from(data.len()).map_err(|_| invalid_arg("buffer length does not fit u64"))?;
        if actual > maximum {
            return Err(invalid_arg(format!(
                "input buffer is {actual} bytes, exceeding limit {maximum}"
            )));
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(data.len()).map_err(|error| {
            Error::from_reason(format!("cannot allocate input buffer: {error}"))
        })?;
        bytes.extend_from_slice(data);
        Ok(AsyncTask::new(OpenBufferTask {
            bytes,
            name: name.unwrap_or_else(|| "memory.assets".to_owned()),
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
        let data = audio.payload.read_to_vec(maximum).map_err(core_error)?;
        Ok(AudioClip {
            name: audio.name,
            extension: audio.raw_extension,
            is_direct_wav: audio.direct_wav.is_some(),
            data: data.into(),
        })
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
            texture_properties: property_names(&material.saved_properties.texture_environments),
            float_properties: property_names(&material.saved_properties.floats),
            color_properties: property_names(&material.saved_properties.colors),
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

    /// Reads an `Avatar`'s skeleton summary.
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
            .read_avatar(AvatarReadLimits {
                maximum_object_bytes: maximum,
                ..AvatarReadLimits::default()
            })
            .map_err(core_error)?;
        Ok(Avatar {
            name: avatar.name,
            declared_size: avatar.declared_avatar_size,
            path_count: u32::try_from(avatar.paths.len())
                .map_err(|_| invalid_arg("avatar path count does not fit u32"))?,
            has_human_description: avatar.human_description.is_some(),
        })
    }

    /// Opens several in-memory inputs as one collection.
    ///
    /// A serialized file and the `.resS` its textures and audio stream from are
    /// separate files; opening them one at a time leaves every streamed payload
    /// unresolvable, so a caller holding both in memory needs to pass them
    /// together.
    #[napi(factory)]
    pub fn from_buffers(inputs: Vec<MemoryInput>, maximum_bytes: Option<i64>) -> Result<Self> {
        let maximum = byte_limit(maximum_bytes)?;
        let mut total = 0_u64;
        let mut regions = Vec::new();
        for input in inputs {
            let length = u64::try_from(input.data.len())
                .map_err(|_| invalid_arg("buffer length does not fit u64"))?;
            total = total
                .checked_add(length)
                .ok_or_else(|| invalid_arg("input buffer sizes overflowed"))?;
            if total > maximum {
                return Err(invalid_arg(format!(
                    "input buffers total {total} bytes, exceeding limit {maximum}"
                )));
            }
            regions.push((input.name, Region::from_bytes(input.data.to_vec())));
        }
        Studio::open_regions(regions)
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
        let mut json = Vec::new();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "physics3.json's fps field is a float"
        )]
        let fallback = motion_fps.unwrap_or(30.0) as f32;
        rig.write_physics3_json(fallback, &mut json, maximum)
            .map_err(core_error)?;
        Ok(CubismDocument {
            name: String::new(),
            json: json.into(),
            entry_count: u32::try_from(rig.sub_rigs.len()).unwrap_or(u32::MAX),
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
        let mut json = Vec::new();
        expression
            .write_exp3_json(&mut json, maximum)
            .map_err(core_error)?;
        Ok(CubismDocument {
            name: expression.source_name.clone(),
            json: json.into(),
            entry_count: u32::try_from(expression.parameters.len()).unwrap_or(u32::MAX),
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
        let mut json = Vec::new();
        motion
            .write_motion3_json(
                &CubismMotionTargetNames::default(),
                false,
                &mut json,
                maximum,
            )
            .map_err(core_error)?;
        Ok(CubismDocument {
            name: motion.source_name.clone(),
            json: json.into(),
            entry_count: u32::try_from(motion.curves.len()).unwrap_or(u32::MAX),
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
        targets: Option<CubismMotionTargets>,
        force_bezier: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<CubismClipMotion> {
        let maximum = byte_limit(maximum_bytes)?;
        let target_names = cubism_motion_targets(targets);
        let motion = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_cubism_clip_motion(&target_names, cubism_clip_motion_limits(maximum)?)
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
        targets: Option<CubismMotionTargets>,
        force_bezier: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<AsyncTask<CubismClipMotionWithAclTask>> {
        Ok(AsyncTask::new(CubismClipMotionWithAclTask {
            studio: Arc::clone(&self.studio),
            file_index: usize::try_from(file_index).expect("u32 fits usize"),
            path_id: bigint_i64(path_id, "pathId")?,
            targets: cubism_motion_targets(targets),
            force_bezier: force_bezier.unwrap_or(false),
            maximum: byte_limit(maximum_bytes)?,
            decoder: Arc::new(JsAclDecoder { callback: decoder }),
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
        Ok(candidates.into_iter().map(into_candidate).collect())
    }

    /// Enumerates the branches an Animator owns.
    #[napi]
    pub fn animator_fbx_candidates(&self) -> Result<Vec<FbxCandidate>> {
        let candidates = self
            .studio
            .animator_fbx_candidates(ModelExportPlanLimits::default())
            .map_err(core_error)?;
        Ok(candidates.into_iter().map(into_candidate).collect())
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
        let options = ExportOptions {
            overwrite_existing: overwrite.unwrap_or(false),
            ..ExportOptions::default()
        };
        let report = self
            .studio
            .export(&output_root, options)
            .map_err(core_error)?;
        Ok(ExportReport {
            exported: report
                .exported
                .into_iter()
                .map(|record| ExportRecord {
                    source: record.source,
                    path_id: BigInt::from(record.path_id),
                    class_id: record.class_id,
                    output_path: record.output_path.to_string_lossy().into_owned(),
                    payload_kind: record.payload_kind.to_owned(),
                })
                .collect(),
            failures: report
                .failures
                .into_iter()
                .map(|failure| ExportFailure {
                    source: failure.source,
                    path_id: BigInt::from(failure.path_id),
                    class_id: failure.class_id,
                    error: failure.error,
                })
                .collect(),
            unsupported: report
                .unsupported
                .into_iter()
                .map(|declined| ExportFailure {
                    source: declined.source,
                    path_id: BigInt::from(declined.path_id),
                    class_id: declined.class_id,
                    error: declined.error,
                })
                .collect(),
        })
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

    /// Reads an `AnimationClip`'s shape without materializing its keyframes.
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
            .read_animation_clip(AnimationClipReadLimits {
                maximum_object_bytes: maximum,
                ..AnimationClipReadLimits::default()
            })
            .map_err(core_error)?;
        let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
        Ok(AnimationClipInfo {
            name: clip.name,
            sample_rate: f64::from(clip.sample_rate),
            wrap_mode: clip.wrap_mode,
            legacy: clip.legacy,
            compressed: clip.compressed,
            rotation_curve_count: count(clip.rotation_curves.len()),
            position_curve_count: count(clip.position_curves.len()),
            scale_curve_count: count(clip.scale_curves.len()),
            euler_curve_count: count(clip.euler_curves.len()),
            float_curve_count: count(clip.float_curves.len()),
            has_muscle_clip: clip.muscle_clip.is_some(),
            has_streaming_info: clip.streaming_info.is_some(),
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
        Ok(AnimatorControllerInfo {
            name: controller.name,
            tos_entry_count: u32::try_from(controller.tos.len())
                .map_err(|_| invalid_arg("TOS entry count does not fit u32"))?,
            animation_clip_path_ids: controller
                .animation_clips
                .iter()
                .map(|reference| BigInt::from(reference.path_id))
                .collect(),
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
        let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
        Ok(set
            .packages
            .into_iter()
            .map(|package| Live2dPackageInfo {
                name: package.name,
                directory_name: package.directory_name,
                moc_file_name: package.moc_file_name,
                texture_count: count(package.textures.len()),
                expression_count: count(package.expressions.len()),
                motion_count: count(package.motions.len()),
                has_physics: package.physics.is_some(),
                has_pose: package.pose.is_some(),
                has_display_info: package.display_info.is_some(),
            })
            .collect())
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
        let maximum = byte_limit(maximum_bytes)?;
        let set = self
            .studio
            .read_live2d_packages(
                Live2dPackageLimits::default(),
                Live2dPackageMaterializeLimits {
                    maximum_total_bytes: maximum,
                    ..Live2dPackageMaterializeLimits::default()
                },
            )
            .map_err(core_error)?;
        let mut packages = Vec::with_capacity(set.packages.len());
        for package in set.packages {
            let mut files = Vec::new();
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
            // The physics, pose and display-info documents are materialized
            // with the rest and were being dropped here, so a package that had
            // them arrived without them and nothing said so.
            for auxiliary in [package.physics, package.pose, package.display_info]
                .into_iter()
                .flatten()
            {
                files.push(Live2dFile {
                    file_name: auxiliary.file_name,
                    data: auxiliary.bytes.into(),
                });
            }
            packages.push(Live2dPackageFiles {
                name: package.name,
                directory_name: package.directory_name,
                files,
            });
        }
        let mut diagnostics = Vec::with_capacity(set.diagnostics.len());
        for diagnostic in set.diagnostics {
            diagnostics.push(Live2dDiagnostic {
                file_index: u32::try_from(diagnostic.object.file_index)
                    .map_err(|_| invalid_arg("serialized file index does not fit u32"))?,
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
        Ok(ModelObj {
            obj: model.obj.into(),
            material_library_name: model.material_library_name,
            material_library: model.material_library.into(),
            textures: model
                .textures
                .textures
                .into_iter()
                .map(|texture| Live2dFile {
                    file_name: texture.file_name,
                    data: texture.encoded.into(),
                })
                .collect(),
            skipped: model
                .textures
                .skipped
                .into_iter()
                .map(|skip| format!("{}: {}", skip.property, skip.reason))
                .collect(),
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
        let mut fbx = Vec::new();
        let (_, textures) = self
            .studio
            .write_fbx_with_textures(&mut fbx, maximum, texture_format, texture_limits)
            .map_err(core_error)?;
        Ok(TexturedFbx {
            fbx: fbx.into(),
            textures: textures
                .textures
                .into_iter()
                .map(|texture| Live2dFile {
                    file_name: texture.file_name,
                    data: texture.encoded.into(),
                })
                .collect(),
            skipped: textures
                .skipped
                .into_iter()
                .map(|skip| format!("{}: {}", skip.property, skip.reason))
                .collect(),
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
            .read_animation_clip(AnimationClipReadLimits {
                maximum_object_bytes: maximum,
                ..AnimationClipReadLimits::default()
            })
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
        file_index: u32,
        path_id: BigInt,
        schemas: Vec<MonoBehaviourSchema>,
        pretty: Option<bool>,
        maximum_bytes: Option<i64>,
    ) -> Result<MonoBehaviourJson> {
        let maximum = byte_limit(maximum_bytes)?;
        let registry = build_schema_registry(schemas)?;
        let limits = MonoBehaviourReadLimits {
            maximum_json_bytes: usize::try_from(maximum).unwrap_or(usize::MAX),
            ..MonoBehaviourReadLimits::default()
        };
        let resolved = self
            .object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_mono_behaviour_json(&registry, pretty.unwrap_or(false), limits)
            .map_err(core_error)?;
        Ok(MonoBehaviourJson {
            json: resolved.json.into(),
            source: schema_source_name(resolved.source).to_owned(),
        })
    }
}

impl AssetStudio {
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
        ) -> assetstudio_core::Result<SimpleBinaryAsset>,
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
    ThreadsafeFunction<AclDecodeRequest, AclDecodedClip, AclDecodeRequest, Status, false>;

/// Bridges a JavaScript ACL decoder into Core's synchronous animation build.
///
/// Same threading constraint as the Oodle bridge: the callback runs on the
/// event loop while a worker waits for it, so this is only reachable from the
/// asynchronous entry points.
struct JsAclDecoder {
    callback: AclCallback,
}

impl assetstudio_core::acl::AclDecoder for JsAclDecoder {
    fn decode(
        &self,
        request: &assetstudio_core::acl::AclDecodeRequest<'_>,
    ) -> assetstudio_core::Result<assetstudio_core::acl::AclDecodedClip> {
        let payload = AclDecodeRequest {
            frame_count: request.frame_count,
            bone_count: request.bone_count,
            sample_rate: f64::from(request.sample_rate()),
            declared_curve_count: request.declared_curve_count,
            use_fast_sample_mode: request.use_fast_sample_mode,
            compressed_tracks: Buffer::from(request.input.compressed_tracks.clone()),
            decoder_map: request.input.decoder_map.clone(),
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        self.callback.call_with_return_value(
            payload,
            ThreadsafeFunctionCallMode::Blocking,
            move |result: Result<AclDecodedClip>, _env| {
                let _ = sender.send(result.map(|clip| {
                    (
                        clip.times,
                        clip.binding_indices,
                        clip.values,
                        clip.following_curve_offset,
                    )
                }));
                Ok(())
            },
        );
        let (times, binding_indices, values, following_curve_offset) = receiver
            .recv()
            .map_err(|_| {
                assetstudio_core::Error::invalid_data("the ACL decoder callback never answered")
            })?
            .map_err(|error| {
                assetstudio_core::Error::invalid_data(format!("the ACL decoder failed: {error}"))
            })?;
        // Core validates shape, ordering and budgets on what comes back; the
        // narrowing here is only f64 to f32, which is what Unity stores.
        #[allow(clippy::cast_possible_truncation)]
        Ok(assetstudio_core::acl::AclDecodedClip {
            times: times.into_iter().map(|value| value as f32).collect(),
            binding_indices,
            values: values.into_iter().map(|value| value as f32).collect(),
            following_curve_offset,
        })
    }
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

impl assetstudio_core::bundle::OodleDecoder for JsOodleDecoder {
    fn decompress(&self, input: &[u8], output: &mut [u8]) -> assetstudio_core::Result<usize> {
        let expected = u32::try_from(output.len()).map_err(|_| {
            assetstudio_core::Error::invalid_data("Oodle output length does not fit u32")
        })?;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.callback.call_with_return_value(
            (Buffer::from(input.to_vec()), expected).into(),
            ThreadsafeFunctionCallMode::Blocking,
            move |result: Result<Buffer>, _env| {
                let _ = sender.send(result.map(|buffer| buffer.to_vec()));
                Ok(())
            },
        );
        let decoded = receiver
            .recv()
            .map_err(|_| {
                assetstudio_core::Error::invalid_data("the Oodle decoder callback never answered")
            })?
            .map_err(|error| {
                assetstudio_core::Error::invalid_data(format!("the Oodle decoder failed: {error}"))
            })?;
        if decoded.len() != output.len() {
            return Err(assetstudio_core::Error::invalid_data(format!(
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
}

impl Task for FbxWithAclTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Self::Output> {
        self.studio
            .read_fbx_with_acl_decoder(self.maximum, Some(self.decoder.as_ref()))
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

pub struct OpenWithOodleTask {
    path: String,
    decoder: Arc<JsOodleDecoder>,
    options: Option<OpenOptions>,
}

impl Task for OpenWithOodleTask {
    type Output = Studio;
    type JsValue = AssetStudio;

    fn compute(&mut self) -> Result<Self::Output> {
        let oodle = Arc::clone(&self.decoder) as Arc<dyn assetstudio_core::bundle::OodleDecoder>;
        let options = load_options(self.options.take(), Some(oodle))?;
        Studio::open_with_options(&self.path, options).map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, studio: Self::Output) -> Result<Self::JsValue> {
        Ok(AssetStudio {
            studio: Arc::new(studio),
        })
    }
}

pub struct OpenPathTask {
    path: String,
}

impl Task for OpenPathTask {
    type Output = Studio;
    type JsValue = AssetStudio;

    fn compute(&mut self) -> Result<Self::Output> {
        Studio::open(&self.path).map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, studio: Self::Output) -> Result<Self::JsValue> {
        Ok(AssetStudio {
            studio: Arc::new(studio),
        })
    }
}

pub struct OpenBufferTask {
    bytes: Vec<u8>,
    name: String,
}

impl Task for OpenBufferTask {
    type Output = Studio;
    type JsValue = AssetStudio;

    fn compute(&mut self) -> Result<Self::Output> {
        let bytes = std::mem::take(&mut self.bytes);
        Studio::open_region(self.name.clone(), Region::from_bytes(bytes)).map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, studio: Self::Output) -> Result<Self::JsValue> {
        Ok(AssetStudio {
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

pub struct ReadTextureTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    mip_level: u32,
    maximum: u64,
}

impl Task for ReadTextureTask {
    type Output = assetstudio_core::texture::RgbaImage;
    type JsValue = RgbaImage;

    fn compute(&mut self) -> Result<Self::Output> {
        studio_object(&self.studio, self.file_index, self.path_id)?
            .decode_texture_mip(
                self.mip_level,
                TextureReadLimits {
                    maximum_payload_bytes: self.maximum,
                    maximum_output_bytes: self.maximum,
                    maximum_decoder_working_bytes: self.maximum,
                    ..TextureReadLimits::default()
                },
            )
            .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, image: Self::Output) -> Result<Self::JsValue> {
        convert_decoded_image(image)
    }
}

pub struct ReadTextureArrayTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    maximum: u64,
}

impl Task for ReadTextureArrayTask {
    type Output = Vec<assetstudio_core::texture::RgbaImage>;
    type JsValue = Vec<RgbaImage>;

    fn compute(&mut self) -> Result<Self::Output> {
        studio_object(&self.studio, self.file_index, self.path_id)?
            .decode_texture_array_mip0(texture_array_limits(self.maximum))
            .map_err(core_error)
    }

    fn resolve(&mut self, _env: Env, images: Self::Output) -> Result<Self::JsValue> {
        convert_decoded_images(images)
    }
}

pub struct ReadSpriteTask {
    studio: Arc<Studio>,
    file_index: usize,
    path_id: i64,
    maximum: u64,
}

impl Task for ReadSpriteTask {
    type Output = assetstudio_core::texture::RgbaImage;
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

/// Converts caller-supplied schema descriptions into a lookup registry.
///
/// Bounded on both node count and total string bytes, because these arrive
/// from JavaScript and nothing else checks them.
fn build_schema_registry(schemas: Vec<MonoBehaviourSchema>) -> Result<MonoBehaviourSchemaRegistry> {
    const MAXIMUM_NODES: usize = 100_000;
    const MAXIMUM_STRING_BYTES: usize = 16 * 1024 * 1024;

    let mut registry = MonoBehaviourSchemaRegistry::new();
    let mut total_string_bytes = 0_usize;
    for schema in schemas {
        if schema.nodes.is_empty() {
            return Err(invalid_arg("a MonoBehaviour schema needs a root node"));
        }
        if schema.nodes.len() > MAXIMUM_NODES {
            return Err(invalid_arg(format!(
                "a MonoBehaviour schema has {} nodes; the maximum is {MAXIMUM_NODES}",
                schema.nodes.len()
            )));
        }
        let mut nodes = Vec::with_capacity(schema.nodes.len());
        for (index, node) in schema.nodes.into_iter().enumerate() {
            total_string_bytes = total_string_bytes
                .checked_add(node.type_name.len())
                .and_then(|value| value.checked_add(node.field_name.len()))
                .ok_or_else(|| invalid_arg("MonoBehaviour schema strings overflowed"))?;
            if total_string_bytes > MAXIMUM_STRING_BYTES {
                return Err(invalid_arg(format!(
                    "MonoBehaviour schema strings exceed {MAXIMUM_STRING_BYTES} bytes"
                )));
            }
            nodes.push(TypeTreeNode {
                type_name: node.type_name,
                field_name: node.field_name,
                byte_size: -1,
                index: i32::try_from(index)
                    .map_err(|_| invalid_arg("schema node index exceeds i32"))?,
                type_flags: 0,
                version: 1,
                meta_flags: if node.align { 0x4000 } else { 0 },
                level: node.level,
                type_string_offset: None,
                name_string_offset: None,
                reference_type_hash: 0,
            });
        }
        registry
            .push(MonoBehaviourSchemaEntry {
                assembly_name: schema.assembly_name,
                namespace: schema.namespace.unwrap_or_default(),
                class_name: schema.class_name,
                unity_version: schema.unity_version,
                tree: TypeTree {
                    nodes,
                    string_buffer: Vec::new(),
                },
            })
            .map_err(core_error)?;
    }
    Ok(registry)
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

fn cubism_motion_targets(targets: Option<CubismMotionTargets>) -> CubismMotionTargetNames {
    let targets = targets.unwrap_or(CubismMotionTargets {
        parameters: None,
        parts: None,
    });
    CubismMotionTargetNames {
        parameters: targets.parameters.unwrap_or_default(),
        parts: targets.parts.unwrap_or_default(),
    }
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
    motion: assetstudio_core::live2d_clip_motion::CubismClipMotion,
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
    properties: &[assetstudio_core::material::NamedMaterialProperty<T>],
) -> Vec<String> {
    properties
        .iter()
        .map(|property| property.name.clone())
        .collect()
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

/// Converts an image Core already returns in display row order, such as a
/// decoded `Sprite`.
fn convert_image(image: assetstudio_core::texture::RgbaImage) -> RgbaImage {
    RgbaImage {
        width: image.width,
        height: image.height,
        pixels: image.pixels.into(),
    }
}

/// Converts a `Texture2D` decoder result, whose rows run bottom-up, into the
/// top-down order every other surface hands to callers.
fn convert_decoded_image(mut image: assetstudio_core::texture::RgbaImage) -> Result<RgbaImage> {
    assetstudio_core::image_export::flip_rgba_rows(&mut image).map_err(core_error)?;
    Ok(convert_image(image))
}

fn convert_decoded_images(
    images: Vec<assetstudio_core::texture::RgbaImage>,
) -> Result<Vec<RgbaImage>> {
    let mut output = reserve(images.len(), "Texture2DArray images")?;
    for image in images {
        output.push(convert_decoded_image(image)?);
    }
    Ok(output)
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
    let value = value.unwrap_or_else(|| "png".to_owned());
    match value.trim().to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Ok(ImageFormat::Jpeg),
        "png" => Ok(ImageFormat::Png),
        "bmp" => Ok(ImageFormat::Bmp),
        "tga" => Ok(ImageFormat::Tga),
        "webp" => Ok(ImageFormat::Webp),
        "raw_rgba" | "raw-rgba" | "rgba" => Ok(ImageFormat::RawRgba),
        _ => Err(invalid_arg(format!("unsupported image format {value:?}"))),
    }
}

fn usize_limit(value: Option<i64>) -> Result<usize> {
    usize::try_from(byte_limit(value)?)
        .map_err(|_| invalid_arg("maximumBytes does not fit this platform"))
}

fn count_u32(value: usize, field: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::from_reason(format!("{field} does not fit u32")))
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

fn copy_string(value: &str, field: &str) -> Result<String> {
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|error| Error::from_reason(format!("cannot allocate {field}: {error}")))?;
    output.push_str(value);
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

fn core_error(error: assetstudio_core::Error) -> Error {
    match error {
        assetstudio_core::Error::Unsupported(message) => {
            Error::new(Status::GenericFailure, message)
        }
        other => Error::new(Status::InvalidArg, other.to_string()),
    }
}
