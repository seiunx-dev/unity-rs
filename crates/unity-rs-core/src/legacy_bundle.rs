use std::io::{Read, Write};

use crate::bundle::{BundleEntry, BundleHeader, BundleParseLimits};
use crate::endian::{Endian, EndianReader, checked_length};
use crate::source::Region;
use crate::{Error, Result};

/// One download level declared by a pre-UnityFS `UnityWeb` or `UnityRaw`
/// bundle. Legacy readers use the final level as the complete payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyBundleLevel {
    pub compressed_size: u32,
    pub uncompressed_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyBundleHeader {
    pub common: BundleHeader,
    pub hash: Option<[u8; 16]>,
    pub crc: Option<u32>,
    pub minimum_streamed_bytes: u32,
    pub header_size: u32,
    pub levels_to_download_before_streaming: u32,
    pub levels: Vec<LegacyBundleLevel>,
    pub complete_file_size: Option<u32>,
    pub file_info_header_size: Option<u32>,
}

/// Source-bounded reader for the legacy (format versions 1 through 5)
/// `UnityWeb` and `UnityRaw` bundle layouts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyBundle {
    pub header: LegacyBundleHeader,
    pub entries: Vec<BundleEntry>,
    /// Position of the compressed/raw payload within the input bundle.
    pub data_offset: u64,
    /// Position immediately after the declared bundle, relative to its input.
    pub bundle_end: u64,
    content: Region,
    entry_read_limit: u64,
}

impl LegacyBundle {
    pub fn open(region: &Region) -> Result<Self> {
        Self::open_with_limits(region, BundleParseLimits::default())
    }

    #[allow(clippy::too_many_lines)]
    pub fn open_with_limits(region: &Region, limits: BundleParseLimits) -> Result<Self> {
        let mut reader = EndianReader::new(region.cursor(), Endian::Big);
        let common = BundleHeader::read(&mut reader)?;
        if !matches!(common.signature.as_str(), "UnityWeb" | "UnityRaw") {
            return Err(Error::unsupported(format!(
                "legacy bundle signature {:?}",
                common.signature
            )));
        }
        if !(1..=5).contains(&common.version) {
            return Err(Error::unsupported(format!(
                "legacy {} format version {}",
                common.signature, common.version
            )));
        }

        let (hash, crc) = if common.version >= 4 {
            let hash: [u8; 16] = reader
                .read_bytes(16)?
                .try_into()
                .map_err(|_| Error::invalid_data("failed to read legacy bundle hash"))?;
            (Some(hash), Some(reader.read_u32()?))
        } else {
            (None, None)
        };
        let minimum_streamed_bytes = reader.read_u32()?;
        let header_size = reader.read_u32()?;
        let levels_to_download_before_streaming = reader.read_u32()?;
        let level_count =
            checked_legacy_count(reader.read_i32()?, "legacy bundle level", limits.max_blocks)?;
        let level_table_bytes = u64::try_from(level_count)
            .expect("level count fits in u64")
            .checked_mul(8)
            .ok_or_else(|| Error::invalid_data("legacy bundle level table size overflowed"))?;
        if level_table_bytes > reader.remaining()? {
            return Err(Error::invalid_data(format!(
                "legacy bundle level table needs {level_table_bytes} bytes but exceeds the input"
            )));
        }
        let mut levels = Vec::new();
        levels.try_reserve_exact(level_count).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate legacy bundle level table: {error}"
            ))
        })?;
        for _ in 0..level_count {
            levels.push(LegacyBundleLevel {
                compressed_size: reader.read_u32()?,
                uncompressed_size: reader.read_u32()?,
            });
        }
        let final_level = levels
            .last()
            .copied()
            .ok_or_else(|| Error::invalid_data("legacy bundle must declare at least one level"))?;
        let complete_file_size = (common.version >= 2)
            .then(|| reader.read_u32())
            .transpose()?;
        let file_info_header_size = (common.version >= 3)
            .then(|| reader.read_u32())
            .transpose()?;

        let metadata_end = reader.position()?;
        let data_offset = u64::from(header_size);
        if data_offset < metadata_end {
            return Err(Error::invalid_data(format!(
                "legacy bundle header size {data_offset} precedes parsed metadata end {metadata_end}"
            )));
        }
        if u64::from(final_level.compressed_size) > limits.max_compressed_block_size {
            return Err(Error::invalid_data(format!(
                "legacy bundle compressed payload size {} exceeds configured limit {}",
                final_level.compressed_size, limits.max_compressed_block_size
            )));
        }
        if u64::from(final_level.uncompressed_size) > limits.max_uncompressed_block_size {
            return Err(Error::invalid_data(format!(
                "legacy bundle uncompressed payload size {} exceeds configured limit {}",
                final_level.uncompressed_size, limits.max_uncompressed_block_size
            )));
        }
        let data_end = data_offset
            .checked_add(u64::from(final_level.compressed_size))
            .ok_or_else(|| Error::invalid_data("legacy bundle payload range overflowed"))?;
        let bundle_end = complete_file_size.map_or(data_end, u64::from);
        if data_end > bundle_end {
            return Err(Error::invalid_data(format!(
                "legacy bundle payload ends at {data_end}, beyond declared file size {bundle_end}"
            )));
        }
        if bundle_end > region.len() {
            return Err(Error::invalid_data(format!(
                "legacy bundle ends at {bundle_end}, beyond input length {}",
                region.len()
            )));
        }

        let stored_payload =
            region.subregion(data_offset, u64::from(final_level.compressed_size))?;
        let content = if common.signature == "UnityWeb" {
            let compressed = stored_payload.read_to_vec(u64::from(final_level.compressed_size))?;
            Region::from_bytes(decompress_lzma_alone(
                &compressed,
                u64::from(final_level.uncompressed_size),
                limits.max_lzma_memory_kib,
            )?)
        } else {
            if final_level.compressed_size != final_level.uncompressed_size {
                return Err(Error::invalid_data(
                    "UnityRaw payload has different compressed and uncompressed sizes",
                ));
            }
            stored_payload
        };

        let entries = parse_directory(&content, file_info_header_size, limits)?;
        Ok(Self {
            header: LegacyBundleHeader {
                common,
                hash,
                crc,
                minimum_streamed_bytes,
                header_size,
                levels_to_download_before_streaming,
                levels,
                complete_file_size,
                file_info_header_size,
            },
            entries,
            data_offset,
            bundle_end,
            content,
            entry_read_limit: limits.max_entry_read_size,
        })
    }

    #[must_use]
    pub fn entry_by_path(&self, path: &str) -> Option<&BundleEntry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    pub fn entry_region(&self, index: usize) -> Result<Region> {
        let entry = self.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("legacy bundle entry index {index} is out of range"))
        })?;
        self.content.subregion(entry.offset, entry.size)
    }

    pub fn copy_entry<W: Write>(&self, index: usize, output: &mut W) -> Result<u64> {
        let entry = self.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("legacy bundle entry index {index} is out of range"))
        })?;
        self.content.copy_range(entry.offset, entry.size, output)
    }

    pub fn read_entry(&self, index: usize) -> Result<Vec<u8>> {
        let entry = self.entries.get(index).ok_or_else(|| {
            Error::invalid_data(format!("legacy bundle entry index {index} is out of range"))
        })?;
        if entry.size > self.entry_read_limit {
            return Err(Error::invalid_data(format!(
                "legacy bundle entry is {} bytes, exceeding the configured read limit {}",
                entry.size, self.entry_read_limit
            )));
        }
        self.entry_region(index)?.read_to_vec(self.entry_read_limit)
    }
}

fn parse_directory(
    content: &Region,
    declared_header_size: Option<u32>,
    limits: BundleParseLimits,
) -> Result<Vec<BundleEntry>> {
    let mut reader = EndianReader::new(content.cursor(), Endian::Big);
    let entry_count = checked_legacy_count(
        reader.read_i32()?,
        "legacy bundle entry",
        limits.max_entries,
    )?;
    let maximum_entries_in_input = usize::try_from(reader.remaining()? / 9).unwrap_or(usize::MAX);
    if entry_count > maximum_entries_in_input {
        return Err(Error::invalid_data(format!(
            "legacy bundle entry count {entry_count} exceeds payload capacity {maximum_entries_in_input}"
        )));
    }

    let mut entries = Vec::new();
    entries.try_reserve_exact(entry_count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate legacy bundle directory: {error}"))
    })?;
    for _ in 0..entry_count {
        let path = reader.read_c_string_required(limits.max_path_length, "legacy bundle path")?;
        let offset = u64::from(reader.read_u32()?);
        let size = u64::from(reader.read_u32()?);
        let end = offset
            .checked_add(size)
            .ok_or_else(|| Error::invalid_data("legacy bundle entry range overflowed"))?;
        if end > content.len() {
            return Err(Error::invalid_data(format!(
                "legacy bundle entry {path:?} ends at {end}, beyond payload length {}",
                content.len()
            )));
        }
        let file_name = portable_file_name(&path).to_owned();
        entries.push(BundleEntry {
            offset,
            size,
            flags: 0,
            path,
            file_name,
        });
    }

    let parsed_directory_end = reader.position()?;
    let directory_end = declared_header_size.map_or(parsed_directory_end, u64::from);
    if directory_end < parsed_directory_end {
        return Err(Error::invalid_data(format!(
            "legacy file-info header size {directory_end} precedes parsed directory end {parsed_directory_end}"
        )));
    }
    if directory_end > content.len() {
        return Err(Error::invalid_data(format!(
            "legacy file-info header ends at {directory_end}, beyond payload length {}",
            content.len()
        )));
    }
    if let Some(entry) = entries.iter().find(|entry| entry.offset < directory_end) {
        return Err(Error::invalid_data(format!(
            "legacy bundle entry {:?} starts inside its directory header",
            entry.path
        )));
    }
    Ok(entries)
}

fn decompress_lzma_alone(
    input: &[u8],
    expected_size: u64,
    maximum_memory_kib: u32,
) -> Result<Vec<u8>> {
    let header = input.get(..13).ok_or_else(|| {
        Error::invalid_data("legacy UnityWeb LZMA stream is shorter than its 13-byte header")
    })?;
    let properties = header[0];
    let dictionary_size = u32::from_le_bytes(
        header[1..5]
            .try_into()
            .expect("the LZMA dictionary slice is exactly four bytes"),
    );
    let declared_size = u64::from_le_bytes(
        header[5..13]
            .try_into()
            .expect("the LZMA output-size slice is exactly eight bytes"),
    );
    if declared_size != expected_size && declared_size != u64::MAX {
        return Err(Error::invalid_data(format!(
            "legacy UnityWeb LZMA header declares {declared_size} bytes; level table declares {expected_size}"
        )));
    }
    let required_memory = lzma_rust2::lzma_get_memory_usage_by_props(dictionary_size, properties)
        .map_err(|error| {
        Error::invalid_data(format!("invalid legacy UnityWeb LZMA properties: {error}"))
    })?;
    if required_memory > maximum_memory_kib {
        return Err(Error::invalid_data(format!(
            "legacy UnityWeb LZMA decoder needs {required_memory} KiB, exceeding limit {maximum_memory_kib} KiB"
        )));
    }

    let output_length = usize::try_from(expected_size).map_err(|_| {
        Error::invalid_data("legacy UnityWeb payload is too large for this platform")
    })?;
    let mut output = Vec::new();
    output.try_reserve_exact(output_length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate legacy UnityWeb output: {error}"))
    })?;
    output.resize(output_length, 0);
    let mut decoder = lzma_rust2::LzmaReader::new_mem_limit(
        std::io::Cursor::new(input),
        maximum_memory_kib,
        None,
    )
    .map_err(|error| {
        Error::invalid_data(format!("invalid legacy UnityWeb LZMA stream: {error}"))
    })?;
    decoder.read_exact(&mut output).map_err(|error| {
        Error::invalid_data(format!("truncated legacy UnityWeb LZMA stream: {error}"))
    })?;
    let mut trailing_output = [0_u8; 1];
    if decoder.read(&mut trailing_output).map_err(|error| {
        Error::invalid_data(format!("invalid legacy UnityWeb LZMA ending: {error}"))
    })? != 0
    {
        return Err(Error::invalid_data(format!(
            "legacy UnityWeb LZMA decoder produced more than {expected_size} bytes"
        )));
    }
    Ok(output)
}

fn checked_legacy_count(value: i32, field: &str, maximum: usize) -> Result<usize> {
    let count = checked_length(value, field)?;
    if count > maximum {
        return Err(Error::invalid_data(format!(
            "{field} count {count} exceeds configured limit {maximum}"
        )));
    }
    Ok(count)
}

fn portable_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use crate::bundle::BundleParseLimits;
    use crate::source::Region;

    use super::LegacyBundle;

    const PAYLOAD: &[u8] = b"legacy-rust-payload";

    #[test]
    fn reads_uncompressed_unity_raw_bundle() {
        let bytes = make_raw_bundle(3, 1, None);
        let bundle = LegacyBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert_eq!(bundle.header.common.signature, "UnityRaw");
        assert_eq!(bundle.header.common.version, 3);
        assert_eq!(bundle.header.levels.len(), 1);
        assert_eq!(bundle.entries.len(), 1);
        assert_eq!(bundle.entries[0].path, "folder/data.bin");
        assert_eq!(bundle.entries[0].file_name, "data.bin");
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn reads_version_four_hash_and_multiple_download_levels() {
        let bytes = make_raw_bundle(4, 2, None);
        let bundle = LegacyBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert_eq!(bundle.header.hash, Some([0x5a; 16]));
        assert_eq!(bundle.header.crc, Some(0x1234_5678));
        assert_eq!(bundle.header.levels.len(), 2);
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn reads_lzma_alone_unity_web_bundle() {
        let bytes = make_web_bundle();
        let bundle = LegacyBundle::open(&Region::from_bytes(bytes)).unwrap();

        assert_eq!(bundle.header.common.signature, "UnityWeb");
        assert_eq!(bundle.entries.len(), 1);
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn accepts_lzma_unknown_size_sentinel_with_bounded_output() {
        let mut bytes = make_web_bundle();
        let data_offset = web_data_offset(&bytes);
        bytes[data_offset + 5..data_offset + 13].copy_from_slice(&u64::MAX.to_le_bytes());

        let bundle = LegacyBundle::open(&Region::from_bytes(bytes)).unwrap();
        assert_eq!(bundle.read_entry(0).unwrap(), PAYLOAD);
    }

    #[test]
    fn validates_lzma_output_size_and_memory_limit() {
        let mut mismatched_size = make_web_bundle();
        let data_offset = web_data_offset(&mismatched_size);
        mismatched_size[data_offset + 5..data_offset + 13].copy_from_slice(&46_u64.to_le_bytes());
        assert!(LegacyBundle::open(&Region::from_bytes(mismatched_size)).is_err());

        assert!(
            LegacyBundle::open_with_limits(
                &Region::from_bytes(make_web_bundle()),
                BundleParseLimits {
                    max_lzma_memory_kib: 1,
                    ..BundleParseLimits::default()
                },
            )
            .is_err()
        );
    }

    #[test]
    fn enforces_entry_and_payload_limits() {
        let bytes = make_raw_bundle(3, 1, None);
        let error = LegacyBundle::open_with_limits(
            &Region::from_bytes(bytes.clone()),
            BundleParseLimits {
                max_entry_read_size: 1,
                ..BundleParseLimits::default()
            },
        )
        .unwrap()
        .read_entry(0)
        .unwrap_err();
        assert!(error.to_string().contains("read limit"));

        assert!(
            LegacyBundle::open_with_limits(
                &Region::from_bytes(bytes),
                BundleParseLimits {
                    max_uncompressed_block_size: 1,
                    ..BundleParseLimits::default()
                },
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_invalid_level_table_and_entry_ranges() {
        let mut no_levels = make_raw_bundle(3, 1, None);
        let level_count_position = common_header_length() + 12;
        no_levels[level_count_position..level_count_position + 4]
            .copy_from_slice(&0_i32.to_be_bytes());
        assert!(LegacyBundle::open(&Region::from_bytes(no_levels)).is_err());

        let mut bad_entry = make_raw_bundle(3, 1, None);
        let content = legacy_content();
        let payload_offset = bad_entry.len() - content.len();
        let entry_offset_position = payload_offset + 4 + b"folder/data.bin\0".len();
        bad_entry[entry_offset_position..entry_offset_position + 4]
            .copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(LegacyBundle::open(&Region::from_bytes(bad_entry)).is_err());
    }

    #[test]
    fn rejects_mismatched_raw_sizes_and_truncated_payload() {
        let content_size = u32::try_from(legacy_content().len()).unwrap();
        let bytes = make_raw_bundle(3, 1, Some(content_size - 1));
        assert!(LegacyBundle::open(&Region::from_bytes(bytes)).is_err());

        let mut truncated = make_raw_bundle(3, 1, None);
        truncated.pop();
        assert!(LegacyBundle::open(&Region::from_bytes(truncated)).is_err());
    }

    fn common_header_length() -> usize {
        b"UnityRaw\0".len() + 4 + b"3.x.x\0".len() + b"3.5.0f5\0".len()
    }

    fn common_web_header_length() -> usize {
        b"UnityWeb\0".len() + 4 + b"3.x.x\0".len() + b"3.5.0f5\0".len()
    }

    fn web_data_offset(bytes: &[u8]) -> usize {
        usize::try_from(u32::from_be_bytes(
            bytes[common_web_header_length() + 4..common_web_header_length() + 8]
                .try_into()
                .unwrap(),
        ))
        .unwrap()
    }

    fn legacy_content() -> Vec<u8> {
        let path = b"folder/data.bin\0";
        let directory_size = 4 + path.len() + 8;
        let mut content = Vec::new();
        content.extend_from_slice(&1_i32.to_be_bytes());
        content.extend_from_slice(path);
        content.extend_from_slice(&u32::try_from(directory_size).unwrap().to_be_bytes());
        content.extend_from_slice(&u32::try_from(PAYLOAD.len()).unwrap().to_be_bytes());
        content.extend_from_slice(PAYLOAD);
        content
    }

    fn make_raw_bundle(version: u32, level_count: usize, raw_size: Option<u32>) -> Vec<u8> {
        let content = legacy_content();
        let content_size = u32::try_from(content.len()).unwrap();
        let file_info_size = u32::try_from(content.len() - PAYLOAD.len()).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityRaw\0");
        bytes.extend_from_slice(&version.to_be_bytes());
        bytes.extend_from_slice(b"3.x.x\0");
        bytes.extend_from_slice(b"3.5.0f5\0");
        if version >= 4 {
            bytes.extend_from_slice(&[0x5a; 16]);
            bytes.extend_from_slice(&0x1234_5678_u32.to_be_bytes());
        }
        let header_size_position = bytes.len() + 4;
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&i32::try_from(level_count).unwrap().to_be_bytes());
        for index in 0..level_count {
            let size = if index + 1 == level_count {
                raw_size.unwrap_or(content_size)
            } else {
                1
            };
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(&content_size.to_be_bytes());
        }
        let complete_file_size_position = (version >= 2).then_some(bytes.len());
        if version >= 2 {
            bytes.extend_from_slice(&0_u32.to_be_bytes());
        }
        if version >= 3 {
            bytes.extend_from_slice(&file_info_size.to_be_bytes());
        }
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        let header_size = u32::try_from(bytes.len()).unwrap();
        bytes[header_size_position..header_size_position + 4]
            .copy_from_slice(&header_size.to_be_bytes());
        bytes.extend_from_slice(&content);
        if let Some(position) = complete_file_size_position {
            let complete_size = u32::try_from(bytes.len()).unwrap();
            bytes[position..position + 4].copy_from_slice(&complete_size.to_be_bytes());
        }
        bytes
    }

    fn make_web_bundle() -> Vec<u8> {
        #[rustfmt::skip]
        const COMPRESSED: &[u8] = &[
            0x5d, 0x00, 0x00, 0x80, 0x00, 0x2f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x67, 0xfe, 0xad, 0x46, 0x89, 0x7a, 0xb5, 0x64, 0x53,
            0x95, 0x4d, 0x45, 0x16, 0x74, 0x85, 0x99, 0xb1, 0x49, 0x59, 0x05, 0xe2,
            0x48, 0x15, 0x25, 0x0e, 0xae, 0x2d, 0x31, 0xec, 0x29, 0xfd, 0x4f, 0x21,
            0xa9, 0x60, 0x23, 0x9e, 0x5a, 0xf9, 0xee, 0x0a, 0x3a, 0xcc, 0x01, 0xf2,
            0xff, 0xff, 0xf6, 0xc0, 0x40, 0x00,
        ];
        let content = legacy_content();
        let file_info_size = u32::try_from(content.len() - PAYLOAD.len()).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"UnityWeb\0");
        bytes.extend_from_slice(&3_u32.to_be_bytes());
        bytes.extend_from_slice(b"3.x.x\0");
        bytes.extend_from_slice(b"3.5.0f5\0");
        let header_size_position = bytes.len() + 4;
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(COMPRESSED.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(content.len()).unwrap().to_be_bytes());
        let complete_file_size_position = bytes.len();
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&file_info_size.to_be_bytes());
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        let header_size = u32::try_from(bytes.len()).unwrap();
        bytes[header_size_position..header_size_position + 4]
            .copy_from_slice(&header_size.to_be_bytes());
        bytes.extend_from_slice(COMPRESSED);
        let complete_file_size = u32::try_from(bytes.len()).unwrap();
        bytes[complete_file_size_position..complete_file_size_position + 4]
            .copy_from_slice(&complete_file_size.to_be_bytes());
        bytes
    }
}
