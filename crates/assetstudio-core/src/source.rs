use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::{Error, Result};

/// Random-access byte source used by parsers and lazy asset payloads.
pub trait Source: Send + Sync {
    fn len(&self) -> u64;

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A source-bound, strictly bounded byte range.
///
/// Cloning a region is cheap. Cursors created from separate clones have
/// independent logical positions and may be consumed concurrently.
#[derive(Clone)]
pub struct Region {
    source: Arc<dyn Source>,
    source_offset: u64,
    length: u64,
}

impl Region {
    pub fn new(source: Arc<dyn Source>, source_offset: u64, length: u64) -> Result<Self> {
        let end = source_offset
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("source region range overflowed"))?;
        if end > source.len() {
            return Err(Error::invalid_data(format!(
                "source region {source_offset}..{end} exceeds source length {}",
                source.len()
            )));
        }
        Ok(Self {
            source,
            source_offset,
            length,
        })
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        Self::new(Arc::new(FileSource::new(file, length)), 0, length)
    }

    /// Concatenates bounded regions into one lazy random-access source.
    ///
    /// Segment bytes remain in their original sources and are read only when a
    /// consumer crosses the corresponding logical range.
    pub fn concatenate(segments: Vec<Self>, maximum_length: u64) -> Result<Self> {
        if segments.is_empty() {
            return Err(Error::invalid_data(
                "segmented source requires at least one region",
            ));
        }
        let source = SegmentedSource::new(segments, maximum_length)?;
        let length = source.length;
        Self::new(Arc::new(source), 0, length)
    }

    #[must_use]
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Self {
        let source = Arc::new(MemorySource {
            bytes: bytes.into(),
        });
        Self {
            length: source.len(),
            source,
            source_offset: 0,
        }
    }

    #[must_use]
    pub const fn len(&self) -> u64 {
        self.length
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    #[must_use]
    pub const fn source_offset(&self) -> u64 {
        self.source_offset
    }

    pub fn subregion(&self, offset: u64, length: u64) -> Result<Self> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("subregion range overflowed"))?;
        if end > self.length {
            return Err(Error::invalid_data(format!(
                "subregion {offset}..{end} exceeds region length {}",
                self.length
            )));
        }
        let source_offset = self
            .source_offset
            .checked_add(offset)
            .ok_or_else(|| Error::invalid_data("subregion source offset overflowed"))?;
        Self::new(Arc::clone(&self.source), source_offset, length)
    }

    #[must_use]
    pub fn cursor(&self) -> RegionCursor {
        RegionCursor {
            region: self.clone(),
            position: 0,
        }
    }

    pub fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> Result<()> {
        self.read_exact_at_io(offset, output)?;
        Ok(())
    }

    pub fn copy_range<W: Write>(&self, offset: u64, length: u64, output: &mut W) -> Result<u64> {
        self.check_range(offset, length)?;
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 16 * 1024];
        while copied < length {
            let remaining = length - copied;
            let chunk_length = usize::try_from(
                remaining.min(u64::try_from(buffer.len()).expect("buffer length fits in u64")),
            )
            .expect("chunk length is bounded by the fixed buffer");
            self.read_exact_at(offset + copied, &mut buffer[..chunk_length])?;
            output.write_all(&buffer[..chunk_length])?;
            copied += u64::try_from(chunk_length).expect("chunk length fits in u64");
        }
        Ok(copied)
    }

    pub fn read_to_vec(&self, maximum_length: u64) -> Result<Vec<u8>> {
        if self.length > maximum_length {
            return Err(Error::invalid_data(format!(
                "region is {} bytes, exceeding materialization limit {maximum_length}",
                self.length
            )));
        }
        let length = usize::try_from(self.length)
            .map_err(|_| Error::invalid_data("region is too large for this platform"))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|error| {
            Error::invalid_data(format!("cannot allocate {length} region bytes: {error}"))
        })?;
        bytes.resize(length, 0);
        self.read_exact_at(0, &mut bytes)?;
        Ok(bytes)
    }

    fn check_range(&self, offset: u64, length: u64) -> Result<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("region read range overflowed"))?;
        if end > self.length {
            return Err(Error::invalid_data(format!(
                "region read {offset}..{end} exceeds length {}",
                self.length
            )));
        }
        Ok(())
    }

    fn read_exact_at_io(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        let length = u64::try_from(output.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "buffer length overflowed"))?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read range overflowed"))?;
        if end > self.length {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("region read {offset}..{end} exceeds length {}", self.length),
            ));
        }
        let source_offset = self.source_offset.checked_add(offset).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "source offset overflowed")
        })?;
        self.source.read_exact_at(source_offset, output)
    }
}

impl fmt::Debug for Region {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Region")
            .field("source_offset", &self.source_offset)
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Region {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.source, &other.source)
            && self.source_offset == other.source_offset
            && self.length == other.length
    }
}

impl Eq for Region {}

pub struct RegionCursor {
    region: Region,
    position: u64,
}

impl RegionCursor {
    #[must_use]
    pub const fn region(&self) -> &Region {
        &self.region
    }

    #[must_use]
    pub const fn position(&self) -> u64 {
        self.position
    }
}

impl Read for RegionCursor {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let remaining = self.region.length - self.position;
        let count = usize::try_from(remaining.min(u64::try_from(output.len()).unwrap_or(u64::MAX)))
            .expect("read count is bounded by the output slice");
        if count == 0 {
            return Ok(0);
        }
        self.region
            .read_exact_at_io(self.position, &mut output[..count])?;
        self.position += u64::try_from(count).expect("read count fits in u64");
        Ok(count)
    }
}

impl Seek for RegionCursor {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
            SeekFrom::End(delta) => i128::from(self.region.length) + i128::from(delta),
        };
        if target < 0 || target > i128::from(self.region.length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek target is outside the bounded region",
            ));
        }
        self.position = u64::try_from(target).expect("validated seek target fits in u64");
        Ok(self.position)
    }
}

struct FileSource {
    file: Mutex<File>,
    length: u64,
}

impl FileSource {
    const fn new(file: File, length: u64) -> Self {
        Self {
            file: Mutex::new(file),
            length,
        }
    }
}

impl Source for FileSource {
    fn len(&self) -> u64 {
        self.length
    }

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("source file lock was poisoned"))?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(output)
    }
}

struct MemorySource {
    bytes: Arc<[u8]>,
}

impl Source for MemorySource {
    fn len(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "memory source offset overflowed",
            )
        })?;
        let end = start.checked_add(output.len()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "memory read range overflowed")
        })?;
        let source = self.bytes.get(start..end).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "memory read exceeds source")
        })?;
        output.copy_from_slice(source);
        Ok(())
    }
}

struct SourceSegment {
    start: u64,
    region: Region,
}

struct SegmentedSource {
    segments: Vec<SourceSegment>,
    length: u64,
}

impl SegmentedSource {
    fn new(regions: Vec<Region>, maximum_length: u64) -> Result<Self> {
        let mut length = 0_u64;
        let mut segments = Vec::new();
        segments.try_reserve_exact(regions.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate segmented source table: {error}"))
        })?;
        for region in regions {
            let start = length;
            length = length
                .checked_add(region.len())
                .ok_or_else(|| Error::invalid_data("segmented source length overflowed"))?;
            if length > maximum_length {
                return Err(Error::invalid_data(format!(
                    "segmented source is {length} bytes, exceeding limit {maximum_length}"
                )));
            }
            segments.push(SourceSegment { start, region });
        }
        Ok(Self { segments, length })
    }
}

impl Source for SegmentedSource {
    fn len(&self) -> u64 {
        self.length
    }

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        let output_length = u64::try_from(output.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "buffer length overflowed"))?;
        let end = offset.checked_add(output_length).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "segmented read overflowed")
        })?;
        if end > self.length {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "segmented read {offset}..{end} exceeds source length {}",
                    self.length
                ),
            ));
        }

        let mut source_offset = offset;
        let mut output_offset = 0_usize;
        while output_offset < output.len() {
            let segment_index = self
                .segments
                .partition_point(|segment| segment.start <= source_offset)
                .checked_sub(1)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "segmented read does not map to a source region",
                    )
                })?;
            let segment = &self.segments[segment_index];
            let region_offset = source_offset.checked_sub(segment.start).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "segment offset underflowed")
            })?;
            let available = segment
                .region
                .len()
                .checked_sub(region_offset)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "segment offset exceeds its region",
                    )
                })?;
            if available == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "segmented read selected an empty region",
                ));
            }
            let output_remaining = output.len() - output_offset;
            let count =
                usize::try_from(available.min(u64::try_from(output_remaining).unwrap_or(u64::MAX)))
                    .expect("segment read length is bounded by the output slice");
            let output_end = output_offset.checked_add(count).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "output offset overflowed")
            })?;
            segment
                .region
                .read_exact_at_io(region_offset, &mut output[output_offset..output_end])?;
            source_offset = source_offset
                .checked_add(u64::try_from(count).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "read length overflowed")
                })?)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "source offset overflowed")
                })?;
            output_offset = output_end;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom};

    use super::Region;

    #[test]
    fn subregions_and_cursors_are_strictly_bounded() {
        let root = Region::from_bytes(Vec::from(&b"0123456789"[..]));
        let region = root.subregion(2, 5).unwrap();
        let mut cursor = region.cursor();
        let mut bytes = Vec::new();
        cursor.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"23456");
        assert!(cursor.seek(SeekFrom::Start(6)).is_err());
        assert!(root.subregion(8, 3).is_err());
    }

    #[test]
    fn cloned_cursors_have_independent_positions() {
        let region = Region::from_bytes(Vec::from(&b"abcdef"[..]));
        let mut first = region.cursor();
        let mut second = region.cursor();
        let mut byte = [0_u8; 1];

        first.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"a");
        second.seek(SeekFrom::Start(3)).unwrap();
        second.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"d");
        first.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"b");
    }

    #[test]
    fn copies_and_materializes_with_limits() {
        let region = Region::from_bytes(Vec::from(&b"payload"[..]));
        let mut output = Vec::new();
        assert_eq!(region.copy_range(1, 3, &mut output).unwrap(), 3);
        assert_eq!(output, b"ayl");
        assert_eq!(region.read_to_vec(7).unwrap(), b"payload");
        assert!(region.read_to_vec(6).is_err());
    }

    #[test]
    fn concatenated_regions_read_lazily_across_segment_boundaries() {
        let region = Region::concatenate(
            vec![
                Region::from_bytes(Vec::from(&b"abc"[..])),
                Region::from_bytes(Vec::<u8>::new()),
                Region::from_bytes(Vec::from(&b"defg"[..])),
            ],
            7,
        )
        .unwrap();

        assert_eq!(region.len(), 7);
        let mut bytes = [0_u8; 5];
        region.read_exact_at(1, &mut bytes).unwrap();
        assert_eq!(&bytes, b"bcdef");
        assert_eq!(
            region.subregion(2, 4).unwrap().read_to_vec(4).unwrap(),
            b"cdef"
        );
        assert!(Region::concatenate(vec![region], 6).is_err());
    }
}
