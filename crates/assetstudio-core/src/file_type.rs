use std::fmt;

pub const HEADER_SCAN_LENGTH: usize = 1_152;

const GZIP_MAGIC: &[u8] = &[0x1f, 0x8b];
const BROTLI_MAGIC: &[u8] = b"brotli";
const ZIP_MAGIC: &[u8] = &[0x50, 0x4b, 0x03, 0x04];
const ZIP_SPANNED_MAGIC: &[u8] = &[0x50, 0x4b, 0x07, 0x08];
const UNITY_FS_MAGIC: &[u8] = b"UnityFS\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    AssetsFile,
    BundleFile,
    WebFile,
    ResourceFile,
    GzipFile,
    BrotliFile,
    ZipFile,
}

impl fmt::Display for FileType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AssetsFile => "AssetsFile",
            Self::BundleFile => "BundleFile",
            Self::WebFile => "WebFile",
            Self::ResourceFile => "ResourceFile",
            Self::GzipFile => "GZipFile",
            Self::BrotliFile => "BrotliFile",
            Self::ZipFile => "ZipFile",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDetection {
    pub file_type: FileType,
    /// Offset of an embedded bundle. Zero for ordinary files.
    pub data_offset: u64,
}

/// Classifies a Unity asset input from its leading bytes and total file size.
///
/// `header` should contain up to [`HEADER_SCAN_LENGTH`] bytes from offset zero.
#[must_use]
pub fn detect_file_type(header: &[u8], file_size: u64) -> FileDetection {
    let signature = read_c_string(header, 20);
    match signature.as_str() {
        "UnityWeb" | "UnityRaw" | "UnityArchive" | "UnityFS" => FileDetection {
            file_type: FileType::BundleFile,
            data_offset: 0,
        },
        "UnityWebData1.0" | "TuanjieWebData1.0" => FileDetection {
            file_type: FileType::WebFile,
            data_offset: 0,
        },
        _ => detect_by_magic(header, file_size),
    }
}

fn detect_by_magic(header: &[u8], file_size: u64) -> FileDetection {
    if header.starts_with(GZIP_MAGIC) {
        return detection(FileType::GzipFile);
    }
    if header.get(32..38) == Some(BROTLI_MAGIC) {
        return detection(FileType::BrotliFile);
    }
    if is_serialized_file(header, file_size) {
        return detection(FileType::AssetsFile);
    }
    if header.starts_with(ZIP_MAGIC) || header.starts_with(ZIP_SPANNED_MAGIC) {
        return detection(FileType::ZipFile);
    }
    if let Some(offset) = bundle_data_offset(header, file_size) {
        return FileDetection {
            file_type: FileType::BundleFile,
            data_offset: offset as u64,
        };
    }
    detection(FileType::ResourceFile)
}

const fn detection(file_type: FileType) -> FileDetection {
    FileDetection {
        file_type,
        data_offset: 0,
    }
}

#[must_use]
pub fn is_serialized_file(header: &[u8], file_size: u64) -> bool {
    if file_size < 20 || header.len() < 20 {
        return false;
    }

    let mut metadata_size = u64::from(read_u32_be(header, 0).unwrap_or(u32::MAX));
    let mut declared_file_size = u64::from(read_u32_be(header, 4).unwrap_or(0));
    let version = read_u32_be(header, 8).unwrap_or(0);
    let mut data_offset = u64::from(read_u32_be(header, 12).unwrap_or(u32::MAX));
    let mut header_size = 16_u64;
    if version >= 9 {
        if header[16] > 1 {
            return false;
        }
        header_size = 20;
    }

    if version >= 22 {
        if file_size < 48 || header.len() < 40 {
            return false;
        }
        metadata_size = u64::from(read_u32_be(header, 20).unwrap_or(u32::MAX));
        let Some(extended_file_size) = read_i64_be(header, 24) else {
            return false;
        };
        let Some(extended_data_offset) = read_i64_be(header, 32) else {
            return false;
        };
        let (Ok(size), Ok(offset)) = (
            u64::try_from(extended_file_size),
            u64::try_from(extended_data_offset),
        ) else {
            return false;
        };
        declared_file_size = size;
        data_offset = offset;
        header_size = 48;
    }

    if declared_file_size != file_size
        || metadata_size > file_size
        || data_offset < header_size
        || data_offset > file_size
    {
        return false;
    }
    if version >= 9 {
        let Some(metadata_end) = header_size.checked_add(metadata_size) else {
            return false;
        };
        if metadata_end > data_offset {
            return false;
        }
    } else {
        let metadata_start = file_size - metadata_size;
        if metadata_start < header_size || data_offset > metadata_start {
            return false;
        }
    }
    true
}

fn bundle_data_offset(header: &[u8], file_size: u64) -> Option<usize> {
    header
        .windows(UNITY_FS_MAGIC.len())
        .enumerate()
        .filter(|(offset, magic)| *offset != 0 && *magic == UNITY_FS_MAGIC)
        .map(|(offset, _)| offset)
        .find(|offset| valid_unity_fs_candidate(header, *offset, file_size))
}

fn valid_unity_fs_candidate(header: &[u8], offset: usize, file_size: u64) -> bool {
    let Some(version_offset) = offset.checked_add(UNITY_FS_MAGIC.len()) else {
        return false;
    };
    let Some(version) = read_u32_be(header, version_offset) else {
        return false;
    };
    if !matches!(version, 6 | 7) {
        return false;
    }

    let Some(mut position) = version_offset.checked_add(4) else {
        return false;
    };
    let Some(after_generator) = skip_c_string(header, position) else {
        return false;
    };
    position = after_generator;
    let Some(after_revision) = skip_c_string(header, position) else {
        return false;
    };
    position = after_revision;
    let Some(bundle_size) = read_i64_be(header, position) else {
        return false;
    };
    let Ok(bundle_size) = u64::try_from(bundle_size) else {
        return false;
    };
    let Ok(offset_u64) = u64::try_from(offset) else {
        return false;
    };
    let Some(bundle_end) = offset_u64.checked_add(bundle_size) else {
        return false;
    };
    let Some(minimum_header_end) = position.checked_add(20) else {
        return false;
    };
    let Ok(minimum_header_size) = u64::try_from(minimum_header_end - offset) else {
        return false;
    };

    bundle_size >= minimum_header_size && bundle_end <= file_size
}

fn skip_c_string(bytes: &[u8], start: usize) -> Option<usize> {
    let tail = bytes.get(start..)?;
    let length = tail.iter().position(|byte| *byte == 0)?;
    start.checked_add(length)?.checked_add(1)
}

fn read_c_string(bytes: &[u8], max_length: usize) -> String {
    let end = bytes
        .iter()
        .take(max_length)
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len().min(max_length));
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_i64_be(bytes: &[u8], offset: usize) -> Option<i64> {
    Some(i64::from_be_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{FileType, detect_file_type, is_serialized_file};

    #[test]
    fn detects_magic_wrappers() {
        assert_eq!(
            detect_file_type(&[0x1f, 0x8b], 2).file_type,
            FileType::GzipFile
        );
        assert_eq!(
            detect_file_type(&[0x50, 0x4b, 0x03, 0x04], 4).file_type,
            FileType::ZipFile
        );

        let mut brotli = [0_u8; 40];
        brotli[32..38].copy_from_slice(b"brotli");
        assert_eq!(
            detect_file_type(&brotli, 40).file_type,
            FileType::BrotliFile
        );
    }

    #[test]
    fn detects_bundle_and_web_signatures() {
        assert_eq!(
            detect_file_type(b"UnityFS\0rest", 12).file_type,
            FileType::BundleFile
        );
        assert_eq!(
            detect_file_type(b"UnityWebData1.0\0", 16).file_type,
            FileType::WebFile
        );
        assert_eq!(
            detect_file_type(b"TuanjieWebData1.0\0", 18).file_type,
            FileType::WebFile
        );
    }

    #[test]
    fn finds_an_embedded_unity_fs_bundle() {
        let mut header = vec![0_u8; 256];
        write_unity_fs_candidate(&mut header, 128, 128);
        let detection = detect_file_type(&header, 256);

        assert_eq!(detection.file_type, FileType::BundleFile);
        assert_eq!(detection.data_offset, 128);
    }

    #[test]
    fn direct_bundle_wins_over_later_magic() {
        let mut header = vec![0_u8; 512];
        write_unity_fs_candidate(&mut header, 0, 512);
        write_unity_fs_candidate(&mut header, 128, 128);

        let detection = detect_file_type(&header, 512);
        assert_eq!(detection.file_type, FileType::BundleFile);
        assert_eq!(detection.data_offset, 0);
    }

    #[test]
    fn detects_legacy_serialized_file_header() {
        let mut header = [0_u8; 64];
        header[0..4].copy_from_slice(&12_u32.to_be_bytes());
        header[4..8].copy_from_slice(&64_u32.to_be_bytes());
        header[8..12].copy_from_slice(&17_u32.to_be_bytes());
        header[12..16].copy_from_slice(&32_u32.to_be_bytes());

        assert!(is_serialized_file(&header, 64));
        assert_eq!(
            detect_file_type(&header, 64).file_type,
            FileType::AssetsFile
        );
    }

    #[test]
    fn detects_large_serialized_file_header() {
        let mut header = [0_u8; 64];
        header[0..4].copy_from_slice(&20_u32.to_be_bytes());
        header[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        header[8..12].copy_from_slice(&22_u32.to_be_bytes());
        header[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        header[20..24].copy_from_slice(&0_u32.to_be_bytes());
        header[24..32].copy_from_slice(&64_i64.to_be_bytes());
        header[32..40].copy_from_slice(&48_i64.to_be_bytes());

        assert!(is_serialized_file(&header, 64));
    }

    #[test]
    fn rejects_mismatched_serialized_file_size() {
        let mut header = [0_u8; 32];
        header[4..8].copy_from_slice(&31_u32.to_be_bytes());
        header[8..12].copy_from_slice(&17_u32.to_be_bytes());
        header[12..16].copy_from_slice(&20_u32.to_be_bytes());

        assert!(!is_serialized_file(&header, 32));
        assert_eq!(
            detect_file_type(&header, 32).file_type,
            FileType::ResourceFile
        );
    }

    fn write_unity_fs_candidate(bytes: &mut [u8], offset: usize, size: i64) {
        let mut position = offset;
        bytes[position..position + 8].copy_from_slice(b"UnityFS\0");
        position += 8;
        bytes[position..position + 4].copy_from_slice(&6_u32.to_be_bytes());
        position += 4;
        bytes[position..position + 2].copy_from_slice(b"x\0");
        position += 2;
        bytes[position] = 0;
        position += 1;
        bytes[position..position + 8].copy_from_slice(&size.to_be_bytes());
    }
}
