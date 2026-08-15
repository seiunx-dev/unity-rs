use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::image_export::{
    DEFAULT_JPEG_QUALITY, ImageFormat, ImageRowOrder, write_rgba_image_with_quality,
};
use crate::json::write_type_value_json;
use crate::loader::AssetCollection;
use crate::mesh::{MESH_CLASS_ID, Mesh, MeshReadLimits, read_mesh_with_collection, write_mesh_obj};
use crate::mono_schema::{
    MonoBehaviourSchemaProvider, MonoBehaviourSchemaSource, read_mono_behaviour_json_with_provider,
};
use crate::monobehaviour::{
    MONO_BEHAVIOUR_CLASS_ID, MonoBehaviourReadLimits, read_mono_behaviour, read_mono_behaviour_json,
};
use crate::serialized::SerializedFile;
use crate::shader::{SHADER_CLASS_ID, ShaderReadLimits, ShaderTextAsset, read_shader};
use crate::simple_assets::{
    AUDIO_CLIP_CLASS_ID, AudioClipAsset, DirectWavKind, FONT_CLASS_ID, MOVIE_TEXTURE_CLASS_ID,
    SimpleAssetReadLimits, SimpleBinaryAsset, VIDEO_CLIP_CLASS_ID, read_audio_clip_asset,
    read_font, read_movie_texture, read_video_clip, write_direct_wav,
};
use crate::source::Region;
use crate::sprite::{SPRITE_CLASS_ID, SpriteReadLimits, decode_sprite_rgba8, read_sprite};
use crate::texture::{RgbaImage, TEXTURE_2D_CLASS_ID, TextureReadLimits, read_texture2d};
use crate::texture_array::{
    TEXTURE_2D_ARRAY_CLASS_ID, Texture2DArray, TextureArrayReadLimits, read_texture2d_array,
    write_texture2d_array_rgba_bundle,
};
use crate::type_tree::TypeValue;
use crate::type_tree_dump::write_type_tree_dump;
use crate::{Error, Result};

const TEXT_ASSET_CLASS_ID: i32 = 49;
const MAXIMUM_TEMPORARY_FILE_ATTEMPTS: u64 = 1024;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportMode {
    Auto,
    Raw,
    TypeTreeJson,
    DumpText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilenameFormat {
    AssetName,
    AssetNamePathId,
    PathId,
}

/// Audio behavior for automatic `AudioClip` export.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AudioExportFormat {
    /// Emit verified direct WAV data and otherwise preserve the source container.
    #[default]
    Auto,
    /// Always preserve the original payload and suggested container extension.
    Raw,
    /// Require a decoder-free WAV path; unsupported FSB/codecs fail explicitly.
    Wav,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOptions {
    pub mode: ExportMode,
    pub filename_format: FilenameFormat,
    pub image_format: ImageFormat,
    pub jpeg_quality: u8,
    pub audio_format: AudioExportFormat,
    pub overwrite_existing: bool,
    pub restore_text_asset_extension: bool,
    pub pretty_json: bool,
    pub maximum_objects: usize,
    pub maximum_total_output_bytes: u64,
    pub maximum_raw_object_bytes: u64,
    pub maximum_type_tree_json_bytes: u64,
    pub maximum_type_tree_dump_bytes: u64,
    pub maximum_text_asset_bytes: usize,
    pub maximum_simple_asset_bytes: u64,
    pub maximum_audio_output_bytes: u64,
    pub maximum_texture_output_bytes: u64,
    pub maximum_texture_array_output_bytes: u64,
    pub maximum_texture_array_bundle_bytes: u64,
    pub maximum_sprite_output_bytes: u64,
    pub maximum_shader_output_bytes: u64,
    pub maximum_monobehaviour_json_bytes: usize,
    pub maximum_mesh_object_bytes: u64,
    pub maximum_mesh_output_bytes: u64,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            mode: ExportMode::Auto,
            filename_format: FilenameFormat::AssetName,
            image_format: ImageFormat::Png,
            jpeg_quality: DEFAULT_JPEG_QUALITY,
            audio_format: AudioExportFormat::Auto,
            overwrite_existing: false,
            restore_text_asset_extension: true,
            pretty_json: true,
            maximum_objects: 1_000_000,
            maximum_total_output_bytes: 16 * 1024 * 1024 * 1024,
            maximum_raw_object_bytes: 512 * 1024 * 1024,
            maximum_type_tree_json_bytes: 256 * 1024 * 1024,
            maximum_type_tree_dump_bytes: 256 * 1024 * 1024,
            maximum_text_asset_bytes: 512 * 1024 * 1024,
            maximum_simple_asset_bytes: 512 * 1024 * 1024,
            maximum_audio_output_bytes: 512 * 1024 * 1024,
            maximum_texture_output_bytes: 512 * 1024 * 1024,
            maximum_texture_array_output_bytes: 1024 * 1024 * 1024,
            maximum_texture_array_bundle_bytes: 1024 * 1024 * 1024,
            maximum_sprite_output_bytes: 512 * 1024 * 1024,
            maximum_shader_output_bytes: 512 * 1024 * 1024,
            maximum_monobehaviour_json_bytes: 256 * 1024 * 1024,
            maximum_mesh_object_bytes: 512 * 1024 * 1024,
            maximum_mesh_output_bytes: 512 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRecord {
    pub source: String,
    pub path_id: i64,
    pub class_id: i32,
    pub output_path: PathBuf,
    pub payload_kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFailure {
    pub source: String,
    pub path_id: i64,
    pub class_id: i32,
    pub error: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExportReport {
    pub exported: Vec<ExportRecord>,
    pub failures: Vec<ExportFailure>,
    /// Objects this implementation declines by design, kept apart from the
    /// ones that went wrong.
    ///
    /// A real game makes the distinction matter rather than academic: a Unity
    /// 2022 build carries hundreds of shaders whose serialized layout no
    /// implementation here reads, so folding them in with genuine failures
    /// means every export of a modern game reports failure and exits non-zero,
    /// and a caller cannot tell that from an export that actually broke.
    pub unsupported: Vec<ExportFailure>,
}

struct ObjectExportContext<'a> {
    collection: &'a AssetCollection,
    file: &'a SerializedFile,
    file_index: usize,
    schemas: Option<&'a dyn MonoBehaviourSchemaProvider>,
    source: &'a str,
    output_directory: &'a Path,
    options: ExportOptions,
}

pub fn export_collection(
    collection: &AssetCollection,
    output_root: &Path,
    options: ExportOptions,
) -> Result<ExportReport> {
    export_collection_with_plan(collection, output_root, options, ExportPlan::default())
}

/// What to export and what extra knowledge to export it with.
///
/// Separate from [`ExportOptions`] because these borrow, and options are
/// copied through the whole selection chain.
#[derive(Clone, Copy, Default)]
pub struct ExportPlan<'a> {
    /// Schemas for `MonoBehaviour` objects whose own type tree was stripped
    /// from the build.
    ///
    /// Without them a stripped behaviour is declined, and in a modern game
    /// that is most of the file: exporting one Unity 2022.3 title reports
    /// 172,192 objects unsupported for this reason alone. The provider
    /// supplies the layout as data; nothing here opens or executes the managed
    /// assembly it came from.
    pub mono_schemas: Option<&'a dyn MonoBehaviourSchemaProvider>,
    /// The class IDs to export. `None` exports every class.
    ///
    /// Worth having because the cost of an export is dominated by whatever is
    /// largest: dumping one game's `Live2D` bundles as type-tree JSON writes
    /// gigabytes of mesh, and a caller after its script data pays all of it.
    pub classes: Option<&'a [i32]>,
}

impl std::fmt::Debug for ExportPlan<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExportPlan")
            // A provider is caller code with no useful rendering, so it is
            // reported as present or absent rather than described.
            .field("mono_schemas", &self.mono_schemas.is_some())
            .field("classes", &self.classes)
            .finish()
    }
}

/// The same export, narrowed and informed by a plan.
pub fn export_collection_with_plan(
    collection: &AssetCollection,
    output_root: &Path,
    options: ExportOptions,
    plan: ExportPlan<'_>,
) -> Result<ExportReport> {
    let schemas = plan.mono_schemas;
    let object_count = collection
        .serialized_files
        .iter()
        .try_fold(0_usize, |total, loaded| {
            total
                .checked_add(loaded.file.objects.len())
                .ok_or_else(|| Error::invalid_data("export object count overflowed"))
        })?;
    if object_count > options.maximum_objects {
        return Err(Error::invalid_data(format!(
            "export has {object_count} objects, exceeding limit {}",
            options.maximum_objects
        )));
    }
    let output_root = prepare_output_root(output_root)?;
    let mut report = ExportReport::default();
    let mut claimed_paths = HashSet::new();
    let mut total_output_bytes = 0_u64;

    for (file_index, loaded) in collection.serialized_files.iter().enumerate() {
        let group_name = format!(
            "{file_index:04}_{}",
            sanitize_file_name(portable_file_name(&loaded.path))
        );
        let group_path = output_root.join(group_name);
        ensure_secure_export_directory(&group_path)?;

        for (object_index, object) in loaded.file.objects.iter().enumerate() {
            if let Some(classes) = plan.classes
                && !classes.contains(&object.class_id)
            {
                continue;
            }
            let remaining_output_bytes = options
                .maximum_total_output_bytes
                .checked_sub(total_output_bytes)
                .ok_or_else(|| Error::invalid_data("export output accounting underflowed"))?;
            let context = ObjectExportContext {
                collection,
                file: &loaded.file,
                file_index,
                schemas,
                source: &loaded.path,
                output_directory: &group_path,
                options,
            };
            let outcome = export_object(
                &context,
                object_index,
                remaining_output_bytes,
                &mut claimed_paths,
            );
            match outcome {
                Ok((record, written)) => {
                    total_output_bytes =
                        total_output_bytes.checked_add(written).ok_or_else(|| {
                            Error::invalid_data("export output byte total overflowed")
                        })?;
                    report.exported.try_reserve(1).map_err(|error| {
                        Error::invalid_data(format!("cannot grow export records: {error}"))
                    })?;
                    report.exported.push(record);
                }
                Err(error) => {
                    let declined = matches!(error, Error::Unsupported(_));
                    let bucket = if declined {
                        &mut report.unsupported
                    } else {
                        &mut report.failures
                    };
                    bucket.try_reserve(1).map_err(|reserve_error| {
                        Error::invalid_data(format!(
                            "cannot grow export records while recording {error}: {reserve_error}"
                        ))
                    })?;
                    bucket.push(ExportFailure {
                        source: loaded.path.clone(),
                        path_id: object.path_id,
                        class_id: object.class_id,
                        error: error.to_string(),
                    });
                }
            }
        }
    }

    Ok(report)
}

fn export_object(
    context: &ObjectExportContext<'_>,
    object_index: usize,
    remaining_output_bytes: u64,
    claimed_paths: &mut HashSet<String>,
) -> Result<(ExportRecord, u64)> {
    let object = context.file.objects.get(object_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized object index {object_index} is out of range"
        ))
    })?;

    let (base_name, extension, payload_kind, payload) = select_export_payload(
        context.collection,
        context.file,
        context.file_index,
        object_index,
        object.class_id,
        context.options,
        context.schemas,
    )?;
    if matches!(&payload, ExportPayload::Raw)
        && object.byte_size > context.options.maximum_raw_object_bytes
    {
        return Err(Error::invalid_data(format!(
            "raw object is {} bytes, exceeding limit {}",
            object.byte_size, context.options.maximum_raw_object_bytes
        )));
    }

    let formatted_name = format_file_name(
        &sanitize_file_name(&base_name),
        object.path_id,
        context.options.filename_format,
    );
    let output_path = unique_output_path(
        context.output_directory,
        &formatted_name,
        &extension,
        object.path_id,
        claimed_paths,
    )?;
    ensure_secure_export_directory(context.output_directory)?;
    let mut output = AtomicExportFile::create(&output_path)?;
    let written;
    {
        let mut writer =
            OutputLimitWriter::new(BufWriter::new(output.file_mut()), remaining_output_bytes);
        let write_result = (|| -> Result<()> {
            write_export_payload(
                payload,
                context,
                object_index,
                object.byte_size,
                &mut writer,
            )?;
            writer.flush()?;
            Ok(())
        })();
        if writer.limit_exceeded {
            return Err(Error::invalid_data(format!(
                "export exceeds the remaining collection output budget of {remaining_output_bytes} bytes"
            )));
        }
        write_result?;
        written = writer.written;
    }
    output.persist(&output_path, context.options.overwrite_existing)?;

    Ok((
        ExportRecord {
            source: context.source.to_owned(),
            path_id: object.path_id,
            class_id: object.class_id,
            output_path,
            payload_kind,
        },
        written,
    ))
}

fn write_export_payload(
    payload: ExportPayload,
    context: &ObjectExportContext<'_>,
    object_index: usize,
    object_size: u64,
    writer: &mut impl Write,
) -> Result<()> {
    match payload {
        ExportPayload::Raw => {
            context
                .file
                .object_region(object_index)?
                .copy_range(0, object_size, writer)?;
        }
        ExportPayload::Bytes(bytes) => writer.write_all(&bytes)?,
        ExportPayload::Region(region) => {
            region.copy_range(0, region.len(), writer)?;
        }
        ExportPayload::DirectWav {
            payload,
            kind,
            maximum_output_bytes,
        } => {
            write_direct_wav(&payload, kind, maximum_output_bytes, writer)?;
        }
        ExportPayload::Image {
            image,
            format,
            row_order,
            maximum_output_bytes,
        } => {
            write_rgba_image_with_quality(
                &image,
                format,
                row_order,
                context.options.jpeg_quality,
                maximum_output_bytes,
                writer,
            )?;
        }
        ExportPayload::TextureArrayBundle { texture, limits } => {
            write_texture2d_array_rgba_bundle(&texture, limits, writer)?;
        }
        ExportPayload::ShaderText(shader) => {
            shader.write_to(writer)?;
        }
        ExportPayload::TypeTreeJson(value) => {
            write_bounded_type_tree_json(
                &value,
                context.options.pretty_json,
                context.options.maximum_type_tree_json_bytes,
                writer,
            )?;
        }
        ExportPayload::TypeTreeDump(value) => {
            let tree = context.file.object_type_tree(object_index)?;
            write_type_tree_dump(
                tree,
                &value,
                writer,
                context.options.maximum_type_tree_dump_bytes,
            )?;
        }
        ExportPayload::MeshObj {
            mesh,
            maximum_output_bytes,
        } => {
            write_mesh_obj(&mesh, writer, maximum_output_bytes)?;
        }
    }
    Ok(())
}

type ExportSelection = (String, String, &'static str, ExportPayload);

fn select_export_payload(
    collection: &AssetCollection,
    file: &SerializedFile,
    file_index: usize,
    object_index: usize,
    class_id: i32,
    options: ExportOptions,
    schemas: Option<&dyn MonoBehaviourSchemaProvider>,
) -> Result<ExportSelection> {
    if options.mode == ExportMode::Auto {
        return select_auto_export_payload(
            collection,
            file,
            file_index,
            object_index,
            class_id,
            options,
            schemas,
        );
    }
    Ok(match options.mode {
        ExportMode::Raw => (
            fallback_object_name(file, object_index),
            ".dat".to_owned(),
            "raw",
            ExportPayload::Raw,
        ),
        ExportMode::TypeTreeJson if class_id == MONO_BEHAVIOUR_CLASS_ID => {
            return select_mono_behaviour_json(
                collection,
                file,
                file_index,
                object_index,
                options,
                schemas,
            );
        }
        ExportMode::TypeTreeJson => {
            let value = file.read_type_tree_value(object_index)?;
            let name = type_value_name(&value)
                .map_or_else(|| fallback_object_name(file, object_index), str::to_owned);
            (
                name,
                ".json".to_owned(),
                "typetree_json",
                ExportPayload::TypeTreeJson(value),
            )
        }
        ExportMode::DumpText => {
            let value = file.read_type_tree_value(object_index)?;
            let name = type_value_name(&value)
                .map_or_else(|| fallback_object_name(file, object_index), str::to_owned);
            (
                name,
                ".txt".to_owned(),
                "typetree_dump",
                ExportPayload::TypeTreeDump(value),
            )
        }
        ExportMode::Auto => unreachable!("auto mode returned above"),
    })
}

#[allow(clippy::too_many_lines)]
fn select_auto_export_payload(
    collection: &AssetCollection,
    file: &SerializedFile,
    file_index: usize,
    object_index: usize,
    class_id: i32,
    options: ExportOptions,
    schemas: Option<&dyn MonoBehaviourSchemaProvider>,
) -> Result<ExportSelection> {
    Ok(match class_id {
        TEXT_ASSET_CLASS_ID => {
            let text = file.read_text_asset(object_index, options.maximum_text_asset_bytes)?;
            let extension = if options.restore_text_asset_extension && has_extension(&text.name) {
                String::new()
            } else {
                ".txt".to_owned()
            };
            (
                text.name,
                extension,
                "text_bytes",
                ExportPayload::Bytes(text.script),
            )
        }
        TEXTURE_2D_CLASS_ID => {
            let limits = TextureReadLimits {
                maximum_payload_bytes: options.maximum_simple_asset_bytes,
                maximum_output_bytes: options.maximum_texture_output_bytes,
                ..TextureReadLimits::default()
            };
            let texture = read_texture2d(collection, file, object_index, limits)?;
            let image = texture.decode_mip_rgba8(0, limits)?;
            (
                texture.name,
                options.image_format.extension().to_owned(),
                options.image_format.payload_kind(),
                ExportPayload::Image {
                    image,
                    format: options.image_format,
                    row_order: ImageRowOrder::UnityDecoded,
                    maximum_output_bytes: options.maximum_texture_output_bytes,
                },
            )
        }
        TEXTURE_2D_ARRAY_CLASS_ID => {
            let limits = TextureArrayReadLimits {
                maximum_payload_bytes: options.maximum_simple_asset_bytes,
                maximum_output_bytes: options.maximum_texture_array_output_bytes,
                maximum_bundle_bytes: options.maximum_texture_array_bundle_bytes,
                ..TextureArrayReadLimits::default()
            };
            let texture = read_texture2d_array(collection, file, object_index, limits)?;
            (
                texture.name.clone(),
                String::new(),
                "image_array_bundle_raw_rgba",
                ExportPayload::TextureArrayBundle { texture, limits },
            )
        }
        SPRITE_CLASS_ID => {
            let sprite_limits = SpriteReadLimits {
                maximum_output_bytes: options.maximum_sprite_output_bytes,
                ..SpriteReadLimits::default()
            };
            let texture_limits = TextureReadLimits {
                maximum_payload_bytes: options.maximum_simple_asset_bytes,
                maximum_output_bytes: options.maximum_texture_output_bytes,
                ..TextureReadLimits::default()
            };
            let sprite = read_sprite(file, object_index, sprite_limits)?;
            let image =
                decode_sprite_rgba8(collection, file, &sprite, sprite_limits, texture_limits)?;
            (
                sprite.name,
                options.image_format.extension().to_owned(),
                options.image_format.payload_kind(),
                ExportPayload::Image {
                    image,
                    format: options.image_format,
                    row_order: ImageRowOrder::Display,
                    maximum_output_bytes: options.maximum_sprite_output_bytes,
                },
            )
        }
        SHADER_CLASS_ID => {
            let limits = ShaderReadLimits {
                maximum_script_bytes: options.maximum_simple_asset_bytes,
                maximum_output_bytes: options.maximum_shader_output_bytes,
                ..ShaderReadLimits::default()
            };
            let shader = read_shader(file, object_index, limits)?;
            let payload_kind = shader.payload_kind();
            let extension = shader.suggested_extension().to_owned();
            (
                shader.name.clone(),
                extension,
                payload_kind,
                ExportPayload::ShaderText(shader),
            )
        }
        MONO_BEHAVIOUR_CLASS_ID => {
            return select_mono_behaviour_json(
                collection,
                file,
                file_index,
                object_index,
                options,
                schemas,
            );
        }
        MESH_CLASS_ID => {
            let limits = MeshReadLimits {
                maximum_object_bytes: options.maximum_mesh_object_bytes,
                maximum_output_bytes: options.maximum_mesh_output_bytes,
                ..MeshReadLimits::default()
            };
            let mesh = read_mesh_with_collection(collection, file, object_index, limits)?;
            (
                mesh.name.clone(),
                ".obj".to_owned(),
                "mesh_obj",
                ExportPayload::MeshObj {
                    mesh,
                    maximum_output_bytes: limits.maximum_output_bytes,
                },
            )
        }
        AUDIO_CLIP_CLASS_ID => {
            return select_audio_clip(collection, file, object_index, options);
        }
        FONT_CLASS_ID | MOVIE_TEXTURE_CLASS_ID | VIDEO_CLIP_CLASS_ID => {
            let asset = read_simple_asset(
                collection,
                file,
                object_index,
                class_id,
                options.maximum_simple_asset_bytes,
            )?;
            let SimpleBinaryAsset {
                name,
                payload,
                payload_kind,
                suggested_extension,
                ..
            } = asset;
            (
                name,
                suggested_extension,
                payload_kind,
                ExportPayload::Region(payload),
            )
        }
        _ => match file.read_type_tree_value(object_index) {
            Ok(value) => {
                let name = type_value_name(&value)
                    .map_or_else(|| fallback_object_name(file, object_index), str::to_owned);
                (
                    name,
                    ".json".to_owned(),
                    "typetree_json",
                    ExportPayload::TypeTreeJson(value),
                )
            }
            Err(_) => (
                fallback_object_name(file, object_index),
                ".dat".to_owned(),
                "raw",
                ExportPayload::Raw,
            ),
        },
    })
}

fn select_mono_behaviour_json(
    collection: &AssetCollection,
    file: &SerializedFile,
    file_index: usize,
    object_index: usize,
    options: ExportOptions,
    schemas: Option<&dyn MonoBehaviourSchemaProvider>,
) -> Result<ExportSelection> {
    let limits = MonoBehaviourReadLimits {
        maximum_json_bytes: options.maximum_monobehaviour_json_bytes,
        ..MonoBehaviourReadLimits::default()
    };
    let behaviour = read_mono_behaviour(file, object_index, limits)?;
    let (json, kind) = match schemas {
        Some(schemas) => {
            let resolved = read_mono_behaviour_json_with_provider(
                collection,
                file_index,
                object_index,
                schemas,
                options.pretty_json,
                limits,
            )?;
            // Named apart because it is a weaker claim: the layout came from a
            // schema the caller supplied, not from anything Unity wrote.
            let kind = match resolved.source {
                MonoBehaviourSchemaSource::Embedded => "typetree_json",
                MonoBehaviourSchemaSource::External => "typetree_json_schema",
            };
            (resolved.json, kind)
        }
        None => (
            read_mono_behaviour_json(file, object_index, options.pretty_json, limits)?,
            "typetree_json",
        ),
    };
    let name = if behaviour.name.is_empty() {
        fallback_object_name(file, object_index)
    } else {
        behaviour.name
    };
    Ok((
        name,
        ".json".to_owned(),
        kind,
        ExportPayload::Bytes(json.into_bytes()),
    ))
}

fn select_audio_clip(
    collection: &AssetCollection,
    file: &SerializedFile,
    object_index: usize,
    options: ExportOptions,
) -> Result<ExportSelection> {
    let limits = SimpleAssetReadLimits {
        maximum_payload_bytes: options.maximum_simple_asset_bytes,
        ..SimpleAssetReadLimits::default()
    };
    let AudioClipAsset {
        name,
        payload,
        raw_extension,
        direct_wav,
        ..
    } = read_audio_clip_asset(collection, file, object_index, limits)?;
    let wav_kind = match options.audio_format {
        AudioExportFormat::Auto => direct_wav,
        AudioExportFormat::Raw => None,
        AudioExportFormat::Wav => Some(direct_wav.ok_or_else(|| {
            Error::unsupported(
                "AudioClip uses a compressed or unsupported audio codec and cannot be exported directly as WAV",
            )
        })?),
    };
    Ok(if let Some(kind) = wav_kind {
        (
            name,
            ".wav".to_owned(),
            "audio_wav",
            ExportPayload::DirectWav {
                payload,
                kind,
                maximum_output_bytes: options.maximum_audio_output_bytes,
            },
        )
    } else {
        (
            name,
            raw_extension,
            "audio_raw",
            ExportPayload::Region(payload),
        )
    })
}

fn read_simple_asset(
    collection: &AssetCollection,
    file: &SerializedFile,
    object_index: usize,
    class_id: i32,
    maximum_payload_bytes: u64,
) -> Result<SimpleBinaryAsset> {
    let limits = SimpleAssetReadLimits {
        maximum_payload_bytes,
        ..SimpleAssetReadLimits::default()
    };
    match class_id {
        FONT_CLASS_ID => read_font(file, object_index, limits),
        MOVIE_TEXTURE_CLASS_ID => read_movie_texture(file, object_index, limits),
        VIDEO_CLIP_CLASS_ID => read_video_clip(collection, file, object_index, limits),
        _ => Err(Error::unsupported(format!(
            "class ID {class_id} has no simple binary exporter"
        ))),
    }
}

enum ExportPayload {
    Raw,
    Bytes(Vec<u8>),
    Region(Region),
    DirectWav {
        payload: Region,
        kind: DirectWavKind,
        maximum_output_bytes: u64,
    },
    Image {
        image: RgbaImage,
        format: ImageFormat,
        row_order: ImageRowOrder,
        maximum_output_bytes: u64,
    },
    TextureArrayBundle {
        texture: Texture2DArray,
        limits: TextureArrayReadLimits,
    },
    ShaderText(ShaderTextAsset),
    TypeTreeJson(crate::type_tree::TypeValue),
    TypeTreeDump(crate::type_tree::TypeValue),
    MeshObj {
        mesh: Mesh,
        maximum_output_bytes: u64,
    },
}

fn write_bounded_type_tree_json(
    value: &TypeValue,
    pretty: bool,
    maximum_output_bytes: u64,
    output: &mut impl Write,
) -> Result<()> {
    let mut bounded = OutputLimitWriter::new(output, maximum_output_bytes);
    let result = write_type_value_json(value, &mut bounded, pretty);
    if bounded.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "TypeTree JSON exceeds the {maximum_output_bytes} byte output limit"
        )));
    }
    result
}

struct OutputLimitWriter<W> {
    inner: W,
    maximum: u64,
    written: u64,
    limit_exceeded: bool,
}

impl<W> OutputLimitWriter<W> {
    const fn new(inner: W, maximum: u64) -> Self {
        Self {
            inner,
            maximum,
            written: 0,
            limit_exceeded: false,
        }
    }
}

impl<W: Write> Write for OutputLimitWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("export write length does not fit in u64"))?;
        let end = self
            .written
            .checked_add(length)
            .ok_or_else(|| io::Error::other("export output length overflowed"))?;
        if end > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "export output byte limit exceeded",
            ));
        }
        let written = self.inner.write(bytes)?;
        let written_u64 = u64::try_from(written)
            .map_err(|_| io::Error::other("export write count does not fit in u64"))?;
        self.written = self
            .written
            .checked_add(written_u64)
            .ok_or_else(|| io::Error::other("export output length overflowed"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn type_value_name(value: &TypeValue) -> Option<&str> {
    let TypeValue::Object(fields) = value else {
        return None;
    };
    fields.iter().find_map(|field| {
        if field.name == "m_Name"
            && let TypeValue::String(name) = &field.value
            && !name.is_empty()
        {
            return Some(name.as_str());
        }
        None
    })
}

fn fallback_object_name(file: &SerializedFile, object_index: usize) -> String {
    let object = &file.objects[object_index];
    file.types
        .get(object.serialized_type_index.unwrap_or(usize::MAX))
        .and_then(|kind| kind.type_tree.as_ref())
        .and_then(|tree| tree.nodes.first())
        .map(|node| node.type_name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("class_{}", object.class_id))
}

fn format_file_name(base_name: &str, path_id: i64, format: FilenameFormat) -> String {
    match format {
        FilenameFormat::AssetName => base_name.to_owned(),
        FilenameFormat::AssetNamePathId => format!("{base_name} @{path_id}"),
        FilenameFormat::PathId => path_id.to_string(),
    }
}

fn unique_output_path(
    directory: &Path,
    name: &str,
    extension: &str,
    path_id: i64,
    claimed_paths: &mut HashSet<String>,
) -> Result<PathBuf> {
    claimed_paths
        .try_reserve(2)
        .map_err(|error| Error::invalid_data(format!("cannot grow export path set: {error}")))?;
    for suffix in [String::new(), format!(" @{path_id}")] {
        let candidate = directory.join(format!("{name}{suffix}{extension}"));
        let identity = candidate.to_string_lossy().to_lowercase();
        if claimed_paths.insert(identity) {
            return Ok(candidate);
        }
    }
    Err(Error::invalid_data(format!(
        "cannot create a unique export path for object {path_id}"
    )))
}

fn prepare_output_root(path: &Path) -> Result<PathBuf> {
    let absolute = lexical_absolute(path)?;
    let temporary_root = lexical_absolute(&std::env::temp_dir())?;
    let secure = if let Ok(relative) = absolute.strip_prefix(&temporary_root) {
        fs::canonicalize(&temporary_root)?.join(relative)
    } else {
        absolute
    };
    ensure_secure_export_directory(&secure)?;
    Ok(secure)
}

fn lexical_absolute(path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(Error::invalid_data(format!(
                        "export path escapes the filesystem root: {}",
                        path.display()
                    )));
                }
            }
        }
    }
    Ok(normalized)
}

fn ensure_secure_export_directory(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::invalid_data(format!(
                    "refusing symbolic-link export directory: {}",
                    current.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(Error::invalid_data(format!(
                    "export path component is not a directory: {}",
                    current.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
                let metadata = fs::symlink_metadata(&current)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(Error::invalid_data(format!(
                        "export directory became unsafe while creating it: {}",
                        current.display()
                    )));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct AtomicExportFile {
    path: PathBuf,
    file: Option<File>,
    persisted: bool,
}

impl AtomicExportFile {
    fn create(destination: &Path) -> Result<Self> {
        let parent = destination.parent().ok_or_else(|| {
            Error::invalid_data(format!(
                "export output has no parent directory: {}",
                destination.display()
            ))
        })?;
        for _ in 0..MAXIMUM_TEMPORARY_FILE_ATTEMPTS {
            let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".assetstudio-export-{}-{sequence}.tmp",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        persisted: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::invalid_data(format!(
            "cannot allocate an export temporary file after {MAXIMUM_TEMPORARY_FILE_ATTEMPTS} attempts"
        )))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("export temporary file is open")
    }

    fn persist(&mut self, destination: &Path, overwrite: bool) -> Result<()> {
        self.file_mut().flush()?;
        self.file_mut().sync_all()?;
        self.file
            .take()
            .ok_or_else(|| Error::invalid_data("export temporary file was already closed"))?;
        match export_file_state(destination)? {
            ExportFileState::Missing => {}
            ExportFileState::Regular if overwrite => {}
            ExportFileState::Regular => {
                return Err(Error::invalid_data(format!(
                    "refusing to overwrite existing export output: {}",
                    destination.display()
                )));
            }
            ExportFileState::Directory => {
                return Err(Error::invalid_data(format!(
                    "export output is a directory: {}",
                    destination.display()
                )));
            }
            ExportFileState::Other => {
                return Err(Error::invalid_data(format!(
                    "export output is not a regular file: {}",
                    destination.display()
                )));
            }
        }
        if !overwrite {
            return match fs::hard_link(&self.path, destination) {
                Ok(()) => {
                    fs::remove_file(&self.path)?;
                    self.persisted = true;
                    Ok(())
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    Err(Error::invalid_data(format!(
                        "refusing to overwrite existing export output: {}",
                        destination.display()
                    )))
                }
                Err(error) => Err(error.into()),
            };
        }
        match fs::rename(&self.path, destination) {
            Ok(()) => {
                self.persisted = true;
                Ok(())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
                ) =>
            {
                self.persist_with_backup(destination, error)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn persist_with_backup(&mut self, destination: &Path, original: io::Error) -> Result<()> {
        let backup = self.path.with_extension("replace-backup");
        if export_file_state(&backup)? != ExportFileState::Missing {
            return Err(original.into());
        }
        fs::rename(destination, &backup).map_err(|_| original)?;
        match fs::rename(&self.path, destination) {
            Ok(()) => {
                self.persisted = true;
                fs::remove_file(backup)?;
                Ok(())
            }
            Err(error) => {
                let _ = fs::rename(&backup, destination);
                Err(error.into())
            }
        }
    }
}

impl Drop for AtomicExportFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportFileState {
    Missing,
    Regular,
    Directory,
    Other,
}

fn export_file_state(path: &Path) -> Result<ExportFileState> {
    Ok(match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(Error::invalid_data(format!(
                "refusing symbolic-link export output: {}",
                path.display()
            )));
        }
        Ok(metadata) if metadata.is_file() => ExportFileState::Regular,
        Ok(metadata) if metadata.is_dir() => ExportFileState::Directory,
        Ok(_) => ExportFileState::Other,
        Err(error) if error.kind() == io::ErrorKind::NotFound => ExportFileState::Missing,
        Err(error) => return Err(error.into()),
    })
}

fn sanitize_file_name(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        {
            output.push('_');
        } else {
            output.push(character);
        }
    }
    let output = output.trim_matches([' ', '.']);
    if output.is_empty() || output == "." || output == ".." {
        "unnamed".to_owned()
    } else {
        output.to_owned()
    }
}

fn portable_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

fn has_extension(name: &str) -> bool {
    Path::new(name)
        .file_name()
        .and_then(|file_name| Path::new(file_name).extension())
        .is_some_and(|extension| !extension.is_empty())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Read};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use flate2::read::ZlibDecoder;
    use image_webp::WebPDecoder;
    use zune_jpeg::JpegDecoder;

    use crate::image_export::ImageFormat;
    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{
        ExportMode, ExportOptions, FilenameFormat, export_collection, format_file_name,
        has_extension, sanitize_file_name, type_value_name,
    };
    use crate::type_tree::{TypeField, TypeValue};

    #[test]
    fn sanitizes_names_and_formats_path_ids() {
        assert_eq!(sanitize_file_name("../bad:name?.txt"), "_bad_name_.txt");
        assert_eq!(sanitize_file_name("..."), "unnamed");
        assert_eq!(
            format_file_name("asset", -7, FilenameFormat::AssetNamePathId),
            "asset @-7"
        );
        assert_eq!(format_file_name("asset", 9, FilenameFormat::PathId), "9");
    }

    #[test]
    fn detects_text_asset_extensions_without_accepting_directories() {
        assert!(has_extension("script.lua"));
        assert!(!has_extension("script"));
        assert!(!has_extension("folder.with.dot/script"));
    }

    #[test]
    fn uses_the_top_level_type_tree_name() {
        let value = TypeValue::Object(vec![TypeField {
            name: "m_Name".to_owned(),
            value: TypeValue::String("named asset".to_owned()),
        }]);
        assert_eq!(type_value_name(&value), Some("named asset"));
    }

    #[test]
    fn output_defaults_to_atomic_no_overwrite() {
        let collection = AssetCollection::load(
            "fixture.assets",
            Region::from_bytes(synthetic_v22_text_asset()),
        )
        .unwrap();
        let output = unique_temp_directory("assetstudio-export-existing");
        let group = output.join("0000_fixture.assets");
        fs::create_dir_all(&group).unwrap();
        let destination = group.join("demo.lua");
        fs::write(&destination, b"existing").unwrap();

        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();

        assert!(report.exported.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(fs::read(destination).unwrap(), b"existing");
        assert!(!fs::read_dir(group).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".assetstudio-export-")
        }));
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn overwrite_failure_preserves_the_previous_regular_file() {
        let collection =
            AssetCollection::load("mesh.assets", Region::from_bytes(synthetic_v22_mesh())).unwrap();
        let output = unique_temp_directory("assetstudio-export-atomic-failure");
        let group = output.join("0000_mesh.assets");
        fs::create_dir_all(&group).unwrap();
        let destination = group.join("tri_mesh.obj");
        fs::write(&destination, b"previous").unwrap();

        let report = export_collection(
            &collection,
            &output,
            ExportOptions {
                overwrite_existing: true,
                maximum_mesh_output_bytes: 8,
                ..ExportOptions::default()
            },
        )
        .unwrap();

        assert!(report.exported.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(fs::read(destination).unwrap(), b"previous");
        assert_eq!(fs::read_dir(group).unwrap().count(), 1);
        fs::remove_dir_all(output).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_rejects_symbolic_link_outputs_without_touching_the_target() {
        use std::os::unix::fs::symlink;

        let collection = AssetCollection::load(
            "fixture.assets",
            Region::from_bytes(synthetic_v22_text_asset()),
        )
        .unwrap();
        let output = unique_temp_directory("assetstudio-export-symlink");
        let group = output.join("0000_fixture.assets");
        fs::create_dir_all(&group).unwrap();
        let victim = output.join("victim.txt");
        fs::write(&victim, b"untouched").unwrap();
        symlink(&victim, group.join("demo.lua")).unwrap();

        let report = export_collection(
            &collection,
            &output,
            ExportOptions {
                overwrite_existing: true,
                ..ExportOptions::default()
            },
        )
        .unwrap();

        assert!(report.exported.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].error.contains("symbolic-link"));
        assert_eq!(fs::read(victim).unwrap(), b"untouched");
        fs::remove_dir_all(output).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn export_root_rejects_symbolic_link_ancestors() {
        use std::os::unix::fs::symlink;

        let collection = AssetCollection::load(
            "fixture.assets",
            Region::from_bytes(synthetic_v22_text_asset()),
        )
        .unwrap();
        let root = unique_temp_directory("assetstudio-export-ancestor");
        let actual = root.join("actual");
        fs::create_dir_all(&actual).unwrap();
        let linked = root.join("linked");
        symlink(&actual, &linked).unwrap();

        let error = export_collection(
            &collection,
            &linked.join("nested"),
            ExportOptions::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("symbolic-link"));
        assert_eq!(fs::read_dir(actual).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exports_a_text_asset_end_to_end() {
        let collection = AssetCollection::load(
            "fixture.assets",
            Region::from_bytes(synthetic_v22_text_asset()),
        )
        .unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = std::env::temp_dir().join(format!("assetstudio-export-tree-{unique}"));

        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();

        assert!(report.failures.is_empty());
        assert_eq!(report.exported.len(), 1);
        assert_eq!(report.exported[0].payload_kind, "text_bytes");
        assert_eq!(
            fs::read(&report.exported[0].output_path).unwrap(),
            b"payload"
        );
        assert_eq!(
            report.exported[0]
                .output_path
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "demo.lua"
        );

        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn enforces_object_raw_json_and_collection_output_budgets_atomically() {
        let single = AssetCollection::load(
            "fixture.assets",
            Region::from_bytes(synthetic_v22_text_asset()),
        )
        .unwrap();
        let rejected_root = unique_temp_directory("assetstudio-export-object-budget");
        let error = export_collection(
            &single,
            &rejected_root,
            ExportOptions {
                maximum_objects: 0,
                ..ExportOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("1 objects"));
        assert!(!rejected_root.exists());

        let raw_root = unique_temp_directory("assetstudio-export-raw-budget");
        let raw = export_collection(
            &single,
            &raw_root,
            ExportOptions {
                mode: ExportMode::Raw,
                maximum_raw_object_bytes: 4,
                ..ExportOptions::default()
            },
        )
        .unwrap();
        assert!(raw.exported.is_empty());
        assert_eq!(raw.failures.len(), 1);
        assert!(raw.failures[0].error.contains("raw object"));
        assert_eq!(
            fs::read_dir(raw_root.join("0000_fixture.assets"))
                .unwrap()
                .count(),
            0
        );
        fs::remove_dir_all(raw_root).unwrap();

        let generic = AssetCollection::load(
            "generic.assets",
            Region::from_bytes(synthetic_v22_embedded_tree(1)),
        )
        .unwrap();
        let json_root = unique_temp_directory("assetstudio-export-json-budget");
        let json = export_collection(
            &generic,
            &json_root,
            ExportOptions {
                mode: ExportMode::TypeTreeJson,
                maximum_type_tree_json_bytes: 8,
                ..ExportOptions::default()
            },
        )
        .unwrap();
        assert!(json.exported.is_empty());
        assert_eq!(json.failures.len(), 1);
        assert!(json.failures[0].error.contains("TypeTree JSON exceeds"));
        assert_eq!(
            fs::read_dir(json_root.join("0000_generic.assets"))
                .unwrap()
                .count(),
            0
        );
        fs::remove_dir_all(json_root).unwrap();

        let files = ["first.assets", "second.assets"]
            .into_iter()
            .map(|path| LoadedSerializedFile {
                path: path.to_owned(),
                file: SerializedFile::open(Region::from_bytes(synthetic_v22_text_asset())).unwrap(),
            })
            .collect();
        let pair = AssetCollection::from_loaded_parts(files, Vec::new());
        let total_root = unique_temp_directory("assetstudio-export-total-budget");
        let total = export_collection(
            &pair,
            &total_root,
            ExportOptions {
                maximum_total_output_bytes: 7,
                ..ExportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(total.exported.len(), 1);
        assert_eq!(total.failures.len(), 1);
        assert!(total.failures[0].error.contains("collection output budget"));
        assert_eq!(
            fs::read(&total.exported[0].output_path).unwrap(),
            b"payload"
        );
        assert_eq!(
            fs::read_dir(total_root.join("0001_second.assets"))
                .unwrap()
                .count(),
            0
        );
        fs::remove_dir_all(total_root).unwrap();
    }

    #[test]
    fn exports_a_texture_array_bundle_with_the_managed_contract() {
        let collection = AssetCollection::load(
            "array.assets",
            Region::from_bytes(synthetic_v22_texture_array()),
        )
        .unwrap();
        let output = unique_temp_directory("assetstudio-texture-array");

        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();

        assert!(report.failures.is_empty());
        assert_eq!(report.exported.len(), 1);
        let record = &report.exported[0];
        assert_eq!(record.class_id, 187);
        assert_eq!(record.payload_kind, "image_array_bundle_raw_rgba");
        assert_eq!(
            record.output_path.file_name().unwrap().to_string_lossy(),
            "layers"
        );
        let bundle = fs::read(&record.output_path).unwrap();
        assert_eq!(&bundle[..30], b"HARUKI_ASSET_PAYLOAD_BUNDLE_V1");
        assert_eq!(read_i32_le(&bundle, 30), 2);
        let mut cursor = 34_usize;
        let mut payload_lengths = Vec::new();
        for expected_name in ["layer_0000.rgba", "layer_0001.rgba"] {
            let name_length = usize::try_from(read_i32_le(&bundle, cursor)).unwrap();
            cursor += 4;
            payload_lengths.push(read_i64_le(&bundle, cursor));
            cursor += 8;
            assert_eq!(
                std::str::from_utf8(&bundle[cursor..cursor + name_length]).unwrap(),
                expected_name
            );
            cursor += name_length;
        }
        assert_eq!(payload_lengths, [44, 44]);
        for (index, expected_pixel) in [[5, 6, 7, 8], [15, 16, 17, 18]].into_iter().enumerate() {
            let payload_start = cursor + index * 44;
            assert_eq!(
                &bundle[payload_start..payload_start + 16],
                b"HARUKI_RGBAIR_V1"
            );
            assert_eq!(
                &bundle[payload_start + 36..payload_start + 40],
                &expected_pixel
            );
        }

        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn exports_texture2d_auto_in_every_selected_image_format() {
        let collection = AssetCollection::load(
            "texture.assets",
            Region::from_bytes(synthetic_v22_texture2d()),
        )
        .unwrap();

        for (format, extension, payload_kind) in [
            (ImageFormat::Png, "png", "image_png"),
            (ImageFormat::Bmp, "bmp", "image_bmp"),
            (ImageFormat::Tga, "tga", "image_tga"),
            (ImageFormat::Webp, "webp", "image_webp"),
            (ImageFormat::RawRgba, "rgba", "image_raw_rgba"),
        ] {
            let output = unique_temp_directory(&format!("assetstudio-texture-{extension}"));
            let report = export_collection(
                &collection,
                &output,
                ExportOptions {
                    image_format: format,
                    ..ExportOptions::default()
                },
            )
            .unwrap();

            assert!(report.failures.is_empty(), "{:?}", report.failures);
            assert_eq!(report.exported.len(), 1);
            let record = &report.exported[0];
            assert_eq!(record.payload_kind, payload_kind);
            assert_eq!(
                record.output_path.file_name().unwrap().to_string_lossy(),
                format!("image.{extension}")
            );
            let bytes = fs::read(&record.output_path).unwrap();
            let displayed_top_left = match format {
                ImageFormat::Jpeg => unreachable!("JPEG has a separate lossy export test"),
                ImageFormat::Png => png_first_pixel(&bytes),
                ImageFormat::Bmp => bytes[122..126].try_into().map(bgra_to_rgba).unwrap(),
                ImageFormat::Tga => bytes[18..22].try_into().map(bgra_to_rgba).unwrap(),
                ImageFormat::Webp => webp_first_pixel(&bytes),
                ImageFormat::RawRgba => bytes[36..40].try_into().unwrap(),
            };
            assert_eq!(displayed_top_left, [0, 0, 255, 3]);

            fs::remove_dir_all(output).unwrap();
        }
    }

    #[test]
    fn exports_texture2d_auto_as_bounded_jpeg() {
        let collection = AssetCollection::load(
            "texture.assets",
            Region::from_bytes(synthetic_v22_texture2d()),
        )
        .unwrap();
        let output = unique_temp_directory("assetstudio-texture-jpeg");
        let report = export_collection(
            &collection,
            &output,
            ExportOptions {
                image_format: ImageFormat::Jpeg,
                jpeg_quality: 100,
                ..ExportOptions::default()
            },
        )
        .unwrap();

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.exported.len(), 1);
        let record = &report.exported[0];
        assert_eq!(record.payload_kind, "image_jpeg");
        assert_eq!(record.output_path.file_name().unwrap(), "image.jpg");
        let bytes = fs::read(&record.output_path).unwrap();
        assert_eq!(&bytes[..2], &[0xff, 0xd8]);
        assert_eq!(&bytes[bytes.len() - 2..], &[0xff, 0xd9]);
        let mut decoder = JpegDecoder::new(Cursor::new(bytes));
        let pixels = decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(pixels.len(), 2 * 2 * 3);

        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn enforces_texture_array_export_output_and_bundle_limits() {
        let collection = AssetCollection::load(
            "array.assets",
            Region::from_bytes(synthetic_v22_texture_array()),
        )
        .unwrap();
        for (label, options, expected) in [
            (
                "output",
                ExportOptions {
                    maximum_texture_array_output_bytes: 7,
                    ..ExportOptions::default()
                },
                "RGBA output",
            ),
            (
                "bundle",
                ExportOptions {
                    maximum_texture_array_bundle_bytes: 1,
                    ..ExportOptions::default()
                },
                "bundle is",
            ),
        ] {
            let output = unique_temp_directory(&format!("assetstudio-texture-array-{label}"));
            let report = export_collection(&collection, &output, options).unwrap();

            assert!(report.exported.is_empty());
            assert_eq!(report.failures.len(), 1);
            assert_eq!(report.failures[0].class_id, 187);
            assert!(
                report.failures[0].error.contains(expected),
                "expected {expected:?} in {}",
                report.failures[0].error
            );

            fs::remove_dir_all(output).unwrap();
        }
    }

    #[test]
    fn exports_a_mesh_obj_end_to_end_with_legacy_text_contract() {
        let collection =
            AssetCollection::load("mesh.assets", Region::from_bytes(synthetic_v22_mesh())).unwrap();
        let output = unique_temp_directory("assetstudio-mesh");

        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();

        assert!(report.failures.is_empty());
        assert_eq!(report.exported.len(), 1);
        let record = &report.exported[0];
        assert_eq!(record.class_id, 43);
        assert_eq!(record.payload_kind, "mesh_obj");
        assert_eq!(
            record.output_path.file_name().unwrap().to_string_lossy(),
            "tri_mesh.obj"
        );
        assert_eq!(
            fs::read(&record.output_path).unwrap(),
            concat!(
                "g tri:mesh\r\n",
                "v -1 0 0\r\n",
                "v -0 1 0\r\n",
                "v -0 0 1\r\n",
                "g tri:mesh_0\r\n",
                "f 3/3/3 2/2/2 1/1/1\r\n",
            )
            .as_bytes()
        );

        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn enforces_mesh_export_object_and_output_limits() {
        let collection =
            AssetCollection::load("mesh.assets", Region::from_bytes(synthetic_v22_mesh())).unwrap();
        for (label, options, expected) in [
            (
                "object",
                ExportOptions {
                    maximum_mesh_object_bytes: 8,
                    ..ExportOptions::default()
                },
                "Mesh object",
            ),
            (
                "output",
                ExportOptions {
                    maximum_mesh_output_bytes: 8,
                    ..ExportOptions::default()
                },
                "Mesh OBJ exceeds",
            ),
        ] {
            let output = unique_temp_directory(&format!("assetstudio-mesh-{label}"));
            let report = export_collection(&collection, &output, options).unwrap();

            assert!(report.exported.is_empty());
            assert_eq!(report.failures.len(), 1);
            assert_eq!(report.failures[0].class_id, 43);
            assert!(
                report.failures[0].error.contains(expected),
                "expected {expected:?} in {}",
                report.failures[0].error
            );

            fs::remove_dir_all(output).unwrap();
        }
    }

    #[test]
    fn exports_mono_behaviour_json_in_auto_and_explicit_modes() {
        let collection = AssetCollection::load(
            "behaviour.assets",
            Region::from_bytes(synthetic_v22_mono_behaviour(true)),
        )
        .unwrap();

        for (label, options) in [
            ("auto", ExportOptions::default()),
            (
                "explicit",
                ExportOptions {
                    mode: ExportMode::TypeTreeJson,
                    pretty_json: false,
                    ..ExportOptions::default()
                },
            ),
        ] {
            let output = unique_temp_directory(&format!("assetstudio-mono-{label}"));
            let report = export_collection(&collection, &output, options).unwrap();

            assert!(report.failures.is_empty());
            assert_eq!(report.exported.len(), 1);
            let record = &report.exported[0];
            assert_eq!(record.class_id, 114);
            assert_eq!(record.payload_kind, "typetree_json");
            assert_eq!(
                record.output_path.file_name().unwrap().to_string_lossy(),
                "Hero.json"
            );
            let json: serde_json::Value =
                serde_json::from_slice(&fs::read(&record.output_path).unwrap()).unwrap();
            assert_eq!(json["m_Name"], "Hero");
            assert_eq!(json["score"], 123);
            assert_eq!(json["m_GameObject"]["m_PathID"], 41);

            fs::remove_dir_all(output).unwrap();
        }
    }

    #[test]
    fn exports_managed_compatible_type_tree_dump_text_with_limits() {
        let collection = AssetCollection::load(
            "dump.assets",
            Region::from_bytes(synthetic_v22_embedded_tree(1)),
        )
        .unwrap();
        let output = unique_temp_directory("assetstudio-dump-text");
        let report = export_collection(
            &collection,
            &output,
            ExportOptions {
                mode: ExportMode::DumpText,
                ..ExportOptions::default()
            },
        )
        .unwrap();

        assert!(report.failures.is_empty());
        assert_eq!(report.exported.len(), 1);
        assert_eq!(report.exported[0].payload_kind, "typetree_dump");
        assert_eq!(
            report.exported[0].output_path.file_name().unwrap(),
            "Hero.txt"
        );
        let bytes = fs::read(&report.exported[0].output_path).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "MonoBehaviour Base\r\n\
\tPPtr<GameObject> m_GameObject\r\n\
\t\tint m_FileID = 0\r\n\
\t\tSInt64 m_PathID = 41\r\n\
\tUInt8 m_Enabled = 1\r\n\
\tPPtr<MonoScript> m_Script\r\n\
\t\tint m_FileID = 2\r\n\
\t\tSInt64 m_PathID = 99\r\n\
\tstring m_Name = \"Hero\"\r\n\
\tint score = 123\r\n"
        );
        fs::remove_dir_all(output).unwrap();

        let limited_output = unique_temp_directory("assetstudio-dump-text-limit");
        let report = export_collection(
            &collection,
            &limited_output,
            ExportOptions {
                mode: ExportMode::DumpText,
                maximum_type_tree_dump_bytes: 8,
                ..ExportOptions::default()
            },
        )
        .unwrap();
        assert!(report.exported.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].error.contains("TypeTree dump exceeds"));
        assert_eq!(
            fs::read_dir(limited_output.join("0000_dump.assets"))
                .unwrap()
                .count(),
            0
        );
        fs::remove_dir_all(limited_output).unwrap();
    }

    #[test]
    fn reports_missing_mono_behaviour_schema_without_raw_fallback() {
        let collection = AssetCollection::load(
            "stripped.assets",
            Region::from_bytes(synthetic_v22_mono_behaviour(false)),
        )
        .unwrap();
        let output = unique_temp_directory("assetstudio-mono-stripped");

        let report = export_collection(&collection, &output, ExportOptions::default()).unwrap();

        assert!(report.exported.is_empty());
        // Declined, not broken: the schema lives in a managed assembly this
        // implementation does not load, which is a statement about the reader
        // rather than about the file.
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.unsupported.len(), 1);
        assert_eq!(report.unsupported[0].class_id, 114);
        assert!(report.unsupported[0].error.contains("managed assembly"));
        assert!(report.unsupported[0].error.contains("dummy-DLL schema"));
        let group = output.join("0000_stripped.assets");
        assert_eq!(fs::read_dir(group).unwrap().count(), 0);

        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn enforces_mono_behaviour_export_json_limit() {
        let collection = AssetCollection::load(
            "behaviour.assets",
            Region::from_bytes(synthetic_v22_mono_behaviour(true)),
        )
        .unwrap();
        let output = unique_temp_directory("assetstudio-mono-limit");
        let options = ExportOptions {
            maximum_monobehaviour_json_bytes: 8,
            ..ExportOptions::default()
        };

        let report = export_collection(&collection, &output, options).unwrap();

        assert!(report.exported.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].error.contains("JSON exceeds"));
        fs::remove_dir_all(output).unwrap();
    }

    fn unique_temp_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{unique}"))
    }

    fn png_first_pixel(png: &[u8]) -> [u8; 4] {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut cursor = 8;
        let mut idat = Vec::new();
        while cursor < png.len() {
            let length = usize::try_from(u32::from_be_bytes(
                png[cursor..cursor + 4].try_into().unwrap(),
            ))
            .unwrap();
            let kind = &png[cursor + 4..cursor + 8];
            let data_start = cursor + 8;
            let data_end = data_start + length;
            if kind == b"IDAT" {
                idat.extend_from_slice(&png[data_start..data_end]);
            }
            cursor = data_end + 4;
        }
        let mut scanlines = Vec::new();
        ZlibDecoder::new(idat.as_slice())
            .read_to_end(&mut scanlines)
            .unwrap();
        assert_eq!(scanlines[0], 0);
        scanlines[1..5].try_into().unwrap()
    }

    fn webp_first_pixel(webp: &[u8]) -> [u8; 4] {
        let mut decoder = WebPDecoder::new(Cursor::new(webp)).unwrap();
        let mut pixels = vec![0; decoder.output_buffer_size().unwrap()];
        decoder.read_image(&mut pixels).unwrap();
        pixels[..4].try_into().unwrap()
    }

    const fn bgra_to_rgba(pixel: [u8; 4]) -> [u8; 4] {
        [pixel[2], pixel[1], pixel[0], pixel[3]]
    }

    #[derive(Clone, Copy)]
    struct TestNode {
        type_name: &'static str,
        field_name: &'static str,
        level: u8,
        align: bool,
    }

    fn synthetic_v22_mono_behaviour(include_tree: bool) -> Vec<u8> {
        synthetic_v22_embedded_tree_with_option(114, include_tree)
    }

    fn synthetic_v22_embedded_tree(class_id: i32) -> Vec<u8> {
        synthetic_v22_embedded_tree_with_option(class_id, true)
    }

    fn synthetic_v22_embedded_tree_with_option(class_id: i32, include_tree: bool) -> Vec<u8> {
        let mut object = Vec::new();
        push_i32_le(&mut object, 0);
        object.extend_from_slice(&41_i64.to_le_bytes());
        object.push(1);
        align_vec(&mut object, 4);
        push_i32_le(&mut object, 2);
        object.extend_from_slice(&99_i64.to_le_bytes());
        push_i32_le(&mut object, 4);
        object.extend_from_slice(b"Hero");
        push_i32_le(&mut object, 123);

        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32_le(&mut metadata, 13);
        metadata.push(u8::from(include_tree));
        push_i32_le(&mut metadata, 1);
        push_i32_le(&mut metadata, class_id);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0x11; 16]);
        if class_id == 114 {
            metadata.extend_from_slice(&[0x22; 16]);
        }
        if include_tree {
            push_blob_tree(&mut metadata, &mono_behaviour_tree());
            push_i32_le(&mut metadata, 0);
        }
        push_i32_le(&mut metadata, 1);
        align_vec_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32_le(&mut metadata, 0);
        for _ in 0..3 {
            push_i32_le(&mut metadata, 0);
        }
        metadata.push(0);

        finish_v22_asset(&metadata, &object)
    }

    fn mono_behaviour_tree() -> Vec<TestNode> {
        vec![
            test_node("MonoBehaviour", "Base", 0, false),
            test_node("PPtr<GameObject>", "m_GameObject", 1, false),
            test_node("int", "m_FileID", 2, false),
            test_node("SInt64", "m_PathID", 2, false),
            test_node("UInt8", "m_Enabled", 1, true),
            test_node("PPtr<MonoScript>", "m_Script", 1, false),
            test_node("int", "m_FileID", 2, false),
            test_node("SInt64", "m_PathID", 2, false),
            test_node("string", "m_Name", 1, false),
            test_node("Array", "Array", 2, true),
            test_node("int", "size", 3, false),
            test_node("char", "data", 3, false),
            test_node("int", "score", 1, false),
        ]
    }

    const fn test_node(
        type_name: &'static str,
        field_name: &'static str,
        level: u8,
        align: bool,
    ) -> TestNode {
        TestNode {
            type_name,
            field_name,
            level,
            align,
        }
    }

    fn push_blob_tree(output: &mut Vec<u8>, nodes: &[TestNode]) {
        let mut strings = Vec::new();
        let mut offsets = Vec::new();
        for node in nodes {
            let type_offset = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(node.type_name.as_bytes());
            strings.push(0);
            let name_offset = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(node.field_name.as_bytes());
            strings.push(0);
            offsets.push((type_offset, name_offset));
        }

        push_i32_le(output, i32::try_from(nodes.len()).unwrap());
        push_i32_le(output, i32::try_from(strings.len()).unwrap());
        for (index, (node, (type_offset, name_offset))) in nodes.iter().zip(offsets).enumerate() {
            output.extend_from_slice(&1_u16.to_le_bytes());
            output.push(node.level);
            output.push(0);
            output.extend_from_slice(&type_offset.to_le_bytes());
            output.extend_from_slice(&name_offset.to_le_bytes());
            output.extend_from_slice(&(-1_i32).to_le_bytes());
            output.extend_from_slice(&i32::try_from(index).unwrap().to_le_bytes());
            output.extend_from_slice(&(if node.align { 0x4000_i32 } else { 0 }).to_le_bytes());
            output.extend_from_slice(&0_u64.to_le_bytes());
        }
        output.extend_from_slice(&strings);
    }

    #[allow(clippy::too_many_lines)]
    fn synthetic_v22_mesh() -> Vec<u8> {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "tri:mesh");
        push_i32_le(&mut object, 1);
        push_u32_le(&mut object, 0);
        push_u32_le(&mut object, 3);
        push_i32_le(&mut object, 0);
        push_u32_le(&mut object, 0);
        push_u32_le(&mut object, 0);
        push_u32_le(&mut object, 3);
        object.extend_from_slice(&[0_u8; 24]);

        for _ in 0..4 {
            push_i32_le(&mut object, 0);
        }
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 0);
        push_u32_le(&mut object, 0);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 0);

        object.extend_from_slice(&[0, 1, 0, 0]);
        align_vec(&mut object, 4);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 6);
        object.extend_from_slice(&0_u16.to_le_bytes());
        object.extend_from_slice(&1_u16.to_le_bytes());
        object.extend_from_slice(&2_u16.to_le_bytes());
        align_vec(&mut object, 4);

        push_u32_le(&mut object, 3);
        push_i32_le(&mut object, 1);
        object.extend_from_slice(&[0, 0, 0, 3]);
        let mut vertex_data = Vec::new();
        for vertex in [[1_f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            for component in vertex {
                vertex_data.extend_from_slice(&component.to_le_bytes());
            }
        }
        vertex_data.resize(48, 0);
        push_i32_le(&mut object, i32::try_from(vertex_data.len()).unwrap());
        object.extend_from_slice(&vertex_data);
        align_vec(&mut object, 4);

        for _ in 0..4 {
            push_empty_packed_float(&mut object);
        }
        for _ in 0..3 {
            push_empty_packed_int(&mut object);
        }
        push_empty_packed_float(&mut object);
        for _ in 0..2 {
            push_empty_packed_int(&mut object);
        }
        push_u32_le(&mut object, 0);

        object.extend_from_slice(&[0_u8; 24]);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 0);
        object.extend_from_slice(&[0_u8; 8]);
        align_vec(&mut object, 4);
        object.extend_from_slice(&0_i64.to_le_bytes());
        push_u32_le(&mut object, 0);
        push_i32_le(&mut object, 0);

        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32_le(&mut metadata, 13);
        metadata.push(0);
        push_i32_le(&mut metadata, 1);
        push_i32_le(&mut metadata, 43);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32_le(&mut metadata, 1);
        align_vec_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32_le(&mut metadata, 0);
        for _ in 0..3 {
            push_i32_le(&mut metadata, 0);
        }
        metadata.push(0);

        finish_v22_asset(&metadata, &object)
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32_le(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align_vec(output, 4);
        }
    }

    fn push_empty_packed_float(output: &mut Vec<u8>) {
        push_u32_le(output, 0);
        output.extend_from_slice(&0_f32.to_le_bytes());
        output.extend_from_slice(&0_f32.to_le_bytes());
        push_i32_le(output, 0);
        align_vec(output, 4);
        output.push(0);
        align_vec(output, 4);
    }

    fn push_empty_packed_int(output: &mut Vec<u8>) {
        push_u32_le(output, 0);
        push_i32_le(output, 0);
        align_vec(output, 4);
        output.push(0);
        align_vec(output, 4);
    }

    fn synthetic_v22_text_asset() -> Vec<u8> {
        let mut object = Vec::new();
        push_i32_le(&mut object, 8);
        object.extend_from_slice(b"demo.lua");
        push_i32_le(&mut object, 7);
        object.extend_from_slice(b"payload");

        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32_le(&mut metadata, 13);
        metadata.push(0);
        push_i32_le(&mut metadata, 1);
        push_i32_le(&mut metadata, 49);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32_le(&mut metadata, 1);
        align_vec_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32_le(&mut metadata, 0);
        for _ in 0..3 {
            push_i32_le(&mut metadata, 0);
        }
        metadata.push(0);

        finish_v22_asset(&metadata, &object)
    }

    fn synthetic_v22_texture_array() -> Vec<u8> {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "layers");
        push_i32_le(&mut object, 0);
        object.extend_from_slice(&[0, 0]);
        align_vec(&mut object, 4);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 4);
        push_i32_le(&mut object, 1);
        push_i32_le(&mut object, 2);
        push_i32_le(&mut object, 2);
        push_i32_le(&mut object, 1);
        push_u32_le(&mut object, 16);
        object.extend_from_slice(&[0_u8; 24]);
        push_i32_le(&mut object, 7);
        object.push(1);
        align_vec(&mut object, 4);
        push_i32_le(&mut object, 16);
        object.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15, 16, 17, 18]);

        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32_le(&mut metadata, 13);
        metadata.push(0);
        push_i32_le(&mut metadata, 1);
        push_i32_le(&mut metadata, 187);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32_le(&mut metadata, 1);
        align_vec_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32_le(&mut metadata, 0);
        for _ in 0..3 {
            push_i32_le(&mut metadata, 0);
        }
        metadata.push(0);

        finish_v22_asset(&metadata, &object)
    }

    fn synthetic_v22_texture2d() -> Vec<u8> {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "image");
        push_i32_le(&mut object, 0);
        object.extend_from_slice(&[0, 0]);
        align_vec(&mut object, 4);
        push_i32_le(&mut object, 2);
        push_i32_le(&mut object, 2);
        push_u32_le(&mut object, 16);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 4);
        push_i32_le(&mut object, 1);
        object.extend_from_slice(&[0, 0, 0]);
        align_vec(&mut object, 4);
        push_aligned_string(&mut object, "");
        object.push(0);
        align_vec(&mut object, 4);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 1);
        push_i32_le(&mut object, 2);
        object.extend_from_slice(&[0_u8; 24]);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 0);
        push_i32_le(&mut object, 16);
        object.extend_from_slice(&[
            255, 0, 0, 1, 0, 255, 0, 2, // decoded bottom row
            0, 0, 255, 3, 255, 255, 255, 4, // decoded top row
        ]);

        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32_le(&mut metadata, 13);
        metadata.push(0);
        push_i32_le(&mut metadata, 1);
        push_i32_le(&mut metadata, 28);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_le_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        push_i32_le(&mut metadata, 1);
        align_vec_with_base(&mut metadata, 48, 4);
        metadata.extend_from_slice(&7_i64.to_le_bytes());
        metadata.extend_from_slice(&0_i64.to_le_bytes());
        metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
        push_i32_le(&mut metadata, 0);
        for _ in 0..3 {
            push_i32_le(&mut metadata, 0);
        }
        metadata.push(0);

        finish_v22_asset(&metadata, &object)
    }

    fn finish_v22_asset(metadata: &[u8], object: &[u8]) -> Vec<u8> {
        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(object.len()).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn push_i32_le(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32_le(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn read_i32_le(input: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes(input[offset..offset + 4].try_into().unwrap())
    }

    fn read_i64_le(input: &[u8], offset: usize) -> i64 {
        i64::from_le_bytes(input[offset..offset + 8].try_into().unwrap())
    }

    fn align_vec(output: &mut Vec<u8>, alignment: usize) {
        while !output.len().is_multiple_of(alignment) {
            output.push(0);
        }
    }

    fn align_vec_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + output.len()).is_multiple_of(alignment) {
            output.push(0);
        }
    }
}
