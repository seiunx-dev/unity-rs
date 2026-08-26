use std::fmt;
use std::io::{Read, Seek};

use crate::endian::{Endian, EndianReader};
use crate::{Error, Result};

/// Numeric Unity serialized-file format version.
///
/// A newtype is used instead of a closed enum so files from future Unity
/// versions can still be inspected and reported without undefined conversions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SerializedFileFormatVersion(pub u32);

impl SerializedFileFormatVersion {
    pub const UNSUPPORTED: Self = Self(1);
    pub const UNKNOWN_5: Self = Self(5);
    pub const UNKNOWN_6: Self = Self(6);
    pub const UNKNOWN_7: Self = Self(7);
    pub const UNKNOWN_8: Self = Self(8);
    pub const UNKNOWN_9: Self = Self(9);
    pub const UNKNOWN_10: Self = Self(10);
    pub const HAS_SCRIPT_TYPE_INDEX: Self = Self(11);
    pub const UNKNOWN_12: Self = Self(12);
    pub const HAS_TYPE_TREE_HASHES: Self = Self(13);
    pub const UNKNOWN_14: Self = Self(14);
    pub const SUPPORTS_STRIPPED_OBJECT: Self = Self(15);
    pub const REFACTORED_CLASS_ID: Self = Self(16);
    pub const REFACTOR_TYPE_DATA: Self = Self(17);
    pub const REFACTOR_SHAREABLE_TYPE_TREE_DATA: Self = Self(18);
    pub const TYPE_TREE_NODE_WITH_TYPE_FLAGS: Self = Self(19);
    pub const SUPPORTS_REF_OBJECT: Self = Self(20);
    pub const STORES_TYPE_DEPENDENCIES: Self = Self(21);
    pub const LARGE_FILES_SUPPORT: Self = Self(22);
}

impl fmt::Display for SerializedFileFormatVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerializedFileHeader {
    pub metadata_size: u32,
    pub file_size: u64,
    pub version: SerializedFileFormatVersion,
    pub data_offset: u64,
    pub endianness: u8,
    pub reserved: [u8; 3],
}

impl SerializedFileHeader {
    /// Reads the serialized-file header and leaves `reader` at the start of
    /// metadata with its byte order switched to the file's metadata byte order.
    pub fn read<R: Read + Seek>(reader: &mut EndianReader<R>) -> Result<Self> {
        reader.set_endian(Endian::Big);
        let actual_file_size = reader.len()?;

        let mut metadata_size = reader.read_u32()?;
        let mut file_size = u64::from(reader.read_u32()?);
        let version = SerializedFileFormatVersion(reader.read_u32()?);
        let mut data_offset = u64::from(reader.read_u32()?);
        let (endianness, reserved) = if version >= SerializedFileFormatVersion::UNKNOWN_9 {
            let endianness = reader.read_u8()?;
            let bytes = reader.read_bytes(3)?;
            let reserved = bytes
                .as_slice()
                .try_into()
                .map_err(|_| Error::invalid_data("failed to read serialized reserved bytes"))?;
            (endianness, reserved)
        } else {
            let metadata_position = file_size
                .checked_sub(u64::from(metadata_size))
                .ok_or_else(|| Error::invalid_data("metadata size exceeds serialized file size"))?;
            if metadata_position >= actual_file_size {
                return Err(Error::invalid_data(
                    "legacy serialized metadata starts outside the input stream",
                ));
            }
            reader.set_position(metadata_position)?;
            (reader.read_u8()?, [0; 3])
        };

        if version >= SerializedFileFormatVersion::LARGE_FILES_SUPPORT {
            metadata_size = reader.read_u32()?;
            file_size = positive_i64(reader.read_i64()?, "serialized file size")?;
            data_offset = positive_i64(reader.read_i64()?, "serialized data offset")?;
            let _unknown = reader.read_i64()?;
        }

        if file_size != actual_file_size {
            return Err(Error::invalid_data(format!(
                "serialized file declares {file_size} bytes but the stream has {actual_file_size} bytes"
            )));
        }

        let header = Self {
            metadata_size,
            file_size,
            version,
            data_offset,
            endianness,
            reserved,
        };
        header.validate_layout(actual_file_size)?;

        reader.set_endian(if endianness == 0 {
            Endian::Little
        } else {
            Endian::Big
        });

        Ok(header)
    }

    pub fn validate_layout(&self, actual_file_size: u64) -> Result<()> {
        if self.file_size != actual_file_size {
            return Err(Error::invalid_data(format!(
                "serialized file declares {} bytes but the stream has {actual_file_size} bytes",
                self.file_size
            )));
        }
        if self.endianness > 1 {
            return Err(Error::invalid_data(format!(
                "serialized metadata endianness must be 0 or 1, got {}",
                self.endianness
            )));
        }
        if u64::from(self.metadata_size) > self.file_size {
            return Err(Error::invalid_data(format!(
                "serialized metadata size {} exceeds file size {}",
                self.metadata_size, self.file_size
            )));
        }

        let header_size = self.header_size();
        if self.data_offset < header_size || self.data_offset > self.file_size {
            return Err(Error::invalid_data(format!(
                "serialized data offset {} is outside the valid {header_size}..={} range",
                self.data_offset, self.file_size
            )));
        }
        if self.version >= SerializedFileFormatVersion::UNKNOWN_9 {
            let metadata_end = header_size
                .checked_add(u64::from(self.metadata_size))
                .ok_or_else(|| Error::invalid_data("serialized metadata range overflowed"))?;
            if metadata_end > self.data_offset {
                return Err(Error::invalid_data(format!(
                    "serialized metadata ends at {metadata_end}, after data offset {}",
                    self.data_offset
                )));
            }
        } else {
            let metadata_start = self.file_size - u64::from(self.metadata_size);
            if metadata_start < header_size {
                return Err(Error::invalid_data(
                    "legacy serialized metadata overlaps the file header",
                ));
            }
            if self.data_offset > metadata_start {
                return Err(Error::invalid_data(format!(
                    "legacy serialized data offset {} is after metadata start {metadata_start}",
                    self.data_offset
                )));
            }
        }
        Ok(())
    }

    #[must_use]
    pub const fn header_size(&self) -> u64 {
        if self.version.0 >= SerializedFileFormatVersion::LARGE_FILES_SUPPORT.0 {
            48
        } else if self.version.0 >= SerializedFileFormatVersion::UNKNOWN_9.0 {
            20
        } else {
            16
        }
    }

    pub fn metadata_range(&self) -> Result<std::ops::Range<u64>> {
        if self.version >= SerializedFileFormatVersion::UNKNOWN_9 {
            let start = self.header_size();
            let end = start
                .checked_add(u64::from(self.metadata_size))
                .ok_or_else(|| Error::invalid_data("serialized metadata range overflowed"))?;
            Ok(start..end)
        } else {
            let start = self
                .file_size
                .checked_sub(u64::from(self.metadata_size))
                .ok_or_else(|| Error::invalid_data("serialized metadata range underflowed"))?;
            Ok(start..self.file_size)
        }
    }

    pub fn object_data_end(&self) -> Result<u64> {
        if self.version >= SerializedFileFormatVersion::UNKNOWN_9 {
            Ok(self.file_size)
        } else {
            Ok(self.metadata_range()?.start)
        }
    }
}

fn positive_i64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::invalid_data(format!("{field} cannot be negative")))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::endian::{Endian, EndianReader};

    use super::{SerializedFileFormatVersion, SerializedFileHeader};

    #[test]
    fn reads_large_file_header_and_switches_endian() {
        let mut bytes = vec![0_u8; 64];
        bytes[0..4].copy_from_slice(&20_u32.to_be_bytes());
        bytes[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        bytes[16] = 0;
        bytes[20..24].copy_from_slice(&0_u32.to_be_bytes());
        bytes[24..32].copy_from_slice(&64_i64.to_be_bytes());
        bytes[32..40].copy_from_slice(&48_i64.to_be_bytes());

        let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Big);
        let header = SerializedFileHeader::read(&mut reader).unwrap();

        assert_eq!(header.metadata_size, 0);
        assert_eq!(header.file_size, 64);
        assert_eq!(header.data_offset, 48);
        assert_eq!(
            header.version,
            SerializedFileFormatVersion::LARGE_FILES_SUPPORT
        );
        assert_eq!(reader.endian(), Endian::Little);
        assert_eq!(reader.position().unwrap(), 48);
    }

    #[test]
    fn reads_modern_big_endian_header() {
        let mut bytes = vec![0_u8; 64];
        bytes[0..4].copy_from_slice(&12_u32.to_be_bytes());
        bytes[4..8].copy_from_slice(&64_u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&17_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&32_u32.to_be_bytes());
        bytes[16] = 1;

        let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Little);
        let header = SerializedFileHeader::read(&mut reader).unwrap();

        assert_eq!(
            header.version,
            SerializedFileFormatVersion::REFACTOR_TYPE_DATA
        );
        assert_eq!(reader.endian(), Endian::Big);
        assert_eq!(reader.position().unwrap(), 20);
    }

    #[test]
    fn reads_legacy_metadata_endianness_byte() {
        let mut bytes = vec![0_u8; 64];
        bytes[0..4].copy_from_slice(&16_u32.to_be_bytes());
        bytes[4..8].copy_from_slice(&64_u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&8_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&32_u32.to_be_bytes());
        bytes[48] = 0;

        let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Big);
        let header = SerializedFileHeader::read(&mut reader).unwrap();

        assert_eq!(header.version, SerializedFileFormatVersion::UNKNOWN_8);
        assert_eq!(reader.endian(), Endian::Little);
        assert_eq!(reader.position().unwrap(), 49);
    }

    #[test]
    fn rejects_declared_size_mismatch() {
        let mut bytes = vec![0_u8; 64];
        bytes[0..4].copy_from_slice(&20_u32.to_be_bytes());
        bytes[4..8].copy_from_slice(&63_u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&17_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&32_u32.to_be_bytes());

        let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Big);
        assert!(SerializedFileHeader::read(&mut reader).is_err());
    }

    #[test]
    fn rejects_invalid_endianness_and_metadata_ranges() {
        let mut invalid_endian = vec![0_u8; 64];
        invalid_endian[0..4].copy_from_slice(&20_u32.to_be_bytes());
        invalid_endian[4..8].copy_from_slice(&64_u32.to_be_bytes());
        invalid_endian[8..12].copy_from_slice(&17_u32.to_be_bytes());
        invalid_endian[12..16].copy_from_slice(&32_u32.to_be_bytes());
        invalid_endian[16] = 2;
        let mut reader = EndianReader::new(Cursor::new(invalid_endian), Endian::Big);
        assert!(SerializedFileHeader::read(&mut reader).is_err());

        let mut oversized_metadata = vec![0_u8; 64];
        oversized_metadata[0..4].copy_from_slice(&65_u32.to_be_bytes());
        oversized_metadata[4..8].copy_from_slice(&64_u32.to_be_bytes());
        oversized_metadata[8..12].copy_from_slice(&17_u32.to_be_bytes());
        oversized_metadata[12..16].copy_from_slice(&32_u32.to_be_bytes());
        let mut reader = EndianReader::new(Cursor::new(oversized_metadata), Endian::Big);
        assert!(SerializedFileHeader::read(&mut reader).is_err());

        let mut data_in_header = vec![0_u8; 64];
        data_in_header[0..4].copy_from_slice(&16_u32.to_be_bytes());
        data_in_header[4..8].copy_from_slice(&64_u32.to_be_bytes());
        data_in_header[8..12].copy_from_slice(&17_u32.to_be_bytes());
        data_in_header[12..16].copy_from_slice(&8_u32.to_be_bytes());
        let mut reader = EndianReader::new(Cursor::new(data_in_header), Endian::Big);
        assert!(SerializedFileHeader::read(&mut reader).is_err());
    }
}
