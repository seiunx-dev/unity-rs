use std::collections::{BTreeMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use assetstudio_core::animation_graph::{AnimationGraphLimits, build_animation_graph};
use assetstudio_core::bundle::{BundleHeader, UnityFsBundle};
use assetstudio_core::compression::{
    CompressionLimits, ZipContainer, decompress_brotli, decompress_gzip,
};
use assetstudio_core::cubism_moc::{CubismMoc, CubismMocReadLimits, try_read_cubism_moc};
use assetstudio_core::endian::{Endian, EndianReader};
use assetstudio_core::export::{
    AudioExportFormat, ExportMode, ExportOptions, FilenameFormat, export_collection,
};
use assetstudio_core::extraction::{ExtractionOptions, extract_path};
use assetstudio_core::fbx_ascii::{
    write_model_ir_fbx_ascii_with_animations, write_model_ir_fbx_ascii_with_textures,
};
use assetstudio_core::fbx_binary_scene::write_model_ir_fbx_binary_full;
use assetstudio_core::file_type::{FileDetection, FileType, HEADER_SCAN_LENGTH, detect_file_type};
use assetstudio_core::image_export::{ImageFormat, ImageRowOrder, write_rgba_image};
use assetstudio_core::live2d_package::{Live2dPackage, Live2dPackageLimits, build_live2d_packages};
use assetstudio_core::loader::{AssetCollection, AssetLoadOptions, LoadFailurePolicy};
use assetstudio_core::model_animation::{
    ModelAnimationLimits, ModelAnimationSet, build_model_animations,
};
use assetstudio_core::model_export::{
    ModelExportCandidate, ModelExportPlanLimits, plan_animator_exports, plan_split_object_exports,
};
use assetstudio_core::model_ir::{ModelIrLimits, build_model_ir, build_model_ir_for_game_object};
use assetstudio_core::monobehaviour::MONO_BEHAVIOUR_CLASS_ID;
use assetstudio_core::obj_scene::{write_model_ir_mtl, write_model_ir_obj};
use assetstudio_core::scene_hierarchy::{
    SceneHierarchy, SceneHierarchyLimits, SceneHierarchyNode, SceneObjectKey, build_scene_hierarchy,
};
use assetstudio_core::scene_textures::{SceneTextureLimits, SceneTextureNames, SceneTextureSet};
use assetstudio_core::serialized::SerializedFile;
use assetstudio_core::source::Region;
use assetstudio_core::texture::TextureReadLimits;
use assetstudio_core::unity_version::UnityVersion;
use assetstudio_core::web_file::WebFile;
use assetstudio_core::{Error, Result};

const MAX_DIRECTORY_FILES: usize = 1_000_000;
const MAX_DIRECTORIES: usize = 1_000_000;
const MAX_DIRECTORY_ENTRIES: usize = 2_000_000;
const MAX_COMPRESSION_DEPTH: usize = 16;
const MAX_SCENE_OUTPUT_DEPTH: usize = 4_096;
const MAX_SCENE_OUTPUT_NODES: usize = 1_000_000;
const MAX_LIVE2D_CANDIDATES: usize = 1_000_000;
const MAX_LIVE2D_OUTPUT_MODELS: usize = 100_000;
const MAX_LIVE2D_MODEL_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LIVE2D_TOTAL_OUTPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_LIVE2D_BASE_NAME_BYTES: usize = 180;
const MAX_LIVE2D_TEMPORARY_ATTEMPTS: u64 = 1_024;
const DEFAULT_FBX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FBX_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FBX_TEMPORARY_ATTEMPTS: u64 = 1_024;
const MAX_FBX_BATCH_CANDIDATES: usize = 1_000_000;
const MAX_FBX_BATCH_TOTAL_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_LIVE2D_PACKAGE_OUTPUTS: usize = 100_000;
const MAX_LIVE2D_PACKAGE_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LIVE2D_PACKAGE_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_LIVE2D_PACKAGE_IMAGE_WORKING_BYTES: u64 = 512 * 1024 * 1024;
const EXIT_RUNTIME_ERROR: u8 = 1;
const EXIT_USAGE_ERROR: u8 = 2;
const EXIT_PARTIAL_FAILURE: u8 = 3;

#[derive(Debug)]
enum CliError {
    Usage(String),
    Runtime(Error),
    Partial {
        operation: &'static str,
        failures: usize,
    },
}

impl CliError {
    fn is_broken_pipe(&self) -> bool {
        matches!(
            self,
            Self::Runtime(Error::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe
        )
    }
}

impl From<Error> for CliError {
    fn from(error: Error) -> Self {
        Self::Runtime(error)
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::Runtime(Error::Io(error))
    }
}

type CliResult<T> = std::result::Result<T, CliError>;

fn main() -> ExitCode {
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    let result = run(&mut output);
    let result = match result {
        Ok(()) => output.flush().map_err(Error::from).map_err(CliError::from),
        Err(error) => {
            let _ = output.flush();
            Err(error)
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(CliError::Usage(message)) => {
            eprintln!("assetstudio: {message}");
            ExitCode::from(EXIT_USAGE_ERROR)
        }
        Err(CliError::Runtime(error)) => {
            eprintln!("assetstudio: {error}");
            ExitCode::from(EXIT_RUNTIME_ERROR)
        }
        Err(CliError::Partial {
            operation,
            failures,
        }) => {
            eprintln!("assetstudio: {operation} completed with {failures} failure(s)");
            ExitCode::from(EXIT_PARTIAL_FAILURE)
        }
    }
}

fn run(output: &mut impl Write) -> CliResult<()> {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    run_with_arguments(&arguments, output)
}

fn run_with_arguments(arguments: &[OsString], output: &mut impl Write) -> CliResult<()> {
    let (arguments, load) = split_load_options(arguments)?;
    match parse_cli_arguments(&arguments)? {
        CliCommand::Help => print_help(output).map_err(CliError::from),
        CliCommand::Inspect(path) => inspect_path(&path, output),
        CliCommand::Info(path) => report_collection(&path, false, &load, output),
        CliCommand::List(path) => report_collection(&path, true, &load, output),
        CliCommand::Scene(path) => report_scene(&path, &load, output),
        CliCommand::Fbx(command) => export_fbx(&command, &load, output),
        CliCommand::Obj(command) => export_obj(&command, &load, output),
        CliCommand::FbxBatch(command) => export_fbx_batch(&command, &load, output),
        CliCommand::Live2d(command) => {
            export_live2d(&command.input, &command.output, &load, output)
        }
        CliCommand::Live2dPackage(command) => {
            export_live2d_packages(&command.input, &command.output, &load, output)
        }
        CliCommand::Export(command) => export_path(
            &command.input,
            &command.output,
            command.options,
            &load,
            output,
        ),
        CliCommand::Extract(command) => {
            extract_path_cli(&command.input, &command.output, command.options, output)
        }
    }
}

/// Options that apply to how an input is opened, whichever command runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LoadOptions {
    unity_version: Option<UnityVersion>,
}

/// Removes the load options from the argument list before command parsing.
///
/// These apply to every command that opens a collection, so handling them once
/// here keeps each command parser unaware of them and makes the flag work with
/// the legacy `<input> -m <mode>` spellings too.
fn split_load_options(arguments: &[OsString]) -> CliResult<(Vec<OsString>, LoadOptions)> {
    const FLAG: &str = "--unity-version";
    let mut remaining = Vec::new();
    let mut load = LoadOptions::default();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let text = argument.to_str();
        let value = if text == Some(FLAG) {
            arguments
                .next()
                .ok_or_else(|| {
                    CliError::Usage(format!("{FLAG} requires a version such as 2022.3.62f1"))
                })?
                .to_str()
        } else if let Some(value) = text.and_then(|text| text.strip_prefix("--unity-version=")) {
            Some(value)
        } else {
            remaining.push(argument.clone());
            continue;
        };
        let value =
            value.ok_or_else(|| CliError::Usage(format!("{FLAG} value must be valid UTF-8")))?;
        if load.unity_version.is_some() {
            return Err(CliError::Usage(format!("{FLAG} was given more than once")));
        }
        load.unity_version = Some(UnityVersion::from_str(value).map_err(|error| {
            CliError::Usage(format!(
                "{FLAG} value {value:?} is not a Unity version: {error}"
            ))
        })?);
    }
    Ok((remaining, load))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliCommand {
    Help,
    Inspect(PathBuf),
    Info(PathBuf),
    List(PathBuf),
    Scene(PathBuf),
    Fbx(FbxCommand),
    Obj(ObjCommand),
    FbxBatch(FbxBatchCommand),
    Live2d(Live2dCommand),
    Live2dPackage(Live2dCommand),
    Export(ExportCommand),
    Extract(ExtractCommand),
}

fn parse_cli_arguments(arguments: &[OsString]) -> CliResult<CliCommand> {
    let Some(command) = arguments.first() else {
        return Err(CliError::Usage(
            "an input path or command is required (try --help)".to_owned(),
        ));
    };

    if command == "-h" || command == "--help" {
        if arguments.len() == 1 {
            return Ok(CliCommand::Help);
        }
        return Err(CliError::Usage(format!(
            "unexpected argument after --help: {}",
            arguments[1].to_string_lossy()
        )));
    }

    match command.to_str() {
        Some("inspect") => parse_read_command("inspect", &arguments[1..], CliCommand::Inspect),
        Some("info") => parse_read_command("info", &arguments[1..], CliCommand::Info),
        Some("list") => parse_read_command("list", &arguments[1..], CliCommand::List),
        Some("scene") => parse_read_command("scene", &arguments[1..], CliCommand::Scene),
        Some("obj") => parse_model_arguments(&arguments[1..], "obj")
            .map(CliCommand::Obj)
            .map_err(|error| CliError::Usage(error.to_string())),
        Some("fbx") => parse_model_arguments(&arguments[1..], "fbx")
            .map(CliCommand::Fbx)
            .map_err(|error| CliError::Usage(error.to_string())),
        Some("split-objects") => parse_fbx_batch_arguments("split-objects", &arguments[1..])
            .map(CliCommand::FbxBatch)
            .map_err(|error| CliError::Usage(error.to_string())),
        Some("animator") => parse_fbx_batch_arguments("animator", &arguments[1..])
            .map(CliCommand::FbxBatch)
            .map_err(|error| CliError::Usage(error.to_string())),
        Some("live2d") => parse_live2d_arguments(&arguments[1..])
            .map(CliCommand::Live2d)
            .map_err(|error| CliError::Usage(error.to_string())),
        Some("live2d-package") => parse_live2d_package_arguments(&arguments[1..])
            .map(CliCommand::Live2dPackage)
            .map_err(|error| CliError::Usage(error.to_string())),
        Some("export") => parse_export_arguments(&arguments[1..])
            .map(CliCommand::Export)
            .map_err(|error| CliError::Usage(error.to_string())),
        Some("extract") => parse_extract_arguments(&arguments[1..])
            .map(CliCommand::Extract)
            .map_err(|error| CliError::Usage(error.to_string())),
        Some(value) if value.starts_with('-') => Err(CliError::Usage(format!(
            "unknown option: {} (try --help)",
            command.to_string_lossy()
        ))),
        _ => parse_bare_or_legacy_arguments(arguments),
    }
}

fn parse_read_command(
    name: &'static str,
    arguments: &[OsString],
    constructor: fn(PathBuf) -> CliCommand,
) -> CliResult<CliCommand> {
    let path = match arguments {
        [path] => path,
        [separator, path] if separator == "--" => path,
        [] => {
            return Err(CliError::Usage(format!("{name} requires an input path")));
        }
        _ => {
            return Err(CliError::Usage(format!(
                "{name} accepts exactly one input path"
            )));
        }
    };
    if path.to_str().is_some_and(|value| value.starts_with('-'))
        && !matches!(arguments, [separator, _] if separator == "--")
    {
        return Err(CliError::Usage(format!(
            "{name} input path begins with '-'; pass it after --"
        )));
    }
    Ok(constructor(PathBuf::from(path)))
}

fn parse_bare_or_legacy_arguments(arguments: &[OsString]) -> CliResult<CliCommand> {
    let input = PathBuf::from(&arguments[0]);
    if arguments.len() == 1 {
        return Ok(CliCommand::Inspect(input));
    }

    let mut mode: Option<String> = None;
    let mut output = None;
    let mut overwrite_existing = false;
    let mut restore_text_asset_extension = true;
    let mut index = 1;
    while index < arguments.len() {
        let argument = &arguments[index];
        match argument.to_str() {
            Some("-m" | "--mode") => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    CliError::Usage(format!("{} requires a value", argument.to_string_lossy()))
                })?;
                if mode.replace(value.to_string_lossy().into_owned()).is_some() {
                    return Err(CliError::Usage(
                        "legacy mode may only be specified once".to_owned(),
                    ));
                }
            }
            Some("-o" | "--output") => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    CliError::Usage(format!("{} requires a value", argument.to_string_lossy()))
                })?;
                if output.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::Usage(
                        "legacy output may only be specified once".to_owned(),
                    ));
                }
            }
            Some("-r" | "--overwrite-existing") => overwrite_existing = true,
            Some("--not-restore-extension") => restore_text_asset_extension = false,
            _ => {
                return Err(CliError::Usage(format!(
                    "unsupported legacy argument: {}",
                    argument.to_string_lossy()
                )));
            }
        }
        index += 1;
    }

    let mode = mode.as_deref().unwrap_or(if output.is_some() {
        "export"
    } else {
        "inspect"
    });
    match mode.to_ascii_lowercase().as_str() {
        "inspect" if output.is_none() && !overwrite_existing && restore_text_asset_extension => {
            Ok(CliCommand::Inspect(input))
        }
        "info" if output.is_none() && !overwrite_existing && restore_text_asset_extension => {
            Ok(CliCommand::Info(input))
        }
        "inspect" | "info" => Err(CliError::Usage(format!(
            "legacy {mode} mode is read-only and does not accept export options"
        ))),
        "extract" => parse_legacy_extract(
            input,
            output,
            overwrite_existing,
            restore_text_asset_extension,
        ),
        "animator" | "splitobjects" => parse_legacy_fbx_batch(input, output, mode),
        "export" | "raw" | "exportraw" | "dump" => {
            let output = output.ok_or_else(|| {
                CliError::Usage(
                    "legacy write modes require -o/--output; implicit ASExport creation is disabled"
                        .to_owned(),
                )
            })?;
            let mut options = ExportOptions {
                overwrite_existing,
                restore_text_asset_extension,
                ..ExportOptions::default()
            };
            options.mode = match mode.to_ascii_lowercase().as_str() {
                "export" => ExportMode::Auto,
                "raw" | "exportraw" => ExportMode::Raw,
                "dump" => ExportMode::DumpText,
                _ => unreachable!("matched legacy export modes"),
            };
            Ok(CliCommand::Export(ExportCommand {
                input,
                output,
                options,
            }))
        }
        _ => Err(CliError::Usage(format!(
            "legacy mode {mode:?} is not implemented by the native CLI"
        ))),
    }
}

fn parse_legacy_fbx_batch(
    input: PathBuf,
    output: Option<PathBuf>,
    mode: &str,
) -> CliResult<CliCommand> {
    let output = output.ok_or_else(|| {
        CliError::Usage(
            "legacy FBX modes require -o/--output; implicit ASExport creation is disabled"
                .to_owned(),
        )
    })?;
    let animator = mode.eq_ignore_ascii_case("animator");
    Ok(CliCommand::FbxBatch(FbxBatchCommand {
        input,
        output,
        mode: if animator {
            FbxBatchMode::Animator
        } else {
            FbxBatchMode::SplitObjects
        },
        maximum_file_bytes: DEFAULT_FBX_OUTPUT_BYTES,
        include_animations: animator,
        textures: true,
        texture_format: ImageFormat::Png,
    }))
}

fn parse_legacy_extract(
    input: PathBuf,
    output: Option<PathBuf>,
    overwrite_existing: bool,
    restore_text_asset_extension: bool,
) -> CliResult<CliCommand> {
    let output = output.ok_or_else(|| {
        CliError::Usage(
            "legacy extract mode requires -o/--output; implicit ASExtract creation is disabled"
                .to_owned(),
        )
    })?;
    if !restore_text_asset_extension {
        return Err(CliError::Usage(
            "--not-restore-extension is not valid for extract mode".to_owned(),
        ));
    }
    Ok(CliCommand::Extract(ExtractCommand {
        input,
        output,
        options: ExtractionOptions {
            overwrite_existing,
            ..ExtractionOptions::default()
        },
    }))
}

fn print_help(output: &mut impl Write) -> Result<()> {
    writeln!(
        output,
        "AssetStudio native Rust rewrite\n\n\
         Usage:\n  assetstudio inspect <file-or-directory>\n  assetstudio info <file-or-directory>\n  \
         assetstudio list <file-or-directory>\n  assetstudio scene <file-or-directory>\n  \
         assetstudio fbx <file-or-directory> <output.fbx> [options]\n  \
         assetstudio obj <file-or-directory> <output.obj> [options]\n  \
         assetstudio split-objects <file-or-directory> <output-directory> [options]\n  \
         assetstudio animator <file-or-directory> <output-directory> [options]\n  \
         assetstudio <file-or-directory>\n  \
         assetstudio export <file-or-directory> <output-directory> [options]\n  \
         assetstudio extract <file-or-directory> <output-directory> [--overwrite]\n  \
         assetstudio live2d <file-or-directory> <output-directory>\n  \
         assetstudio live2d-package <file-or-directory> <output-directory>\n\n\
         Read-only commands:\n  inspect  Show container and serialized-file structure\n  \
         info     Summarize serialized files, Unity versions, and class counts\n  \
         list     List every discovered serialized object\n  \
         scene    Print the assembled GameObject hierarchy and model bindings\n\n\
         Load options (accepted by every command that opens a collection):\n  \
         --unity-version <VERSION>   Parse against this version, for example 2022.3.62f1.\n  \
         Required for files whose own version was stripped at build time, and\n  \
         overrides both the declared version and any enclosing bundle revision.\n\n\
         FBX export:\n  Writes deterministic ASCII FBX 7.4 for transform hierarchies, resident\n  \
         triangle meshes, submeshes, material slots, normals, UV0, local TRS, direct/hash bones,\n  \
         skinning, static blend shapes, explicit/packed legacy curves, and streamed/dense/constant\n  \
         Transform or blend-shape samples.\n  \
         Material textures are decoded and written beside the FBX, which references\n  \
         them by file name.\n  \
         FBX options:\n  --maximum-output-bytes <N>  N must be a positive integer no greater\n  \
         than 536870912; the default is 16777216 bytes\n  \
         --no-textures                 Write the model without its textures\n  \
         --texture-format <FORMAT>     jpg|jpeg|png|bmp|tga|webp|raw-rgba; the default is png\n  \
         --binary                      Write FBX 7.4's binary encoding instead of its text one\n  \
         Existing files are never overwritten.\n\n\
         OBJ export:\n  Writes the whole model as Wavefront OBJ with node transforms baked into\n  \
         world space, a companion .mtl under the same stem, and the same sibling textures.\n  \
         Face references name only the channels the mesh has, unlike the single-mesh\n  \
         .obj the export command writes, which mirrors the managed writer exactly.\n  \
         It takes the same options as fbx, except --binary: OBJ has one encoding.\n\n\
         Batch FBX options:\n  --maximum-output-bytes <N>  Per-file limit\n  \
         --no-animations               Omit selected animation clips\n  \
         --no-textures                 Write the models without their textures\n  \
         --texture-format <FORMAT>     As above; textures are shared across the batch\n\n\
         Export options:\n  --mode <auto|raw|typetree-json|dump-text>\n  \
         --filename <asset-name|asset-name-path-id|path-id>\n  --overwrite\n  \
         --image-format <jpg|jpeg|png|bmp|tga|webp|raw-rgba>\n  \
         --jpeg-quality <1-100>\n  --no-restore-text-extension\n  \
         --audio-format <auto|raw|wav>\n  \
         --compact-json\n\n\
         Extract options:\n  --overwrite\n\n\
         Live2D export:\n  Exports only MonoBehaviours whose resolved MonoScript class is CubismMoc.\n  \
         Existing files are never overwritten.\n  \
         live2d-package exports verified MOC, texture PNG, model3.json, expression, motion,\n  \
         physics, pose, and display-info files when embedded or supplied schemas are available.\n\n\
         Legacy compatibility:\n  assetstudio <input> -m info\n  \
         assetstudio <input> -m <export|exportRaw|dump|extract|animator|splitObjects> -o <output>\n  \
         Implicit ASExport/ASExtract directories are never created. Legacy Animator and\n  \
         SplitObjects modes require an explicit output directory.\n\n\
         The default export mode prefers TextAsset bytes and TypeTree JSON, with raw \
         object data as a fallback. Images default to PNG. Existing files are not \
         overwritten by default."
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExportCommand {
    input: PathBuf,
    output: PathBuf,
    options: ExportOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Live2dCommand {
    input: PathBuf,
    output: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FbxCommand {
    input: PathBuf,
    output: PathBuf,
    maximum_output_bytes: u64,
    textures: bool,
    texture_format: ImageFormat,
    /// Write FBX 7.4's binary encoding rather than its text one.
    ///
    /// `obj` ignores this; only the FBX writers have two encodings.
    binary: bool,
}

/// `obj` takes the same options as `fbx`; only the writer differs.
type ObjCommand = FbxCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FbxBatchMode {
    SplitObjects,
    Animator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FbxBatchCommand {
    input: PathBuf,
    output: PathBuf,
    mode: FbxBatchMode,
    maximum_file_bytes: u64,
    include_animations: bool,
    textures: bool,
    texture_format: ImageFormat,
}

#[derive(Debug, Clone)]
struct ExtractCommand {
    input: PathBuf,
    output: PathBuf,
    options: ExtractionOptions,
}

impl PartialEq for ExtractCommand {
    fn eq(&self, other: &Self) -> bool {
        let decoders_equal = match (&self.options.oodle_decoder, &other.options.oodle_decoder) {
            (Some(left), Some(right)) => std::sync::Arc::ptr_eq(left, right),
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        self.input == other.input
            && self.output == other.output
            && self.options.limits == other.options.limits
            && self.options.overwrite_existing == other.options.overwrite_existing
            && decoders_equal
    }
}

impl Eq for ExtractCommand {}

fn parse_live2d_arguments(arguments: &[OsString]) -> Result<Live2dCommand> {
    parse_live2d_write_arguments("live2d", arguments)
}

fn parse_model_arguments(arguments: &[OsString], command_name: &str) -> Result<FbxCommand> {
    let mut positional = Vec::new();
    let mut maximum_output_bytes = DEFAULT_FBX_OUTPUT_BYTES;
    let mut saw_maximum = false;
    let mut textures = true;
    let mut texture_format = ImageFormat::Png;
    let mut binary = false;
    let mut parse_options = true;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "--maximum-output-bytes" {
            if saw_maximum {
                return Err(Error::invalid_data(
                    "--maximum-output-bytes may only be specified once",
                ));
            }
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--maximum-output-bytes requires a value"))?;
            let value = value.to_str().ok_or_else(|| {
                Error::invalid_data("--maximum-output-bytes must be valid UTF-8 digits")
            })?;
            maximum_output_bytes = value.parse::<u64>().map_err(|_| {
                Error::invalid_data("--maximum-output-bytes must be a positive integer")
            })?;
            if maximum_output_bytes == 0 || maximum_output_bytes > MAX_FBX_OUTPUT_BYTES {
                return Err(Error::invalid_data(format!(
                    "--maximum-output-bytes must be between 1 and {MAX_FBX_OUTPUT_BYTES}"
                )));
            }
            saw_maximum = true;
        } else if parse_options && argument == "--no-textures" {
            textures = false;
        } else if parse_options && argument == "--binary" {
            // Rejected for `obj` rather than ignored: an option that silently
            // does nothing is worse than one that says it does not apply.
            if command_name != "fbx" {
                return Err(Error::invalid_data(
                    "--binary applies to fbx only; OBJ has a single text encoding",
                ));
            }
            binary = true;
        } else if parse_options && argument == "--texture-format" {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--texture-format requires a value"))?;
            texture_format = parse_image_format(value)?;
        } else if parse_options
            && argument
                .to_str()
                .is_some_and(|value| value.starts_with('-'))
        {
            return Err(Error::invalid_data(format!(
                "unknown {command_name} option: {}",
                argument.to_string_lossy()
            )));
        } else {
            positional.push(PathBuf::from(argument));
        }
        index += 1;
    }
    if positional.len() != 2 {
        return Err(Error::invalid_data(format!(
            "{command_name} requires an input path and an output .{command_name} path"
        )));
    }
    if positional[1]
        .extension()
        .and_then(|value| value.to_str())
        .is_none_or(|value| !value.eq_ignore_ascii_case(command_name))
    {
        return Err(Error::invalid_data(format!(
            "{command_name} output path must end in .{command_name}"
        )));
    }
    Ok(FbxCommand {
        input: positional.remove(0),
        output: positional.remove(0),
        maximum_output_bytes,
        textures,
        texture_format,
        binary,
    })
}

fn parse_fbx_batch_arguments(
    command_name: &str,
    arguments: &[OsString],
) -> Result<FbxBatchCommand> {
    let mode = if command_name == "animator" {
        FbxBatchMode::Animator
    } else {
        FbxBatchMode::SplitObjects
    };
    let mut positional = Vec::new();
    let mut maximum_file_bytes = DEFAULT_FBX_OUTPUT_BYTES;
    let mut include_animations = mode == FbxBatchMode::Animator;
    let mut textures = true;
    let mut texture_format = ImageFormat::Png;
    let mut parse_options = true;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "--maximum-output-bytes" {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--maximum-output-bytes requires a value"))?;
            maximum_file_bytes = value
                .to_str()
                .ok_or_else(|| Error::invalid_data("output limit must be UTF-8 digits"))?
                .parse::<u64>()
                .map_err(|_| Error::invalid_data("output limit must be a positive integer"))?;
            if maximum_file_bytes == 0 || maximum_file_bytes > MAX_FBX_OUTPUT_BYTES {
                return Err(Error::invalid_data(format!(
                    "--maximum-output-bytes must be between 1 and {MAX_FBX_OUTPUT_BYTES}"
                )));
            }
        } else if parse_options && argument == "--no-animations" {
            include_animations = false;
        } else if parse_options && argument == "--no-textures" {
            textures = false;
        } else if parse_options && argument == "--texture-format" {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--texture-format requires a value"))?;
            texture_format = parse_image_format(value)?;
        } else if parse_options
            && argument
                .to_str()
                .is_some_and(|value| value.starts_with('-'))
        {
            return Err(Error::invalid_data(format!(
                "unknown {command_name} option: {}",
                argument.to_string_lossy()
            )));
        } else {
            positional.push(PathBuf::from(argument));
        }
        index += 1;
    }
    if positional.len() != 2 {
        return Err(Error::invalid_data(format!(
            "{command_name} requires an input path and output directory"
        )));
    }
    Ok(FbxBatchCommand {
        input: positional.remove(0),
        output: positional.remove(0),
        mode,
        maximum_file_bytes,
        include_animations,
        textures,
        texture_format,
    })
}

fn parse_live2d_package_arguments(arguments: &[OsString]) -> Result<Live2dCommand> {
    parse_live2d_write_arguments("live2d-package", arguments)
}

fn parse_live2d_write_arguments(
    command_name: &str,
    arguments: &[OsString],
) -> Result<Live2dCommand> {
    let mut positional = Vec::new();
    let mut parse_options = true;
    for argument in arguments {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options
            && argument
                .to_str()
                .is_some_and(|value| value.starts_with('-'))
        {
            return Err(Error::invalid_data(format!(
                "unknown {command_name} option: {}",
                argument.to_string_lossy()
            )));
        } else {
            positional.push(PathBuf::from(argument));
        }
    }
    if positional.len() != 2 {
        return Err(Error::invalid_data(format!(
            "{command_name} requires an input path and an output directory"
        )));
    }
    Ok(Live2dCommand {
        input: positional.remove(0),
        output: positional.remove(0),
    })
}

fn parse_extract_arguments(arguments: &[OsString]) -> Result<ExtractCommand> {
    let mut options = ExtractionOptions::default();
    let mut positional = Vec::new();
    let mut parse_options = true;
    for argument in arguments {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "--overwrite" {
            options.overwrite_existing = true;
        } else if parse_options
            && argument
                .to_str()
                .is_some_and(|value| value.starts_with('-'))
        {
            return Err(Error::invalid_data(format!(
                "unknown extract option: {}",
                argument.to_string_lossy()
            )));
        } else {
            positional.push(PathBuf::from(argument));
        }
    }
    if positional.len() != 2 {
        return Err(Error::invalid_data(
            "extract requires an input path and an output directory",
        ));
    }
    Ok(ExtractCommand {
        input: positional.remove(0),
        output: positional.remove(0),
        options,
    })
}

fn parse_export_arguments(arguments: &[OsString]) -> Result<ExportCommand> {
    let mut options = ExportOptions::default();
    let mut positional = Vec::new();
    let mut parse_options = true;
    let mut index = 0;

    while index < arguments.len() {
        let argument = &arguments[index];
        if parse_options && argument == "--" {
            parse_options = false;
            index += 1;
            continue;
        }

        if parse_options && argument == "--overwrite" {
            options.overwrite_existing = true;
        } else if parse_options && argument == "--no-restore-text-extension" {
            options.restore_text_asset_extension = false;
        } else if parse_options && argument == "--compact-json" {
            options.pretty_json = false;
        } else if parse_options && argument == "--mode" {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--mode requires a value"))?;
            options.mode = parse_export_mode(value)?;
        } else if parse_options && argument == "--filename" {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--filename requires a value"))?;
            options.filename_format = parse_filename_format(value)?;
        } else if parse_options && argument == "--image-format" {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--image-format requires a value"))?;
            options.image_format = parse_image_format(value)?;
        } else if parse_options && argument == "--jpeg-quality" {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--jpeg-quality requires a value"))?;
            options.jpeg_quality = parse_jpeg_quality(value)?;
        } else if parse_options && argument == "--audio-format" {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--audio-format requires a value"))?;
            options.audio_format = parse_audio_format(value)?;
        } else if parse_options
            && argument
                .to_str()
                .is_some_and(|value| value.starts_with('-'))
        {
            return Err(Error::invalid_data(format!(
                "unknown export option: {}",
                argument.to_string_lossy()
            )));
        } else {
            positional.push(PathBuf::from(argument));
        }
        index += 1;
    }

    if positional.len() != 2 {
        return Err(Error::invalid_data(
            "export requires an input path and an output directory",
        ));
    }

    Ok(ExportCommand {
        input: positional.remove(0),
        output: positional.remove(0),
        options,
    })
}

fn parse_export_mode(value: &OsString) -> Result<ExportMode> {
    match value.to_str() {
        Some("auto") => Ok(ExportMode::Auto),
        Some("raw") => Ok(ExportMode::Raw),
        Some("typetree-json") => Ok(ExportMode::TypeTreeJson),
        Some("dump-text") => Ok(ExportMode::DumpText),
        _ => Err(Error::invalid_data(format!(
            "invalid export mode: {} (expected auto, raw, typetree-json, or dump-text)",
            value.to_string_lossy()
        ))),
    }
}

fn parse_filename_format(value: &OsString) -> Result<FilenameFormat> {
    match value.to_str() {
        Some("asset-name") => Ok(FilenameFormat::AssetName),
        Some("asset-name-path-id") => Ok(FilenameFormat::AssetNamePathId),
        Some("path-id") => Ok(FilenameFormat::PathId),
        _ => Err(Error::invalid_data(format!(
            "invalid filename format: {} (expected asset-name, asset-name-path-id, or path-id)",
            value.to_string_lossy()
        ))),
    }
}

fn parse_image_format(value: &OsString) -> Result<ImageFormat> {
    match value.to_str() {
        Some("jpg" | "jpeg") => Ok(ImageFormat::Jpeg),
        Some("png") => Ok(ImageFormat::Png),
        Some("bmp") => Ok(ImageFormat::Bmp),
        Some("tga") => Ok(ImageFormat::Tga),
        Some("webp") => Ok(ImageFormat::Webp),
        Some("raw-rgba" | "raw_rgba" | "rgba") => Ok(ImageFormat::RawRgba),
        _ => Err(Error::invalid_data(format!(
            "invalid image format: {} (expected jpg, jpeg, png, bmp, tga, webp, or raw-rgba)",
            value.to_string_lossy()
        ))),
    }
}

fn parse_jpeg_quality(value: &OsString) -> Result<u8> {
    let text = value
        .to_str()
        .ok_or_else(|| Error::invalid_data("JPEG quality must be valid UTF-8"))?;
    let quality = text.parse::<u8>().map_err(|_| {
        Error::invalid_data(format!(
            "invalid JPEG quality {text:?} (expected an integer from 1 through 100)"
        ))
    })?;
    if !(1..=100).contains(&quality) {
        return Err(Error::invalid_data(format!(
            "invalid JPEG quality {quality} (expected an integer from 1 through 100)"
        )));
    }
    Ok(quality)
}

fn parse_audio_format(value: &OsString) -> Result<AudioExportFormat> {
    match value.to_str() {
        Some("auto") => Ok(AudioExportFormat::Auto),
        Some("raw" | "none") => Ok(AudioExportFormat::Raw),
        Some("wav" | "wave") => Ok(AudioExportFormat::Wav),
        _ => Err(Error::invalid_data(format!(
            "invalid audio format: {} (expected auto, raw, none, wav, or wave)",
            value.to_string_lossy()
        ))),
    }
}

fn export_path(
    input: &Path,
    output_directory: &Path,
    options: ExportOptions,
    load: &LoadOptions,
    output: &mut impl Write,
) -> CliResult<()> {
    let collection = load_asset_collection(input, load, output)?;
    let report = export_collection(&collection, output_directory, options)?;

    for record in &report.exported {
        writeln!(
            output,
            "exported {}::{} (class {}, {}) -> {}",
            escape_text(&record.source),
            record.path_id,
            record.class_id,
            record.payload_kind,
            record.output_path.display()
        )?;
    }
    for failure in &report.failures {
        writeln!(
            output,
            "failed {}::{} (class {}): {}",
            escape_text(&failure.source),
            failure.path_id,
            failure.class_id,
            failure.error
        )?;
    }
    // Listed rather than counted: an unsupported object is a statement about
    // this implementation, and the caller cannot act on a number alone.
    for declined in &report.unsupported {
        writeln!(
            output,
            "unsupported {}::{} (class {}): {}",
            escape_text(&declined.source),
            declined.path_id,
            declined.class_id,
            declined.error
        )?;
    }
    writeln!(
        output,
        "export summary: {} succeeded, {} unsupported, {} failed",
        report.exported.len(),
        report.unsupported.len(),
        report.failures.len()
    )?;

    if !report.failures.is_empty() {
        output.flush()?;
        return Err(CliError::Partial {
            operation: "export",
            failures: report.failures.len(),
        });
    }
    skipped_input_result("export", &collection)
}

fn export_fbx(command: &FbxCommand, load: &LoadOptions, output: &mut impl Write) -> CliResult<()> {
    let collection = load_asset_collection(&command.input, load, output)?;
    let hierarchy = build_scene_hierarchy(&collection, SceneHierarchyLimits::default())?;
    let model = build_model_ir(&collection, &hierarchy, ModelIrLimits::default())?;
    let graph = build_animation_graph(&collection, &hierarchy, AnimationGraphLimits::default())?;
    let animations =
        build_model_animations(&collection, &model, &graph, ModelAnimationLimits::default())?;
    let parent = prepare_fbx_output_parent(&command.output)?;
    let textures = if command.textures {
        SceneTextureSet::from_model(
            &collection,
            &model,
            command.texture_format,
            SceneTextureLimits::default(),
        )?
    } else {
        SceneTextureSet::default()
    };
    let mut temporary = FbxTemporaryFile::create(&parent)?;
    // Both encodings take the same scene, so the choice is only which writer
    // consumes it. The binary one takes the animation and texture sets by
    // option because it shares one entry point where ASCII has three.
    let written = if command.binary {
        write_model_ir_fbx_binary_full(
            &model,
            Some(&animations),
            (!textures.is_empty()).then_some(&textures),
            temporary.file_mut(),
            command.maximum_output_bytes,
        )?
    } else {
        write_model_ir_fbx_ascii_with_textures(
            &model,
            &animations,
            &textures,
            temporary.file_mut(),
            command.maximum_output_bytes,
        )?
    };
    temporary.file_mut().flush()?;
    temporary.file_mut().sync_all()?;
    temporary.close()?;
    temporary.persist_no_clobber(&command.output)?;
    // The FBX references its textures by file name, so they only resolve once
    // they sit beside it. Written after the model so a failed model export
    // leaves no orphaned images.
    let written_textures = textures.write_to_directory(&parent)?;
    writeln!(
        output,
        "exported {} FBX 7.4 ({written} bytes, {} animation clips) -> {}",
        if command.binary { "binary" } else { "ASCII" },
        animations.clips.len(),
        command.output.display()
    )?;
    report_model_textures(command.textures, &textures, written_textures.len(), output)?;
    skipped_input_result("FBX export", &collection)
}

fn export_obj(command: &ObjCommand, load: &LoadOptions, output: &mut impl Write) -> CliResult<()> {
    let collection = load_asset_collection(&command.input, load, output)?;
    let hierarchy = build_scene_hierarchy(&collection, SceneHierarchyLimits::default())?;
    let model = build_model_ir(&collection, &hierarchy, ModelIrLimits::default())?;
    let parent = prepare_fbx_output_parent(&command.output)?;
    let textures = if command.textures {
        SceneTextureSet::from_model(
            &collection,
            &model,
            command.texture_format,
            SceneTextureLimits::default(),
        )?
    } else {
        SceneTextureSet::default()
    };
    // The MTL sits beside the OBJ under the same stem, which is what `mtllib`
    // resolves against.
    let mtl_name = obj_material_library_name(&command.output)?;
    let mtl_path = parent.join(&mtl_name);

    let mut temporary = FbxTemporaryFile::create(&parent)?;
    let written = write_model_ir_obj(
        &model,
        Some(mtl_name.as_str()),
        temporary.file_mut(),
        command.maximum_output_bytes,
    )?;
    temporary.file_mut().flush()?;
    temporary.file_mut().sync_all()?;
    temporary.close()?;
    temporary.persist_no_clobber(&command.output)?;

    let mut temporary = FbxTemporaryFile::create(&parent)?;
    let mtl_written = write_model_ir_mtl(
        &model,
        &textures,
        temporary.file_mut(),
        command.maximum_output_bytes,
    )?;
    temporary.file_mut().flush()?;
    temporary.file_mut().sync_all()?;
    temporary.close()?;
    temporary.persist_no_clobber(&mtl_path)?;

    let written_textures = textures.write_to_directory(&parent)?;
    writeln!(
        output,
        "exported Wavefront OBJ ({written} bytes) -> {}",
        command.output.display()
    )?;
    writeln!(
        output,
        "  wrote the material library ({mtl_written} bytes) -> {}",
        mtl_path.display()
    )?;
    report_model_textures(command.textures, &textures, written_textures.len(), output)?;
    skipped_input_result("OBJ export", &collection)
}

/// The `mtllib` name for an OBJ destination: its file stem plus `.mtl`.
fn obj_material_library_name(destination: &Path) -> Result<String> {
    let stem = destination
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "OBJ output path has no usable file name: {}",
                destination.display()
            ))
        })?;
    Ok(format!("{stem}.mtl"))
}

/// Reports what happened to the model's textures.
///
/// A texture that resolved to something other than a `Texture2D`, or that
/// failed to decode, is skipped rather than failing the export, so the count
/// has to be visible or the result is a silent partial one. A texture already
/// present in the directory is left alone, which is why the written count can
/// be lower than the resolved count.
fn report_model_textures(
    requested: bool,
    textures: &SceneTextureSet,
    written: usize,
    output: &mut impl Write,
) -> io::Result<()> {
    if !requested {
        return Ok(());
    }
    if !textures.textures.is_empty() {
        writeln!(
            output,
            "  wrote {written} of {} texture file(s) beside the model",
            textures.textures.len()
        )?;
    }
    for skip in &textures.skipped {
        writeln!(
            output,
            "  note: texture {} of Material {}::{} was skipped: {}",
            skip.property, skip.material.file_index, skip.material.path_id, skip.reason
        )?;
    }
    Ok(())
}

fn export_fbx_batch(
    command: &FbxBatchCommand,
    load: &LoadOptions,
    output: &mut impl Write,
) -> CliResult<()> {
    let collection = load_asset_collection(&command.input, load, output)?;
    let hierarchy = build_scene_hierarchy(&collection, SceneHierarchyLimits::default())?;
    let candidates = match command.mode {
        FbxBatchMode::SplitObjects => {
            plan_split_object_exports(&hierarchy, ModelExportPlanLimits::default())?
        }
        FbxBatchMode::Animator => {
            plan_animator_exports(&hierarchy, ModelExportPlanLimits::default())?
        }
    };
    if candidates.len() > MAX_FBX_BATCH_CANDIDATES {
        return Err(Error::invalid_data(format!(
            "FBX batch has {} candidates, exceeding limit {MAX_FBX_BATCH_CANDIDATES}",
            candidates.len()
        ))
        .into());
    }
    if candidates.is_empty() {
        writeln!(output, "no matching FBX models found")?;
        return Ok(());
    }
    let graph = command
        .include_animations
        .then(|| build_animation_graph(&collection, &hierarchy, AnimationGraphLimits::default()))
        .transpose()?;
    let parent = prepare_fbx_output_parent(&command.output.join("placeholder"))?;
    let mut names = HashSet::new();
    names.try_reserve(candidates.len()).map_err(|error| {
        CliError::Runtime(Error::invalid_data(format!(
            "cannot allocate FBX output-name index: {error}"
        )))
    })?;
    let mut succeeded = 0_usize;
    let mut failures = 0_usize;
    let mut total_bytes = 0_u64;
    // One allocator across the batch: every model writes into the same
    // directory, so two textures that share a Unity name must still get
    // separate files.
    let mut texture_names = SceneTextureNames::default();
    let mut texture_files = 0_usize;
    for candidate in &candidates {
        let base = allocate_fbx_batch_name(candidate, &mut names)?;
        let destination = parent.join(format!("{base}.fbx"));
        match write_fbx_batch_candidate(
            &collection,
            &hierarchy,
            graph.as_ref(),
            candidate,
            command,
            &destination,
            total_bytes,
            &mut texture_names,
            &mut texture_files,
        ) {
            Ok(written) => {
                total_bytes = total_bytes.checked_add(written).ok_or_else(|| {
                    CliError::Runtime(Error::invalid_data("FBX batch byte count overflowed"))
                })?;
                succeeded = succeeded.checked_add(1).ok_or_else(|| {
                    CliError::Runtime(Error::invalid_data("FBX batch success count overflowed"))
                })?;
                writeln!(
                    output,
                    "exported {}::{} ({written} bytes) -> {}",
                    candidate.game_object.file_index,
                    candidate.game_object.path_id,
                    destination.display()
                )?;
            }
            Err(error) => {
                failures = failures.checked_add(1).ok_or_else(|| {
                    CliError::Runtime(Error::invalid_data("FBX batch failure count overflowed"))
                })?;
                writeln!(
                    output,
                    "failed {}::{}: {error}",
                    candidate.game_object.file_index, candidate.game_object.path_id
                )?;
            }
        }
    }
    writeln!(
        output,
        "FBX batch summary: {succeeded} exported, {failures} failed, {total_bytes} bytes, {texture_files} texture file(s)"
    )?;
    if failures != 0 {
        return Err(CliError::Partial {
            operation: "FBX batch export",
            failures,
        });
    }
    skipped_input_result("FBX batch export", &collection)
}

#[allow(clippy::too_many_arguments)]
fn write_fbx_batch_candidate(
    collection: &AssetCollection,
    hierarchy: &SceneHierarchy,
    graph: Option<&assetstudio_core::animation_graph::AnimationGraph>,
    candidate: &ModelExportCandidate,
    command: &FbxBatchCommand,
    destination: &Path,
    previously_written: u64,
    texture_names: &mut SceneTextureNames,
    texture_files: &mut usize,
) -> Result<u64> {
    let model = build_model_ir_for_game_object(
        collection,
        hierarchy,
        candidate.game_object,
        ModelIrLimits::default(),
    )?;
    let animations = graph
        .map(|graph| {
            build_model_animations(collection, &model, graph, ModelAnimationLimits::default())
        })
        .transpose()?;
    let mut temporary = FbxTemporaryFile::create(
        destination
            .parent()
            .ok_or_else(|| Error::invalid_data("FBX batch destination has no parent"))?,
    )?;
    let textures = if command.textures {
        SceneTextureSet::from_model_with_names(
            collection,
            &model,
            command.texture_format,
            SceneTextureLimits::default(),
            texture_names,
        )?
    } else {
        SceneTextureSet::default()
    };
    let written = match (&animations, textures.is_empty()) {
        (Some(animations), false) => write_model_ir_fbx_ascii_with_textures(
            &model,
            animations,
            &textures,
            temporary.file_mut(),
            command.maximum_file_bytes,
        )?,
        (Some(animations), true) => write_model_ir_fbx_ascii_with_animations(
            &model,
            animations,
            temporary.file_mut(),
            command.maximum_file_bytes,
        )?,
        (None, false) => write_model_ir_fbx_ascii_with_textures(
            &model,
            &ModelAnimationSet::default(),
            &textures,
            temporary.file_mut(),
            command.maximum_file_bytes,
        )?,
        (None, true) => assetstudio_core::fbx_ascii::write_model_ir_fbx_ascii(
            &model,
            temporary.file_mut(),
            command.maximum_file_bytes,
        )?,
    };
    let total = previously_written
        .checked_add(written)
        .ok_or_else(|| Error::invalid_data("FBX batch byte count overflowed"))?;
    if total > MAX_FBX_BATCH_TOTAL_BYTES {
        return Err(Error::invalid_data(format!(
            "FBX batch output exceeds {MAX_FBX_BATCH_TOTAL_BYTES} bytes"
        )));
    }
    temporary.file_mut().flush()?;
    temporary.file_mut().sync_all()?;
    temporary.close()?;
    temporary.persist_no_clobber(destination)?;
    let directory = destination
        .parent()
        .ok_or_else(|| Error::invalid_data("FBX batch destination has no parent"))?;
    *texture_files = texture_files
        .checked_add(textures.write_to_directory(directory)?.len())
        .ok_or_else(|| Error::invalid_data("FBX batch texture count overflowed"))?;
    Ok(written)
}

fn allocate_fbx_batch_name(
    candidate: &ModelExportCandidate,
    names: &mut HashSet<String>,
) -> Result<String> {
    let base = sanitize_live2d_base_name(&candidate.name);
    for suffix in 0_u64..=MAX_FBX_TEMPORARY_ATTEMPTS {
        let value = if suffix == 0 {
            fallible_fbx_name(&base)?
        } else {
            fallible_fbx_suffixed_name(&base, suffix)?
        };
        let portable = value.to_ascii_lowercase();
        if !names.contains(&portable) {
            names.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow FBX output-name index: {error}"))
            })?;
            names.insert(portable);
            return Ok(value);
        }
    }
    Err(Error::invalid_data(format!(
        "cannot allocate a unique FBX name for {:?}",
        candidate.game_object
    )))
}

fn fallible_fbx_name(base: &str) -> Result<String> {
    let mut output = String::new();
    output.try_reserve_exact(base.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate FBX output name: {error}"))
    })?;
    output.push_str(base);
    Ok(output)
}

fn fallible_fbx_suffixed_name(base: &str, suffix: u64) -> Result<String> {
    use std::fmt::Write as _;

    let mut output = String::new();
    output
        .try_reserve(base.len().saturating_add(24))
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate FBX output name: {error}"))
        })?;
    output.push_str(base);
    write!(output, "~{suffix}")
        .map_err(|error| Error::invalid_data(format!("cannot format FBX output name: {error}")))?;
    Ok(output)
}

fn prepare_fbx_output_parent(destination: &Path) -> Result<PathBuf> {
    let raw_parent = destination.parent().ok_or_else(|| {
        Error::invalid_data(format!(
            "FBX output path has no parent: {}",
            destination.display()
        ))
    })?;
    let parent = if raw_parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        raw_parent.to_owned()
    };
    let mut missing = Vec::new();
    let mut current = parent.as_path();
    loop {
        let candidate = current;
        match fs::symlink_metadata(candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(Error::invalid_data(format!(
                        "refusing FBX output through symbolic link: {}",
                        candidate.display()
                    )));
                }
                if !metadata.is_dir() {
                    return Err(Error::invalid_data(format!(
                        "FBX output ancestor is not a directory: {}",
                        candidate.display()
                    )));
                }
                fs::canonicalize(candidate)?;
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(candidate.to_owned());
                current = candidate.parent().ok_or_else(|| {
                    Error::invalid_data(format!(
                        "FBX output has no existing directory anchor: {}",
                        destination.display()
                    ))
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    for directory in missing.iter().rev() {
        match fs::create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(directory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::invalid_data(format!(
                "FBX output directory became unsafe while creating it: {}",
                directory.display()
            )));
        }
    }
    Ok(parent)
}

struct FbxTemporaryFile {
    path: PathBuf,
    file: Option<File>,
    persisted: bool,
}

impl FbxTemporaryFile {
    fn create(directory: &Path) -> Result<Self> {
        for sequence in 0..MAX_FBX_TEMPORARY_ATTEMPTS {
            let path = directory.join(format!(
                ".assetstudio-fbx-{}-{sequence}.tmp",
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
            "cannot allocate an FBX temporary file after {MAX_FBX_TEMPORARY_ATTEMPTS} attempts"
        )))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("FBX temporary file is open")
    }

    fn close(&mut self) -> Result<()> {
        self.file
            .take()
            .ok_or_else(|| Error::invalid_data("FBX temporary file was already closed"))?;
        Ok(())
    }

    fn persist_no_clobber(&mut self, destination: &Path) -> Result<()> {
        match fs::hard_link(&self.path, destination) {
            Ok(()) => {
                fs::remove_file(&self.path)?;
                self.persisted = true;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(Error::invalid_data(format!(
                    "refusing to overwrite existing FBX output: {}",
                    destination.display()
                )))
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for FbxTemporaryFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn extract_path_cli(
    input: &Path,
    output_directory: &Path,
    options: ExtractionOptions,
    output: &mut impl Write,
) -> CliResult<()> {
    let report = extract_path(input, output_directory, options)?;
    for record in &report.extracted {
        writeln!(
            output,
            "extracted {} ({} bytes) -> {}",
            escape_text(&record.source),
            record.bytes,
            record.output_path.display()
        )?;
    }
    for record in &report.skipped_existing {
        writeln!(
            output,
            "skipped existing {} -> {}",
            escape_text(&record.source),
            record.output_path.display()
        )?;
    }
    for failure in &report.failures {
        writeln!(
            output,
            "failed {}: {}",
            escape_text(&failure.source),
            failure.error
        )?;
    }
    writeln!(
        output,
        "extract summary: {} succeeded, {} skipped, {} failed, {} bytes",
        report.extracted.len(),
        report.skipped_existing.len(),
        report.failures.len(),
        report.output_bytes
    )?;
    if !report.failures.is_empty() {
        output.flush()?;
        return Err(CliError::Partial {
            operation: "extract",
            failures: report.failures.len(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Live2dCandidate {
    file_index: usize,
    object_index: usize,
}

#[derive(Debug, Default)]
struct Live2dExportState {
    claimed_paths: HashSet<String>,
    temporary_sequence: u64,
    output_ready: bool,
    models_found: usize,
    scheduled_bytes: u64,
    exported: usize,
    exported_bytes: u64,
    failures: usize,
}

fn export_live2d(
    input: &Path,
    output_directory: &Path,
    load: &LoadOptions,
    output: &mut impl Write,
) -> CliResult<()> {
    let collection = load_asset_collection(input, load, output)?;
    let candidates = collect_live2d_candidates(&collection)?;
    let read_limits = CubismMocReadLimits {
        maximum_model_bytes: MAX_LIVE2D_MODEL_OUTPUT_BYTES,
        ..CubismMocReadLimits::default()
    };
    let mut state = Live2dExportState::default();
    for candidate in candidates {
        export_live2d_candidate(
            &collection,
            output_directory,
            candidate,
            read_limits,
            &mut state,
            output,
        )?;
    }

    if state.models_found == 0 && state.failures == 0 {
        writeln!(output, "no CubismMoc models found")?;
    }
    writeln!(
        output,
        "live2d summary: {} exported, {} failed, {} bytes",
        state.exported, state.failures, state.exported_bytes
    )?;
    if state.failures != 0 {
        output.flush()?;
        return Err(CliError::Partial {
            operation: "live2d",
            failures: state.failures,
        });
    }
    skipped_input_result("live2d", &collection)
}

fn collect_live2d_candidates(collection: &AssetCollection) -> CliResult<Vec<Live2dCandidate>> {
    let mut candidates = Vec::new();
    for (file_index, loaded) in collection.serialized_files.iter().enumerate() {
        for (object_index, object) in loaded.file.objects.iter().enumerate() {
            if object.class_id != MONO_BEHAVIOUR_CLASS_ID {
                continue;
            }
            if candidates.len() >= MAX_LIVE2D_CANDIDATES {
                return Err(Error::invalid_data(format!(
                    "Live2D scan exceeds {MAX_LIVE2D_CANDIDATES} MonoBehaviour candidates"
                ))
                .into());
            }
            candidates.push(Live2dCandidate {
                file_index,
                object_index,
            });
        }
    }
    candidates.sort_unstable_by(|left, right| {
        let left_file = &collection.serialized_files[left.file_index];
        let right_file = &collection.serialized_files[right.file_index];
        left_file
            .path
            .cmp(&right_file.path)
            .then_with(|| {
                left_file.file.objects[left.object_index]
                    .path_id
                    .cmp(&right_file.file.objects[right.object_index].path_id)
            })
            .then_with(|| left.file_index.cmp(&right.file_index))
            .then_with(|| left.object_index.cmp(&right.object_index))
    });
    Ok(candidates)
}

fn export_live2d_candidate(
    collection: &AssetCollection,
    output_directory: &Path,
    candidate: Live2dCandidate,
    read_limits: CubismMocReadLimits,
    state: &mut Live2dExportState,
    output: &mut impl Write,
) -> CliResult<()> {
    let loaded = &collection.serialized_files[candidate.file_index];
    let object = &loaded.file.objects[candidate.object_index];
    let model = match try_read_cubism_moc(
        collection,
        candidate.file_index,
        candidate.object_index,
        read_limits,
    ) {
        Ok(Some(model)) => model,
        Ok(None) => return Ok(()),
        Err(error) => {
            return report_live2d_failure(
                state,
                &loaded.path,
                object.path_id,
                object.class_id,
                &error.to_string(),
                output,
            );
        }
    };
    if let Err(error) = charge_live2d_model(state, model.model_data.len()) {
        return report_live2d_failure(
            state,
            &loaded.path,
            object.path_id,
            object.class_id,
            &error.to_string(),
            output,
        );
    }

    if !state.output_ready {
        fs::create_dir_all(output_directory)?;
        state.output_ready = true;
    }
    let output_path = allocate_live2d_output_path(
        output_directory,
        &sanitize_live2d_base_name(&model.name),
        object.path_id,
        candidate,
        &mut state.claimed_paths,
    )?;
    match atomic_write_cubism_moc(
        &output_path,
        &model,
        MAX_LIVE2D_MODEL_OUTPUT_BYTES,
        &mut state.temporary_sequence,
    ) {
        Ok(written) => {
            state.exported = state.exported.checked_add(1).ok_or_else(|| {
                CliError::Runtime(Error::invalid_data("Live2D export count overflowed"))
            })?;
            state.exported_bytes = state.exported_bytes.checked_add(written).ok_or_else(|| {
                CliError::Runtime(Error::invalid_data("Live2D exported byte count overflowed"))
            })?;
            writeln!(
                output,
                "exported {}::{} (CubismMoc, {written} bytes) -> {}",
                escape_text(&loaded.path),
                object.path_id,
                output_path.display()
            )?;
            Ok(())
        }
        Err(error) => report_live2d_failure(
            state,
            &loaded.path,
            object.path_id,
            object.class_id,
            &error.to_string(),
            output,
        ),
    }
}

fn charge_live2d_model(state: &mut Live2dExportState, model_bytes: u64) -> Result<()> {
    state.models_found = state
        .models_found
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("Live2D model count overflowed"))?;
    if state.models_found > MAX_LIVE2D_OUTPUT_MODELS {
        return Err(Error::invalid_data(format!(
            "Live2D output exceeds {MAX_LIVE2D_OUTPUT_MODELS} models"
        )));
    }
    let next_bytes = state
        .scheduled_bytes
        .checked_add(model_bytes)
        .ok_or_else(|| Error::invalid_data("Live2D output byte count overflowed"))?;
    if next_bytes > MAX_LIVE2D_TOTAL_OUTPUT_BYTES {
        return Err(Error::invalid_data(format!(
            "Live2D output exceeds {MAX_LIVE2D_TOTAL_OUTPUT_BYTES} total bytes"
        )));
    }
    state.scheduled_bytes = next_bytes;
    Ok(())
}

fn report_live2d_failure(
    state: &mut Live2dExportState,
    source: &str,
    path_id: i64,
    class_id: i32,
    message: &str,
    output: &mut impl Write,
) -> CliResult<()> {
    state.failures = state
        .failures
        .checked_add(1)
        .ok_or_else(|| CliError::Runtime(Error::invalid_data("Live2D failure count overflowed")))?;
    writeln!(
        output,
        "failed {}::{path_id} (class {class_id}): {message}",
        escape_text(source)
    )?;
    Ok(())
}

fn sanitize_live2d_base_name(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len().min(MAX_LIVE2D_BASE_NAME_BYTES));
    for character in value.chars() {
        let replacement = if character.is_control()
            || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            ) {
            '_'
        } else {
            character
        };
        if sanitized.len() + replacement.len_utf8() > MAX_LIVE2D_BASE_NAME_BYTES {
            break;
        }
        sanitized.push(replacement);
    }
    let mut base_name = sanitized.trim_matches([' ', '.']).to_owned();
    if base_name
        .get(base_name.len().saturating_sub(5)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".moc3"))
    {
        base_name.truncate(base_name.len() - 5);
        let trimmed_length = base_name.trim_end_matches([' ', '.']).len();
        base_name.truncate(trimmed_length);
    }
    if base_name.is_empty() || base_name == "." || base_name == ".." {
        "unnamed".to_owned()
    } else {
        base_name
    }
}

fn allocate_live2d_output_path(
    output_directory: &Path,
    base_name: &str,
    path_id: i64,
    candidate: Live2dCandidate,
    claimed_paths: &mut HashSet<String>,
) -> Result<PathBuf> {
    let candidates = [
        format!("{base_name}.moc3"),
        format!("{base_name} @{path_id}.moc3"),
        format!(
            "{base_name} @{path_id} f{:04}o{}.moc3",
            candidate.file_index, candidate.object_index
        ),
    ];
    for file_name in candidates {
        if claimed_paths.insert(file_name.to_lowercase()) {
            return Ok(output_directory.join(file_name));
        }
    }
    Err(Error::invalid_data(format!(
        "cannot create a unique Live2D output name for path ID {path_id}"
    )))
}

fn atomic_write_cubism_moc(
    destination: &Path,
    model: &CubismMoc,
    maximum_output_bytes: u64,
    temporary_sequence: &mut u64,
) -> Result<u64> {
    let parent = destination.parent().ok_or_else(|| {
        Error::invalid_data(format!(
            "Live2D output path has no parent: {}",
            destination.display()
        ))
    })?;
    let mut temporary = Live2dTemporaryFile::create(parent, temporary_sequence)?;
    let written = model.write_moc3(temporary.file_mut(), maximum_output_bytes)?;
    if written != model.model_data.len() {
        return Err(Error::invalid_data(format!(
            "Cubism MOC3 copy wrote {written} bytes; expected {}",
            model.model_data.len()
        )));
    }
    temporary.file_mut().flush()?;
    temporary.file_mut().sync_all()?;
    temporary.close()?;
    temporary.persist_no_clobber(destination)?;
    Ok(written)
}

struct Live2dTemporaryFile {
    path: PathBuf,
    file: Option<File>,
    persisted: bool,
}

impl Live2dTemporaryFile {
    fn create(directory: &Path, sequence: &mut u64) -> Result<Self> {
        for _ in 0..MAX_LIVE2D_TEMPORARY_ATTEMPTS {
            *sequence = sequence
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("Live2D temporary-file counter overflowed"))?;
            let path = directory.join(format!(
                ".assetstudio-live2d-{}-{}.tmp",
                std::process::id(),
                sequence
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
            "cannot allocate a Live2D temporary file after {MAX_LIVE2D_TEMPORARY_ATTEMPTS} attempts"
        )))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("Live2D temporary file is still open")
    }

    fn close(&mut self) -> Result<()> {
        self.file
            .take()
            .ok_or_else(|| Error::invalid_data("Live2D temporary file was already closed"))?;
        Ok(())
    }

    fn persist_no_clobber(&mut self, destination: &Path) -> Result<()> {
        match fs::hard_link(&self.path, destination) {
            Ok(()) => {
                fs::remove_file(&self.path)?;
                self.persisted = true;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(Error::invalid_data(format!(
                    "refusing to overwrite existing Live2D output: {}",
                    destination.display()
                )))
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for Live2dTemporaryFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Default)]
struct Live2dPackageExportState {
    temporary_sequence: u64,
    output_ready: bool,
    exported: usize,
    exported_bytes: u64,
    failures: usize,
}

fn export_live2d_packages(
    input: &Path,
    output_directory: &Path,
    load: &LoadOptions,
    output: &mut impl Write,
) -> CliResult<()> {
    let collection = load_asset_collection(input, load, output)?;
    let set = build_live2d_packages(&collection, live2d_package_limits())?;
    let mut state = Live2dPackageExportState::default();
    for diagnostic in set.diagnostics {
        state.failures = state.failures.checked_add(1).ok_or_else(|| {
            CliError::Runtime(Error::invalid_data(
                "Live2D package diagnostic count overflowed",
            ))
        })?;
        writeln!(
            output,
            "diagnostic f{}:{} ({:?}): {}",
            diagnostic.object.file_index,
            diagnostic.object.path_id,
            diagnostic.kind,
            diagnostic.message
        )?;
    }
    if set.packages.len() > MAX_LIVE2D_PACKAGE_OUTPUTS {
        return Err(Error::invalid_data(format!(
            "Live2D package output exceeds {MAX_LIVE2D_PACKAGE_OUTPUTS} packages"
        ))
        .into());
    }
    for package in set.packages {
        if !state.output_ready {
            create_live2d_package_output_root(output_directory)?;
            state.output_ready = true;
        }
        match write_live2d_package_atomic(output_directory, &package, &mut state) {
            Ok(written) => {
                state.exported = state.exported.checked_add(1).ok_or_else(|| {
                    CliError::Runtime(Error::invalid_data(
                        "Live2D package export count overflowed",
                    ))
                })?;
                state.exported_bytes =
                    state.exported_bytes.checked_add(written).ok_or_else(|| {
                        CliError::Runtime(Error::invalid_data(
                            "Live2D package exported byte count overflowed",
                        ))
                    })?;
                writeln!(
                    output,
                    "exported Live2D package f{}:{} ({} bytes) -> {}",
                    package.model.file_index,
                    package.model.path_id,
                    written,
                    output_directory.join(&package.directory_name).display()
                )?;
            }
            Err(error) => {
                state.failures = state.failures.checked_add(1).ok_or_else(|| {
                    CliError::Runtime(Error::invalid_data(
                        "Live2D package failure count overflowed",
                    ))
                })?;
                writeln!(
                    output,
                    "failed Live2D package f{}:{}: {error}",
                    package.model.file_index, package.model.path_id
                )?;
            }
        }
    }
    if state.exported == 0 && state.failures == 0 {
        writeln!(output, "no verified Live2D packages found")?;
    }
    writeln!(
        output,
        "live2d-package summary: {} exported, {} failed, {} bytes",
        state.exported, state.failures, state.exported_bytes
    )?;
    if state.failures != 0 {
        output.flush()?;
        return Err(CliError::Partial {
            operation: "live2d-package",
            failures: state.failures,
        });
    }
    skipped_input_result("live2d-package", &collection)
}

fn live2d_package_limits() -> Live2dPackageLimits {
    Live2dPackageLimits {
        maximum_models: MAX_LIVE2D_PACKAGE_OUTPUTS,
        maximum_total_moc_bytes: MAX_LIVE2D_PACKAGE_TOTAL_BYTES,
        maximum_total_texture_payload_bytes: MAX_LIVE2D_PACKAGE_TOTAL_BYTES,
        maximum_total_manifest_bytes: MAX_LIVE2D_PACKAGE_TOTAL_BYTES,
        texture: TextureReadLimits {
            maximum_output_bytes: MAX_LIVE2D_PACKAGE_FILE_BYTES,
            maximum_decoder_working_bytes: MAX_LIVE2D_PACKAGE_IMAGE_WORKING_BYTES,
            ..TextureReadLimits::default()
        },
        ..Live2dPackageLimits::default()
    }
}

fn create_live2d_package_output_root(output_directory: &Path) -> Result<()> {
    let absolute = if output_directory.is_absolute() {
        output_directory.to_path_buf()
    } else {
        env::current_dir()?.join(output_directory)
    };
    let mut missing = Vec::new();
    let mut candidate = Some(absolute.as_path());
    while let Some(path) = candidate {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::invalid_data(format!(
                    "refusing symbolic-link in Live2D package output path: {}",
                    path.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => {
                return Err(Error::invalid_data(format!(
                    "Live2D package output path component is not a directory: {}",
                    path.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(path.to_path_buf());
                candidate = path.parent();
            }
            Err(error) => return Err(error.into()),
        }
    }
    if candidate.is_none() {
        return Err(Error::invalid_data(format!(
            "Live2D package output path has no existing directory ancestor: {}",
            output_directory.display()
        )));
    }
    for path in missing.iter().rev() {
        match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::invalid_data(format!(
                "Live2D package output path component is not a real directory: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn write_live2d_package_atomic(
    output_root: &Path,
    package: &Live2dPackage,
    state: &mut Live2dPackageExportState,
) -> Result<u64> {
    let destination = output_root.join(&package.directory_name);
    let mut publication_lock =
        Live2dPublicationLock::acquire(output_root, &package.directory_name)?;
    ensure_live2d_package_destination_missing(&destination)?;
    let mut temporary = Live2dTemporaryDirectory::create(
        output_root,
        &package.directory_name,
        &mut state.temporary_sequence,
    )?;
    let mut package_bytes = 0_u64;
    package_bytes = charge_package_output(
        package_bytes,
        state.exported_bytes,
        write_package_moc(temporary.path(), package)?,
    )?;
    for texture in &package.textures {
        let decoded = texture
            .texture
            .decode_mip_rgba8(0, live2d_package_limits().texture)?;
        let destination = temporary.path().join(&texture.file_name);
        let texture_directory = destination.parent().ok_or_else(|| {
            Error::invalid_data("Live2D package texture path has no parent directory")
        })?;
        if !texture_directory.exists() {
            fs::create_dir(texture_directory)?;
        }
        let written = write_synced_file(&destination, |file| {
            write_rgba_image(
                &decoded,
                ImageFormat::Png,
                ImageRowOrder::UnityDecoded,
                MAX_LIVE2D_PACKAGE_FILE_BYTES,
                file,
            )
        })?;
        package_bytes = charge_package_output(package_bytes, state.exported_bytes, written)?;
    }
    for expression in &package.expressions {
        let destination = temporary.path().join(&expression.file_name);
        let expression_directory = destination.parent().ok_or_else(|| {
            Error::invalid_data("Live2D package expression path has no parent directory")
        })?;
        if !expression_directory.exists() {
            fs::create_dir(expression_directory)?;
        }
        let written = write_synced_file(&destination, |file| {
            expression
                .expression
                .write_exp3_json(file, MAX_LIVE2D_PACKAGE_FILE_BYTES)
        })?;
        package_bytes = charge_package_output(package_bytes, state.exported_bytes, written)?;
    }
    package_bytes = write_package_motions(
        temporary.path(),
        package,
        package_bytes,
        state.exported_bytes,
    )?;
    for json in [
        package.physics.as_ref(),
        package.pose.as_ref(),
        package.display_info.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        let destination = temporary.path().join(&json.file_name);
        let written = write_synced_file(&destination, |file| {
            file.write_all(&json.bytes)?;
            u64::try_from(json.bytes.len())
                .map_err(|_| Error::invalid_data("Live2D auxiliary JSON length does not fit u64"))
        })?;
        package_bytes = charge_package_output(package_bytes, state.exported_bytes, written)?;
    }
    let manifest_path = temporary.path().join(&package.manifest_file_name);
    let manifest_written = write_synced_file(&manifest_path, |file| {
        package.write_model3_json(file, MAX_LIVE2D_PACKAGE_FILE_BYTES)
    })?;
    package_bytes = charge_package_output(package_bytes, state.exported_bytes, manifest_written)?;
    if temporary.path().join("textures").is_dir() {
        sync_directory(&temporary.path().join("textures"))?;
    }
    if temporary.path().join("expressions").is_dir() {
        sync_directory(&temporary.path().join("expressions"))?;
    }
    if temporary.path().join("motions").is_dir() {
        sync_directory(&temporary.path().join("motions"))?;
    }
    sync_directory(temporary.path())?;
    temporary.persist_no_clobber(&destination)?;
    sync_directory(output_root)?;
    publication_lock.release()?;
    sync_directory(output_root)?;
    Ok(package_bytes)
}

fn write_package_motions(
    directory: &Path,
    package: &Live2dPackage,
    mut package_bytes: u64,
    exported_bytes: u64,
) -> Result<u64> {
    for motion in &package.motions {
        let destination = directory.join(&motion.file_name);
        let motion_directory = destination.parent().ok_or_else(|| {
            Error::invalid_data("Live2D package motion path has no parent directory")
        })?;
        if !motion_directory.exists() {
            fs::create_dir(motion_directory)?;
        }
        let written = write_synced_file(&destination, |file| {
            motion.motion.write_motion3_json(
                &package.motion_targets,
                package.force_bezier_motions,
                file,
                MAX_LIVE2D_PACKAGE_FILE_BYTES,
            )
        })?;
        package_bytes = charge_package_output(package_bytes, exported_bytes, written)?;
    }
    Ok(package_bytes)
}

fn write_package_moc(directory: &Path, package: &Live2dPackage) -> Result<u64> {
    let destination = directory.join(&package.moc_file_name);
    write_synced_file(&destination, |file| {
        package.moc.write_moc3(file, MAX_LIVE2D_PACKAGE_FILE_BYTES)
    })
}

fn write_synced_file(path: &Path, write: impl FnOnce(&mut File) -> Result<u64>) -> Result<u64> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let written = write(&mut file)?;
    file.flush()?;
    file.sync_all()?;
    Ok(written)
}

fn charge_package_output(package: u64, completed: u64, additional: u64) -> Result<u64> {
    if additional > MAX_LIVE2D_PACKAGE_FILE_BYTES {
        return Err(Error::invalid_data(format!(
            "Live2D package file is {additional} bytes, exceeding {MAX_LIVE2D_PACKAGE_FILE_BYTES}"
        )));
    }
    let package = package
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data("Live2D package byte count overflowed"))?;
    let total = completed
        .checked_add(package)
        .ok_or_else(|| Error::invalid_data("Live2D package total byte count overflowed"))?;
    if total > MAX_LIVE2D_PACKAGE_TOTAL_BYTES {
        return Err(Error::invalid_data(format!(
            "Live2D package output exceeds {MAX_LIVE2D_PACKAGE_TOTAL_BYTES} total bytes"
        )));
    }
    Ok(package)
}

fn ensure_live2d_package_destination_missing(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Error::invalid_data(format!(
            "refusing symbolic-link Live2D package destination: {}",
            path.display()
        ))),
        Ok(_) => Err(Error::invalid_data(format!(
            "refusing to overwrite existing Live2D package destination: {}",
            path.display()
        ))),
        Err(error) => Err(error.into()),
    }
}

struct Live2dTemporaryDirectory {
    path: PathBuf,
    persisted: bool,
}

impl Live2dTemporaryDirectory {
    fn create(root: &Path, name: &str, sequence: &mut u64) -> Result<Self> {
        for _ in 0..MAX_LIVE2D_TEMPORARY_ATTEMPTS {
            *sequence = sequence.checked_add(1).ok_or_else(|| {
                Error::invalid_data("Live2D package temporary-directory counter overflowed")
            })?;
            let path = root.join(format!(
                ".assetstudio-live2d-package-{}-{}-{}.tmp",
                std::process::id(),
                sequence,
                sanitize_live2d_base_name(name)
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        persisted: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::invalid_data(format!(
            "cannot allocate a Live2D package temporary directory after {MAX_LIVE2D_TEMPORARY_ATTEMPTS} attempts"
        )))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn persist_no_clobber(&mut self, destination: &Path) -> Result<()> {
        ensure_live2d_package_destination_missing(destination)?;
        fs::rename(&self.path, destination)?;
        self.persisted = true;
        Ok(())
    }
}

impl Drop for Live2dTemporaryDirectory {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct Live2dPublicationLock {
    path: PathBuf,
    file: Option<File>,
}

impl Live2dPublicationLock {
    fn acquire(root: &Path, name: &str) -> Result<Self> {
        let path = root.join(format!(
            ".assetstudio-live2d-package-publish-{}.lock",
            sanitize_live2d_base_name(name)
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => Ok(Self {
                path,
                file: Some(file),
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(Error::invalid_data(format!(
                    "another Live2D package publisher holds the destination lock: {}",
                    path.display()
                )))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn release(&mut self) -> Result<()> {
        self.file.take();
        fs::remove_file(&self.path)?;
        Ok(())
    }
}

impl Drop for Live2dPublicationLock {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

/// Flushes a directory entry so a create or rename inside it survives a crash.
///
/// This is a POSIX technique and it has no supported Windows equivalent.
/// `File::open` on a directory there fails outright with `ERROR_ACCESS_DENIED`
/// because a directory handle needs `FILE_FLAG_BACKUP_SEMANTICS`, and even with
/// that flag `FlushFileBuffers` wants write access a directory handle cannot
/// carry. Attempting it made every `Live2D` package publish fail on Windows, so
/// the sync is skipped there; the publish itself stays atomic either way,
/// because it is still a single rename of a fully written directory.
#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

/// Opens a collection, skipping inputs that cannot be parsed.
///
/// A game directory routinely mixes readable assets with encrypted, truncated
/// or not-yet-supported containers, so refusing the whole load over one of them
/// would report nothing where the managed tool reports almost everything. Each
/// skipped input is printed, and a load where nothing at all parsed is still a
/// hard failure rather than an empty success.
fn load_asset_collection(
    path: &Path,
    load: &LoadOptions,
    output: &mut impl Write,
) -> Result<AssetCollection> {
    let collection = AssetCollection::load_path_with_options(
        path,
        AssetLoadOptions {
            unity_version_override: load.unity_version.clone(),
            failure_policy: LoadFailurePolicy::SkipInput,
            ..AssetLoadOptions::default()
        },
    )?;
    if collection.serialized_files.is_empty()
        && collection.resources.is_empty()
        && let Some(first) = collection.diagnostics.first()
    {
        return Err(Error::invalid_data(format!(
            "{}: {}",
            first.path, first.message
        )));
    }
    for diagnostic in &collection.diagnostics {
        writeln!(
            output,
            "  skipped {}: {}",
            escape_text(&diagnostic.path),
            escape_text(&diagnostic.message)
        )?;
    }
    Ok(collection)
}

/// Turns skipped inputs into the CLI's partial-failure exit status.
fn skipped_input_result(operation: &'static str, collection: &AssetCollection) -> CliResult<()> {
    if collection.diagnostics.is_empty() {
        return Ok(());
    }
    Err(CliError::Partial {
        operation,
        failures: collection.diagnostics.len(),
    })
}

fn inspect_path(path: &Path, output: &mut impl Write) -> CliResult<()> {
    let metadata = fs::metadata(path)?;
    if metadata.is_file() {
        return inspect_file(path, output).map_err(CliError::from);
    }
    if !metadata.is_dir() {
        return Err(Error::invalid_data(format!(
            "input is neither a regular file nor a directory: {}",
            path.display()
        ))
        .into());
    }

    let files = collect_files(path)?;
    let total = files.len();
    let mut failures = 0_usize;
    for file in files {
        if let Err(error) = inspect_file(&file, output) {
            failures = failures.checked_add(1).ok_or_else(|| {
                CliError::Runtime(Error::invalid_data("inspect failure count overflowed"))
            })?;
            writeln!(
                output,
                "  inspect error for {}: {error}",
                escape_text(&file.to_string_lossy())
            )?;
        }
    }
    writeln!(
        output,
        "inspect summary: {} succeeded, {failures} failed",
        total - failures
    )?;
    if failures != 0 {
        output.flush()?;
        return Err(CliError::Partial {
            operation: "inspect",
            failures,
        });
    }
    Ok(())
}

fn report_collection(
    path: &Path,
    include_objects: bool,
    load: &LoadOptions,
    output: &mut impl Write,
) -> CliResult<()> {
    let collection = load_asset_collection(path, load, output)?;
    let mut total_objects = 0_usize;
    let mut total_object_bytes = 0_u64;
    let mut class_counts = BTreeMap::<i32, usize>::new();
    let mut unity_versions = BTreeMap::<String, usize>::new();

    writeln!(output, "{}", path.display())?;
    writeln!(
        output,
        "  serialized files: {}",
        collection.serialized_files.len()
    )?;
    writeln!(output, "  resources: {}", collection.resources.len())?;

    for loaded in &collection.serialized_files {
        total_objects = total_objects
            .checked_add(loaded.file.objects.len())
            .ok_or_else(|| Error::invalid_data("serialized object count overflowed"))?;
        let version_count = unity_versions
            .entry(loaded.file.unity_version_string.clone())
            .or_default();
        *version_count = version_count
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("Unity version count overflowed"))?;

        if include_objects {
            writeln!(output, "  {}", escape_text(&loaded.path))?;
            writeln!(
                output,
                "    Unity version: {}",
                escape_text(&loaded.file.unity_version_string)
            )?;
            // A stripped or pre-v7 file is parsed against a version it does not
            // declare, whether that came from --unity-version or the enclosing
            // bundle. Report which one the version gates actually used.
            let effective = loaded.file.unity_version.to_string();
            if effective != loaded.file.unity_version_string {
                writeln!(
                    output,
                    "    effective Unity version: {}",
                    escape_text(&effective)
                )?;
            }
        }
        for object in &loaded.file.objects {
            total_object_bytes = total_object_bytes
                .checked_add(object.byte_size)
                .ok_or_else(|| Error::invalid_data("serialized object byte total overflowed"))?;
            let class_count = class_counts.entry(object.class_id).or_default();
            *class_count = class_count
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("serialized class count overflowed"))?;
            if include_objects {
                writeln!(
                    output,
                    "    path ID {}: class {}{}, type {}, {} bytes",
                    object.path_id,
                    object.class_id,
                    class_name_suffix(object.class_id),
                    object.type_id,
                    object.byte_size
                )?;
            }
        }
    }

    writeln!(output, "  objects: {total_objects}")?;
    writeln!(output, "  object bytes: {total_object_bytes}")?;
    writeln!(output, "  Unity versions:")?;
    if unity_versions.is_empty() {
        writeln!(output, "    none")?;
    } else {
        for (version, count) in unity_versions {
            writeln!(output, "    {}: {count} file(s)", escape_text(&version))?;
        }
    }
    writeln!(output, "  object classes:")?;
    if class_counts.is_empty() {
        writeln!(output, "    none")?;
    } else {
        for (class_id, count) in class_counts {
            writeln!(
                output,
                "    {class_id}{}: {count}",
                class_name_suffix(class_id)
            )?;
        }
    }
    skipped_input_result("info/list", &collection)
}

fn report_scene(path: &Path, load: &LoadOptions, output: &mut impl Write) -> CliResult<()> {
    let collection = load_asset_collection(path, load, output)?;
    let hierarchy = build_scene_hierarchy(&collection, SceneHierarchyLimits::default())?;
    writeln!(output, "scene {}", path.display())?;
    writeln!(
        output,
        "  serialized files: {}",
        collection.serialized_files.len()
    )?;
    writeln!(output, "  nodes: {}", hierarchy.nodes.len())?;
    writeln!(output, "  roots: {}", hierarchy.roots.len())?;
    if hierarchy.roots.is_empty() {
        writeln!(output, "    none")?;
        return Ok(());
    }

    let mut visited = 0_usize;
    for root in &hierarchy.roots {
        visit_scene_root(&collection, &hierarchy, *root, &mut visited, output)?;
    }
    if visited != hierarchy.nodes.len() {
        return Err(CliError::Runtime(Error::invalid_data(format!(
            "scene traversal visited {visited} of {} hierarchy nodes",
            hierarchy.nodes.len()
        ))));
    }
    skipped_input_result("scene", &collection)
}

fn visit_scene_root(
    collection: &AssetCollection,
    hierarchy: &SceneHierarchy,
    root: SceneObjectKey,
    visited: &mut usize,
    output: &mut impl Write,
) -> Result<()> {
    charge_scene_visit(visited)?;
    let root_node = hierarchy
        .node(root)
        .ok_or_else(|| Error::invalid_data("scene root is absent from the node index"))?;
    write_scene_node(collection, root_node, 0, true, output)?;
    let mut stack = Vec::<(SceneObjectKey, usize, usize)>::new();
    stack.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot allocate scene traversal stack: {error}"))
    })?;
    stack.push((root, 0, 0));

    while let Some((key, depth, next_child)) = stack.last_mut() {
        let node = hierarchy
            .node(*key)
            .ok_or_else(|| Error::invalid_data("scene traversal key is absent from node index"))?;
        let Some(child) = node.children.get(*next_child).copied() else {
            stack.pop();
            continue;
        };
        *next_child = next_child
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("scene child index overflowed"))?;
        let child_depth = depth
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("scene output depth overflowed"))?;
        if child_depth > MAX_SCENE_OUTPUT_DEPTH {
            return Err(Error::invalid_data(format!(
                "scene output depth exceeds {MAX_SCENE_OUTPUT_DEPTH}"
            )));
        }
        charge_scene_visit(visited)?;
        let child_node = hierarchy
            .node(child)
            .ok_or_else(|| Error::invalid_data("scene child is absent from the node index"))?;
        write_scene_node(collection, child_node, child_depth, false, output)?;
        stack.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow scene traversal stack: {error}"))
        })?;
        stack.push((child, child_depth, 0));
    }
    Ok(())
}

fn charge_scene_visit(visited: &mut usize) -> Result<()> {
    *visited = visited
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("scene output visit count overflowed"))?;
    if *visited > MAX_SCENE_OUTPUT_NODES {
        return Err(Error::invalid_data(format!(
            "scene output exceeds {MAX_SCENE_OUTPUT_NODES} visited nodes"
        )));
    }
    Ok(())
}

fn write_scene_node(
    collection: &AssetCollection,
    node: &SceneHierarchyNode,
    depth: usize,
    root: bool,
    output: &mut impl Write,
) -> Result<()> {
    write_scene_indent(output, depth + 2)?;
    write!(output, "{} ", if root { "root" } else { "node" })?;
    write_scene_key(output, node.object)?;
    let source = collection
        .serialized_files
        .get(node.object.file_index)
        .ok_or_else(|| Error::invalid_data("scene node source file index is out of range"))?;
    writeln!(
        output,
        " source=\"{}\" name=\"{}\"",
        escape_text(&source.path),
        escape_text(&node.name)
    )?;
    write_scene_bindings(node, depth, output)
}

fn write_scene_bindings(
    node: &SceneHierarchyNode,
    depth: usize,
    output: &mut impl Write,
) -> Result<()> {
    if let Some(binding) = &node.transform {
        write_scene_indent(output, depth + 3)?;
        write!(output, "transform component=")?;
        write_scene_key(output, binding.component)?;
        write!(output, " parent=")?;
        write_optional_scene_key(output, binding.parent_transform)?;
        writeln!(
            output,
            " position=({},{},{}) rotation=({},{},{},{}) scale=({},{},{})",
            binding.local_position.x,
            binding.local_position.y,
            binding.local_position.z,
            binding.local_rotation.x,
            binding.local_rotation.y,
            binding.local_rotation.z,
            binding.local_rotation.w,
            binding.local_scale.x,
            binding.local_scale.y,
            binding.local_scale.z
        )?;
    }
    if let Some(binding) = &node.mesh_filter {
        write_scene_indent(output, depth + 3)?;
        write!(output, "mesh-filter component=")?;
        write_scene_key(output, binding.component)?;
        write!(output, " mesh=")?;
        write_optional_scene_key(output, binding.mesh)?;
        writeln!(output)?;
    }
    if let Some(binding) = &node.mesh_renderer {
        write_scene_indent(output, depth + 3)?;
        write!(output, "mesh-renderer component=")?;
        write_scene_key(output, binding.component)?;
        write!(output, " materials=")?;
        write_scene_key_list(output, &binding.materials)?;
        writeln!(output)?;
    }
    if let Some(binding) = &node.skinned_mesh_renderer {
        write_scene_indent(output, depth + 3)?;
        write!(output, "skinned-mesh-renderer component=")?;
        write_scene_key(output, binding.component)?;
        write!(output, " mesh=")?;
        write_optional_scene_key(output, binding.mesh)?;
        write!(output, " materials=")?;
        write_scene_key_list(output, &binding.materials)?;
        write!(output, " bones=")?;
        write_scene_key_list(output, &binding.bones)?;
        writeln!(output)?;
    }
    if let Some(binding) = &node.animator {
        write_scene_indent(output, depth + 3)?;
        write!(output, "animator component=")?;
        write_scene_key(output, binding.component)?;
        write!(output, " avatar=")?;
        write_object_reference(output, binding.component.file_index, binding.avatar)?;
        write!(output, " controller=")?;
        write_object_reference(output, binding.component.file_index, binding.controller)?;
        writeln!(output)?;
    }
    Ok(())
}

fn write_scene_key(output: &mut impl Write, key: SceneObjectKey) -> Result<()> {
    write!(output, "f{}:{}", key.file_index, key.path_id)?;
    Ok(())
}

fn write_optional_scene_key(output: &mut impl Write, key: Option<SceneObjectKey>) -> Result<()> {
    if let Some(key) = key {
        write_scene_key(output, key)
    } else {
        write!(output, "null")?;
        Ok(())
    }
}

fn write_scene_key_list(output: &mut impl Write, keys: &[Option<SceneObjectKey>]) -> Result<()> {
    write!(output, "[")?;
    for (index, key) in keys.iter().enumerate() {
        if index != 0 {
            write!(output, ",")?;
        }
        write_optional_scene_key(output, *key)?;
    }
    write!(output, "]")?;
    Ok(())
}

fn write_object_reference(
    output: &mut impl Write,
    source_file_index: usize,
    reference: assetstudio_core::serialized::ObjectReference,
) -> Result<()> {
    if reference.path_id == 0 {
        write!(output, "null")?;
    } else if reference.file_id == 0 {
        write!(
            output,
            "local(file=f{},path={})",
            source_file_index, reference.path_id
        )?;
    } else {
        write!(
            output,
            "external(fileID={},path={})",
            reference.file_id, reference.path_id
        )?;
    }
    Ok(())
}

fn write_scene_indent(output: &mut impl Write, levels: usize) -> Result<()> {
    for _ in 0..levels {
        write!(output, "  ")?;
    }
    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending_directories = Vec::new();
    pending_directories.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot allocate directory queue: {error}"))
    })?;
    pending_directories.push(root.to_owned());
    let mut files = Vec::new();
    let mut directory_count = 1_usize;
    let mut entry_count = 0_usize;
    while let Some(directory) = pending_directories.pop() {
        let mut children = Vec::new();
        for child in fs::read_dir(directory)? {
            entry_count = entry_count
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("directory entry count overflowed"))?;
            if entry_count > MAX_DIRECTORY_ENTRIES {
                return Err(Error::invalid_data(format!(
                    "directory traversal exceeds {MAX_DIRECTORY_ENTRIES} entries"
                )));
            }
            children.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot allocate directory entries: {error}"))
            })?;
            children.push(child?);
        }
        children.sort_unstable_by_key(std::fs::DirEntry::file_name);
        for child in children.into_iter().rev() {
            let file_type = child.file_type()?;
            if file_type.is_dir() {
                directory_count = directory_count
                    .checked_add(1)
                    .ok_or_else(|| Error::invalid_data("directory count overflowed"))?;
                if directory_count > MAX_DIRECTORIES {
                    return Err(Error::invalid_data(format!(
                        "directory traversal exceeds {MAX_DIRECTORIES} directories"
                    )));
                }
                pending_directories.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow directory queue: {error}"))
                })?;
                pending_directories.push(child.path());
            } else if file_type.is_file() {
                if files.len() >= MAX_DIRECTORY_FILES {
                    return Err(Error::invalid_data(format!(
                        "directory traversal exceeds {MAX_DIRECTORY_FILES} files"
                    )));
                }
                files.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow input file list: {error}"))
                })?;
                files.push(child.path());
            }
        }
    }
    files.sort_unstable();
    Ok(files)
}

fn inspect_file(path: &Path, output: &mut impl Write) -> Result<()> {
    let region = Region::from_file(path)?;
    inspect_region(&path.display().to_string(), region, output, 0)
}

fn inspect_region(
    label: &str,
    region: Region,
    output: &mut impl Write,
    compression_depth: usize,
) -> Result<()> {
    let file_size = region.len();
    let scan_length = u64::try_from(HEADER_SCAN_LENGTH).expect("scan length fits in u64");
    let header_length =
        usize::try_from(file_size.min(scan_length)).expect("bounded header length fits in usize");
    let mut header = Vec::new();
    header
        .try_reserve_exact(header_length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate file header: {error}")))?;
    header.resize(header_length, 0);
    region.read_exact_at(0, &mut header)?;

    let detection = detect_file_type(&header, file_size);
    writeln!(output, "{label}")?;
    writeln!(output, "  type: {}", detection.file_type)?;
    if detection.data_offset != 0 {
        writeln!(
            output,
            "  embedded bundle offset: {}",
            detection.data_offset
        )?;
    }

    match detection.file_type {
        FileType::AssetsFile => inspect_serialized_file(&region, output),
        FileType::BundleFile => inspect_bundle(&region, detection, output),
        FileType::WebFile => inspect_web_file(region, output),
        FileType::GzipFile => inspect_compressed_stream(
            label,
            "gzip",
            decompress_gzip(&region, CompressionLimits::default())?,
            output,
            compression_depth,
        ),
        FileType::BrotliFile => inspect_compressed_stream(
            label,
            "brotli",
            decompress_brotli(&region, CompressionLimits::default())?,
            output,
            compression_depth,
        ),
        FileType::ZipFile => inspect_zip(label, &region, output, compression_depth),
        FileType::ResourceFile => Ok(()),
    }
}

fn inspect_compressed_stream(
    label: &str,
    wrapper: &str,
    decoded: Region,
    output: &mut impl Write,
    depth: usize,
) -> Result<()> {
    let next_depth = checked_compression_depth(depth)?;
    writeln!(output, "  expanded size: {}", decoded.len())?;
    inspect_region(&format!("{label}::{wrapper}"), decoded, output, next_depth)
}

fn inspect_zip(label: &str, region: &Region, output: &mut impl Write, depth: usize) -> Result<()> {
    let next_depth = checked_compression_depth(depth)?;
    let archive = ZipContainer::open(region, CompressionLimits::default())?;
    writeln!(output, "  entries: {}", archive.entries.len())?;
    for (index, entry) in archive.entries.iter().enumerate() {
        writeln!(
            output,
            "    {} ({} bytes)",
            escape_text(&entry.path),
            entry.size
        )?;
        inspect_region(
            &format!("{label}::{}", escape_text(&entry.path)),
            archive.read_entry(index)?,
            output,
            next_depth,
        )?;
    }
    Ok(())
}

fn checked_compression_depth(depth: usize) -> Result<usize> {
    let next = depth
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("compression nesting depth overflowed"))?;
    if next > MAX_COMPRESSION_DEPTH {
        return Err(Error::invalid_data(format!(
            "compression nesting exceeds {MAX_COMPRESSION_DEPTH} layers"
        )));
    }
    Ok(next)
}

fn inspect_serialized_file(region: &Region, output: &mut impl Write) -> Result<()> {
    let file = SerializedFile::open(region.clone())?;
    let mut class_counts = BTreeMap::<i32, usize>::new();
    let mut object_bytes = 0_u64;
    for object in &file.objects {
        object_bytes = object_bytes
            .checked_add(object.byte_size)
            .ok_or_else(|| Error::invalid_data("serialized object byte total overflowed"))?;
        let count = class_counts.entry(object.class_id).or_default();
        *count = count
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("serialized class count overflowed"))?;
    }
    writeln!(output, "  format version: {}", file.header.version)?;
    writeln!(output, "  Unity version: {}", file.unity_version)?;
    writeln!(output, "  target platform: {}", file.target_platform)?;
    writeln!(output, "  metadata size: {}", file.header.metadata_size)?;
    writeln!(output, "  data offset: {}", file.header.data_offset)?;
    writeln!(
        output,
        "  metadata endian: {}",
        if file.header.endianness == 0 {
            "little"
        } else {
            "big"
        }
    )?;
    writeln!(output, "  types: {}", file.types.len())?;
    writeln!(output, "  objects: {}", file.objects.len())?;
    writeln!(output, "  object bytes: {object_bytes}")?;
    writeln!(output, "  externals: {}", file.externals.len())?;
    writeln!(output, "  object classes:")?;
    if class_counts.is_empty() {
        writeln!(output, "    none")?;
    } else {
        for (class_id, count) in class_counts {
            writeln!(
                output,
                "    {class_id}{}: {count}",
                class_name_suffix(class_id)
            )?;
        }
    }
    for object in &file.objects {
        writeln!(
            output,
            "    path ID {}: class {}{}, type {}, {} bytes",
            object.path_id,
            object.class_id,
            class_name_suffix(object.class_id),
            object.type_id,
            object.byte_size
        )?;
    }
    Ok(())
}

fn inspect_bundle(root: &Region, detection: FileDetection, output: &mut impl Write) -> Result<()> {
    let bundle_length = root
        .len()
        .checked_sub(detection.data_offset)
        .ok_or_else(|| Error::invalid_data("embedded bundle offset exceeds file size"))?;
    let region = root.subregion(detection.data_offset, bundle_length)?;
    let mut reader = EndianReader::new(region.cursor(), Endian::Big);
    let header = BundleHeader::read(&mut reader)?;
    writeln!(output, "  signature: {}", header.signature)?;
    writeln!(output, "  bundle version: {}", header.version)?;
    writeln!(output, "  generator version: {}", header.unity_version)?;
    writeln!(output, "  Unity revision: {}", header.unity_revision)?;

    if header.signature == "UnityFS" {
        match UnityFsBundle::open(&region) {
            Ok(bundle) => {
                writeln!(output, "  blocks: {}", bundle.blocks.len())?;
                writeln!(output, "  entries: {}", bundle.entries.len())?;
                for entry in bundle.entries {
                    writeln!(
                        output,
                        "    {} ({} bytes)",
                        escape_text(&entry.path),
                        entry.size
                    )?;
                }
            }
            Err(Error::Unsupported(message)) => {
                writeln!(output, "  directory: unsupported ({message})")?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn inspect_web_file(region: Region, output: &mut impl Write) -> Result<()> {
    let web_file = WebFile::open(region)?;
    writeln!(output, "  entries: {}", web_file.entries.len())?;
    for entry in web_file.entries {
        writeln!(
            output,
            "    {} ({} bytes)",
            escape_text(&entry.path),
            entry.data_length
        )?;
    }
    Ok(())
}

fn escape_text(value: &str) -> String {
    value.chars().flat_map(char::escape_default).collect()
}

fn class_name_suffix(class_id: i32) -> &'static str {
    match class_id {
        1 => " (GameObject)",
        4 => " (Transform)",
        21 => " (Material)",
        28 => " (Texture2D)",
        43 => " (Mesh)",
        48 => " (Shader)",
        49 => " (TextAsset)",
        74 => " (AnimationClip)",
        83 => " (AudioClip)",
        89 => " (Cubemap)",
        90 => " (Avatar)",
        91 => " (AnimatorController)",
        95 => " (Animator)",
        114 => " (MonoBehaviour)",
        115 => " (MonoScript)",
        128 => " (Font)",
        152 => " (MovieTexture)",
        187 => " (Texture2DArray)",
        213 => " (Sprite)",
        329 => " (VideoClip)",
        687_078_895 => " (SpriteAtlas)",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AudioExportFormat, CliCommand, ExportMode, FilenameFormat, ImageFormat, Live2dExportState,
        MAX_LIVE2D_OUTPUT_MODELS, MAX_LIVE2D_TOTAL_OUTPUT_BYTES, SceneObjectKey,
        charge_live2d_model, obj_material_library_name, parse_cli_arguments,
        parse_export_arguments, parse_extract_arguments, parse_live2d_arguments,
        parse_live2d_package_arguments, sanitize_live2d_base_name, write_object_reference,
        write_scene_key,
    };
    use assetstudio_core::serialized::ObjectReference;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn model_commands_default_to_writing_png_textures() {
        for (command, extension) in [("fbx", "out.fbx"), ("obj", "out.obj")] {
            let parsed =
                parse_cli_arguments(&arguments(&[command, "input.assets", extension])).unwrap();
            let parsed = match parsed {
                CliCommand::Fbx(parsed) | CliCommand::Obj(parsed) => parsed,
                other => panic!("{command} parsed as {other:?}"),
            };
            assert_eq!(parsed.input, PathBuf::from("input.assets"));
            assert_eq!(parsed.output, PathBuf::from(extension));
            assert!(
                parsed.textures,
                "{command} should write textures by default"
            );
            assert_eq!(parsed.texture_format, ImageFormat::Png);
        }
    }

    #[test]
    fn model_commands_accept_the_texture_options_and_check_the_extension() {
        let parsed = parse_cli_arguments(&arguments(&[
            "obj",
            "input.assets",
            "out.obj",
            "--no-textures",
            "--texture-format",
            "tga",
        ]))
        .unwrap();
        let CliCommand::Obj(parsed) = parsed else {
            panic!("expected an obj command");
        };
        assert!(!parsed.textures);
        assert_eq!(parsed.texture_format, ImageFormat::Tga);

        // The output extension has to match the command, or the two writers
        // would silently produce a file named for the other format.
        assert!(parse_cli_arguments(&arguments(&["obj", "input.assets", "out.fbx"])).is_err());
        assert!(parse_cli_arguments(&arguments(&["fbx", "input.assets", "out.obj"])).is_err());
    }

    #[test]
    fn the_material_library_takes_the_obj_stem() {
        assert_eq!(
            obj_material_library_name(&PathBuf::from("/models/body.obj")).unwrap(),
            "body.mtl"
        );
        assert!(obj_material_library_name(&PathBuf::from("/")).is_err());
    }

    #[test]
    fn export_arguments_use_safe_defaults() {
        let command = parse_export_arguments(&arguments(&["input.assets", "output"])).unwrap();

        assert_eq!(command.input, PathBuf::from("input.assets"));
        assert_eq!(command.output, PathBuf::from("output"));
        assert_eq!(command.options.mode, ExportMode::Auto);
        assert_eq!(command.options.filename_format, FilenameFormat::AssetName);
        assert_eq!(command.options.image_format, ImageFormat::Png);
        assert_eq!(command.options.jpeg_quality, 75);
        assert_eq!(command.options.audio_format, AudioExportFormat::Auto);
        assert!(!command.options.overwrite_existing);
        assert!(command.options.restore_text_asset_extension);
        assert!(command.options.pretty_json);
    }

    #[test]
    fn export_arguments_accept_all_requested_options() {
        let command = parse_export_arguments(&arguments(&[
            "--mode",
            "typetree-json",
            "input",
            "--filename",
            "path-id",
            "--image-format",
            "tga",
            "--jpeg-quality",
            "91",
            "--audio-format",
            "wav",
            "--overwrite",
            "--no-restore-text-extension",
            "--compact-json",
            "output",
        ]))
        .unwrap();

        assert_eq!(command.options.mode, ExportMode::TypeTreeJson);
        assert_eq!(command.options.filename_format, FilenameFormat::PathId);
        assert_eq!(command.options.image_format, ImageFormat::Tga);
        assert_eq!(command.options.jpeg_quality, 91);
        assert_eq!(command.options.audio_format, AudioExportFormat::Wav);
        assert!(command.options.overwrite_existing);
        assert!(!command.options.restore_text_asset_extension);
        assert!(!command.options.pretty_json);
    }

    #[test]
    fn export_arguments_accept_managed_dump_text_mode() {
        let command = parse_export_arguments(&arguments(&[
            "--mode",
            "dump-text",
            "input.assets",
            "output",
        ]))
        .unwrap();

        assert_eq!(command.options.mode, ExportMode::DumpText);
    }

    #[test]
    fn export_arguments_accept_image_formats_and_raw_rgba_aliases() {
        for (value, expected) in [
            ("jpg", ImageFormat::Jpeg),
            ("jpeg", ImageFormat::Jpeg),
            ("png", ImageFormat::Png),
            ("bmp", ImageFormat::Bmp),
            ("tga", ImageFormat::Tga),
            ("webp", ImageFormat::Webp),
            ("raw-rgba", ImageFormat::RawRgba),
            ("raw_rgba", ImageFormat::RawRgba),
            ("rgba", ImageFormat::RawRgba),
        ] {
            let command =
                parse_export_arguments(&arguments(&["--image-format", value, "input", "output"]))
                    .unwrap();
            assert_eq!(command.options.image_format, expected);
        }
        assert!(
            parse_export_arguments(&arguments(&["--image-format", "gif", "input", "output",]))
                .is_err()
        );
        for value in ["0", "101", "not-a-number"] {
            assert!(
                parse_export_arguments(&arguments(&["--jpeg-quality", value, "input", "output",]))
                    .is_err()
            );
        }
    }

    #[test]
    fn export_arguments_accept_audio_modes_and_reject_unknown_values() {
        for (value, expected) in [
            ("auto", AudioExportFormat::Auto),
            ("raw", AudioExportFormat::Raw),
            ("none", AudioExportFormat::Raw),
            ("wav", AudioExportFormat::Wav),
            ("wave", AudioExportFormat::Wav),
        ] {
            let command =
                parse_export_arguments(&arguments(&["--audio-format", value, "input", "output"]))
                    .unwrap();
            assert_eq!(command.options.audio_format, expected);
        }
        assert!(
            parse_export_arguments(&arguments(&["--audio-format", "flac", "input", "output",]))
                .is_err()
        );
    }

    #[test]
    fn export_arguments_support_dash_prefixed_paths_after_separator() {
        let command = parse_export_arguments(&arguments(&["--", "-input", "-output"])).unwrap();
        assert_eq!(command.input, PathBuf::from("-input"));
        assert_eq!(command.output, PathBuf::from("-output"));
    }

    #[test]
    fn export_arguments_reject_unknown_options_and_missing_paths() {
        assert!(parse_export_arguments(&arguments(&["--unknown", "input", "output"])).is_err());
        assert!(parse_export_arguments(&arguments(&["input"])).is_err());
    }

    #[test]
    fn extract_arguments_accept_safe_options_and_require_two_paths() {
        let command =
            parse_extract_arguments(&arguments(&["--overwrite", "input", "output"])).unwrap();
        assert_eq!(command.input, PathBuf::from("input"));
        assert_eq!(command.output, PathBuf::from("output"));
        assert!(command.options.overwrite_existing);

        assert!(parse_extract_arguments(&arguments(&["input"])).is_err());
        assert!(parse_extract_arguments(&arguments(&["--unknown", "input", "output"])).is_err());
    }

    #[test]
    fn live2d_arguments_are_explicit_and_support_dash_prefixed_paths_after_separator() {
        let command = parse_live2d_arguments(&arguments(&["--", "-input", "-output"])).unwrap();
        assert_eq!(command.input, PathBuf::from("-input"));
        assert_eq!(command.output, PathBuf::from("-output"));
        assert!(parse_live2d_arguments(&arguments(&["input"])).is_err());
        assert!(parse_live2d_arguments(&arguments(&["--overwrite", "input", "output"])).is_err());
        let command =
            parse_live2d_package_arguments(&arguments(&["--", "-input", "-output"])).unwrap();
        assert_eq!(command.input, PathBuf::from("-input"));
        assert_eq!(command.output, PathBuf::from("-output"));
        assert!(parse_live2d_package_arguments(&arguments(&["input"])).is_err());
    }

    #[test]
    fn live2d_name_cleaning_and_aggregate_budgets_are_bounded() {
        assert_eq!(
            sanitize_live2d_base_name("../unsafe:name.moc3"),
            "_unsafe_name"
        );
        assert_eq!(sanitize_live2d_base_name("..."), "unnamed");

        let mut state = Live2dExportState {
            models_found: MAX_LIVE2D_OUTPUT_MODELS,
            ..Live2dExportState::default()
        };
        assert!(charge_live2d_model(&mut state, 1).is_err());

        let mut state = Live2dExportState {
            scheduled_bytes: MAX_LIVE2D_TOTAL_OUTPUT_BYTES,
            ..Live2dExportState::default()
        };
        assert!(charge_live2d_model(&mut state, 1).is_err());
    }

    #[test]
    fn read_only_commands_and_bare_path_have_distinct_parsing() {
        assert_eq!(
            parse_cli_arguments(&arguments(&["inspect", "input.assets"])).unwrap(),
            CliCommand::Inspect(PathBuf::from("input.assets"))
        );
        assert_eq!(
            parse_cli_arguments(&arguments(&["info", "input.assets"])).unwrap(),
            CliCommand::Info(PathBuf::from("input.assets"))
        );
        assert_eq!(
            parse_cli_arguments(&arguments(&["list", "input.assets"])).unwrap(),
            CliCommand::List(PathBuf::from("input.assets"))
        );
        assert_eq!(
            parse_cli_arguments(&arguments(&["scene", "input.assets"])).unwrap(),
            CliCommand::Scene(PathBuf::from("input.assets"))
        );
        assert_eq!(
            parse_cli_arguments(&arguments(&["input.assets"])).unwrap(),
            CliCommand::Inspect(PathBuf::from("input.assets"))
        );
    }

    #[test]
    fn read_only_commands_support_dash_prefixed_paths_after_separator() {
        assert_eq!(
            parse_cli_arguments(&arguments(&["info", "--", "-input.assets"])).unwrap(),
            CliCommand::Info(PathBuf::from("-input.assets"))
        );
        assert_eq!(
            parse_cli_arguments(&arguments(&["scene", "--", "-input.assets"])).unwrap(),
            CliCommand::Scene(PathBuf::from("-input.assets"))
        );
        assert!(parse_cli_arguments(&arguments(&["info", "-input.assets"])).is_err());
        assert!(parse_cli_arguments(&arguments(&["scene", "-input.assets"])).is_err());
    }

    #[test]
    fn legacy_info_is_read_only_and_rejects_export_options() {
        assert_eq!(
            parse_cli_arguments(&arguments(&["input.assets", "-m", "info"])).unwrap(),
            CliCommand::Info(PathBuf::from("input.assets"))
        );
        assert!(
            parse_cli_arguments(&arguments(&["input.assets", "-m", "info", "-o", "unused"]))
                .is_err()
        );
    }

    #[test]
    fn legacy_write_modes_require_explicit_output_and_map_payload_modes() {
        for (legacy_mode, expected_mode) in [
            ("export", ExportMode::Auto),
            ("exportRaw", ExportMode::Raw),
            ("dump", ExportMode::DumpText),
        ] {
            let command = parse_cli_arguments(&arguments(&[
                "input.assets",
                "-m",
                legacy_mode,
                "-o",
                "output",
            ]))
            .unwrap();
            let CliCommand::Export(command) = command else {
                panic!("expected export command for {legacy_mode}");
            };
            assert_eq!(command.input, PathBuf::from("input.assets"));
            assert_eq!(command.output, PathBuf::from("output"));
            assert_eq!(command.options.mode, expected_mode);
        }

        assert!(parse_cli_arguments(&arguments(&["input.assets", "-m", "export"])).is_err());
        assert!(parse_cli_arguments(&arguments(&["input.assets", "-m", "extract"])).is_err());

        let extraction = parse_cli_arguments(&arguments(&[
            "input.assets",
            "-m",
            "extract",
            "-o",
            "output",
            "--overwrite-existing",
        ]))
        .unwrap();
        let CliCommand::Extract(extraction) = extraction else {
            panic!("expected legacy extract command");
        };
        assert_eq!(extraction.input, PathBuf::from("input.assets"));
        assert_eq!(extraction.output, PathBuf::from("output"));
        assert!(extraction.options.overwrite_existing);
    }

    #[test]
    fn help_and_usage_are_not_conflated() {
        assert_eq!(
            parse_cli_arguments(&arguments(&["--help"])).unwrap(),
            CliCommand::Help
        );
        assert!(parse_cli_arguments(&[]).is_err());
        assert!(parse_cli_arguments(&arguments(&["inspect"])).is_err());
        assert!(parse_cli_arguments(&arguments(&["scene"])).is_err());
        assert!(parse_cli_arguments(&arguments(&["extract", "input"])).is_err());
    }

    #[test]
    fn scene_keys_distinguish_collection_files_and_unresolved_external_references() {
        let mut output = Vec::new();
        write_scene_key(
            &mut output,
            SceneObjectKey {
                file_index: 2,
                path_id: 91,
            },
        )
        .unwrap();
        assert_eq!(output, b"f2:91");

        output.clear();
        write_object_reference(
            &mut output,
            0,
            ObjectReference {
                file_id: 3,
                path_id: 74,
            },
        )
        .unwrap();
        assert_eq!(output, b"external(fileID=3,path=74)");
    }
}
