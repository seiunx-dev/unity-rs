use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::{Arc, Mutex};

use flate2::read::MultiGzDecoder;

use crate::source::Region;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressionLimits {
    pub maximum_input_bytes: u64,
    pub maximum_output_bytes: u64,
    pub maximum_zip_entries: usize,
    pub maximum_zip_path_bytes: usize,
    pub maximum_zip_entry_bytes: u64,
    pub maximum_zip_total_bytes: u64,
}

impl Default for CompressionLimits {
    fn default() -> Self {
        Self {
            maximum_input_bytes: 512 * 1024 * 1024,
            maximum_output_bytes: 1024 * 1024 * 1024,
            maximum_zip_entries: 1_000_000,
            maximum_zip_path_bytes: 32_767,
            maximum_zip_entry_bytes: 512 * 1024 * 1024,
            maximum_zip_total_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

pub fn decompress_gzip(region: &Region, limits: CompressionLimits) -> Result<Region> {
    let input = region.read_to_vec(limits.maximum_input_bytes)?;
    let decoder = MultiGzDecoder::new(Cursor::new(input));
    let output = read_bounded(decoder, limits.maximum_output_bytes, "gzip output")?;
    Ok(Region::from_bytes(output))
}

pub fn decompress_brotli(region: &Region, limits: CompressionLimits) -> Result<Region> {
    let input = region.read_to_vec(limits.maximum_input_bytes)?;
    let decoder = brotli::Decompressor::new(Cursor::new(input), 64 * 1024);
    let output = read_bounded(decoder, limits.maximum_output_bytes, "Brotli output")?;
    Ok(Region::from_bytes(output))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    archive_index: usize,
    pub path: String,
    pub compressed_size: u64,
    pub size: u64,
}

pub struct ZipContainer {
    /// The parsed central directory, kept rather than re-parsed.
    ///
    /// Reading an entry needs `&mut` on the archive while the container itself
    /// is shared, so it sits behind a lock. It used to be reopened per entry,
    /// which re-parsed the whole central directory every time and made
    /// extraction quadratic in the entry count: a 20,000-entry archive spent
    /// 29 seconds of CPU doing nothing else.
    archive: Mutex<zip::ZipArchive<Cursor<Arc<[u8]>>>>,
    pub entries: Vec<ZipEntry>,
    maximum_entry_bytes: u64,
}

impl std::fmt::Debug for ZipContainer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The archive is a cursor over bytes with nothing readable to say.
        formatter
            .debug_struct("ZipContainer")
            .field("entries", &self.entries)
            .field("maximum_entry_bytes", &self.maximum_entry_bytes)
            .finish_non_exhaustive()
    }
}

impl ZipContainer {
    pub fn open(region: &Region, limits: CompressionLimits) -> Result<Self> {
        let bytes: Arc<[u8]> = region.read_to_vec(limits.maximum_input_bytes)?.into();
        let mut archive = zip::ZipArchive::new(Cursor::new(Arc::clone(&bytes)))
            .map_err(|error| invalid_zip("cannot read ZIP central directory", &error))?;
        if archive.len() > limits.maximum_zip_entries {
            return Err(Error::invalid_data(format!(
                "ZIP contains {} records, exceeding limit {}",
                archive.len(),
                limits.maximum_zip_entries
            )));
        }
        if archive
            .decompressed_size()
            .is_some_and(|size| size > u128::from(limits.maximum_zip_total_bytes))
        {
            return Err(Error::invalid_data(format!(
                "ZIP declared output exceeds the {} byte total limit",
                limits.maximum_zip_total_bytes
            )));
        }
        if archive
            .has_overlapping_files()
            .map_err(|error| invalid_zip("cannot validate ZIP record ranges", &error))?
        {
            return Err(Error::invalid_data("ZIP contains overlapping file records"));
        }

        let mut entries = Vec::new();
        entries.try_reserve_exact(archive.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate ZIP entry table: {error}"))
        })?;
        let mut total_size = 0_u64;
        for archive_index in 0..archive.len() {
            let file = archive
                .by_index(archive_index)
                .map_err(|error| invalid_zip("cannot read ZIP entry", &error))?;
            if file.is_dir() {
                continue;
            }
            if file.encrypted() {
                return Err(Error::unsupported(format!(
                    "encrypted ZIP entry at index {archive_index}"
                )));
            }
            if file.is_symlink() {
                return Err(Error::unsupported(format!(
                    "symbolic-link ZIP entry at index {archive_index}"
                )));
            }
            if !matches!(
                file.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            ) {
                return Err(Error::unsupported(format!(
                    "ZIP compression {:?} for entry at index {archive_index}",
                    file.compression()
                )));
            }
            let enclosed_path = file.enclosed_name().ok_or_else(|| {
                Error::invalid_data(format!(
                    "ZIP entry at index {archive_index} has an unsafe path"
                ))
            })?;
            let path = portable_zip_path(&enclosed_path)?;
            if path.len() > limits.maximum_zip_path_bytes {
                return Err(Error::invalid_data(format!(
                    "ZIP entry path is {} bytes, exceeding limit {}",
                    path.len(),
                    limits.maximum_zip_path_bytes
                )));
            }
            if file.size() > limits.maximum_zip_entry_bytes {
                return Err(Error::invalid_data(format!(
                    "ZIP entry {path:?} is {} bytes, exceeding limit {}",
                    file.size(),
                    limits.maximum_zip_entry_bytes
                )));
            }
            total_size = total_size
                .checked_add(file.size())
                .ok_or_else(|| Error::invalid_data("ZIP total output size overflowed"))?;
            if total_size > limits.maximum_zip_total_bytes {
                return Err(Error::invalid_data(format!(
                    "ZIP declared output exceeds the {} byte total limit",
                    limits.maximum_zip_total_bytes
                )));
            }
            entries.push(ZipEntry {
                archive_index,
                path,
                compressed_size: file.compressed_size(),
                size: file.size(),
            });
        }

        Ok(Self {
            archive: Mutex::new(archive),
            entries,
            maximum_entry_bytes: limits.maximum_zip_entry_bytes,
        })
    }

    pub fn read_entry(&self, index: usize) -> Result<Region> {
        let entry = self.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("ZIP entry index {index} is out of range"))
        })?;
        let mut archive = self.archive.lock().map_err(|_| {
            Error::invalid_data("ZIP archive lock was poisoned by an earlier panic")
        })?;
        let file = archive
            .by_index(entry.archive_index)
            .map_err(|error| invalid_zip("cannot read ZIP entry", &error))?;
        let output = read_bounded(file, self.maximum_entry_bytes, "ZIP entry output")?;
        drop(archive);
        if u64::try_from(output.len()).ok() != Some(entry.size) {
            return Err(Error::invalid_data(format!(
                "ZIP entry {:?} decoded to {} bytes; expected {}",
                entry.path,
                output.len(),
                entry.size
            )));
        }
        Ok(Region::from_bytes(output))
    }
}

fn portable_zip_path(path: &Path) -> Result<String> {
    let mut value = String::new();
    for component in path.components() {
        let component = component.as_os_str().to_str().ok_or_else(|| {
            Error::invalid_data("ZIP entry path is not valid Unicode after decoding")
        })?;
        if !value.is_empty() {
            value.push('/');
        }
        value.push_str(component);
    }
    if value.is_empty() {
        return Err(Error::invalid_data("ZIP file entry has an empty path"));
    }
    Ok(value)
}

fn read_bounded<R: Read>(mut reader: R, maximum: u64, field: &str) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let initial_capacity = usize::try_from(maximum.min(64 * 1024))
        .expect("initial decompression capacity is bounded by 64 KiB");
    output
        .try_reserve_exact(initial_capacity)
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate initial {field} buffer: {error}"))
        })?;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            Error::invalid_data(format!("failed while decoding {field}: {error}"))
        })?;
        if count == 0 {
            break;
        }
        let new_length = output
            .len()
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data(format!("{field} length overflowed")))?;
        if u64::try_from(new_length).map_or(true, |length| length > maximum) {
            return Err(Error::invalid_data(format!(
                "{field} exceeds the {maximum} byte limit"
            )));
        }
        output
            .try_reserve(count)
            .map_err(|error| Error::invalid_data(format!("cannot grow {field} buffer: {error}")))?;
        output.extend_from_slice(&buffer[..count]);
    }
    Ok(output)
}

fn invalid_zip(context: &str, error: &zip::result::ZipError) -> Error {
    Error::invalid_data(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use zip::write::SimpleFileOptions;

    use crate::source::Region;

    use super::{CompressionLimits, ZipContainer, decompress_brotli, decompress_gzip};

    #[test]
    fn decodes_concatenated_gzip_members_with_output_limit() {
        let mut bytes = gzip_member(b"first");
        bytes.extend_from_slice(&gzip_member(b"second"));
        let region = Region::from_bytes(bytes);
        assert_eq!(
            decompress_gzip(&region, CompressionLimits::default())
                .unwrap()
                .read_to_vec(64)
                .unwrap(),
            b"firstsecond"
        );
        let limits = CompressionLimits {
            maximum_output_bytes: 10,
            ..CompressionLimits::default()
        };
        assert!(decompress_gzip(&region, limits).is_err());
    }

    #[test]
    fn decodes_brotli_with_output_limit() {
        let mut encoder = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 20);
        encoder.write_all(b"brotli payload").unwrap();
        let compressed = encoder.into_inner();
        let region = Region::from_bytes(compressed);
        assert_eq!(
            decompress_brotli(&region, CompressionLimits::default())
                .unwrap()
                .read_to_vec(64)
                .unwrap(),
            b"brotli payload"
        );
        let limits = CompressionLimits {
            maximum_output_bytes: 3,
            ..CompressionLimits::default()
        };
        assert!(decompress_brotli(&region, limits).is_err());
    }

    #[test]
    fn lists_and_decodes_safe_zip_entries() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("folder/data.bin", options).unwrap();
        writer.write_all(b"zip payload").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let archive =
            ZipContainer::open(&Region::from_bytes(bytes), CompressionLimits::default()).unwrap();

        assert_eq!(archive.entries.len(), 1);
        assert_eq!(archive.entries[0].path, "folder/data.bin");
        assert_eq!(
            archive.read_entry(0).unwrap().read_to_vec(64).unwrap(),
            b"zip payload"
        );
    }

    #[test]
    fn rejects_unsafe_zip_paths_and_declared_output_limits() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("../../escape", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"x").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(
            ZipContainer::open(&Region::from_bytes(bytes), CompressionLimits::default()).is_err()
        );

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("large", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"large").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let limits = CompressionLimits {
            maximum_zip_entry_bytes: 4,
            ..CompressionLimits::default()
        };
        assert!(ZipContainer::open(&Region::from_bytes(bytes), limits).is_err());
    }

    fn gzip_member(payload: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(payload).unwrap();
        encoder.finish().unwrap()
    }
}
