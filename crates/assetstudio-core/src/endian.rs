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
        let mut bytes = Vec::with_capacity(max_length.min(256));
        let available = usize::try_from(
            self.remaining()?
                .min(u64::try_from(max_length).unwrap_or(u64::MAX)),
        )
        .expect("available string bytes are bounded by usize");
        while bytes.len() < available {
            let byte = self.read_u8()?;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Reads a nul-terminated UTF-8 string and rejects EOF or a length-limit
    /// hit before the terminator.
    pub fn read_c_string_required(&mut self, max_length: usize, field: &str) -> Result<String> {
        let mut bytes = Vec::with_capacity(max_length.min(256));
        let available = usize::try_from(
            self.remaining()?
                .min(u64::try_from(max_length).unwrap_or(u64::MAX)),
        )
        .expect("available string bytes are bounded by usize");
        while bytes.len() < available {
            let byte = self.read_u8()?;
            if byte == 0 {
                return Ok(String::from_utf8_lossy(&bytes).into_owned());
            }
            bytes.push(byte);
        }
        Err(Error::invalid_data(format!(
            "{field} is not nul-terminated within {max_length} bytes"
        )))
    }

    pub fn read_utf8(&mut self, length: usize) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.read_bytes(length)?).into_owned())
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
