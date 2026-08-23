use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as _};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
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
    AudioExportFormat, ExportMode, ExportOptions, ExportPlan, FilenameFormat,
    export_collection_with_plan,
};
use assetstudio_core::extraction::{ExtractionOptions, extract_path};
use assetstudio_core::fbx_ascii::{
    write_model_ir_fbx_ascii_with_animations, write_model_ir_fbx_ascii_with_textures,
};
use assetstudio_core::fbx_binary_scene::write_model_ir_fbx_binary_full;
use assetstudio_core::file_type::{FileDetection, FileType, HEADER_SCAN_LENGTH, detect_file_type};
use assetstudio_core::image_export::{ImageFormat, ImageRowOrder, write_rgba_image};
use assetstudio_core::live2d_package::{Live2dPackage, Live2dPackageLimits, build_live2d_packages};
use assetstudio_core::loader::{
    AssetCollection, AssetLoadLimits, AssetLoadOptions, LoadFailurePolicy,
};
use assetstudio_core::model_animation::{
    ModelAnimationLimits, ModelAnimationSet, build_model_animations,
};
use assetstudio_core::model_export::{
    ModelExportCandidate, ModelExportPlanLimits, plan_animator_exports, plan_split_object_exports,
};
use assetstudio_core::model_ir::{ModelIrLimits, build_model_ir, build_model_ir_for_game_object};
use assetstudio_core::mono_schema::{
    MonoBehaviourSchemaDocumentLimits, MonoBehaviourSchemaProvider, MonoBehaviourSchemaRegistry,
};
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
const MAX_MONO_SCHEMA_DOCUMENTS: usize = 1_024;
const MONO_SCHEMA_READ_BUFFER_BYTES: usize = 16 * 1024;
const MAX_LIVE2D_PACKAGE_IMAGE_WORKING_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CLI_ARGUMENTS: usize = 65_536;
const MAX_CLI_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_CLI_ARGUMENT_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_CLI_ARGUMENT_DIAGNOSTIC_BYTES: usize = 64;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CliArgumentLimits {
    arguments: usize,
    argument_bytes: usize,
    total_bytes: usize,
}

impl Default for CliArgumentLimits {
    fn default() -> Self {
        Self {
            arguments: MAX_CLI_ARGUMENTS,
            argument_bytes: MAX_CLI_ARGUMENT_BYTES,
            total_bytes: MAX_CLI_ARGUMENT_TOTAL_BYTES,
        }
    }
}

struct CliArgumentDisplay<'a>(&'a OsStr);

impl fmt::Display for CliArgumentDisplay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let length = self.0.as_encoded_bytes().len();
        if length <= MAX_CLI_ARGUMENT_DIAGNOSTIC_BYTES {
            write!(formatter, "{}", LossyOsStr(self.0))
        } else {
            write!(formatter, "<argument of {length} encoded bytes>")
        }
    }
}

fn collect_cli_arguments(
    arguments: impl IntoIterator<Item = OsString>,
) -> CliResult<Vec<OsString>> {
    collect_cli_arguments_with_limits(arguments, CliArgumentLimits::default())
}

fn collect_cli_arguments_with_limits(
    arguments: impl IntoIterator<Item = OsString>,
    limits: CliArgumentLimits,
) -> CliResult<Vec<OsString>> {
    let mut output = Vec::new();
    let mut total_bytes = 0_usize;
    for argument in arguments {
        if output.len() >= limits.arguments {
            return Err(CliError::Usage(format!(
                "received more than {} command-line arguments",
                limits.arguments
            )));
        }
        let bytes = argument.as_encoded_bytes().len();
        if bytes > limits.argument_bytes {
            return Err(CliError::Usage(format!(
                "command-line argument {bytes} bytes long exceeds the {} byte per-argument limit",
                limits.argument_bytes
            )));
        }
        let next_total = total_bytes.checked_add(bytes).ok_or_else(|| {
            CliError::Usage("command-line argument byte count overflowed".to_owned())
        })?;
        if next_total > limits.total_bytes {
            return Err(CliError::Usage(format!(
                "command-line arguments total {next_total} bytes, exceeding the {} byte limit",
                limits.total_bytes
            )));
        }
        output.try_reserve(1).map_err(|error| {
            CliError::Runtime(Error::invalid_data(format!(
                "cannot grow command-line argument table: {error}"
            )))
        })?;
        output.push(argument);
        total_bytes = next_total;
    }
    Ok(output)
}

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
    let arguments = collect_cli_arguments(env::args_os().skip(1))?;
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
            &command.classes,
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
    /// Schema documents describing `MonoBehaviour` layouts, in the order
    /// given: the first document holding a class wins.
    mono_schemas: Vec<PathBuf>,
    /// Read through the schemas even where the file carries its own type tree.
    mono_schema_override: bool,
}

impl LoadOptions {
    /// Reads every schema document into one registry.
    ///
    /// `None` when no document was named, which leaves a stripped
    /// `MonoBehaviour` reported as unsupported rather than guessed at.
    fn mono_schema_registry(&self) -> CliResult<Option<MonoBehaviourSchemaRegistry>> {
        if self.mono_schemas.is_empty() {
            if self.mono_schema_override {
                return Err(CliError::Usage(format!(
                    "{MONO_SCHEMA_OVERRIDE_FLAG} has nothing to override with: pass --mono-schema"
                )));
            }
            return Ok(None);
        }
        if self.mono_schemas.len() > MAX_MONO_SCHEMA_DOCUMENTS {
            return Err(CliError::Usage(format!(
                "received {} --mono-schema documents, exceeding limit {MAX_MONO_SCHEMA_DOCUMENTS}",
                self.mono_schemas.len()
            )));
        }
        let mut registry = MonoBehaviourSchemaRegistry::new();
        registry.set_overrides_embedded_tree(self.mono_schema_override);
        let limits = MonoBehaviourSchemaDocumentLimits::default();
        let mut budget = MonoSchemaDocumentBudget::default();
        for path in &self.mono_schemas {
            let path_label = EscapedOsStr(path.as_os_str());
            let remaining = budget.remaining(limits)?;
            let file = File::open(path)
                .map_err(|error| CliError::Usage(format!("--mono-schema {path_label}: {error}")))?;
            let document = read_bounded_schema_document(file, remaining.maximum_document_bytes)
                .map_err(|error| CliError::Usage(format!("--mono-schema {path_label}: {error}")))?;
            let loaded = MonoBehaviourSchemaRegistry::from_json_with_limits(&document, remaining)
                .map_err(|error| {
                CliError::Usage(format!("--mono-schema {path_label}: {error}"))
            })?;
            budget.charge(document.len(), &loaded)?;
            registry.extend(loaded)?;
        }
        Ok(Some(registry))
    }
}

#[derive(Debug, Default)]
struct MonoSchemaDocumentBudget {
    document_bytes: usize,
    entries: usize,
    nodes: usize,
    string_bytes: usize,
}

impl MonoSchemaDocumentBudget {
    fn remaining(
        &self,
        limits: MonoBehaviourSchemaDocumentLimits,
    ) -> CliResult<MonoBehaviourSchemaDocumentLimits> {
        Ok(MonoBehaviourSchemaDocumentLimits {
            maximum_document_bytes: remaining_schema_budget(
                limits.maximum_document_bytes,
                self.document_bytes,
                "document bytes",
            )?,
            maximum_entries: remaining_schema_budget(
                limits.maximum_entries,
                self.entries,
                "entries",
            )?,
            maximum_nodes_per_entry: limits.maximum_nodes_per_entry,
            maximum_total_nodes: remaining_schema_budget(
                limits.maximum_total_nodes,
                self.nodes,
                "nodes",
            )?,
            maximum_string_bytes: limits.maximum_string_bytes,
            maximum_total_string_bytes: remaining_schema_budget(
                limits.maximum_total_string_bytes,
                self.string_bytes,
                "string bytes",
            )?,
        })
    }

    fn charge(
        &mut self,
        document_bytes: usize,
        registry: &MonoBehaviourSchemaRegistry,
    ) -> CliResult<()> {
        self.document_bytes =
            checked_schema_budget_add(self.document_bytes, document_bytes, "document bytes")?;
        self.entries =
            checked_schema_budget_add(self.entries, registry.entries().len(), "entry count")?;
        for entry in registry.entries() {
            self.nodes =
                checked_schema_budget_add(self.nodes, entry.tree.nodes.len(), "node count")?;
            for string in [
                entry.assembly_name.as_str(),
                entry.namespace.as_str(),
                entry.class_name.as_str(),
                entry.unity_version.as_deref().unwrap_or_default(),
            ] {
                self.string_bytes =
                    checked_schema_budget_add(self.string_bytes, string.len(), "string bytes")?;
            }
            for node in &entry.tree.nodes {
                self.string_bytes = checked_schema_budget_add(
                    self.string_bytes,
                    node.type_name.len(),
                    "string bytes",
                )?;
                self.string_bytes = checked_schema_budget_add(
                    self.string_bytes,
                    node.field_name.len(),
                    "string bytes",
                )?;
            }
        }
        Ok(())
    }
}

fn remaining_schema_budget(maximum: usize, used: usize, field: &str) -> CliResult<usize> {
    maximum.checked_sub(used).ok_or_else(|| {
        CliError::Usage(format!(
            "MonoBehaviour schema {field} already exceed the configured limit"
        ))
    })
}

fn checked_schema_budget_add(left: usize, right: usize, field: &str) -> CliResult<usize> {
    left.checked_add(right)
        .ok_or_else(|| CliError::Usage(format!("MonoBehaviour schema {field} overflowed")))
}

fn read_bounded_schema_document(
    mut reader: impl Read,
    maximum_document_bytes: usize,
) -> io::Result<Vec<u8>> {
    let mut document = Vec::new();
    let mut buffer = [0_u8; MONO_SCHEMA_READ_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(document);
        }
        let end = document
            .len()
            .checked_add(read)
            .ok_or_else(|| io::Error::other("MonoBehaviour schema document size overflowed"))?;
        if end > maximum_document_bytes {
            return Err(io::Error::other(format!(
                "MonoBehaviour schema documents exceed the {maximum_document_bytes} byte total limit"
            )));
        }
        document.try_reserve(read).map_err(|error| {
            io::Error::other(format!(
                "cannot allocate MonoBehaviour schema document: {error}"
            ))
        })?;
        document.extend_from_slice(&buffer[..read]);
    }
}

/// The load options this parser understands.
#[derive(Debug, Clone, Copy)]
enum LoadFlag {
    UnityVersion,
    MonoSchema,
}

/// A load option that is on or off rather than carrying a value.
const MONO_SCHEMA_OVERRIDE_FLAG: &str = "--mono-schema-override";

impl LoadFlag {
    const fn name(self) -> &'static str {
        match self {
            Self::UnityVersion => "--unity-version",
            Self::MonoSchema => "--mono-schema",
        }
    }

    const fn expects(self) -> &'static str {
        match self {
            Self::UnityVersion => "a version such as 2022.3.62f1",
            Self::MonoSchema => "a path to a schema document",
        }
    }
}

/// Removes the load options from the argument list before command parsing.
///
/// These apply to every command that opens a collection, so handling them once
/// here keeps each command parser unaware of them and makes the flags work with
/// the legacy `<input> -m <mode>` spellings too.
fn copy_cli_argument(value: &OsStr, field: &str) -> CliResult<OsString> {
    let mut copy = OsString::new();
    copy.try_reserve_exact(value.as_encoded_bytes().len())
        .map_err(|error| {
            CliError::Runtime(Error::invalid_data(format!(
                "cannot allocate {field}: {error}"
            )))
        })?;
    copy.push(value);
    Ok(copy)
}

fn copy_cli_path(value: &OsStr, field: &str) -> CliResult<PathBuf> {
    copy_path_argument(value, field).map_err(CliError::from)
}

fn copy_path_argument(value: &OsStr, field: &str) -> Result<PathBuf> {
    let mut copy = PathBuf::new();
    copy.try_reserve_exact(value.as_encoded_bytes().len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    copy.push(value);
    Ok(copy)
}

fn positional_path_table(command_name: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    paths.try_reserve_exact(2).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate {command_name} positional path table: {error}"
        ))
    })?;
    Ok(paths)
}

fn push_positional_path(paths: &mut Vec<PathBuf>, value: &OsStr, command_name: &str) -> Result<()> {
    if paths.len() >= 2 {
        return Err(Error::invalid_data(format!(
            "{command_name} accepts exactly two positional paths"
        )));
    }
    paths.push(copy_path_argument(value, "command-line path")?);
    Ok(())
}

fn split_load_options(arguments: &[OsString]) -> CliResult<(Vec<OsString>, LoadOptions)> {
    let mut remaining = Vec::new();
    remaining
        .try_reserve_exact(arguments.len())
        .map_err(|error| {
            CliError::Runtime(Error::invalid_data(format!(
                "cannot allocate filtered command-line argument table: {error}"
            )))
        })?;
    let mut load = LoadOptions::default();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let text = argument.to_str();
        let (flag, inline) = if text == Some(LoadFlag::UnityVersion.name()) {
            (LoadFlag::UnityVersion, None)
        } else if let Some(value) = text.and_then(|text| text.strip_prefix("--unity-version=")) {
            (LoadFlag::UnityVersion, Some(OsStr::new(value)))
        } else if text == Some(LoadFlag::MonoSchema.name()) {
            (LoadFlag::MonoSchema, None)
        } else if let Some(value) = text.and_then(|text| text.strip_prefix("--mono-schema=")) {
            (LoadFlag::MonoSchema, Some(OsStr::new(value)))
        } else if text == Some(MONO_SCHEMA_OVERRIDE_FLAG) {
            load.mono_schema_override = true;
            continue;
        } else {
            remaining.push(copy_cli_argument(
                argument,
                "filtered command-line argument",
            )?);
            continue;
        };
        let name = flag.name();
        let value = match inline {
            Some(value) => value,
            None => arguments
                .next()
                .map(OsString::as_os_str)
                .ok_or_else(|| CliError::Usage(format!("{name} requires {}", flag.expects())))?,
        };
        match flag {
            LoadFlag::UnityVersion => {
                let value = value
                    .to_str()
                    .ok_or_else(|| CliError::Usage(format!("{name} value must be valid UTF-8")))?;
                if load.unity_version.is_some() {
                    return Err(CliError::Usage(format!("{name} was given more than once")));
                }
                load.unity_version = Some(UnityVersion::from_str(value).map_err(|error| {
                    CliError::Usage(format!(
                        "{name} value {} is not a Unity version: {error}",
                        CliArgumentDisplay(OsStr::new(value))
                    ))
                })?);
            }
            // Repeatable: a game's classes are spread over several assemblies,
            // and one document per assembly is the shape a generator produces.
            LoadFlag::MonoSchema => {
                if load.mono_schemas.len() >= MAX_MONO_SCHEMA_DOCUMENTS {
                    return Err(CliError::Usage(format!(
                        "received more than {MAX_MONO_SCHEMA_DOCUMENTS} --mono-schema documents"
                    )));
                }
                let path = copy_cli_path(value, "MonoBehaviour schema path")?;
                load.mono_schemas.try_reserve(1).map_err(|error| {
                    CliError::Runtime(Error::invalid_data(format!(
                        "cannot grow MonoBehaviour schema path table: {error}"
                    )))
                })?;
                load.mono_schemas.push(path);
            }
        }
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
            CliArgumentDisplay(&arguments[1])
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
            CliArgumentDisplay(command)
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
    Ok(constructor(copy_cli_path(path, "read-only input path")?))
}

fn parse_bare_or_legacy_arguments(arguments: &[OsString]) -> CliResult<CliCommand> {
    let input = copy_cli_path(&arguments[0], "legacy input path")?;
    if arguments.len() == 1 {
        return Ok(CliCommand::Inspect(input));
    }

    let mut mode: Option<&str> = None;
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
                    CliError::Usage(format!("{} requires a value", CliArgumentDisplay(argument)))
                })?;
                let value = value
                    .to_str()
                    .ok_or_else(|| CliError::Usage("legacy mode must be valid UTF-8".to_owned()))?;
                if mode.replace(value).is_some() {
                    return Err(CliError::Usage(
                        "legacy mode may only be specified once".to_owned(),
                    ));
                }
            }
            Some("-o" | "--output") => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    CliError::Usage(format!("{} requires a value", CliArgumentDisplay(argument)))
                })?;
                if output
                    .replace(copy_cli_path(value, "legacy output path")?)
                    .is_some()
                {
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
                    CliArgumentDisplay(argument)
                )));
            }
        }
        index += 1;
    }

    let mode = mode.unwrap_or(if output.is_some() {
        "export"
    } else {
        "inspect"
    });
    dispatch_legacy_mode(
        input,
        mode,
        output,
        overwrite_existing,
        restore_text_asset_extension,
    )
}

fn dispatch_legacy_mode(
    input: PathBuf,
    mode: &str,
    output: Option<PathBuf>,
    overwrite_existing: bool,
    restore_text_asset_extension: bool,
) -> CliResult<CliCommand> {
    let read_only = mode.eq_ignore_ascii_case("inspect") || mode.eq_ignore_ascii_case("info");
    if read_only && output.is_none() && !overwrite_existing && restore_text_asset_extension {
        return if mode.eq_ignore_ascii_case("inspect") {
            Ok(CliCommand::Inspect(input))
        } else {
            Ok(CliCommand::Info(input))
        };
    }
    if read_only {
        return Err(CliError::Usage(format!(
            "legacy {} mode is read-only and does not accept export options",
            CliArgumentDisplay(OsStr::new(mode))
        )));
    }
    if mode.eq_ignore_ascii_case("extract") {
        return parse_legacy_extract(
            input,
            output,
            overwrite_existing,
            restore_text_asset_extension,
        );
    }
    if mode.eq_ignore_ascii_case("l2d") || mode.eq_ignore_ascii_case("live2d") {
        return parse_legacy_live2d(
            input,
            output,
            overwrite_existing,
            restore_text_asset_extension,
        );
    }
    if mode.eq_ignore_ascii_case("animator") || mode.eq_ignore_ascii_case("splitobjects") {
        return parse_legacy_fbx_batch(input, output, mode);
    }
    let export_mode = if mode.eq_ignore_ascii_case("export") {
        Some(ExportMode::Auto)
    } else if mode.eq_ignore_ascii_case("raw") || mode.eq_ignore_ascii_case("exportraw") {
        Some(ExportMode::Raw)
    } else if mode.eq_ignore_ascii_case("dump") {
        Some(ExportMode::DumpText)
    } else {
        None
    };
    if let Some(export_mode) = export_mode {
        let output = output.ok_or_else(|| {
            CliError::Usage(
                "legacy write modes require -o/--output; implicit ASExport creation is disabled"
                    .to_owned(),
            )
        })?;
        let options = ExportOptions {
            mode: export_mode,
            overwrite_existing,
            restore_text_asset_extension,
            ..ExportOptions::default()
        };
        return Ok(CliCommand::Export(ExportCommand {
            input,
            output,
            options,
            // The legacy spellings never took a class filter, and adding one
            // to them would be inventing a command that never existed.
            classes: Vec::new(),
        }));
    }
    Err(CliError::Usage(format!(
        "legacy mode {} is not implemented by the native CLI",
        CliArgumentDisplay(OsStr::new(mode))
    )))
}

fn parse_legacy_live2d(
    input: PathBuf,
    output: Option<PathBuf>,
    overwrite_existing: bool,
    restore_text_asset_extension: bool,
) -> CliResult<CliCommand> {
    let output = output.ok_or_else(|| {
        CliError::Usage(
            "legacy Live2D mode requires -o/--output; implicit ASExport creation is disabled"
                .to_owned(),
        )
    })?;
    if overwrite_existing {
        return Err(CliError::Usage(
            "legacy Live2D overwrite is not supported; existing packages are never overwritten"
                .to_owned(),
        ));
    }
    if !restore_text_asset_extension {
        return Err(CliError::Usage(
            "--not-restore-extension is not valid for Live2D mode".to_owned(),
        ));
    }
    Ok(CliCommand::Live2dPackage(Live2dCommand { input, output }))
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
         Invocation limits: {MAX_CLI_ARGUMENTS} arguments, {MAX_CLI_ARGUMENT_BYTES} encoded\n  \
         bytes per argument, and {MAX_CLI_ARGUMENT_TOTAL_BYTES} encoded bytes in total.\n\n\
         Read-only commands:\n  inspect  Show container and serialized-file structure\n  \
         info     Summarize serialized files, Unity versions, and class counts\n  \
         list     List every discovered serialized object\n  \
         scene    Print the assembled GameObject hierarchy and model bindings\n\n\
         Load options (accepted by every command that opens a collection):\n  \
         --unity-version <VERSION>   Parse against this version, for example 2022.3.62f1.\n  \
         Required for files whose own version was stripped at build time, and\n  \
         overrides both the declared version and any enclosing bundle revision.\n  \
         --mono-schema <PATH>        Read MonoBehaviour layouts from a schema document.\n  \
         A release build usually ships no type tree for its own scripts, and\n  \
         those objects are reported unsupported without one. Repeatable; the\n  \
         first document holding a class wins. See docs/mono-schema.md.\n  \
         --mono-schema-override      Read through the schemas even where the file carries\n  \
         its own type tree. For checking a generated schema against a build\n  \
         that still ships trees; extraction should not use it.\n\n\
         FBX export:\n  Writes deterministic ASCII FBX 7.4 for transform hierarchies, resident\n  \
         triangle meshes, submeshes, material slots, normals, UV0, local TRS, direct/hash bones,\n  \
         skinning, static blend shapes, explicit/packed legacy curves, and streamed/dense/constant\n  \
         Transform or blend-shape samples.\n  \
         Material textures are decoded and written beside the FBX, which references\n  \
         them by file name.\n  \
         FBX options:\n  --maximum-output-bytes <N>  Maximum bytes newly published by this command,\n  \
         including the model, companion MTL, and textures; N must be a positive\n  \
         integer no greater than 536870912; the default is 16777216 bytes\n  \
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
         --class <ID>                  Export only this class, repeatable. IDs are the\n  \
         numbers list and export print, for example 114 for MonoBehaviour.\n  \
         --compact-json\n\n\
         Extract options:\n  --overwrite\n\n\
         Live2D export:\n  Exports only MonoBehaviours whose resolved MonoScript class is CubismMoc.\n  \
         Existing files are never overwritten.\n  \
         live2d-package exports verified MOC, texture PNG, model3.json, expression, motion,\n  \
         physics, pose, and display-info files when embedded or supplied schemas are available.\n\n\
         Legacy compatibility:\n  assetstudio <input> -m info\n  \
         assetstudio <input> -m <export|exportRaw|dump|extract|l2d|live2d|animator|splitObjects> -o <output>\n  \
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
    /// Class IDs to export, in the order given. Empty exports every class.
    classes: Vec<i32>,
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
    let mut positional = positional_path_table(command_name)?;
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
                CliArgumentDisplay(argument)
            )));
        } else {
            push_positional_path(&mut positional, argument, command_name)?;
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
    let mut positional = positional_path_table(command_name)?;
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
                CliArgumentDisplay(argument)
            )));
        } else {
            push_positional_path(&mut positional, argument, command_name)?;
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
    let mut positional = positional_path_table(command_name)?;
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
                CliArgumentDisplay(argument)
            )));
        } else {
            push_positional_path(&mut positional, argument, command_name)?;
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
    let mut positional = positional_path_table("extract")?;
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
                CliArgumentDisplay(argument)
            )));
        } else {
            push_positional_path(&mut positional, argument, "extract")?;
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
    let mut classes = Vec::new();
    let mut positional = positional_path_table("export")?;
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
        } else if parse_options && argument == "--class" {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or_else(|| Error::invalid_data("--class requires a class ID"))?;
            push_class_filter(&mut classes, value)?;
        } else if parse_options
            && argument
                .to_str()
                .is_some_and(|value| value.starts_with('-'))
        {
            return Err(Error::invalid_data(format!(
                "unknown export option: {}",
                CliArgumentDisplay(argument)
            )));
        } else {
            push_positional_path(&mut positional, argument, "export")?;
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
        classes,
    })
}

/// Reads a class ID as the `list` and `export` output prints it.
///
/// Numbers rather than names: this reader has no class-name table, and one
/// invented here would be wrong for exactly the classes a caller most needs to
/// name -- the ones a new Unity version added.
fn parse_class_id(value: &OsString) -> Result<i32> {
    value
        .to_str()
        .and_then(|value| value.parse::<i32>().ok())
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "invalid class ID: {} (expected a number, as `list` prints)",
                CliArgumentDisplay(value)
            ))
        })
}

fn push_class_filter(classes: &mut Vec<i32>, value: &OsString) -> Result<()> {
    let class_id = parse_class_id(value)?;
    classes.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow export class filter table: {error}"))
    })?;
    classes.push(class_id);
    Ok(())
}

fn parse_export_mode(value: &OsString) -> Result<ExportMode> {
    match value.to_str() {
        Some("auto") => Ok(ExportMode::Auto),
        Some("raw") => Ok(ExportMode::Raw),
        Some("typetree-json") => Ok(ExportMode::TypeTreeJson),
        Some("dump-text") => Ok(ExportMode::DumpText),
        _ => Err(Error::invalid_data(format!(
            "invalid export mode: {} (expected auto, raw, typetree-json, or dump-text)",
            CliArgumentDisplay(value)
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
            CliArgumentDisplay(value)
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
            CliArgumentDisplay(value)
        ))),
    }
}

fn parse_jpeg_quality(value: &OsString) -> Result<u8> {
    let text = value
        .to_str()
        .ok_or_else(|| Error::invalid_data("JPEG quality must be valid UTF-8"))?;
    let quality = text.parse::<u8>().map_err(|_| {
        Error::invalid_data(format!(
            "invalid JPEG quality {} (expected an integer from 1 through 100)",
            CliArgumentDisplay(value)
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
            CliArgumentDisplay(value)
        ))),
    }
}

fn export_path(
    input: &Path,
    output_directory: &Path,
    options: ExportOptions,
    classes: &[i32],
    load: &LoadOptions,
    output: &mut impl Write,
) -> CliResult<()> {
    let collection = load_asset_collection(input, load, output)?;
    let schemas = load.mono_schema_registry()?;
    let report = export_collection_with_plan(
        &collection,
        output_directory,
        options,
        ExportPlan {
            mono_schemas: schemas
                .as_ref()
                .map(|registry| registry as &dyn MonoBehaviourSchemaProvider),
            classes: (!classes.is_empty()).then_some(classes),
        },
    )?;

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
    // The FBX references its textures by file name, so they only resolve once
    // they sit beside it. Publish the model last: a texture batch is
    // transactional, and a late model collision rolls its newly written files
    // back rather than leaving an incomplete multi-file export.
    let publication = publish_fbx_with_textures(
        &mut temporary,
        &command.output,
        &textures,
        0,
        0,
        written,
        command.maximum_output_bytes,
    )?;
    writeln!(
        output,
        "exported {} FBX 7.4 ({written} bytes, {} animation clips) -> {}",
        if command.binary { "binary" } else { "ASCII" },
        animations.clips.len(),
        command.output.display()
    )?;
    report_model_textures(
        command.textures,
        &textures,
        publication.written_textures.len(),
        output,
    )?;
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

    let mut obj_temporary = FbxTemporaryFile::create(&parent)?;
    let written = write_model_ir_obj(
        &model,
        Some(mtl_name.as_str()),
        obj_temporary.file_mut(),
        command.maximum_output_bytes,
    )?;
    obj_temporary.file_mut().flush()?;
    obj_temporary.file_mut().sync_all()?;
    obj_temporary.close()?;

    let mut mtl_temporary = FbxTemporaryFile::create(&parent)?;
    let mtl_written = write_model_ir_mtl(
        &model,
        &textures,
        mtl_temporary.file_mut(),
        command.maximum_output_bytes,
    )?;
    mtl_temporary.file_mut().flush()?;
    mtl_temporary.file_mut().sync_all()?;
    mtl_temporary.close()?;

    let written_textures = textures.write_to_directory(&parent)?;
    let prepared = (|| {
        let texture_bytes = published_file_bytes(&written_textures, "OBJ texture")?;
        let total_output_bytes = written
            .checked_add(mtl_written)
            .and_then(|value| value.checked_add(texture_bytes))
            .ok_or_else(|| Error::invalid_data("OBJ output byte count overflowed"))?;
        if total_output_bytes > command.maximum_output_bytes {
            return Err(Error::invalid_data(format!(
                "OBJ output exceeds the {} byte total output limit",
                command.maximum_output_bytes
            )));
        }
        Ok(())
    })();
    if let Err(error) = prepared {
        return Err(rollback_model_publication(error, &written_textures, None).into());
    }
    if let Err(error) = mtl_temporary.persist_no_clobber(&mtl_path) {
        return Err(rollback_model_publication(error, &written_textures, None).into());
    }
    if let Err(error) = obj_temporary.persist_no_clobber(&command.output) {
        return Err(rollback_model_publication(error, &written_textures, Some(&mtl_path)).into());
    }
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

fn rollback_model_publication(
    error: Error,
    textures: &[PathBuf],
    material_library: Option<&Path>,
) -> Error {
    let mut cleanup_error = None;
    if let Some(path) = material_library
        && let Err(cleanup) = fs::remove_file(path)
        && cleanup.kind() != io::ErrorKind::NotFound
    {
        cleanup_error = Some(cleanup);
    }
    for path in textures.iter().rev() {
        if let Err(cleanup) = fs::remove_file(path)
            && cleanup.kind() != io::ErrorKind::NotFound
            && cleanup_error.is_none()
        {
            cleanup_error = Some(cleanup);
        }
    }
    match cleanup_error {
        None => error,
        Some(cleanup) => Error::invalid_data(format!(
            "{error}; additionally failed to roll back model export files: {cleanup}"
        )),
    }
}

fn published_file_bytes(paths: &[PathBuf], output_kind: &str) -> Result<u64> {
    paths.iter().try_fold(0_u64, |total, path| {
        total
            .checked_add(fs::metadata(path)?.len())
            .ok_or_else(|| Error::invalid_data(format!("{output_kind} byte count overflowed")))
    })
}

/// Publishes one complete texture set before its referring FBX.
///
/// Any error after the texture batch succeeds removes only files created by
/// this call; files skipped because they already existed never enter
/// `written_textures`. The aggregate byte budget is checked against those
/// actual new files before the referring FBX reaches its commit point.
#[derive(Debug)]
struct FbxPublication {
    written_textures: Vec<PathBuf>,
    total_texture_files: usize,
    written_bytes: u64,
    total_output_bytes: u64,
}

fn publish_fbx_with_textures(
    temporary: &mut FbxTemporaryFile,
    destination: &Path,
    textures: &SceneTextureSet,
    existing_texture_files: usize,
    previous_output_bytes: u64,
    model_bytes: u64,
    maximum_total_output_bytes: u64,
) -> Result<FbxPublication> {
    let directory = destination
        .parent()
        .ok_or_else(|| Error::invalid_data("FBX destination has no parent directory"))?;
    let written_textures = textures.write_to_directory(directory)?;
    let prepared = (|| {
        let total_texture_files = existing_texture_files
            .checked_add(written_textures.len())
            .ok_or_else(|| Error::invalid_data("FBX batch texture count overflowed"))?;
        let texture_bytes = published_file_bytes(&written_textures, "FBX texture")?;
        let written_bytes = model_bytes
            .checked_add(texture_bytes)
            .ok_or_else(|| Error::invalid_data("FBX output byte count overflowed"))?;
        let total_output_bytes = previous_output_bytes
            .checked_add(written_bytes)
            .ok_or_else(|| Error::invalid_data("FBX batch byte count overflowed"))?;
        if total_output_bytes > maximum_total_output_bytes {
            return Err(Error::invalid_data(format!(
                "FBX output exceeds the {maximum_total_output_bytes} byte total output limit"
            )));
        }
        Ok((total_texture_files, written_bytes, total_output_bytes))
    })();
    let (total_texture_files, written_bytes, total_output_bytes) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return Err(rollback_model_publication(error, &written_textures, None));
        }
    };
    if let Err(error) = temporary.persist_no_clobber(destination) {
        return Err(rollback_model_publication(error, &written_textures, None));
    }
    Ok(FbxPublication {
        written_textures,
        total_texture_files,
        written_bytes,
        total_output_bytes,
    })
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
            Ok((written, next_total_bytes)) => {
                total_bytes = next_total_bytes;
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
) -> Result<(u64, u64)> {
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
    let publication = publish_fbx_with_textures(
        &mut temporary,
        destination,
        &textures,
        *texture_files,
        previously_written,
        written,
        MAX_FBX_BATCH_TOTAL_BYTES,
    )?;
    *texture_files = publication.total_texture_files;
    Ok((publication.written_bytes, publication.total_output_bytes))
}

fn allocate_fbx_batch_name(
    candidate: &ModelExportCandidate,
    names: &mut HashSet<String>,
) -> Result<String> {
    let base = sanitize_live2d_base_name(&candidate.name)?;
    for suffix in 0_u64..=MAX_FBX_TEMPORARY_ATTEMPTS {
        let value = if suffix == 0 {
            fallible_fbx_name(&base)?
        } else {
            fallible_fbx_suffixed_name(&base, suffix)?
        };
        let portable = fallible_lowercase(&value, "FBX portable output name")?;
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
    fallible_copy_string(base, "FBX output name")
}

fn fallible_copy_string(value: &str, field: &'static str) -> Result<String> {
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    output.push_str(value);
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
        Path::new(".")
    } else {
        raw_parent
    };
    ensure_secure_cli_output_directory(parent, "FBX")
}

fn lexical_cli_output_path(path: &Path, output_kind: &str) -> Result<PathBuf> {
    let current_directory = (!path.is_absolute()).then(env::current_dir).transpose()?;
    let capacity = current_directory
        .as_ref()
        .map_or(0, |directory| {
            directory.as_os_str().as_encoded_bytes().len()
        })
        .checked_add(path.as_os_str().as_encoded_bytes().len())
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| Error::invalid_data(format!("{output_kind} output path overflowed")))?;
    let mut joined = PathBuf::new();
    joined.try_reserve_exact(capacity).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate {output_kind} output path: {error}"
        ))
    })?;
    if let Some(directory) = current_directory {
        joined.push(directory);
    }
    joined.push(path);

    let mut normalized = PathBuf::new();
    normalized
        .try_reserve_exact(joined.as_os_str().as_encoded_bytes().len())
        .map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate normalized {output_kind} output path: {error}"
            ))
        })?;
    for component in joined.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(Error::invalid_data(format!(
                        "{output_kind} output path escapes the filesystem root: {}",
                        path.display()
                    )));
                }
            }
        }
    }
    Ok(normalized)
}

fn is_trusted_cli_output_alias(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let expected = if path == Path::new("/var") {
            Some(Path::new("/private/var"))
        } else if path == Path::new("/tmp") {
            Some(Path::new("/private/tmp"))
        } else {
            None
        };
        expected
            .is_some_and(|expected| fs::canonicalize(path).is_ok_and(|target| target == expected))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

fn ensure_secure_cli_output_directory(path: &Path, output_kind: &str) -> Result<PathBuf> {
    let normalized = lexical_cli_output_path(path, output_kind)?;
    let mut current = PathBuf::new();
    current
        .try_reserve_exact(normalized.as_os_str().as_encoded_bytes().len())
        .map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate secure {output_kind} output path: {error}"
            ))
        })?;
    for component in normalized.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if is_trusted_cli_output_alias(&current) {
                    continue;
                }
                return Err(Error::invalid_data(format!(
                    "refusing symbolic-link in {output_kind} output path: {}",
                    current.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(Error::invalid_data(format!(
                    "{output_kind} output path component is not a directory: {}",
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
                        "{output_kind} output path component became unsafe: {}",
                        current.display()
                    )));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(normalized)
}

/// Creates the destination link and treats that link as the commit point.
///
/// Removing the temporary name is cleanup, not publication. A failed cleanup
/// therefore leaves `false` for the owner's `Drop` implementation to retry,
/// but it must not turn an already visible destination into a reported error.
fn persist_temporary_hard_link(
    temporary: &Path,
    destination: &Path,
    output_kind: &str,
    remove_temporary: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<bool> {
    match fs::hard_link(temporary, destination) {
        Ok(()) => Ok(remove_temporary(temporary).is_ok()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Err(Error::invalid_data(format!(
                "refusing to overwrite existing {output_kind} output: {}",
                destination.display()
            )))
        }
        Err(error) => Err(error.into()),
    }
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
        self.persisted = persist_temporary_hard_link(&self.path, destination, "FBX", |path| {
            fs::remove_file(path)
        })?;
        Ok(())
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
    for (file_index, loaded) in collection.serialized_files().iter().enumerate() {
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
            candidates.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow Live2D candidate table: {error}"))
            })?;
            candidates.push(Live2dCandidate {
                file_index,
                object_index,
            });
        }
    }
    candidates.sort_unstable_by(|left, right| {
        let left_file = &collection.serialized_files()[left.file_index];
        let right_file = &collection.serialized_files()[right.file_index];
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
    let loaded = &collection.serialized_files()[candidate.file_index];
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
    let next_exported_bytes = match charge_live2d_model(state, model.model_data.len()) {
        Ok(next) => next,
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

    if !state.output_ready {
        create_live2d_output_root(output_directory)?;
        state.output_ready = true;
    }
    let base_name = sanitize_live2d_base_name(&model.name)?;
    let output_path = allocate_live2d_output_path(
        output_directory,
        &base_name,
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
            state.exported_bytes = next_exported_bytes;
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

fn charge_live2d_model(state: &mut Live2dExportState, model_bytes: u64) -> Result<u64> {
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
        .exported_bytes
        .checked_add(model_bytes)
        .ok_or_else(|| Error::invalid_data("Live2D output byte count overflowed"))?;
    if next_bytes > MAX_LIVE2D_TOTAL_OUTPUT_BYTES {
        return Err(Error::invalid_data(format!(
            "Live2D output exceeds {MAX_LIVE2D_TOTAL_OUTPUT_BYTES} total bytes"
        )));
    }
    Ok(next_bytes)
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

fn sanitize_live2d_base_name(value: &str) -> Result<String> {
    let maximum = value.len().min(MAX_LIVE2D_BASE_NAME_BYTES);
    let mut sanitized = String::new();
    sanitized.try_reserve_exact(maximum).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Live2D base name: {error}"))
    })?;
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
    let trimmed = sanitized.trim_matches([' ', '.']);
    let mut base_name = String::new();
    base_name
        .try_reserve_exact(trimmed.len())
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D base name: {error}"))
        })?;
    base_name.push_str(trimmed);
    if base_name
        .get(base_name.len().saturating_sub(5)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".moc3"))
    {
        base_name.truncate(base_name.len() - 5);
        let trimmed_length = base_name.trim_end_matches([' ', '.']).len();
        base_name.truncate(trimmed_length);
    }
    if base_name.is_empty() || base_name == "." || base_name == ".." {
        base_name.clear();
        base_name
            .try_reserve_exact("unnamed".len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D fallback name: {error}"))
            })?;
        base_name.push_str("unnamed");
        Ok(base_name)
    } else {
        Ok(base_name)
    }
}

fn allocate_live2d_output_path(
    output_directory: &Path,
    base_name: &str,
    path_id: i64,
    candidate: Live2dCandidate,
    claimed_paths: &mut HashSet<String>,
) -> Result<PathBuf> {
    for variant in 0..3 {
        let file_name = fallible_live2d_output_name(base_name, path_id, candidate, variant)?;
        let portable = fallible_lowercase(&file_name, "Live2D portable output name")?;
        if !claimed_paths.contains(&portable) {
            claimed_paths.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow Live2D output-name index: {error}"))
            })?;
            claimed_paths.insert(portable);
            return Ok(output_directory.join(file_name));
        }
    }
    Err(Error::invalid_data(format!(
        "cannot create a unique Live2D output name for path ID {path_id}"
    )))
}

fn fallible_live2d_output_name(
    base_name: &str,
    path_id: i64,
    candidate: Live2dCandidate,
    variant: usize,
) -> Result<String> {
    let mut output = String::new();
    output
        .try_reserve(base_name.len().saturating_add(80))
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D output name: {error}"))
        })?;
    match variant {
        0 => write!(output, "{base_name}.moc3"),
        1 => write!(output, "{base_name} @{path_id}.moc3"),
        2 => write!(
            output,
            "{base_name} @{path_id} f{:04}o{}.moc3",
            candidate.file_index, candidate.object_index
        ),
        _ => return Err(Error::invalid_data("unknown Live2D output-name variant")),
    }
    .map_err(|error| Error::invalid_data(format!("cannot format Live2D output name: {error}")))?;
    Ok(output)
}

fn fallible_lowercase(value: &str, field: &'static str) -> Result<String> {
    let length = value
        .chars()
        .flat_map(char::to_lowercase)
        .try_fold(0_usize, |length, character| {
            length.checked_add(character.len_utf8())
        })
        .ok_or_else(|| Error::invalid_data(format!("{field} length overflowed")))?;
    let mut output = String::new();
    output
        .try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    output.extend(value.chars().flat_map(char::to_lowercase));
    Ok(output)
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
        self.persisted = persist_temporary_hard_link(&self.path, destination, "Live2D", |path| {
            fs::remove_file(path)
        })?;
        Ok(())
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
            create_live2d_output_root(output_directory)?;
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

fn create_live2d_output_root(output_directory: &Path) -> Result<()> {
    ensure_secure_cli_output_directory(output_directory, "Live2D")?;
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
    if publication_lock.release_after_commit() {
        // The package rename was already durably synced above. Persisting the
        // lock cleanup is best effort and cannot reverse that commit.
        let _ = sync_directory(output_root);
    }
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
            let base_name = sanitize_live2d_base_name(name)?;
            let path = root.join(format!(
                ".assetstudio-live2d-package-{}-{}-{}.tmp",
                std::process::id(),
                sequence,
                base_name
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
        let base_name = sanitize_live2d_base_name(name)?;
        let path = root.join(format!(
            ".assetstudio-live2d-package-publish-{base_name}.lock"
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

    fn release_after_commit(&mut self) -> bool {
        self.release_after_commit_with(|path| fs::remove_file(path))
    }

    fn release_after_commit_with(
        &mut self,
        remove_lock: impl FnOnce(&Path) -> io::Result<()>,
    ) -> bool {
        self.file.take();
        remove_lock(&self.path).is_ok()
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
    if collection.serialized_files().is_empty()
        && collection.resources().is_empty()
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
                EscapedOsStr(file.as_os_str())
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
    let mut class_counts = HashMap::<i32, usize>::new();
    let mut unity_versions = HashMap::<String, usize>::new();

    writeln!(output, "{}", path.display())?;
    writeln!(
        output,
        "  serialized files: {}",
        collection.serialized_files().len()
    )?;
    writeln!(output, "  resources: {}", collection.resources().len())?;

    for loaded in collection.serialized_files() {
        total_objects = total_objects
            .checked_add(loaded.file.objects.len())
            .ok_or_else(|| Error::invalid_data("serialized object count overflowed"))?;
        increment_string_count(
            &mut unity_versions,
            &loaded.file.unity_version_string,
            "Unity version",
        )?;

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
            increment_class_count(&mut class_counts, object.class_id)?;
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
        for (version, count) in sorted_map_entries(unity_versions, "Unity version summary")? {
            writeln!(output, "    {}: {count} file(s)", escape_text(&version))?;
        }
    }
    writeln!(output, "  object classes:")?;
    if class_counts.is_empty() {
        writeln!(output, "    none")?;
    } else {
        for (class_id, count) in sorted_map_entries(class_counts, "class summary")? {
            writeln!(
                output,
                "    {class_id}{}: {count}",
                class_name_suffix(class_id)
            )?;
        }
    }
    skipped_input_result("info/list", &collection)
}

fn increment_class_count(counts: &mut HashMap<i32, usize>, class_id: i32) -> Result<()> {
    if let Some(count) = counts.get_mut(&class_id) {
        *count = count
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("serialized class count overflowed"))?;
        return Ok(());
    }
    counts.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow serialized class summary: {error}"))
    })?;
    counts.insert(class_id, 1);
    Ok(())
}

fn increment_string_count(
    counts: &mut HashMap<String, usize>,
    value: &str,
    field: &'static str,
) -> Result<()> {
    if let Some(count) = counts.get_mut(value) {
        *count = count
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data(format!("{field} count overflowed")))?;
        return Ok(());
    }
    counts
        .try_reserve(1)
        .map_err(|error| Error::invalid_data(format!("cannot grow {field} summary: {error}")))?;
    counts.insert(fallible_copy_string(value, field)?, 1);
    Ok(())
}

fn sorted_map_entries<K: Ord, V>(
    entries: HashMap<K, V>,
    field: &'static str,
) -> Result<Vec<(K, V)>> {
    let mut sorted = Vec::new();
    sorted
        .try_reserve_exact(entries.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    sorted.extend(entries);
    sorted.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(sorted)
}

fn report_scene(path: &Path, load: &LoadOptions, output: &mut impl Write) -> CliResult<()> {
    let collection = load_asset_collection(path, load, output)?;
    let hierarchy = build_scene_hierarchy(&collection, SceneHierarchyLimits::default())?;
    writeln!(output, "scene {}", path.display())?;
    writeln!(
        output,
        "  serialized files: {}",
        collection.serialized_files().len()
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
        .serialized_files()
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

#[derive(Debug, Default)]
struct InspectPathBudget {
    bytes: usize,
}

impl InspectPathBudget {
    fn charge(&mut self, length: usize, limits: AssetLoadLimits) -> Result<()> {
        if length > limits.maximum_path_bytes {
            return Err(Error::invalid_data(format!(
                "inspect filesystem path is {length} UTF-8 bytes, exceeding limit {}",
                limits.maximum_path_bytes
            )));
        }
        let total = self
            .bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("inspect filesystem path byte count overflowed"))?;
        if total > limits.maximum_total_path_bytes {
            return Err(Error::invalid_data(format!(
                "inspect filesystem paths total {total} UTF-8 bytes, exceeding limit {}",
                limits.maximum_total_path_bytes
            )));
        }
        self.bytes = total;
        Ok(())
    }
}

fn inspect_path_byte_length(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

#[cfg(not(windows))]
fn for_each_inspect_utf8_char(
    mut input: &[u8],
    mut visitor: impl FnMut(char) -> Result<()>,
) -> Result<()> {
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
                        Error::invalid_data("valid inspect path prefix could not be decoded")
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

#[cfg(windows)]
fn for_each_inspect_os_str_char(
    value: &OsStr,
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
fn for_each_inspect_os_str_char(
    value: &OsStr,
    visitor: impl FnMut(char) -> Result<()>,
) -> Result<()> {
    for_each_inspect_utf8_char(value.as_encoded_bytes(), visitor)
}

fn inspect_os_str_utf8_length(value: &OsStr) -> Result<usize> {
    let mut length = 0_usize;
    for_each_inspect_os_str_char(value, |character| {
        length = length
            .checked_add(character.len_utf8())
            .ok_or_else(|| Error::invalid_data("inspect filesystem path length overflowed"))?;
        Ok(())
    })?;
    Ok(length)
}

struct EscapedOsStr<'a>(&'a OsStr);

impl fmt::Display for EscapedOsStr<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for_each_inspect_os_str_char(self.0, |character| {
            for escaped in character.escape_default() {
                formatter.write_char(escaped).map_err(|_| {
                    Error::invalid_data("cannot stream escaped inspect filesystem path")
                })?;
            }
            Ok(())
        })
        .map_err(|_| fmt::Error)
    }
}

struct LossyOsStr<'a>(&'a OsStr);

impl fmt::Display for LossyOsStr<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for_each_inspect_os_str_char(self.0, |character| {
            formatter
                .write_char(character)
                .map_err(|_| Error::invalid_data("cannot stream inspect filesystem path"))?;
            Ok(())
        })
        .map_err(|_| fmt::Error)
    }
}

enum InspectLabelComponent<'a> {
    Plain(&'a str),
    Escaped(&'a str),
}

impl fmt::Display for InspectLabelComponent<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plain(value) => formatter.write_str(value),
            Self::Escaped(value) => write!(formatter, "{}", escape_text(value)),
        }
    }
}

struct NestedInspectLabel<'a> {
    parent: &'a dyn fmt::Display,
    component: InspectLabelComponent<'a>,
}

impl fmt::Display for NestedInspectLabel<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}::{}", self.parent, self.component)
    }
}

fn copy_inspect_path(
    path: &Path,
    limits: AssetLoadLimits,
    budget: &mut InspectPathBudget,
) -> Result<PathBuf> {
    let utf8_length = inspect_os_str_utf8_length(path.as_os_str())?;
    budget.charge(utf8_length, limits)?;
    let encoded_length = inspect_path_byte_length(path);
    let mut copy = PathBuf::new();
    copy.try_reserve_exact(encoded_length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate inspect filesystem path: {error}"))
    })?;
    copy.push(path);
    Ok(copy)
}

fn join_inspect_path(
    parent: &Path,
    child: &OsStr,
    limits: AssetLoadLimits,
    budget: &mut InspectPathBudget,
) -> Result<PathBuf> {
    let parent_bytes = parent.as_os_str().as_encoded_bytes();
    let separator_length =
        usize::from(!parent_bytes.is_empty() && !matches!(parent_bytes.last(), Some(b'/' | b'\\')));
    let encoded_length = parent_bytes
        .len()
        .checked_add(separator_length)
        .and_then(|length| length.checked_add(child.as_encoded_bytes().len()))
        .ok_or_else(|| Error::invalid_data("inspect filesystem path length overflowed"))?;
    let parent_utf8_length = inspect_os_str_utf8_length(parent.as_os_str())?;
    let child_utf8_length = inspect_os_str_utf8_length(child)?;
    let utf8_length = parent_utf8_length
        .checked_add(separator_length)
        .and_then(|length| length.checked_add(child_utf8_length))
        .ok_or_else(|| Error::invalid_data("inspect filesystem path length overflowed"))?;
    budget.charge(utf8_length, limits)?;
    let mut path = PathBuf::new();
    path.try_reserve_exact(encoded_length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate inspect filesystem path: {error}"))
    })?;
    path.push(parent);
    path.push(child);
    if inspect_path_byte_length(&path) > encoded_length {
        return Err(Error::invalid_data(
            "inspect filesystem path grew beyond its checked allocation",
        ));
    }
    Ok(path)
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let limits = AssetLoadLimits::default();
    let mut path_budget = InspectPathBudget::default();
    let mut pending_directories = Vec::new();
    pending_directories.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot allocate directory queue: {error}"))
    })?;
    pending_directories.push(copy_inspect_path(root, limits, &mut path_budget)?);
    let mut files = Vec::new();
    let mut directory_count = 1_usize;
    let mut entry_count = 0_usize;
    while let Some(directory) = pending_directories.pop() {
        let mut children = Vec::new();
        for child in fs::read_dir(&directory)? {
            entry_count = entry_count
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("directory entry count overflowed"))?;
            if entry_count > limits.maximum_directory_entries {
                return Err(Error::invalid_data(format!(
                    "directory traversal exceeds {} entries",
                    limits.maximum_directory_entries
                )));
            }
            let child = child?;
            let file_type = child.file_type()?;
            if !file_type.is_dir() && !file_type.is_file() {
                continue;
            }
            let path = join_inspect_path(&directory, &child.file_name(), limits, &mut path_budget)?;
            children.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot allocate directory entries: {error}"))
            })?;
            children.push((path, file_type));
        }
        children.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        for (path, file_type) in children.into_iter().rev() {
            if file_type.is_dir() {
                directory_count = directory_count
                    .checked_add(1)
                    .ok_or_else(|| Error::invalid_data("directory count overflowed"))?;
                if directory_count > limits.maximum_input_directories {
                    return Err(Error::invalid_data(format!(
                        "directory traversal exceeds {} directories",
                        limits.maximum_input_directories
                    )));
                }
                pending_directories.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow directory queue: {error}"))
                })?;
                pending_directories.push(path);
            } else if file_type.is_file() {
                if files.len() >= limits.maximum_input_files {
                    return Err(Error::invalid_data(format!(
                        "directory traversal exceeds {} files",
                        limits.maximum_input_files
                    )));
                }
                files.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow input file list: {error}"))
                })?;
                files.push(path);
            }
        }
    }
    files.sort_unstable();
    Ok(files)
}

fn inspect_file(path: &Path, output: &mut impl Write) -> Result<()> {
    let region = Region::from_file(path)?;
    let label = LossyOsStr(path.as_os_str());
    inspect_region(&label, region, output, 0)
}

fn inspect_region(
    label: &dyn fmt::Display,
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
    label: &dyn fmt::Display,
    wrapper: &str,
    decoded: Region,
    output: &mut impl Write,
    depth: usize,
) -> Result<()> {
    let next_depth = checked_compression_depth(depth)?;
    writeln!(output, "  expanded size: {}", decoded.len())?;
    let nested_label = NestedInspectLabel {
        parent: label,
        component: InspectLabelComponent::Plain(wrapper),
    };
    inspect_region(&nested_label, decoded, output, next_depth)
}

fn inspect_zip(
    label: &dyn fmt::Display,
    region: &Region,
    output: &mut impl Write,
    depth: usize,
) -> Result<()> {
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
        let nested_label = NestedInspectLabel {
            parent: label,
            component: InspectLabelComponent::Escaped(&entry.path),
        };
        inspect_region(
            &nested_label,
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
    let mut class_counts = HashMap::<i32, usize>::new();
    let mut object_bytes = 0_u64;
    for object in &file.objects {
        object_bytes = object_bytes
            .checked_add(object.byte_size)
            .ok_or_else(|| Error::invalid_data("serialized object byte total overflowed"))?;
        increment_class_count(&mut class_counts, object.class_id)?;
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
        for (class_id, count) in sorted_map_entries(class_counts, "class summary")? {
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

struct EscapedText<'a>(&'a str);

impl fmt::Display for EscapedText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for character in self.0.chars().flat_map(char::escape_default) {
            formatter.write_char(character)?;
        }
        Ok(())
    }
}

const fn escape_text(value: &str) -> EscapedText<'_> {
    EscapedText(value)
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
        AudioExportFormat, CliArgumentDisplay, CliArgumentLimits, CliCommand, CliError,
        EscapedOsStr, ExportMode, FbxTemporaryFile, FilenameFormat, ImageFormat,
        InspectLabelComponent, InspectPathBudget, Live2dCommand, Live2dExportState,
        Live2dPublicationLock, LoadOptions, LossyOsStr, MAX_LIVE2D_OUTPUT_MODELS,
        MAX_LIVE2D_TOTAL_OUTPUT_BYTES, MAX_MONO_SCHEMA_DOCUMENTS, MonoSchemaDocumentBudget,
        NestedInspectLabel, SceneObjectKey, charge_live2d_model, collect_cli_arguments_with_limits,
        copy_inspect_path, copy_path_argument, escape_text, fallible_lowercase,
        increment_class_count, increment_string_count, join_inspect_path,
        obj_material_library_name, parse_cli_arguments, parse_export_arguments,
        parse_extract_arguments, parse_live2d_arguments, parse_live2d_package_arguments,
        persist_temporary_hard_link, positional_path_table, publish_fbx_with_textures,
        push_class_filter, push_positional_path, read_bounded_schema_document,
        sanitize_live2d_base_name, sorted_map_entries, split_load_options, write_object_reference,
        write_scene_key,
    };
    use assetstudio_core::loader::AssetLoadLimits;
    use assetstudio_core::mono_schema::{
        MonoBehaviourSchemaDocumentLimits, MonoBehaviourSchemaRegistry,
    };
    use assetstudio_core::scene_textures::{SceneTexture, SceneTextureSet};
    use assetstudio_core::serialized::ObjectReference;
    use std::collections::HashMap;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::{self, Cursor, Write as _};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn command_paths_are_copied_fallibly_and_positional_tables_stay_bounded() {
        let value = OsStr::new("目录/model.assets");
        assert_eq!(
            copy_path_argument(value, "test path").unwrap().as_os_str(),
            value
        );

        let mut paths = positional_path_table("test").unwrap();
        push_positional_path(&mut paths, OsStr::new("input"), "test").unwrap();
        push_positional_path(&mut paths, OsStr::new("output"), "test").unwrap();
        let error = push_positional_path(&mut paths, OsStr::new("extra"), "test").unwrap_err();
        assert!(error.to_string().contains("exactly two"), "{error}");
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn repeated_export_class_filters_grow_fallibly_and_preserve_order() {
        let mut classes = Vec::new();
        for value in ["28", "114", "-187", "28"] {
            push_class_filter(&mut classes, &OsString::from(value)).unwrap();
        }
        assert_eq!(classes, [28, 114, -187, 28]);

        let error = push_class_filter(&mut classes, &OsString::from("not-a-class")).unwrap_err();
        assert!(error.to_string().contains("invalid class ID"), "{error}");
        assert_eq!(classes, [28, 114, -187, 28]);
    }

    fn temporary_test_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "assetstudio-cli-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn closed_fbx_temporary(directory: &Path) -> FbxTemporaryFile {
        let mut temporary = FbxTemporaryFile::create(directory).unwrap();
        temporary.file_mut().write_all(b"complete FBX").unwrap();
        temporary.file_mut().flush().unwrap();
        temporary.file_mut().sync_all().unwrap();
        temporary.close().unwrap();
        temporary
    }

    fn scene_texture(file_name: &str, path_id: i64) -> SceneTexture {
        SceneTexture {
            file_name: file_name.to_owned(),
            object: SceneObjectKey {
                file_index: 0,
                path_id,
            },
            encoded: b"encoded texture".to_vec(),
        }
    }

    #[test]
    fn fbx_waits_for_the_complete_texture_batch_before_publication() {
        let directory = temporary_test_directory("fbx-texture-transaction");
        let destination = directory.join("model.fbx");
        let mut temporary = closed_fbx_temporary(&directory);
        let mut textures = SceneTextureSet::default();
        textures.push_texture(scene_texture("first.png", 1));
        textures.push_texture(scene_texture("../invalid.png", 2));

        let error =
            publish_fbx_with_textures(&mut temporary, &destination, &textures, 0, 0, 12, u64::MAX)
                .unwrap_err();
        assert!(error.to_string().contains("portable"), "{error}");
        assert!(!destination.exists());
        assert!(!directory.join("first.png").exists());
        drop(temporary);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn fbx_collision_rolls_back_the_new_texture_batch() {
        let directory = temporary_test_directory("fbx-late-collision");
        let destination = directory.join("model.fbx");
        fs::write(&destination, b"existing FBX").unwrap();
        let mut temporary = closed_fbx_temporary(&directory);
        let mut textures = SceneTextureSet::default();
        textures.push_texture(scene_texture("body.png", 1));

        let error =
            publish_fbx_with_textures(&mut temporary, &destination, &textures, 0, 0, 12, u64::MAX)
                .unwrap_err();
        assert!(error.to_string().contains("refusing to overwrite"));
        assert_eq!(fs::read(&destination).unwrap(), b"existing FBX");
        assert!(!directory.join("body.png").exists());
        drop(temporary);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_file(&destination).unwrap();
        fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn fbx_total_budget_counts_new_texture_bytes_before_model_commit() {
        let directory = temporary_test_directory("fbx-texture-budget");
        let destination = directory.join("model.fbx");
        let mut temporary = closed_fbx_temporary(&directory);
        let mut textures = SceneTextureSet::default();
        textures.push_texture(scene_texture("body.png", 1));

        let error =
            publish_fbx_with_textures(&mut temporary, &destination, &textures, 0, 0, 12, 26)
                .unwrap_err();
        assert!(
            error.to_string().contains("26 byte total output limit"),
            "{error}"
        );
        assert!(!destination.exists());
        assert!(!directory.join("body.png").exists());
        drop(temporary);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn hard_link_is_the_commit_point_even_when_temp_cleanup_is_deferred() {
        let directory = temporary_test_directory("hard-link-commit");
        let temporary = directory.join("temporary");
        let destination = directory.join("destination");
        fs::write(&temporary, b"published").unwrap();

        let cleaned = persist_temporary_hard_link(&temporary, &destination, "test", |_| {
            Err(io::Error::other("deferred cleanup"))
        })
        .unwrap();
        assert!(!cleaned);
        assert_eq!(fs::read(&temporary).unwrap(), b"published");
        assert_eq!(fs::read(&destination).unwrap(), b"published");

        fs::remove_file(&temporary).unwrap();
        fs::remove_file(&destination).unwrap();
        fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn live2d_lock_cleanup_failure_does_not_reverse_a_committed_package() {
        let directory = temporary_test_directory("live2d-lock-cleanup");
        let mut lock = Live2dPublicationLock::acquire(&directory, "Hero").unwrap();
        let lock_path = lock.path.clone();

        let removed =
            lock.release_after_commit_with(|_| Err(io::Error::other("deferred lock cleanup")));
        assert!(!removed);
        assert!(lock_path.exists());

        drop(lock);
        assert!(!lock_path.exists());
        fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn command_line_arguments_are_bounded_before_collection() {
        let limits = CliArgumentLimits {
            arguments: 2,
            argument_bytes: 4,
            total_bytes: 4,
        };
        assert_eq!(
            collect_cli_arguments_with_limits(arguments(&["ab", "cd"]), limits).unwrap(),
            arguments(&["ab", "cd"])
        );

        let count_error = collect_cli_arguments_with_limits(
            arguments(&["", ""]),
            CliArgumentLimits {
                arguments: 1,
                ..limits
            },
        )
        .unwrap_err();
        assert!(format!("{count_error:?}").contains("more than 1"));

        let item_error = collect_cli_arguments_with_limits(
            arguments(&["five!"]),
            CliArgumentLimits {
                argument_bytes: 4,
                ..limits
            },
        )
        .unwrap_err();
        assert!(format!("{item_error:?}").contains("5 bytes long"));

        let total_error = collect_cli_arguments_with_limits(
            arguments(&["abc", "de"]),
            CliArgumentLimits {
                total_bytes: 4,
                ..limits
            },
        )
        .unwrap_err();
        assert!(format!("{total_error:?}").contains("total 5 bytes"));
    }

    #[test]
    fn command_line_diagnostics_do_not_echo_large_arguments() {
        let oversized = format!("--{}", "é".repeat(33));
        assert_eq!(oversized.len(), 68);
        assert_eq!(
            CliArgumentDisplay(OsStr::new(&oversized)).to_string(),
            "<argument of 68 encoded bytes>"
        );

        let error = parse_cli_arguments(&[OsString::from(&oversized)]).unwrap_err();
        let CliError::Usage(message) = error else {
            panic!("an unknown option must be a usage error");
        };
        assert!(message.contains("<argument of 68 encoded bytes>"));
        assert!(!message.contains(&oversized));
    }

    #[test]
    fn inspect_paths_are_charged_before_retained_allocation() {
        let mut budget = InspectPathBudget::default();
        let single_limit = AssetLoadLimits {
            maximum_path_bytes: 3,
            ..AssetLoadLimits::default()
        };
        let error = copy_inspect_path(Path::new("four"), single_limit, &mut budget).unwrap_err();
        assert!(error.to_string().contains("path is 4 UTF-8 bytes"));
        assert_eq!(budget.bytes, 0);

        let mut budget = InspectPathBudget::default();
        let cumulative_limit = AssetLoadLimits {
            maximum_total_path_bytes: 4,
            ..AssetLoadLimits::default()
        };
        let root = copy_inspect_path(Path::new("root"), cumulative_limit, &mut budget).unwrap();
        let error = join_inspect_path(&root, OsStr::new("child"), cumulative_limit, &mut budget)
            .unwrap_err();
        assert!(error.to_string().contains("filesystem paths total"));
        assert_eq!(budget.bytes, 4);

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            let invalid = PathBuf::from(OsString::from_vec(vec![b'a', 0xff]));
            let mut budget = InspectPathBudget::default();
            let replacement_limit = AssetLoadLimits {
                maximum_path_bytes: 3,
                ..AssetLoadLimits::default()
            };
            let error = copy_inspect_path(&invalid, replacement_limit, &mut budget).unwrap_err();
            assert!(error.to_string().contains("is 4 UTF-8 bytes"), "{error}");
            assert_eq!(budget.bytes, 0);

            let exact_limit = AssetLoadLimits {
                maximum_path_bytes: 4,
                maximum_total_path_bytes: 4,
                ..AssetLoadLimits::default()
            };
            assert_eq!(
                copy_inspect_path(&invalid, exact_limit, &mut budget).unwrap(),
                invalid
            );
            assert_eq!(budget.bytes, 4);
            assert_eq!(
                EscapedOsStr(invalid.as_os_str()).to_string(),
                escape_text(&invalid.to_string_lossy()).to_string()
            );
            assert_eq!(
                LossyOsStr(invalid.as_os_str()).to_string(),
                invalid.to_string_lossy()
            );
            let root_label = LossyOsStr(invalid.as_os_str());
            let compressed_label = NestedInspectLabel {
                parent: &root_label,
                component: InspectLabelComponent::Plain("gzip"),
            };
            assert_eq!(compressed_label.to_string(), "a\u{fffd}::gzip");
            let entry_label = NestedInspectLabel {
                parent: &compressed_label,
                component: InspectLabelComponent::Escaped("line\nname"),
            };
            assert_eq!(entry_label.to_string(), "a\u{fffd}::gzip::line\\nname");

            let invalid_child = OsString::from_vec(vec![0xff]);
            let mut budget = InspectPathBudget::default();
            let cumulative_limit = AssetLoadLimits {
                maximum_total_path_bytes: 11,
                ..AssetLoadLimits::default()
            };
            let root = copy_inspect_path(Path::new("root"), cumulative_limit, &mut budget).unwrap();
            let error = join_inspect_path(
                &root,
                invalid_child.as_os_str(),
                cumulative_limit,
                &mut budget,
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("total 12 UTF-8 bytes"),
                "{error}"
            );
            assert_eq!(budget.bytes, 4);
        }
    }

    #[test]
    fn mono_schema_documents_accumulate_and_leave_the_command_untouched() {
        let (remaining, load) = split_load_options(&arguments(&[
            "--mono-schema",
            "first.json",
            "export",
            "--mono-schema=second.json",
            "input.assets",
            "out",
            "--mono-schema-override",
        ]))
        .unwrap();
        // Both spellings are taken out of the way before the command parses,
        // wherever they appear, and the order they were given is the order
        // that decides which document wins a class.
        assert_eq!(remaining, arguments(&["export", "input.assets", "out"]));
        assert_eq!(
            load.mono_schemas,
            vec![PathBuf::from("first.json"), PathBuf::from("second.json")]
        );
        assert!(load.mono_schema_override);
        assert!(
            load.mono_schema_registry().is_err(),
            "first.json does not exist"
        );
    }

    #[test]
    fn mono_schema_flags_refuse_the_shapes_that_would_do_nothing() {
        // A path is required, and so is something to override with: silently
        // ignoring either would leave the caller believing their schemas were
        // in use.
        assert!(split_load_options(&arguments(&["--mono-schema"])).is_err());
        let (_, load) =
            split_load_options(&arguments(&["--mono-schema-override", "info"])).unwrap();
        assert!(load.mono_schema_registry().is_err());
        assert!(
            split_load_options(&arguments(&["info"]))
                .unwrap()
                .1
                .mono_schema_registry()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mono_schema_documents_are_streamed_and_budgeted_across_repeated_flags() {
        assert_eq!(
            read_bounded_schema_document(Cursor::new(b"1234"), 4).unwrap(),
            b"1234"
        );
        let error = read_bounded_schema_document(Cursor::new(b"12345"), 4).unwrap_err();
        assert!(error.to_string().contains("4 byte total limit"), "{error}");

        let schema = br#"{"version":1,"entries":[{"assembly":"A","class":"C","nodes":[{"level":0,"type":"T","name":"N"}]}]}"#;
        let registry = MonoBehaviourSchemaRegistry::from_json(schema).unwrap();
        let mut budget = MonoSchemaDocumentBudget::default();
        budget.charge(schema.len(), &registry).unwrap();
        let remaining = budget
            .remaining(MonoBehaviourSchemaDocumentLimits {
                maximum_document_bytes: schema.len(),
                maximum_entries: 1,
                maximum_nodes_per_entry: 1,
                maximum_total_nodes: 1,
                maximum_string_bytes: 1,
                maximum_total_string_bytes: 4,
            })
            .unwrap();
        assert_eq!(remaining.maximum_document_bytes, 0);
        assert_eq!(remaining.maximum_entries, 0);
        assert_eq!(remaining.maximum_total_nodes, 0);
        assert_eq!(remaining.maximum_total_string_bytes, 0);

        let load = LoadOptions {
            mono_schemas: vec![PathBuf::from("schema.json"); MAX_MONO_SCHEMA_DOCUMENTS + 1],
            ..LoadOptions::default()
        };
        let error = load.mono_schema_registry().unwrap_err();
        let CliError::Usage(message) = error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(message.contains("documents"), "{message}");
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
            sanitize_live2d_base_name("../unsafe:name.moc3").unwrap(),
            "_unsafe_name"
        );
        assert_eq!(sanitize_live2d_base_name("...").unwrap(), "unnamed");
        assert_eq!(
            fallible_lowercase("Modelİ", "test portable name").unwrap(),
            "modeli\u{307}"
        );

        let mut state = Live2dExportState {
            models_found: MAX_LIVE2D_OUTPUT_MODELS,
            ..Live2dExportState::default()
        };
        assert!(charge_live2d_model(&mut state, 1).is_err());

        let mut state = Live2dExportState::default();
        let next = charge_live2d_model(&mut state, MAX_LIVE2D_TOTAL_OUTPUT_BYTES).unwrap();
        assert_eq!(next, MAX_LIVE2D_TOTAL_OUTPUT_BYTES);
        assert_eq!(state.exported_bytes, 0);
        assert_eq!(
            charge_live2d_model(&mut state, MAX_LIVE2D_TOTAL_OUTPUT_BYTES).unwrap(),
            MAX_LIVE2D_TOTAL_OUTPUT_BYTES
        );
        state.exported_bytes = next;
        assert!(charge_live2d_model(&mut state, 1).is_err());
    }

    #[test]
    fn read_only_summaries_count_without_losing_deterministic_order() {
        let mut classes = HashMap::new();
        increment_class_count(&mut classes, 49).unwrap();
        increment_class_count(&mut classes, 28).unwrap();
        increment_class_count(&mut classes, 49).unwrap();
        assert_eq!(
            sorted_map_entries(classes, "test class summary").unwrap(),
            vec![(28, 1), (49, 2)]
        );

        let mut versions = HashMap::new();
        increment_string_count(&mut versions, "6000.3.0f1", "test Unity version").unwrap();
        increment_string_count(&mut versions, "2022.3.62f1", "test Unity version").unwrap();
        increment_string_count(&mut versions, "6000.3.0f1", "test Unity version").unwrap();
        assert_eq!(
            sorted_map_entries(versions, "test Unity version summary").unwrap(),
            vec![("2022.3.62f1".to_owned(), 1), ("6000.3.0f1".to_owned(), 2),]
        );
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

        for legacy_mode in ["l2d", "live2d"] {
            assert_eq!(
                parse_cli_arguments(&arguments(&[
                    "input.assets",
                    "-m",
                    legacy_mode,
                    "-o",
                    "output",
                ]))
                .unwrap(),
                CliCommand::Live2dPackage(Live2dCommand {
                    input: PathBuf::from("input.assets"),
                    output: PathBuf::from("output"),
                })
            );
        }
        assert!(parse_cli_arguments(&arguments(&["input.assets", "-m", "live2d"])).is_err());
        assert!(
            parse_cli_arguments(&arguments(&[
                "input.assets",
                "-m",
                "live2d",
                "-o",
                "output",
                "--overwrite-existing",
            ]))
            .is_err()
        );
        assert!(
            parse_cli_arguments(&arguments(&[
                "input.assets",
                "-m",
                "l2d",
                "-o",
                "output",
                "--not-restore-extension",
            ]))
            .is_err()
        );
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
