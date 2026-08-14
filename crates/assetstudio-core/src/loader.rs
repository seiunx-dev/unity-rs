use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::bundle::{
    BundleHeader, BundleOpenOptions, BundleParseLimits, OodleDecoder, UnityFsBundle,
};
use crate::compression::{CompressionLimits, ZipContainer, decompress_brotli, decompress_gzip};
use crate::endian::{Endian, EndianReader};
use crate::file_type::{FileType, HEADER_SCAN_LENGTH, detect_file_type};
use crate::legacy_bundle::LegacyBundle;
use crate::monobehaviour::{MONO_SCRIPT_CLASS_ID, MonoBehaviourReadLimits, read_mono_script};
use crate::object_name::{
    ANIMATOR_CLASS_ID, MONO_BEHAVIOUR_CLASS_ID, ObjectNameReadLimits, read_object_name_metadata,
};
use crate::serialized::{
    ASSET_BUNDLE_CLASS_ID, ContainerMetadataReadLimits, ObjectReference, PRELOAD_DATA_CLASS_ID,
    RESOURCE_MANAGER_CLASS_ID, SerializedFile, SerializedOpenOptions,
};
use crate::source::Region;
use crate::unity_version::UnityVersion;
use crate::web_file::{WebFile, WebParseLimits};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetLoadLimits {
    pub maximum_input_files: usize,
    pub maximum_input_directories: usize,
    pub maximum_directory_entries: usize,
    pub maximum_nesting_depth: usize,
    pub maximum_discovered_files: usize,
    pub maximum_expanded_bytes: u64,
    pub maximum_single_entry_bytes: u64,
    pub maximum_container_assignments: usize,
    pub maximum_object_name_assignments: usize,
    pub maximum_total_object_name_bytes: usize,
    /// Maximum combined serialized-file and object entries in the collection-wide `PPtr`
    /// lookup index.
    pub maximum_reference_index_entries: usize,
    pub container_metadata: ContainerMetadataReadLimits,
    pub object_names: ObjectNameReadLimits,
    pub compression: CompressionLimits,
}

impl Default for AssetLoadLimits {
    fn default() -> Self {
        Self {
            maximum_input_files: 1_000_000,
            maximum_input_directories: 1_000_000,
            maximum_directory_entries: 2_000_000,
            maximum_nesting_depth: 32,
            maximum_discovered_files: 1_000_000,
            maximum_expanded_bytes: 4 * 1024 * 1024 * 1024,
            maximum_single_entry_bytes: 512 * 1024 * 1024,
            maximum_container_assignments: 10_000_000,
            maximum_object_name_assignments: 1_000_000,
            maximum_total_object_name_bytes: 256 * 1024 * 1024,
            maximum_reference_index_entries: 10_000_000,
            container_metadata: ContainerMetadataReadLimits::default(),
            object_names: ObjectNameReadLimits::default(),
            compression: CompressionLimits::default(),
        }
    }
}

/// What to do when one discovered input cannot be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadFailurePolicy {
    /// Fail the whole load. Every input must parse.
    #[default]
    Abort,
    /// Record the failure and keep the inputs that did parse.
    ///
    /// A game directory routinely mixes readable assets with encrypted,
    /// truncated or not-yet-supported containers, and the managed
    /// `AssetsManager` logs those and carries on. Aborting instead turns one
    /// unreadable file into an empty collection.
    SkipInput,
}

/// One input that was skipped under [`LoadFailurePolicy::SkipInput`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadDiagnostic {
    pub path: String,
    pub message: String,
}

/// Upper bound on a recorded diagnostic message, in bytes.
///
/// The number of diagnostics is already bounded by the discovered-file limit;
/// this bounds each message so a pathological input cannot grow the collection
/// through its error text.
const MAXIMUM_DIAGNOSTIC_MESSAGE_BYTES: usize = 4096;

/// How one root is opened, shared by every `load*` entry point.
struct RootLoadSettings<'a> {
    limits: &'a AssetLoadLimits,
    unity_version_override: Option<&'a UnityVersion>,
    oodle_decoder: Option<&'a Arc<dyn OodleDecoder>>,
    failure_policy: LoadFailurePolicy,
}

#[derive(Clone, Default)]
pub struct AssetLoadOptions {
    pub limits: AssetLoadLimits,
    pub unity_version_override: Option<UnityVersion>,
    pub oodle_decoder: Option<Arc<dyn OodleDecoder>>,
    pub failure_policy: LoadFailurePolicy,
}

impl fmt::Debug for AssetLoadOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AssetLoadOptions")
            .field("limits", &self.limits)
            .field("unity_version_override", &self.unity_version_override)
            .field(
                "oodle_decoder",
                &self.oodle_decoder.as_ref().map(|_| "<configured>"),
            )
            .field("failure_policy", &self.failure_policy)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct LoadedSerializedFile {
    pub path: String,
    pub file: SerializedFile,
}

#[derive(Debug, Clone)]
pub struct LoadedResource {
    pub path: String,
    pub region: Region,
}

#[derive(Debug, Default, Clone)]
pub struct AssetCollection {
    pub serialized_files: Vec<LoadedSerializedFile>,
    pub resources: Vec<LoadedResource>,
    /// Inputs skipped under [`LoadFailurePolicy::SkipInput`], in discovery
    /// order. Always empty under [`LoadFailurePolicy::Abort`].
    pub diagnostics: Vec<LoadDiagnostic>,
    pub(crate) object_metadata: BTreeMap<(usize, i64), LoadedObjectMetadata>,
    reference_index: Option<AssetReferenceIndex>,
}

#[derive(Debug, Clone, Copy)]
struct ObjectIndexEntry {
    path_id: i64,
    object_index: usize,
}

#[derive(Debug, Default, Clone)]
struct AssetReferenceIndex {
    /// Serialized-file indices sorted by portable name (ASCII-insensitive), then input order.
    files_by_portable_name: Vec<usize>,
    /// Per-file object indices sorted by path ID, then serialized object order.
    objects_by_file: Vec<Vec<ObjectIndexEntry>>,
}

#[derive(Debug, Default, Clone)]
pub struct LoadedObjectMetadata {
    pub name: Option<String>,
    pub container: Option<Arc<str>>,
}

impl AssetCollection {
    /// Builds a collection from already parsed files and resources.
    ///
    /// Container metadata is intentionally empty for this low-level constructor; normal `load*`
    /// entry points populate it after discovering the complete cross-file dependency set.
    #[must_use]
    pub fn from_loaded_parts(
        serialized_files: Vec<LoadedSerializedFile>,
        resources: Vec<LoadedResource>,
    ) -> Self {
        Self {
            serialized_files,
            resources,
            diagnostics: Vec::new(),
            object_metadata: BTreeMap::new(),
            reference_index: None,
        }
    }

    pub fn load(path: impl Into<String>, region: Region) -> Result<Self> {
        Self::load_with_options(path, region, AssetLoadOptions::default())
    }

    pub fn load_with_limits(
        path: impl Into<String>,
        region: Region,
        limits: AssetLoadLimits,
    ) -> Result<Self> {
        Self::load_with_options(
            path,
            region,
            AssetLoadOptions {
                limits,
                ..AssetLoadOptions::default()
            },
        )
    }

    pub fn load_with_options(
        path: impl Into<String>,
        region: Region,
        options: AssetLoadOptions,
    ) -> Result<Self> {
        let AssetLoadOptions {
            limits,
            unity_version_override,
            oodle_decoder,
            failure_policy,
        } = options;
        let mut collection = Self::default();
        let mut budget = AssetLoadBudget::default();
        collection.load_root_with_policy(
            path.into(),
            region,
            &RootLoadSettings {
                limits: &limits,
                unity_version_override: unity_version_override.as_ref(),
                oodle_decoder: oodle_decoder.as_ref(),
                failure_policy,
            },
            &mut budget,
        )?;
        collection.rebuild_object_metadata(&limits)?;
        Ok(collection)
    }

    /// Loads multiple caller-provided roots with one shared traversal budget.
    /// Root order is preserved and object metadata is rebuilt only after all
    /// serialized files and external resources have been discovered.
    pub fn load_regions_with_options(
        inputs: impl IntoIterator<Item = (String, Region)>,
        options: AssetLoadOptions,
    ) -> Result<Self> {
        let AssetLoadOptions {
            limits,
            unity_version_override,
            oodle_decoder,
            failure_policy,
        } = options;
        let settings = RootLoadSettings {
            limits: &limits,
            unity_version_override: unity_version_override.as_ref(),
            oodle_decoder: oodle_decoder.as_ref(),
            failure_policy,
        };
        let mut collection = Self::default();
        let mut budget = AssetLoadBudget::default();
        let mut root_count = 0_usize;
        for (label, region) in inputs {
            root_count = root_count
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("input root count overflowed"))?;
            if root_count > limits.maximum_input_files {
                return Err(Error::invalid_data(format!(
                    "memory input exceeds {} root files",
                    limits.maximum_input_files
                )));
            }
            collection.load_root_with_policy(label, region, &settings, &mut budget)?;
        }
        collection.rebuild_object_metadata(&limits)?;
        Ok(collection)
    }

    /// Loads either one regular file or every regular file below a directory.
    ///
    /// Directory traversal is deterministic, does not follow child symlinks,
    /// and shares the container/count/expansion budgets across every root file.
    pub fn load_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::load_path_with_options(path, AssetLoadOptions::default())
    }

    pub fn load_path_with_limits(path: impl AsRef<Path>, limits: AssetLoadLimits) -> Result<Self> {
        Self::load_path_with_options(
            path,
            AssetLoadOptions {
                limits,
                ..AssetLoadOptions::default()
            },
        )
    }

    pub fn load_path_with_options(
        path: impl AsRef<Path>,
        options: AssetLoadOptions,
    ) -> Result<Self> {
        let AssetLoadOptions {
            limits,
            unity_version_override,
            oodle_decoder,
            failure_policy,
        } = options;
        let path = path.as_ref();
        let metadata = fs::metadata(path)?;
        let mut budget = AssetLoadBudget::default();
        let inputs = if metadata.is_file() {
            prepare_single_file_input(path, &limits, &mut budget)?
        } else if metadata.is_dir() {
            let files = collect_regular_files(path, &limits)?;
            prepare_directory_inputs(files, &limits, &mut budget)?
        } else {
            return Err(Error::invalid_data(format!(
                "input is neither a regular file nor a directory: {}",
                path.display()
            )));
        };

        let settings = RootLoadSettings {
            limits: &limits,
            unity_version_override: unity_version_override.as_ref(),
            oodle_decoder: oodle_decoder.as_ref(),
            failure_policy,
        };
        let mut collection = Self::default();
        for (label, region) in inputs {
            collection.load_root_with_policy(label, region, &settings, &mut budget)?;
        }
        collection.rebuild_object_metadata(&limits)?;
        Ok(collection)
    }

    #[allow(clippy::too_many_lines)]
    fn load_root(
        &mut self,
        path: String,
        region: Region,
        limits: &AssetLoadLimits,
        unity_version_override: Option<&UnityVersion>,
        oodle_decoder: Option<&Arc<dyn OodleDecoder>>,
        budget: &mut AssetLoadBudget,
    ) -> Result<()> {
        let mut pending = VecDeque::from([PendingInput {
            path,
            region,
            depth: 0,
            unity_version_hint: None,
        }]);

        while let Some(input) = pending.pop_front() {
            budget.discovered_files = budget
                .discovered_files
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("discovered file count overflowed"))?;
            if budget.discovered_files > limits.maximum_discovered_files {
                return Err(Error::invalid_data(format!(
                    "asset traversal exceeds {} discovered files",
                    limits.maximum_discovered_files
                )));
            }
            if input.depth > limits.maximum_nesting_depth {
                return Err(Error::invalid_data(format!(
                    "asset traversal exceeds {} container layers",
                    limits.maximum_nesting_depth
                )));
            }

            let detection = detect_region(&input.region)?;
            match detection.file_type {
                FileType::AssetsFile => {
                    let file = SerializedFile::open_with_options(
                        input.region,
                        SerializedOpenOptions {
                            unity_version_override: unity_version_override.cloned(),
                            bundle_version_hint: input.unity_version_hint,
                            ..SerializedOpenOptions::default()
                        },
                    )?;
                    self.serialized_files.push(LoadedSerializedFile {
                        path: input.path,
                        file,
                    });
                }
                FileType::BundleFile => {
                    let length = input
                        .region
                        .len()
                        .checked_sub(detection.data_offset)
                        .ok_or_else(|| Error::invalid_data("bundle offset exceeds its input"))?;
                    let bundle_region = input.region.subregion(detection.data_offset, length)?;
                    let bundle_limits = BundleParseLimits {
                        max_entry_read_size: limits.maximum_single_entry_bytes,
                        ..BundleParseLimits::default()
                    };
                    let common = BundleHeader::read(&mut EndianReader::new(
                        bundle_region.cursor(),
                        Endian::Big,
                    ))?;
                    if common.signature == "UnityArchive" {
                        return Err(Error::unsupported(
                            "UnityArchive bundles are recognized, but their layout is not documented or sample-verified",
                        ));
                    } else if common.signature == "UnityFS"
                        || (matches!(common.signature.as_str(), "UnityWeb" | "UnityRaw")
                            && common.version == 6)
                    {
                        let bundle = UnityFsBundle::open_with_options(
                            &bundle_region,
                            BundleOpenOptions {
                                limits: bundle_limits,
                                oodle_decoder: oodle_decoder.cloned(),
                            },
                        )?;
                        let unity_version_hint =
                            (!bundle.header.common.unity_revision.is_stripped())
                                .then(|| bundle.header.common.unity_revision.clone());
                        for index in 0..bundle.entries.len() {
                            let entry = &bundle.entries[index];
                            let region = Region::from_bytes(bundle.read_entry(index)?);
                            charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                            pending.push_back(PendingInput {
                                path: nested_path(&input.path, &entry.path),
                                region,
                                depth: input.depth + 1,
                                unity_version_hint: unity_version_hint.clone(),
                            });
                        }
                    } else if matches!(common.signature.as_str(), "UnityWeb" | "UnityRaw") {
                        let bundle = LegacyBundle::open_with_limits(&bundle_region, bundle_limits)?;
                        let unity_version_hint =
                            (!bundle.header.common.unity_revision.is_stripped())
                                .then(|| bundle.header.common.unity_revision.clone());
                        for index in 0..bundle.entries.len() {
                            let entry = &bundle.entries[index];
                            let region = Region::from_bytes(bundle.read_entry(index)?);
                            charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                            pending.push_back(PendingInput {
                                path: nested_path(&input.path, &entry.path),
                                region,
                                depth: input.depth + 1,
                                unity_version_hint: unity_version_hint.clone(),
                            });
                        }
                    } else {
                        return Err(Error::unsupported(format!(
                            "bundle signature {:?}",
                            common.signature
                        )));
                    }
                }
                FileType::WebFile => {
                    let web_limits = WebParseLimits {
                        max_entry_read_size: limits.maximum_single_entry_bytes,
                        ..WebParseLimits::default()
                    };
                    let web = WebFile::open_with_limits(input.region, web_limits)?;
                    for index in 0..web.entries.len() {
                        let entry = &web.entries[index];
                        pending.push_back(PendingInput {
                            path: nested_path(&input.path, &entry.path),
                            region: web.entry_region(index)?,
                            depth: input.depth + 1,
                            unity_version_hint: input.unity_version_hint.clone(),
                        });
                    }
                }
                FileType::GzipFile => {
                    let region = decompress_gzip(&input.region, limits.compression)?;
                    charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                    pending.push_back(PendingInput {
                        path: format!("{}::gzip", input.path),
                        region,
                        depth: input.depth + 1,
                        unity_version_hint: input.unity_version_hint,
                    });
                }
                FileType::BrotliFile => {
                    let region = decompress_brotli(&input.region, limits.compression)?;
                    charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                    pending.push_back(PendingInput {
                        path: format!("{}::brotli", input.path),
                        region,
                        depth: input.depth + 1,
                        unity_version_hint: input.unity_version_hint,
                    });
                }
                FileType::ZipFile => {
                    let archive = ZipContainer::open(&input.region, limits.compression)?;
                    for index in 0..archive.entries.len() {
                        let entry = &archive.entries[index];
                        let region = archive.read_entry(index)?;
                        charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                        pending.push_back(PendingInput {
                            path: nested_path(&input.path, &entry.path),
                            region,
                            depth: input.depth + 1,
                            unity_version_hint: input.unity_version_hint.clone(),
                        });
                    }
                }
                FileType::ResourceFile => self.resources.push(LoadedResource {
                    path: input.path,
                    region: input.region,
                }),
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn resource(&self, requested_path: &str) -> Option<&LoadedResource> {
        let normalized_request = normalize_resource_path(requested_path);
        self.resources.iter().find(|resource| {
            let normalized_resource = normalize_resource_path(&resource.path);
            normalized_resource.eq_ignore_ascii_case(&normalized_request)
                || portable_file_name(&normalized_resource)
                    .eq_ignore_ascii_case(portable_file_name(&normalized_request))
        })
    }

    #[must_use]
    pub fn object_metadata(
        &self,
        file_index: usize,
        path_id: i64,
    ) -> Option<&LoadedObjectMetadata> {
        self.object_metadata.get(&(file_index, path_id))
    }

    /// Resolves container/name metadata after constructing a collection from pre-parsed parts.
    pub fn resolve_object_metadata(&mut self, limits: AssetLoadLimits) -> Result<()> {
        self.rebuild_object_metadata(&limits)
    }

    /// Rebuilds the collection-wide `PPtr` lookup index from the currently exposed file tables.
    ///
    /// The low-level [`Self::from_loaded_parts`] constructor intentionally starts without an
    /// index so it can retain its infallible API. Call this method after construction (and after
    /// directly mutating `serialized_files`) to enable indexed reference lookup. A failed rebuild
    /// leaves the collection in the safe linear-lookup mode.
    pub fn rebuild_reference_index(&mut self, limits: AssetLoadLimits) -> Result<()> {
        match AssetReferenceIndex::build(&self.serialized_files, &limits) {
            Ok(index) => {
                self.reference_index = Some(index);
                Ok(())
            }
            Err(error) => {
                self.reference_index = None;
                Err(error)
            }
        }
    }

    /// Loads one root, honouring the configured failure policy.
    fn load_root_with_policy(
        &mut self,
        label: String,
        region: Region,
        settings: &RootLoadSettings<'_>,
        budget: &mut AssetLoadBudget,
    ) -> Result<()> {
        // load_root appends as it discovers, so a failure part way through
        // leaves the collection holding half of an input. Remember where this
        // root started so a skipped one leaves nothing behind.
        let serialized_files = self.serialized_files.len();
        let resources = self.resources.len();
        let result = self.load_root(
            label.clone(),
            region,
            settings.limits,
            settings.unity_version_override,
            settings.oodle_decoder,
            budget,
        );
        if let Err(error) = result {
            if settings.failure_policy == LoadFailurePolicy::Abort {
                return Err(error);
            }
            self.serialized_files.truncate(serialized_files);
            self.resources.truncate(resources);
            self.record_skipped_input(label, &error)?;
        }
        Ok(())
    }

    /// Records one skipped input, truncating its message to a bounded length.
    fn record_skipped_input(&mut self, path: String, error: &Error) -> Result<()> {
        let mut message = error.to_string();
        if message.len() > MAXIMUM_DIAGNOSTIC_MESSAGE_BYTES {
            let mut end = MAXIMUM_DIAGNOSTIC_MESSAGE_BYTES;
            while end > 0 && !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        self.diagnostics.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow load diagnostics: {error}"))
        })?;
        self.diagnostics.push(LoadDiagnostic { path, message });
        Ok(())
    }

    fn rebuild_object_metadata(&mut self, limits: &AssetLoadLimits) -> Result<()> {
        self.rebuild_reference_index(*limits)?;
        let mut metadata = BTreeMap::new();
        let mut pending_names = PendingObjectNames::default();
        let mut assignment_count = 0_usize;
        let mut name_budget = ObjectNameBudget::default();
        for file_index in 0..self.serialized_files.len() {
            let loaded = &self.serialized_files[file_index];
            let mut preload_table = Vec::new();
            for (object_index, object) in loaded.file.objects.iter().enumerate() {
                pending_names.collect(
                    &mut metadata,
                    file_index,
                    &loaded.file,
                    object_index,
                    object.path_id,
                    object.class_id,
                    &mut name_budget,
                    limits,
                )?;
                self.collect_container_object_metadata(
                    file_index,
                    object_index,
                    &mut preload_table,
                    &mut metadata,
                    &mut assignment_count,
                    limits,
                )?;
            }
        }

        pending_names.resolve(self, &mut metadata, &mut name_budget, limits)?;
        self.object_metadata = metadata;
        Ok(())
    }

    fn collect_container_object_metadata(
        &self,
        file_index: usize,
        object_index: usize,
        preload_table: &mut Vec<ObjectReference>,
        metadata: &mut BTreeMap<ObjectMetadataKey, LoadedObjectMetadata>,
        assignment_count: &mut usize,
        limits: &AssetLoadLimits,
    ) -> Result<()> {
        let loaded = &self.serialized_files[file_index];
        let object = &loaded.file.objects[object_index];
        match object.class_id {
            PRELOAD_DATA_CLASS_ID => {
                *preload_table = loaded
                    .file
                    .read_preload_data_metadata(object_index, limits.container_metadata)?
                    .assets;
            }
            ASSET_BUNDLE_CLASS_ID => {
                let bundle = loaded
                    .file
                    .read_asset_bundle_metadata(object_index, limits.container_metadata)?;
                if !bundle.name.is_empty() {
                    metadata
                        .entry((file_index, object.path_id))
                        .or_default()
                        .name = Some(bundle.name);
                }
                if !bundle.is_streamed_scene_asset_bundle {
                    *preload_table = bundle.preload_table;
                }
                for entry in bundle.container {
                    let preload_size = if bundle.is_streamed_scene_asset_bundle {
                        preload_table.len()
                    } else {
                        entry.preload_size
                    };
                    let preload_end =
                        entry
                            .preload_index
                            .checked_add(preload_size)
                            .ok_or_else(|| {
                                Error::invalid_data(
                                    "AssetBundle container preload range overflowed",
                                )
                            })?;
                    let references = preload_table
                        .get(entry.preload_index..preload_end)
                        .ok_or_else(|| {
                            Error::invalid_data(format!(
                                "AssetBundle container {:?} preload range {}..{} exceeds {} entries",
                                entry.key,
                                entry.preload_index,
                                preload_end,
                                preload_table.len()
                            ))
                        })?;
                    let key: Arc<str> = Arc::from(entry.key);
                    for reference in references {
                        charge_container_assignment(assignment_count, limits)?;
                        if let Some(target) = self.resolve_object_reference(file_index, *reference)
                        {
                            metadata.entry(target).or_default().container = Some(Arc::clone(&key));
                        }
                    }
                }
            }
            RESOURCE_MANAGER_CLASS_ID => {
                let manager = loaded
                    .file
                    .read_resource_manager_metadata(object_index, limits.container_metadata)?;
                for entry in manager.container {
                    charge_container_assignment(assignment_count, limits)?;
                    if let Some(target) = self.resolve_object_reference(file_index, entry.asset) {
                        metadata.entry(target).or_default().container = Some(Arc::from(entry.key));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn resolve_object_reference(
        &self,
        source_file_index: usize,
        reference: ObjectReference,
    ) -> Option<(usize, i64)> {
        if reference.is_null() {
            return None;
        }
        let target_file_index = if reference.file_id == 0 {
            source_file_index
        } else {
            let external_index = usize::try_from(reference.file_id.checked_sub(1)?).ok()?;
            let external = self.serialized_files[source_file_index]
                .file
                .externals
                .get(external_index)?;
            self.serialized_file_index_by_portable_name(portable_file_name(&external.path))?
        };
        self.object_index_by_path_id(target_file_index, reference.path_id)
            .is_some()
            .then_some((target_file_index, reference.path_id))
    }

    pub(crate) fn serialized_file_index_by_portable_name(
        &self,
        target_name: &str,
    ) -> Option<usize> {
        if let Some(index) = &self.reference_index {
            let mut index_mismatch = false;
            let start = index.files_by_portable_name.partition_point(|file_index| {
                let Some(file) = self.serialized_files.get(*file_index) else {
                    index_mismatch = true;
                    return false;
                };
                compare_ascii_case_insensitive(portable_file_name(&file.path), target_name)
                    == Ordering::Less
            });
            if !index_mismatch
                && let Some(&file_index) = index.files_by_portable_name.get(start)
                && self.serialized_files.get(file_index).is_some_and(|file| {
                    portable_file_name(&file.path).eq_ignore_ascii_case(target_name)
                })
            {
                return Some(file_index);
            }
        }
        self.serialized_files.iter().position(|candidate| {
            portable_file_name(&candidate.path).eq_ignore_ascii_case(target_name)
        })
    }

    pub(crate) fn object_index_by_path_id(&self, file_index: usize, path_id: i64) -> Option<usize> {
        if let Some(entries) = self
            .reference_index
            .as_ref()
            .and_then(|index| index.objects_by_file.get(file_index))
        {
            let start = entries.partition_point(|entry| entry.path_id < path_id);
            if let Some(entry) = entries.get(start).filter(|entry| entry.path_id == path_id)
                && self
                    .serialized_files
                    .get(file_index)
                    .and_then(|loaded| loaded.file.objects.get(entry.object_index))
                    .is_some_and(|object| object.path_id == path_id)
            {
                return Some(entry.object_index);
            }
        }
        self.serialized_files
            .get(file_index)?
            .file
            .objects
            .iter()
            .position(|object| object.path_id == path_id)
    }
}

impl AssetReferenceIndex {
    fn build(files: &[LoadedSerializedFile], limits: &AssetLoadLimits) -> Result<Self> {
        let mut entry_count = files.len();
        for loaded in files {
            entry_count = entry_count
                .checked_add(loaded.file.objects.len())
                .ok_or_else(|| Error::invalid_data("PPtr lookup index entry count overflowed"))?;
        }
        if entry_count > limits.maximum_reference_index_entries {
            return Err(Error::invalid_data(format!(
                "PPtr lookup index needs {entry_count} entries, exceeding limit {}",
                limits.maximum_reference_index_entries
            )));
        }

        let mut files_by_portable_name = Vec::new();
        files_by_portable_name
            .try_reserve_exact(files.len())
            .map_err(|error| {
                Error::invalid_data(format!(
                    "cannot allocate PPtr portable file-name index: {error}"
                ))
            })?;
        files_by_portable_name.extend(0..files.len());
        files_by_portable_name.sort_unstable_by(|left, right| {
            compare_ascii_case_insensitive(
                portable_file_name(&files[*left].path),
                portable_file_name(&files[*right].path),
            )
            .then_with(|| left.cmp(right))
        });

        let mut objects_by_file = Vec::new();
        objects_by_file
            .try_reserve_exact(files.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate PPtr per-file index: {error}"))
            })?;
        for loaded in files {
            let mut entries = Vec::new();
            entries
                .try_reserve_exact(loaded.file.objects.len())
                .map_err(|error| {
                    Error::invalid_data(format!("cannot allocate PPtr object index: {error}"))
                })?;
            entries.extend(
                loaded
                    .file
                    .objects
                    .iter()
                    .enumerate()
                    .map(|(object_index, object)| ObjectIndexEntry {
                        path_id: object.path_id,
                        object_index,
                    }),
            );
            entries.sort_unstable_by(|left, right| {
                left.path_id
                    .cmp(&right.path_id)
                    .then_with(|| left.object_index.cmp(&right.object_index))
            });
            objects_by_file.push(entries);
        }

        Ok(Self {
            files_by_portable_name,
            objects_by_file,
        })
    }
}

type ObjectMetadataKey = (usize, i64);

#[derive(Default)]
struct PendingObjectNames {
    animator_game_objects: Vec<(ObjectMetadataKey, ObjectReference)>,
    mono_behaviour_scripts: Vec<(ObjectMetadataKey, ObjectReference)>,
    mono_script_classes: BTreeMap<ObjectMetadataKey, String>,
}

#[derive(Default)]
struct ObjectNameBudget {
    assignments: usize,
    string_bytes: usize,
}

impl ObjectNameBudget {
    fn charge(&mut self, string_bytes: usize, limits: &AssetLoadLimits) -> Result<()> {
        self.assignments = self
            .assignments
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("object-name assignment count overflowed"))?;
        if self.assignments > limits.maximum_object_name_assignments {
            return Err(Error::invalid_data(format!(
                "object-name metadata exceeds {} assignments",
                limits.maximum_object_name_assignments
            )));
        }
        self.string_bytes = self
            .string_bytes
            .checked_add(string_bytes)
            .ok_or_else(|| Error::invalid_data("object-name string byte budget overflowed"))?;
        if self.string_bytes > limits.maximum_total_object_name_bytes {
            return Err(Error::invalid_data(format!(
                "object-name metadata strings exceed the {} byte total limit",
                limits.maximum_total_object_name_bytes
            )));
        }
        Ok(())
    }
}

impl PendingObjectNames {
    #[allow(clippy::too_many_arguments)]
    fn collect(
        &mut self,
        metadata: &mut BTreeMap<ObjectMetadataKey, LoadedObjectMetadata>,
        file_index: usize,
        file: &SerializedFile,
        object_index: usize,
        path_id: i64,
        class_id: i32,
        budget: &mut ObjectNameBudget,
        limits: &AssetLoadLimits,
    ) -> Result<()> {
        if let Some(name_metadata) =
            read_object_name_metadata(file, object_index, limits.object_names)?
        {
            if let Some(name) = name_metadata.name {
                budget.charge(name.len(), limits)?;
                metadata.entry((file_index, path_id)).or_default().name = Some(name);
            }
            if class_id == ANIMATOR_CLASS_ID {
                if let Some(game_object) = name_metadata.game_object {
                    budget.charge(0, limits)?;
                    self.animator_game_objects.try_reserve(1).map_err(|error| {
                        Error::invalid_data(format!(
                            "cannot allocate Animator name reference: {error}"
                        ))
                    })?;
                    self.animator_game_objects
                        .push(((file_index, path_id), game_object));
                }
            } else if class_id == MONO_BEHAVIOUR_CLASS_ID {
                if let Some(script) = name_metadata.mono_script {
                    budget.charge(0, limits)?;
                    self.mono_behaviour_scripts
                        .try_reserve(1)
                        .map_err(|error| {
                            Error::invalid_data(format!(
                                "cannot allocate MonoBehaviour name reference: {error}"
                            ))
                        })?;
                    self.mono_behaviour_scripts
                        .push(((file_index, path_id), script));
                }
            }
        }
        if class_id == MONO_SCRIPT_CLASS_ID {
            let mono_limits = MonoBehaviourReadLimits {
                maximum_string_bytes: limits.object_names.maximum_string_bytes,
                maximum_total_string_bytes: limits.container_metadata.maximum_total_string_bytes,
                ..MonoBehaviourReadLimits::default()
            };
            let script = read_mono_script(file, object_index, mono_limits)?;
            budget.charge(script.class_name.len(), limits)?;
            self.mono_script_classes
                .insert((file_index, path_id), script.class_name);
        }
        Ok(())
    }

    fn resolve(
        self,
        collection: &AssetCollection,
        metadata: &mut BTreeMap<ObjectMetadataKey, LoadedObjectMetadata>,
        budget: &mut ObjectNameBudget,
        limits: &AssetLoadLimits,
    ) -> Result<()> {
        for (animator, game_object) in self.animator_game_objects {
            let Some(target) = collection.resolve_object_reference(animator.0, game_object) else {
                continue;
            };
            let Some(name) = metadata
                .get(&target)
                .and_then(|target_metadata| target_metadata.name.clone())
            else {
                continue;
            };
            budget.charge(name.len(), limits)?;
            metadata.entry(animator).or_default().name = Some(name);
        }
        for (behaviour, script) in self.mono_behaviour_scripts {
            if metadata
                .get(&behaviour)
                .and_then(|value| value.name.as_deref())
                .is_some_and(|name| !name.is_empty())
            {
                continue;
            }
            let Some(target) = collection.resolve_object_reference(behaviour.0, script) else {
                continue;
            };
            let Some(class_name) = self.mono_script_classes.get(&target) else {
                continue;
            };
            budget.charge(class_name.len(), limits)?;
            metadata.entry(behaviour).or_default().name = Some(class_name.clone());
        }
        Ok(())
    }
}

fn charge_container_assignment(count: &mut usize, limits: &AssetLoadLimits) -> Result<()> {
    *count = count
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("container assignment count overflowed"))?;
    if *count > limits.maximum_container_assignments {
        return Err(Error::invalid_data(format!(
            "container metadata exceeds {} reference assignments",
            limits.maximum_container_assignments
        )));
    }
    Ok(())
}

#[derive(Debug, Default)]
struct AssetLoadBudget {
    discovered_files: usize,
    expanded_bytes: u64,
}

#[derive(Debug)]
struct PendingInput {
    path: String,
    region: Region,
    depth: usize,
    unity_version_hint: Option<crate::unity_version::UnityVersion>,
}

fn collect_regular_files(root: &Path, limits: &AssetLoadLimits) -> Result<Vec<PathBuf>> {
    if limits.maximum_input_directories == 0 {
        return Err(Error::invalid_data(
            "directory traversal exceeds 0 directories",
        ));
    }
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
            if entry_count > limits.maximum_directory_entries {
                return Err(Error::invalid_data(format!(
                    "directory traversal exceeds {} entries",
                    limits.maximum_directory_entries
                )));
            }
            children.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot allocate directory entries: {error}"))
            })?;
            children.push(child?);
        }
        children.sort_unstable_by_key(fs::DirEntry::file_name);
        for child in children.into_iter().rev() {
            let file_type = child.file_type()?;
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
                pending_directories.push(child.path());
            } else if file_type.is_file() {
                if files.len() >= limits.maximum_input_files {
                    return Err(Error::invalid_data(format!(
                        "directory traversal exceeds {} regular files",
                        limits.maximum_input_files
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

#[derive(Debug)]
struct SplitSegmentPath {
    base: PathBuf,
    index: usize,
}

fn split_base_path(path: &Path) -> Option<PathBuf> {
    path.extension()
        .and_then(|value| value.to_str())?
        .strip_prefix("split")?;
    Some(path.with_extension(""))
}

fn parse_split_segment_path(path: &Path) -> Result<Option<SplitSegmentPath>> {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return Ok(None);
    };
    let Some(suffix) = extension.strip_prefix("split") else {
        return Ok(None);
    };
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::invalid_data(format!(
            "invalid Unity split segment suffix: {}",
            path.display()
        )));
    }
    let index = suffix.parse::<usize>().map_err(|_| {
        Error::invalid_data(format!(
            "Unity split segment index is too large: {}",
            path.display()
        ))
    })?;
    if index.to_string() != suffix {
        return Err(Error::invalid_data(format!(
            "Unity split segment index is not canonical: {}",
            path.display()
        )));
    }
    Ok(Some(SplitSegmentPath {
        base: path.with_extension(""),
        index,
    }))
}

fn prepare_directory_inputs(
    files: Vec<PathBuf>,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
) -> Result<Vec<(String, Region)>> {
    let mut regular_files = BTreeSet::new();
    let mut split_groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for file in files {
        if let Some(base) = split_base_path(&file) {
            split_groups.entry(base).or_default().push(file);
        } else {
            regular_files.insert(file);
        }
    }

    let mut inputs = Vec::new();
    inputs
        .try_reserve_exact(regular_files.len().saturating_add(split_groups.len()))
        .map_err(|error| Error::invalid_data(format!("cannot allocate input table: {error}")))?;
    for file in &regular_files {
        inputs.push((
            file.to_string_lossy().into_owned(),
            Region::from_file(file)?,
        ));
    }
    for (base, segment_paths) in split_groups {
        if regular_files.contains(&base) {
            continue;
        }
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(segment_paths.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate split segment table: {error}"))
            })?;
        for path in segment_paths {
            let segment = parse_split_segment_path(&path)?.ok_or_else(|| {
                Error::invalid_data(format!(
                    "invalid Unity split segment path: {}",
                    path.display()
                ))
            })?;
            segments.push((segment.index, path));
        }
        inputs.push(open_split_group(&base, segments, limits, budget)?);
    }
    inputs.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(inputs)
}

fn prepare_single_file_input(
    path: &Path,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
) -> Result<Vec<(String, Region)>> {
    let Some(base) = split_base_path(path) else {
        return Ok(vec![(
            path.to_string_lossy().into_owned(),
            Region::from_file(path)?,
        )]);
    };
    if base.is_file() {
        return Ok(vec![(
            base.to_string_lossy().into_owned(),
            Region::from_file(&base)?,
        )]);
    }
    let selected_segment = parse_split_segment_path(path)?.ok_or_else(|| {
        Error::invalid_data(format!(
            "invalid Unity split segment path: {}",
            path.display()
        ))
    })?;

    let base_name = selected_segment.base.file_name().ok_or_else(|| {
        Error::invalid_data(format!(
            "Unity split segment has no base file name: {}",
            path.display()
        ))
    })?;
    let parent = selected_segment
        .base
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut segments = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let candidate = entry.path();
        if candidate.with_extension("").file_name() != Some(base_name) {
            continue;
        }
        let Some(segment) = parse_split_segment_path(&candidate)? else {
            continue;
        };
        if segments.len() >= limits.maximum_input_files {
            return Err(Error::invalid_data(format!(
                "Unity split group exceeds {} input files",
                limits.maximum_input_files
            )));
        }
        segments.push((segment.index, candidate));
    }
    Ok(vec![open_split_group(
        &selected_segment.base,
        segments,
        limits,
        budget,
    )?])
}

fn open_split_group(
    base: &Path,
    mut segments: Vec<(usize, PathBuf)>,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
) -> Result<(String, Region)> {
    if segments.is_empty() {
        return Err(Error::invalid_data(format!(
            "Unity split group has no segments: {}",
            base.display()
        )));
    }
    if segments.len() > limits.maximum_input_files {
        return Err(Error::invalid_data(format!(
            "Unity split group {} has {} segments, exceeding limit {}",
            base.display(),
            segments.len(),
            limits.maximum_input_files
        )));
    }
    segments
        .sort_unstable_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (expected_index, (actual_index, path)) in segments.iter().enumerate() {
        if *actual_index < expected_index {
            return Err(Error::invalid_data(format!(
                "duplicate Unity split segment index {actual_index} for {}: {}",
                base.display(),
                path.display()
            )));
        }
        if *actual_index > expected_index {
            return Err(Error::invalid_data(format!(
                "Unity split group {} is missing .split{expected_index}",
                base.display()
            )));
        }
    }

    let remaining_budget = limits
        .maximum_expanded_bytes
        .checked_sub(budget.expanded_bytes)
        .ok_or_else(|| Error::invalid_data("split source byte budget was already exceeded"))?;
    let mut regions = Vec::new();
    regions.try_reserve_exact(segments.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate split source table: {error}"))
    })?;
    let mut group_length = 0_u64;
    for (_, path) in segments {
        let region = Region::from_file(path)?;
        group_length = group_length
            .checked_add(region.len())
            .ok_or_else(|| Error::invalid_data("Unity split group length overflowed"))?;
        if group_length > remaining_budget {
            return Err(Error::invalid_data(format!(
                "Unity split group {} is {group_length} bytes, exceeding remaining budget {remaining_budget}",
                base.display()
            )));
        }
        regions.push(region);
    }
    let region = Region::concatenate(regions, remaining_budget)?;
    budget.expanded_bytes = budget
        .expanded_bytes
        .checked_add(region.len())
        .ok_or_else(|| Error::invalid_data("split source byte budget overflowed"))?;
    Ok((base.to_string_lossy().into_owned(), region))
}

fn detect_region(region: &Region) -> Result<crate::file_type::FileDetection> {
    let scan_limit = u64::try_from(HEADER_SCAN_LENGTH).expect("scan length fits in u64");
    let length =
        usize::try_from(region.len().min(scan_limit)).expect("bounded header length fits in usize");
    let mut header = Vec::new();
    header.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate file detection header: {error}"))
    })?;
    header.resize(length, 0);
    region.read_exact_at(0, &mut header)?;
    Ok(detect_file_type(&header, region.len()))
}

fn charge_expansion(length: u64, limits: &AssetLoadLimits, expanded_bytes: &mut u64) -> Result<()> {
    if length > limits.maximum_single_entry_bytes {
        return Err(Error::invalid_data(format!(
            "expanded entry is {length} bytes, exceeding limit {}",
            limits.maximum_single_entry_bytes
        )));
    }
    *expanded_bytes = expanded_bytes
        .checked_add(length)
        .ok_or_else(|| Error::invalid_data("total expanded byte count overflowed"))?;
    if *expanded_bytes > limits.maximum_expanded_bytes {
        return Err(Error::invalid_data(format!(
            "asset traversal expanded {} bytes, exceeding limit {}",
            *expanded_bytes, limits.maximum_expanded_bytes
        )));
    }
    Ok(())
}

fn nested_path(parent: &str, child: &str) -> String {
    format!("{parent}::{}", child.replace('\\', "/"))
}

fn normalize_resource_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("archive:/")
        .to_owned()
}

fn portable_file_name(path: &str) -> &str {
    let component = path.rsplit_once("::").map_or(path, |(_, name)| name);
    component.rsplit(['/', '\\']).next().unwrap_or(component)
}

fn compare_ascii_case_insensitive(left: &str, right: &str) -> Ordering {
    left.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use crate::serialized::{ContainerMetadataReadLimits, SerializedFile};
    use crate::source::Region;
    use crate::unity_version::UnityVersion;

    use super::{
        AssetCollection, AssetLoadLimits, AssetLoadOptions, LoadFailurePolicy, LoadedSerializedFile,
    };

    #[test]
    fn loads_directory_roots_deterministically_and_limits_input_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("assetstudio-load-path-{unique}"));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("z.resS"), b"z").unwrap();
        fs::write(root.join("nested/a.resS"), b"a").unwrap();

        let collection = AssetCollection::load_path(&root).unwrap();
        assert_eq!(collection.resources.len(), 2);
        assert_eq!(
            Path::new(&collection.resources[0].path).file_name(),
            Some(std::ffi::OsStr::new("a.resS"))
        );
        assert_eq!(
            Path::new(&collection.resources[1].path).file_name(),
            Some(std::ffi::OsStr::new("z.resS"))
        );

        let limits = AssetLoadLimits {
            maximum_input_files: 1,
            ..AssetLoadLimits::default()
        };
        assert!(AssetCollection::load_path_with_limits(&root, limits).is_err());
        let limits = AssetLoadLimits {
            maximum_input_directories: 1,
            ..AssetLoadLimits::default()
        };
        assert!(AssetCollection::load_path_with_limits(&root, limits).is_err());
        let limits = AssetLoadLimits {
            maximum_directory_entries: 2,
            ..AssetLoadLimits::default()
        };
        assert!(AssetCollection::load_path_with_limits(&root, limits).is_err());
        let limits = AssetLoadLimits {
            maximum_input_directories: 0,
            ..AssetLoadLimits::default()
        };
        assert!(AssetCollection::load_path_with_limits(&root, limits).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rebuilds_reference_index_transactionally_with_a_collection_budget() {
        let mut file = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(1, 9, Vec::new()), (1, 10, Vec::new())],
            &[],
        )))
        .unwrap();
        file.objects[1].path_id = 9;
        let mut collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "indexed.assets".to_owned(),
                file,
            }],
            Vec::new(),
        );
        assert!(collection.reference_index.is_none());

        collection
            .rebuild_reference_index(AssetLoadLimits::default())
            .unwrap();
        assert!(collection.reference_index.is_some());
        assert_eq!(collection.object_index_by_path_id(0, 9), Some(0));

        let limits = AssetLoadLimits {
            maximum_reference_index_entries: 2,
            ..AssetLoadLimits::default()
        };
        let error = collection.rebuild_reference_index(limits).unwrap_err();
        assert!(error.to_string().contains("needs 3 entries"));
        assert!(collection.reference_index.is_none());
        // A failed explicit rebuild keeps lookups correct through the linear fallback.
        assert_eq!(collection.object_index_by_path_id(0, 9), Some(0));
    }

    #[test]
    fn resolves_asset_bundle_and_resource_manager_containers_across_files_stably() {
        let mut material = Vec::new();
        push_aligned_string(&mut material, "material");
        align_with_base(
            &mut material,
            asset_bundle_object().len() + resource_manager_object().len(),
            4,
        );
        let source = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[
                (142, 100, asset_bundle_object()),
                (147, 101, resource_manager_object()),
                (21, 1, material),
            ],
            &["archive:/nested/target.assets"],
        )))
        .unwrap();
        assert_eq!(source.externals.len(), 1);
        assert_eq!(source.externals[0].path, "archive:/nested/target.assets");
        let mut text_asset = Vec::new();
        push_aligned_string(&mut text_asset, "text");
        push_i32(&mut text_asset, 0);
        align_with_base(&mut text_asset, 0, 4);
        let mut game_object = Vec::new();
        push_i32(&mut game_object, 0);
        push_i32(&mut game_object, 0);
        push_aligned_string(&mut game_object, "game object");
        let target = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(49, 2, text_asset), (1, 3, game_object)],
            &[],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![
                LoadedSerializedFile {
                    path: "bundle::source.assets".to_owned(),
                    file: source,
                },
                LoadedSerializedFile {
                    path: "bundle::Target.Assets".to_owned(),
                    file: target,
                },
            ],
            Vec::new(),
        );

        collection
            .resolve_object_metadata(AssetLoadLimits::default())
            .unwrap();

        assert_eq!(
            collection
                .object_metadata(0, 100)
                .and_then(|metadata| metadata.name.as_deref()),
            Some("bundle-name")
        );
        // Later ResourceManager entries overwrite earlier AssetBundle keys just like the C#
        // state.Containers dictionary assignment in parsed object order.
        assert_eq!(
            collection
                .object_metadata(0, 1)
                .and_then(|metadata| metadata.container.as_deref()),
            Some("resource/local")
        );
        assert_eq!(
            collection
                .object_metadata(1, 2)
                .and_then(|metadata| metadata.container.as_deref()),
            Some("resource/external")
        );
        assert_eq!(
            collection
                .object_metadata(1, 3)
                .and_then(|metadata| metadata.container.as_deref()),
            Some("bundle/external")
        );
    }

    #[test]
    fn resolves_production_object_names_and_cross_file_display_name_references() {
        let mut material = Vec::new();
        push_aligned_string(&mut material, "Local Material");

        let mut animator = Vec::new();
        push_pptr(&mut animator, 1, 3);

        let mut behaviour = Vec::new();
        push_pptr(&mut behaviour, 0, 0);
        behaviour.push(1);
        align_with_base(&mut behaviour, 0, 4);
        push_pptr(&mut behaviour, 1, 4);
        push_aligned_string(&mut behaviour, "");

        let source = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(21, 1, material), (95, 2, animator), (114, 5, behaviour)],
            &["archive:/nested/target.assets"],
        )))
        .unwrap();

        let mut game_object = Vec::new();
        push_i32(&mut game_object, 0);
        push_i32(&mut game_object, 7);
        push_aligned_string(&mut game_object, "External Hero");

        let mut mono_script = Vec::new();
        push_aligned_string(&mut mono_script, "HeroScript");
        push_i32(&mut mono_script, 0);
        mono_script.extend_from_slice(&[0_u8; 16]);
        push_aligned_string(&mut mono_script, "HeroBehaviour");
        push_aligned_string(&mut mono_script, "Example");
        push_aligned_string(&mut mono_script, "Assembly-CSharp.dll");

        let target = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(1, 3, game_object), (115, 4, mono_script)],
            &[],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![
                LoadedSerializedFile {
                    path: "bundle::source.assets".to_owned(),
                    file: source,
                },
                LoadedSerializedFile {
                    path: "bundle::Target.Assets".to_owned(),
                    file: target,
                },
            ],
            Vec::new(),
        );

        collection
            .resolve_object_metadata(AssetLoadLimits::default())
            .unwrap();

        assert_eq!(
            collection
                .object_metadata(0, 1)
                .and_then(|metadata| metadata.name.as_deref()),
            Some("Local Material")
        );
        assert_eq!(
            collection
                .object_metadata(0, 2)
                .and_then(|metadata| metadata.name.as_deref()),
            Some("External Hero")
        );
        assert_eq!(
            collection
                .object_metadata(0, 5)
                .and_then(|metadata| metadata.name.as_deref()),
            Some("HeroBehaviour")
        );
    }

    #[test]
    fn rejects_corrupt_and_over_budget_supported_names_but_ignores_unknown_layouts() {
        let corrupt = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(21, 1, 64_i32.to_le_bytes().to_vec())],
            &[],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "corrupt.assets".to_owned(),
                file: corrupt,
            }],
            Vec::new(),
        );
        assert!(
            collection
                .resolve_object_metadata(AssetLoadLimits::default())
                .is_err()
        );

        let mut named = Vec::new();
        push_aligned_string(&mut named, "fives");
        let over_budget = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(21, 1, named)],
            &[],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "over-budget.assets".to_owned(),
                file: over_budget,
            }],
            Vec::new(),
        );
        let limits = AssetLoadLimits {
            object_names: crate::object_name::ObjectNameReadLimits {
                maximum_string_bytes: 4,
                ..crate::object_name::ObjectNameReadLimits::default()
            },
            ..AssetLoadLimits::default()
        };
        assert!(collection.resolve_object_metadata(limits).is_err());

        let mut first = Vec::new();
        push_aligned_string(&mut first, "first");
        let mut second = Vec::new();
        push_aligned_string(&mut second, "second");
        let assignment_limited = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(21, 1, first), (21, 2, second)],
            &[],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "assignment-limited.assets".to_owned(),
                file: assignment_limited,
            }],
            Vec::new(),
        );
        let limits = AssetLoadLimits {
            maximum_object_name_assignments: 1,
            ..AssetLoadLimits::default()
        };
        assert!(collection.resolve_object_metadata(limits).is_err());

        let mut bytes_limited = Vec::new();
        push_aligned_string(&mut bytes_limited, "bytes");
        let bytes_limited = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(21, 1, bytes_limited)],
            &[],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "bytes-limited.assets".to_owned(),
                file: bytes_limited,
            }],
            Vec::new(),
        );
        let limits = AssetLoadLimits {
            maximum_total_object_name_bytes: 4,
            ..AssetLoadLimits::default()
        };
        assert!(collection.resolve_object_metadata(limits).is_err());

        let unknown = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(364, 1, 64_i32.to_le_bytes().to_vec())],
            &[],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "unknown.assets".to_owned(),
                file: unknown,
            }],
            Vec::new(),
        );
        collection
            .resolve_object_metadata(AssetLoadLimits::default())
            .unwrap();
        assert!(collection.object_metadata(0, 1).is_none());
    }

    #[test]
    fn rejects_container_counts_ranges_and_assignment_budgets() {
        let source = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(142, 100, asset_bundle_object())],
            &["target.assets"],
        )))
        .unwrap();
        let target = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(49, 2, vec![0]), (1, 3, vec![0])],
            &[],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![
                LoadedSerializedFile {
                    path: "source.assets".to_owned(),
                    file: source,
                },
                LoadedSerializedFile {
                    path: "target.assets".to_owned(),
                    file: target,
                },
            ],
            Vec::new(),
        );
        let assignment_limited = AssetLoadLimits {
            maximum_container_assignments: 1,
            ..AssetLoadLimits::default()
        };
        assert!(
            collection
                .resolve_object_metadata(assignment_limited)
                .is_err()
        );

        let oversized = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(147, 101, 2_i32.to_le_bytes().to_vec())],
            &[],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "oversized.assets".to_owned(),
                file: oversized,
            }],
            Vec::new(),
        );
        let count_limited = AssetLoadLimits {
            container_metadata: ContainerMetadataReadLimits {
                maximum_container_entries: 1,
                ..ContainerMetadataReadLimits::default()
            },
            ..AssetLoadLimits::default()
        };
        assert!(collection.resolve_object_metadata(count_limited).is_err());

        let mut invalid_bundle = asset_bundle_object();
        // m_Name is 4 + 4 bytes, preload count is at byte 8, two PPtrs end at 36, container count
        // is at 36, then the first aligned key and its preload range begin at 40.
        let first_preload_index = 40 + aligned_string_size("bundle/local");
        invalid_bundle[first_preload_index..first_preload_index + 4]
            .copy_from_slice(&99_i32.to_le_bytes());
        let invalid = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(142, 100, invalid_bundle), (21, 1, vec![0])],
            &["target.assets"],
        )))
        .unwrap();
        let mut collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "invalid.assets".to_owned(),
                file: invalid,
            }],
            Vec::new(),
        );
        assert!(
            collection
                .resolve_object_metadata(AssetLoadLimits::default())
                .is_err()
        );
    }

    #[test]
    fn joins_split_assets_and_resources_without_exposing_segments() {
        let root = temporary_directory("split-assets-resources");
        let serialized = empty_v22_serialized_file();
        write_split_parts(&root.join("CAB-split"), &serialized, &[17, 31]);
        write_split_parts(
            &root.join("CAB-split.resS"),
            b"split resource payload",
            &[4, 11],
        );

        let collection = AssetCollection::load_path(&root).unwrap();

        assert_eq!(collection.serialized_files.len(), 1);
        assert_eq!(collection.resources.len(), 1);
        assert_eq!(
            Path::new(&collection.serialized_files[0].path).file_name(),
            Some(std::ffi::OsStr::new("CAB-split"))
        );
        assert_eq!(
            collection
                .resource("archive:/CAB-split/CAB-split.resS")
                .unwrap()
                .region
                .read_to_vec(64)
                .unwrap(),
            b"split resource payload"
        );
        assert!(
            collection
                .resources
                .iter()
                .all(|resource| !resource.path.contains(".split"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn orders_split_segments_numerically_through_split10() {
        let root = temporary_directory("split-numeric-order");
        for index in 0_u8..=10 {
            fs::write(
                root.join(format!("ordered.resS.split{index}")),
                [b'0' + index],
            )
            .unwrap();
        }

        let collection = AssetCollection::load_path(&root).unwrap();
        let expected: Vec<u8> = (0_u8..=10).map(|index| b'0' + index).collect();

        assert_eq!(collection.resources.len(), 1);
        assert_eq!(
            collection
                .resource("ordered.resS")
                .unwrap()
                .region
                .read_to_vec(11)
                .unwrap(),
            expected
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_missing_duplicate_invalid_and_over_budget_split_groups() {
        let missing_root = temporary_directory("split-missing");
        fs::write(missing_root.join("missing.resS.split0"), b"a").unwrap();
        fs::write(missing_root.join("missing.resS.split2"), b"c").unwrap();
        assert!(AssetCollection::load_path(&missing_root).is_err());
        fs::remove_dir_all(missing_root).unwrap();

        let duplicate_root = temporary_directory("split-duplicate");
        fs::write(duplicate_root.join("duplicate.resS.split0"), b"a").unwrap();
        fs::write(duplicate_root.join("duplicate.resS.split00"), b"b").unwrap();
        assert!(AssetCollection::load_path(&duplicate_root).is_err());
        fs::remove_dir_all(duplicate_root).unwrap();

        let invalid_root = temporary_directory("split-invalid");
        fs::write(invalid_root.join("invalid.resS.splitx"), b"a").unwrap();
        assert!(AssetCollection::load_path(&invalid_root).is_err());
        fs::remove_dir_all(invalid_root).unwrap();

        let budget_root = temporary_directory("split-budget");
        fs::write(budget_root.join("budget.resS.split0"), b"abcd").unwrap();
        fs::write(budget_root.join("budget.resS.split1"), b"efgh").unwrap();
        let limits = AssetLoadLimits {
            maximum_expanded_bytes: 7,
            ..AssetLoadLimits::default()
        };
        assert!(AssetCollection::load_path_with_limits(&budget_root, limits).is_err());
        fs::remove_dir_all(budget_root).unwrap();
    }

    #[test]
    fn loads_a_selected_split_segment_as_its_logical_base_path() {
        let root = temporary_directory("split-single-path");
        let payload = b"single split resource";
        write_split_parts(&root.join("single.resS"), payload, &[3, 12]);

        let collection = AssetCollection::load_path(root.join("single.resS.split1")).unwrap();

        assert_eq!(collection.resources.len(), 1);
        assert_eq!(
            Path::new(&collection.resources[0].path).file_name(),
            Some(std::ffi::OsStr::new("single.resS"))
        );
        assert_eq!(
            collection.resources[0].region.read_to_vec(64).unwrap(),
            payload
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preserves_regular_single_file_paths_and_prefers_existing_base_files() {
        let root = temporary_directory("split-existing-base");
        let regular = root.join("regular.resS");
        fs::write(&regular, b"regular payload").unwrap();
        fs::write(root.join("regular.resS.split0"), b"ignored").unwrap();
        fs::write(root.join("regular.resS.split00"), b"also ignored").unwrap();

        let selected = AssetCollection::load_path(&regular).unwrap();
        assert_eq!(selected.resources[0].path, regular.to_string_lossy());
        assert_eq!(
            selected.resources[0].region.read_to_vec(64).unwrap(),
            b"regular payload"
        );

        let directory = AssetCollection::load_path(&root).unwrap();
        assert_eq!(directory.resources.len(), 1);
        assert_eq!(
            directory
                .resource("regular.resS")
                .unwrap()
                .region
                .read_to_vec(64)
                .unwrap(),
            b"regular payload"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_serialized_files_and_resources_through_gzip_and_webdata() {
        let serialized = empty_v22_serialized_file();
        let web = web_file(&[
            ("CAB-test", serialized.as_slice()),
            ("CAB-test.resS", b"resource payload"),
        ]);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&web).unwrap();
        let gzip = encoder.finish().unwrap();

        let collection = AssetCollection::load("input.gz", Region::from_bytes(gzip)).unwrap();
        assert_eq!(collection.serialized_files.len(), 1);
        assert_eq!(collection.resources.len(), 1);
        assert!(collection.reference_index.is_some());
        assert_eq!(collection.serialized_files[0].file.header.version.0, 22);
        assert_eq!(
            collection
                .resource("archive:/CAB-test/CAB-test.resS")
                .unwrap()
                .region
                .read_to_vec(64)
                .unwrap(),
            b"resource payload"
        );
    }

    #[test]
    fn enforces_traversal_depth_and_expansion_budgets() {
        let serialized = empty_v22_serialized_file();
        let web = web_file(&[("CAB-test", serialized.as_slice())]);
        let limits = AssetLoadLimits {
            maximum_nesting_depth: 0,
            ..AssetLoadLimits::default()
        };
        assert!(
            AssetCollection::load_with_limits("input", Region::from_bytes(web), limits).is_err()
        );
    }

    #[test]
    fn skipping_unreadable_inputs_keeps_the_rest_of_a_directory() {
        let directory = temporary_directory("skip-unreadable");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("a-good.assets"), empty_v22_serialized_file()).unwrap();
        // The kind of file a real game directory mixes in: recognized, and
        // explicitly refused because its layout has never been verified.
        let mut archive = Vec::new();
        archive.extend_from_slice(b"UnityArchive\0");
        archive.extend_from_slice(&5_u32.to_be_bytes());
        archive.extend_from_slice(b"5.x.x\0");
        archive.extend_from_slice(b"5.0.0f4\0");
        std::fs::write(directory.join("b-archive.unity3d"), &archive).unwrap();
        std::fs::write(directory.join("c-good.assets"), empty_v22_serialized_file()).unwrap();

        // The default policy still refuses the whole directory, so callers that
        // depend on an all-or-nothing load are unaffected.
        let error = AssetCollection::load_path(&directory)
            .expect_err("the default policy fails the whole load");
        assert!(error.to_string().contains("UnityArchive"));

        let collection = AssetCollection::load_path_with_options(
            &directory,
            AssetLoadOptions {
                failure_policy: LoadFailurePolicy::SkipInput,
                ..AssetLoadOptions::default()
            },
        )
        .unwrap();
        assert_eq!(collection.serialized_files.len(), 2);
        assert!(
            collection
                .serialized_files
                .iter()
                .all(|loaded| loaded.path.contains("good"))
        );
        assert_eq!(collection.diagnostics.len(), 1);
        assert!(collection.diagnostics[0].path.contains("b-archive"));
        assert!(collection.diagnostics[0].message.contains("UnityArchive"));

        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn rejects_unity_archive_before_other_bundle_parsers() {
        let mut archive = Vec::new();
        archive.extend_from_slice(b"UnityArchive\0");
        archive.extend_from_slice(&5_u32.to_be_bytes());
        archive.extend_from_slice(b"5.x.x\0");
        archive.extend_from_slice(b"5.0.0f4\0");

        let error = AssetCollection::load("archive.unity3d", Region::from_bytes(archive))
            .expect_err("UnityArchive has no verified parser");
        match error {
            crate::Error::Unsupported(message) => assert_eq!(
                message,
                "UnityArchive bundles are recognized, but their layout is not documented or sample-verified"
            ),
            other => panic!("expected an explicit unsupported error, got {other:?}"),
        }
    }

    #[test]
    fn discovers_and_charges_entries_in_legacy_unity_raw_bundles() {
        let serialized = empty_v22_serialized_file();
        let raw = legacy_raw_bundle(&[
            ("CAB-legacy", serialized.as_slice()),
            ("CAB-legacy.resS", b"legacy resource"),
        ]);

        let collection =
            AssetCollection::load("legacy.unity3d", Region::from_bytes(raw.clone())).unwrap();
        assert_eq!(collection.serialized_files.len(), 1);
        assert_eq!(collection.resources.len(), 1);
        assert_eq!(
            collection
                .resource("archive:/CAB-legacy/CAB-legacy.resS")
                .unwrap()
                .region
                .read_to_vec(64)
                .unwrap(),
            b"legacy resource"
        );

        let limits = AssetLoadLimits {
            maximum_expanded_bytes: u64::try_from(serialized.len()).unwrap(),
            ..AssetLoadLimits::default()
        };
        assert!(
            AssetCollection::load_with_limits("legacy.unity3d", Region::from_bytes(raw), limits,)
                .is_err()
        );
    }

    #[test]
    fn a_bundle_revision_does_not_replace_a_declared_serialized_file_version() {
        // The managed reader applies the bundle revision only below format 7.
        // These files are v22 and say what they are, so the enclosing bundle's
        // 3.5.0f5 must not reach the version gates that drive every later
        // layout decision.
        let declared = empty_v22_serialized_file_with_version("2019.4.40f1");
        let raw = legacy_raw_bundle(&[("CAB-declared", declared.as_slice())]);

        let loaded =
            AssetCollection::load("declared.unity3d", Region::from_bytes(raw.clone())).unwrap();
        assert_eq!(loaded.serialized_files.len(), 1);
        assert_eq!(
            loaded.serialized_files[0].file.unity_version.full_version,
            "2019.4.40f1"
        );

        // A caller-supplied version still outranks the file's own.
        let explicit = UnityVersion::new(6000, 2, 0);
        let overridden = AssetCollection::load_with_options(
            "declared.unity3d",
            Region::from_bytes(raw),
            AssetLoadOptions {
                unity_version_override: Some(explicit.clone()),
                ..AssetLoadOptions::default()
            },
        )
        .unwrap();
        assert_eq!(overridden.serialized_files[0].file.unity_version, explicit);
    }

    #[test]
    fn global_unity_version_override_wins_over_bundle_hint_for_stripped_files() {
        let stripped = empty_v22_serialized_file_with_version("0.0.0");
        let raw = legacy_raw_bundle(&[
            ("CAB-stripped-a", stripped.as_slice()),
            ("CAB-stripped-b", stripped.as_slice()),
        ]);

        let hinted =
            AssetCollection::load("hinted.unity3d", Region::from_bytes(raw.clone())).unwrap();
        assert_eq!(hinted.serialized_files.len(), 2);
        assert!(hinted.serialized_files.iter().all(|loaded| {
            loaded.file.unity_version_string == "0.0.0"
                && loaded.file.unity_version.full_version == "3.5.0f5"
        }));

        let explicit_version = UnityVersion::new(2022, 3, 62);
        let overridden = AssetCollection::load_with_options(
            "overridden.unity3d",
            Region::from_bytes(raw),
            AssetLoadOptions {
                unity_version_override: Some(explicit_version.clone()),
                ..AssetLoadOptions::default()
            },
        )
        .unwrap();
        assert_eq!(overridden.serialized_files.len(), 2);
        assert!(overridden.serialized_files.iter().all(|loaded| {
            loaded.file.unity_version_string == "0.0.0"
                && loaded.file.unity_version == explicit_version
        }));
    }

    fn empty_v22_serialized_file() -> Vec<u8> {
        empty_v22_serialized_file_with_version("2022.3.62f1")
    }

    fn temporary_directory(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("assetstudio-{label}-{unique}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_split_parts(base: &Path, payload: &[u8], cut_points: &[usize]) {
        let mut start = 0;
        for (index, end) in cut_points
            .iter()
            .copied()
            .chain(std::iter::once(payload.len()))
            .enumerate()
        {
            assert!(end >= start && end <= payload.len());
            let mut path = base.as_os_str().to_os_string();
            path.push(format!(".split{index}"));
            fs::write(path, &payload[start..end]).unwrap();
            start = end;
        }
    }

    fn empty_v22_serialized_file_with_version(unity_version: &str) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        metadata.extend_from_slice(&13_i32.to_le_bytes());
        metadata.push(0);
        for _ in 0..5 {
            metadata.extend_from_slice(&0_i32.to_le_bytes());
        }
        metadata.push(0);

        let metadata_end = 48_u64 + u64::try_from(metadata.len()).unwrap();
        let data_offset = metadata_end.div_ceil(16) * 16;
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes
    }

    fn asset_bundle_object() -> Vec<u8> {
        let mut object = Vec::new();
        push_aligned_string(&mut object, "root");
        push_i32(&mut object, 2);
        push_pptr(&mut object, 0, 1);
        push_pptr(&mut object, 1, 3);
        push_i32(&mut object, 2);
        push_aligned_string(&mut object, "bundle/local");
        push_i32(&mut object, 0);
        push_i32(&mut object, 1);
        push_pptr(&mut object, 0, 1);
        push_aligned_string(&mut object, "bundle/external");
        push_i32(&mut object, 1);
        push_i32(&mut object, 1);
        push_pptr(&mut object, 1, 3);
        push_i32(&mut object, 0);
        push_i32(&mut object, 0);
        push_pptr(&mut object, 0, 0);
        object.extend_from_slice(&0_u32.to_le_bytes());
        push_aligned_string(&mut object, "bundle-name");
        push_i32(&mut object, 0);
        object.push(0);
        object
    }

    fn resource_manager_object() -> Vec<u8> {
        let mut object = Vec::new();
        push_i32(&mut object, 2);
        push_aligned_string(&mut object, "resource/local");
        push_pptr(&mut object, 0, 1);
        push_aligned_string(&mut object, "resource/external");
        push_pptr(&mut object, 1, 2);
        object
    }

    fn synthetic_v22_file(
        unity_version: &str,
        objects: &[(i32, i64, Vec<u8>)],
        externals: &[&str],
    ) -> Vec<u8> {
        let mut class_ids: Vec<i32> = objects.iter().map(|(class_id, _, _)| *class_id).collect();
        class_ids.sort_unstable();
        class_ids.dedup();

        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, i32::try_from(class_ids.len()).unwrap());
        for class_id in &class_ids {
            push_i32(&mut metadata, *class_id);
            metadata.push(0);
            metadata.extend_from_slice(&(-1_i16).to_le_bytes());
            if *class_id == 114 {
                metadata.extend_from_slice(&[0_u8; 16]);
            }
            metadata.extend_from_slice(&[0_u8; 16]);
        }
        push_i32(&mut metadata, i32::try_from(objects.len()).unwrap());
        let mut relative_offset = 0_u64;
        for (class_id, path_id, object) in objects {
            align_with_base(&mut metadata, 48, 4);
            metadata.extend_from_slice(&path_id.to_le_bytes());
            metadata.extend_from_slice(&i64::try_from(relative_offset).unwrap().to_le_bytes());
            metadata.extend_from_slice(&u32::try_from(object.len()).unwrap().to_le_bytes());
            let type_index = class_ids
                .iter()
                .position(|candidate| candidate == class_id)
                .unwrap();
            push_i32(&mut metadata, i32::try_from(type_index).unwrap());
            relative_offset += u64::try_from(object.len()).unwrap();
        }
        push_i32(&mut metadata, 0); // script types
        push_i32(&mut metadata, i32::try_from(externals.len()).unwrap());
        for external in externals {
            metadata.push(0);
            metadata.extend_from_slice(&[0_u8; 16]);
            push_i32(&mut metadata, 0);
            metadata.extend_from_slice(external.as_bytes());
            metadata.push(0);
        }
        push_i32(&mut metadata, 0);
        metadata.push(0);

        let metadata_end = 48_u64 + u64::try_from(metadata.len()).unwrap();
        let data_offset = metadata_end.div_ceil(16) * 16;
        let object_bytes = objects
            .iter()
            .map(|(_, _, object)| object.len())
            .sum::<usize>();
        let file_size = data_offset + u64::try_from(object_bytes).unwrap();
        let mut bytes = vec![0_u8; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        for (_, _, object) in objects {
            bytes.extend_from_slice(object);
        }
        bytes
    }

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        push_i32(output, file_id);
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        while !output.len().is_multiple_of(4) {
            output.push(0);
        }
    }

    fn aligned_string_size(value: &str) -> usize {
        (4 + value.len()).div_ceil(4) * 4
    }

    fn align_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + output.len()).is_multiple_of(alignment) {
            output.push(0);
        }
    }

    fn web_file(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let signature = b"UnityWebData1.0\0";
        let directory_size = entries
            .iter()
            .map(|(path, _)| 12 + path.len())
            .sum::<usize>();
        let header_size = signature.len() + 4 + directory_size;
        let mut next_offset = header_size;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(signature);
        bytes.extend_from_slice(&i32::try_from(header_size).unwrap().to_le_bytes());
        for (path, payload) in entries {
            bytes.extend_from_slice(&i32::try_from(next_offset).unwrap().to_le_bytes());
            bytes.extend_from_slice(&i32::try_from(payload.len()).unwrap().to_le_bytes());
            bytes.extend_from_slice(&i32::try_from(path.len()).unwrap().to_le_bytes());
            bytes.extend_from_slice(path.as_bytes());
            next_offset += payload.len();
        }
        for (_, payload) in entries {
            bytes.extend_from_slice(payload);
        }
        bytes
    }

    fn legacy_raw_bundle(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let directory_size = 4 + entries
            .iter()
            .map(|(path, _)| path.len() + 1 + 8)
            .sum::<usize>();
        let mut next_offset = directory_size;
        let mut content = Vec::new();
        content.extend_from_slice(&i32::try_from(entries.len()).unwrap().to_be_bytes());
        for (path, payload) in entries {
            content.extend_from_slice(path.as_bytes());
            content.push(0);
            content.extend_from_slice(&u32::try_from(next_offset).unwrap().to_be_bytes());
            content.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
            next_offset += payload.len();
        }
        for (_, payload) in entries {
            content.extend_from_slice(payload);
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityRaw\0");
        bytes.extend_from_slice(&3_u32.to_be_bytes());
        bytes.extend_from_slice(b"3.x.x\0");
        bytes.extend_from_slice(b"3.5.0f5\0");
        let minimum_streamed_bytes_position = bytes.len();
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        let header_size_position = bytes.len();
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        let content_size = u32::try_from(content.len()).unwrap();
        bytes.extend_from_slice(&content_size.to_be_bytes());
        bytes.extend_from_slice(&content_size.to_be_bytes());
        let complete_file_size_position = bytes.len();
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(directory_size).unwrap().to_be_bytes());
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        let header_size = u32::try_from(bytes.len()).unwrap();
        bytes[header_size_position..header_size_position + 4]
            .copy_from_slice(&header_size.to_be_bytes());
        bytes.extend_from_slice(&content);
        let complete_size = u32::try_from(bytes.len()).unwrap();
        bytes[minimum_streamed_bytes_position..minimum_streamed_bytes_position + 4]
            .copy_from_slice(&complete_size.to_be_bytes());
        bytes[complete_file_size_position..complete_file_size_position + 4]
            .copy_from_slice(&complete_size.to_be_bytes());
        bytes
    }
}
