use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::bundle::{
    BundleHeader, BundleOpenOptions, BundleParseLimits, OodleDecoder, UnityFsBundle,
};
use crate::compression::{CompressionLimits, ZipContainer, decompress_brotli, decompress_gzip};
use crate::endian::{Endian, EndianReader};
use crate::file_type::{FileType, HEADER_SCAN_LENGTH, detect_file_type};
use crate::filesystem_text::{copy_os_str_with_replacement, lossy_os_str_utf8_length};
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
use crate::sprite::SPRITE_CLASS_ID;
use crate::sprite_atlas::{SPRITE_ATLAS_CLASS_ID, SpriteAtlasReadLimits, read_sprite_atlas};
use crate::texture::{RgbaImage, TextureReadLimits};
use crate::unity_cn::UnityCnKey;
use crate::unity_version::UnityVersion;
use crate::web_file::{WebFile, WebParseLimits};
use crate::{Error, Result};

/// Default maximum UTF-8 byte length of one root label or discovered nested path.
pub const DEFAULT_MAXIMUM_LOAD_PATH_BYTES: usize = 1024 * 1024;
/// Default cumulative UTF-8 bytes charged for root labels and discovered nested paths.
pub const DEFAULT_MAXIMUM_TOTAL_LOAD_PATH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetLoadLimits {
    pub maximum_input_files: usize,
    pub maximum_input_directories: usize,
    pub maximum_directory_entries: usize,
    pub maximum_nesting_depth: usize,
    pub maximum_discovered_files: usize,
    pub maximum_expanded_bytes: u64,
    pub maximum_single_entry_bytes: u64,
    /// Maximum UTF-8 byte length of one caller label or fully qualified nested path.
    pub maximum_path_bytes: usize,
    /// Maximum cumulative UTF-8 bytes of paths discovered during one load.
    ///
    /// Moving the same path through a gzip or Brotli wrapper is not charged again.
    pub maximum_total_path_bytes: usize,
    /// Maximum cumulative UTF-8 bytes retained by skipped-input diagnostic
    /// paths and messages. This is separate from traversal paths because the
    /// returned diagnostics outlive the loader's temporary path table.
    pub maximum_diagnostic_bytes: usize,
    pub maximum_container_assignments: usize,
    pub maximum_object_name_assignments: usize,
    pub maximum_total_object_name_bytes: usize,
    /// Maximum unique objects retaining a name or container assignment.
    pub maximum_object_metadata_entries: usize,
    /// Maximum logical bytes simultaneously retained by object-metadata
    /// values and their temporary build indexes.
    pub maximum_object_metadata_index_bytes: usize,
    /// Maximum combined serialized-file and object entries in the collection-wide `PPtr`
    /// lookup index.
    pub maximum_reference_index_entries: usize,
    /// Maximum combined full-path and portable-name entries in the resource lookup index.
    pub maximum_resource_index_entries: usize,
    /// Maximum retained `SpriteAtlas` records plus resolved `packedSprites`
    /// assignments in the collection-wide Sprite lookup index.
    pub maximum_sprite_atlas_index_entries: usize,
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
            maximum_path_bytes: DEFAULT_MAXIMUM_LOAD_PATH_BYTES,
            maximum_total_path_bytes: DEFAULT_MAXIMUM_TOTAL_LOAD_PATH_BYTES,
            maximum_diagnostic_bytes: 256 * 1024 * 1024,
            maximum_container_assignments: 10_000_000,
            maximum_object_name_assignments: 1_000_000,
            maximum_total_object_name_bytes: 256 * 1024 * 1024,
            maximum_object_metadata_entries: 10_000_000,
            maximum_object_metadata_index_bytes: 1024 * 1024 * 1024,
            maximum_reference_index_entries: 10_000_000,
            maximum_resource_index_entries: 2_000_000,
            maximum_sprite_atlas_index_entries: 10_000_000,
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
    unity_cn_key: Option<UnityCnKey>,
    failure_policy: LoadFailurePolicy,
    strict_unity_versions: bool,
}

#[derive(Clone, Default)]
pub struct AssetLoadOptions {
    pub limits: AssetLoadLimits,
    pub unity_version_override: Option<UnityVersion>,
    pub oodle_decoder: Option<Arc<dyn OodleDecoder>>,
    /// Key for UnityCN-encrypted bundles. Without one they stay refused.
    pub unity_cn_key: Option<UnityCnKey>,
    pub failure_policy: LoadFailurePolicy,
    /// Reject classes whose standard-Unity version is above the verified
    /// ceiling instead of attempting the newest known layout (the default).
    pub strict_unity_versions: bool,
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
            .field("unity_cn_key", &self.unity_cn_key)
            .field("failure_policy", &self.failure_policy)
            .field("strict_unity_versions", &self.strict_unity_versions)
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

/// Owned, deliberately unindexed contents of an [`AssetCollection`].
///
/// These tables may be freely changed before they are passed back to
/// [`AssetCollection::from_parts`]. Derived object metadata and lookup indexes
/// are intentionally absent and must be resolved or rebuilt on the new
/// collection.
#[derive(Debug, Default, Clone)]
pub struct AssetCollectionParts {
    pub serialized_files: Vec<LoadedSerializedFile>,
    pub resources: Vec<LoadedResource>,
    pub diagnostics: Vec<LoadDiagnostic>,
}

/// A loaded, indexed set of serialized files and external resources.
///
/// The two indexed tables are exposed as read-only slices. Low-level callers
/// that need different contents consume the collection through
/// [`Self::into_parts`], change the unindexed parts, and build a new collection
/// with [`Self::from_parts`] before explicitly resolving metadata or rebuilding
/// indexes. This prevents safe external mutation from silently invalidating
/// first-match lookup order without forcing a clone of every loaded region.
///
/// ```compile_fail
/// use unity_rs_core::loader::AssetCollection;
///
/// let mut collection = AssetCollection::default();
/// collection.serialized_files.clear();
/// collection.resources.clear();
/// ```
#[derive(Debug, Default, Clone)]
pub struct AssetCollection {
    pub(crate) serialized_files: Vec<LoadedSerializedFile>,
    pub(crate) resources: Vec<LoadedResource>,
    /// Inputs skipped under [`LoadFailurePolicy::SkipInput`], in discovery
    /// order. Always empty under [`LoadFailurePolicy::Abort`].
    pub diagnostics: Vec<LoadDiagnostic>,
    pub(crate) object_metadata: Vec<ObjectMetadataEntry>,
    reference_index: Option<AssetReferenceIndex>,
    resource_index: Option<AssetResourceIndex>,
    sprite_atlas_index: Option<SpriteAtlasIndex>,
    sprite_texture_cache: SpriteTextureCache,
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
struct AssetResourceIndex {
    /// Resource indices sorted by normalized full path, then discovery order.
    by_full_path: Vec<usize>,
    /// Resource indices sorted by portable file name, then discovery order.
    by_portable_name: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexedSpriteAtlas {
    pub(crate) file_index: usize,
    pub(crate) object_index: usize,
    pub(crate) is_variant: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SpriteAtlasAssignment {
    pub(crate) sprite_file: usize,
    pub(crate) sprite_object: usize,
    pub(crate) atlas: usize,
}

#[derive(Debug, Default, Clone)]
struct SpriteAtlasIndex {
    /// Successfully parsed atlases in stable file/object discovery order.
    atlases: Vec<IndexedSpriteAtlas>,
    /// Packed-Sprite assignments sorted by Sprite identity, then atlas order.
    assignments: Vec<SpriteAtlasAssignment>,
}

/// Maximum decoded texture pages retained by [`SpriteTextureCache`].
const SPRITE_TEXTURE_CACHE_MAX_ENTRIES: usize = 4;
/// Maximum cumulative pixel bytes retained by [`SpriteTextureCache`].
const SPRITE_TEXTURE_CACHE_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Bounded most-recently-used cache of decoded mip-zero Sprite source pages.
///
/// Every Sprite packed into an atlas page resolves and decodes that page's
/// `Texture2D`, so decoding many Sprites over one page repeats its dominant
/// cost per Sprite. This cache remembers the last few successfully decoded
/// pages, keyed by resolved collection file/object identity plus the exact
/// `TextureReadLimits` used, so a batch of Sprites over one page decodes it
/// once. Retention is capped by entry count and cumulative pixel bytes, and
/// each retained image was already individually bounded by its caller's
/// limits. Failed decodes are never cached, and a limits change never reuses
/// a page decoded under different limits.
#[derive(Debug, Default)]
pub(crate) struct SpriteTextureCache {
    inner: Mutex<SpriteTextureCacheInner>,
}

#[derive(Debug, Default)]
struct SpriteTextureCacheInner {
    /// Most-recently-used first; never longer than the entry cap.
    entries: Vec<SpriteTextureCacheEntry>,
    hits: u64,
    misses: u64,
}

#[derive(Debug)]
struct SpriteTextureCacheEntry {
    file_index: usize,
    object_index: usize,
    limits: TextureReadLimits,
    image: Arc<RgbaImage>,
}

impl SpriteTextureCacheEntry {
    fn matches(&self, file_index: usize, object_index: usize, limits: TextureReadLimits) -> bool {
        self.file_index == file_index && self.object_index == object_index && self.limits == limits
    }
}

impl Clone for SpriteTextureCache {
    /// Cached pages are derived state; a cloned collection re-decodes on demand.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl SpriteTextureCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, SpriteTextureCacheInner> {
        // The cache is auxiliary state: a panic while another thread held the
        // lock leaves at worst a smaller cache, never an inconsistent decode.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn get(
        &self,
        file_index: usize,
        object_index: usize,
        limits: TextureReadLimits,
    ) -> Option<Arc<RgbaImage>> {
        let mut inner = self.lock();
        let Some(position) = inner
            .entries
            .iter()
            .position(|entry| entry.matches(file_index, object_index, limits))
        else {
            inner.misses = inner.misses.saturating_add(1);
            return None;
        };
        let entry = inner.entries.remove(position);
        let image = Arc::clone(&entry.image);
        inner.entries.insert(0, entry);
        inner.hits = inner.hits.saturating_add(1);
        Some(image)
    }

    fn insert(
        &self,
        file_index: usize,
        object_index: usize,
        limits: TextureReadLimits,
        image: &Arc<RgbaImage>,
    ) {
        self.insert_with_caps(
            file_index,
            object_index,
            limits,
            image,
            SPRITE_TEXTURE_CACHE_MAX_ENTRIES,
            SPRITE_TEXTURE_CACHE_MAX_TOTAL_BYTES,
        );
    }

    fn insert_with_caps(
        &self,
        file_index: usize,
        object_index: usize,
        limits: TextureReadLimits,
        image: &Arc<RgbaImage>,
        maximum_entries: usize,
        maximum_total_bytes: u64,
    ) {
        let bytes = image.pixels.len() as u64;
        if maximum_entries == 0 || bytes > maximum_total_bytes {
            return;
        }
        let mut inner = self.lock();
        inner
            .entries
            .retain(|entry| !entry.matches(file_index, object_index, limits));
        while inner.entries.len() >= maximum_entries
            || inner.entries.iter().fold(bytes, |total, entry| {
                total.saturating_add(entry.image.pixels.len() as u64)
            }) > maximum_total_bytes
        {
            if inner.entries.pop().is_none() {
                return;
            }
        }
        inner.entries.insert(
            0,
            SpriteTextureCacheEntry {
                file_index,
                object_index,
                limits,
                image: Arc::clone(image),
            },
        );
    }

    fn stats(&self) -> (u64, u64) {
        let inner = self.lock();
        (inner.hits, inner.misses)
    }

    #[cfg(test)]
    fn cached_images(&self) -> Vec<(usize, usize)> {
        self.lock()
            .entries
            .iter()
            .map(|entry| (entry.file_index, entry.object_index))
            .collect()
    }
}

#[derive(Debug, Default, Clone)]
pub struct LoadedObjectMetadata {
    pub name: Option<String>,
    pub container: Option<Arc<str>>,
}

impl AssetCollection {
    /// Returns serialized files in stable discovery order.
    #[must_use]
    pub fn serialized_files(&self) -> &[LoadedSerializedFile] {
        &self.serialized_files
    }

    /// Returns external resources in stable discovery order.
    #[must_use]
    pub fn resources(&self) -> &[LoadedResource] {
        &self.resources
    }

    /// Builds a collection from already parsed files and resources.
    ///
    /// Container metadata is intentionally empty for this low-level constructor; normal `load*`
    /// entry points populate it after discovering the complete cross-file dependency set.
    #[must_use]
    pub fn from_loaded_parts(
        serialized_files: Vec<LoadedSerializedFile>,
        resources: Vec<LoadedResource>,
    ) -> Self {
        Self::from_parts(AssetCollectionParts {
            serialized_files,
            resources,
            diagnostics: Vec::new(),
        })
    }

    /// Builds an unindexed collection from owned parts.
    ///
    /// Object metadata and lookup indexes are derived state and start empty.
    /// Call [`Self::resolve_object_metadata`], [`Self::rebuild_reference_index`],
    /// [`Self::rebuild_resource_index`] or [`Self::rebuild_sprite_atlas_index`]
    /// as appropriate before repeated lookups.
    #[must_use]
    pub fn from_parts(parts: AssetCollectionParts) -> Self {
        let AssetCollectionParts {
            serialized_files,
            resources,
            diagnostics,
        } = parts;
        Self {
            serialized_files,
            resources,
            diagnostics,
            object_metadata: Vec::new(),
            reference_index: None,
            resource_index: None,
            sprite_atlas_index: None,
            sprite_texture_cache: SpriteTextureCache::default(),
        }
    }

    /// Consumes this collection and returns its owned, unindexed contents.
    ///
    /// Derived object metadata and lookup indexes are intentionally discarded;
    /// reconstructing through [`Self::from_parts`] cannot carry a stale index
    /// across table mutation.
    #[must_use]
    pub fn into_parts(self) -> AssetCollectionParts {
        AssetCollectionParts {
            serialized_files: self.serialized_files,
            resources: self.resources,
            diagnostics: self.diagnostics,
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
            unity_cn_key,
            failure_policy,
            strict_unity_versions,
        } = options;
        let mut collection = Self::default();
        let mut budget = AssetLoadBudget::default();
        let path = LoadPath::from_owned(path.into(), &limits, &mut budget)?;
        collection.load_root_with_policy(
            path,
            region,
            &RootLoadSettings {
                limits: &limits,
                unity_version_override: unity_version_override.as_ref(),
                oodle_decoder: oodle_decoder.as_ref(),
                unity_cn_key,
                failure_policy,
                strict_unity_versions,
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
            unity_cn_key,
            failure_policy,
            strict_unity_versions,
        } = options;
        let settings = RootLoadSettings {
            limits: &limits,
            unity_version_override: unity_version_override.as_ref(),
            oodle_decoder: oodle_decoder.as_ref(),
            unity_cn_key,
            failure_policy,
            strict_unity_versions,
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
            let label = LoadPath::from_owned(label, &limits, &mut budget)?;
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
            unity_cn_key,
            failure_policy,
            strict_unity_versions,
        } = options;
        let path = path.as_ref();
        let metadata = fs::metadata(path)?;
        let mut budget = AssetLoadBudget::default();
        let inputs = if metadata.is_file() {
            prepare_single_file_input(path, &limits, &mut budget)?
        } else if metadata.is_dir() {
            let files = collect_regular_files(path, &limits, &mut budget)?;
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
            unity_cn_key,
            failure_policy,
            strict_unity_versions,
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
        path: LoadPath,
        region: Region,
        settings: &RootLoadSettings<'_>,
        budget: &mut AssetLoadBudget,
    ) -> Result<()> {
        let RootLoadSettings {
            limits,
            unity_version_override,
            oodle_decoder,
            unity_cn_key,
            strict_unity_versions,
            ..
        } = *settings;
        let mut pending = VecDeque::new();
        charge_pending_inputs(&mut pending, 1, budget, limits)?;
        pending.push_back(PendingInput {
            path,
            region,
            depth: 0,
            unity_version_hint: None,
        });

        while let Some(input) = pending.pop_front() {
            if input.depth > limits.maximum_nesting_depth {
                return Err(Error::invalid_data(format!(
                    "asset traversal exceeds {} container layers",
                    limits.maximum_nesting_depth
                )));
            }

            let detection = detect_region(&input.region)?;
            match detection.file_type {
                FileType::AssetsFile => {
                    self.serialized_files.try_reserve(1).map_err(|error| {
                        Error::invalid_data(format!(
                            "cannot grow loaded serialized-file table: {error}"
                        ))
                    })?;
                    let file = SerializedFile::open_with_options(
                        input.region,
                        SerializedOpenOptions {
                            unity_version_override: unity_version_override.cloned(),
                            bundle_version_hint: input.unity_version_hint,
                            strict_unity_versions,
                            ..SerializedOpenOptions::default()
                        },
                    )?;
                    self.serialized_files.push(LoadedSerializedFile {
                        path: input.path.into_string(),
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
                    let bundle_defaults = BundleParseLimits::default();
                    let bundle_limits = BundleParseLimits {
                        max_path_length: limits
                            .maximum_path_bytes
                            .min(bundle_defaults.max_path_length),
                        max_entry_read_size: limits.maximum_single_entry_bytes,
                        ..bundle_defaults
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
                                unity_cn_key,
                            },
                        )?;
                        let unity_version_hint =
                            (!bundle.header.common.unity_revision.is_stripped())
                                .then(|| bundle.header.common.unity_revision.clone());
                        charge_pending_inputs(&mut pending, bundle.entries.len(), budget, limits)?;
                        for index in 0..bundle.entries.len() {
                            let entry = &bundle.entries[index];
                            let region = Region::from_bytes(bundle.read_entry(index)?);
                            charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                            pending.push_back(PendingInput {
                                path: nested_path(&input.path, &entry.path, limits, budget)?,
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
                        charge_pending_inputs(&mut pending, bundle.entries.len(), budget, limits)?;
                        for index in 0..bundle.entries.len() {
                            let entry = &bundle.entries[index];
                            let region = Region::from_bytes(bundle.read_entry(index)?);
                            charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                            pending.push_back(PendingInput {
                                path: nested_path(&input.path, &entry.path, limits, budget)?,
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
                    let web_defaults = WebParseLimits::default();
                    let web_limits = WebParseLimits {
                        max_path_length: limits
                            .maximum_path_bytes
                            .min(web_defaults.max_path_length),
                        max_entry_read_size: limits.maximum_single_entry_bytes,
                        ..web_defaults
                    };
                    let web = WebFile::open_with_limits(input.region, web_limits)?;
                    charge_pending_inputs(&mut pending, web.entries.len(), budget, limits)?;
                    for index in 0..web.entries.len() {
                        let entry = &web.entries[index];
                        pending.push_back(PendingInput {
                            path: nested_path(&input.path, &entry.path, limits, budget)?,
                            region: web.entry_region(index)?,
                            depth: input.depth + 1,
                            unity_version_hint: input.unity_version_hint.clone(),
                        });
                    }
                }
                // gzip and Brotli wrap exactly one stream, so the decompressed
                // input keeps the container's own path. Appending a `::gzip`
                // segment would make the portable name -- which is what
                // cross-file external references are matched against -- the
                // literal string "gzip" instead of the file's name, so nothing
                // could ever reference it. The managed reader keeps the name
                // too.
                FileType::GzipFile => {
                    let region = decompress_gzip(&input.region, limits.compression)?;
                    charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                    charge_pending_inputs(&mut pending, 1, budget, limits)?;
                    pending.push_back(PendingInput {
                        path: input.path,
                        region,
                        depth: input.depth + 1,
                        unity_version_hint: input.unity_version_hint,
                    });
                }
                FileType::BrotliFile => {
                    let region = decompress_brotli(&input.region, limits.compression)?;
                    charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                    charge_pending_inputs(&mut pending, 1, budget, limits)?;
                    pending.push_back(PendingInput {
                        path: input.path,
                        region,
                        depth: input.depth + 1,
                        unity_version_hint: input.unity_version_hint,
                    });
                }
                FileType::ZipFile => {
                    let compression = CompressionLimits {
                        maximum_zip_path_bytes: limits
                            .maximum_path_bytes
                            .min(limits.compression.maximum_zip_path_bytes),
                        ..limits.compression
                    };
                    let archive = ZipContainer::open(&input.region, compression)?;
                    charge_pending_inputs(&mut pending, archive.entries.len(), budget, limits)?;
                    for index in 0..archive.entries.len() {
                        let entry = &archive.entries[index];
                        let region = archive.read_entry(index)?;
                        charge_expansion(region.len(), limits, &mut budget.expanded_bytes)?;
                        pending.push_back(PendingInput {
                            path: nested_path(&input.path, &entry.path, limits, budget)?,
                            region,
                            depth: input.depth + 1,
                            unity_version_hint: input.unity_version_hint.clone(),
                        });
                    }
                }
                FileType::ResourceFile => {
                    self.resources.try_reserve(1).map_err(|error| {
                        Error::invalid_data(format!("cannot grow loaded resource table: {error}"))
                    })?;
                    self.resources.push(LoadedResource {
                        path: input.path.into_string(),
                        region: input.region,
                    });
                }
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn resource(&self, requested_path: &str) -> Option<&LoadedResource> {
        self.resource_index_by_path(requested_path)
            .and_then(|index| self.resources.get(index))
    }

    #[must_use]
    pub fn object_metadata(
        &self,
        file_index: usize,
        path_id: i64,
    ) -> Option<&LoadedObjectMetadata> {
        object_metadata_position(&self.object_metadata, (file_index, path_id))
            .and_then(|index| self.object_metadata.get(index))
            .map(|entry| &entry.1)
    }

    /// Resolves container/name metadata after constructing a collection from pre-parsed parts.
    pub fn resolve_object_metadata(&mut self, limits: AssetLoadLimits) -> Result<()> {
        self.rebuild_object_metadata(&limits)
    }

    /// Rebuilds the collection-wide `PPtr` lookup index from the currently exposed file tables.
    ///
    /// The low-level [`Self::from_loaded_parts`] constructor intentionally starts without an
    /// index so it can retain its infallible API. Call this method after construction to enable
    /// indexed reference lookup. A failed rebuild leaves the collection in the safe linear-lookup
    /// mode.
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

    /// Rebuilds the resource full-path and portable-name lookup index.
    ///
    /// [`Self::from_loaded_parts`] intentionally starts without one. A failed rebuild leaves
    /// resource lookup in the allocation-free linear fallback mode.
    pub fn rebuild_resource_index(&mut self, limits: AssetLoadLimits) -> Result<()> {
        match AssetResourceIndex::build(&self.resources, &limits) {
            Ok(index) => {
                self.resource_index = Some(index);
                Ok(())
            }
            Err(error) => {
                self.resource_index = None;
                Err(error)
            }
        }
    }

    /// Rebuilds the collection-wide Sprite-to-SpriteAtlas assignment index.
    ///
    /// Invalid or unsupported atlas objects are omitted, matching the managed
    /// construction pass. A failed index allocation or collection budget
    /// leaves Sprite decoding on its safe linear compatibility fallback.
    pub fn rebuild_sprite_atlas_index(&mut self, limits: AssetLoadLimits) -> Result<()> {
        match SpriteAtlasIndex::build(self, &limits) {
            Ok(index) => {
                self.sprite_atlas_index = Some(index);
                Ok(())
            }
            Err(error) => {
                self.sprite_atlas_index = None;
                Err(error)
            }
        }
    }

    /// Loads one root, honouring the configured failure policy.
    fn load_root_with_policy(
        &mut self,
        label: LoadPath,
        region: Region,
        settings: &RootLoadSettings<'_>,
        budget: &mut AssetLoadBudget,
    ) -> Result<()> {
        // load_root appends as it discovers, so a failure part way through
        // leaves the collection holding half of an input. Remember where this
        // root started so a skipped one leaves nothing behind.
        let serialized_files = self.serialized_files.len();
        let resources = self.resources.len();
        let diagnostic_path = if settings.failure_policy == LoadFailurePolicy::SkipInput {
            Some(label.try_copy_string()?)
        } else {
            None
        };
        let result = self.load_root(label, region, settings, budget);
        if let Err(error) = result {
            if settings.failure_policy == LoadFailurePolicy::Abort {
                return Err(error);
            }
            self.serialized_files.truncate(serialized_files);
            self.resources.truncate(resources);
            self.record_skipped_input(
                diagnostic_path.expect("skip policy prepared a diagnostic path"),
                &error,
                settings.limits,
                budget,
            )?;
        }
        Ok(())
    }

    /// Records one skipped input, truncating its message to a bounded length.
    fn record_skipped_input(
        &mut self,
        path: String,
        error: &Error,
        limits: &AssetLoadLimits,
        budget: &mut AssetLoadBudget,
    ) -> Result<()> {
        let message_length = load_diagnostic_message_length(error)?;
        let diagnostic_bytes =
            budget.checked_diagnostic_bytes(path.len(), message_length, limits)?;
        let message = format_load_diagnostic(error, message_length)?;
        self.diagnostics.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow load diagnostics: {error}"))
        })?;
        budget.diagnostic_bytes = diagnostic_bytes;
        self.diagnostics.push(LoadDiagnostic { path, message });
        Ok(())
    }

    fn rebuild_object_metadata(&mut self, limits: &AssetLoadLimits) -> Result<()> {
        self.rebuild_reference_index(*limits)?;
        self.rebuild_resource_index(*limits)?;
        self.rebuild_sprite_atlas_index(*limits)?;
        let mut metadata = ObjectMetadataBuilder::default();
        let mut pending_names = PendingObjectNames::default();
        let mut assignment_count = 0_usize;
        let mut name_budget = ObjectNameBudget::default();
        let mut index_budget = ObjectMetadataIndexBudget::default();
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
                    &mut index_budget,
                    limits,
                )?;
                let mut container_state = ContainerObjectMetadataBuildState {
                    metadata: &mut metadata,
                    assignment_count: &mut assignment_count,
                    index_budget: &mut index_budget,
                };
                self.collect_container_object_metadata(
                    file_index,
                    object_index,
                    &mut preload_table,
                    &mut container_state,
                    limits,
                )?;
            }
        }

        pending_names.resolve(
            self,
            &mut metadata,
            &mut name_budget,
            &mut index_budget,
            limits,
        )?;
        self.object_metadata = metadata.finish();
        Ok(())
    }

    fn collect_container_object_metadata(
        &self,
        file_index: usize,
        object_index: usize,
        preload_table: &mut Vec<ObjectReference>,
        state: &mut ContainerObjectMetadataBuildState<'_>,
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
                    state
                        .metadata
                        .entry_mut((file_index, object.path_id), state.index_budget, limits)?
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
                        charge_container_assignment(state.assignment_count, limits)?;
                        if let Some(target) = self.resolve_object_reference(file_index, *reference)
                        {
                            state
                                .metadata
                                .entry_mut(target, state.index_budget, limits)?
                                .container = Some(Arc::clone(&key));
                        }
                    }
                }
            }
            RESOURCE_MANAGER_CLASS_ID => {
                let manager = loaded
                    .file
                    .read_resource_manager_metadata(object_index, limits.container_metadata)?;
                for entry in manager.container {
                    charge_container_assignment(state.assignment_count, limits)?;
                    if let Some(target) = self.resolve_object_reference(file_index, entry.asset) {
                        state
                            .metadata
                            .entry_mut(target, state.index_budget, limits)?
                            .container = Some(Arc::from(entry.key));
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
        let (target_file_index, _) = self.resolve_object_location(source_file_index, reference)?;
        Some((target_file_index, reference.path_id))
    }

    fn resolve_object_location(
        &self,
        source_file_index: usize,
        reference: ObjectReference,
    ) -> Option<(usize, usize)> {
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
        let object_index = self.object_index_by_path_id(target_file_index, reference.path_id)?;
        Some((target_file_index, object_index))
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
        self.object_index_by_path_id_with_probe(file_index, path_id, || {})
    }

    pub(crate) fn object_index_by_path_id_with_probe(
        &self,
        file_index: usize,
        path_id: i64,
        mut probe: impl FnMut(),
    ) -> Option<usize> {
        if let Some(entries) = self
            .reference_index
            .as_ref()
            .and_then(|index| index.objects_by_file.get(file_index))
        {
            let start = entries.partition_point(|entry| {
                probe();
                entry.path_id < path_id
            });
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
            .position(|object| {
                probe();
                object.path_id == path_id
            })
    }

    pub(crate) fn resource_index_by_path(&self, requested_path: &str) -> Option<usize> {
        if let Some(index) = &self.resource_index
            && let Some(resource_index) = index.find(&self.resources, requested_path)
            && self
                .resources
                .get(resource_index)
                .is_some_and(|resource| resource_path_matches(&resource.path, requested_path))
        {
            return Some(resource_index);
        }
        self.resources
            .iter()
            .position(|resource| resource_path_matches(&resource.path, requested_path))
    }

    pub(crate) fn indexed_sprite_atlas(
        &self,
        file_index: usize,
        object_index: usize,
    ) -> Option<IndexedSpriteAtlas> {
        self.sprite_atlas_index
            .as_ref()?
            .atlas(file_index, object_index)
    }

    pub(crate) fn indexed_sprite_atlas_by_index(
        &self,
        atlas_index: usize,
    ) -> Option<IndexedSpriteAtlas> {
        self.sprite_atlas_index
            .as_ref()?
            .atlases
            .get(atlas_index)
            .copied()
    }

    pub(crate) fn indexed_sprite_atlas_assignments(
        &self,
        sprite_file_index: usize,
        sprite_object_index: usize,
    ) -> Option<&[SpriteAtlasAssignment]> {
        self.sprite_atlas_index
            .as_ref()
            .map(|index| index.assignments(sprite_file_index, sprite_object_index, || {}))
    }

    /// Returns a previously cached decoded mip-zero Sprite source page.
    pub(crate) fn cached_sprite_texture(
        &self,
        file_index: usize,
        object_index: usize,
        limits: TextureReadLimits,
    ) -> Option<Arc<RgbaImage>> {
        self.sprite_texture_cache
            .get(file_index, object_index, limits)
    }

    /// Retains one successfully decoded mip-zero Sprite source page in the
    /// bounded most-recently-used cache.
    pub(crate) fn cache_sprite_texture(
        &self,
        file_index: usize,
        object_index: usize,
        limits: TextureReadLimits,
        image: &Arc<RgbaImage>,
    ) {
        self.sprite_texture_cache
            .insert(file_index, object_index, limits, image);
    }

    /// Returns the hit and miss counters of the bounded decoded
    /// sprite-page cache, in that order.
    pub(crate) fn sprite_texture_cache_stats(&self) -> (u64, u64) {
        self.sprite_texture_cache.stats()
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

impl SpriteAtlasIndex {
    fn build(collection: &AssetCollection, limits: &AssetLoadLimits) -> Result<Self> {
        let atlas_object_count = Self::object_count(collection)?;
        let mut atlases = Vec::new();
        atlases
            .try_reserve_exact(atlas_object_count.min(limits.maximum_sprite_atlas_index_entries))
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate SpriteAtlas object index: {error}"))
            })?;
        let mut assignments = Vec::new();
        let mut retained_entries = 0_usize;

        for (file_index, loaded) in collection.serialized_files.iter().enumerate() {
            for (object_index, object) in loaded.file.objects.iter().enumerate() {
                if object.class_id != SPRITE_ATLAS_CLASS_ID {
                    continue;
                }
                let Ok(atlas) =
                    read_sprite_atlas(&loaded.file, object_index, SpriteAtlasReadLimits::default())
                else {
                    // Managed construction failures do not participate in
                    // AssetsManager.ProcessAssets either.
                    continue;
                };

                let resolved_count = atlas
                    .packed_sprites
                    .iter()
                    .filter(|reference| {
                        collection
                            .resolve_object_location(file_index, **reference)
                            .and_then(|(target_file_index, target_object_index)| {
                                collection
                                    .serialized_files
                                    .get(target_file_index)?
                                    .file
                                    .objects
                                    .get(target_object_index)
                                    .filter(|target| target.class_id == SPRITE_CLASS_ID)
                                    .map(|_| ())
                            })
                            .is_some()
                    })
                    .count();
                let next_entries = retained_entries
                    .checked_add(1)
                    .and_then(|count| count.checked_add(resolved_count))
                    .ok_or_else(|| {
                        Error::invalid_data("SpriteAtlas lookup index entry count overflowed")
                    })?;
                if next_entries > limits.maximum_sprite_atlas_index_entries {
                    return Err(Error::invalid_data(format!(
                        "SpriteAtlas lookup index needs {next_entries} entries, exceeding limit {}",
                        limits.maximum_sprite_atlas_index_entries
                    )));
                }
                atlases.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow SpriteAtlas object index: {error}"))
                })?;
                assignments.try_reserve(resolved_count).map_err(|error| {
                    Error::invalid_data(format!(
                        "cannot grow SpriteAtlas assignment index: {error}"
                    ))
                })?;

                let atlas_index = atlases.len();
                atlases.push(IndexedSpriteAtlas {
                    file_index,
                    object_index,
                    is_variant: atlas.is_variant,
                });
                for reference in atlas.packed_sprites {
                    let Some((sprite_file_index, sprite_object_index)) =
                        collection.resolve_object_location(file_index, reference)
                    else {
                        continue;
                    };
                    let Some(sprite_object) = collection
                        .serialized_files
                        .get(sprite_file_index)
                        .and_then(|target| target.file.objects.get(sprite_object_index))
                    else {
                        continue;
                    };
                    if sprite_object.class_id != SPRITE_CLASS_ID {
                        continue;
                    }
                    assignments.push(SpriteAtlasAssignment {
                        sprite_file: sprite_file_index,
                        sprite_object: sprite_object_index,
                        atlas: atlas_index,
                    });
                }
                retained_entries = next_entries;
            }
        }

        assignments
            .sort_unstable_by_key(|entry| (entry.sprite_file, entry.sprite_object, entry.atlas));
        Ok(Self {
            atlases,
            assignments,
        })
    }

    fn object_count(collection: &AssetCollection) -> Result<usize> {
        collection
            .serialized_files
            .iter()
            .try_fold(0_usize, |count, loaded| {
                loaded
                    .file
                    .objects
                    .iter()
                    .filter(|object| object.class_id == SPRITE_ATLAS_CLASS_ID)
                    .try_fold(count, |count, _| {
                        count.checked_add(1).ok_or_else(|| {
                            Error::invalid_data("SpriteAtlas object count overflowed")
                        })
                    })
            })
    }

    fn atlas(&self, file_index: usize, object_index: usize) -> Option<IndexedSpriteAtlas> {
        let target = (file_index, object_index);
        let index = self
            .atlases
            .partition_point(|atlas| (atlas.file_index, atlas.object_index) < target);
        self.atlases
            .get(index)
            .copied()
            .filter(|atlas| (atlas.file_index, atlas.object_index) == target)
    }

    fn assignments(
        &self,
        sprite_file_index: usize,
        sprite_object_index: usize,
        mut probe: impl FnMut(),
    ) -> &[SpriteAtlasAssignment] {
        let target = (sprite_file_index, sprite_object_index);
        let start = self.assignments.partition_point(|entry| {
            probe();
            (entry.sprite_file, entry.sprite_object) < target
        });
        let width = self.assignments[start..].partition_point(|entry| {
            probe();
            (entry.sprite_file, entry.sprite_object) == target
        });
        &self.assignments[start..start + width]
    }
}

impl AssetResourceIndex {
    fn build(resources: &[LoadedResource], limits: &AssetLoadLimits) -> Result<Self> {
        let entry_count = resources
            .len()
            .checked_mul(2)
            .ok_or_else(|| Error::invalid_data("resource lookup index entry count overflowed"))?;
        if entry_count > limits.maximum_resource_index_entries {
            return Err(Error::invalid_data(format!(
                "resource lookup index needs {entry_count} entries, exceeding limit {}",
                limits.maximum_resource_index_entries
            )));
        }

        let mut by_full_path = Vec::new();
        by_full_path
            .try_reserve_exact(resources.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate resource full-path index: {error}"))
            })?;
        by_full_path.extend(0..resources.len());
        by_full_path.sort_unstable_by(|left, right| {
            compare_normalized_resource_paths(&resources[*left].path, &resources[*right].path)
                .then_with(|| left.cmp(right))
        });

        let mut by_portable_name = Vec::new();
        by_portable_name
            .try_reserve_exact(resources.len())
            .map_err(|error| {
                Error::invalid_data(format!(
                    "cannot allocate resource portable-name index: {error}"
                ))
            })?;
        by_portable_name.extend(0..resources.len());
        by_portable_name.sort_unstable_by(|left, right| {
            compare_ascii_case_insensitive(
                portable_file_name(&resources[*left].path),
                portable_file_name(&resources[*right].path),
            )
            .then_with(|| left.cmp(right))
        });

        Ok(Self {
            by_full_path,
            by_portable_name,
        })
    }

    fn find(&self, resources: &[LoadedResource], requested_path: &str) -> Option<usize> {
        let full = first_resource_index(
            &self.by_full_path,
            resources,
            requested_path,
            compare_normalized_resource_paths,
        );
        let requested_name = portable_file_name(requested_path);
        let portable = first_resource_index(
            &self.by_portable_name,
            resources,
            requested_name,
            |candidate, requested| {
                compare_ascii_case_insensitive(portable_file_name(candidate), requested)
            },
        );
        match (full, portable) {
            (Some(full), Some(portable)) => Some(full.min(portable)),
            (Some(index), None) | (None, Some(index)) => Some(index),
            (None, None) => None,
        }
    }
}

fn first_resource_index(
    entries: &[usize],
    resources: &[LoadedResource],
    requested: &str,
    compare: impl Fn(&str, &str) -> Ordering,
) -> Option<usize> {
    let start = entries.partition_point(|resource_index| {
        resources
            .get(*resource_index)
            .is_some_and(|resource| compare(&resource.path, requested) == Ordering::Less)
    });
    let resource_index = *entries.get(start)?;
    resources
        .get(resource_index)
        .filter(|resource| compare(&resource.path, requested) == Ordering::Equal)
        .map(|_| resource_index)
}

type ObjectMetadataKey = (usize, i64);
type ObjectMetadataEntry = (ObjectMetadataKey, LoadedObjectMetadata);

#[derive(Default)]
struct ObjectMetadataBuilder {
    entries: Vec<ObjectMetadataEntry>,
    index: HashMap<ObjectMetadataKey, usize>,
}

#[derive(Default)]
struct ObjectMetadataIndexBudget {
    logical_bytes: usize,
}

struct ContainerObjectMetadataBuildState<'a> {
    metadata: &'a mut ObjectMetadataBuilder,
    assignment_count: &'a mut usize,
    index_budget: &'a mut ObjectMetadataIndexBudget,
}

impl ObjectMetadataBuilder {
    fn get(&self, key: &ObjectMetadataKey) -> Option<&LoadedObjectMetadata> {
        self.index
            .get(key)
            .and_then(|index| self.entries.get(*index))
            .map(|entry| &entry.1)
    }

    fn entry_mut(
        &mut self,
        key: ObjectMetadataKey,
        budget: &mut ObjectMetadataIndexBudget,
        limits: &AssetLoadLimits,
    ) -> Result<&mut LoadedObjectMetadata> {
        if let Some(index) = self.index.get(&key).copied() {
            return self
                .entries
                .get_mut(index)
                .map(|entry| &mut entry.1)
                .ok_or_else(|| Error::invalid_data("object metadata build index is inconsistent"));
        }
        if self.entries.len() >= limits.maximum_object_metadata_entries {
            return Err(Error::invalid_data(format!(
                "object metadata exceeds {} unique entries",
                limits.maximum_object_metadata_entries
            )));
        }
        let additional = size_of::<ObjectMetadataEntry>()
            .checked_add(size_of::<(ObjectMetadataKey, usize)>())
            .ok_or_else(|| Error::invalid_data("object metadata entry size overflowed"))?;
        let next = checked_object_metadata_index_bytes(
            budget.logical_bytes,
            additional,
            limits.maximum_object_metadata_index_bytes,
            "object metadata build entry",
        )?;
        self.entries.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow object metadata entries: {error}"))
        })?;
        self.index.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow object metadata build index: {error}"))
        })?;
        let index = self.entries.len();
        self.entries.push((key, LoadedObjectMetadata::default()));
        self.index.insert(key, index);
        budget.logical_bytes = next;
        Ok(&mut self.entries[index].1)
    }

    fn finish(self) -> Vec<ObjectMetadataEntry> {
        let Self { mut entries, index } = self;
        drop(index);
        entries.sort_unstable_by_key(|entry| entry.0);
        entries
    }
}

fn object_metadata_position(
    entries: &[ObjectMetadataEntry],
    key: ObjectMetadataKey,
) -> Option<usize> {
    object_metadata_position_with_probe(entries, key, || {})
}

fn object_metadata_position_with_probe(
    entries: &[ObjectMetadataEntry],
    key: ObjectMetadataKey,
    mut probe: impl FnMut(),
) -> Option<usize> {
    let position = entries.partition_point(|entry| {
        probe();
        entry.0 < key
    });
    entries.get(position).filter(|entry| entry.0 == key)?;
    Some(position)
}

fn checked_object_metadata_index_bytes(
    current: usize,
    additional: usize,
    maximum: usize,
    field: &str,
) -> Result<usize> {
    let next = current
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data("object metadata index bytes overflowed"))?;
    if next > maximum {
        return Err(Error::invalid_data(format!(
            "{field} raises metadata indexes to {next} bytes, exceeding limit {maximum}"
        )));
    }
    Ok(next)
}

#[derive(Default)]
struct PendingObjectNames {
    animator_game_objects: Vec<(ObjectMetadataKey, ObjectReference)>,
    mono_behaviour_scripts: Vec<(ObjectMetadataKey, ObjectReference)>,
    mono_script_classes: HashMap<ObjectMetadataKey, String>,
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
        metadata: &mut ObjectMetadataBuilder,
        file_index: usize,
        file: &SerializedFile,
        object_index: usize,
        path_id: i64,
        class_id: i32,
        budget: &mut ObjectNameBudget,
        index_budget: &mut ObjectMetadataIndexBudget,
        limits: &AssetLoadLimits,
    ) -> Result<()> {
        if let Some(name_metadata) =
            read_object_name_metadata(file, object_index, limits.object_names)?
        {
            if let Some(name) = name_metadata.name {
                budget.charge(name.len(), limits)?;
                metadata
                    .entry_mut((file_index, path_id), index_budget, limits)?
                    .name = Some(name);
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
            self.insert_mono_script_class(
                (file_index, path_id),
                script.class_name,
                index_budget,
                limits,
            )?;
        }
        Ok(())
    }

    fn insert_mono_script_class(
        &mut self,
        key: ObjectMetadataKey,
        class_name: String,
        index_budget: &mut ObjectMetadataIndexBudget,
        limits: &AssetLoadLimits,
    ) -> Result<()> {
        if !self.mono_script_classes.contains_key(&key) {
            let next = checked_object_metadata_index_bytes(
                index_budget.logical_bytes,
                size_of::<(ObjectMetadataKey, String)>(),
                limits.maximum_object_metadata_index_bytes,
                "MonoScript class build entry",
            )?;
            self.mono_script_classes.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow MonoScript class build index: {error}"))
            })?;
            index_budget.logical_bytes = next;
        }
        self.mono_script_classes.insert(key, class_name);
        Ok(())
    }

    fn resolve(
        self,
        collection: &AssetCollection,
        metadata: &mut ObjectMetadataBuilder,
        budget: &mut ObjectNameBudget,
        index_budget: &mut ObjectMetadataIndexBudget,
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
            metadata.entry_mut(animator, index_budget, limits)?.name = Some(name);
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
            metadata.entry_mut(behaviour, index_budget, limits)?.name = Some(class_name.clone());
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
    path_bytes: usize,
    diagnostic_bytes: usize,
    input_directories: usize,
    directory_entries: usize,
}

impl AssetLoadBudget {
    fn charge_path(&mut self, length: usize, limits: &AssetLoadLimits) -> Result<()> {
        if length > limits.maximum_path_bytes {
            return Err(Error::invalid_data(format!(
                "asset path has {length} UTF-8 bytes, exceeding limit {}",
                limits.maximum_path_bytes
            )));
        }
        let total = self
            .path_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("asset path byte count overflowed"))?;
        if total > limits.maximum_total_path_bytes {
            return Err(Error::invalid_data(format!(
                "asset traversal paths total {total} UTF-8 bytes, exceeding limit {}",
                limits.maximum_total_path_bytes
            )));
        }
        self.path_bytes = total;
        Ok(())
    }

    fn checked_diagnostic_bytes(
        &self,
        path_bytes: usize,
        message_bytes: usize,
        limits: &AssetLoadLimits,
    ) -> Result<usize> {
        let additional = path_bytes
            .checked_add(message_bytes)
            .ok_or_else(|| Error::invalid_data("load diagnostic byte count overflowed"))?;
        let total = self
            .diagnostic_bytes
            .checked_add(additional)
            .ok_or_else(|| Error::invalid_data("load diagnostic byte total overflowed"))?;
        if total > limits.maximum_diagnostic_bytes {
            return Err(Error::invalid_data(format!(
                "load diagnostics require {total} UTF-8 bytes, exceeding limit {}",
                limits.maximum_diagnostic_bytes
            )));
        }
        Ok(total)
    }

    fn charge_input_directory(&mut self, limits: &AssetLoadLimits) -> Result<()> {
        self.input_directories = self
            .input_directories
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("directory count overflowed"))?;
        if self.input_directories > limits.maximum_input_directories {
            return Err(Error::invalid_data(format!(
                "directory traversal exceeds {} directories",
                limits.maximum_input_directories
            )));
        }
        Ok(())
    }

    fn charge_directory_entry(&mut self, limits: &AssetLoadLimits) -> Result<()> {
        self.directory_entries = self
            .directory_entries
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("directory entry count overflowed"))?;
        if self.directory_entries > limits.maximum_directory_entries {
            return Err(Error::invalid_data(format!(
                "directory traversal exceeds {} entries",
                limits.maximum_directory_entries
            )));
        }
        Ok(())
    }
}

#[derive(Default)]
struct LoadDiagnosticLength {
    length: usize,
    saturated: bool,
}

impl fmt::Write for LoadDiagnosticLength {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.saturated {
            return Ok(());
        }
        let remaining = MAXIMUM_DIAGNOSTIC_MESSAGE_BYTES - self.length;
        let prefix = bounded_utf8_prefix_length(value, remaining);
        self.length += prefix;
        self.saturated = prefix < value.len();
        Ok(())
    }
}

struct LoadDiagnosticString {
    value: String,
    saturated: bool,
}

impl fmt::Write for LoadDiagnosticString {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.saturated {
            return Ok(());
        }
        let remaining = MAXIMUM_DIAGNOSTIC_MESSAGE_BYTES - self.value.len();
        let prefix = bounded_utf8_prefix_length(value, remaining);
        self.value.push_str(&value[..prefix]);
        self.saturated = prefix < value.len();
        Ok(())
    }
}

fn bounded_utf8_prefix_length(value: &str, maximum: usize) -> usize {
    let mut end = value.len().min(maximum);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn load_diagnostic_message_length(error: &Error) -> Result<usize> {
    let mut output = LoadDiagnosticLength::default();
    fmt::write(&mut output, format_args!("{error}"))
        .map_err(|_| Error::invalid_data("cannot measure load diagnostic message"))?;
    Ok(output.length)
}

fn format_load_diagnostic(error: &Error, length: usize) -> Result<String> {
    let mut value = String::new();
    value
        .try_reserve_exact(length)
        .map_err(|_| Error::invalid_data("cannot allocate load diagnostic message"))?;
    let mut output = LoadDiagnosticString {
        value,
        saturated: false,
    };
    fmt::write(&mut output, format_args!("{error}"))
        .map_err(|_| Error::invalid_data("cannot format load diagnostic message"))?;
    debug_assert_eq!(output.value.len(), length);
    Ok(output.value)
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LoadPath(String);

impl LoadPath {
    fn from_owned(
        value: String,
        limits: &AssetLoadLimits,
        budget: &mut AssetLoadBudget,
    ) -> Result<Self> {
        budget.charge_path(value.len(), limits)?;
        Ok(Self(value))
    }

    fn from_path(
        value: &Path,
        limits: &AssetLoadLimits,
        budget: &mut AssetLoadBudget,
    ) -> Result<Self> {
        let utf8_length = lossy_os_str_utf8_length(value.as_os_str())?;
        budget.charge_path(utf8_length, limits)?;
        Ok(Self(copy_os_str_with_replacement(
            value.as_os_str(),
            utf8_length,
            "asset path",
        )?))
    }

    fn from_precharged_path(
        value: &Path,
        limits: &AssetLoadLimits,
        budget: &mut AssetLoadBudget,
    ) -> Result<Self> {
        let charged_length = filesystem_path_byte_length(value);
        let utf8_length = lossy_os_str_utf8_length(value.as_os_str())?;
        if utf8_length > limits.maximum_path_bytes {
            return Err(Error::invalid_data(format!(
                "asset path has {} UTF-8 bytes, exceeding limit {}",
                utf8_length, limits.maximum_path_bytes
            )));
        }
        if utf8_length > charged_length {
            budget.charge_path(utf8_length - charged_length, limits)?;
        }
        Ok(Self(copy_os_str_with_replacement(
            value.as_os_str(),
            utf8_length,
            "asset path",
        )?))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn into_string(self) -> String {
        self.0
    }

    fn try_copy_string(&self) -> Result<String> {
        let mut copy = String::new();
        copy.try_reserve_exact(self.0.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate load diagnostic path: {error}"))
        })?;
        copy.push_str(&self.0);
        Ok(copy)
    }
}

#[derive(Debug)]
struct PendingInput {
    path: LoadPath,
    region: Region,
    depth: usize,
    unity_version_hint: Option<crate::unity_version::UnityVersion>,
}

fn charge_pending_inputs(
    pending: &mut VecDeque<PendingInput>,
    additional: usize,
    budget: &mut AssetLoadBudget,
    limits: &AssetLoadLimits,
) -> Result<()> {
    let next = budget
        .discovered_files
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data("discovered file count overflowed"))?;
    budget.discovered_files = next;
    if next > limits.maximum_discovered_files {
        return Err(Error::invalid_data(format!(
            "asset traversal exceeds {} discovered files",
            limits.maximum_discovered_files
        )));
    }
    pending.try_reserve(additional).map_err(|error| {
        Error::invalid_data(format!("cannot grow pending asset input queue: {error}"))
    })?;
    Ok(())
}

fn filesystem_path_byte_length(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

fn copy_filesystem_path(
    path: &Path,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
) -> Result<PathBuf> {
    let length = filesystem_path_byte_length(path);
    budget.charge_path(length, limits)?;
    let mut copy = PathBuf::new();
    copy.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate filesystem path: {error}"))
    })?;
    copy.push(path);
    Ok(copy)
}

fn join_filesystem_path(
    parent: &Path,
    child: &std::ffi::OsStr,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
) -> Result<PathBuf> {
    let parent_bytes = parent.as_os_str().as_encoded_bytes();
    let child_length = child.as_encoded_bytes().len();
    let separator_length =
        usize::from(!parent_bytes.is_empty() && !matches!(parent_bytes.last(), Some(b'/' | b'\\')));
    let length = parent_bytes
        .len()
        .checked_add(separator_length)
        .and_then(|length| length.checked_add(child_length))
        .ok_or_else(|| Error::invalid_data("filesystem path length overflowed"))?;
    budget.charge_path(length, limits)?;
    let mut path = PathBuf::new();
    path.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate filesystem child path: {error}"))
    })?;
    path.push(parent);
    path.push(child);
    if filesystem_path_byte_length(&path) > length {
        return Err(Error::invalid_data(
            "filesystem path grew beyond its checked encoded length",
        ));
    }
    Ok(path)
}

fn collect_regular_files(
    root: &Path,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
) -> Result<Vec<PathBuf>> {
    budget.charge_input_directory(limits)?;
    let mut pending_directories = Vec::new();
    pending_directories.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot allocate directory queue: {error}"))
    })?;
    pending_directories.push(copy_filesystem_path(root, limits, budget)?);
    let mut files = Vec::new();
    while let Some(directory) = pending_directories.pop() {
        let mut children = Vec::new();
        for child in fs::read_dir(&directory)? {
            budget.charge_directory_entry(limits)?;
            let child = child?;
            let file_type = child.file_type()?;
            if !file_type.is_dir() && !file_type.is_file() {
                continue;
            }
            children.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot allocate directory entries: {error}"))
            })?;
            children.push((
                join_filesystem_path(&directory, &child.file_name(), limits, budget)?,
                file_type,
            ));
        }
        children.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        for (child, file_type) in children.into_iter().rev() {
            if file_type.is_dir() {
                budget.charge_input_directory(limits)?;
                pending_directories.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow directory queue: {error}"))
                })?;
                pending_directories.push(child);
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
                files.push(child);
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
) -> Result<Vec<(LoadPath, Region)>> {
    let mut regular_files = Vec::new();
    let mut split_files = Vec::new();
    for file in files {
        if file
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| extension.starts_with("split"))
        {
            split_files.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow split input table: {error}"))
            })?;
            split_files.push(file);
        } else {
            regular_files.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow regular input table: {error}"))
            })?;
            regular_files.push(file);
        }
    }

    let mut inputs = Vec::new();
    inputs
        .try_reserve_exact(regular_files.len().saturating_add(split_files.len()))
        .map_err(|error| Error::invalid_data(format!("cannot allocate input table: {error}")))?;
    let mut split_files = split_files.into_iter().peekable();
    while let Some(path) = split_files.next() {
        let base = split_base_path(&path).ok_or_else(|| {
            Error::invalid_data(format!(
                "invalid Unity split segment path: {}",
                path.display()
            ))
        })?;
        let mut segment_paths = Vec::new();
        segment_paths.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot allocate split path table: {error}"))
        })?;
        segment_paths.push(path);
        while let Some(next) = split_files.peek() {
            if split_base_path(next).as_ref() != Some(&base) {
                break;
            }
            let next = split_files
                .next()
                .expect("peeked split segment remains available");
            segment_paths.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow split path table: {error}"))
            })?;
            segment_paths.push(next);
        }
        if regular_files.binary_search(&base).is_ok() {
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
        budget.charge_path(filesystem_path_byte_length(&base), limits)?;
        inputs.push(open_split_group(&base, segments, limits, budget, true)?);
    }
    for file in regular_files {
        inputs.push((
            LoadPath::from_precharged_path(&file, limits, budget)?,
            Region::from_file(&file)?,
        ));
    }
    inputs.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(inputs)
}

/// Finds the streamed-data files Unity writes beside a serialized file.
///
/// Unity keeps a serialized file's texture, mesh and audio bytes in companions
/// named after it, and it uses two conventions rather than one:
///
/// * the whole file name plus an extension -- `resources.assets` and
///   `resources.assets.resS`, `level0` and `level0.resS`;
/// * the stem plus an extension -- `resources.assets` and
///   `resources.resource`, which is where a player build keeps its audio.
///
/// Pointed at a directory this loader picks them up with everything else, but
/// pointed at one file it used to load only that file, so every streamed
/// object in it failed with "external resource was not found", naming a file
/// sitting right beside the one it had been given. Exporting a single
/// `globalgamemanagers.assets` lost all 48 of its streamed objects that way,
/// and the stem form -- missed on the first attempt at this -- cost two audio
/// clips in `resources.assets`.
///
/// The stem form is restricted to the streamed-data extensions. Matching any
/// sibling that merely shares a stem would pull in `resources.txt` and
/// anything else a game happens to keep next door. Only real siblings are
/// considered either way, so nothing here can reach outside the directory the
/// caller named.
const STREAMED_EXTENSIONS: [&str; 2] = ["ress", "resource"];

fn companion_resource_inputs(
    path: &Path,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
) -> Result<Vec<(LoadPath, Region)>> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(Vec::new());
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    budget.charge_input_directory(limits)?;
    let prefix = format!("{name}.");
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let mut companions = Vec::new();
    for entry in fs::read_dir(parent)? {
        budget.charge_directory_entry(limits)?;
        let entry = entry?;
        // Every directory entry is inspected for every input, so this loop is
        // quadratic in the size of the directory -- a game data folder is tens
        // of thousands of files. Judge each entry from its own name first:
        // building `entry.path()` here allocated a `PathBuf` and then re-parsed
        // the whole path, parent components included, three times per entry,
        // and `file_type` was asked before anything had shown the entry to be
        // worth asking about.
        let candidate_file_name = entry.file_name();
        let Some(candidate_name) = candidate_file_name.to_str() else {
            continue;
        };
        if candidate_name == name {
            continue;
        }
        if !candidate_name.starts_with(&prefix) {
            let candidate = Path::new(candidate_name);
            let shares_stem = stem.is_some_and(|stem| {
                candidate.file_stem().and_then(|value| value.to_str()) == Some(stem)
            }) && candidate
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    STREAMED_EXTENSIONS
                        .iter()
                        .any(|expected| extension.eq_ignore_ascii_case(expected))
                });
            if !shares_stem {
                continue;
            }
        }
        if !entry.file_type()?.is_file() {
            continue;
        }
        let candidate = join_filesystem_path(parent, &candidate_file_name, limits, budget)?;
        if companions.len() >= limits.maximum_input_files {
            return Err(Error::invalid_data(format!(
                "companion resource files for {name} exceed {} inputs",
                limits.maximum_input_files
            )));
        }
        companions.push((
            LoadPath::from_precharged_path(&candidate, limits, budget)?,
            Region::from_file(&candidate)?,
        ));
    }
    // `read_dir` order is whatever the filesystem gives, and a collection that
    // depends on it is a collection that differs between machines.
    companions.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(companions)
}

fn prepare_single_file_input(
    path: &Path,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
) -> Result<Vec<(LoadPath, Region)>> {
    let Some(base) = split_base_path(path) else {
        let mut inputs = vec![(
            LoadPath::from_path(path, limits, budget)?,
            Region::from_file(path)?,
        )];
        inputs.extend(companion_resource_inputs(path, limits, budget)?);
        return Ok(inputs);
    };
    if base.is_file() {
        return Ok(vec![(
            LoadPath::from_path(&base, limits, budget)?,
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
    budget.charge_input_directory(limits)?;
    for entry in fs::read_dir(parent)? {
        budget.charge_directory_entry(limits)?;
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let candidate = join_filesystem_path(parent, &entry.file_name(), limits, budget)?;
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
        false,
    )?])
}

fn open_split_group(
    base: &Path,
    mut segments: Vec<(usize, PathBuf)>,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
    base_is_precharged: bool,
) -> Result<(LoadPath, Region)> {
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
    let load_path = if base_is_precharged {
        LoadPath::from_precharged_path(base, limits, budget)?
    } else {
        LoadPath::from_path(base, limits, budget)?
    };

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
    Ok((load_path, region))
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

fn nested_path(
    parent: &LoadPath,
    child: &str,
    limits: &AssetLoadLimits,
    budget: &mut AssetLoadBudget,
) -> Result<LoadPath> {
    let length = parent
        .as_str()
        .len()
        .checked_add(2)
        .and_then(|length| length.checked_add(child.len()))
        .ok_or_else(|| Error::invalid_data("nested asset path length overflowed"))?;
    budget.charge_path(length, limits)?;
    let mut path = String::new();
    path.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate nested asset path: {error}"))
    })?;
    path.push_str(parent.as_str());
    path.push_str("::");
    for character in child.chars() {
        path.push(if character == '\\' { '/' } else { character });
    }
    debug_assert_eq!(path.len(), length);
    Ok(LoadPath(path))
}

fn resource_path_matches(candidate: &str, requested: &str) -> bool {
    compare_normalized_resource_paths(candidate, requested) == Ordering::Equal
        || portable_file_name(candidate).eq_ignore_ascii_case(portable_file_name(requested))
}

fn compare_normalized_resource_paths(left: &str, right: &str) -> Ordering {
    normalized_resource_path_bytes(left).cmp(normalized_resource_path_bytes(right))
}

fn normalized_resource_path_bytes(path: &str) -> impl Iterator<Item = u8> + '_ {
    strip_archive_prefix(path).bytes().map(|byte| {
        if byte == b'\\' {
            b'/'
        } else {
            byte.to_ascii_lowercase()
        }
    })
}

fn strip_archive_prefix(mut path: &str) -> &str {
    while path.len() >= 9
        && path.as_bytes()[..8] == *b"archive:"
        && matches!(path.as_bytes()[8], b'/' | b'\\')
    {
        path = &path[9..];
    }
    path
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
    use std::collections::VecDeque;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use crate::Error;
    use crate::serialized::{ContainerMetadataReadLimits, SerializedFile};
    use crate::source::Region;
    use crate::unity_version::UnityVersion;

    use super::{
        AssetCollection, AssetLoadBudget, AssetLoadLimits, AssetLoadOptions, LoadDiagnostic,
        LoadFailurePolicy, LoadPath, LoadedResource, LoadedSerializedFile, ObjectMetadataBuilder,
        ObjectMetadataEntry, ObjectMetadataIndexBudget, ObjectMetadataKey, PendingInput,
        PendingObjectNames, SpriteAtlasAssignment, SpriteAtlasIndex, SpriteTextureCache,
        charge_pending_inputs, format_load_diagnostic, load_diagnostic_message_length,
        object_metadata_position_with_probe,
    };

    fn cache_image(byte: u8, pixel_bytes: usize) -> std::sync::Arc<crate::texture::RgbaImage> {
        std::sync::Arc::new(crate::texture::RgbaImage {
            width: 1,
            height: 1,
            pixels: vec![byte; pixel_bytes],
        })
    }

    #[test]
    fn sprite_texture_cache_reuses_only_exact_identity_and_limits() {
        use crate::texture::TextureReadLimits;

        let cache = SpriteTextureCache::default();
        let limits = TextureReadLimits::default();
        assert!(cache.get(0, 1, limits).is_none());

        let image = cache_image(7, 4);
        cache.insert(0, 1, limits, &image);
        let cached = cache.get(0, 1, limits).expect("same key is cached");
        assert_eq!(cached.pixels, image.pixels);

        // A different object, file, or limits value must decode again rather
        // than reuse a page decoded under different conditions.
        assert!(cache.get(0, 2, limits).is_none());
        assert!(cache.get(1, 1, limits).is_none());
        let other_limits = TextureReadLimits {
            maximum_dimension: 16,
            ..TextureReadLimits::default()
        };
        assert!(cache.get(0, 1, other_limits).is_none());

        assert_eq!(cache.stats(), (1, 4));
    }

    #[test]
    fn sprite_texture_cache_evicts_least_recently_used_within_caps() {
        use crate::texture::TextureReadLimits;

        let cache = SpriteTextureCache::default();
        let limits = TextureReadLimits::default();
        for object_index in 0..3 {
            cache.insert_with_caps(0, object_index, limits, &cache_image(1, 4), 2, 64);
        }
        // Two-entry cap: object 0 was evicted, 1 and 2 remain.
        assert_eq!(cache.cached_images(), [(0, 2), (0, 1)]);

        // A hit refreshes recency, so the untouched entry is evicted next.
        assert!(cache.get(0, 1, limits).is_some());
        cache.insert_with_caps(0, 3, limits, &cache_image(1, 4), 2, 64);
        assert_eq!(cache.cached_images(), [(0, 3), (0, 1)]);

        // The byte budget evicts older pages before retaining a new one, and
        // an image over the whole budget is simply not cached.
        cache.insert_with_caps(0, 4, limits, &cache_image(1, 62), 2, 64);
        assert_eq!(cache.cached_images(), [(0, 4)]);
        cache.insert_with_caps(0, 5, limits, &cache_image(1, 65), 2, 64);
        assert_eq!(cache.cached_images(), [(0, 4)]);

        // Re-inserting an existing key replaces it instead of duplicating it.
        cache.insert_with_caps(0, 4, limits, &cache_image(9, 4), 2, 64);
        assert_eq!(cache.cached_images(), [(0, 4)]);
        assert_eq!(cache.get(0, 4, limits).expect("still cached").pixels[0], 9);
    }

    #[test]
    fn sprite_atlas_assignment_lookup_scales_with_logarithmic_queries() {
        const ENTRY_COUNT: usize = 16_384;
        let assignments = (0..ENTRY_COUNT)
            .map(|sprite_object_index| SpriteAtlasAssignment {
                sprite_file: 0,
                sprite_object: sprite_object_index,
                atlas: sprite_object_index,
            })
            .collect();
        let index = SpriteAtlasIndex {
            atlases: Vec::new(),
            assignments,
        };
        let mut probes = 0_usize;
        for sprite_object_index in (0..ENTRY_COUNT).rev() {
            let matches = index.assignments(0, sprite_object_index, || probes += 1);
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].atlas, sprite_object_index);
        }
        assert!(
            probes < ENTRY_COUNT * 40,
            "{probes} boundary probes exceeded logarithmic lookup budget"
        );
    }

    #[test]
    fn loads_the_streamed_companions_beside_a_single_file_input() {
        // Unity puts a serialized file's streamed bytes in a companion named
        // after it. Given the directory this loader always found them; given
        // the file it did not, and every streamed object failed with "external
        // resource was not found" naming a file sitting right beside it.
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("unity-rs-companion-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let input = root.join("resources.assets");
        fs::write(&input, b"not a serialized file, but a real one on disk").unwrap();
        fs::write(root.join("resources.assets.resS"), b"streamed").unwrap();
        fs::write(root.join("resources.assets.resource"), b"also streamed").unwrap();
        // The stem form, which is where a player build keeps its audio.
        fs::write(root.join("resources.resource"), b"stem companion").unwrap();
        // Extension matching is ASCII-insensitive without allocating a
        // lowercase copy for every directory entry.
        fs::write(root.join("resources.ReSS"), b"mixed-case stem companion").unwrap();
        // Shares the stem but is not streamed data, so it is not a companion.
        fs::write(root.join("resources.txt"), b"not streamed data").unwrap();
        // Shares neither, so it is not a companion either.
        fs::write(root.join("other.assets.resS"), b"another file entirely").unwrap();

        let collection = AssetCollection::load_path(&input).unwrap();

        let names: Vec<_> = collection
            .resources
            .iter()
            .filter_map(|resource| {
                Path::new(&resource.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        assert!(
            names.contains(&"resources.assets.resS".to_owned())
                && names.contains(&"resources.assets.resource".to_owned()),
            "the companions beside the input were not loaded: {names:?}"
        );
        assert!(
            names.contains(&"resources.resource".to_owned())
                && names.contains(&"resources.ReSS".to_owned()),
            "the stem-named companion was not loaded: {names:?}"
        );
        assert!(
            !names.contains(&"other.assets.resS".to_owned())
                && !names.contains(&"resources.txt".to_owned()),
            "a file that is not this input's companion was loaded: {names:?}"
        );
        assert!(
            collection
                .resource("archive:/CAB-x/resources.assets.resS")
                .is_some(),
            "the companion should resolve by the name an asset refers to it by"
        );
        let limits = AssetLoadLimits {
            maximum_directory_entries: 1,
            ..AssetLoadLimits::default()
        };
        let error = AssetCollection::load_path_with_limits(&input, limits).unwrap_err();
        assert!(error.to_string().contains("exceeds 1 entries"));
        let limits = AssetLoadLimits {
            maximum_input_directories: 0,
            ..AssetLoadLimits::default()
        };
        let error = AssetCollection::load_path_with_limits(&input, limits).unwrap_err();
        assert!(error.to_string().contains("exceeds 0 directories"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pending_input_batches_charge_before_queue_growth() {
        let limits = AssetLoadLimits {
            maximum_discovered_files: 3,
            ..AssetLoadLimits::default()
        };
        let mut budget = AssetLoadBudget::default();
        let mut pending = VecDeque::new();
        charge_pending_inputs(&mut pending, 1, &mut budget, &limits).unwrap();
        pending.push_back(PendingInput {
            path: LoadPath("root".to_owned()),
            region: Region::from_bytes(b"root".to_vec()),
            depth: 0,
            unity_version_hint: None,
        });
        charge_pending_inputs(&mut pending, 2, &mut budget, &limits).unwrap();
        assert_eq!(budget.discovered_files, 3);
        assert!(pending.capacity() >= pending.len() + 2);

        let capacity = pending.capacity();
        let error = charge_pending_inputs(&mut pending, 1, &mut budget, &limits).unwrap_err();
        assert!(error.to_string().contains("exceeds 3 discovered files"));
        assert_eq!(budget.discovered_files, 4);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending.capacity(), capacity);
    }

    #[test]
    fn loads_large_web_batch_at_the_exact_discovered_file_limit() {
        const ENTRY_COUNT: usize = 16_384;
        let paths: Vec<_> = (0..ENTRY_COUNT)
            .map(|index| format!("entry-{index:05}"))
            .collect();
        let entries: Vec<_> = paths.iter().map(|path| (path.as_str(), &[][..])).collect();
        let web = web_file(&entries);
        let low_limits = AssetLoadLimits {
            maximum_discovered_files: ENTRY_COUNT,
            ..AssetLoadLimits::default()
        };
        let error =
            AssetCollection::load_with_limits("root", Region::from_bytes(web.clone()), low_limits)
                .unwrap_err();
        assert!(error.to_string().contains("exceeds 16384 discovered files"));

        let exact_limits = AssetLoadLimits {
            maximum_discovered_files: ENTRY_COUNT + 1,
            ..AssetLoadLimits::default()
        };
        let collection =
            AssetCollection::load_with_limits("root", Region::from_bytes(web), exact_limits)
                .unwrap();
        assert_eq!(collection.resources.len(), ENTRY_COUNT);
        assert_eq!(collection.resources[0].path, "root::entry-00000");
        assert_eq!(
            collection.resources[ENTRY_COUNT - 1].path,
            "root::entry-16383"
        );
    }

    #[test]
    fn loads_directory_roots_deterministically_and_limits_input_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("unity-rs-load-path-{unique}"));
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

        // The filesystem path budget is consumed as each directory entry is
        // discovered, not after every PathBuf has already been collected.
        let mut budget = super::AssetLoadBudget::default();
        let limits = AssetLoadLimits {
            maximum_total_path_bytes: super::filesystem_path_byte_length(&root),
            ..AssetLoadLimits::default()
        };
        let error = super::collect_regular_files(&root, &limits, &mut budget).unwrap_err();
        assert!(error.to_string().contains("asset traversal paths total"));
        assert_eq!(budget.directory_entries, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounds_root_and_nested_path_bytes_across_one_load() {
        let limits = AssetLoadLimits {
            maximum_path_bytes: 4,
            ..AssetLoadLimits::default()
        };
        let error = AssetCollection::load_with_limits(
            "12345",
            Region::from_bytes(b"resource".to_vec()),
            limits,
        )
        .unwrap_err();
        assert!(error.to_string().contains("5 UTF-8 bytes"));

        let limits = AssetLoadLimits {
            maximum_path_bytes: 6,
            maximum_total_path_bytes: 7,
            ..AssetLoadLimits::default()
        };
        let collection = AssetCollection::load_with_limits(
            "r",
            Region::from_bytes(web_file(&[("a\\b", b"payload")])),
            limits,
        )
        .unwrap();
        assert_eq!(collection.resources.len(), 1);
        assert_eq!(collection.resources[0].path, "r::a/b");

        let limits = AssetLoadLimits {
            maximum_path_bytes: 6,
            maximum_total_path_bytes: 6,
            ..AssetLoadLimits::default()
        };
        let error = AssetCollection::load_with_limits(
            "r",
            Region::from_bytes(web_file(&[("a\\b", b"payload")])),
            limits,
        )
        .unwrap_err();
        assert!(error.to_string().contains("paths total 7 UTF-8 bytes"));

        let limits = AssetLoadLimits {
            maximum_path_bytes: 4,
            maximum_total_path_bytes: 4,
            ..AssetLoadLimits::default()
        };
        let error = AssetCollection::load_regions_with_options(
            [
                ("abc".to_owned(), Region::from_bytes(b"a".to_vec())),
                ("de".to_owned(), Region::from_bytes(b"b".to_vec())),
            ],
            AssetLoadOptions {
                limits,
                ..AssetLoadOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("paths total 5 UTF-8 bytes"));
    }

    #[cfg(unix)]
    #[test]
    fn streams_non_utf8_filesystem_paths_before_committing_load_budgets() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let path = PathBuf::from(OsString::from_vec(vec![b'a', 0xff]));
        let per_path_limits = AssetLoadLimits {
            maximum_path_bytes: 3,
            ..AssetLoadLimits::default()
        };
        let mut budget = AssetLoadBudget::default();
        let error = LoadPath::from_path(&path, &per_path_limits, &mut budget).unwrap_err();
        assert!(error.to_string().contains("has 4 UTF-8 bytes"), "{error}");
        assert_eq!(budget.path_bytes, 0);

        let cumulative_limits = AssetLoadLimits {
            maximum_path_bytes: 4,
            maximum_total_path_bytes: 3,
            ..AssetLoadLimits::default()
        };
        let error = LoadPath::from_path(&path, &cumulative_limits, &mut budget).unwrap_err();
        assert!(
            error.to_string().contains("paths total 4 UTF-8 bytes"),
            "{error}"
        );
        assert_eq!(budget.path_bytes, 0);

        budget.charge_path(2, &cumulative_limits).unwrap();
        let error =
            LoadPath::from_precharged_path(&path, &cumulative_limits, &mut budget).unwrap_err();
        assert!(
            error.to_string().contains("paths total 4 UTF-8 bytes"),
            "{error}"
        );
        assert_eq!(budget.path_bytes, 2);

        let exact_limits = AssetLoadLimits {
            maximum_path_bytes: 4,
            maximum_total_path_bytes: 4,
            ..AssetLoadLimits::default()
        };
        let mut exact_budget = AssetLoadBudget::default();
        exact_budget.charge_path(2, &exact_limits).unwrap();
        let label =
            LoadPath::from_precharged_path(&path, &exact_limits, &mut exact_budget).unwrap();
        assert_eq!(label.as_str(), "a\u{fffd}");
        assert_eq!(exact_budget.path_bytes, 4);

        // Darwin's filesystem conversion rejects an arbitrary invalid byte
        // sequence with EILSEQ before Rust can open it. Linux accepts these
        // names, so its CI run additionally exercises the complete file path.
        #[cfg(target_os = "linux")]
        {
            let root = temporary_directory("non-utf8-load-path");
            let input = root.join(OsString::from_vec(vec![
                b'a', 0xff, b'.', b'r', b'e', b's', b'S',
            ]));
            fs::write(&input, b"resource payload").unwrap();
            let expected = input.to_string_lossy().into_owned();
            let collection = AssetCollection::load_path(&input).unwrap();
            assert_eq!(collection.resources.len(), 1);
            assert_eq!(collection.resources[0].path, expected);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn indexes_resource_paths_without_changing_first_match_or_stale_table_semantics() {
        let resources = [
            "bundle::other/foo.resS",
            "archive:/exact/foo.resS",
            "archive:\\folder\\bar.resS",
        ]
        .into_iter()
        .map(|path| LoadedResource {
            path: path.to_owned(),
            region: Region::from_bytes(path.as_bytes().to_vec()),
        })
        .collect();
        let mut collection = AssetCollection::from_loaded_parts(Vec::new(), resources);
        assert!(collection.resource_index.is_none());
        collection
            .rebuild_resource_index(AssetLoadLimits::default())
            .unwrap();
        assert!(collection.resource_index.is_some());

        // Resource zero only matches by portable name while resource one is
        // an exact normalized path match. The managed/legacy contract walks
        // discovery order and accepts either condition, so zero still wins.
        assert_eq!(collection.resource_index_by_path("EXACT/FOO.RESS"), Some(0));
        assert_eq!(
            collection.resource_index_by_path("folder/bar.ress"),
            Some(2)
        );

        // The tables are public for low-level callers. A stale index must not
        // return the old object, miss a newly matching path, or panic after a
        // reorder/removal.
        collection.resources[0].path = "renamed.resS".to_owned();
        assert_eq!(collection.resource_index_by_path("renamed.resS"), Some(0));
        assert_eq!(collection.resource_index_by_path("foo.resS"), Some(1));
        collection.resources.swap(0, 2);
        assert_eq!(collection.resource_index_by_path("bar.resS"), Some(0));
        collection.resources.clear();
        assert_eq!(collection.resource_index_by_path("bar.resS"), None);
    }

    #[test]
    fn builds_resource_index_transactionally_with_a_combined_entry_budget() {
        let resources = ["a.resS", "b.resS"]
            .into_iter()
            .map(|path| LoadedResource {
                path: path.to_owned(),
                region: Region::from_bytes(Vec::new()),
            })
            .collect();
        let mut collection = AssetCollection::from_loaded_parts(Vec::new(), resources);
        let limits = AssetLoadLimits {
            maximum_resource_index_entries: 3,
            ..AssetLoadLimits::default()
        };
        let error = collection.rebuild_resource_index(limits).unwrap_err();
        assert!(error.to_string().contains("needs 4 entries"));
        assert!(collection.resource_index.is_none());
        assert_eq!(collection.resource_index_by_path("B.RESS"), Some(1));

        let loaded = AssetCollection::load_regions_with_options(
            [
                ("a.resS".to_owned(), Region::from_bytes(b"a".to_vec())),
                ("b.resS".to_owned(), Region::from_bytes(b"b".to_vec())),
            ],
            AssetLoadOptions::default(),
        )
        .unwrap();
        assert!(loaded.resource_index.is_some());
        assert_eq!(loaded.resource_index_by_path("B.RESS"), Some(1));
    }

    #[test]
    fn round_trips_owned_parts_without_carrying_derived_indexes() {
        let mut collection = AssetCollection::from_loaded_parts(
            Vec::new(),
            vec![LoadedResource {
                path: "old.resS".to_owned(),
                region: Region::from_bytes(b"payload".to_vec()),
            }],
        );
        collection.diagnostics.push(LoadDiagnostic {
            path: "skipped.assets".to_owned(),
            message: "unsupported fixture".to_owned(),
        });
        collection
            .rebuild_resource_index(AssetLoadLimits::default())
            .unwrap();
        assert!(collection.resource_index.is_some());

        let mut parts = collection.into_parts();
        parts.resources[0].path = "new.resS".to_owned();
        let mut rebuilt = AssetCollection::from_parts(parts);

        assert!(rebuilt.reference_index.is_none());
        assert!(rebuilt.resource_index.is_none());
        assert!(rebuilt.object_metadata.is_empty());
        assert!(rebuilt.serialized_files().is_empty());
        assert_eq!(rebuilt.resources()[0].path, "new.resS");
        assert_eq!(rebuilt.diagnostics.len(), 1);
        assert_eq!(rebuilt.diagnostics[0].path, "skipped.assets");
        assert_eq!(rebuilt.resource_index_by_path("NEW.RESS"), Some(0));

        rebuilt
            .rebuild_resource_index(AssetLoadLimits::default())
            .unwrap();
        assert!(rebuilt.resource_index.is_some());
        assert_eq!(rebuilt.resource_index_by_path("NEW.RESS"), Some(0));
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
    fn object_metadata_index_scales_and_preserves_last_assignment() {
        const COUNT: usize = 16_384;
        let entry_bytes = object_metadata_entry_bytes();
        let limits = AssetLoadLimits {
            maximum_object_metadata_entries: COUNT,
            maximum_object_metadata_index_bytes: COUNT.checked_mul(entry_bytes).unwrap(),
            ..AssetLoadLimits::default()
        };
        let mut metadata = ObjectMetadataBuilder::default();
        let mut budget = ObjectMetadataIndexBudget::default();
        for path_id in (0..COUNT).rev() {
            metadata
                .entry_mut((0, i64::try_from(path_id).unwrap()), &mut budget, &limits)
                .unwrap();
        }
        let charged = budget.logical_bytes;
        metadata
            .entry_mut((0, 7), &mut budget, &limits)
            .unwrap()
            .name = Some("last".to_owned());
        assert_eq!(budget.logical_bytes, charged);

        let entries = metadata.finish();
        let mut comparisons = 0_usize;
        for path_id in 0..COUNT {
            let key = (0, i64::try_from(path_id).unwrap());
            let position = object_metadata_position_with_probe(&entries, key, || {
                comparisons += 1;
            })
            .unwrap();
            assert_eq!(entries[position].0, key);
        }
        assert!(comparisons < COUNT * 20, "used {comparisons} comparisons");
        let seventh = object_metadata_position_with_probe(&entries, (0, 7), || {}).unwrap();
        assert_eq!(entries[seventh].1.name.as_deref(), Some("last"));
    }

    #[test]
    fn object_metadata_budget_rejects_before_growth() {
        let entry_bytes = object_metadata_entry_bytes();
        let limits = AssetLoadLimits {
            maximum_object_metadata_entries: 1,
            maximum_object_metadata_index_bytes: entry_bytes,
            ..AssetLoadLimits::default()
        };
        let mut metadata = ObjectMetadataBuilder::default();
        let mut budget = ObjectMetadataIndexBudget::default();
        metadata.entry_mut((0, 1), &mut budget, &limits).unwrap();
        metadata.entry_mut((0, 1), &mut budget, &limits).unwrap();
        let error = metadata
            .entry_mut((0, 2), &mut budget, &limits)
            .unwrap_err();
        assert!(error.to_string().contains("exceeds 1 unique entries"));
        assert_eq!(metadata.entries.len(), 1);
        assert_eq!(metadata.index.len(), 1);
        assert_eq!(budget.logical_bytes, entry_bytes);

        let low_limits = AssetLoadLimits {
            maximum_object_metadata_index_bytes: entry_bytes - 1,
            ..AssetLoadLimits::default()
        };
        let mut metadata = ObjectMetadataBuilder::default();
        let mut budget = ObjectMetadataIndexBudget::default();
        let error = metadata
            .entry_mut((0, 1), &mut budget, &low_limits)
            .unwrap_err();
        assert!(error.to_string().contains("metadata indexes"));
        assert!(metadata.entries.is_empty() && metadata.index.is_empty());
        assert_eq!(budget.logical_bytes, 0);
    }

    #[test]
    fn mono_script_class_index_shares_budget_and_preserves_last_value() {
        let entry_bytes = object_metadata_entry_bytes();
        let script_bytes = std::mem::size_of::<(ObjectMetadataKey, String)>();
        let limits = AssetLoadLimits {
            maximum_object_metadata_index_bytes: entry_bytes + script_bytes,
            ..AssetLoadLimits::default()
        };
        let mut metadata = ObjectMetadataBuilder::default();
        let mut pending = PendingObjectNames::default();
        let mut budget = ObjectMetadataIndexBudget::default();
        metadata.entry_mut((0, 1), &mut budget, &limits).unwrap();
        pending
            .insert_mono_script_class((0, 2), "First".to_owned(), &mut budget, &limits)
            .unwrap();
        let charged = budget.logical_bytes;
        pending
            .insert_mono_script_class((0, 2), "Second".to_owned(), &mut budget, &limits)
            .unwrap();
        assert_eq!(budget.logical_bytes, charged);
        assert_eq!(pending.mono_script_classes.get(&(0, 2)).unwrap(), "Second");
        let error = pending
            .insert_mono_script_class((0, 3), "Third".to_owned(), &mut budget, &limits)
            .unwrap_err();
        assert!(error.to_string().contains("metadata indexes"));
        assert_eq!(pending.mono_script_classes.len(), 1);
    }

    #[test]
    fn public_object_metadata_limits_reject_named_assets() {
        let entry_bytes = object_metadata_entry_bytes();
        let limits = [
            AssetLoadLimits {
                maximum_object_metadata_entries: 0,
                ..AssetLoadLimits::default()
            },
            AssetLoadLimits {
                maximum_object_metadata_index_bytes: entry_bytes - 1,
                ..AssetLoadLimits::default()
            },
        ];
        for limits in limits {
            let mut collection = named_material_collection();
            let error = collection.resolve_object_metadata(limits).unwrap_err();
            assert!(
                error.to_string().contains("object metadata"),
                "unexpected error: {error}"
            );
            assert!(collection.object_metadata.is_empty());
        }
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
    fn skipped_input_diagnostics_have_an_exact_cumulative_string_budget() {
        const MESSAGE: &str = "unsupported: UnityArchive bundles are recognized, but their layout is not documented or sample-verified";
        let archive = || {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"UnityArchive\0");
            bytes.extend_from_slice(&5_u32.to_be_bytes());
            bytes.extend_from_slice(b"5.x.x\0");
            bytes.extend_from_slice(b"5.0.0f4\0");
            bytes
        };
        let first = "a-archive.unity3d";
        let second = "b-archive.unity3d";
        assert_eq!(first.len(), second.len());
        let one_record = first.len() + MESSAGE.len();

        let exact = AssetCollection::load_regions_with_options(
            [(first.to_owned(), Region::from_bytes(archive()))],
            AssetLoadOptions {
                limits: AssetLoadLimits {
                    maximum_diagnostic_bytes: one_record,
                    ..AssetLoadLimits::default()
                },
                failure_policy: LoadFailurePolicy::SkipInput,
                ..AssetLoadOptions::default()
            },
        )
        .unwrap();
        assert_eq!(exact.diagnostics.len(), 1);
        assert_eq!(exact.diagnostics[0].path, first);
        assert_eq!(exact.diagnostics[0].message, MESSAGE);

        let low = AssetCollection::load_regions_with_options(
            [(first.to_owned(), Region::from_bytes(archive()))],
            AssetLoadOptions {
                limits: AssetLoadLimits {
                    maximum_diagnostic_bytes: one_record - 1,
                    ..AssetLoadLimits::default()
                },
                failure_policy: LoadFailurePolicy::SkipInput,
                ..AssetLoadOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            low.to_string().contains("load diagnostics require"),
            "{low}"
        );

        let cumulative = AssetCollection::load_regions_with_options(
            [
                (first.to_owned(), Region::from_bytes(archive())),
                (second.to_owned(), Region::from_bytes(archive())),
            ],
            AssetLoadOptions {
                limits: AssetLoadLimits {
                    maximum_diagnostic_bytes: one_record * 2,
                    ..AssetLoadLimits::default()
                },
                failure_policy: LoadFailurePolicy::SkipInput,
                ..AssetLoadOptions::default()
            },
        )
        .unwrap();
        assert_eq!(cumulative.diagnostics.len(), 2);

        let cumulative_low = AssetCollection::load_regions_with_options(
            [
                (first.to_owned(), Region::from_bytes(archive())),
                (second.to_owned(), Region::from_bytes(archive())),
            ],
            AssetLoadOptions {
                limits: AssetLoadLimits {
                    maximum_diagnostic_bytes: one_record * 2 - 1,
                    ..AssetLoadLimits::default()
                },
                failure_policy: LoadFailurePolicy::SkipInput,
                ..AssetLoadOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            cumulative_low
                .to_string()
                .contains("load diagnostics require"),
            "{cumulative_low}"
        );

        let long_error = Error::unsupported("é".repeat(4096));
        let length = load_diagnostic_message_length(&long_error).unwrap();
        let bounded = format_load_diagnostic(&long_error, length).unwrap();
        assert!(bounded.len() <= super::MAXIMUM_DIAGNOSTIC_MESSAGE_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
        assert_eq!(bounded.len(), length);
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
    fn strict_unity_versions_option_reaches_every_loaded_serialized_file() {
        let declared = empty_v22_serialized_file_with_version("2019.4.40f1");

        let default_loaded =
            AssetCollection::load("strictness.assets", Region::from_bytes(declared.clone()))
                .unwrap();
        assert!(
            !default_loaded.serialized_files[0]
                .file
                .strict_unity_versions
        );

        let strict_loaded = AssetCollection::load_with_options(
            "strictness.assets",
            Region::from_bytes(declared),
            AssetLoadOptions {
                strict_unity_versions: true,
                ..AssetLoadOptions::default()
            },
        )
        .unwrap();
        assert!(strict_loaded.serialized_files[0].file.strict_unity_versions);
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
        let root = std::env::temp_dir().join(format!("unity-rs-{label}-{unique}"));
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

    fn object_metadata_entry_bytes() -> usize {
        std::mem::size_of::<ObjectMetadataEntry>()
            .checked_add(std::mem::size_of::<(ObjectMetadataKey, usize)>())
            .unwrap()
    }

    fn named_material_collection() -> AssetCollection {
        let mut material = Vec::new();
        push_aligned_string(&mut material, "named material");
        let file = SerializedFile::open(Region::from_bytes(synthetic_v22_file(
            "2022.3.62f1",
            &[(21, 1, material)],
            &[],
        )))
        .unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "named.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
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
