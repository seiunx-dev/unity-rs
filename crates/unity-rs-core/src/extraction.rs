use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::bundle::{
    BlockDecodeCache, BundleHeader, BundleOpenOptions, BundleParseLimits, OodleDecoder,
    UnityFsBundle,
};
use crate::compression::{CompressionLimits, ZipContainer, decompress_brotli, decompress_gzip};
use crate::endian::{Endian, EndianReader};
use crate::file_type::{FileDetection, FileType, HEADER_SCAN_LENGTH, detect_file_type};
use crate::filesystem_text::{
    copy_os_str_with_replacement, for_each_os_str_char_lossy, lossy_os_str_utf8_length,
};
use crate::legacy_bundle::LegacyBundle;
use crate::source::Region;
use crate::unity_cn::UnityCnKey;
use crate::web_file::{WebFile, WebParseLimits};
use crate::{Error, Result};

const MAX_PORTABLE_COMPONENT_BYTES: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractionLimits {
    pub maximum_input_files: usize,
    pub maximum_nesting_depth: usize,
    pub maximum_entries: usize,
    pub maximum_single_entry_bytes: u64,
    pub maximum_expanded_bytes: u64,
    pub maximum_output_bytes: u64,
    /// Maximum bytes in one filesystem input path, caller label, archive path,
    /// or fully qualified recursive diagnostic label.
    pub maximum_path_bytes: usize,
    /// Maximum cumulative bytes retained for filesystem traversal paths and
    /// recursive diagnostic labels during one extraction.
    pub maximum_total_path_bytes: usize,
    /// Maximum cumulative bytes retained by report source labels, output
    /// paths, and failure messages. This is separate from traversal-path
    /// storage because the returned report outlives the extractor's indexes.
    pub maximum_metadata_bytes: usize,
    pub compression: CompressionLimits,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            maximum_input_files: 1_000_000,
            maximum_nesting_depth: 32,
            maximum_entries: 1_000_000,
            maximum_single_entry_bytes: 512 * 1024 * 1024,
            maximum_expanded_bytes: 4 * 1024 * 1024 * 1024,
            maximum_output_bytes: 4 * 1024 * 1024 * 1024,
            maximum_path_bytes: 32_767,
            maximum_total_path_bytes: 64 * 1024 * 1024,
            maximum_metadata_bytes: 256 * 1024 * 1024,
            compression: CompressionLimits::default(),
        }
    }
}

#[derive(Clone, Default)]
pub struct ExtractionOptions {
    pub limits: ExtractionLimits,
    pub overwrite_existing: bool,
    pub oodle_decoder: Option<Arc<dyn OodleDecoder>>,
    /// Key for UnityCN-encrypted bundles. Without one they stay refused.
    pub unity_cn_key: Option<UnityCnKey>,
}

impl fmt::Debug for ExtractionOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExtractionOptions")
            .field("limits", &self.limits)
            .field("overwrite_existing", &self.overwrite_existing)
            .field(
                "oodle_decoder",
                &self.oodle_decoder.as_ref().map(|_| "<configured>"),
            )
            .field("unity_cn_key", &self.unity_cn_key)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionRecord {
    pub source: String,
    pub output_path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionSkip {
    pub source: String,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionFailure {
    pub source: String,
    pub error: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExtractionReport {
    pub extracted: Vec<ExtractionRecord>,
    pub skipped_existing: Vec<ExtractionSkip>,
    pub failures: Vec<ExtractionFailure>,
    pub output_bytes: u64,
}

#[derive(Debug, Default)]
struct ExtractionPathBudget {
    bytes: usize,
}

impl ExtractionPathBudget {
    fn charge_path(&mut self, length: usize, limits: ExtractionLimits) -> Result<()> {
        self.bytes = self.checked_path_total(length, limits)?;
        Ok(())
    }

    fn checked_path_total(&self, length: usize, limits: ExtractionLimits) -> Result<usize> {
        if length > limits.maximum_path_bytes {
            return Err(Error::invalid_data(format!(
                "extraction path or label is {length} bytes, exceeding limit {}",
                limits.maximum_path_bytes
            )));
        }
        self.checked_additional_total(length, limits)
    }

    fn charge_additional(&mut self, length: usize, limits: ExtractionLimits) -> Result<()> {
        self.bytes = self.checked_additional_total(length, limits)?;
        Ok(())
    }

    fn checked_additional_total(&self, length: usize, limits: ExtractionLimits) -> Result<usize> {
        let total = self
            .bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("extraction path byte count overflowed"))?;
        if total > limits.maximum_total_path_bytes {
            return Err(Error::invalid_data(format!(
                "extraction paths total {total} bytes, exceeding limit {}",
                limits.maximum_total_path_bytes
            )));
        }
        Ok(total)
    }
}

/// Recursively extracts one regular file or a directory tree.
///
/// Child symlinks are never followed. Every archive path is converted to a
/// portable relative path before it is joined to `output_root`.
pub fn extract_path(
    input: &Path,
    output_root: &Path,
    options: ExtractionOptions,
) -> Result<ExtractionReport> {
    validate_limits(options.limits)?;
    let mut path_budget = ExtractionPathBudget::default();
    let metadata = fs::symlink_metadata(input)?;
    if metadata.file_type().is_symlink() {
        return Err(Error::invalid_data(format!(
            "input path must not be a symbolic link: {}",
            input.display()
        )));
    }

    let input_absolute = lexical_absolute(input)?;
    let output_absolute = lexical_absolute(output_root)?;
    let roots = if metadata.is_file() {
        let file_name = input.file_name().ok_or_else(|| {
            Error::invalid_data(format!("input file has no name: {}", input.display()))
        })?;
        let mut roots = Vec::new();
        roots.try_reserve_exact(1).map_err(|error| {
            Error::invalid_data(format!("cannot allocate input roots: {error}"))
        })?;
        roots.push((
            copy_filesystem_path(input, options.limits, &mut path_budget)?,
            sanitize_os_component_path(file_name, options.limits)?,
        ));
        roots
    } else if metadata.is_dir() {
        if output_absolute.starts_with(&input_absolute) {
            return Err(Error::invalid_data(
                "output directory must not be inside the input directory",
            ));
        }
        let files = collect_regular_files(input, options.limits, &mut path_budget)?;
        let mut roots = Vec::new();
        roots.try_reserve_exact(files.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate extraction input roots: {error}"))
        })?;
        for path in files {
            let relative = path
                .strip_prefix(input)
                .map_err(|_| Error::invalid_data("directory input escaped its traversal root"))?;
            let output_path = sanitize_filesystem_relative_path(relative, options.limits)?;
            roots.push((path, output_path));
        }
        roots
    } else {
        return Err(Error::invalid_data(format!(
            "input is neither a regular file nor a directory: {}",
            input.display()
        )));
    };

    ensure_secure_directory(&output_absolute)?;
    let limits = options.limits;
    let mut extractor = Extractor::with_path_budget(output_absolute, options, path_budget);
    for (path, relative) in roots {
        let label = filesystem_path_label(&path, limits, &mut extractor.budget.paths)?;
        let result = Region::from_file(&path)
            .and_then(|region| extractor.process_region(&label, region, relative, 0));
        if let Err(error) = result {
            extractor.record_failure(label, &error)?;
        }
    }
    Ok(extractor.report)
}

/// Recursively extracts an already bounded source region.
pub fn extract_region(
    label: &str,
    region: Region,
    output_root: &Path,
    options: ExtractionOptions,
) -> Result<ExtractionReport> {
    validate_limits(options.limits)?;
    let mut path_budget = ExtractionPathBudget::default();
    path_budget.charge_path(label.len(), options.limits)?;
    let output_absolute = lexical_absolute(output_root)?;
    ensure_secure_directory(&output_absolute)?;
    let file_name = label.rsplit(['/', '\\']).next().unwrap_or(label);
    let relative = sanitize_component_path(file_name, options.limits)?;
    let mut extractor = Extractor::with_path_budget(output_absolute, options, path_budget);
    if let Err(error) = extractor.process_region(label, region, relative, 0) {
        extractor.record_failure(copy_extraction_label(label)?, &error)?;
    }
    Ok(extractor.report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ClaimKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CollisionCursorKind {
    Leaf(ClaimKind),
    Parent,
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct CollisionCursorKey {
    path: PathBuf,
    kind: CollisionCursorKind,
}

#[derive(Debug, Default)]
struct ExtractionBudget {
    paths: ExtractionPathBudget,
    entries: usize,
    expanded_bytes: u64,
    report_metadata_bytes: usize,
}

struct Extractor {
    output_root: PathBuf,
    options: ExtractionOptions,
    budget: ExtractionBudget,
    /// Output paths already handed out, keyed by [`portable_key`] rather than
    /// by the path itself.
    ///
    /// Two names that differ only in case are the same file on the platforms
    /// this has to be safe on, so a claim has to be found by that comparison.
    /// Doing it by scanning every key made allocation quadratic in the number
    /// of entries, and an archive with tens of thousands of them in one
    /// directory spent all its time there. The map is never iterated for
    /// output ordering, so randomized hashing cannot affect file names; growth
    /// is reserved fallibly before every insertion.
    claims: HashMap<PathBuf, ClaimKind>,
    /// The suffix already reached for one portable desired path.
    ///
    /// Claims only grow during an extraction, so a rejected lower suffix never
    /// becomes usable later through this process. Remembering the next leaf
    /// suffix avoids restarting an O(n) scan for every duplicate entry. Parent
    /// cursors remember the usable directory suffix itself so every child can
    /// reuse it instead of rescanning the same file/directory conflicts.
    /// Cursor keys are charged to the same cumulative path budget as claims.
    collision_cursors: HashMap<CollisionCursorKey, u64>,
    temporary_sequence: u64,
    report: ExtractionReport,
}

impl Extractor {
    fn with_path_budget(
        output_root: PathBuf,
        options: ExtractionOptions,
        path_budget: ExtractionPathBudget,
    ) -> Self {
        Self {
            output_root,
            options,
            budget: ExtractionBudget {
                paths: path_budget,
                ..ExtractionBudget::default()
            },
            claims: HashMap::new(),
            collision_cursors: HashMap::new(),
            temporary_sequence: 0,
            report: ExtractionReport::default(),
        }
    }

    fn process_region(
        &mut self,
        label: &str,
        region: Region,
        desired_path: PathBuf,
        depth: usize,
    ) -> Result<()> {
        let detection = detect_region(&region)?;
        self.process_detected_region(label, region, desired_path, depth, detection)
    }

    fn process_detected_region(
        &mut self,
        label: &str,
        region: Region,
        desired_path: PathBuf,
        depth: usize,
        detection: FileDetection,
    ) -> Result<()> {
        if depth > self.options.limits.maximum_nesting_depth {
            return Err(Error::invalid_data(format!(
                "extraction exceeds {} container layers",
                self.options.limits.maximum_nesting_depth
            )));
        }
        match detection.file_type {
            FileType::AssetsFile | FileType::ResourceFile => {
                self.write_region_leaf(label, &desired_path, &region)
            }
            FileType::BundleFile => {
                self.process_bundle(label, &region, &desired_path, depth, detection)
            }
            FileType::WebFile => self.process_web_file(label, region, &desired_path, depth),
            FileType::ZipFile => self.process_zip(label, &region, &desired_path, depth),
            FileType::GzipFile => {
                let decoded = decompress_gzip(&region, self.stream_compression_limits()?)?;
                self.process_wrapper(label, "gzip", decoded, desired_path, depth)
            }
            FileType::BrotliFile => {
                let decoded = decompress_brotli(&region, self.stream_compression_limits()?)?;
                self.process_wrapper(label, "brotli", decoded, desired_path, depth)
            }
        }
    }

    fn process_wrapper(
        &mut self,
        label: &str,
        wrapper: &str,
        decoded: Region,
        desired_path: PathBuf,
        depth: usize,
    ) -> Result<()> {
        self.charge_expansion(decoded.len())?;
        let next_depth = self.next_depth(depth)?;
        let detection = detect_region(&decoded)?;
        let child_path = if matches!(
            detection.file_type,
            FileType::AssetsFile | FileType::ResourceFile
        ) {
            decoded_leaf_path(
                &desired_path,
                wrapper,
                self.options.limits.maximum_path_bytes,
            )?
        } else {
            desired_path
        };
        let child_label = self.nested_label(label, wrapper)?;
        self.process_detected_region(&child_label, decoded, child_path, next_depth, detection)
    }

    fn process_web_file(
        &mut self,
        label: &str,
        region: Region,
        desired_path: &Path,
        depth: usize,
    ) -> Result<()> {
        let limits = WebParseLimits {
            max_header_size: WebParseLimits::default().max_header_size,
            max_entries: self.options.limits.maximum_entries,
            max_path_length: self.options.limits.maximum_path_bytes,
            max_entry_read_size: self.options.limits.maximum_single_entry_bytes,
        };
        let web = WebFile::open_with_limits(region, limits)?;
        if web.entries.is_empty() {
            return Ok(());
        }
        let container = self.allocate_container_directory(desired_path)?;
        let next_depth = self.next_depth(depth)?;
        for index in 0..web.entries.len() {
            let entry = &web.entries[index];
            let child_label = self.nested_label(label, &entry.path)?;
            let result = self
                .charge_entry(entry.data_length)
                .and_then(|()| sanitize_archive_path(&entry.path, self.options.limits))
                .and_then(|path| {
                    let desired_path = join_relative_path_fallibly(
                        &container,
                        &path,
                        self.options.limits.maximum_path_bytes,
                        "nested extraction output path",
                    )?;
                    self.process_region(
                        &child_label,
                        web.entry_region(index)?,
                        desired_path,
                        next_depth,
                    )
                });
            if let Err(error) = result {
                self.record_failure(child_label, &error)?;
            }
        }
        Ok(())
    }

    fn process_zip(
        &mut self,
        label: &str,
        region: &Region,
        desired_path: &Path,
        depth: usize,
    ) -> Result<()> {
        let archive = ZipContainer::open(region, self.zip_compression_limits()?)?;
        if archive.entries.is_empty() {
            return Ok(());
        }
        let container = self.allocate_container_directory(desired_path)?;
        let next_depth = self.next_depth(depth)?;
        for index in 0..archive.entries.len() {
            let entry = &archive.entries[index];
            let child_label = self.nested_label(label, &entry.path)?;
            let result = self
                .charge_entry(entry.size)
                .and_then(|()| sanitize_archive_path(&entry.path, self.options.limits))
                .and_then(|path| {
                    let desired_path = join_relative_path_fallibly(
                        &container,
                        &path,
                        self.options.limits.maximum_path_bytes,
                        "nested extraction output path",
                    )?;
                    self.process_region(
                        &child_label,
                        archive.read_entry(index)?,
                        desired_path,
                        next_depth,
                    )
                });
            if let Err(error) = result {
                self.record_failure(child_label, &error)?;
            }
        }
        Ok(())
    }

    fn process_bundle(
        &mut self,
        label: &str,
        root: &Region,
        desired_path: &Path,
        depth: usize,
        detection: FileDetection,
    ) -> Result<()> {
        let bundle_length = root
            .len()
            .checked_sub(detection.data_offset)
            .ok_or_else(|| Error::invalid_data("embedded bundle offset exceeds input"))?;
        let region = root.subregion(detection.data_offset, bundle_length)?;
        let header = BundleHeader::read(&mut EndianReader::new(region.cursor(), Endian::Big))?;
        if header.signature == "UnityArchive" {
            return Err(Error::unsupported(
                "UnityArchive bundles are recognized, but their layout is not documented or sample-verified",
            ));
        }
        let limits = BundleParseLimits {
            max_entries: self.options.limits.maximum_entries,
            max_path_length: self.options.limits.maximum_path_bytes,
            max_entry_read_size: self.options.limits.maximum_single_entry_bytes,
            ..BundleParseLimits::default()
        };
        if header.signature == "UnityFS"
            || (matches!(header.signature.as_str(), "UnityWeb" | "UnityRaw") && header.version == 6)
        {
            let bundle = UnityFsBundle::open_with_options(
                &region,
                BundleOpenOptions {
                    limits,
                    oodle_decoder: self.options.oodle_decoder.clone(),
                    unity_cn_key: self.options.unity_cn_key,
                },
            )?;
            self.process_unity_fs_entries(label, desired_path, depth, &bundle)
        } else if matches!(header.signature.as_str(), "UnityWeb" | "UnityRaw") {
            let bundle = LegacyBundle::open_with_limits(&region, limits)?;
            self.process_legacy_entries(label, desired_path, depth, &bundle)
        } else {
            Err(Error::unsupported(format!(
                "bundle signature {:?}",
                header.signature
            )))
        }
    }

    fn process_unity_fs_entries(
        &mut self,
        label: &str,
        desired_path: &Path,
        depth: usize,
        bundle: &UnityFsBundle,
    ) -> Result<()> {
        if bundle.entries.is_empty() {
            return Ok(());
        }
        let container = self.allocate_container_directory(desired_path)?;
        let next_depth = self.next_depth(depth)?;
        // One decoded-block cache for the whole bundle: the header probe and
        // the write pass of every entry reuse each other's block decodes
        // instead of decompressing the same blocks again.
        let mut block_cache = BlockDecodeCache::new();
        for index in 0..bundle.entries.len() {
            let entry = &bundle.entries[index];
            let child_label = self.nested_label(label, &entry.path)?;
            let result = self
                .charge_entry(entry.size)
                .and_then(|()| sanitize_archive_path(&entry.path, self.options.limits))
                .and_then(|path| {
                    let desired_path = join_relative_path_fallibly(
                        &container,
                        &path,
                        self.options.limits.maximum_path_bytes,
                        "nested extraction output path",
                    )?;
                    self.process_unity_fs_entry(
                        &child_label,
                        desired_path,
                        next_depth,
                        bundle,
                        index,
                        &mut block_cache,
                    )
                });
            if let Err(error) = result {
                self.record_failure(child_label, &error)?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn process_unity_fs_entry(
        &mut self,
        label: &str,
        desired_path: PathBuf,
        depth: usize,
        bundle: &UnityFsBundle,
        index: usize,
        block_cache: &mut BlockDecodeCache,
    ) -> Result<()> {
        let entry = bundle.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("UnityFS entry index {index} is out of range"))
        })?;
        // The probe only needs the detection window: copying the prefix
        // stops the block walk early instead of decompressing the whole
        // entry and discarding it, and the write pass below re-verifies the
        // full declared size.
        let probe_length = u64::try_from(HEADER_SCAN_LENGTH)
            .map_err(|_| Error::invalid_data("header scan length does not fit u64"))?
            .min(entry.size);
        let mut probe = HeaderProbe::new();
        let probed = bundle.copy_entry_prefix(index, probe_length, &mut probe, block_cache)?;
        if probed != probe_length || probe.written != probe_length {
            return Err(Error::invalid_data(format!(
                "UnityFS entry probe wrote {} bytes; expected {probe_length}",
                probe.written
            )));
        }
        let detection = detect_file_type(&probe.header, entry.size);
        if matches!(
            detection.file_type,
            FileType::AssetsFile | FileType::ResourceFile
        ) {
            return self.write_streaming_leaf(label, &desired_path, entry.size, |output| {
                bundle.copy_entry_with_cache(index, output, block_cache)
            });
        }
        let region = Region::from_bytes(bundle.read_entry_with_cache(index, block_cache)?);
        self.process_detected_region(label, region, desired_path, depth, detection)
    }

    fn process_legacy_entries(
        &mut self,
        label: &str,
        desired_path: &Path,
        depth: usize,
        bundle: &LegacyBundle,
    ) -> Result<()> {
        if bundle.entries.is_empty() {
            return Ok(());
        }
        let container = self.allocate_container_directory(desired_path)?;
        let next_depth = self.next_depth(depth)?;
        for index in 0..bundle.entries.len() {
            let entry = &bundle.entries[index];
            let child_label = self.nested_label(label, &entry.path)?;
            let result = self
                .charge_entry(entry.size)
                .and_then(|()| sanitize_archive_path(&entry.path, self.options.limits))
                .and_then(|path| {
                    let desired_path = join_relative_path_fallibly(
                        &container,
                        &path,
                        self.options.limits.maximum_path_bytes,
                        "nested extraction output path",
                    )?;
                    self.process_region(
                        &child_label,
                        bundle.entry_region(index)?,
                        desired_path,
                        next_depth,
                    )
                });
            if let Err(error) = result {
                self.record_failure(child_label, &error)?;
            }
        }
        Ok(())
    }

    fn write_region_leaf(&mut self, label: &str, path: &Path, region: &Region) -> Result<()> {
        self.write_streaming_leaf(label, path, region.len(), |output| {
            region.copy_range(0, region.len(), output)
        })
    }

    fn write_streaming_leaf<F>(
        &mut self,
        label: &str,
        desired_path: &Path,
        length: u64,
        copy: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut File) -> Result<u64>,
    {
        if length > self.options.limits.maximum_single_entry_bytes {
            return Err(Error::invalid_data(format!(
                "leaf is {length} bytes, exceeding limit {}",
                self.options.limits.maximum_single_entry_bytes
            )));
        }
        let relative = self.allocate_path(desired_path, ClaimKind::File)?;
        let output_path = join_relative_path_fallibly(
            &self.output_root,
            &relative,
            usize::MAX,
            "absolute extraction output path",
        )?;
        ensure_secure_parent(&self.output_root, &relative)?;
        match safe_file_state(&output_path)? {
            FileState::Regular if !self.options.overwrite_existing => {
                self.push_skipped(label, output_path)?;
                return Ok(());
            }
            FileState::Regular | FileState::Missing => {}
            FileState::Directory => {
                return Err(Error::invalid_data(format!(
                    "output file path is a directory: {}",
                    output_path.display()
                )));
            }
            FileState::Other => {
                return Err(Error::invalid_data(format!(
                    "output file path is not a regular file: {}",
                    output_path.display()
                )));
            }
        }

        let new_total = self
            .report
            .output_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("extracted output byte count overflowed"))?;
        if new_total > self.options.limits.maximum_output_bytes {
            return Err(Error::invalid_data(format!(
                "extracted output would exceed the {} byte limit",
                self.options.limits.maximum_output_bytes
            )));
        }

        let metadata_bytes =
            self.checked_report_metadata(label.len(), filesystem_path_byte_length(&output_path))?;
        let source = copy_extraction_label(label)?;
        reserve_report_entry(&mut self.report.extracted, "extraction records")?;
        // A no-clobber destination can appear between the initial check and
        // atomic publication. Reserve both possible report variants before
        // writing so success never creates a file and then fails to record it.
        reserve_report_entry(&mut self.report.skipped_existing, "extraction skips")?;
        let outcome = self.atomic_write(&output_path, length, copy)?;
        self.budget.report_metadata_bytes = metadata_bytes;
        if outcome == PersistOutcome::SkippedExisting {
            self.report.skipped_existing.push(ExtractionSkip {
                source,
                output_path,
            });
            return Ok(());
        }
        self.report.output_bytes = new_total;
        self.report.extracted.push(ExtractionRecord {
            source,
            output_path,
            bytes: length,
        });
        Ok(())
    }

    fn push_skipped(&mut self, label: &str, output_path: PathBuf) -> Result<()> {
        let metadata_bytes =
            self.checked_report_metadata(label.len(), filesystem_path_byte_length(&output_path))?;
        let source = copy_extraction_label(label)?;
        reserve_report_entry(&mut self.report.skipped_existing, "extraction skips")?;
        self.budget.report_metadata_bytes = metadata_bytes;
        self.report.skipped_existing.push(ExtractionSkip {
            source,
            output_path,
        });
        Ok(())
    }

    fn atomic_write<F>(
        &mut self,
        destination: &Path,
        expected: u64,
        copy: F,
    ) -> Result<PersistOutcome>
    where
        F: FnOnce(&mut File) -> Result<u64>,
    {
        let parent = destination.parent().ok_or_else(|| {
            Error::invalid_data(format!(
                "output path has no parent: {}",
                destination.display()
            ))
        })?;
        let temporary_path = self.next_temporary_path(parent)?;
        let mut temporary = TemporaryFile::create(temporary_path)?;
        let written = copy(temporary.file_mut())?;
        if written != expected {
            return Err(Error::invalid_data(format!(
                "leaf copy wrote {written} bytes; expected {expected}"
            )));
        }
        temporary.file_mut().flush()?;
        temporary.file_mut().sync_all()?;
        temporary.close()?;

        match safe_file_state(destination)? {
            FileState::Regular if !self.options.overwrite_existing => {
                return Ok(PersistOutcome::SkippedExisting);
            }
            FileState::Regular | FileState::Missing => {}
            FileState::Directory => {
                return Err(Error::invalid_data(format!(
                    "output file path became a directory: {}",
                    destination.display()
                )));
            }
            FileState::Other => {
                return Err(Error::invalid_data(format!(
                    "output file path became a non-regular file: {}",
                    destination.display()
                )));
            }
        }
        temporary.persist(destination, self.options.overwrite_existing)
    }

    fn allocate_container_directory(&mut self, source_path: &Path) -> Result<PathBuf> {
        let file_name = source_path.file_name().ok_or_else(|| {
            Error::invalid_data(format!(
                "container path has no file name: {}",
                source_path.display()
            ))
        })?;
        let file_name = file_name
            .to_str()
            .ok_or_else(|| Error::invalid_data("container output file name is not valid UTF-8"))?;
        let unpacked_length = file_name
            .len()
            .checked_add("_unpacked".len())
            .ok_or_else(|| Error::invalid_data("container output file name length overflowed"))?;
        if unpacked_length > MAX_PORTABLE_COMPONENT_BYTES {
            return Err(Error::invalid_data(format!(
                "container output component is {unpacked_length} bytes, exceeding portable limit {MAX_PORTABLE_COMPONENT_BYTES}"
            )));
        }
        let mut unpacked_name = String::new();
        unpacked_name
            .try_reserve_exact(unpacked_length)
            .map_err(|error| {
                Error::invalid_data(format!(
                    "cannot allocate container output component: {error}"
                ))
            })?;
        unpacked_name.push_str(file_name);
        unpacked_name.push_str("_unpacked");
        let desired = join_relative_path_fallibly(
            source_path.parent().unwrap_or_else(|| Path::new("")),
            Path::new(&unpacked_name),
            self.options.limits.maximum_path_bytes,
            "container output path",
        )?;
        let relative = self.allocate_path(&desired, ClaimKind::Directory)?;
        ensure_secure_relative_directory(&self.output_root, &relative)?;
        Ok(relative)
    }

    fn allocate_path(&mut self, desired: &Path, kind: ClaimKind) -> Result<PathBuf> {
        self.allocate_path_with_probe(desired, kind, || {})
    }

    fn allocate_path_with_probe(
        &mut self,
        desired: &Path,
        kind: ClaimKind,
        mut probe: impl FnMut(),
    ) -> Result<PathBuf> {
        let resolved = self.resolve_parent_collisions(desired)?;
        let cursor_key = self.collision_cursor_key(&resolved, CollisionCursorKind::Leaf(kind))?;
        let mut collision_index = self
            .collision_cursors
            .get(&cursor_key)
            .copied()
            .unwrap_or(0);
        let mut needs_cursor = collision_index != 0;
        loop {
            probe();
            let candidate = if collision_index == 0 {
                copy_path_fallibly(&resolved, "resolved extraction output path")?
            } else {
                suffixed_path(
                    &resolved,
                    collision_index,
                    self.options.limits.maximum_path_bytes,
                )?
            };
            let (key, claimed_path_total) =
                portable_key(&candidate, self.options.limits, &self.budget.paths)?;
            if self.claims.contains_key(&key) {
                needs_cursor = true;
                collision_index = collision_index
                    .checked_add(1)
                    .ok_or_else(|| Error::invalid_data("output collision counter overflowed"))?;
                continue;
            }
            let absolute = join_relative_path_fallibly(
                &self.output_root,
                &candidate,
                usize::MAX,
                "absolute extraction collision path",
            )?;
            let usable = match safe_path_kind(&absolute)? {
                ExistingKind::Symlink => {
                    return Err(Error::invalid_data(format!(
                        "refusing symbolic-link output path: {}",
                        absolute.display()
                    )));
                }
                ExistingKind::Directory => kind == ClaimKind::Directory,
                ExistingKind::Regular => kind == ClaimKind::File,
                ExistingKind::Other => false,
                ExistingKind::Missing => true,
            };
            if usable {
                let cursor = if needs_cursor {
                    let next = collision_index.checked_add(1).ok_or_else(|| {
                        Error::invalid_data("output collision counter overflowed")
                    })?;
                    Some((cursor_key, next))
                } else {
                    None
                };
                self.commit_path_claim(key, kind, claimed_path_total, cursor)?;
                return Ok(candidate);
            }
            needs_cursor = true;
            collision_index = collision_index
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("output collision counter overflowed"))?;
        }
    }

    fn resolve_parent_collisions(&mut self, desired: &Path) -> Result<PathBuf> {
        self.resolve_parent_collisions_with_probe(desired, || {})
    }

    fn resolve_parent_collisions_with_probe(
        &mut self,
        desired: &Path,
        mut probe: impl FnMut(),
    ) -> Result<PathBuf> {
        let component_count = desired.components().count();
        let mut components = Vec::new();
        components
            .try_reserve_exact(component_count)
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate output path components: {error}"))
            })?;
        for component in desired.components() {
            let Component::Normal(value) = component else {
                return Err(Error::invalid_data("output path is not strictly relative"));
            };
            let value = value
                .to_str()
                .ok_or_else(|| Error::invalid_data("output path component is not valid UTF-8"))?;
            components.push(copy_string_fallibly(value, "output path component")?);
        }
        if components.is_empty() {
            return Err(Error::invalid_data("output path is empty"));
        }
        for index in 0..components.len().saturating_sub(1) {
            let original = copy_string_fallibly(&components[index], "original output component")?;
            let desired_parent = path_from_components(
                &components[..=index],
                self.options.limits.maximum_path_bytes,
                "desired extraction output parent",
            )?;
            let cursor_key =
                self.collision_cursor_key(&desired_parent, CollisionCursorKind::Parent)?;
            let mut collision_index = self
                .collision_cursors
                .get(&cursor_key)
                .copied()
                .unwrap_or(0);
            loop {
                probe();
                if collision_index != 0 {
                    components[index] = suffixed_component(&original, collision_index)?;
                }
                let prefix = path_from_components(
                    &components[..=index],
                    self.options.limits.maximum_path_bytes,
                    "extraction output parent",
                )?;
                let absolute = join_relative_path_fallibly(
                    &self.output_root,
                    &prefix,
                    usize::MAX,
                    "absolute extraction output parent",
                )?;
                let claim = self.claim_kind(&prefix)?;
                match safe_path_kind(&absolute)? {
                    ExistingKind::Symlink => {
                        return Err(Error::invalid_data(format!(
                            "refusing symbolic-link output parent: {}",
                            absolute.display()
                        )));
                    }
                    ExistingKind::Directory
                        if claim.is_none() || claim == Some(ClaimKind::Directory) =>
                    {
                        if collision_index != 0 {
                            self.retain_collision_cursor(cursor_key, collision_index)?;
                        }
                        break;
                    }
                    ExistingKind::Missing if claim != Some(ClaimKind::File) => {
                        if collision_index != 0 {
                            self.retain_collision_cursor(cursor_key, collision_index)?;
                        }
                        break;
                    }
                    ExistingKind::Regular
                    | ExistingKind::Other
                    | ExistingKind::Directory
                    | ExistingKind::Missing => {}
                }
                collision_index = collision_index.checked_add(1).ok_or_else(|| {
                    Error::invalid_data("output parent collision counter overflowed")
                })?;
            }
        }
        path_from_components(
            &components,
            self.options.limits.maximum_path_bytes,
            "resolved extraction output path",
        )
    }

    fn collision_cursor_key(
        &self,
        path: &Path,
        kind: CollisionCursorKind,
    ) -> Result<CollisionCursorKey> {
        let (path, _) = portable_key(path, self.options.limits, &self.budget.paths)?;
        Ok(CollisionCursorKey { path, kind })
    }

    fn commit_path_claim(
        &mut self,
        path: PathBuf,
        kind: ClaimKind,
        claimed_path_total: usize,
        cursor: Option<(CollisionCursorKey, u64)>,
    ) -> Result<()> {
        let cursor_is_new = cursor
            .as_ref()
            .is_some_and(|(key, _)| !self.collision_cursors.contains_key(key));
        let retained_total = if cursor_is_new {
            self.checked_cursor_total(
                claimed_path_total,
                cursor
                    .as_ref()
                    .expect("new cursor is present")
                    .0
                    .path
                    .as_path(),
            )?
        } else {
            claimed_path_total
        };
        self.claims.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow extraction path claims: {error}"))
        })?;
        if cursor_is_new {
            self.collision_cursors.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow extraction collision cursors: {error}"))
            })?;
        }
        self.budget.paths.bytes = retained_total;
        let previous = self.claims.insert(path, kind);
        debug_assert!(previous.is_none());
        if let Some((key, value)) = cursor {
            self.collision_cursors.insert(key, value);
        }
        Ok(())
    }

    fn retain_collision_cursor(&mut self, key: CollisionCursorKey, value: u64) -> Result<()> {
        if let Some(cursor) = self.collision_cursors.get_mut(&key) {
            *cursor = value;
            return Ok(());
        }
        let retained_total = self.checked_cursor_total(self.budget.paths.bytes, &key.path)?;
        self.collision_cursors.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow extraction collision cursors: {error}"))
        })?;
        self.budget.paths.bytes = retained_total;
        self.collision_cursors.insert(key, value);
        Ok(())
    }

    fn checked_cursor_total(&self, retained: usize, path: &Path) -> Result<usize> {
        let bytes = filesystem_path_byte_length(path);
        let total = retained
            .checked_add(bytes)
            .ok_or_else(|| Error::invalid_data("extraction path byte count overflowed"))?;
        if total > self.options.limits.maximum_total_path_bytes {
            return Err(Error::invalid_data(format!(
                "extraction paths total {total} bytes, exceeding limit {}",
                self.options.limits.maximum_total_path_bytes
            )));
        }
        Ok(total)
    }

    fn claim_kind(&self, path: &Path) -> Result<Option<ClaimKind>> {
        let (key, _) = portable_key(path, self.options.limits, &self.budget.paths)?;
        Ok(self.claims.get(&key).copied())
    }

    fn charge_entry(&mut self, length: u64) -> Result<()> {
        self.budget.entries = self
            .budget
            .entries
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("extracted entry count overflowed"))?;
        if self.budget.entries > self.options.limits.maximum_entries {
            return Err(Error::invalid_data(format!(
                "extraction exceeds {} container entries",
                self.options.limits.maximum_entries
            )));
        }
        if length > self.options.limits.maximum_single_entry_bytes {
            return Err(Error::invalid_data(format!(
                "container entry is {length} bytes, exceeding limit {}",
                self.options.limits.maximum_single_entry_bytes
            )));
        }
        self.charge_expansion(length)
    }

    fn charge_expansion(&mut self, length: u64) -> Result<()> {
        let expanded = self
            .budget
            .expanded_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("expanded byte count overflowed"))?;
        if expanded > self.options.limits.maximum_expanded_bytes {
            return Err(Error::invalid_data(format!(
                "extraction expands to {expanded} bytes, exceeding limit {}",
                self.options.limits.maximum_expanded_bytes
            )));
        }
        self.budget.expanded_bytes = expanded;
        Ok(())
    }

    fn next_depth(&self, depth: usize) -> Result<usize> {
        let next = depth
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("container depth overflowed"))?;
        if next > self.options.limits.maximum_nesting_depth {
            return Err(Error::invalid_data(format!(
                "extraction exceeds {} container layers",
                self.options.limits.maximum_nesting_depth
            )));
        }
        Ok(next)
    }

    fn nested_label(&mut self, parent: &str, child: &str) -> Result<String> {
        nested_label(parent, child, self.options.limits, &mut self.budget.paths)
    }

    fn stream_compression_limits(&self) -> Result<CompressionLimits> {
        let remaining = self.remaining_expansion()?;
        Ok(CompressionLimits {
            maximum_output_bytes: self
                .options
                .limits
                .compression
                .maximum_output_bytes
                .min(self.options.limits.maximum_single_entry_bytes)
                .min(remaining),
            ..self.options.limits.compression
        })
    }

    fn zip_compression_limits(&self) -> Result<CompressionLimits> {
        let remaining_entries = self
            .options
            .limits
            .maximum_entries
            .saturating_sub(self.budget.entries);
        Ok(CompressionLimits {
            maximum_zip_entries: self
                .options
                .limits
                .compression
                .maximum_zip_entries
                .min(remaining_entries),
            maximum_zip_path_bytes: self
                .options
                .limits
                .compression
                .maximum_zip_path_bytes
                .min(self.options.limits.maximum_path_bytes),
            maximum_zip_entry_bytes: self
                .options
                .limits
                .compression
                .maximum_zip_entry_bytes
                .min(self.options.limits.maximum_single_entry_bytes),
            maximum_zip_total_bytes: self
                .options
                .limits
                .compression
                .maximum_zip_total_bytes
                .min(self.remaining_expansion()?),
            ..self.options.limits.compression
        })
    }

    fn remaining_expansion(&self) -> Result<u64> {
        self.options
            .limits
            .maximum_expanded_bytes
            .checked_sub(self.budget.expanded_bytes)
            .ok_or_else(|| Error::invalid_data("expanded byte budget was already exceeded"))
    }

    fn next_temporary_path(&mut self, directory: &Path) -> Result<PathBuf> {
        loop {
            self.temporary_sequence = self
                .temporary_sequence
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("temporary file counter overflowed"))?;
            let mut name = FallibleFormatString::default();
            fmt::write(
                &mut name,
                format_args!(
                    ".unity-rs-tmp-{}-{}",
                    std::process::id(),
                    self.temporary_sequence
                ),
            )
            .map_err(|_| Error::invalid_data("cannot allocate extraction temporary file name"))?;
            let candidate = join_relative_path_fallibly(
                directory,
                Path::new(&name.value),
                usize::MAX,
                "extraction temporary path",
            )?;
            if matches!(safe_path_kind(&candidate)?, ExistingKind::Missing) {
                return Ok(candidate);
            }
        }
    }

    fn record_failure(&mut self, source: String, error: &Error) -> Result<()> {
        let error_length = extraction_error_length(error)?;
        let metadata_bytes = self.checked_report_metadata(source.len(), error_length)?;
        let error = format_extraction_error(error)?;
        reserve_report_entry(&mut self.report.failures, "extraction failures")?;
        self.budget.report_metadata_bytes = metadata_bytes;
        self.report
            .failures
            .push(ExtractionFailure { source, error });
        Ok(())
    }

    fn checked_report_metadata(&self, first: usize, second: usize) -> Result<usize> {
        let additional = first
            .checked_add(second)
            .ok_or_else(|| Error::invalid_data("extraction report metadata size overflowed"))?;
        let next = self
            .budget
            .report_metadata_bytes
            .checked_add(additional)
            .ok_or_else(|| Error::invalid_data("extraction report metadata total overflowed"))?;
        if next > self.options.limits.maximum_metadata_bytes {
            return Err(Error::invalid_data(format!(
                "extraction report metadata requires {next} bytes, exceeding limit {}",
                self.options.limits.maximum_metadata_bytes
            )));
        }
        Ok(next)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistOutcome {
    Written,
    SkippedExisting,
}

struct HeaderProbe {
    header: Vec<u8>,
    written: u64,
}

impl HeaderProbe {
    fn new() -> Self {
        Self {
            header: Vec::with_capacity(HEADER_SCAN_LENGTH),
            written: 0,
        }
    }
}

impl Write for HeaderProbe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.written = self
            .written
            .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "entry length overflowed"))?;
        let wanted = HEADER_SCAN_LENGTH.saturating_sub(self.header.len());
        self.header
            .extend_from_slice(&bytes[..bytes.len().min(wanted)]);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct TemporaryFile {
    path: PathBuf,
    file: Option<File>,
    persisted: bool,
}

impl TemporaryFile {
    fn create(path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Some(file),
            persisted: false,
        })
    }

    fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("temporary file is still open")
    }

    fn close(&mut self) -> Result<()> {
        self.file
            .take()
            .ok_or_else(|| Error::invalid_data("temporary output file was already closed"))?;
        Ok(())
    }

    fn persist(&mut self, destination: &Path, overwrite: bool) -> Result<PersistOutcome> {
        if !overwrite {
            return match fs::hard_link(&self.path, destination) {
                Ok(()) => {
                    // The destination is committed once the hard-link exists.
                    // A temporary-link cleanup failure is retried by Drop and
                    // must not be reported as a failed extraction.
                    self.persisted = fs::remove_file(&self.path).is_ok();
                    Ok(PersistOutcome::Written)
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    Ok(PersistOutcome::SkippedExisting)
                }
                Err(error) => Err(error.into()),
            };
        }
        match fs::rename(&self.path, destination) {
            Ok(()) => {
                self.persisted = true;
                Ok(PersistOutcome::Written)
            }
            Err(error)
                if overwrite
                    && matches!(
                        error.kind(),
                        io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
                    ) =>
            {
                self.persist_with_backup(destination, error)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn persist_with_backup(
        &mut self,
        destination: &Path,
        original_error: io::Error,
    ) -> Result<PersistOutcome> {
        let backup = extraction_backup_path(&self.path)?;
        if !matches!(safe_path_kind(&backup)?, ExistingKind::Missing) {
            return Err(original_error.into());
        }
        fs::rename(destination, &backup).map_err(|_| original_error)?;
        match fs::rename(&self.path, destination) {
            Ok(()) => {
                // The new destination is committed. Point Drop at the backup
                // of the previous file so cleanup can be retried without
                // changing the publication result.
                self.path = backup;
                self.persisted = fs::remove_file(&self.path).is_ok();
                Ok(PersistOutcome::Written)
            }
            Err(error) => {
                let _ = fs::rename(&backup, destination);
                Err(error.into())
            }
        }
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileState {
    Missing,
    Regular,
    Directory,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingKind {
    Missing,
    Directory,
    Symlink,
    Regular,
    Other,
}

fn safe_file_state(path: &Path) -> Result<FileState> {
    Ok(match safe_path_kind(path)? {
        ExistingKind::Missing => FileState::Missing,
        ExistingKind::Directory => FileState::Directory,
        ExistingKind::Symlink => {
            return Err(Error::invalid_data(format!(
                "refusing symbolic-link output file: {}",
                path.display()
            )));
        }
        ExistingKind::Regular => FileState::Regular,
        ExistingKind::Other => FileState::Other,
    })
}

fn safe_path_kind(path: &Path) -> Result<ExistingKind> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(ExistingKind::Symlink),
        Ok(metadata) if metadata.is_dir() => Ok(ExistingKind::Directory),
        Ok(metadata) if metadata.is_file() => Ok(ExistingKind::Regular),
        Ok(_) => Ok(ExistingKind::Other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ExistingKind::Missing),
        Err(error) => Err(error.into()),
    }
}

fn filesystem_path_byte_length(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

fn copy_path_fallibly(path: &Path, label: &str) -> Result<PathBuf> {
    let length = filesystem_path_byte_length(path);
    let mut copy = PathBuf::new();
    copy.try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {label}: {error}")))?;
    copy.push(path);
    Ok(copy)
}

fn join_relative_path_fallibly(
    parent: &Path,
    child: &Path,
    maximum_bytes: usize,
    label: &str,
) -> Result<PathBuf> {
    if child.is_absolute() {
        return Err(Error::invalid_data(format!("{label} child is absolute")));
    }
    let parent_bytes = parent.as_os_str().as_encoded_bytes();
    let child_bytes = child.as_os_str().as_encoded_bytes();
    if child_bytes.is_empty() {
        return copy_path_fallibly(parent, label);
    }
    let separator_length =
        usize::from(!parent_bytes.is_empty() && !matches!(parent_bytes.last(), Some(b'/' | b'\\')));
    let length = parent_bytes
        .len()
        .checked_add(separator_length)
        .and_then(|length| length.checked_add(child_bytes.len()))
        .ok_or_else(|| Error::invalid_data(format!("{label} length overflowed")))?;
    if length > maximum_bytes {
        return Err(Error::invalid_data(format!(
            "{label} is {length} bytes, exceeding limit {maximum_bytes}"
        )));
    }
    let mut path = PathBuf::new();
    path.try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {label}: {error}")))?;
    path.push(parent);
    path.push(child);
    if filesystem_path_byte_length(&path) > length {
        return Err(Error::invalid_data(format!(
            "{label} grew beyond its checked allocation"
        )));
    }
    Ok(path)
}

fn copy_filesystem_path(
    path: &Path,
    limits: ExtractionLimits,
    budget: &mut ExtractionPathBudget,
) -> Result<PathBuf> {
    let length = filesystem_path_byte_length(path);
    budget.charge_path(length, limits)?;
    let mut copy = PathBuf::new();
    copy.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate extraction input path: {error}"))
    })?;
    copy.push(path);
    Ok(copy)
}

fn join_filesystem_path(
    parent: &Path,
    child: &OsStr,
    limits: ExtractionLimits,
    budget: &mut ExtractionPathBudget,
) -> Result<PathBuf> {
    let parent_bytes = parent.as_os_str().as_encoded_bytes();
    let separator_length =
        usize::from(!parent_bytes.is_empty() && !matches!(parent_bytes.last(), Some(b'/' | b'\\')));
    let length = parent_bytes
        .len()
        .checked_add(separator_length)
        .and_then(|length| length.checked_add(child.as_encoded_bytes().len()))
        .ok_or_else(|| Error::invalid_data("extraction input path length overflowed"))?;
    budget.charge_path(length, limits)?;
    let mut path = PathBuf::new();
    path.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate extraction input path: {error}"))
    })?;
    path.push(parent);
    path.push(child);
    if filesystem_path_byte_length(&path) > length {
        return Err(Error::invalid_data(
            "extraction input path grew beyond its checked allocation",
        ));
    }
    Ok(path)
}

fn filesystem_path_label(
    path: &Path,
    limits: ExtractionLimits,
    budget: &mut ExtractionPathBudget,
) -> Result<String> {
    let encoded_length = filesystem_path_byte_length(path);
    let utf8_length = lossy_os_str_utf8_length(path.as_os_str())?;
    if utf8_length > limits.maximum_path_bytes {
        return Err(Error::invalid_data(format!(
            "input filesystem label is {} UTF-8 bytes, exceeding limit {}",
            utf8_length, limits.maximum_path_bytes
        )));
    }
    if utf8_length > encoded_length {
        budget.charge_additional(utf8_length - encoded_length, limits)?;
    }
    copy_os_str_with_replacement(path.as_os_str(), utf8_length, "extraction input label")
}

fn collect_regular_files(
    root: &Path,
    limits: ExtractionLimits,
    path_budget: &mut ExtractionPathBudget,
) -> Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    directories.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate extraction directory queue: {error}"
        ))
    })?;
    directories.push(copy_filesystem_path(root, limits, path_budget)?);
    let mut files = Vec::new();
    let mut entry_count = 0_usize;
    while let Some(directory) = directories.pop() {
        let children =
            extraction_directory_children(&directory, limits, path_budget, &mut entry_count)?;
        for (path, file_type) in children.into_iter().rev() {
            if file_type.is_dir() {
                directories.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow extraction directory queue: {error}"))
                })?;
                directories.push(path);
            } else if file_type.is_file() {
                if files.len() >= limits.maximum_input_files {
                    return Err(Error::invalid_data(format!(
                        "extraction directory traversal exceeds {} regular files",
                        limits.maximum_input_files
                    )));
                }
                files.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow extraction input list: {error}"))
                })?;
                files.push(path);
            }
        }
    }
    files.sort_unstable();
    Ok(files)
}

fn extraction_directory_children(
    directory: &Path,
    limits: ExtractionLimits,
    path_budget: &mut ExtractionPathBudget,
    entry_count: &mut usize,
) -> Result<Vec<(PathBuf, fs::FileType)>> {
    let mut children = Vec::new();
    for child in fs::read_dir(directory)? {
        *entry_count = entry_count
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("extraction directory entry count overflowed"))?;
        if *entry_count > limits.maximum_entries {
            return Err(Error::invalid_data(format!(
                "extraction directory traversal exceeds {} entries",
                limits.maximum_entries
            )));
        }
        let child = child?;
        let file_type = child.file_type()?;
        if !file_type.is_dir() && !file_type.is_file() {
            continue;
        }
        let path = join_filesystem_path(directory, &child.file_name(), limits, path_budget)?;
        children.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate extraction directory entries: {error}"
            ))
        })?;
        children.push((path, file_type));
    }
    children.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(children)
}

fn detect_region(region: &Region) -> Result<FileDetection> {
    let scan = u64::try_from(HEADER_SCAN_LENGTH).expect("scan length fits in u64");
    let length =
        usize::try_from(region.len().min(scan)).expect("bounded detection header fits in usize");
    let mut header = Vec::new();
    header.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate extraction header: {error}"))
    })?;
    header.resize(length, 0);
    region.read_exact_at(0, &mut header)?;
    Ok(detect_file_type(&header, region.len()))
}

fn validate_limits(limits: ExtractionLimits) -> Result<()> {
    if limits.maximum_input_files == 0
        || limits.maximum_entries == 0
        || limits.maximum_single_entry_bytes == 0
        || limits.maximum_expanded_bytes == 0
        || limits.maximum_output_bytes == 0
        || limits.maximum_path_bytes == 0
        || limits.maximum_total_path_bytes == 0
    {
        return Err(Error::invalid_data(
            "extraction count and byte limits must be greater than zero",
        ));
    }
    Ok(())
}

fn sanitize_archive_path(path: &str, limits: ExtractionLimits) -> Result<PathBuf> {
    if path
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(byte, b'/' | b'\\'))
    {
        return Err(Error::invalid_data(format!(
            "archive entry path must be relative: {path:?}"
        )));
    }
    if path
        .split(['/', '\\'])
        .next()
        .is_some_and(is_windows_drive_component)
    {
        return Err(Error::invalid_data(format!(
            "archive entry path has a Windows drive prefix: {path:?}"
        )));
    }
    let mut result = PathBuf::new();
    for component in path.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." => {
                return Err(Error::invalid_data(format!(
                    "archive entry path contains '..': {path:?}"
                )));
            }
            value => result.push(sanitize_component(value)?),
        }
    }
    validate_relative_output_path(&result, limits)?;
    Ok(result)
}

fn sanitize_filesystem_relative_path(path: &Path, limits: ExtractionLimits) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                result.push(sanitize_os_component(value)?);
            }
            _ => {
                return Err(Error::invalid_data(format!(
                    "input relative path is not enclosed: {}",
                    path.display()
                )));
            }
        }
    }
    validate_relative_output_path(&result, limits)?;
    Ok(result)
}

fn sanitize_component_path(component: &str, limits: ExtractionLimits) -> Result<PathBuf> {
    let result = PathBuf::from(sanitize_component(component)?);
    validate_relative_output_path(&result, limits)?;
    Ok(result)
}

fn sanitize_os_component_path(component: &OsStr, limits: ExtractionLimits) -> Result<PathBuf> {
    let result = PathBuf::from(sanitize_os_component(component)?);
    validate_relative_output_path(&result, limits)?;
    Ok(result)
}

fn sanitize_component(component: &str) -> Result<String> {
    // Archive entry names are untrusted. Do not reserve the full declared
    // component before applying the portable 240-byte ceiling: a malformed
    // entry can otherwise force a large allocation only to be rejected below.
    let mut sanitized = String::new();
    for character in component.chars() {
        push_sanitized_character(&mut sanitized, character)?;
    }
    finish_sanitized_component(sanitized)
}

fn sanitize_os_component(component: &OsStr) -> Result<String> {
    let mut sanitized = String::new();
    for_each_os_str_char_lossy(component, |character| {
        push_sanitized_character(&mut sanitized, character)
    })?;
    finish_sanitized_component(sanitized)
}

fn push_sanitized_character(sanitized: &mut String, character: char) -> Result<()> {
    let sanitized_character =
        if character.is_control() || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            '_'
        } else {
            character
        };
    let next_length = sanitized
        .len()
        .checked_add(sanitized_character.len_utf8())
        .ok_or_else(|| Error::invalid_data("path component length overflowed"))?;
    if next_length > MAX_PORTABLE_COMPONENT_BYTES {
        return Err(Error::invalid_data(format!(
            "path component is at least {next_length} bytes, exceeding portable limit {MAX_PORTABLE_COMPONENT_BYTES}",
        )));
    }
    sanitized
        .try_reserve_exact(sanitized_character.len_utf8())
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate sanitized path component: {error}"))
        })?;
    sanitized.push(sanitized_character);
    Ok(())
}

fn finish_sanitized_component(mut sanitized: String) -> Result<String> {
    while sanitized.ends_with([' ', '.']) {
        sanitized.pop();
        sanitized.push('_');
    }
    if sanitized.is_empty() || matches!(sanitized.as_str(), "." | "..") {
        sanitized.try_reserve_exact(1).map_err(|error| {
            Error::invalid_data(format!("cannot allocate sanitized path component: {error}"))
        })?;
        sanitized.push('_');
    }
    if is_windows_reserved_name(&sanitized) {
        if sanitized.len() == MAX_PORTABLE_COMPONENT_BYTES {
            return Err(Error::invalid_data(format!(
                "reserved path component needs a prefix beyond portable limit {MAX_PORTABLE_COMPONENT_BYTES}",
            )));
        }
        sanitized.try_reserve_exact(1).map_err(|error| {
            Error::invalid_data(format!("cannot allocate sanitized path component: {error}"))
        })?;
        sanitized.insert(0, '_');
    }
    Ok(sanitized)
}

fn validate_relative_output_path(path: &Path, limits: ExtractionLimits) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(Error::invalid_data(
            "sanitized extraction path is empty or absolute",
        ));
    }
    let length = path
        .to_str()
        .ok_or_else(|| Error::invalid_data("output path is not valid UTF-8"))?
        .len();
    if length > limits.maximum_path_bytes {
        return Err(Error::invalid_data(format!(
            "output path is {length} bytes, exceeding limit {}",
            limits.maximum_path_bytes
        )));
    }
    Ok(())
}

fn is_windows_drive_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_windows_reserved_name(component: &str) -> bool {
    let base = component.split('.').next().unwrap_or(component);
    let upper = base.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
}

fn decoded_leaf_path(path: &Path, wrapper: &str, maximum_bytes: usize) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        Error::invalid_data(format!("wrapped path has no file name: {}", path.display()))
    })?;
    let value = file_name
        .to_str()
        .ok_or_else(|| Error::invalid_data("wrapped output file name is not valid UTF-8"))?;
    let extensions: &[&str] = match wrapper {
        "gzip" => &[".gzip", ".gz"],
        "brotli" => &[".brotli", ".br"],
        _ => &[],
    };
    let decoded = extensions
        .iter()
        .find_map(|extension| {
            let prefix_length = value.len().checked_sub(extension.len())?;
            value[prefix_length..]
                .eq_ignore_ascii_case(extension)
                .then_some(&value[..prefix_length])
        })
        .filter(|name| !name.is_empty());
    let decoded = if let Some(decoded) = decoded {
        copy_string_fallibly(decoded, "decoded wrapper file name")?
    } else {
        let length = value
            .len()
            .checked_add(".decoded".len())
            .ok_or_else(|| Error::invalid_data("decoded wrapper name length overflowed"))?;
        if length > MAX_PORTABLE_COMPONENT_BYTES {
            return Err(Error::invalid_data(format!(
                "decoded wrapper component is {length} bytes, exceeding portable limit {MAX_PORTABLE_COMPONENT_BYTES}"
            )));
        }
        let mut decoded = String::new();
        decoded.try_reserve_exact(length).map_err(|error| {
            Error::invalid_data(format!("cannot allocate decoded wrapper name: {error}"))
        })?;
        decoded.push_str(value);
        decoded.push_str(".decoded");
        decoded
    };
    join_relative_path_fallibly(
        path.parent().unwrap_or_else(|| Path::new("")),
        Path::new(&decoded),
        maximum_bytes,
        "decoded wrapper output path",
    )
}

fn suffixed_path(path: &Path, index: u64, maximum_bytes: usize) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        Error::invalid_data(format!(
            "collision path has no file name: {}",
            path.display()
        ))
    })?;
    let component = suffixed_component(
        file_name
            .to_str()
            .ok_or_else(|| Error::invalid_data("collision file name is not valid UTF-8"))?,
        index,
    )?;
    join_relative_path_fallibly(
        path.parent().unwrap_or_else(|| Path::new("")),
        Path::new(&component),
        maximum_bytes,
        "suffixed extraction output path",
    )
}

fn suffixed_component(component: &str, index: u64) -> Result<String> {
    let path = Path::new(component);
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_str()
        .ok_or_else(|| Error::invalid_data("collision stem is not valid UTF-8"))?;
    let extension = path
        .extension()
        .map(|value| {
            value
                .to_str()
                .ok_or_else(|| Error::invalid_data("collision extension is not valid UTF-8"))
        })
        .transpose()?;
    let mut digits = 1_usize;
    let mut remaining = index;
    while remaining >= 10 {
        remaining /= 10;
        digits += 1;
    }
    let length = stem
        .len()
        .checked_add(1)
        .and_then(|length| length.checked_add(digits))
        .and_then(|length| {
            extension.map_or(Some(length), |extension| {
                length
                    .checked_add(1)
                    .and_then(|length| length.checked_add(extension.len()))
            })
        })
        .ok_or_else(|| Error::invalid_data("collision suffix length overflowed"))?;
    if length > MAX_PORTABLE_COMPONENT_BYTES {
        return Err(Error::invalid_data(
            "collision suffix makes output component too long",
        ));
    }
    let mut output = String::new();
    output.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate collision output component: {error}"
        ))
    })?;
    output.push_str(stem);
    output.push('~');
    fmt::write(&mut output, format_args!("{index}"))
        .map_err(|_| Error::invalid_data("cannot format collision suffix"))?;
    if let Some(extension) = extension {
        output.push('.');
        output.push_str(extension);
    }
    debug_assert_eq!(output.len(), length);
    Ok(output)
}

fn copy_extraction_label(value: &str) -> Result<String> {
    copy_string_fallibly(value, "extraction source label")
}

fn extraction_backup_path(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| Error::invalid_data("extraction temporary file name is not valid UTF-8"))?;
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| Error::invalid_data("extraction temporary file stem is not valid UTF-8"))?;
    let length = stem
        .len()
        .checked_add(".replace-backup".len())
        .ok_or_else(|| Error::invalid_data("extraction backup name length overflowed"))?;
    let mut name = String::new();
    name.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate extraction backup name: {error}"))
    })?;
    name.push_str(stem);
    name.push_str(".replace-backup");
    join_relative_path_fallibly(
        path.parent().unwrap_or_else(|| Path::new("")),
        Path::new(&name),
        usize::MAX,
        "extraction replacement backup path",
    )
}

fn copy_string_fallibly(value: &str, label: &str) -> Result<String> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {label}: {error}")))?;
    copy.push_str(value);
    Ok(copy)
}

fn path_from_components(
    components: &[String],
    maximum_bytes: usize,
    label: &str,
) -> Result<PathBuf> {
    let separators = components.len().saturating_sub(1);
    let length = components
        .iter()
        .try_fold(separators, |length, component| {
            length
                .checked_add(component.len())
                .ok_or_else(|| Error::invalid_data(format!("{label} length overflowed")))
        })?;
    if length > maximum_bytes {
        return Err(Error::invalid_data(format!(
            "{label} is {length} bytes, exceeding limit {maximum_bytes}"
        )));
    }
    let mut path = String::new();
    path.try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {label}: {error}")))?;
    for (index, component) in components.iter().enumerate() {
        if index != 0 {
            path.push(std::path::MAIN_SEPARATOR);
        }
        path.push_str(component);
    }
    debug_assert_eq!(path.len(), length);
    Ok(PathBuf::from(path))
}

fn reserve_report_entry<T>(values: &mut Vec<T>, label: &str) -> Result<()> {
    values
        .try_reserve(1)
        .map_err(|error| Error::invalid_data(format!("cannot grow {label}: {error}")))
}

#[derive(Default)]
struct FallibleFormatString {
    value: String,
}

impl fmt::Write for FallibleFormatString {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.value
            .try_reserve(value.len())
            .map_err(|_| fmt::Error)?;
        self.value.push_str(value);
        Ok(())
    }
}

#[derive(Default)]
struct FallibleFormatLength {
    length: usize,
}

impl fmt::Write for FallibleFormatLength {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.length = self.length.checked_add(value.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

fn extraction_error_length(error: &Error) -> Result<usize> {
    let mut output = FallibleFormatLength::default();
    fmt::write(&mut output, format_args!("{error}"))
        .map_err(|_| Error::invalid_data("extraction failure message length overflowed"))?;
    Ok(output.length)
}

fn format_extraction_error(error: &Error) -> Result<String> {
    let length = extraction_error_length(error)?;
    let mut value = String::new();
    value
        .try_reserve_exact(length)
        .map_err(|_| Error::invalid_data("cannot allocate extraction failure message"))?;
    let mut output = FallibleFormatString { value };
    fmt::write(&mut output, format_args!("{error}"))
        .map_err(|_| Error::invalid_data("cannot allocate extraction failure message"))?;
    debug_assert_eq!(output.value.len(), length);
    Ok(output.value)
}

fn nested_label(
    parent: &str,
    child: &str,
    limits: ExtractionLimits,
    budget: &mut ExtractionPathBudget,
) -> Result<String> {
    let length = parent
        .len()
        .checked_add(2)
        .and_then(|length| length.checked_add(child.len()))
        .ok_or_else(|| Error::invalid_data("nested extraction label length overflowed"))?;
    budget.charge_path(length, limits)?;
    let mut label = String::new();
    label.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate nested extraction label: {error}"))
    })?;
    label.push_str(parent);
    label.push_str("::");
    for character in child.chars() {
        label.push(if character == '\\' { '/' } else { character });
    }
    debug_assert_eq!(label.len(), length);
    Ok(label)
}

/// The form two paths share when they name the same file on a
/// case-insensitive filesystem.
///
/// Components rather than the whole string: a component can never contain a
/// separator, so joining the lowercased components back up cannot make two
/// different paths collide.
fn portable_key(
    path: &Path,
    limits: ExtractionLimits,
    budget: &ExtractionPathBudget,
) -> Result<(PathBuf, usize)> {
    let mut length = 0_usize;
    let mut component_count = 0_usize;
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(Error::invalid_data(
                "portable output key is not strictly relative",
            ));
        };
        let value = value.to_str().ok_or_else(|| {
            Error::invalid_data("portable output key component is not valid UTF-8")
        })?;
        if component_count != 0 {
            length = length
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("portable output key length overflowed"))?;
        }
        component_count = component_count
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("portable output component count overflowed"))?;
        for character in value.chars() {
            for lowercase in character.to_lowercase() {
                length = length
                    .checked_add(lowercase.len_utf8())
                    .ok_or_else(|| Error::invalid_data("portable output key length overflowed"))?;
            }
        }
    }
    let retained_total = budget.checked_path_total(length, limits)?;
    let mut key = String::new();
    key.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate portable output key: {error}"))
    })?;
    for (index, component) in path.components().enumerate() {
        let Component::Normal(value) = component else {
            return Err(Error::invalid_data(
                "portable output key is not strictly relative",
            ));
        };
        let value = value.to_str().ok_or_else(|| {
            Error::invalid_data("portable output key component is not valid UTF-8")
        })?;
        if index != 0 {
            key.push(std::path::MAIN_SEPARATOR);
        }
        for character in value.chars() {
            key.extend(character.to_lowercase());
        }
    }
    debug_assert_eq!(key.len(), length);
    Ok((PathBuf::from(key), retained_total))
}

fn lexical_absolute(path: &Path) -> Result<PathBuf> {
    let joined = if path.is_absolute() {
        copy_path_fallibly(path, "absolute extraction path")?
    } else {
        join_relative_path_fallibly(
            &std::env::current_dir()?,
            path,
            usize::MAX,
            "absolute extraction path",
        )?
    };
    let mut normalized = PathBuf::new();
    normalized
        .try_reserve_exact(joined.as_os_str().as_encoded_bytes().len())
        .map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate normalized extraction path: {error}"
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
                        "path escapes the filesystem root: {}",
                        path.display()
                    )));
                }
            }
        }
    }
    Ok(normalized)
}

fn ensure_secure_directory(path: &Path) -> Result<()> {
    if let Some((ancestor, suffix)) = nearest_existing_ancestor(path)? {
        reject_symlink_ancestors(&ancestor)?;
        return create_secure_suffix(ancestor, &suffix);
    }
    Err(Error::invalid_data(format!(
        "output path has no existing filesystem ancestor: {}",
        path.display()
    )))
}

fn reject_symlink_ancestors(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    current
        .try_reserve_exact(path.as_os_str().as_encoded_bytes().len())
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate output ancestor path: {error}"))
        })?;
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match safe_path_kind(&current)? {
            ExistingKind::Directory => {}
            ExistingKind::Symlink => {
                if is_trusted_system_output_alias(&current) {
                    continue;
                }
                return Err(Error::invalid_data(format!(
                    "refusing symbolic-link output ancestor: {}",
                    current.display()
                )));
            }
            ExistingKind::Regular | ExistingKind::Other => {
                return Err(Error::invalid_data(format!(
                    "output ancestor is not a directory: {}",
                    current.display()
                )));
            }
            ExistingKind::Missing => {
                return Err(Error::invalid_data(format!(
                    "expected output ancestor does not exist: {}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

fn is_trusted_system_output_alias(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let expected = if path == Path::new("/var") {
            Some(Path::new("/private/var"))
        } else if path == Path::new("/tmp") {
            Some(Path::new("/private/tmp"))
        } else {
            None
        };
        let Some(expected) = expected else {
            return false;
        };
        // macOS exposes these root-owned aliases as part of its standard
        // filesystem layout. Require the exact canonical target; arbitrary
        // user-controlled symlinks remain rejected.
        fs::canonicalize(path).is_ok_and(|target| target == expected)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

fn nearest_existing_ancestor(path: &Path) -> Result<Option<(PathBuf, PathBuf)>> {
    let mut ancestor = copy_path_fallibly(path, "output ancestor path")?;
    loop {
        match safe_path_kind(&ancestor)? {
            ExistingKind::Missing => {
                ancestor.file_name().ok_or_else(|| {
                    Error::invalid_data(format!(
                        "missing output ancestor has no name: {}",
                        ancestor.display()
                    ))
                })?;
                if !ancestor.pop() {
                    return Ok(None);
                }
            }
            ExistingKind::Directory
            | ExistingKind::Regular
            | ExistingKind::Other
            | ExistingKind::Symlink => {
                let suffix = path.strip_prefix(&ancestor).map_err(|_| {
                    Error::invalid_data("output path escaped its nearest existing ancestor")
                })?;
                let suffix = copy_path_fallibly(suffix, "output ancestor suffix")?;
                return Ok(Some((ancestor, suffix)));
            }
        }
    }
}

fn create_secure_suffix(mut current: PathBuf, suffix: &Path) -> Result<()> {
    let separator = usize::from(!current.as_os_str().is_empty() && !suffix.as_os_str().is_empty());
    let additional = suffix
        .as_os_str()
        .as_encoded_bytes()
        .len()
        .checked_add(separator)
        .ok_or_else(|| Error::invalid_data("output directory path length overflowed"))?;
    current.try_reserve_exact(additional).map_err(|error| {
        Error::invalid_data(format!("cannot allocate output directory path: {error}"))
    })?;
    for component in suffix.components() {
        let Component::Normal(component) = component else {
            return Err(Error::invalid_data(
                "output suffix is not strictly relative",
            ));
        };
        current.push(component);
        match safe_path_kind(&current)? {
            ExistingKind::Directory => {}
            ExistingKind::Missing => {
                fs::create_dir(&current)?;
                if !matches!(safe_path_kind(&current)?, ExistingKind::Directory) {
                    return Err(Error::invalid_data(format!(
                        "created output component is not a directory: {}",
                        current.display()
                    )));
                }
            }
            ExistingKind::Symlink => {
                return Err(Error::invalid_data(format!(
                    "refusing symbolic-link output directory: {}",
                    current.display()
                )));
            }
            ExistingKind::Regular | ExistingKind::Other => {
                return Err(Error::invalid_data(format!(
                    "output directory component is not a directory: {}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

fn ensure_secure_relative_directory(root: &Path, relative: &Path) -> Result<()> {
    let path =
        join_relative_path_fallibly(root, relative, usize::MAX, "absolute extraction directory")?;
    if !path.starts_with(root) {
        return Err(Error::invalid_data("output directory escaped its root"));
    }
    ensure_secure_directory(&path)
}

fn ensure_secure_parent(root: &Path, relative: &Path) -> Result<()> {
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    ensure_secure_relative_directory(root, parent)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use zip::write::SimpleFileOptions;

    use crate::Error;
    use crate::source::Region;

    use super::{
        ClaimKind, ExtractionLimits, ExtractionOptions, ExtractionPathBudget, Extractor,
        MAX_PORTABLE_COMPONENT_BYTES, PersistOutcome, TemporaryFile, decoded_leaf_path,
        ensure_secure_directory, extract_path, extract_region, extraction_backup_path,
        filesystem_path_label, format_extraction_error, join_relative_path_fallibly,
        lexical_absolute, nearest_existing_ancestor, nested_label, path_from_components,
        portable_key, sanitize_component, sanitize_filesystem_relative_path, sanitize_os_component,
        suffixed_component, suffixed_path,
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn output_path_builders_are_fallible_bounded_and_preserve_empty_parents() {
        let root = Path::new("output-root");
        assert_eq!(
            join_relative_path_fallibly(root, Path::new(""), usize::MAX, "test path").unwrap(),
            root
        );
        let error =
            join_relative_path_fallibly(root, Path::new("child"), 16, "test path").unwrap_err();
        assert!(error.to_string().contains("is 17 bytes"));

        assert_eq!(
            path_from_components(
                &["parent".to_owned(), "child".to_owned()],
                12,
                "test components",
            )
            .unwrap(),
            PathBuf::from("parent").join("child")
        );
        let error = path_from_components(
            &["parent".to_owned(), "child".to_owned()],
            11,
            "test components",
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeding limit 11"));

        let directory = TestDirectory::new("container-component-limit");
        let mut extractor = Extractor::with_path_budget(
            directory.path().to_path_buf(),
            ExtractionOptions::default(),
            ExtractionPathBudget::default(),
        );
        let oversized = "a".repeat(MAX_PORTABLE_COMPONENT_BYTES);
        let error = extractor
            .allocate_container_directory(Path::new(&oversized))
            .unwrap_err();
        assert!(error.to_string().contains("exceeding portable limit"));

        assert_eq!(
            decoded_leaf_path(Path::new("payload.GZ"), "gzip", 7).unwrap(),
            PathBuf::from("payload")
        );
        let oversized_decoded = PathBuf::from("a".repeat(233));
        let error = decoded_leaf_path(&oversized_decoded, "gzip", usize::MAX).unwrap_err();
        assert!(error.to_string().contains("exceeding portable limit"));

        assert_eq!(suffixed_component("asset.bin", 10).unwrap(), "asset~10.bin");
        let oversized_suffix = "a".repeat(239);
        let error = suffixed_component(&oversized_suffix, 1).unwrap_err();
        assert!(error.to_string().contains("too long"));
        let error = suffixed_path(Path::new("parent/asset.bin"), 1, 17).unwrap_err();
        assert!(error.to_string().contains("exceeding limit 17"));

        let temporary = extractor.next_temporary_path(directory.path()).unwrap();
        assert_eq!(temporary.parent(), Some(directory.path()));
        assert!(
            temporary
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.starts_with(".unity-rs-tmp-"))
        );
        assert_eq!(
            extraction_backup_path(Path::new("group/.unity-rs-tmp-1-2")).unwrap(),
            PathBuf::from("group/.unity-rs-tmp-1-2.replace-backup")
        );
    }

    #[test]
    fn portable_claim_keys_are_fallible_case_insensitive_and_budgeted() {
        let limits = ExtractionLimits::default();
        let budget = ExtractionPathBudget::default();
        let (key, retained_total) = portable_key(Path::new("İ/ASSET"), limits, &budget).unwrap();
        assert_eq!(key, PathBuf::from("i\u{307}").join("asset"));
        assert_eq!(retained_total, key.as_os_str().as_encoded_bytes().len());
        assert_eq!(budget.bytes, 0, "a temporary lookup must not commit bytes");

        let mut budget = ExtractionPathBudget::default();
        budget.charge_path(4, limits).unwrap();
        let key_bytes = key.as_os_str().as_encoded_bytes().len();
        let limited = ExtractionLimits {
            maximum_total_path_bytes: 4 + key_bytes - 1,
            ..limits
        };
        let error = portable_key(Path::new("İ/ASSET"), limited, &budget).unwrap_err();
        assert!(error.to_string().contains("extraction paths total"));
        assert_eq!(budget.bytes, 4);

        let root = TestDirectory::new("fallible-claims");
        let mut extractor = Extractor::with_path_budget(
            root.path().to_path_buf(),
            ExtractionOptions::default(),
            ExtractionPathBudget::default(),
        );
        let first = extractor
            .allocate_path(Path::new("Asset.bin"), ClaimKind::File)
            .unwrap();
        let second = extractor
            .allocate_path(Path::new("asset.bin"), ClaimKind::File)
            .unwrap();
        assert_eq!(first, PathBuf::from("Asset.bin"));
        assert_eq!(second, PathBuf::from("asset~1.bin"));
        assert_eq!(extractor.claims.len(), 2);
        assert!(extractor.budget.paths.bytes > 0);
    }

    #[test]
    fn duplicate_leaf_names_resume_the_next_unchecked_suffix() {
        const ENTRIES: usize = 16_384;

        let root = TestDirectory::new("leaf-collision-cursor");
        let mut extractor = Extractor::with_path_budget(
            root.path().to_path_buf(),
            ExtractionOptions::default(),
            ExtractionPathBudget::default(),
        );
        let mut probes = 0_usize;
        let mut last = PathBuf::new();
        for _ in 0..ENTRIES {
            last = extractor
                .allocate_path_with_probe(Path::new("asset.bin"), ClaimKind::File, || {
                    probes += 1;
                })
                .unwrap();
        }

        assert_eq!(last, PathBuf::from("asset~16383.bin"));
        assert_eq!(extractor.claims.len(), ENTRIES);
        assert_eq!(extractor.collision_cursors.len(), 1);
        assert_eq!(
            probes,
            ENTRIES + 1,
            "only the second claim should rediscover the unsuffixed path"
        );
    }

    #[test]
    fn collision_cursor_keys_are_retained_transactionally_under_the_path_budget() {
        let root = TestDirectory::new("collision-cursor-budget");
        let exact_limits = ExtractionLimits {
            maximum_total_path_bytes: 5,
            ..ExtractionLimits::default()
        };
        let mut exact = Extractor::with_path_budget(
            root.path().to_path_buf(),
            ExtractionOptions {
                limits: exact_limits,
                ..ExtractionOptions::default()
            },
            ExtractionPathBudget::default(),
        );
        assert_eq!(
            exact
                .allocate_path(Path::new("x"), ClaimKind::File)
                .unwrap(),
            PathBuf::from("x")
        );
        assert_eq!(
            exact
                .allocate_path(Path::new("x"), ClaimKind::File)
                .unwrap(),
            PathBuf::from("x~1")
        );
        assert_eq!(exact.budget.paths.bytes, 5);
        assert_eq!(exact.collision_cursors.len(), 1);

        let short_limits = ExtractionLimits {
            maximum_total_path_bytes: 4,
            ..ExtractionLimits::default()
        };
        let mut short = Extractor::with_path_budget(
            root.path().to_path_buf(),
            ExtractionOptions {
                limits: short_limits,
                ..ExtractionOptions::default()
            },
            ExtractionPathBudget::default(),
        );
        short
            .allocate_path(Path::new("x"), ClaimKind::File)
            .unwrap();
        let error = short
            .allocate_path(Path::new("x"), ClaimKind::File)
            .unwrap_err();
        assert!(error.to_string().contains("paths total 5 bytes"));
        assert_eq!(short.budget.paths.bytes, 1);
        assert_eq!(short.claims.len(), 1);
        assert!(short.collision_cursors.is_empty());
    }

    #[test]
    fn collided_parent_directories_reuse_the_resolved_suffix() {
        const BLOCKED_SUFFIXES: usize = 4_096;
        const CHILDREN: usize = 16_384;

        let root = TestDirectory::new("parent-collision-cursor");
        let mut extractor = Extractor::with_path_budget(
            root.path().to_path_buf(),
            ExtractionOptions::default(),
            ExtractionPathBudget::default(),
        );
        for suffix in 0..BLOCKED_SUFFIXES {
            let path = if suffix == 0 {
                PathBuf::from("tree")
            } else {
                PathBuf::from(format!("tree~{suffix}"))
            };
            extractor.allocate_path(&path, ClaimKind::File).unwrap();
        }

        let mut probes = 0_usize;
        for _ in 0..CHILDREN {
            let resolved = extractor
                .resolve_parent_collisions_with_probe(Path::new("tree/leaf.bin"), || {
                    probes += 1;
                })
                .unwrap();
            assert_eq!(resolved, PathBuf::from("tree~4096").join("leaf.bin"));
        }
        assert_eq!(extractor.collision_cursors.len(), 1);
        assert_eq!(
            probes,
            BLOCKED_SUFFIXES + CHILDREN,
            "the blocked prefix should be scanned once, then reused"
        );
    }

    #[test]
    fn caller_output_roots_normalize_and_create_deep_suffixes_linearly() {
        let root = TestDirectory::new("output-root-builders");
        let lexical = root
            .path()
            .join("discarded")
            .join("..")
            .join("kept")
            .join(".")
            .join("leaf");
        assert_eq!(
            lexical_absolute(&lexical).unwrap(),
            root.path().join("kept/leaf")
        );

        let target = root.path().join("one/two/three/four");
        let (ancestor, suffix) = nearest_existing_ancestor(&target).unwrap().unwrap();
        assert_eq!(ancestor, root.path());
        assert_eq!(suffix, PathBuf::from("one/two/three/four"));

        ensure_secure_directory(&target).unwrap();
        assert!(target.is_dir());
    }

    #[test]
    fn failure_messages_preserve_error_families_through_fallible_formatting() {
        assert_eq!(
            format_extraction_error(&Error::invalid_data("invalid payload")).unwrap(),
            "invalid payload"
        );
        assert_eq!(
            format_extraction_error(&Error::unsupported("future layout")).unwrap(),
            "unsupported: future layout"
        );
        assert_eq!(
            format_extraction_error(&Error::from(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing input",
            )))
            .unwrap(),
            "missing input"
        );
    }

    #[test]
    fn report_metadata_is_exact_transactional_and_checked_before_publication() {
        let root = TestDirectory::new("report-metadata");
        let output = root.path().join("exact-output");
        let label = "payload.bin";
        let destination = output.join(label);
        let required = label.len() + super::filesystem_path_byte_length(&destination);
        let report = extract_region(
            label,
            Region::from_bytes(b"payload".to_vec()),
            &output,
            ExtractionOptions {
                limits: ExtractionLimits {
                    maximum_metadata_bytes: required,
                    ..ExtractionLimits::default()
                },
                ..ExtractionOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.extracted.len(), 1);
        assert_eq!(report.extracted[0].source, label);
        assert_eq!(report.extracted[0].output_path, destination);

        let short_output = root.path().join("short-output");
        let short_destination = short_output.join(label);
        let short_required = label.len() + super::filesystem_path_byte_length(&short_destination);
        let short_report = extract_region(
            label,
            Region::from_bytes(b"payload".to_vec()),
            &short_output,
            ExtractionOptions {
                limits: ExtractionLimits {
                    maximum_metadata_bytes: short_required - 1,
                    ..ExtractionLimits::default()
                },
                ..ExtractionOptions::default()
            },
        )
        .unwrap();
        assert!(short_report.extracted.is_empty());
        assert_eq!(short_report.failures.len(), 1);
        assert!(
            short_report.failures[0]
                .error
                .contains("extraction report metadata requires"),
            "{:?}",
            short_report.failures
        );
        assert!(!short_destination.exists());

        let failure = Error::unsupported("future layout");
        let failure_source = "fixture".to_owned();
        let failure_required = failure_source.len() + "unsupported: future layout".len();
        let failure_limit = failure_required * 2;
        let mut exact = Extractor::with_path_budget(
            root.path().to_path_buf(),
            ExtractionOptions {
                limits: ExtractionLimits {
                    maximum_metadata_bytes: failure_limit,
                    ..ExtractionLimits::default()
                },
                ..ExtractionOptions::default()
            },
            ExtractionPathBudget::default(),
        );
        exact
            .record_failure(failure_source.clone(), &failure)
            .unwrap();
        exact
            .record_failure(failure_source.clone(), &failure)
            .unwrap();
        assert_eq!(exact.budget.report_metadata_bytes, failure_limit);
        assert_eq!(exact.report.failures.len(), 2);
        assert!(exact.record_failure(failure_source, &failure).is_err());
        assert_eq!(exact.budget.report_metadata_bytes, failure_limit);
        assert_eq!(exact.report.failures.len(), 2);
    }

    #[test]
    fn bounds_root_and_nested_diagnostic_labels_before_allocation() {
        let root = TestDirectory::new("label-budgets");
        let root_error = extract_region(
            "12345",
            Region::from_bytes(b"payload".as_slice()),
            &root.path().join("root-label-output"),
            ExtractionOptions {
                limits: ExtractionLimits {
                    maximum_path_bytes: 4,
                    ..ExtractionLimits::default()
                },
                ..ExtractionOptions::default()
            },
        )
        .unwrap_err();
        assert!(root_error.to_string().contains("path or label is 5 bytes"));
        assert!(!root.path().join("root-label-output").exists());

        let mut budget = ExtractionPathBudget::default();
        let limits = ExtractionLimits {
            maximum_total_path_bytes: 12,
            ..ExtractionLimits::default()
        };
        budget.charge_path(4, limits).unwrap();
        let nested_error = nested_label("root", "a\\b", limits, &mut budget).unwrap_err();
        assert!(nested_error.to_string().contains("paths total 13 bytes"));
        assert_eq!(budget.bytes, 4);

        let limits = ExtractionLimits {
            maximum_total_path_bytes: 13,
            ..ExtractionLimits::default()
        };
        assert_eq!(
            nested_label("root", "a\\b", limits, &mut budget).unwrap(),
            "root::a/b"
        );
        assert_eq!(budget.bytes, 13);
    }

    #[test]
    fn sanitizes_path_components_within_the_portable_budget() {
        let ascii_boundary = "a".repeat(MAX_PORTABLE_COMPONENT_BYTES);
        assert_eq!(sanitize_component(&ascii_boundary).unwrap(), ascii_boundary);

        let unicode_boundary = "界".repeat(MAX_PORTABLE_COMPONENT_BYTES / "界".len());
        assert_eq!(unicode_boundary.len(), MAX_PORTABLE_COMPONENT_BYTES);
        assert_eq!(
            sanitize_component(&unicode_boundary).unwrap(),
            unicode_boundary
        );

        let oversized = format!("{ascii_boundary}b");
        let error = sanitize_component(&oversized).unwrap_err();
        assert!(error.to_string().contains("portable limit 240"), "{error}");

        let reserved_at_boundary = format!("CON.{}", "x".repeat(236));
        assert_eq!(reserved_at_boundary.len(), MAX_PORTABLE_COMPONENT_BYTES);
        let error = sanitize_component(&reserved_at_boundary).unwrap_err();
        assert!(error.to_string().contains("needs a prefix"), "{error}");
        assert_eq!(sanitize_component("CON").unwrap(), "_CON");
    }

    #[cfg(unix)]
    #[test]
    fn streams_non_utf8_filesystem_names_under_component_and_label_budgets() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let accepted = OsString::from_vec(vec![0xff; 80]);
        let expected = "\u{fffd}".repeat(80);
        let sanitized = sanitize_os_component(&accepted).unwrap();
        assert_eq!(sanitized, expected);
        assert_eq!(sanitized.len(), MAX_PORTABLE_COMPONENT_BYTES);
        assert_eq!(
            sanitize_filesystem_relative_path(Path::new(&accepted), ExtractionLimits::default())
                .unwrap()
                .to_str(),
            Some(expected.as_str())
        );

        let rejected = OsString::from_vec(vec![0xff; 81]);
        let error = sanitize_os_component(&rejected).unwrap_err();
        assert!(error.to_string().contains("at least 243 bytes"), "{error}");

        let mixed = OsString::from_vec(vec![b'a', 0xff, b':', 0x80]);
        assert_eq!(sanitize_os_component(&mixed).unwrap(), "a\u{fffd}_\u{fffd}");

        let path = PathBuf::from(OsString::from_vec(vec![0xff, 0xfe]));
        let mut budget = ExtractionPathBudget::default();
        let per_path_limits = ExtractionLimits {
            maximum_path_bytes: 5,
            ..ExtractionLimits::default()
        };
        let error = filesystem_path_label(&path, per_path_limits, &mut budget).unwrap_err();
        assert!(error.to_string().contains("is 6 UTF-8 bytes"), "{error}");
        assert_eq!(budget.bytes, 0);

        let cumulative_limits = ExtractionLimits {
            maximum_path_bytes: 6,
            maximum_total_path_bytes: 5,
            ..ExtractionLimits::default()
        };
        budget.charge_path(2, cumulative_limits).unwrap();
        let error = filesystem_path_label(&path, cumulative_limits, &mut budget).unwrap_err();
        assert!(error.to_string().contains("paths total 6 bytes"), "{error}");
        assert_eq!(budget.bytes, 2);

        let exact_limits = ExtractionLimits {
            maximum_path_bytes: 6,
            maximum_total_path_bytes: 6,
            ..ExtractionLimits::default()
        };
        let mut exact_budget = ExtractionPathBudget::default();
        exact_budget.charge_path(2, exact_limits).unwrap();
        assert_eq!(
            filesystem_path_label(&path, exact_limits, &mut exact_budget).unwrap(),
            "\u{fffd}\u{fffd}"
        );
        assert_eq!(exact_budget.bytes, 6);
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "unity-rs-extraction-{label}-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn recursively_extracts_gzip_webdata_and_preserves_leaf_bytes() {
        let root = TestDirectory::new("nested");
        let input = root.path().join("payload.gz");
        let output = root.path().join("output");
        let web = web_file(&[("folder/data.bin", b"exact payload")]);
        fs::write(&input, gzip(&web)).unwrap();

        let report = extract_path(&input, &output, ExtractionOptions::default()).unwrap();

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.extracted.len(), 1);
        assert_eq!(report.output_bytes, 13);
        assert_eq!(
            fs::read(output.join("payload.gz_unpacked").join("folder/data.bin")).unwrap(),
            b"exact payload"
        );
    }

    #[test]
    fn extracts_unity_fs_legacy_raw_and_zip_entries() {
        let root = TestDirectory::new("container-kinds");
        let fixtures = [
            (
                "modern.bundle",
                unity_fs_bundle("folder/modern.bin", b"modern"),
            ),
            (
                "web-v6.bundle",
                unity_fs_bundle_with_signature(b"UnityWeb\0", "folder/web-v6.bin", b"web-v6"),
            ),
            (
                "legacy.bundle",
                legacy_raw_bundle("folder/legacy.bin", b"legacy"),
            ),
            ("archive.zip", zip_file("folder/zipped.bin", b"zipped")),
        ];
        for (name, bytes) in fixtures {
            let output = root.path().join(format!("out-{name}"));
            let report = extract_region(
                name,
                Region::from_bytes(bytes),
                &output,
                ExtractionOptions::default(),
            )
            .unwrap();
            assert!(report.failures.is_empty(), "{name}: {:?}", report.failures);
            assert_eq!(report.extracted.len(), 1, "{name}");
        }
        assert_eq!(
            fs::read(
                root.path()
                    .join("out-web-v6.bundle/web-v6.bundle_unpacked/folder/web-v6.bin")
            )
            .unwrap(),
            b"web-v6"
        );
        assert_eq!(
            fs::read(
                root.path()
                    .join("out-modern.bundle/modern.bundle_unpacked/folder/modern.bin")
            )
            .unwrap(),
            b"modern"
        );
        assert_eq!(
            fs::read(
                root.path()
                    .join("out-legacy.bundle/legacy.bundle_unpacked/folder/legacy.bin")
            )
            .unwrap(),
            b"legacy"
        );
        assert_eq!(
            fs::read(
                root.path()
                    .join("out-archive.zip/archive.zip_unpacked/folder/zipped.bin")
            )
            .unwrap(),
            b"zipped"
        );
    }

    #[test]
    fn extracts_a_recognized_brotli_wrapper_to_a_leaf() {
        // A standards-compliant Brotli stream with a metadata block whose
        // `brotli` marker starts at byte 32, matching Unity's legacy detector.
        const BROTLI: &[u8] = &[
            0x6f, 0x89, 0x00, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78,
            0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78,
            0x78, 0x78, 0x78, 0x78, 0x62, 0x72, 0x6f, 0x74, 0x6c, 0x69, 0x50, 0x00, 0x08, 0x62,
            0x72, 0x6f, 0x74, 0x6c, 0x69, 0x20, 0x6c, 0x65, 0x61, 0x66, 0x03,
        ];
        let root = TestDirectory::new("brotli");
        let output = root.path().join("output");

        let report = extract_region(
            "payload.br",
            Region::from_bytes(BROTLI),
            &output,
            ExtractionOptions::default(),
        )
        .unwrap();

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(fs::read(output.join("payload")).unwrap(), b"brotli leaf");
    }

    #[test]
    fn extracts_tuanjie_webdata_and_rejects_unity_archive_explicitly() {
        let root = TestDirectory::new("tuanjie-archive");
        let tuanjie = web_file_with_signature(b"TuanjieWebData1.0\0", &[("data.bin", b"tuanjie")]);
        let report = extract_region(
            "tuanjie.data",
            Region::from_bytes(tuanjie),
            &root.path().join("tuanjie-output"),
            ExtractionOptions::default(),
        )
        .unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(
            fs::read(
                root.path()
                    .join("tuanjie-output/tuanjie.data_unpacked/data.bin")
            )
            .unwrap(),
            b"tuanjie"
        );

        let mut archive = Vec::new();
        archive.extend_from_slice(b"UnityArchive\0");
        archive.extend_from_slice(&5_u32.to_be_bytes());
        archive.extend_from_slice(b"5.x.x\0");
        archive.extend_from_slice(b"5.0.0f4\0");
        let report = extract_region(
            "unsupported.bundle",
            Region::from_bytes(archive),
            &root.path().join("archive-output"),
            ExtractionOptions::default(),
        )
        .unwrap();
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].error.contains("UnityArchive bundles"));
        assert!(report.extracted.is_empty());
    }

    #[test]
    fn rejects_parent_and_absolute_entries_but_continues_safe_siblings() {
        let root = TestDirectory::new("traversal");
        let output = root.path().join("output");
        // The Windows spellings matter as much as the POSIX ones: an archive
        // is data, so a backslash path escapes on Windows whatever host wrote
        // it, and a drive or UNC prefix is absolute there. The sanitiser
        // normalises separators and rejects both, and nothing exercised that
        // until this list grew.
        let web = web_file(&[
            ("../escape.bin", b"escape"),
            ("/absolute.bin", b"absolute"),
            ("..\\escape-backslash.bin", b"escape"),
            ("sub\\..\\..\\escape-mixed.bin", b"escape"),
            ("C:\\drive.bin", b"drive"),
            ("//unc-share.bin", b"unc"),
            ("safe/data.bin", b"safe"),
            ("safe\\nested\\data.bin", b"safe backslash"),
        ]);

        let report = extract_region(
            "web.data",
            Region::from_bytes(web),
            &output,
            ExtractionOptions::default(),
        )
        .unwrap();

        assert_eq!(report.extracted.len(), 2);
        assert_eq!(report.failures.len(), 6);
        // Each rejection has to be the one intended: a path that merely failed
        // to parse would count the same and prove nothing about the guard.
        let reasons: Vec<&str> = report
            .failures
            .iter()
            .map(|failure| failure.error.as_str())
            .collect();
        assert_eq!(
            reasons
                .iter()
                .filter(|reason| reason.contains("contains '..'"))
                .count(),
            3,
            "{reasons:?}"
        );
        assert_eq!(
            reasons
                .iter()
                .filter(|reason| reason.contains("must be relative"))
                .count(),
            2,
            "{reasons:?}"
        );
        assert_eq!(
            reasons
                .iter()
                .filter(|reason| reason.contains("Windows drive prefix"))
                .count(),
            1,
            "{reasons:?}"
        );
        assert_eq!(
            fs::read(output.join("web.data_unpacked/safe/data.bin")).unwrap(),
            b"safe"
        );
        assert_eq!(
            fs::read(output.join("web.data_unpacked/safe/nested/data.bin")).unwrap(),
            b"safe backslash"
        );
        assert!(!root.path().join("escape.bin").exists());
        assert!(!output.join("absolute.bin").exists());
        for escaped in [
            "escape-backslash.bin",
            "escape-mixed.bin",
            "drive.bin",
            "unc-share.bin",
        ] {
            assert!(
                !root.path().join(escaped).exists() && !output.join(escaped).exists(),
                "{escaped} was written outside the extraction root"
            );
        }
        // The tree below the output root holds exactly the one safe file, so
        // nothing landed under a mangled name either.
        let mut written = Vec::new();
        let mut queue = vec![output.clone()];
        while let Some(directory) = queue.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    queue.push(path);
                } else {
                    written.push(path);
                }
            }
        }
        assert_eq!(written.len(), 2, "{written:?}");
    }

    #[test]
    fn sanitizes_names_and_assigns_stable_collision_suffixes() {
        let root = TestDirectory::new("collision");
        let output = root.path().join("output");
        let web = web_file(&[("a:b.txt", b"first"), ("a?b.txt", b"second")]);

        let report = extract_region(
            "collision.web",
            Region::from_bytes(web),
            &output,
            ExtractionOptions::default(),
        )
        .unwrap();

        assert!(report.failures.is_empty());
        assert_eq!(
            fs::read(output.join("collision.web_unpacked/a_b.txt")).unwrap(),
            b"first"
        );
        assert_eq!(
            fs::read(output.join("collision.web_unpacked/a_b~1.txt")).unwrap(),
            b"second"
        );
    }

    #[test]
    fn skips_existing_by_default_and_overwrites_only_when_requested() {
        let root = TestDirectory::new("overwrite");
        let output = root.path().join("output");
        let destination = output.join("input.bin");
        fs::create_dir_all(&output).unwrap();
        fs::write(&destination, b"existing").unwrap();

        let skipped = extract_region(
            "input.bin",
            Region::from_bytes(b"replacement".as_slice()),
            &output,
            ExtractionOptions::default(),
        )
        .unwrap();
        assert_eq!(skipped.skipped_existing.len(), 1);
        assert!(skipped.extracted.is_empty());
        assert_eq!(fs::read(&destination).unwrap(), b"existing");

        let overwritten = extract_region(
            "input.bin",
            Region::from_bytes(b"replacement".as_slice()),
            &output,
            ExtractionOptions {
                overwrite_existing: true,
                ..ExtractionOptions::default()
            },
        )
        .unwrap();
        assert_eq!(overwritten.extracted.len(), 1);
        assert_eq!(fs::read(destination).unwrap(), b"replacement");
    }

    #[test]
    fn no_overwrite_persist_is_atomic_against_a_late_destination() {
        let root = TestDirectory::new("atomic-no-overwrite");
        let temporary_path = root.path().join("temporary");
        let destination = root.path().join("destination");
        let mut temporary = TemporaryFile::create(temporary_path).unwrap();
        temporary.file_mut().write_all(b"replacement").unwrap();
        temporary.close().unwrap();
        fs::write(&destination, b"late existing").unwrap();

        assert_eq!(
            temporary.persist(&destination, false).unwrap(),
            PersistOutcome::SkippedExisting
        );
        assert_eq!(fs::read(destination).unwrap(), b"late existing");
    }

    #[test]
    fn enforces_depth_entry_expansion_and_output_limits() {
        let root = TestDirectory::new("limits");
        let web = web_file(&[("one.bin", b"1234"), ("two.bin", b"5678")]);
        let cases = [
            ExtractionLimits {
                maximum_nesting_depth: 0,
                ..ExtractionLimits::default()
            },
            ExtractionLimits {
                maximum_entries: 1,
                ..ExtractionLimits::default()
            },
            ExtractionLimits {
                maximum_expanded_bytes: 4,
                ..ExtractionLimits::default()
            },
            ExtractionLimits {
                maximum_output_bytes: 4,
                ..ExtractionLimits::default()
            },
        ];
        for (index, limits) in cases.into_iter().enumerate() {
            let report = extract_region(
                "limited.web",
                Region::from_bytes(web.clone()),
                &root.path().join(format!("out-{index}")),
                ExtractionOptions {
                    limits,
                    ..ExtractionOptions::default()
                },
            )
            .unwrap();
            assert!(!report.failures.is_empty(), "limit case {index}");
        }
    }

    #[test]
    fn bounds_input_directory_traversal_before_collection() {
        let root = TestDirectory::new("input-traversal-limits");
        let input = root.path().join("input");
        fs::create_dir_all(input.join("empty-a/nested")).unwrap();
        fs::create_dir_all(input.join("empty-b")).unwrap();
        fs::write(input.join("payload.bin"), b"payload").unwrap();

        let entry_error = extract_path(
            &input,
            &root.path().join("entry-output"),
            ExtractionOptions {
                limits: ExtractionLimits {
                    maximum_entries: 2,
                    ..ExtractionLimits::default()
                },
                ..ExtractionOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            entry_error.to_string().contains("exceeds 2 entries"),
            "{entry_error}"
        );

        let root_path_bytes = super::filesystem_path_byte_length(&input);
        let path_error = extract_path(
            &input,
            &root.path().join("path-output"),
            ExtractionOptions {
                limits: ExtractionLimits {
                    maximum_total_path_bytes: root_path_bytes,
                    ..ExtractionLimits::default()
                },
                ..ExtractionOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            path_error.to_string().contains("extraction paths total"),
            "{path_error}"
        );

        let single_path_error = extract_path(
            &input,
            &root.path().join("single-path-output"),
            ExtractionOptions {
                limits: ExtractionLimits {
                    maximum_path_bytes: root_path_bytes - 1,
                    ..ExtractionLimits::default()
                },
                ..ExtractionOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            single_path_error
                .to_string()
                .contains("extraction path or label is"),
            "{single_path_error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_preexisting_symbolic_link_parents() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new("symlink");
        let output = root.path().join("output");
        let outside = root.path().join("outside");
        fs::create_dir_all(output.join("links.web_unpacked")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, output.join("links.web_unpacked/link")).unwrap();
        let web = web_file(&[("link/pwn.bin", b"no")]);

        let report = extract_region(
            "links.web",
            Region::from_bytes(web),
            &output,
            ExtractionOptions::default(),
        )
        .unwrap();

        assert_eq!(report.failures.len(), 1);
        assert!(!outside.join("pwn.bin").exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_in_the_output_root_ancestor_chain() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new("root-ancestor-symlink");
        let outside = root.path().join("outside");
        let linked_parent = root.path().join("linked-parent");
        fs::create_dir_all(outside.join("existing-output")).unwrap();
        symlink(&outside, &linked_parent).unwrap();

        let error = extract_region(
            "leaf.bin",
            Region::from_bytes(b"no".as_slice()),
            &linked_parent.join("existing-output"),
            ExtractionOptions::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("symbolic-link output ancestor"));
        assert!(!outside.join("existing-output/leaf.bin").exists());
    }

    #[test]
    fn treats_ascii_case_collisions_portably() {
        let root = TestDirectory::new("case-collision");
        let output = root.path().join("output");
        let web = web_file(&[("File.bin", b"first"), ("file.bin", b"second")]);

        let report = extract_region(
            "case.web",
            Region::from_bytes(web),
            &output,
            ExtractionOptions::default(),
        )
        .unwrap();

        assert!(report.failures.is_empty());
        assert_eq!(
            fs::read(output.join("case.web_unpacked/File.bin")).unwrap(),
            b"first"
        );
        assert_eq!(
            fs::read(output.join("case.web_unpacked/file~1.bin")).unwrap(),
            b"second"
        );
    }

    fn gzip(payload: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(payload).unwrap();
        encoder.finish().unwrap()
    }

    fn web_file(entries: &[(&str, &[u8])]) -> Vec<u8> {
        web_file_with_signature(b"UnityWebData1.0\0", entries)
    }

    fn web_file_with_signature(signature: &[u8], entries: &[(&str, &[u8])]) -> Vec<u8> {
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

    fn zip_file(path: &str, payload: &[u8]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file(path, options).unwrap();
        writer.write_all(payload).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn unity_fs_bundle(path: &str, payload: &[u8]) -> Vec<u8> {
        unity_fs_bundle_with_signature(b"UnityFS\0", path, payload)
    }

    fn unity_fs_bundle_with_signature(signature: &[u8], path: &str, payload: &[u8]) -> Vec<u8> {
        const BLOCKS_AND_DIRECTORY_INFO_COMBINED: u32 = 0x40;
        let payload_size = u32::try_from(payload.len()).unwrap();
        let mut blocks_info = vec![0_u8; 16];
        blocks_info.extend_from_slice(&1_i32.to_be_bytes());
        blocks_info.extend_from_slice(&payload_size.to_be_bytes());
        blocks_info.extend_from_slice(&payload_size.to_be_bytes());
        blocks_info.extend_from_slice(&0_u16.to_be_bytes());
        blocks_info.extend_from_slice(&1_i32.to_be_bytes());
        blocks_info.extend_from_slice(&0_i64.to_be_bytes());
        blocks_info.extend_from_slice(&i64::try_from(payload.len()).unwrap().to_be_bytes());
        blocks_info.extend_from_slice(&0_u32.to_be_bytes());
        blocks_info.extend_from_slice(path.as_bytes());
        blocks_info.push(0);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(signature);
        bytes.extend_from_slice(&6_u32.to_be_bytes());
        bytes.extend_from_slice(b"5.x.x\0");
        bytes.extend_from_slice(b"2018.4.0f1\0");
        let size_position = bytes.len();
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(blocks_info.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(blocks_info.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&BLOCKS_AND_DIRECTORY_INFO_COMBINED.to_be_bytes());
        if signature != b"UnityFS\0" {
            bytes.push(0);
        }
        bytes.extend_from_slice(&blocks_info);
        bytes.extend_from_slice(payload);
        let size = i64::try_from(bytes.len()).unwrap();
        bytes[size_position..size_position + 8].copy_from_slice(&size.to_be_bytes());
        bytes
    }

    fn legacy_raw_bundle(path: &str, payload: &[u8]) -> Vec<u8> {
        let directory_size = 4 + path.len() + 1 + 8;
        let mut content = Vec::new();
        content.extend_from_slice(&1_i32.to_be_bytes());
        content.extend_from_slice(path.as_bytes());
        content.push(0);
        content.extend_from_slice(&u32::try_from(directory_size).unwrap().to_be_bytes());
        content.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        content.extend_from_slice(payload);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityRaw\0");
        bytes.extend_from_slice(&3_u32.to_be_bytes());
        bytes.extend_from_slice(b"3.x.x\0");
        bytes.extend_from_slice(b"3.5.0f5\0");
        let minimum_streamed_position = bytes.len();
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        let header_size_position = bytes.len();
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        let content_size = u32::try_from(content.len()).unwrap();
        bytes.extend_from_slice(&content_size.to_be_bytes());
        bytes.extend_from_slice(&content_size.to_be_bytes());
        let complete_size_position = bytes.len();
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(directory_size).unwrap().to_be_bytes());
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        let header_size = u32::try_from(bytes.len()).unwrap();
        bytes[header_size_position..header_size_position + 4]
            .copy_from_slice(&header_size.to_be_bytes());
        bytes.extend_from_slice(&content);
        let complete_size = u32::try_from(bytes.len()).unwrap();
        bytes[minimum_streamed_position..minimum_streamed_position + 4]
            .copy_from_slice(&complete_size.to_be_bytes());
        bytes[complete_size_position..complete_size_position + 4]
            .copy_from_slice(&complete_size.to_be_bytes());
        bytes
    }
}
