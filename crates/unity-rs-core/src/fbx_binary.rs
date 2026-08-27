//! Binary FBX 7.4 encoding.
//!
//! An FBX file is a tree of node records. Each record names itself, carries a
//! typed property list, and may nest further records. The ASCII form this crate
//! already writes spells that tree out as text; the binary form writes the same
//! tree with length-prefixed records and typed property tags.
//!
//! What this module owns is the encoding: the node tree, the property types and
//! the byte layout. Building a scene's tree is the caller's job, which keeps the
//! format rules in one place and testable on their own.
//!
//! Deliberately not supported:
//!
//! * Version 7.5 and newer, whose record offsets widen to 64 bits. Every offset
//!   here is a `u32` and a file that would overflow one is refused rather than
//!   silently truncated.
//! * Encrypted or compressed *records*. Array properties may be deflated, which
//!   is part of 7.4; whole-file compression is not.

use std::io::{self, Read, Write};

use crate::{Error, Result};

/// The magic every binary FBX begins with, including its two reserved bytes.
const MAGIC: &[u8] = b"Kaydara FBX Binary  \x00\x1a\x00";
/// The version this writer emits. Offsets are 32-bit at and below 7400.
const VERSION: u32 = 7400;
/// A record's fixed header: end offset, property count, property bytes, name
/// length.
const RECORD_HEADER_BYTES: usize = 4 + 4 + 4 + 1;
/// The all-zero record that terminates a nested list.
const NULL_RECORD_BYTES: usize = RECORD_HEADER_BYTES;
/// Arrays at or above this many bytes are deflated, matching what the reference
/// writers do. Below it the compression header costs more than it saves.
const DEFLATE_THRESHOLD: usize = 128;
/// Stack chunk that batches array elements into the deflate stream.
const DEFLATE_CHUNK_BYTES: usize = 16 * 1024;
const FOOTER_ID: [u8; 16] = [
    0xfa, 0xbc, 0xab, 0x09, 0xd0, 0xc8, 0xd4, 0x66, 0xb1, 0x76, 0xfb, 0x83, 0x1c, 0xf7, 0x26, 0x7e,
];
const FOOTER_MAGIC: [u8; 16] = [
    0xf8, 0x5a, 0x8c, 0x6a, 0xde, 0xf5, 0xd9, 0x7e, 0xec, 0xe9, 0x0c, 0xe3, 0x75, 0x8f, 0x29, 0x0b,
];
const FOOTER_RESERVED_BYTES: usize = 120;

/// Resource ceilings for the binary FBX parser.
///
/// Parsing clones names, properties and decoded arrays out of the caller's
/// input. The limits therefore cover both structural counts and cumulative
/// owned allocation, rather than treating an already-borrowed input slice as
/// its own memory budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FbxBinaryParseLimits {
    pub maximum_input_bytes: u64,
    pub maximum_nodes: usize,
    pub maximum_properties: usize,
    /// Maximum non-null record levels. Zero accepts only an empty root list.
    pub maximum_depth: usize,
    pub maximum_array_elements: usize,
    pub maximum_total_allocation_bytes: u64,
}

impl Default for FbxBinaryParseLimits {
    fn default() -> Self {
        Self {
            maximum_input_bytes: 1024 * 1024 * 1024,
            maximum_nodes: 1_000_000,
            maximum_properties: 4_000_000,
            maximum_depth: 256,
            maximum_array_elements: 128_000_000,
            maximum_total_allocation_bytes: 1024 * 1024 * 1024,
        }
    }
}

/// Structural and output ceilings for the binary FBX writer.
///
/// The public node tree is caller-owned, but encoding still walks it
/// recursively and can otherwise spend unbounded stack and CPU before the
/// byte-output limit is reached. Zero depth accepts only an empty root list,
/// matching [`FbxBinaryParseLimits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FbxBinaryWriteLimits {
    pub maximum_output_bytes: u64,
    pub maximum_nodes: usize,
    pub maximum_properties: usize,
    pub maximum_depth: usize,
    pub maximum_array_elements: usize,
}

impl Default for FbxBinaryWriteLimits {
    fn default() -> Self {
        Self {
            maximum_output_bytes: u64::from(u32::MAX),
            maximum_nodes: 1_000_000,
            maximum_properties: 4_000_000,
            maximum_depth: 256,
            maximum_array_elements: 128_000_000,
        }
    }
}

/// Deflate effort for large binary FBX arrays.
///
/// Both settings produce a valid FBX 7.4 deflated array; the choice trades
/// encode CPU for file size. `Default` (zlib level 6) reproduces the
/// historical output byte for byte and matches the managed exporter;
/// measurement on mesh float data puts `Fast` at 7.5-12.6x the deflate
/// throughput for about 7% more bytes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FbxArrayCompression {
    /// The historical zlib level 6.
    #[default]
    Default,
    /// The lightest zlib effort, for throughput-oriented pipelines.
    Fast,
}

impl FbxArrayCompression {
    fn zlib_level(self) -> flate2::Compression {
        match self {
            Self::Default => flate2::Compression::default(),
            Self::Fast => flate2::Compression::fast(),
        }
    }
}

struct WriteBudget {
    limits: FbxBinaryWriteLimits,
    nodes: usize,
    properties: usize,
}

impl WriteBudget {
    const fn new(limits: FbxBinaryWriteLimits) -> Self {
        Self {
            limits,
            nodes: 0,
            properties: 0,
        }
    }

    fn charge_node(&mut self, node: &FbxNode, depth: usize) -> Result<()> {
        if depth >= self.limits.maximum_depth {
            return Err(Error::invalid_data(format!(
                "binary FBX writer nesting exceeds {} records",
                self.limits.maximum_depth
            )));
        }
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("binary FBX writer node count overflowed"))?;
        if self.nodes > self.limits.maximum_nodes {
            return Err(Error::invalid_data(format!(
                "binary FBX writer exceeds {} nodes",
                self.limits.maximum_nodes
            )));
        }
        self.properties = self
            .properties
            .checked_add(node.properties.len())
            .ok_or_else(|| Error::invalid_data("binary FBX writer property count overflowed"))?;
        if self.properties > self.limits.maximum_properties {
            return Err(Error::invalid_data(format!(
                "binary FBX writer exceeds {} properties",
                self.limits.maximum_properties
            )));
        }
        for property in &node.properties {
            let count = match property {
                FbxProperty::BoolArray(values) => Some(values.len()),
                FbxProperty::I32Array(values) => Some(values.len()),
                FbxProperty::I64Array(values) => Some(values.len()),
                FbxProperty::F32Array(values) => Some(values.len()),
                FbxProperty::F64Array(values) => Some(values.len()),
                _ => None,
            };
            if let Some(count) = count
                && count > self.limits.maximum_array_elements
            {
                return Err(Error::invalid_data(format!(
                    "binary FBX writer array has {count} elements, exceeding limit {}",
                    self.limits.maximum_array_elements
                )));
            }
        }
        Ok(())
    }
}

struct ParseBudget {
    limits: FbxBinaryParseLimits,
    nodes: usize,
    properties: usize,
    allocation_bytes: u64,
}

impl ParseBudget {
    const fn new(limits: FbxBinaryParseLimits) -> Self {
        Self {
            limits,
            nodes: 0,
            properties: 0,
            allocation_bytes: 0,
        }
    }

    fn charge_nodes(&mut self, count: usize) -> Result<()> {
        self.nodes = self
            .nodes
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("binary FBX node count overflowed"))?;
        if self.nodes > self.limits.maximum_nodes {
            return Err(Error::invalid_data(format!(
                "binary FBX exceeds {} nodes",
                self.limits.maximum_nodes
            )));
        }
        self.charge_elements::<FbxNode>(count, "binary FBX nodes")
    }

    fn charge_properties(&mut self, count: usize) -> Result<()> {
        self.properties = self
            .properties
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("binary FBX property count overflowed"))?;
        if self.properties > self.limits.maximum_properties {
            return Err(Error::invalid_data(format!(
                "binary FBX exceeds {} properties",
                self.limits.maximum_properties
            )));
        }
        self.charge_elements::<FbxProperty>(count, "binary FBX properties")
    }

    fn charge_elements<T>(&mut self, count: usize, label: &str) -> Result<()> {
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(|| Error::invalid_data(format!("{label} allocation overflowed")))?;
        self.charge_bytes(bytes, label)
    }

    fn charge_bytes(&mut self, bytes: usize, label: &str) -> Result<()> {
        let bytes = u64::try_from(bytes)
            .map_err(|_| Error::invalid_data(format!("{label} allocation does not fit u64")))?;
        self.allocation_bytes = self
            .allocation_bytes
            .checked_add(bytes)
            .ok_or_else(|| Error::invalid_data("binary FBX allocation budget overflowed"))?;
        if self.allocation_bytes > self.limits.maximum_total_allocation_bytes {
            return Err(Error::invalid_data(format!(
                "{label} exceeds the {} byte binary FBX allocation budget",
                self.limits.maximum_total_allocation_bytes
            )));
        }
        Ok(())
    }
}

struct EncodeBuffer {
    bytes: Vec<u8>,
    maximum: usize,
}

impl EncodeBuffer {
    const fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
        }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.maximum.saturating_sub(self.bytes.len())
    }

    fn extend_from_slice(&mut self, bytes: &[u8]) -> Result<()> {
        self.reserve(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn push(&mut self, value: u8) -> Result<()> {
        self.reserve(1)?;
        self.bytes.push(value);
        Ok(())
    }

    fn resize(&mut self, length: usize, value: u8) -> Result<()> {
        let additional = length
            .checked_sub(self.bytes.len())
            .ok_or_else(|| Error::invalid_data("binary FBX buffer cannot shrink while encoding"))?;
        self.reserve(additional)?;
        self.bytes.resize(length, value);
        Ok(())
    }

    fn reserve(&mut self, additional: usize) -> Result<()> {
        let length = self
            .bytes
            .len()
            .checked_add(additional)
            .ok_or_else(|| Error::invalid_data("binary FBX output length overflowed"))?;
        if length > self.maximum {
            return Err(Error::invalid_data(format!(
                "binary FBX would be {length} bytes, exceeding limit {}",
                self.maximum,
            )));
        }
        self.bytes.try_reserve(additional).map_err(|error| {
            Error::invalid_data(format!("cannot allocate binary FBX output: {error}"))
        })
    }

    fn into_vec(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for EncodeBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.extend_from_slice(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One typed value in a node's property list.
#[derive(Debug, Clone, PartialEq)]
pub enum FbxProperty {
    Bool(bool),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    /// A UTF-8 string. FBX stores these length-prefixed and unterminated.
    String(String),
    /// Opaque bytes, written as the `R` type.
    Raw(Vec<u8>),
    /// Boolean array, written as the standard FBX `b` type.
    BoolArray(Vec<bool>),
    I32Array(Vec<i32>),
    I64Array(Vec<i64>),
    F32Array(Vec<f32>),
    F64Array(Vec<f64>),
}

impl FbxProperty {
    /// The single-character type code FBX tags this value with.
    const fn type_code(&self) -> u8 {
        match self {
            Self::Bool(_) => b'C',
            Self::I16(_) => b'Y',
            Self::I32(_) => b'I',
            Self::I64(_) => b'L',
            Self::F32(_) => b'F',
            Self::F64(_) => b'D',
            Self::String(_) => b'S',
            Self::Raw(_) => b'R',
            Self::BoolArray(_) => b'b',
            Self::I32Array(_) => b'i',
            Self::I64Array(_) => b'l',
            Self::F32Array(_) => b'f',
            Self::F64Array(_) => b'd',
        }
    }
}

/// One record in the tree: a name, its properties, and its children.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FbxNode {
    pub name: String,
    pub properties: Vec<FbxProperty>,
    pub children: Vec<FbxNode>,
}

impl FbxNode {
    /// A named record with no properties or children.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            properties: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Adds one property, returning the node so records read as a sequence.
    #[must_use]
    pub fn with(mut self, property: FbxProperty) -> Self {
        self.properties.push(property);
        self
    }

    /// Adds one child record.
    #[must_use]
    pub fn child(mut self, child: Self) -> Self {
        self.children.push(child);
        self
    }
}

/// Writes a complete binary FBX 7.4 file.
///
/// `roots` are the top-level records, in the order they should appear.
pub fn write_fbx_binary<W: Write>(roots: &[FbxNode], output: &mut W) -> Result<u64> {
    write_fbx_binary_with_limits(roots, output, FbxBinaryWriteLimits::default())
}

/// Writes a complete binary FBX 7.4 file under explicit structural and output
/// ceilings.
pub fn write_fbx_binary_with_limits<W: Write>(
    roots: &[FbxNode],
    output: &mut W,
    limits: FbxBinaryWriteLimits,
) -> Result<u64> {
    write_fbx_binary_with_encoding(roots, output, limits, FbxArrayCompression::Default)
}

/// Writes a complete binary FBX 7.4 file with an explicit array deflate
/// effort. `FbxArrayCompression::Default` reproduces
/// [`write_fbx_binary_with_limits`] byte for byte.
pub fn write_fbx_binary_with_encoding<W: Write>(
    roots: &[FbxNode],
    output: &mut W,
    limits: FbxBinaryWriteLimits,
    compression: FbxArrayCompression,
) -> Result<u64> {
    let bytes = encode_fbx_binary(roots, limits, compression)?;
    output.write_all(&bytes)?;
    u64::try_from(bytes.len())
        .map_err(|_| Error::invalid_data("binary FBX length does not fit u64"))
}

fn encode_fbx_binary(
    roots: &[FbxNode],
    limits: FbxBinaryWriteLimits,
    compression: FbxArrayCompression,
) -> Result<Vec<u8>> {
    let format_maximum = usize::try_from(u32::MAX).unwrap_or(usize::MAX);
    let requested_maximum = usize::try_from(limits.maximum_output_bytes).unwrap_or(usize::MAX);
    let maximum = requested_maximum.min(format_maximum);
    let mut output = EncodeBuffer::new(maximum);
    let mut budget = WriteBudget::new(limits);
    output.extend_from_slice(MAGIC)?;
    output.extend_from_slice(&VERSION.to_le_bytes())?;

    let mut position = output.len();
    for root in roots {
        encode_node(
            root,
            0,
            &mut position,
            &mut output,
            &mut budget,
            compression,
        )?;
    }
    // The top-level list is terminated the same way a nested one is.
    output.extend_from_slice(&[0_u8; NULL_RECORD_BYTES])?;

    let footer = footer_bytes(output.len())?;
    output.extend_from_slice(&footer)?;
    Ok(output.into_vec())
}

/// Encodes one record and its subtree, advancing `position` past it.
///
/// A record's header stores the absolute offset of its end, so the length has
/// to be known before the header is written. The body is encoded first into a
/// scratch buffer and the header prepended, which costs one copy per record and
/// keeps the offsets exact.
fn encode_node(
    node: &FbxNode,
    depth: usize,
    position: &mut usize,
    output: &mut EncodeBuffer,
    budget: &mut WriteBudget,
    compression: FbxArrayCompression,
) -> Result<()> {
    budget.charge_node(node, depth)?;
    let name_length = u8::try_from(node.name.len())
        .map_err(|_| Error::invalid_data("binary FBX node names are limited to 255 bytes"))?;

    let fixed_prefix = RECORD_HEADER_BYTES
        .checked_add(node.name.len())
        .ok_or_else(|| Error::invalid_data("binary FBX record prefix length overflowed"))?;
    let mut properties = EncodeBuffer::new(output.remaining().saturating_sub(fixed_prefix));
    for property in &node.properties {
        encode_property(property, &mut properties, compression)?;
    }

    let header_end = position
        .checked_add(RECORD_HEADER_BYTES)
        .and_then(|value| value.checked_add(node.name.len()))
        .and_then(|value| value.checked_add(properties.len()))
        .ok_or_else(|| Error::invalid_data("binary FBX record offset overflowed"))?;

    let node_prefix = fixed_prefix
        .checked_add(properties.len())
        .ok_or_else(|| Error::invalid_data("binary FBX record prefix length overflowed"))?;
    let mut children = EncodeBuffer::new(output.remaining().saturating_sub(node_prefix));
    if !node.children.is_empty() {
        let mut child_position = header_end;
        for child in &node.children {
            encode_node(
                child,
                depth
                    .checked_add(1)
                    .ok_or_else(|| Error::invalid_data("binary FBX writer depth overflowed"))?,
                &mut child_position,
                &mut children,
                budget,
                compression,
            )?;
        }
        // A record with children ends with a null record; one without has none,
        // and a reader distinguishes the two by the end offset alone.
        children.extend_from_slice(&[0_u8; NULL_RECORD_BYTES])?;
    }

    let end_offset = header_end
        .checked_add(children.len())
        .ok_or_else(|| Error::invalid_data("binary FBX record offset overflowed"))?;
    let end_offset = u32::try_from(end_offset)
        .map_err(|_| Error::invalid_data("binary FBX exceeds the 4 GiB the 7.4 format allows"))?;

    output.extend_from_slice(&end_offset.to_le_bytes())?;
    output.extend_from_slice(
        &u32::try_from(node.properties.len())
            .map_err(|_| Error::invalid_data("binary FBX property count overflowed"))?
            .to_le_bytes(),
    )?;
    output.extend_from_slice(
        &u32::try_from(properties.len())
            .map_err(|_| Error::invalid_data("binary FBX property block overflowed"))?
            .to_le_bytes(),
    )?;
    output.push(name_length)?;
    output.extend_from_slice(node.name.as_bytes())?;
    output.extend_from_slice(&properties.bytes)?;
    output.extend_from_slice(&children.bytes)?;

    *position = end_offset as usize;
    Ok(())
}

fn encode_property(
    property: &FbxProperty,
    output: &mut EncodeBuffer,
    compression: FbxArrayCompression,
) -> Result<()> {
    output.push(property.type_code())?;
    match property {
        FbxProperty::Bool(value) => output.push(u8::from(*value))?,
        FbxProperty::I16(value) => output.extend_from_slice(&value.to_le_bytes())?,
        FbxProperty::I32(value) => output.extend_from_slice(&value.to_le_bytes())?,
        FbxProperty::I64(value) => output.extend_from_slice(&value.to_le_bytes())?,
        FbxProperty::F32(value) => output.extend_from_slice(&value.to_le_bytes())?,
        FbxProperty::F64(value) => output.extend_from_slice(&value.to_le_bytes())?,
        FbxProperty::String(value) => {
            let length = u32::try_from(value.len())
                .map_err(|_| Error::invalid_data("binary FBX string length overflowed"))?;
            output.extend_from_slice(&length.to_le_bytes())?;
            output.extend_from_slice(value.as_bytes())?;
        }
        FbxProperty::Raw(value) => {
            let length = u32::try_from(value.len())
                .map_err(|_| Error::invalid_data("binary FBX raw length overflowed"))?;
            output.extend_from_slice(&length.to_le_bytes())?;
            output.extend_from_slice(value)?;
        }
        FbxProperty::BoolArray(values) => {
            encode_array(
                values.iter().map(|value| [u8::from(*value)]),
                output,
                compression,
            )?;
        }
        FbxProperty::I32Array(values) => {
            encode_array(
                values.iter().copied().map(i32::to_le_bytes),
                output,
                compression,
            )?;
        }
        FbxProperty::I64Array(values) => {
            encode_array(
                values.iter().copied().map(i64::to_le_bytes),
                output,
                compression,
            )?;
        }
        FbxProperty::F32Array(values) => {
            encode_array(
                values.iter().copied().map(f32::to_le_bytes),
                output,
                compression,
            )?;
        }
        FbxProperty::F64Array(values) => {
            encode_array(
                values.iter().copied().map(f64::to_le_bytes),
                output,
                compression,
            )?;
        }
    }
    Ok(())
}

/// Writes a numeric array without first duplicating its full uncompressed byte
/// representation. Large arrays stream directly into zlib; short arrays stream
/// directly into the record property block.
fn encode_array<const N: usize>(
    values: impl ExactSizeIterator<Item = [u8; N]>,
    output: &mut EncodeBuffer,
    compression: FbxArrayCompression,
) -> Result<()> {
    let raw_length = values
        .len()
        .checked_mul(N)
        .ok_or_else(|| Error::invalid_data("binary FBX array byte length overflowed"))?;
    let count = u32::try_from(values.len())
        .map_err(|_| Error::invalid_data("binary FBX array length overflowed"))?;
    if raw_length < DEFLATE_THRESHOLD {
        output.extend_from_slice(&count.to_le_bytes())?;
        output.extend_from_slice(&0_u32.to_le_bytes())?;
        output.extend_from_slice(
            &u32::try_from(raw_length)
                .map_err(|_| Error::invalid_data("binary FBX array size overflowed"))?
                .to_le_bytes(),
        )?;
        for value in values {
            output.extend_from_slice(&value)?;
        }
        return Ok(());
    }

    let compressed_maximum = output.remaining().saturating_sub(12);
    let mut encoder = flate2::write::ZlibEncoder::new(
        EncodeBuffer::new(compressed_maximum),
        compression.zlib_level(),
    );
    // Feeding the compressor per element made its per-call overhead about a
    // third of large-array writes; batching through one bounded chunk keeps
    // the byte stream - and therefore the output - identical.
    let mut chunk = [0_u8; DEFLATE_CHUNK_BYTES];
    let mut filled = 0_usize;
    for value in values {
        if filled + N > DEFLATE_CHUNK_BYTES {
            encoder.write_all(&chunk[..filled]).map_err(|error| {
                Error::invalid_data(format!("cannot deflate an FBX array: {error}"))
            })?;
            filled = 0;
        }
        chunk[filled..filled + N].copy_from_slice(&value);
        filled += N;
    }
    if filled > 0 {
        encoder.write_all(&chunk[..filled]).map_err(|error| {
            Error::invalid_data(format!("cannot deflate an FBX array: {error}"))
        })?;
    }
    let deflated = encoder
        .finish()
        .map_err(|error| Error::invalid_data(format!("cannot deflate an FBX array: {error}")))?;
    output.extend_from_slice(&count.to_le_bytes())?;
    output.extend_from_slice(&1_u32.to_le_bytes())?;
    output.extend_from_slice(
        &u32::try_from(deflated.len())
            .map_err(|_| Error::invalid_data("binary FBX array size overflowed"))?
            .to_le_bytes(),
    )?;
    output.extend_from_slice(&deflated.bytes)?;
    Ok(())
}

/// The trailer every binary FBX ends with.
///
/// A 16-byte footer id, padding to a 16-byte boundary, the version repeated,
/// 120 zero bytes and a 16-byte magic. The padding is what makes the total
/// length a multiple of 16, which readers rely on to find the trailer.
fn footer_bytes(body_length: usize) -> Result<Vec<u8>> {
    let mut footer = EncodeBuffer::new(
        FOOTER_ID.len() + 16 + 4 + 4 + FOOTER_RESERVED_BYTES + FOOTER_MAGIC.len(),
    );
    footer.extend_from_slice(&FOOTER_ID)?;
    footer.resize(FOOTER_ID.len() + footer_padding(body_length)?, 0)?;
    footer.extend_from_slice(&0_u32.to_le_bytes())?;
    footer.extend_from_slice(&VERSION.to_le_bytes())?;
    footer.resize(footer.len() + FOOTER_RESERVED_BYTES, 0)?;
    footer.extend_from_slice(&FOOTER_MAGIC)?;
    Ok(footer.into_vec())
}

fn footer_padding(body_length: usize) -> Result<usize> {
    let with_id = body_length
        .checked_add(FOOTER_ID.len())
        .ok_or_else(|| Error::invalid_data("binary FBX footer offset overflowed"))?;
    // Reference writers always emit at least one padding byte. A body already
    // aligned after the footer id therefore receives a complete 16-byte block.
    Ok(16 - (with_id % 16))
}

/// Materializes a binary FBX under an explicit output cap.
pub fn read_fbx_binary(roots: &[FbxNode], maximum_output_bytes: u64) -> Result<Vec<u8>> {
    read_fbx_binary_with_limits(
        roots,
        FbxBinaryWriteLimits {
            maximum_output_bytes,
            ..FbxBinaryWriteLimits::default()
        },
    )
}

/// Materializes a binary FBX under explicit structural and output ceilings.
pub fn read_fbx_binary_with_limits(
    roots: &[FbxNode],
    limits: FbxBinaryWriteLimits,
) -> Result<Vec<u8>> {
    encode_fbx_binary(roots, limits, FbxArrayCompression::Default)
}

/// Materializes a binary FBX with an explicit array deflate effort;
/// `FbxArrayCompression::Default` reproduces
/// [`read_fbx_binary_with_limits`] byte for byte.
pub fn read_fbx_binary_with_encoding(
    roots: &[FbxNode],
    limits: FbxBinaryWriteLimits,
    compression: FbxArrayCompression,
) -> Result<Vec<u8>> {
    encode_fbx_binary(roots, limits, compression)
}

/// Reads back a binary FBX node tree.
///
/// Present so a caller can verify what was written, and so the writer has a
/// decoder to be checked against that does not share its code.
pub fn parse_fbx_binary(bytes: &[u8]) -> Result<Vec<FbxNode>> {
    parse_fbx_binary_with_limits(bytes, FbxBinaryParseLimits::default())
}

/// Reads back a binary FBX node tree under explicit structural and allocation
/// ceilings.
pub fn parse_fbx_binary_with_limits(
    bytes: &[u8],
    limits: FbxBinaryParseLimits,
) -> Result<Vec<FbxNode>> {
    let input_length = u64::try_from(bytes.len())
        .map_err(|_| Error::invalid_data("binary FBX input length does not fit u64"))?;
    if input_length > limits.maximum_input_bytes {
        return Err(Error::invalid_data(format!(
            "binary FBX input is {input_length} bytes, exceeding limit {}",
            limits.maximum_input_bytes
        )));
    }
    if bytes.len() < MAGIC.len() + 4 {
        return Err(Error::invalid_data("binary FBX is shorter than its header"));
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(Error::invalid_data("binary FBX magic does not match"));
    }
    let version = u32::from_le_bytes(
        bytes[MAGIC.len()..MAGIC.len() + 4]
            .try_into()
            .expect("a four byte version"),
    );
    if version > VERSION {
        return Err(Error::unsupported(format!(
            "binary FBX version {version} uses 64-bit record offsets"
        )));
    }
    let mut cursor = MAGIC.len() + 4;
    let mut nodes = Vec::new();
    let mut budget = ParseBudget::new(limits);
    while let Some(node) = parse_node(bytes, &mut cursor, 0, &mut budget)? {
        nodes.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot allocate binary FBX root records: {error}"))
        })?;
        nodes.push(node);
    }
    validate_footer(bytes, cursor, version)?;
    Ok(nodes)
}

fn validate_footer(bytes: &[u8], body_end: usize, version: u32) -> Result<()> {
    let padding = footer_padding(body_end)?;
    let footer_length = FOOTER_ID
        .len()
        .checked_add(padding)
        .and_then(|value| value.checked_add(4 + 4 + FOOTER_RESERVED_BYTES))
        .and_then(|value| value.checked_add(FOOTER_MAGIC.len()))
        .ok_or_else(|| Error::invalid_data("binary FBX footer length overflowed"))?;
    let file_end = body_end
        .checked_add(footer_length)
        .ok_or_else(|| Error::invalid_data("binary FBX footer end offset overflowed"))?;
    if bytes.len() != file_end {
        return Err(Error::invalid_data(format!(
            "binary FBX footer should end at byte {file_end}, not {}",
            bytes.len()
        )));
    }
    let footer = &bytes[body_end..file_end];
    if footer[..FOOTER_ID.len()] != FOOTER_ID {
        return Err(Error::invalid_data("binary FBX footer id does not match"));
    }

    let padding_start = FOOTER_ID.len();
    let padding_end = padding_start + padding;
    if footer[padding_start..padding_end]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(Error::invalid_data("binary FBX footer padding is not zero"));
    }
    let zero_end = padding_end + 4;
    if footer[padding_end..zero_end].iter().any(|byte| *byte != 0) {
        return Err(Error::invalid_data(
            "binary FBX footer version prefix is not zero",
        ));
    }
    let version_end = zero_end + 4;
    let footer_version = u32::from_le_bytes(
        footer[zero_end..version_end]
            .try_into()
            .expect("a four byte footer version"),
    );
    if footer_version != version {
        return Err(Error::invalid_data(format!(
            "binary FBX footer repeats version {footer_version}, not {version}"
        )));
    }
    let reserved_end = version_end + FOOTER_RESERVED_BYTES;
    if footer[version_end..reserved_end]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(Error::invalid_data(
            "binary FBX footer reserved bytes are not zero",
        ));
    }
    if footer[reserved_end..] != FOOTER_MAGIC {
        return Err(Error::invalid_data(
            "binary FBX footer magic does not match",
        ));
    }
    Ok(())
}

/// Parses one record, returning `None` at the null record that ends a list.
fn parse_node(
    bytes: &[u8],
    cursor: &mut usize,
    depth: usize,
    budget: &mut ParseBudget,
) -> Result<Option<FbxNode>> {
    let Some(header) = parse_node_header(bytes, cursor)? else {
        return Ok(None);
    };
    if depth >= budget.limits.maximum_depth {
        return Err(Error::invalid_data(format!(
            "binary FBX nesting exceeds {} records",
            budget.limits.maximum_depth
        )));
    }
    budget.charge_nodes(1)?;

    let name_start = header.header_end;
    let name_end = name_start
        .checked_add(header.name_length)
        .ok_or_else(|| Error::invalid_data("binary FBX record name offset overflowed"))?;
    let name_bytes = bytes
        .get(name_start..name_end)
        .ok_or_else(|| Error::invalid_data("binary FBX record name runs past the file"))?;
    let name_text = std::str::from_utf8(name_bytes)
        .map_err(|_| Error::invalid_data("binary FBX record name is not UTF-8"))?;
    budget.charge_bytes(name_text.len(), "binary FBX record names")?;
    let mut name = String::new();
    name.try_reserve_exact(name_text.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate a binary FBX record name: {error}"))
    })?;
    name.push_str(name_text);

    let mut property_cursor = name_end;
    let property_end = property_cursor
        .checked_add(header.property_bytes)
        .ok_or_else(|| Error::invalid_data("binary FBX property block offset overflowed"))?;
    if property_end > header.end_offset || property_end > bytes.len() {
        return Err(Error::invalid_data(
            "binary FBX property block runs past its record",
        ));
    }
    if header.property_count > header.property_bytes {
        return Err(Error::invalid_data(
            "binary FBX property count exceeds its byte block",
        ));
    }
    budget.charge_properties(header.property_count)?;
    let properties = parse_properties(
        bytes,
        &mut property_cursor,
        property_end,
        header.property_count,
        budget,
    )?;

    let mut children = Vec::new();
    let mut child_cursor = property_end;
    while child_cursor < header.end_offset {
        match parse_node(bytes, &mut child_cursor, depth + 1, budget)? {
            Some(child) => {
                children.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!(
                        "cannot allocate binary FBX child records: {error}"
                    ))
                })?;
                children.push(child);
            }
            None => break,
        }
    }
    if child_cursor != header.end_offset {
        return Err(Error::invalid_data(
            "binary FBX child records do not end at their parent boundary",
        ));
    }

    *cursor = header.end_offset;
    Ok(Some(FbxNode {
        name,
        properties,
        children,
    }))
}

struct ParsedNodeHeader {
    header_end: usize,
    end_offset: usize,
    property_count: usize,
    property_bytes: usize,
    name_length: usize,
}

fn parse_node_header(bytes: &[u8], cursor: &mut usize) -> Result<Option<ParsedNodeHeader>> {
    let header_end = cursor
        .checked_add(RECORD_HEADER_BYTES)
        .ok_or_else(|| Error::invalid_data("binary FBX record header offset overflowed"))?;
    let header = bytes
        .get(*cursor..header_end)
        .ok_or_else(|| Error::invalid_data("binary FBX record header runs past the file"))?;
    let end_offset = u32::from_le_bytes(header[0..4].try_into().expect("four bytes")) as usize;
    let property_count = u32::from_le_bytes(header[4..8].try_into().expect("four bytes")) as usize;
    let property_bytes = u32::from_le_bytes(header[8..12].try_into().expect("four bytes")) as usize;
    let name_length = usize::from(header[12]);
    if end_offset == 0 && property_count == 0 && property_bytes == 0 && name_length == 0 {
        *cursor = header_end;
        return Ok(None);
    }
    if end_offset <= *cursor || end_offset > bytes.len() {
        return Err(Error::invalid_data(
            "binary FBX record end offset is invalid",
        ));
    }
    Ok(Some(ParsedNodeHeader {
        header_end,
        end_offset,
        property_count,
        property_bytes,
        name_length,
    }))
}

fn parse_properties(
    bytes: &[u8],
    cursor: &mut usize,
    property_end: usize,
    property_count: usize,
    budget: &mut ParseBudget,
) -> Result<Vec<FbxProperty>> {
    let mut properties = Vec::new();
    properties
        .try_reserve_exact(property_count)
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate binary FBX properties: {error}"))
        })?;
    for _ in 0..property_count {
        properties.push(parse_property(bytes, cursor, property_end, budget)?);
    }
    if *cursor != property_end {
        return Err(Error::invalid_data(
            "binary FBX property block length disagrees with its contents",
        ));
    }
    Ok(properties)
}

fn parse_property(
    bytes: &[u8],
    cursor: &mut usize,
    property_end: usize,
    budget: &mut ParseBudget,
) -> Result<FbxProperty> {
    let mut reader = PropertyReader {
        bytes,
        cursor,
        end: property_end,
    };
    let code = reader.read(1)?[0];
    match code {
        b'C' => Ok(FbxProperty::Bool(reader.read(1)?[0] != 0)),
        b'Y' => Ok(FbxProperty::I16(i16::from_le_bytes(
            reader.read(2)?.try_into().expect("two bytes"),
        ))),
        b'I' => Ok(FbxProperty::I32(i32::from_le_bytes(
            reader.read(4)?.try_into().expect("four bytes"),
        ))),
        b'L' => Ok(FbxProperty::I64(i64::from_le_bytes(
            reader.read(8)?.try_into().expect("eight bytes"),
        ))),
        b'F' => Ok(FbxProperty::F32(f32::from_le_bytes(
            reader.read(4)?.try_into().expect("four bytes"),
        ))),
        b'D' => Ok(FbxProperty::F64(f64::from_le_bytes(
            reader.read(8)?.try_into().expect("eight bytes"),
        ))),
        b'S' | b'R' => parse_blob_property(code, &mut reader, budget),
        b'b' | b'i' | b'l' | b'f' | b'd' => parse_array_property(code, &mut reader, budget),
        other => Err(Error::unsupported(format!(
            "binary FBX property type {:?}",
            char::from(other)
        ))),
    }
}

struct PropertyReader<'bytes, 'cursor> {
    bytes: &'bytes [u8],
    cursor: &'cursor mut usize,
    end: usize,
}

impl<'bytes> PropertyReader<'bytes, '_> {
    fn read(&mut self, length: usize) -> Result<&'bytes [u8]> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("binary FBX property offset overflowed"))?;
        if end > self.end {
            return Err(Error::invalid_data(
                "binary FBX property runs past its property block",
            ));
        }
        let slice = self
            .bytes
            .get(*self.cursor..end)
            .ok_or_else(|| Error::invalid_data("binary FBX property runs past the file"))?;
        *self.cursor = end;
        Ok(slice)
    }
}

fn parse_blob_property(
    code: u8,
    reader: &mut PropertyReader<'_, '_>,
    budget: &mut ParseBudget,
) -> Result<FbxProperty> {
    let length = u32::from_le_bytes(reader.read(4)?.try_into().expect("four bytes")) as usize;
    let source = reader.read(length)?;
    budget.charge_bytes(source.len(), "binary FBX string and raw properties")?;
    let mut value = Vec::new();
    value.try_reserve_exact(source.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate a binary FBX property: {error}"))
    })?;
    value.extend_from_slice(source);
    if code == b'S' {
        Ok(FbxProperty::String(String::from_utf8(value).map_err(
            |_| Error::invalid_data("binary FBX string is not UTF-8"),
        )?))
    } else {
        Ok(FbxProperty::Raw(value))
    }
}

fn parse_array_property(
    code: u8,
    reader: &mut PropertyReader<'_, '_>,
    budget: &mut ParseBudget,
) -> Result<FbxProperty> {
    let count = u32::from_le_bytes(reader.read(4)?.try_into().expect("four bytes")) as usize;
    let encoding = u32::from_le_bytes(reader.read(4)?.try_into().expect("four bytes"));
    let stored = u32::from_le_bytes(reader.read(4)?.try_into().expect("four bytes")) as usize;
    let payload = reader.read(stored)?;
    if count > budget.limits.maximum_array_elements {
        return Err(Error::invalid_data(format!(
            "binary FBX array has {count} elements, exceeding limit {}",
            budget.limits.maximum_array_elements
        )));
    }
    let decoded_length = count
        .checked_mul(array_element_width(code))
        .ok_or_else(|| Error::invalid_data("binary FBX decoded array length overflowed"))?;
    budget.charge_bytes(decoded_length, "binary FBX decoded arrays")?;
    let inflated = match encoding {
        0 => None,
        1 => Some(inflate_array(payload, decoded_length, budget)?),
        other => {
            return Err(Error::unsupported(format!(
                "binary FBX array encoding {other}"
            )));
        }
    };
    decode_array(code, count, inflated.as_deref().unwrap_or(payload))
}

fn inflate_array(
    payload: &[u8],
    decoded_length: usize,
    budget: &mut ParseBudget,
) -> Result<Vec<u8>> {
    budget.charge_bytes(decoded_length, "binary FBX inflated array buffers")?;
    // Use the BufRead adapter so bytes following the zlib member remain
    // visible. The Read adapter owns a 32 KiB look-ahead buffer and may read
    // past the member, which would make it impossible to prove that the FBX
    // property's declared compressed payload is exactly one zlib stream.
    let mut decoder = flate2::bufread::ZlibDecoder::new(payload);
    let mut inflated = Vec::new();
    inflated
        .try_reserve_exact(decoded_length)
        .map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate a decoded binary FBX array: {error}"
            ))
        })?;
    inflated.resize(decoded_length, 0);
    decoder.read_exact(&mut inflated).map_err(|error| {
        Error::invalid_data(format!(
            "cannot inflate the declared binary FBX array length: {error}"
        ))
    })?;
    let mut extra = [0_u8; 1];
    if decoder
        .read(&mut extra)
        .map_err(|error| Error::invalid_data(format!("cannot finish an FBX array: {error}")))?
        != 0
    {
        return Err(Error::invalid_data(
            "binary FBX array expands beyond its declared element count",
        ));
    }
    if !decoder.get_ref().is_empty() {
        return Err(Error::invalid_data(
            "binary FBX compressed array has bytes after its zlib stream",
        ));
    }
    Ok(inflated)
}

const fn array_element_width(code: u8) -> usize {
    match code {
        b'b' => 1,
        b'i' | b'f' => 4,
        b'l' | b'd' => 8,
        _ => unreachable!(),
    }
}

fn decode_array(code: u8, count: usize, raw: &[u8]) -> Result<FbxProperty> {
    fn values<T, const N: usize>(
        raw: &[u8],
        count: usize,
        decode: fn([u8; N]) -> T,
    ) -> Result<Vec<T>> {
        let expected = count
            .checked_mul(N)
            .ok_or_else(|| Error::invalid_data("binary FBX array length overflowed"))?;
        if raw.len() != expected {
            return Err(Error::invalid_data(
                "binary FBX array length disagrees with its payload",
            ));
        }
        let mut output = Vec::new();
        output.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!("cannot allocate a binary FBX array: {error}"))
        })?;
        for chunk in raw.chunks_exact(N) {
            output.push(decode(chunk.try_into().expect("an exact chunk")));
        }
        Ok(output)
    }
    match code {
        b'b' => Ok(FbxProperty::BoolArray(values(raw, count, |[value]| {
            value != 0
        })?)),
        b'i' => Ok(FbxProperty::I32Array(values(
            raw,
            count,
            i32::from_le_bytes,
        )?)),
        b'l' => Ok(FbxProperty::I64Array(values(
            raw,
            count,
            i64::from_le_bytes,
        )?)),
        b'f' => Ok(FbxProperty::F32Array(values(
            raw,
            count,
            f32::from_le_bytes,
        )?)),
        _ => Ok(FbxProperty::F64Array(values(
            raw,
            count,
            f64::from_le_bytes,
        )?)),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FOOTER_ID, FbxArrayCompression, FbxBinaryParseLimits, FbxBinaryWriteLimits, FbxNode,
        FbxProperty, MAGIC, NULL_RECORD_BYTES, RECORD_HEADER_BYTES, VERSION, footer_padding,
        parse_fbx_binary, parse_fbx_binary_with_limits, read_fbx_binary,
        read_fbx_binary_with_encoding, read_fbx_binary_with_limits, write_fbx_binary,
        write_fbx_binary_with_limits,
    };

    fn sample() -> Vec<FbxNode> {
        vec![
            FbxNode::new("FBXHeaderExtension")
                .child(FbxNode::new("FBXVersion").with(FbxProperty::I32(7400))),
            FbxNode::new("Objects").child(
                FbxNode::new("Geometry")
                    .with(FbxProperty::I64(1234))
                    .with(FbxProperty::String("Geometry::mesh".to_owned()))
                    .with(FbxProperty::String("Mesh".to_owned()))
                    .child(
                        FbxNode::new("Vertices").with(FbxProperty::F64Array(vec![0.0, 1.0, 2.0])),
                    )
                    .child(
                        FbxNode::new("PolygonVertexIndex")
                            .with(FbxProperty::I32Array(vec![0, 1, -3])),
                    ),
            ),
        ]
    }

    #[test]
    fn writes_a_header_version_and_footer_the_format_requires() {
        let mut output = Vec::new();
        let written = write_fbx_binary(&sample(), &mut output).unwrap();
        assert_eq!(written, u64::try_from(output.len()).unwrap());
        assert!(output.starts_with(MAGIC));
        assert_eq!(
            u32::from_le_bytes(output[MAGIC.len()..MAGIC.len() + 4].try_into().unwrap()),
            VERSION
        );
        // Readers scan back from the end for the trailer, which only lands on a
        // predictable boundary if the padding was computed from the body.
        assert_eq!(output.len() % 16, 0);
    }

    #[test]
    fn writes_and_reads_a_full_footer_padding_block_on_an_exact_boundary() {
        let root = FbxNode::new("elevenbytes");
        let mut output = Vec::new();
        write_fbx_binary(std::slice::from_ref(&root), &mut output).unwrap();

        let body_end = MAGIC.len() + 4 + RECORD_HEADER_BYTES + root.name.len() + NULL_RECORD_BYTES;
        assert_eq!(body_end % 16, 0);
        assert_eq!(footer_padding(body_end).unwrap(), 16);
        assert_eq!(&output[body_end..body_end + FOOTER_ID.len()], &FOOTER_ID);
        assert!(
            output[body_end + FOOTER_ID.len()..body_end + FOOTER_ID.len() + 16]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(parse_fbx_binary(&output).unwrap(), vec![root]);
    }

    #[test]
    fn rejects_truncated_corrupt_or_trailing_footer_bytes() {
        let root = FbxNode::new("leaf");
        let mut output = Vec::new();
        write_fbx_binary(std::slice::from_ref(&root), &mut output).unwrap();
        let body_end = MAGIC.len() + 4 + RECORD_HEADER_BYTES + root.name.len() + NULL_RECORD_BYTES;

        let mut truncated = output.clone();
        truncated.pop();
        assert!(
            parse_fbx_binary(&truncated)
                .unwrap_err()
                .to_string()
                .contains("footer should end")
        );

        let mut trailing = output.clone();
        trailing.push(0);
        assert!(
            parse_fbx_binary(&trailing)
                .unwrap_err()
                .to_string()
                .contains("footer should end")
        );

        let padding = footer_padding(body_end).unwrap();
        let padding_start = body_end + FOOTER_ID.len();
        let zero_start = padding_start + padding;
        let version_start = zero_start + 4;
        let reserved_start = version_start + 4;
        let magic_start = reserved_start + super::FOOTER_RESERVED_BYTES;
        for (offset, expected) in [
            (body_end, "footer id"),
            (padding_start, "footer padding"),
            (zero_start, "version prefix"),
            (version_start, "repeats version"),
            (reserved_start, "reserved bytes"),
            (magic_start, "footer magic"),
        ] {
            let mut corrupt = output.clone();
            corrupt[offset] ^= 1;
            assert!(
                parse_fbx_binary(&corrupt)
                    .unwrap_err()
                    .to_string()
                    .contains(expected),
                "corruption at {offset} did not report {expected}"
            );
        }
    }

    #[test]
    fn round_trips_every_property_type() {
        let node = FbxNode::new("Every")
            .with(FbxProperty::Bool(true))
            .with(FbxProperty::I16(-2))
            .with(FbxProperty::I32(-3))
            .with(FbxProperty::I64(-4))
            .with(FbxProperty::F32(1.5))
            .with(FbxProperty::F64(-2.5))
            .with(FbxProperty::String("name".to_owned()))
            .with(FbxProperty::Raw(vec![1, 2, 3]))
            .with(FbxProperty::BoolArray(vec![false, true, false, true]))
            .with(FbxProperty::I32Array(vec![1, -2, 3]))
            .with(FbxProperty::I64Array(vec![4, -5]))
            .with(FbxProperty::F32Array(vec![0.25, -0.5]))
            .with(FbxProperty::F64Array(vec![0.125]));
        let mut output = Vec::new();
        write_fbx_binary(std::slice::from_ref(&node), &mut output).unwrap();
        let parsed = parse_fbx_binary(&output).unwrap();
        assert_eq!(parsed, vec![node]);
    }

    #[test]
    fn round_trips_raw_and_deflated_boolean_arrays() {
        let raw = FbxNode::new("RawBool").with(FbxProperty::BoolArray(vec![false, true, true]));
        let deflated = FbxNode::new("DeflatedBool").with(FbxProperty::BoolArray(
            (0..512).map(|index| index % 3 == 0).collect(),
        ));
        let roots = vec![raw, deflated];
        let mut output = Vec::new();
        write_fbx_binary(&roots, &mut output).unwrap();
        assert_eq!(parse_fbx_binary(&output).unwrap(), roots);

        // FBX booleans follow the scalar `C` convention: zero is false and
        // every non-zero byte is true. The writer canonicalises to 0/1, while
        // the reader must still accept files produced with another true byte.
        let property = MAGIC.len() + 4 + RECORD_HEADER_BYTES + "RawBool".len();
        assert_eq!(output[property], b'b');
        assert_eq!(
            u32::from_le_bytes(output[property + 5..property + 9].try_into().unwrap()),
            0
        );
        let second =
            u32::from_le_bytes(output[MAGIC.len() + 4..MAGIC.len() + 8].try_into().unwrap())
                as usize;
        let compressed_property = second + RECORD_HEADER_BYTES + "DeflatedBool".len();
        assert_eq!(output[compressed_property], b'b');
        assert_eq!(
            u32::from_le_bytes(
                output[compressed_property + 5..compressed_property + 9]
                    .try_into()
                    .unwrap()
            ),
            1
        );
        let payload = property + 1 + 4 + 4 + 4;
        output[payload + 1] = 2;
        output[payload + 2] = u8::MAX;
        assert_eq!(parse_fbx_binary(&output).unwrap(), roots);
    }

    #[test]
    fn deflates_only_arrays_worth_compressing() {
        // A long run compresses; a short one would grow.
        let long = FbxNode::new("Long").with(FbxProperty::F64Array(vec![1.0; 256]));
        let short = FbxNode::new("Short").with(FbxProperty::F64Array(vec![1.0; 2]));
        let mut long_output = Vec::new();
        write_fbx_binary(std::slice::from_ref(&long), &mut long_output).unwrap();
        let mut short_output = Vec::new();
        write_fbx_binary(std::slice::from_ref(&short), &mut short_output).unwrap();

        assert!(
            long_output.len() < 256 * 8,
            "a long array should not be stored raw"
        );
        assert_eq!(
            read_fbx_binary(
                std::slice::from_ref(&long),
                u64::try_from(long_output.len()).unwrap(),
            )
            .unwrap(),
            long_output
        );
        // Both still parse back to exactly what went in, whichever path they took.
        assert_eq!(parse_fbx_binary(&long_output).unwrap(), vec![long]);
        assert_eq!(parse_fbx_binary(&short_output).unwrap(), vec![short]);
    }

    #[test]
    fn nests_records_and_reports_their_own_end_offsets() {
        let mut output = Vec::new();
        write_fbx_binary(&sample(), &mut output).unwrap();
        let parsed = parse_fbx_binary(&output).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "FBXHeaderExtension");
        assert_eq!(parsed[0].children[0].name, "FBXVersion");
        let objects = &parsed[1];
        assert_eq!(objects.name, "Objects");
        let geometry = &objects.children[0];
        assert_eq!(geometry.name, "Geometry");
        assert_eq!(geometry.properties.len(), 3);
        assert_eq!(geometry.children.len(), 2);
        assert_eq!(
            geometry.children[1].properties[0],
            FbxProperty::I32Array(vec![0, 1, -3])
        );
    }

    #[test]
    fn refuses_input_that_is_not_a_binary_fbx() {
        assert!(parse_fbx_binary(b"too short").is_err());
        let mut wrong = MAGIC.to_vec();
        wrong[0] = b'X';
        wrong.extend_from_slice(&VERSION.to_le_bytes());
        assert!(parse_fbx_binary(&wrong).is_err());

        // 7.5 widens every record offset to 64 bits, so a 7.4 parser must
        // refuse it rather than read the first half of an offset.
        let mut newer = MAGIC.to_vec();
        newer.extend_from_slice(&7500_u32.to_le_bytes());
        assert!(parse_fbx_binary(&newer).is_err());
    }

    #[test]
    fn honours_the_output_budget() {
        let exact = read_fbx_binary(&sample(), u64::MAX).unwrap();
        assert_eq!(
            read_fbx_binary(&sample(), u64::try_from(exact.len()).unwrap()).unwrap(),
            exact
        );
        let error = read_fbx_binary(
            &sample(),
            u64::try_from(exact.len()).unwrap().saturating_sub(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeding limit"));
    }

    /// The fast array effort must change only the deflated stream: the
    /// decoded node tree round-trips identically through the independent
    /// reader, and the default effort reproduces the historical bytes.
    #[test]
    fn fast_array_compression_roundtrips_and_default_is_byte_stable() {
        let values: Vec<f64> = (0..8192).map(|index| f64::from(index) * 0.25).collect();
        let root = FbxNode::new("Vertices").with(FbxProperty::F64Array(values));
        let roots = [root];
        let default_bytes =
            read_fbx_binary_with_limits(&roots, FbxBinaryWriteLimits::default()).unwrap();
        let default_again = read_fbx_binary_with_encoding(
            &roots,
            FbxBinaryWriteLimits::default(),
            FbxArrayCompression::Default,
        )
        .unwrap();
        assert_eq!(default_bytes, default_again);
        let fast_bytes = read_fbx_binary_with_encoding(
            &roots,
            FbxBinaryWriteLimits::default(),
            FbxArrayCompression::Fast,
        )
        .unwrap();
        assert_ne!(fast_bytes, default_bytes);
        let default_tree = parse_fbx_binary(&default_bytes).unwrap();
        let fast_tree = parse_fbx_binary(&fast_bytes).unwrap();
        assert_eq!(format!("{default_tree:?}"), format!("{fast_tree:?}"));
    }

    #[test]
    fn writer_enforces_structural_limits_before_writing_output() {
        fn rejects(roots: &[FbxNode], limits: FbxBinaryWriteLimits, expected: &str) {
            let mut output = Vec::new();
            let error = write_fbx_binary_with_limits(roots, &mut output, limits).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
            assert!(output.is_empty(), "failed encoding wrote a partial file");
        }

        let empty_limits = FbxBinaryWriteLimits {
            maximum_nodes: 0,
            maximum_properties: 0,
            maximum_depth: 0,
            maximum_array_elements: 0,
            ..FbxBinaryWriteLimits::default()
        };
        let empty = read_fbx_binary_with_limits(&[], empty_limits).unwrap();
        assert!(parse_fbx_binary(&empty).unwrap().is_empty());

        rejects(
            &[FbxNode::new("leaf")],
            FbxBinaryWriteLimits {
                maximum_depth: 0,
                ..FbxBinaryWriteLimits::default()
            },
            "nesting exceeds 0",
        );
        rejects(
            &[FbxNode::new("one"), FbxNode::new("two")],
            FbxBinaryWriteLimits {
                maximum_nodes: 1,
                ..FbxBinaryWriteLimits::default()
            },
            "exceeds 1 nodes",
        );
        rejects(
            &[FbxNode::new("property").with(FbxProperty::I32(1))],
            FbxBinaryWriteLimits {
                maximum_properties: 0,
                ..FbxBinaryWriteLimits::default()
            },
            "exceeds 0 properties",
        );
        rejects(
            &[FbxNode::new("array").with(FbxProperty::BoolArray(vec![true; 3]))],
            FbxBinaryWriteLimits {
                maximum_array_elements: 2,
                ..FbxBinaryWriteLimits::default()
            },
            "array has 3 elements",
        );
        rejects(
            &[FbxNode::new("root").child(FbxNode::new("child"))],
            FbxBinaryWriteLimits {
                maximum_depth: 1,
                ..FbxBinaryWriteLimits::default()
            },
            "nesting exceeds 1",
        );
    }

    #[test]
    fn rejects_array_bombs_before_allocating_their_declared_shape() {
        let node = FbxNode::new("Bomb").with(FbxProperty::F64Array(vec![0.0; 256]));
        let mut output = Vec::new();
        write_fbx_binary(std::slice::from_ref(&node), &mut output).unwrap();

        let property = MAGIC.len() + 4 + RECORD_HEADER_BYTES + node.name.len();
        assert_eq!(output[property], b'd');
        output[property + 1..property + 5].copy_from_slice(&u32::MAX.to_le_bytes());

        let limits = FbxBinaryParseLimits {
            maximum_array_elements: 1024,
            ..FbxBinaryParseLimits::default()
        };
        let error = parse_fbx_binary_with_limits(&output, limits).unwrap_err();
        assert!(error.to_string().contains("array has 4294967295 elements"));
    }

    #[test]
    fn rejects_compressed_arrays_that_expand_past_their_declared_count() {
        let node = FbxNode::new("Long").with(FbxProperty::F64Array(vec![1.0; 256]));
        let mut output = Vec::new();
        write_fbx_binary(std::slice::from_ref(&node), &mut output).unwrap();

        let property = MAGIC.len() + 4 + RECORD_HEADER_BYTES + node.name.len();
        assert_eq!(output[property], b'd');
        output[property + 1..property + 5].copy_from_slice(&1_u32.to_le_bytes());

        let error = parse_fbx_binary(&output).unwrap_err();
        assert!(error.to_string().contains("expands beyond"));
    }

    #[test]
    fn rejects_bytes_after_a_compressed_array_stream() {
        use std::io::Write as _;

        let raw = [false, true, false, true];
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        for value in raw {
            encoder.write_all(&[u8::from(value)]).unwrap();
        }
        let mut payload = encoder.finish().unwrap();
        payload.extend_from_slice(b"trailing");

        let error = super::inflate_array(
            &payload,
            raw.len(),
            &mut super::ParseBudget::new(FbxBinaryParseLimits::default()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("after its zlib stream"));
    }

    #[test]
    fn rejects_child_lists_terminated_before_the_parent_boundary() {
        let node = FbxNode::new("Parent").child(FbxNode::new("Child"));
        let mut output = Vec::new();
        write_fbx_binary(std::slice::from_ref(&node), &mut output).unwrap();

        let child = MAGIC.len() + 4 + RECORD_HEADER_BYTES + node.name.len();
        output[child..child + RECORD_HEADER_BYTES].fill(0);
        let error = parse_fbx_binary(&output).unwrap_err();
        assert!(error.to_string().contains("parent boundary"));
    }

    #[test]
    fn depth_limit_counts_records_without_charging_list_terminators() {
        let mut empty = Vec::new();
        write_fbx_binary(&[], &mut empty).unwrap();
        let empty_limits = FbxBinaryParseLimits {
            maximum_nodes: 0,
            maximum_properties: 0,
            maximum_depth: 0,
            maximum_array_elements: 0,
            maximum_total_allocation_bytes: 0,
            ..FbxBinaryParseLimits::default()
        };
        assert!(
            parse_fbx_binary_with_limits(&empty, empty_limits)
                .unwrap()
                .is_empty()
        );

        let leaf = FbxNode::new("leaf");
        let mut one_root = Vec::new();
        write_fbx_binary(std::slice::from_ref(&leaf), &mut one_root).unwrap();
        assert_eq!(
            parse_fbx_binary_with_limits(
                &one_root,
                FbxBinaryParseLimits {
                    maximum_depth: 1,
                    ..FbxBinaryParseLimits::default()
                }
            )
            .unwrap(),
            vec![leaf]
        );
        let error = parse_fbx_binary_with_limits(
            &one_root,
            FbxBinaryParseLimits {
                maximum_depth: 0,
                ..FbxBinaryParseLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("nesting exceeds 0"));
    }

    #[test]
    fn applies_tree_and_cumulative_allocation_limits() {
        let nested = FbxNode::new("root")
            .child(FbxNode::new("one").child(FbxNode::new("two").child(FbxNode::new("three"))));
        let mut output = Vec::new();
        write_fbx_binary(std::slice::from_ref(&nested), &mut output).unwrap();

        let depth_error = parse_fbx_binary_with_limits(
            &output,
            FbxBinaryParseLimits {
                maximum_depth: 3,
                ..FbxBinaryParseLimits::default()
            },
        )
        .unwrap_err();
        assert!(depth_error.to_string().contains("nesting exceeds 3"));

        let node_error = parse_fbx_binary_with_limits(
            &output,
            FbxBinaryParseLimits {
                maximum_nodes: 2,
                ..FbxBinaryParseLimits::default()
            },
        )
        .unwrap_err();
        assert!(node_error.to_string().contains("exceeds 2 nodes"));

        let allocation_error = parse_fbx_binary_with_limits(
            &output,
            FbxBinaryParseLimits {
                maximum_total_allocation_bytes: 1,
                ..FbxBinaryParseLimits::default()
            },
        )
        .unwrap_err();
        assert!(allocation_error.to_string().contains("allocation budget"));
    }
}
