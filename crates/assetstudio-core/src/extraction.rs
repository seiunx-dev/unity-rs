use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::bundle::{
    BundleHeader, BundleOpenOptions, BundleParseLimits, OodleDecoder, UnityFsBundle,
};
use crate::compression::{CompressionLimits, ZipContainer, decompress_brotli, decompress_gzip};
use crate::endian::{Endian, EndianReader};
use crate::file_type::{FileDetection, FileType, HEADER_SCAN_LENGTH, detect_file_type};
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
    pub maximum_path_bytes: usize,
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
        vec![(
            input.to_owned(),
            sanitize_component_path(file_name.to_string_lossy().as_ref(), options.limits)?,
        )]
    } else if metadata.is_dir() {
        if output_absolute.starts_with(&input_absolute) {
            return Err(Error::invalid_data(
                "output directory must not be inside the input directory",
            ));
        }
        collect_regular_files(input, options.limits)?
            .into_iter()
            .map(|path| {
                let relative = path
                    .strip_prefix(input)
                    .map_err(|_| Error::invalid_data("directory input escaped its traversal root"))?
                    .to_owned();
                Ok((
                    path,
                    sanitize_filesystem_relative_path(&relative, options.limits)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        return Err(Error::invalid_data(format!(
            "input is neither a regular file nor a directory: {}",
            input.display()
        )));
    };

    ensure_secure_directory(&output_absolute)?;
    let mut extractor = Extractor::new(output_absolute, options);
    for (path, relative) in roots {
        let label = path.to_string_lossy().into_owned();
        let result = Region::from_file(&path)
            .and_then(|region| extractor.process_region(&label, region, relative, 0));
        if let Err(error) = result {
            extractor.record_failure(label, &error);
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
    let output_absolute = lexical_absolute(output_root)?;
    ensure_secure_directory(&output_absolute)?;
    let file_name = label.rsplit(['/', '\\']).next().unwrap_or(label);
    let relative = sanitize_component_path(file_name, options.limits)?;
    let mut extractor = Extractor::new(output_absolute, options);
    if let Err(error) = extractor.process_region(label, region, relative, 0) {
        extractor.record_failure(label.to_owned(), &error);
    }
    Ok(extractor.report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimKind {
    File,
    Directory,
}

#[derive(Debug, Default)]
struct ExtractionBudget {
    entries: usize,
    expanded_bytes: u64,
}

struct Extractor {
    output_root: PathBuf,
    options: ExtractionOptions,
    budget: ExtractionBudget,
    claims: BTreeMap<PathBuf, ClaimKind>,
    temporary_sequence: u64,
    report: ExtractionReport,
}

impl Extractor {
    fn new(output_root: PathBuf, options: ExtractionOptions) -> Self {
        Self {
            output_root,
            options,
            budget: ExtractionBudget::default(),
            claims: BTreeMap::new(),
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
            decoded_leaf_path(&desired_path, wrapper)?
        } else {
            desired_path
        };
        let child_label = format!("{label}::{wrapper}");
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
            let child_label = nested_label(label, &entry.path);
            let result = self
                .charge_entry(entry.data_length)
                .and_then(|()| sanitize_archive_path(&entry.path, self.options.limits))
                .and_then(|path| {
                    self.process_region(
                        &child_label,
                        web.entry_region(index)?,
                        container.join(path),
                        next_depth,
                    )
                });
            if let Err(error) = result {
                self.record_failure(child_label, &error);
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
            let child_label = nested_label(label, &entry.path);
            let result = self
                .charge_entry(entry.size)
                .and_then(|()| sanitize_archive_path(&entry.path, self.options.limits))
                .and_then(|path| {
                    self.process_region(
                        &child_label,
                        archive.read_entry(index)?,
                        container.join(path),
                        next_depth,
                    )
                });
            if let Err(error) = result {
                self.record_failure(child_label, &error);
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
        for index in 0..bundle.entries.len() {
            let entry = &bundle.entries[index];
            let child_label = nested_label(label, &entry.path);
            let result = self
                .charge_entry(entry.size)
                .and_then(|()| sanitize_archive_path(&entry.path, self.options.limits))
                .and_then(|path| {
                    self.process_unity_fs_entry(
                        &child_label,
                        container.join(path),
                        next_depth,
                        bundle,
                        index,
                    )
                });
            if let Err(error) = result {
                self.record_failure(child_label, &error);
            }
        }
        Ok(())
    }

    fn process_unity_fs_entry(
        &mut self,
        label: &str,
        desired_path: PathBuf,
        depth: usize,
        bundle: &UnityFsBundle,
        index: usize,
    ) -> Result<()> {
        let entry = bundle.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("UnityFS entry index {index} is out of range"))
        })?;
        let mut probe = HeaderProbe::new();
        let probed = bundle.copy_entry(index, &mut probe)?;
        if probed != entry.size || probe.written != entry.size {
            return Err(Error::invalid_data(format!(
                "UnityFS entry probe wrote {} bytes; expected {}",
                probe.written, entry.size
            )));
        }
        let detection = detect_file_type(&probe.header, entry.size);
        if matches!(
            detection.file_type,
            FileType::AssetsFile | FileType::ResourceFile
        ) {
            return self.write_streaming_leaf(label, &desired_path, entry.size, |output| {
                bundle.copy_entry(index, output)
            });
        }
        let region = Region::from_bytes(bundle.read_entry(index)?);
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
            let child_label = nested_label(label, &entry.path);
            let result = self
                .charge_entry(entry.size)
                .and_then(|()| sanitize_archive_path(&entry.path, self.options.limits))
                .and_then(|path| {
                    self.process_region(
                        &child_label,
                        bundle.entry_region(index)?,
                        container.join(path),
                        next_depth,
                    )
                });
            if let Err(error) = result {
                self.record_failure(child_label, &error);
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
        let output_path = self.output_root.join(&relative);
        ensure_secure_parent(&self.output_root, &relative)?;
        match safe_file_state(&output_path)? {
            FileState::Regular if !self.options.overwrite_existing => {
                self.report.skipped_existing.push(ExtractionSkip {
                    source: label.to_owned(),
                    output_path,
                });
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

        let outcome = self.atomic_write(&output_path, length, copy)?;
        if outcome == PersistOutcome::SkippedExisting {
            self.report.skipped_existing.push(ExtractionSkip {
                source: label.to_owned(),
                output_path,
            });
            return Ok(());
        }
        self.report.output_bytes = new_total;
        self.report.extracted.push(ExtractionRecord {
            source: label.to_owned(),
            output_path,
            bytes: length,
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
        let mut unpacked_name = file_name.to_string_lossy().into_owned();
        unpacked_name.push_str("_unpacked");
        let desired = source_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(unpacked_name);
        let relative = self.allocate_path(&desired, ClaimKind::Directory)?;
        ensure_secure_relative_directory(&self.output_root, &relative)?;
        Ok(relative)
    }

    fn allocate_path(&mut self, desired: &Path, kind: ClaimKind) -> Result<PathBuf> {
        let resolved = self.resolve_parent_collisions(desired)?;
        for collision_index in 0_u64.. {
            let candidate = if collision_index == 0 {
                resolved.clone()
            } else {
                suffixed_path(&resolved, collision_index)?
            };
            if self.path_is_claimed(&candidate) {
                continue;
            }
            let absolute = self.output_root.join(&candidate);
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
                self.claims.insert(candidate.clone(), kind);
                return Ok(candidate);
            }
        }
        Err(Error::invalid_data("output collision counter overflowed"))
    }

    fn path_is_claimed(&self, candidate: &Path) -> bool {
        self.claims
            .keys()
            .any(|claimed| paths_equal_portably(claimed, candidate))
    }

    fn resolve_parent_collisions(&self, desired: &Path) -> Result<PathBuf> {
        let mut components = desired
            .components()
            .map(|component| match component {
                Component::Normal(value) => Ok(value.to_string_lossy().into_owned()),
                _ => Err(Error::invalid_data("output path is not strictly relative")),
            })
            .collect::<Result<Vec<_>>>()?;
        if components.is_empty() {
            return Err(Error::invalid_data("output path is empty"));
        }
        for index in 0..components.len().saturating_sub(1) {
            let original = components[index].clone();
            for collision_index in 0_u64.. {
                components[index] = if collision_index == 0 {
                    original.clone()
                } else {
                    suffixed_component(&original, collision_index)?
                };
                let prefix = components[..=index].iter().collect::<PathBuf>();
                let absolute = self.output_root.join(&prefix);
                let claim = self.claim_kind(&prefix);
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
                        break;
                    }
                    ExistingKind::Missing if claim != Some(ClaimKind::File) => break,
                    ExistingKind::Regular
                    | ExistingKind::Other
                    | ExistingKind::Directory
                    | ExistingKind::Missing => {}
                }
            }
        }
        Ok(components.iter().collect())
    }

    fn claim_kind(&self, path: &Path) -> Option<ClaimKind> {
        self.claims
            .iter()
            .find_map(|(claimed, kind)| paths_equal_portably(claimed, path).then_some(*kind))
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
            let candidate = directory.join(format!(
                ".assetstudio-tmp-{}-{}",
                std::process::id(),
                self.temporary_sequence
            ));
            if matches!(safe_path_kind(&candidate)?, ExistingKind::Missing) {
                return Ok(candidate);
            }
        }
    }

    fn record_failure(&mut self, source: String, error: &Error) {
        self.report.failures.push(ExtractionFailure {
            source,
            error: error.to_string(),
        });
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
                    fs::remove_file(&self.path)?;
                    self.persisted = true;
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
        let backup = self.path.with_extension("replace-backup");
        if !matches!(safe_path_kind(&backup)?, ExistingKind::Missing) {
            return Err(original_error.into());
        }
        fs::rename(destination, &backup).map_err(|_| original_error)?;
        match fs::rename(&self.path, destination) {
            Ok(()) => {
                self.persisted = true;
                fs::remove_file(backup)?;
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

fn collect_regular_files(root: &Path, limits: ExtractionLimits) -> Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    directories.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate extraction directory queue: {error}"
        ))
    })?;
    directories.push(root.to_owned());
    let mut files = Vec::new();
    let mut entry_count = 0_usize;
    while let Some(directory) = directories.pop() {
        let mut children = Vec::new();
        for child in fs::read_dir(directory)? {
            entry_count = entry_count.checked_add(1).ok_or_else(|| {
                Error::invalid_data("extraction directory entry count overflowed")
            })?;
            if entry_count > limits.maximum_entries {
                return Err(Error::invalid_data(format!(
                    "extraction directory traversal exceeds {} entries",
                    limits.maximum_entries
                )));
            }
            children.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!(
                    "cannot allocate extraction directory entries: {error}"
                ))
            })?;
            children.push(child?);
        }
        children.sort_unstable_by_key(fs::DirEntry::file_name);
        for child in children.into_iter().rev() {
            let file_type = child.file_type()?;
            if file_type.is_dir() {
                directories.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow extraction directory queue: {error}"))
                })?;
                directories.push(child.path());
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
                files.push(child.path());
            }
        }
    }
    files.sort_unstable();
    Ok(files)
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
    {
        return Err(Error::invalid_data(
            "extraction count and byte limits must be greater than zero",
        ));
    }
    Ok(())
}

fn sanitize_archive_path(path: &str, limits: ExtractionLimits) -> Result<PathBuf> {
    let normalized = path.replace('\\', "/");
    if normalized.starts_with('/') || normalized.starts_with("//") {
        return Err(Error::invalid_data(format!(
            "archive entry path must be relative: {path:?}"
        )));
    }
    if normalized
        .split('/')
        .next()
        .is_some_and(is_windows_drive_component)
    {
        return Err(Error::invalid_data(format!(
            "archive entry path has a Windows drive prefix: {path:?}"
        )));
    }
    let mut result = PathBuf::new();
    for component in normalized.split('/') {
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
    validate_relative_output_path(&result, limits, path)?;
    Ok(result)
}

fn sanitize_filesystem_relative_path(path: &Path, limits: ExtractionLimits) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                result.push(sanitize_component(value.to_string_lossy().as_ref())?);
            }
            _ => {
                return Err(Error::invalid_data(format!(
                    "input relative path is not enclosed: {}",
                    path.display()
                )));
            }
        }
    }
    validate_relative_output_path(&result, limits, &path.display().to_string())?;
    Ok(result)
}

fn sanitize_component_path(component: &str, limits: ExtractionLimits) -> Result<PathBuf> {
    let result = PathBuf::from(sanitize_component(component)?);
    validate_relative_output_path(&result, limits, component)?;
    Ok(result)
}

fn sanitize_component(component: &str) -> Result<String> {
    let mut sanitized = String::with_capacity(component.len());
    for character in component.chars() {
        if character.is_control() || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            sanitized.push('_');
        } else {
            sanitized.push(character);
        }
    }
    while sanitized.ends_with([' ', '.']) {
        sanitized.pop();
        sanitized.push('_');
    }
    if sanitized.is_empty() || matches!(sanitized.as_str(), "." | "..") {
        sanitized.push('_');
    }
    if is_windows_reserved_name(&sanitized) {
        sanitized.insert(0, '_');
    }
    if sanitized.len() > MAX_PORTABLE_COMPONENT_BYTES {
        return Err(Error::invalid_data(format!(
            "path component is {} bytes, exceeding portable limit {MAX_PORTABLE_COMPONENT_BYTES}",
            sanitized.len()
        )));
    }
    Ok(sanitized)
}

fn validate_relative_output_path(
    path: &Path,
    limits: ExtractionLimits,
    original: &str,
) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(Error::invalid_data(format!(
            "archive entry path is empty or absolute: {original:?}"
        )));
    }
    let length = path.to_string_lossy().len();
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

fn decoded_leaf_path(path: &Path, wrapper: &str) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        Error::invalid_data(format!("wrapped path has no file name: {}", path.display()))
    })?;
    let value = file_name.to_string_lossy();
    let extensions: &[&str] = match wrapper {
        "gzip" => &[".gzip", ".gz"],
        "brotli" => &[".brotli", ".br"],
        _ => &[],
    };
    let decoded = extensions
        .iter()
        .find_map(|extension| {
            value
                .to_ascii_lowercase()
                .strip_suffix(extension)
                .map(|prefix| value[..prefix.len()].to_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("{value}.decoded"));
    Ok(path.parent().unwrap_or_else(|| Path::new("")).join(decoded))
}

fn suffixed_path(path: &Path, index: u64) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        Error::invalid_data(format!(
            "collision path has no file name: {}",
            path.display()
        ))
    })?;
    let component = suffixed_component(file_name.to_string_lossy().as_ref(), index)?;
    Ok(path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(component))
}

fn suffixed_component(component: &str, index: u64) -> Result<String> {
    let path = Path::new(component);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let extension = path.extension().map(|value| value.to_string_lossy());
    let mut output = format!("{stem}~{index}");
    if let Some(extension) = extension {
        output.push('.');
        output.push_str(&extension);
    }
    if output.len() > MAX_PORTABLE_COMPONENT_BYTES {
        return Err(Error::invalid_data(
            "collision suffix makes output component too long",
        ));
    }
    Ok(output)
}

fn nested_label(parent: &str, child: &str) -> String {
    format!("{parent}::{}", child.replace('\\', "/"))
}

fn paths_equal_portably(left: &Path, right: &Path) -> bool {
    left.components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .eq(right
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase()))
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
    let mut ancestor = path.to_owned();
    let mut suffix = PathBuf::new();
    loop {
        match safe_path_kind(&ancestor)? {
            ExistingKind::Missing => {
                let component = ancestor.file_name().ok_or_else(|| {
                    Error::invalid_data(format!(
                        "missing output ancestor has no name: {}",
                        ancestor.display()
                    ))
                })?;
                let mut new_suffix = PathBuf::from(component);
                new_suffix.push(suffix);
                suffix = new_suffix;
                if !ancestor.pop() {
                    return Ok(None);
                }
            }
            ExistingKind::Directory
            | ExistingKind::Regular
            | ExistingKind::Other
            | ExistingKind::Symlink => {
                return Ok(Some((ancestor, suffix)));
            }
        }
    }
}

fn create_secure_suffix(mut current: PathBuf, suffix: &Path) -> Result<()> {
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
    let path = root.join(relative);
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

    use crate::source::Region;

    use super::{
        ExtractionLimits, ExtractionOptions, PersistOutcome, TemporaryFile, extract_path,
        extract_region,
    };

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "assetstudio-extraction-{label}-{}-{timestamp}-{sequence}",
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
        ]);

        let report = extract_region(
            "web.data",
            Region::from_bytes(web),
            &output,
            ExtractionOptions::default(),
        )
        .unwrap();

        assert_eq!(report.extracted.len(), 1);
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
        assert_eq!(written.len(), 1, "{written:?}");
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
