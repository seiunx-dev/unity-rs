//! Thin Node-API bindings over the safe high-level Rust API.

use std::sync::Arc;

use assetstudio_core::acl::AclCompressedTracksLimits;
use assetstudio_core::animation_clip::AnimationClipReadLimits;
use assetstudio_core::animator_controller::AnimatorControllerReadLimits;
use assetstudio_core::avatar::AvatarReadLimits;
use assetstudio_core::export::ExportOptions;
use assetstudio_core::extraction::ExtractionOptions;
use assetstudio_core::image_export::ImageFormat;
use assetstudio_core::live2d_package::{Live2dPackageLimits, Live2dPackageMaterializeLimits};
use assetstudio_core::loader::AssetLoadOptions;
use assetstudio_core::material::MaterialReadLimits;
use assetstudio_core::mesh::MeshReadLimits;
use assetstudio_core::mono_schema::{MonoBehaviourSchemaEntry, MonoBehaviourSchemaRegistry};
use assetstudio_core::monobehaviour::MonoBehaviourReadLimits;
use assetstudio_core::project_settings::ProjectSettingsReadLimits;
use assetstudio_core::scene_hierarchy::SceneHierarchyLimits;
use assetstudio_core::scene_textures::SceneTextureLimits;
use assetstudio_core::serialized::{TypeTree, TypeTreeNode};
use assetstudio_core::simple_assets::SimpleAssetReadLimits;
use assetstudio_core::source::Region;
use assetstudio_core::sprite::SpriteReadLimits;
use assetstudio_core::studio::{Studio, StudioObject};
use assetstudio_core::texture::TextureReadLimits;
use assetstudio_core::texture_array::TextureArrayReadLimits;
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
    pub fn open_with_oodle(path: String, decoder: OodleCallback) -> AsyncTask<OpenWithOodleTask> {
        AsyncTask::new(OpenWithOodleTask {
            path,
            decoder: Arc::new(JsOodleDecoder { callback: decoder }),
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
        let hierarchy = self.studio.scene_hierarchy(limits).map_err(core_error)?;
        let mut nodes = Vec::with_capacity(hierarchy.nodes.len());
        for node in &hierarchy.nodes {
            nodes.push(SceneNode {
                file_index: u32::try_from(node.object.file_index)
                    .map_err(|_| invalid_arg("file index does not fit u32"))?,
                path_id: BigInt::from(node.object.path_id),
                name: node.name.clone(),
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

    /// Materializes every Live2D package: the MOC, the model3 manifest, the
    /// mip-zero texture PNGs, and the expression, motion, physics, pose and
    /// display-info JSON where their verified fields are present.
    ///
    /// Returned in memory rather than written, so the caller decides where the
    /// files land and stays inside whatever budget it set.
    #[napi]
    pub fn read_live2d_packages(
        &self,
        maximum_bytes: Option<i64>,
    ) -> Result<Vec<Live2dPackageFiles>> {
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
            packages.push(Live2dPackageFiles {
                name: package.name,
                directory_name: package.directory_name,
                files,
            });
        }
        Ok(packages)
    }

    /// Writes the collection as ASCII FBX with its animations and returns the
    /// material textures it references.
    ///
    /// The FBX names each texture by file name, so the returned files have to
    /// be written beside it for those references to resolve. They come back
    /// rather than being written because this call has no directory of its own
    /// and where they land is the caller's decision.
    #[napi]
    pub fn read_fbx_with_textures(&self, maximum_bytes: Option<i64>) -> Result<TexturedFbx> {
        let maximum = byte_limit(maximum_bytes)?;
        let mut fbx = Vec::new();
        let (_, textures) = self
            .studio
            .write_fbx_with_textures(
                &mut fbx,
                maximum,
                ImageFormat::Png,
                SceneTextureLimits::default(),
            )
            .map_err(core_error)?;
        Ok(TexturedFbx {
            fbx: fbx.into(),
            textures: textures
                .textures
                .iter()
                .map(|texture| Live2dFile {
                    file_name: texture.file_name.clone(),
                    data: texture.encoded.clone().into(),
                })
                .collect(),
            skipped: textures
                .skipped
                .iter()
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
    ) -> Result<Buffer> {
        let maximum = byte_limit(maximum_bytes)?;
        let registry = build_schema_registry(schemas)?;
        let limits = MonoBehaviourReadLimits {
            maximum_json_bytes: usize::try_from(maximum).unwrap_or(usize::MAX),
            ..MonoBehaviourReadLimits::default()
        };
        self.object(file_index, bigint_i64(path_id, "pathId")?)?
            .read_mono_behaviour_json(&registry, pretty.unwrap_or(false), limits)
            .map(Into::into)
            .map_err(core_error)
    }
}

impl AssetStudio {
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

pub struct OpenWithOodleTask {
    path: String,
    decoder: Arc<JsOodleDecoder>,
}

impl Task for OpenWithOodleTask {
    type Output = Studio;
    type JsValue = AssetStudio;

    fn compute(&mut self) -> Result<Self::Output> {
        Studio::open_with_options(
            &self.path,
            AssetLoadOptions {
                oodle_decoder: Some(
                    Arc::clone(&self.decoder) as Arc<dyn assetstudio_core::bundle::OodleDecoder>
                ),
                ..AssetLoadOptions::default()
            },
        )
        .map_err(core_error)
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
    match value {
        None => Ok(DEFAULT_PAYLOAD_LIMIT),
        Some(value) if value >= 0 => u64::try_from(value)
            .map_err(|_| invalid_arg("maximumBytes does not fit an unsigned 64-bit integer")),
        Some(value) => Err(invalid_arg(format!(
            "maximumBytes must be non-negative, received {value}"
        ))),
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

fn core_error(error: assetstudio_core::Error) -> Error {
    match error {
        assetstudio_core::Error::Unsupported(message) => {
            Error::new(Status::GenericFailure, message)
        }
        other => Error::new(Status::InvalidArg, other.to_string()),
    }
}
