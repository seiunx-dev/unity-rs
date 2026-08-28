use crate::endian::{Endian, EndianReader, checked_length};
use crate::source::Region;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebFile {
    pub signature: String,
    pub entries: Vec<WebFileEntry>,
    entry_read_limit: u64,
    region: Region,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebFileEntry {
    pub path: String,
    pub file_name: String,
    pub data_offset: u64,
    pub data_length: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebParseLimits {
    pub max_header_size: u64,
    pub max_entries: usize,
    pub max_path_length: usize,
    pub max_entry_read_size: u64,
}

impl Default for WebParseLimits {
    fn default() -> Self {
        Self {
            max_header_size: 256 * 1024 * 1024,
            max_entries: 1_000_000,
            max_path_length: 32_767,
            max_entry_read_size: 256 * 1024 * 1024,
        }
    }
}

impl WebFile {
    /// Reads the `WebData` directory without retaining every embedded payload.
    pub fn open(region: Region) -> Result<Self> {
        Self::open_with_limits(region, WebParseLimits::default())
    }

    pub fn open_with_limits(region: Region, limits: WebParseLimits) -> Result<Self> {
        let mut reader = EndianReader::new(region.cursor(), Endian::Little);
        reader.set_endian(Endian::Little);
        let signature = reader.read_c_string_required(32_767, "WebData signature")?;
        validate_signature(&signature)?;

        let header_length = checked_u64(reader.read_i32()?, "WebData header length")?;
        let stream_length = reader.len()?;
        let directory_start = reader.position()?;
        validate_header_length(header_length, directory_start, stream_length, limits)?;

        let mut entries = Vec::new();
        while reader.position()? < header_length {
            if entries.len() >= limits.max_entries {
                return Err(Error::invalid_data(format!(
                    "WebData entry count exceeds configured limit {}",
                    limits.max_entries
                )));
            }
            if entries.len() == entries.capacity() {
                entries.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot allocate WebData entry table: {error}"))
                })?;
            }
            entries.push(read_entry(
                &mut reader,
                header_length,
                stream_length,
                limits.max_path_length,
            )?);
        }

        if reader.position()? != header_length {
            return Err(Error::invalid_data(
                "WebData directory did not end at its declared header length",
            ));
        }

        Ok(Self {
            signature,
            entries,
            entry_read_limit: limits.max_entry_read_size,
            region,
        })
    }

    pub fn entry_region(&self, index: usize) -> Result<Region> {
        let entry = self.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("WebData entry index {index} is out of range"))
        })?;
        self.region.subregion(entry.data_offset, entry.data_length)
    }

    pub fn read_entry(&self, index: usize) -> Result<Vec<u8>> {
        let entry = self.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("WebData entry index {index} is out of range"))
        })?;
        if entry.data_length > self.entry_read_limit {
            return Err(Error::invalid_data(format!(
                "WebData entry is {} bytes, exceeding the configured read limit {}",
                entry.data_length, self.entry_read_limit
            )));
        }
        self.entry_region(index)?.read_to_vec(self.entry_read_limit)
    }
}

fn validate_signature(signature: &str) -> Result<()> {
    if matches!(signature, "UnityWebData1.0" | "TuanjieWebData1.0") {
        return Ok(());
    }
    Err(Error::invalid_data(format!(
        "unsupported WebData signature: {signature:?}"
    )))
}

fn validate_header_length(
    header_length: u64,
    directory_start: u64,
    stream_length: u64,
    limits: WebParseLimits,
) -> Result<()> {
    if header_length < directory_start || header_length > stream_length {
        return Err(Error::invalid_data(format!(
            "WebData header length {header_length} is outside the stream"
        )));
    }
    if header_length > limits.max_header_size {
        return Err(Error::invalid_data(format!(
            "WebData header length {header_length} exceeds configured limit {}",
            limits.max_header_size
        )));
    }
    Ok(())
}

fn read_entry(
    reader: &mut EndianReader<impl std::io::Read + std::io::Seek>,
    header_length: u64,
    stream_length: u64,
    max_path_length: usize,
) -> Result<WebFileEntry> {
    if header_length - reader.position()? < 12 {
        return Err(Error::invalid_data(
            "truncated WebData directory record header",
        ));
    }
    let data_offset = checked_u64(reader.read_i32()?, "WebData entry offset")?;
    let data_length = checked_u64(reader.read_i32()?, "WebData entry length")?;
    let path_length = checked_length(reader.read_i32()?, "WebData path")?;
    if path_length > max_path_length {
        return Err(Error::invalid_data(format!(
            "WebData path length {path_length} exceeds configured limit {max_path_length}"
        )));
    }
    let path_length_u64 = u64::try_from(path_length)
        .map_err(|_| Error::invalid_data("WebData path length does not fit in u64"))?;
    if path_length_u64 > header_length - reader.position()? {
        return Err(Error::invalid_data(
            "WebData path extends past the end of its directory",
        ));
    }
    let path = reader.read_utf8(path_length)?;
    let data_end = data_offset
        .checked_add(data_length)
        .ok_or_else(|| Error::invalid_data("WebData entry range overflowed"))?;
    if data_offset < header_length || data_end > stream_length {
        return Err(Error::invalid_data(format!(
            "WebData entry {path:?} range {data_offset}..{data_end} is outside payload area {header_length}..{stream_length}"
        )));
    }
    Ok(WebFileEntry {
        file_name: copy_portable_file_name(&path, "WebData file name")?,
        path,
        data_offset,
        data_length,
    })
}

fn checked_u64(value: i32, field: &str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| Error::invalid_data(format!("{field} cannot be negative: {value}")))
}

fn copy_portable_file_name(path: &str, field: &str) -> Result<String> {
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let mut copied = String::new();
    copied
        .try_reserve_exact(file_name.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    copied.push_str(file_name);
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use crate::source::Region;

    use super::{WebFile, WebParseLimits};

    fn sample_web_file() -> Vec<u8> {
        sample_web_file_with_path(b"folder/test.bin")
    }

    fn sample_web_file_with_path(path: &[u8]) -> Vec<u8> {
        let signature = b"UnityWebData1.0\0";
        let payload = b"payload";
        let header_length = signature.len() + 4 + 12 + path.len();
        let header_length_i32 = i32::try_from(header_length).unwrap();
        let payload_length_i32 = i32::try_from(payload.len()).unwrap();
        let path_length_i32 = i32::try_from(path.len()).unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(signature);
        bytes.extend_from_slice(&header_length_i32.to_le_bytes());
        bytes.extend_from_slice(&header_length_i32.to_le_bytes());
        bytes.extend_from_slice(&payload_length_i32.to_le_bytes());
        bytes.extend_from_slice(&path_length_i32.to_le_bytes());
        bytes.extend_from_slice(path);
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn reads_directory_and_payload() {
        let bytes = sample_web_file();
        let web_file = WebFile::open(Region::from_bytes(bytes)).unwrap();

        assert_eq!(web_file.signature, "UnityWebData1.0");
        assert_eq!(web_file.entries.len(), 1);
        assert_eq!(web_file.entries[0].path, "folder/test.bin");
        assert_eq!(web_file.entries[0].file_name, "test.bin");
        assert_eq!(web_file.read_entry(0).unwrap(), b"payload");
    }

    #[test]
    fn copies_a_unicode_file_name_after_a_windows_separator() {
        let path = "folder\\目录\\表.bin";
        let web_file = WebFile::open(Region::from_bytes(sample_web_file_with_path(
            path.as_bytes(),
        )))
        .unwrap();

        assert_eq!(web_file.entries[0].path, path);
        assert_eq!(web_file.entries[0].file_name, "表.bin");
    }

    #[test]
    fn rejects_entry_outside_stream() {
        let mut bytes = sample_web_file();
        let offset_position = b"UnityWebData1.0\0".len() + 4;
        bytes[offset_position..offset_position + 4].copy_from_slice(&999_i32.to_le_bytes());

        assert!(WebFile::open(Region::from_bytes(bytes)).is_err());
    }

    #[test]
    fn rejects_negative_path_length() {
        let mut bytes = sample_web_file();
        let path_length_position = b"UnityWebData1.0\0".len() + 4 + 8;
        bytes[path_length_position..path_length_position + 4]
            .copy_from_slice(&(-1_i32).to_le_bytes());

        assert!(WebFile::open(Region::from_bytes(bytes)).is_err());
    }

    #[test]
    fn rejects_payload_overlapping_directory() {
        let mut bytes = sample_web_file();
        let offset_position = b"UnityWebData1.0\0".len() + 4;
        bytes[offset_position..offset_position + 4].copy_from_slice(&0_i32.to_le_bytes());

        assert!(WebFile::open(Region::from_bytes(bytes)).is_err());
    }

    #[test]
    fn enforces_entry_and_path_limits() {
        let bytes = sample_web_file();
        let limits = WebParseLimits {
            max_entries: 0,
            ..WebParseLimits::default()
        };
        assert!(WebFile::open_with_limits(Region::from_bytes(bytes.clone()), limits).is_err());

        let limits = WebParseLimits {
            max_path_length: 4,
            ..WebParseLimits::default()
        };
        assert!(WebFile::open_with_limits(Region::from_bytes(bytes), limits).is_err());
    }

    #[test]
    fn enforces_entry_materialization_limit() {
        let bytes = sample_web_file();
        let limits = WebParseLimits {
            max_entry_read_size: 2,
            ..WebParseLimits::default()
        };
        let web_file = WebFile::open_with_limits(Region::from_bytes(bytes), limits).unwrap();
        assert!(web_file.read_entry(0).is_err());
    }
}
