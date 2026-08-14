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

use std::io::Write;

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
    let mut body = Vec::new();
    let mut position = MAGIC.len() + 4;
    for root in roots {
        encode_node(root, &mut position, &mut body)?;
    }
    // The top-level list is terminated the same way a nested one is.
    body.extend_from_slice(&[0_u8; NULL_RECORD_BYTES]);

    output.write_all(MAGIC)?;
    output.write_all(&VERSION.to_le_bytes())?;
    output.write_all(&body)?;
    let footer = footer_bytes(MAGIC.len() + 4 + body.len());
    output.write_all(&footer)?;

    u64::try_from(MAGIC.len() + 4 + body.len() + footer.len())
        .map_err(|_| Error::invalid_data("binary FBX length does not fit u64"))
}

/// Encodes one record and its subtree, advancing `position` past it.
///
/// A record's header stores the absolute offset of its end, so the length has
/// to be known before the header is written. The body is encoded first into a
/// scratch buffer and the header prepended, which costs one copy per record and
/// keeps the offsets exact.
fn encode_node(node: &FbxNode, position: &mut usize, output: &mut Vec<u8>) -> Result<()> {
    let name_length = u8::try_from(node.name.len())
        .map_err(|_| Error::invalid_data("binary FBX node names are limited to 255 bytes"))?;

    let mut properties = Vec::new();
    for property in &node.properties {
        encode_property(property, &mut properties)?;
    }

    let header_end = position
        .checked_add(RECORD_HEADER_BYTES)
        .and_then(|value| value.checked_add(node.name.len()))
        .and_then(|value| value.checked_add(properties.len()))
        .ok_or_else(|| Error::invalid_data("binary FBX record offset overflowed"))?;

    let mut children = Vec::new();
    if !node.children.is_empty() {
        let mut child_position = header_end;
        for child in &node.children {
            encode_node(child, &mut child_position, &mut children)?;
        }
        // A record with children ends with a null record; one without has none,
        // and a reader distinguishes the two by the end offset alone.
        children.extend_from_slice(&[0_u8; NULL_RECORD_BYTES]);
    }

    let end_offset = header_end
        .checked_add(children.len())
        .ok_or_else(|| Error::invalid_data("binary FBX record offset overflowed"))?;
    let end_offset = u32::try_from(end_offset)
        .map_err(|_| Error::invalid_data("binary FBX exceeds the 4 GiB the 7.4 format allows"))?;

    output.extend_from_slice(&end_offset.to_le_bytes());
    output.extend_from_slice(
        &u32::try_from(node.properties.len())
            .map_err(|_| Error::invalid_data("binary FBX property count overflowed"))?
            .to_le_bytes(),
    );
    output.extend_from_slice(
        &u32::try_from(properties.len())
            .map_err(|_| Error::invalid_data("binary FBX property block overflowed"))?
            .to_le_bytes(),
    );
    output.push(name_length);
    output.extend_from_slice(node.name.as_bytes());
    output.extend_from_slice(&properties);
    output.extend_from_slice(&children);

    *position = end_offset as usize;
    Ok(())
}

fn encode_property(property: &FbxProperty, output: &mut Vec<u8>) -> Result<()> {
    output.push(property.type_code());
    match property {
        FbxProperty::Bool(value) => output.push(u8::from(*value)),
        FbxProperty::I16(value) => output.extend_from_slice(&value.to_le_bytes()),
        FbxProperty::I32(value) => output.extend_from_slice(&value.to_le_bytes()),
        FbxProperty::I64(value) => output.extend_from_slice(&value.to_le_bytes()),
        FbxProperty::F32(value) => output.extend_from_slice(&value.to_le_bytes()),
        FbxProperty::F64(value) => output.extend_from_slice(&value.to_le_bytes()),
        FbxProperty::String(value) => {
            let length = u32::try_from(value.len())
                .map_err(|_| Error::invalid_data("binary FBX string length overflowed"))?;
            output.extend_from_slice(&length.to_le_bytes());
            output.extend_from_slice(value.as_bytes());
        }
        FbxProperty::Raw(value) => {
            let length = u32::try_from(value.len())
                .map_err(|_| Error::invalid_data("binary FBX raw length overflowed"))?;
            output.extend_from_slice(&length.to_le_bytes());
            output.extend_from_slice(value);
        }
        FbxProperty::I32Array(values) => {
            encode_array(values.len(), &flatten(values, i32::to_le_bytes), output)?;
        }
        FbxProperty::I64Array(values) => {
            encode_array(values.len(), &flatten(values, i64::to_le_bytes), output)?;
        }
        FbxProperty::F32Array(values) => {
            encode_array(values.len(), &flatten(values, f32::to_le_bytes), output)?;
        }
        FbxProperty::F64Array(values) => {
            encode_array(values.len(), &flatten(values, f64::to_le_bytes), output)?;
        }
    }
    Ok(())
}

/// Concatenates each element's little-endian bytes.
fn flatten<T: Copy, const N: usize>(values: &[T], encode: fn(T) -> [u8; N]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * N);
    for value in values {
        bytes.extend_from_slice(&encode(*value));
    }
    bytes
}

/// Writes an array property's length, encoding and payload.
///
/// Encoding zero is raw and encoding one is zlib-deflated. Anything large
/// enough to be worth compressing is deflated; below the threshold the twelve
/// byte header plus zlib framing would usually make the file bigger.
fn encode_array(count: usize, raw: &[u8], output: &mut Vec<u8>) -> Result<()> {
    let count = u32::try_from(count)
        .map_err(|_| Error::invalid_data("binary FBX array length overflowed"))?;
    if raw.len() < DEFLATE_THRESHOLD {
        output.extend_from_slice(&count.to_le_bytes());
        output.extend_from_slice(&0_u32.to_le_bytes());
        output.extend_from_slice(
            &u32::try_from(raw.len())
                .map_err(|_| Error::invalid_data("binary FBX array size overflowed"))?
                .to_le_bytes(),
        );
        output.extend_from_slice(raw);
        return Ok(());
    }

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(raw)
        .map_err(|error| Error::invalid_data(format!("cannot deflate an FBX array: {error}")))?;
    let deflated = encoder
        .finish()
        .map_err(|error| Error::invalid_data(format!("cannot deflate an FBX array: {error}")))?;
    output.extend_from_slice(&count.to_le_bytes());
    output.extend_from_slice(&1_u32.to_le_bytes());
    output.extend_from_slice(
        &u32::try_from(deflated.len())
            .map_err(|_| Error::invalid_data("binary FBX array size overflowed"))?
            .to_le_bytes(),
    );
    output.extend_from_slice(&deflated);
    Ok(())
}

/// The trailer every binary FBX ends with.
///
/// A 16-byte footer id, padding to a 16-byte boundary, the version repeated,
/// 120 zero bytes and a 16-byte magic. The padding is what makes the total
/// length a multiple of 16, which readers rely on to find the trailer.
fn footer_bytes(body_length: usize) -> Vec<u8> {
    const FOOTER_ID: [u8; 16] = [
        0xfa, 0xbc, 0xab, 0x09, 0xd0, 0xc8, 0xd4, 0x66, 0xb1, 0x76, 0xfb, 0x83, 0x1c, 0xf7, 0x26,
        0x7e,
    ];
    const FOOTER_MAGIC: [u8; 16] = [
        0xf8, 0x5a, 0x8c, 0x6a, 0xde, 0xf5, 0xd9, 0x7e, 0xec, 0xe9, 0x0c, 0xe3, 0x75, 0x8f, 0x29,
        0x0b,
    ];

    let mut footer = Vec::new();
    footer.extend_from_slice(&FOOTER_ID);
    let aligned = (body_length + FOOTER_ID.len()).next_multiple_of(16);
    footer.resize(
        FOOTER_ID.len() + (aligned - body_length - FOOTER_ID.len()),
        0,
    );
    // A file already on a boundary still gets a full 16 bytes of padding, which
    // is what the reference writers emit and what readers scan back over.
    if footer.len() == FOOTER_ID.len() {
        footer.resize(FOOTER_ID.len() + 16, 0);
    }
    footer.extend_from_slice(&0_u32.to_le_bytes());
    footer.extend_from_slice(&VERSION.to_le_bytes());
    footer.resize(footer.len() + 120, 0);
    footer.extend_from_slice(&FOOTER_MAGIC);
    footer
}

/// Materializes a binary FBX under an explicit output cap.
pub fn read_fbx_binary(roots: &[FbxNode], maximum_output_bytes: u64) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let written = write_fbx_binary(roots, &mut output)?;
    if written > maximum_output_bytes {
        return Err(Error::invalid_data(format!(
            "binary FBX is {written} bytes, exceeding limit {maximum_output_bytes}"
        )));
    }
    Ok(output)
}

/// Reads back a binary FBX node tree.
///
/// Present so a caller can verify what was written, and so the writer has a
/// decoder to be checked against that does not share its code.
pub fn parse_fbx_binary(bytes: &[u8]) -> Result<Vec<FbxNode>> {
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
    while let Some(node) = parse_node(bytes, &mut cursor)? {
        nodes.push(node);
    }
    Ok(nodes)
}

/// Parses one record, returning `None` at the null record that ends a list.
fn parse_node(bytes: &[u8], cursor: &mut usize) -> Result<Option<FbxNode>> {
    let header = bytes
        .get(*cursor..*cursor + RECORD_HEADER_BYTES)
        .ok_or_else(|| Error::invalid_data("binary FBX record header runs past the file"))?;
    let end_offset = u32::from_le_bytes(header[0..4].try_into().expect("four bytes")) as usize;
    let property_count = u32::from_le_bytes(header[4..8].try_into().expect("four bytes")) as usize;
    let property_bytes = u32::from_le_bytes(header[8..12].try_into().expect("four bytes")) as usize;
    let name_length = usize::from(header[12]);
    if end_offset == 0 && property_count == 0 && property_bytes == 0 && name_length == 0 {
        *cursor += RECORD_HEADER_BYTES;
        return Ok(None);
    }
    if end_offset <= *cursor || end_offset > bytes.len() {
        return Err(Error::invalid_data(
            "binary FBX record end offset is invalid",
        ));
    }

    let name_start = *cursor + RECORD_HEADER_BYTES;
    let name = std::str::from_utf8(
        bytes
            .get(name_start..name_start + name_length)
            .ok_or_else(|| Error::invalid_data("binary FBX record name runs past the file"))?,
    )
    .map_err(|_| Error::invalid_data("binary FBX record name is not UTF-8"))?
    .to_owned();

    let mut property_cursor = name_start + name_length;
    let property_end = property_cursor + property_bytes;
    let mut properties = Vec::new();
    for _ in 0..property_count {
        properties.push(parse_property(bytes, &mut property_cursor)?);
    }
    if property_cursor != property_end {
        return Err(Error::invalid_data(
            "binary FBX property block length disagrees with its contents",
        ));
    }

    let mut children = Vec::new();
    let mut child_cursor = property_end;
    while child_cursor < end_offset {
        match parse_node(bytes, &mut child_cursor)? {
            Some(child) => children.push(child),
            None => break,
        }
    }

    *cursor = end_offset;
    Ok(Some(FbxNode {
        name,
        properties,
        children,
    }))
}

fn parse_property(bytes: &[u8], cursor: &mut usize) -> Result<FbxProperty> {
    let code = *bytes
        .get(*cursor)
        .ok_or_else(|| Error::invalid_data("binary FBX property runs past the file"))?;
    *cursor += 1;
    let mut scalar = |length: usize| -> Result<&[u8]> {
        let slice = bytes
            .get(*cursor..*cursor + length)
            .ok_or_else(|| Error::invalid_data("binary FBX property runs past the file"))?;
        *cursor += length;
        Ok(slice)
    };
    match code {
        b'C' => Ok(FbxProperty::Bool(scalar(1)?[0] != 0)),
        b'Y' => Ok(FbxProperty::I16(i16::from_le_bytes(
            scalar(2)?.try_into().expect("two bytes"),
        ))),
        b'I' => Ok(FbxProperty::I32(i32::from_le_bytes(
            scalar(4)?.try_into().expect("four bytes"),
        ))),
        b'L' => Ok(FbxProperty::I64(i64::from_le_bytes(
            scalar(8)?.try_into().expect("eight bytes"),
        ))),
        b'F' => Ok(FbxProperty::F32(f32::from_le_bytes(
            scalar(4)?.try_into().expect("four bytes"),
        ))),
        b'D' => Ok(FbxProperty::F64(f64::from_le_bytes(
            scalar(8)?.try_into().expect("eight bytes"),
        ))),
        b'S' | b'R' => {
            let length = u32::from_le_bytes(scalar(4)?.try_into().expect("four bytes")) as usize;
            let value = scalar(length)?.to_vec();
            if code == b'S' {
                Ok(FbxProperty::String(String::from_utf8(value).map_err(
                    |_| Error::invalid_data("binary FBX string is not UTF-8"),
                )?))
            } else {
                Ok(FbxProperty::Raw(value))
            }
        }
        b'i' | b'l' | b'f' | b'd' => {
            let count = u32::from_le_bytes(scalar(4)?.try_into().expect("four bytes")) as usize;
            let encoding = u32::from_le_bytes(scalar(4)?.try_into().expect("four bytes"));
            let stored = u32::from_le_bytes(scalar(4)?.try_into().expect("four bytes")) as usize;
            let payload = scalar(stored)?.to_vec();
            let raw = match encoding {
                0 => payload,
                1 => {
                    use std::io::Read;
                    let mut decoder = flate2::read::ZlibDecoder::new(payload.as_slice());
                    let mut inflated = Vec::new();
                    decoder.read_to_end(&mut inflated).map_err(|error| {
                        Error::invalid_data(format!("cannot inflate an FBX array: {error}"))
                    })?;
                    inflated
                }
                other => {
                    return Err(Error::unsupported(format!(
                        "binary FBX array encoding {other}"
                    )));
                }
            };
            decode_array(code, count, &raw)
        }
        other => Err(Error::unsupported(format!(
            "binary FBX property type {:?}",
            char::from(other)
        ))),
    }
}

fn decode_array(code: u8, count: usize, raw: &[u8]) -> Result<FbxProperty> {
    fn chunks<const N: usize>(raw: &[u8], count: usize) -> Result<Vec<[u8; N]>> {
        if raw.len() != count * N {
            return Err(Error::invalid_data(
                "binary FBX array length disagrees with its payload",
            ));
        }
        Ok(raw
            .chunks_exact(N)
            .map(|chunk| chunk.try_into().expect("an exact chunk"))
            .collect())
    }
    match code {
        b'i' => Ok(FbxProperty::I32Array(
            chunks::<4>(raw, count)?
                .into_iter()
                .map(i32::from_le_bytes)
                .collect(),
        )),
        b'l' => Ok(FbxProperty::I64Array(
            chunks::<8>(raw, count)?
                .into_iter()
                .map(i64::from_le_bytes)
                .collect(),
        )),
        b'f' => Ok(FbxProperty::F32Array(
            chunks::<4>(raw, count)?
                .into_iter()
                .map(f32::from_le_bytes)
                .collect(),
        )),
        _ => Ok(FbxProperty::F64Array(
            chunks::<8>(raw, count)?
                .into_iter()
                .map(f64::from_le_bytes)
                .collect(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FbxNode, FbxProperty, MAGIC, VERSION, parse_fbx_binary, read_fbx_binary, write_fbx_binary,
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
        let error = read_fbx_binary(&sample(), 8).unwrap_err();
        assert!(error.to_string().contains("exceeding limit"));
    }
}
