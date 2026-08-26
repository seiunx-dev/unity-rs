use std::io::{self, Read, Seek, SeekFrom};

use crate::{Error, Result};

/// Byte order used by a Unity data stream.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    #[default]
    Big,
}

/// A checked, endian-aware reader over a seekable stream.
pub struct EndianReader<R> {
    inner: R,
    endian: Endian,
}

impl<R> EndianReader<R> {
    #[must_use]
    pub const fn new(inner: R, endian: Endian) -> Self {
        Self { inner, endian }
    }

    #[must_use]
    pub const fn endian(&self) -> Endian {
        self.endian
    }

    pub const fn set_endian(&mut self, endian: Endian) {
        self.endian = endian;
    }

    #[must_use]
    pub const fn get_ref(&self) -> &R {
        &self.inner
    }

    pub const fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read + Seek> EndianReader<R> {
    pub fn position(&mut self) -> Result<u64> {
        Ok(self.inner.stream_position()?)
    }

    pub fn set_position(&mut self, position: u64) -> Result<()> {
        self.inner.seek(SeekFrom::Start(position))?;
        Ok(())
    }

    pub fn len(&mut self) -> Result<u64> {
        let position = self.inner.stream_position()?;
        let length = self.inner.seek(SeekFrom::End(0))?;
        self.inner.seek(SeekFrom::Start(position))?;
        Ok(length)
    }

    pub fn is_empty(&mut self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    pub fn remaining(&mut self) -> Result<u64> {
        let position = self.position()?;
        let length = self.len()?;
        length
            .checked_sub(position)
            .ok_or_else(|| Error::invalid_data("reader position is past the end of the stream"))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0_u8; N];
        self.inner.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_array::<1>()?[0])
    }

    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(i8::from_ne_bytes(self.read_array::<1>()?))
    }

    pub fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read_u8()? != 0)
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        let bytes = self.read_array::<2>()?;
        Ok(match self.endian {
            Endian::Little => u16::from_le_bytes(bytes),
            Endian::Big => u16::from_be_bytes(bytes),
        })
    }

    pub fn read_i16(&mut self) -> Result<i16> {
        let bytes = self.read_array::<2>()?;
        Ok(match self.endian {
            Endian::Little => i16::from_le_bytes(bytes),
            Endian::Big => i16::from_be_bytes(bytes),
        })
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.read_array::<4>()?;
        Ok(match self.endian {
            Endian::Little => u32::from_le_bytes(bytes),
            Endian::Big => u32::from_be_bytes(bytes),
        })
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        let bytes = self.read_array::<4>()?;
        Ok(match self.endian {
            Endian::Little => i32::from_le_bytes(bytes),
            Endian::Big => i32::from_be_bytes(bytes),
        })
    }

    pub fn read_u64(&mut self) -> Result<u64> {
        let bytes = self.read_array::<8>()?;
        Ok(match self.endian {
            Endian::Little => u64::from_le_bytes(bytes),
            Endian::Big => u64::from_be_bytes(bytes),
        })
    }

    pub fn read_i64(&mut self) -> Result<i64> {
        let bytes = self.read_array::<8>()?;
        Ok(match self.endian {
            Endian::Little => i64::from_le_bytes(bytes),
            Endian::Big => i64::from_be_bytes(bytes),
        })
    }

    pub fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    pub fn read_f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    pub fn read_bytes(&mut self, length: usize) -> Result<Vec<u8>> {
        let length_u64 = u64::try_from(length)
            .map_err(|_| Error::invalid_data("requested byte length does not fit in u64"))?;
        let remaining = self.remaining()?;
        if length_u64 > remaining {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("requested {length} bytes but only {remaining} remain"),
            )
            .into());
        }

        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|error| {
            Error::invalid_data(format!("cannot allocate {length} input bytes: {error}"))
        })?;
        bytes.resize(length, 0);
        self.inner.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// Reads a UTF-8, nul-terminated string, mirroring .NET's replacement
    /// behavior for malformed UTF-8.
    pub fn read_c_string(&mut self, max_length: usize) -> Result<String> {
        let (bytes, _) = self.read_c_string_bytes(max_length)?;
        utf8_with_replacement(bytes, "C string")
    }

    /// Reads a nul-terminated UTF-8 string and rejects EOF or a length-limit
    /// hit before the terminator.
    pub fn read_c_string_required(&mut self, max_length: usize, field: &str) -> Result<String> {
        let (bytes, terminated) = self.read_c_string_bytes(max_length)?;
        if terminated {
            return utf8_with_replacement(bytes, field);
        }
        Err(Error::invalid_data(format!(
            "{field} is not nul-terminated within {max_length} bytes"
        )))
    }

    fn read_c_string_bytes(&mut self, max_length: usize) -> Result<(Vec<u8>, bool)> {
        let available = usize::try_from(
            self.remaining()?
                .min(u64::try_from(max_length).unwrap_or(u64::MAX)),
        )
        .map_err(|_| Error::invalid_data("available string length does not fit this platform"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(available.min(256))
            .map_err(|error| Error::invalid_data(format!("cannot allocate C string: {error}")))?;
        while bytes.len() < available {
            let byte = self.read_u8()?;
            if byte == 0 {
                return Ok((bytes, true));
            }
            if bytes.len() == bytes.capacity() {
                let additional = bytes
                    .capacity()
                    .max(256)
                    .min(available.saturating_sub(bytes.len()));
                bytes.try_reserve_exact(additional).map_err(|error| {
                    Error::invalid_data(format!("cannot grow C string: {error}"))
                })?;
            }
            bytes.push(byte);
        }
        Ok((bytes, false))
    }

    pub fn read_utf8(&mut self, length: usize) -> Result<String> {
        utf8_with_replacement(self.read_bytes(length)?, "UTF-8 string")
    }

    pub fn read_aligned_string(&mut self) -> Result<String> {
        let length = checked_length(self.read_i32()?, "string")?;
        let value = self.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    pub fn align(&mut self, alignment: u64) -> Result<()> {
        if alignment == 0 {
            return Err(Error::invalid_data("alignment must be non-zero"));
        }

        let position = self.position()?;
        let remainder = position % alignment;
        if remainder == 0 {
            return Ok(());
        }

        let target = position
            .checked_add(alignment - remainder)
            .ok_or_else(|| Error::invalid_data("aligned stream position overflowed"))?;
        if target > self.len()? {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "alignment would seek past the end of the stream",
            )
            .into());
        }
        self.set_position(target)
    }
}

fn utf8_with_replacement(bytes: Vec<u8>, field: &str) -> Result<String> {
    let error = match String::from_utf8(bytes) {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    let bytes = error.into_bytes();
    let mut remaining = bytes.as_slice();
    let mut output_length = 0_usize;
    loop {
        match std::str::from_utf8(remaining) {
            Ok(value) => {
                output_length = output_length
                    .checked_add(value.len())
                    .ok_or_else(|| Error::invalid_data(format!("{field} length overflowed")))?;
                break;
            }
            Err(error) => {
                output_length = output_length
                    .checked_add(error.valid_up_to())
                    .and_then(|length| length.checked_add('\u{fffd}'.len_utf8()))
                    .ok_or_else(|| Error::invalid_data(format!("{field} length overflowed")))?;
                let Some(invalid_length) = error.error_len() else {
                    break;
                };
                remaining = &remaining[error.valid_up_to() + invalid_length..];
            }
        }
    }

    let mut output = String::new();
    output.try_reserve_exact(output_length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {field} replacement text: {error}"))
    })?;
    remaining = bytes.as_slice();
    loop {
        match std::str::from_utf8(remaining) {
            Ok(value) => {
                output.push_str(value);
                break;
            }
            Err(error) => {
                let valid =
                    std::str::from_utf8(&remaining[..error.valid_up_to()]).map_err(|_| {
                        Error::invalid_data(format!("{field} UTF-8 prefix validation disagrees"))
                    })?;
                output.push_str(valid);
                output.push('\u{fffd}');
                let Some(invalid_length) = error.error_len() else {
                    break;
                };
                remaining = &remaining[error.valid_up_to() + invalid_length..];
            }
        }
    }
    if output.len() != output_length {
        return Err(Error::invalid_data(format!(
            "{field} replacement length changed from {output_length} to {} bytes",
            output.len()
        )));
    }
    Ok(output)
}

pub(crate) fn checked_length(value: i32, field: &str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| Error::invalid_data(format!("{field} length cannot be negative: {value}")))
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::io::{Cursor, ErrorKind};

    use super::{Endian, EndianReader};

    #[test]
    fn reads_big_endian_numbers() {
        let bytes = [
            0x01, 0x02, 0x03, 0x04, // u32
            0x40, 0x49, 0x0f, 0xdb, // pi as f32
            0x01, 0x02, // u16
        ];
        let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Big);

        assert_eq!(reader.read_u32().unwrap(), 0x0102_0304);
        assert!((reader.read_f32().unwrap() - std::f32::consts::PI).abs() < 1.0e-6);
        assert_eq!(reader.read_u16().unwrap(), 0x0102);
    }

    #[test]
    fn reads_little_endian_numbers() {
        let bytes = [0x04, 0x03, 0x02, 0x01, 0x02, 0x01];
        let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Little);

        assert_eq!(reader.read_u32().unwrap(), 0x0102_0304);
        assert_eq!(reader.read_i16().unwrap(), 0x0102);
    }

    #[test]
    fn reads_strings_and_alignment() {
        let bytes = [
            b'a', b'b', 0, 0xff, // c string and padding
            3, 0, 0, 0, b'f', b'o', b'o', 0, // aligned string
        ];
        let mut reader = EndianReader::new(Cursor::new(bytes), Endian::Little);

        assert_eq!(reader.read_c_string(32).unwrap(), "ab");
        reader.align(4).unwrap();
        assert_eq!(reader.read_aligned_string().unwrap(), "foo");
        assert_eq!(reader.position().unwrap(), 12);
    }

    #[test]
    fn strings_grow_fallibly_and_match_standard_utf8_replacement() {
        let mut terminated = vec![b'x'; 4096];
        terminated.push(0);
        let mut reader = EndianReader::new(Cursor::new(terminated), Endian::Little);
        assert_eq!(
            reader
                .read_c_string_required(4097, "long string")
                .unwrap()
                .len(),
            4096
        );

        let invalid = [b'a', 0xff, 0xc0, 0xaf, 0xe2, 0x82];
        let expected = String::from_utf8_lossy(&invalid);
        let mut reader = EndianReader::new(Cursor::new(invalid), Endian::Little);
        assert_eq!(reader.read_utf8(invalid.len()).unwrap(), expected);

        let mut terminated_invalid = invalid.to_vec();
        terminated_invalid.push(0);
        let mut reader = EndianReader::new(Cursor::new(terminated_invalid), Endian::Little);
        assert_eq!(
            reader.read_c_string_required(32, "invalid string").unwrap(),
            expected
        );

        let all_invalid = vec![0xff; 1024];
        let mut reader = EndianReader::new(Cursor::new(&all_invalid), Endian::Little);
        let replacement = reader.read_utf8(all_invalid.len()).unwrap();
        assert_eq!(replacement.chars().count(), all_invalid.len());
        assert_eq!(replacement.len(), all_invalid.len() * '\u{fffd}'.len_utf8());
    }

    #[test]
    fn required_c_strings_reject_missing_terminators_at_the_exact_limit() {
        let mut reader = EndianReader::new(Cursor::new(b"abcd\0"), Endian::Little);
        let error = reader.read_c_string_required(4, "fixture").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not nul-terminated within 4 bytes")
        );

        let mut reader = EndianReader::new(Cursor::new(b"abcd"), Endian::Little);
        assert_eq!(reader.read_c_string(4).unwrap(), "abcd");
    }

    #[test]
    fn rejects_reads_past_end() {
        let mut reader = EndianReader::new(Cursor::new([1_u8, 2]), Endian::Big);
        let error = reader.read_bytes(3).unwrap_err();
        assert_eq!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .map(std::io::Error::kind),
            Some(ErrorKind::UnexpectedEof)
        );
    }
}
