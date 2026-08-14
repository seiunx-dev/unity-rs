//! Managed-compatible textual dumps for materialized Unity type trees.

use std::io::{self, Write};

use crate::serialized::{TypeTree, TypeTreeNode};
use crate::type_tree::{TypeField, TypeMapEntry, TypeValue};
use crate::{Error, Result};

/// Writes the legacy `TypeTreeHelper.ReadTypeString` representation.
///
/// Field order, type labels, tab indentation, array/map markers, and CRLF line
/// endings are preserved. The value must have been decoded from `tree`.
pub fn write_type_tree_dump(
    tree: &TypeTree,
    value: &TypeValue,
    output: &mut impl Write,
    maximum_output_bytes: u64,
) -> Result<u64> {
    if tree.nodes.is_empty() {
        return Err(Error::unsupported("cannot dump an empty TypeTree"));
    }
    let mut output = BoundedDumpWriter::new(output, maximum_output_bytes);
    let result = DumpFormatter {
        nodes: &tree.nodes,
        output: &mut output,
    }
    .write_node(0, value);
    if output.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "TypeTree dump exceeds the {maximum_output_bytes} byte output limit"
        )));
    }
    let next = result?;
    if next != tree.nodes.len() {
        return Err(Error::invalid_data(format!(
            "TypeTree dump root covers {next} of {} nodes",
            tree.nodes.len()
        )));
    }
    Ok(output.written)
}

struct DumpFormatter<'a, W> {
    nodes: &'a [TypeTreeNode],
    output: &'a mut W,
}

impl<W: Write> DumpFormatter<'_, W> {
    fn write_node(&mut self, index: usize, value: &TypeValue) -> Result<usize> {
        let node = self.nodes.get(index).ok_or_else(|| {
            Error::invalid_data(format!("TypeTree dump node index {index} is out of range"))
        })?;
        match value {
            TypeValue::String(value) => {
                require_type(node, "string", "string")?;
                array_shape(self.nodes, index)?;
                self.write_prefix(node.level)?;
                write!(
                    self.output,
                    "{} {} = \"{}\"\r\n",
                    node.type_name, node.field_name, value
                )?;
                Ok(subtree_end(self.nodes, index))
            }
            TypeValue::TypelessData { size, .. } => {
                require_type(node, "TypelessData", "TypelessData")?;
                typeless_shape(self.nodes, index)?;
                self.write_container(node)?;
                self.write_prefix(node.level)?;
                writeln_crlf(self.output, format_args!("int size = {size}"))?;
                Ok(subtree_end(self.nodes, index))
            }
            TypeValue::Array(values) => self.write_array(index, node, values),
            TypeValue::Map(entries) => {
                require_type(node, "map", "map")?;
                self.write_map(index, node, entries)
            }
            TypeValue::Object(fields) => {
                if is_primitive_type(&node.type_name)
                    || matches!(node.type_name.as_str(), "string" | "map" | "TypelessData")
                {
                    return Err(value_mismatch(node, "object"));
                }
                self.write_object(index, node, fields)
            }
            primitive => {
                validate_primitive(node, primitive)?;
                if subtree_end(self.nodes, index) != index + 1 {
                    return Err(Error::invalid_data(format!(
                        "TypeTree primitive {} {} has child nodes",
                        node.type_name, node.field_name
                    )));
                }
                self.write_prefix(node.level)?;
                write!(self.output, "{} {} = ", node.type_name, node.field_name)?;
                self.write_primitive(node, primitive)?;
                self.output.write_all(b"\r\n")?;
                Ok(index + 1)
            }
        }
    }

    fn write_object(
        &mut self,
        index: usize,
        node: &TypeTreeNode,
        fields: &[TypeField],
    ) -> Result<usize> {
        self.write_container(node)?;
        let end = subtree_end(self.nodes, index);
        let expected_level = node
            .level
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("TypeTree level overflowed"))?;
        let mut child_index = index + 1;
        for field in fields {
            if child_index >= end {
                return Err(Error::invalid_data(format!(
                    "TypeTree dump object {} has more values than tree fields",
                    node.field_name
                )));
            }
            let child = &self.nodes[child_index];
            if child.level != expected_level {
                return Err(Error::invalid_data(format!(
                    "TypeTree child at index {child_index} has level {}, expected {expected_level}",
                    child.level
                )));
            }
            if child.field_name != field.name {
                return Err(Error::invalid_data(format!(
                    "TypeTree dump field {:?} does not match tree field {:?}",
                    field.name, child.field_name
                )));
            }
            self.write_node(child_index, &field.value)?;
            child_index = subtree_end(self.nodes, child_index);
        }
        if child_index != end {
            return Err(Error::invalid_data(format!(
                "TypeTree dump object {} has fewer values than tree fields",
                node.field_name
            )));
        }
        Ok(end)
    }

    fn write_array(
        &mut self,
        index: usize,
        node: &TypeTreeNode,
        values: &[TypeValue],
    ) -> Result<usize> {
        let data_index = array_shape(self.nodes, index)?;
        let array_level = checked_level(node.level, 1)?;
        let element_level = checked_level(node.level, 2)?;
        self.write_container(node)?;
        self.write_prefix(array_level)?;
        self.output.write_all(b"Array Array\r\n")?;
        self.write_prefix(array_level)?;
        writeln_crlf(self.output, format_args!("int size = {}", values.len()))?;
        for (element_index, value) in values.iter().enumerate() {
            self.write_prefix(element_level)?;
            writeln_crlf(self.output, format_args!("[{element_index}]"))?;
            self.write_node(data_index, value)?;
        }
        Ok(subtree_end(self.nodes, index))
    }

    fn write_map(
        &mut self,
        index: usize,
        node: &TypeTreeNode,
        entries: &[TypeMapEntry],
    ) -> Result<usize> {
        let (first_index, second_index) = map_shape(self.nodes, index)?;
        let array_level = checked_level(node.level, 1)?;
        let element_level = checked_level(node.level, 2)?;
        self.write_container(node)?;
        self.write_prefix(array_level)?;
        self.output.write_all(b"Array Array\r\n")?;
        self.write_prefix(array_level)?;
        writeln_crlf(self.output, format_args!("int size = {}", entries.len()))?;
        for (entry_index, entry) in entries.iter().enumerate() {
            self.write_prefix(element_level)?;
            writeln_crlf(self.output, format_args!("[{entry_index}]"))?;
            self.write_prefix(element_level)?;
            self.output.write_all(b"pair data\r\n")?;
            self.write_node(first_index, &entry.key)?;
            self.write_node(second_index, &entry.value)?;
        }
        Ok(subtree_end(self.nodes, index))
    }

    fn write_container(&mut self, node: &TypeTreeNode) -> io::Result<()> {
        self.write_prefix(node.level)?;
        write!(self.output, "{} {}\r\n", node.type_name, node.field_name)
    }

    fn write_prefix(&mut self, level: u32) -> io::Result<()> {
        for _ in 0..level {
            self.output.write_all(b"\t")?;
        }
        Ok(())
    }

    fn write_primitive(&mut self, node: &TypeTreeNode, value: &TypeValue) -> Result<()> {
        match value {
            TypeValue::Signed(value) => write!(self.output, "{value}")?,
            TypeValue::Unsigned(value) => write!(self.output, "{value}")?,
            TypeValue::Character(value) => {
                let character = char::decode_utf16([*value])
                    .next()
                    .expect("one UTF-16 unit produces one item")
                    .unwrap_or(char::REPLACEMENT_CHARACTER);
                write!(self.output, "{character}")?;
            }
            TypeValue::Float32(value) => write_managed_single(self.output, *value)?,
            TypeValue::Float(value) => write_managed_double(self.output, *value)?,
            TypeValue::Boolean(value) => {
                self.output
                    .write_all(if *value { b"True" } else { b"False" })?;
            }
            _ => {
                return Err(Error::invalid_data(format!(
                    "TypeTree dump value does not match primitive {} {}",
                    node.type_name, node.field_name
                )));
            }
        }
        Ok(())
    }
}

/// Round-trip digit count .NET uses as the general-format precision specifier.
const SINGLE_ROUND_TRIP_DIGITS: i32 = 9;
const DOUBLE_ROUND_TRIP_DIGITS: i32 = 17;

fn write_managed_single(output: &mut impl Write, value: f32) -> Result<()> {
    if write_non_finite(
        output,
        value.is_nan(),
        value.is_sign_negative(),
        value.is_finite(),
    )? {
        return Ok(());
    }
    write_general_format(output, &format!("{value:e}"), SINGLE_ROUND_TRIP_DIGITS)
}

fn write_managed_double(output: &mut impl Write, value: f64) -> Result<()> {
    if write_non_finite(
        output,
        value.is_nan(),
        value.is_sign_negative(),
        value.is_finite(),
    )? {
        return Ok(());
    }
    write_general_format(output, &format!("{value:e}"), DOUBLE_ROUND_TRIP_DIGITS)
}

/// Writes the invariant text for a non-finite value, reporting whether it did.
fn write_non_finite(
    output: &mut impl Write,
    is_nan: bool,
    is_negative: bool,
    is_finite: bool,
) -> Result<bool> {
    if is_nan {
        output.write_all(b"NaN")?;
    } else if !is_finite {
        output.write_all(if is_negative {
            b"-Infinity".as_slice()
        } else {
            b"Infinity".as_slice()
        })?;
    } else {
        return Ok(false);
    }
    Ok(true)
}

/// Renders one finite value the way .NET's default `ToString()` does.
///
/// The managed dump boxes each value and hands it to `AppendFormat`, so the
/// text follows .NET's general format at round-trip precision: fixed notation
/// while the decimal exponent satisfies `-5 < exponent < precision`, and
/// scientific notation with a two-digit-minimum exponent otherwise. Rust's own
/// `Display` never switches to an exponent, so `float.MaxValue` would come out
/// as a 39-digit integer where the managed reader writes `3.4028235E+38`.
///
/// `exponential` is the value already rendered by Rust's `LowerExp`, which
/// supplies exactly the shortest round-trip digits and decimal exponent this
/// needs.
///
/// The managed formatter uses `CurrentCulture`, so this targets
/// `InvariantCulture` deliberately: it is what the managed exporter runs under
/// (`InvariantGlobalization` is set in the shipped projects) and what a CI
/// runner without a locale falls back to. Under a culture such as de-DE the
/// managed text would use a decimal comma and a localized infinity symbol; that
/// is not reproduced, and the dump is not culture-sensitive.
fn write_general_format(output: &mut impl Write, exponential: &str, precision: i32) -> Result<()> {
    let malformed = || Error::invalid_data("float exponential form is malformed");
    let (mantissa, exponent) = exponential.split_once('e').ok_or_else(malformed)?;
    let exponent: i32 = exponent.parse().map_err(|_| malformed())?;
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return Err(malformed());
    }
    if digits.bytes().all(|digit| digit == b'0') {
        // Rust renders zero as `0e0`; .NET keeps the sign of negative zero.
        write!(output, "{sign}0")?;
        return Ok(());
    }

    if exponent > -5 && exponent < precision {
        write_fixed(output, sign, &digits, exponent)?;
    } else {
        write!(output, "{sign}{}", &digits[..1])?;
        if digits.len() > 1 {
            write!(output, ".{}", &digits[1..])?;
        }
        let sign = if exponent < 0 { '-' } else { '+' };
        write!(output, "E{sign}{:02}", exponent.unsigned_abs())?;
    }
    Ok(())
}

fn write_fixed(output: &mut impl Write, sign: &str, digits: &str, exponent: i32) -> Result<()> {
    output.write_all(sign.as_bytes())?;
    if exponent < 0 {
        // -4 through -1: a leading zero, then the gap before the first digit.
        let leading_zeros = usize::try_from(-exponent - 1)
            .map_err(|_| Error::invalid_data("float exponent does not fit this platform"))?;
        write!(output, "0.")?;
        for _ in 0..leading_zeros {
            output.write_all(b"0")?;
        }
        output.write_all(digits.as_bytes())?;
        return Ok(());
    }
    let integer_digits = usize::try_from(exponent + 1)
        .map_err(|_| Error::invalid_data("float exponent does not fit this platform"))?;
    if digits.len() <= integer_digits {
        output.write_all(digits.as_bytes())?;
        for _ in 0..integer_digits - digits.len() {
            output.write_all(b"0")?;
        }
    } else {
        output.write_all(&digits.as_bytes()[..integer_digits])?;
        write!(output, ".{}", &digits[integer_digits..])?;
    }
    Ok(())
}

#[allow(clippy::cast_possible_truncation)]
fn array_shape(nodes: &[TypeTreeNode], index: usize) -> Result<usize> {
    let node = &nodes[index];
    let array_level = checked_level(node.level, 1)?;
    let array_index = index + 1;
    let array = nodes
        .get(array_index)
        .ok_or_else(|| Error::invalid_data("TypeTree array has no Array child"))?;
    if array.level != array_level || array.type_name != "Array" {
        return Err(Error::invalid_data("TypeTree array child is not Array"));
    }
    if subtree_end(nodes, array_index) != subtree_end(nodes, index) {
        return Err(Error::invalid_data("TypeTree array has sibling fields"));
    }
    let size_index = array_index + 1;
    let size = nodes
        .get(size_index)
        .ok_or_else(|| Error::invalid_data("TypeTree Array has no size child"))?;
    let data_index = subtree_end(nodes, size_index);
    let data = nodes
        .get(data_index)
        .ok_or_else(|| Error::invalid_data("TypeTree Array has no data child"))?;
    let child_level = checked_level(array.level, 1)?;
    if size.level != child_level
        || data.level != child_level
        || size.field_name != "size"
        || data.field_name != "data"
        || subtree_end(nodes, data_index) != subtree_end(nodes, array_index)
    {
        return Err(Error::invalid_data(
            "TypeTree Array children are not size and data",
        ));
    }
    Ok(data_index)
}

fn map_shape(nodes: &[TypeTreeNode], index: usize) -> Result<(usize, usize)> {
    let node = &nodes[index];
    let array_level = checked_level(node.level, 1)?;
    let array_index = index + 1;
    let array = nodes
        .get(array_index)
        .ok_or_else(|| Error::invalid_data("TypeTree map has no Array child"))?;
    if array.level != array_level || array.type_name != "Array" {
        return Err(Error::invalid_data("TypeTree map child is not Array"));
    }
    let size_index = array_index + 1;
    let size = nodes
        .get(size_index)
        .ok_or_else(|| Error::invalid_data("TypeTree map has no size child"))?;
    let pair_index = subtree_end(nodes, size_index);
    let pair = nodes
        .get(pair_index)
        .ok_or_else(|| Error::invalid_data("TypeTree map has no pair child"))?;
    let array_child_level = checked_level(array.level, 1)?;
    if size.level != array_child_level
        || pair.level != array_child_level
        || size.field_name != "size"
        || subtree_end(nodes, pair_index) != subtree_end(nodes, array_index)
        || subtree_end(nodes, array_index) != subtree_end(nodes, index)
    {
        return Err(Error::invalid_data(
            "TypeTree map Array does not have size and pair children",
        ));
    }
    let first_index = pair_index + 1;
    let first = nodes
        .get(first_index)
        .ok_or_else(|| Error::invalid_data("TypeTree map pair has no first child"))?;
    let second_index = subtree_end(nodes, first_index);
    let second = nodes
        .get(second_index)
        .ok_or_else(|| Error::invalid_data("TypeTree map pair has no second child"))?;
    let pair_child_level = checked_level(pair.level, 1)?;
    if first.level != pair_child_level
        || second.level != pair_child_level
        || subtree_end(nodes, second_index) != subtree_end(nodes, pair_index)
    {
        return Err(Error::invalid_data(
            "TypeTree map pair does not have first and second children",
        ));
    }
    Ok((first_index, second_index))
}

fn typeless_shape(nodes: &[TypeTreeNode], index: usize) -> Result<()> {
    let node = &nodes[index];
    let child_level = checked_level(node.level, 1)?;
    let size_index = index
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("TypeTree TypelessData size index overflowed"))?;
    let data_index = index
        .checked_add(2)
        .ok_or_else(|| Error::invalid_data("TypeTree TypelessData data index overflowed"))?;
    let size = nodes
        .get(size_index)
        .ok_or_else(|| Error::invalid_data("TypeTree TypelessData has no size child"))?;
    let data = nodes
        .get(data_index)
        .ok_or_else(|| Error::invalid_data("TypeTree TypelessData has no data child"))?;
    if size.level != child_level
        || !matches!(size.type_name.as_str(), "int" | "SInt32")
        || size.field_name != "size"
        || data.level != child_level
        || !matches!(data.type_name.as_str(), "UInt8" | "SInt8" | "char")
        || data.field_name != "data"
        || subtree_end(nodes, data_index) != subtree_end(nodes, index)
    {
        return Err(Error::invalid_data(
            "TypeTree TypelessData children are not size and byte data",
        ));
    }
    Ok(())
}

fn require_type(node: &TypeTreeNode, expected: &str, value_kind: &str) -> Result<()> {
    if node.type_name == expected {
        Ok(())
    } else {
        Err(value_mismatch(node, value_kind))
    }
}

fn validate_primitive(node: &TypeTreeNode, value: &TypeValue) -> Result<()> {
    let matches = match value {
        TypeValue::Signed(_) => matches!(
            node.type_name.as_str(),
            "SInt8" | "short" | "SInt16" | "int" | "SInt32" | "long long" | "SInt64"
        ),
        TypeValue::Unsigned(_) => matches!(
            node.type_name.as_str(),
            "UInt8"
                | "UInt16"
                | "unsigned short"
                | "UInt32"
                | "unsigned int"
                | "Type*"
                | "UInt64"
                | "unsigned long long"
                | "FileSize"
        ),
        TypeValue::Character(_) => node.type_name == "char",
        TypeValue::Float32(_) => node.type_name == "float",
        TypeValue::Float(_) => node.type_name == "double",
        TypeValue::Boolean(_) => node.type_name == "bool",
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(value_mismatch(node, "primitive"))
    }
}

fn is_primitive_type(value: &str) -> bool {
    matches!(
        value,
        "SInt8"
            | "UInt8"
            | "char"
            | "short"
            | "SInt16"
            | "UInt16"
            | "unsigned short"
            | "int"
            | "SInt32"
            | "UInt32"
            | "unsigned int"
            | "Type*"
            | "long long"
            | "SInt64"
            | "UInt64"
            | "unsigned long long"
            | "FileSize"
            | "float"
            | "double"
            | "bool"
    )
}

fn value_mismatch(node: &TypeTreeNode, value_kind: &str) -> Error {
    Error::invalid_data(format!(
        "TypeTree dump {value_kind} value does not match {} {}",
        node.type_name, node.field_name
    ))
}

fn checked_level(level: u32, additional: u32) -> Result<u32> {
    level
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data("TypeTree dump level overflowed"))
}

fn subtree_end(nodes: &[TypeTreeNode], index: usize) -> usize {
    let level = nodes[index].level;
    nodes[index + 1..]
        .iter()
        .position(|node| node.level <= level)
        .map_or(nodes.len(), |offset| index + 1 + offset)
}

fn writeln_crlf(output: &mut impl Write, arguments: std::fmt::Arguments<'_>) -> io::Result<()> {
    output.write_fmt(arguments)?;
    output.write_all(b"\r\n")
}

struct BoundedDumpWriter<'a, W> {
    output: &'a mut W,
    maximum: u64,
    written: u64,
    limit_exceeded: bool,
}

impl<'a, W> BoundedDumpWriter<'a, W> {
    const fn new(output: &'a mut W, maximum: u64) -> Self {
        Self {
            output,
            maximum,
            written: 0,
            limit_exceeded: false,
        }
    }
}

impl<W: Write> Write for BoundedDumpWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(buffer.len())
            .map_err(|_| io::Error::other("TypeTree dump write length does not fit u64"))?;
        let end = self
            .written
            .checked_add(length)
            .ok_or_else(|| io::Error::other("TypeTree dump output length overflowed"))?;
        if end > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::other("TypeTree dump output limit exceeded"));
        }
        self.output.write_all(buffer)?;
        self.written = end;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DOUBLE_ROUND_TRIP_DIGITS, SINGLE_ROUND_TRIP_DIGITS, write_general_format,
        write_type_tree_dump,
    };

    /// Every value formatted by the managed reader on .NET 10. See the file
    /// header for how it was produced.
    const MANAGED_FLOAT_FORMAT: &str = include_str!("../tests/fixtures/managed/float_format.txt");

    #[test]
    fn float_text_matches_the_managed_formatter_for_every_recorded_value() {
        let mut checked = 0_usize;
        for line in MANAGED_FLOAT_FORMAT.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let mut columns = line.split('\t');
            let kind = columns.next().expect("value kind");
            let bits = columns.next().expect("bit pattern");
            let expected = columns.next().expect("expected text");
            assert!(columns.next().is_none(), "unexpected column in {line:?}");

            let mut actual = Vec::new();
            let (exponential, precision) = match kind {
                "f" => (
                    format!("{:e}", f32::from_bits(bits.parse().unwrap())),
                    SINGLE_ROUND_TRIP_DIGITS,
                ),
                "d" => (
                    format!("{:e}", f64::from_bits(bits.parse().unwrap())),
                    DOUBLE_ROUND_TRIP_DIGITS,
                ),
                other => panic!("unknown value kind {other:?}"),
            };
            write_general_format(&mut actual, &exponential, precision).unwrap();
            assert_eq!(
                std::str::from_utf8(&actual).unwrap(),
                expected,
                "{kind} bits {bits}"
            );
            checked += 1;
        }
        assert!(checked > 500, "only {checked} values were compared");
    }
    use crate::serialized::{TypeTree, TypeTreeNode};
    use crate::type_tree::{TypeField, TypeMapEntry, TypeValue};

    #[test]
    fn writes_classes_strings_arrays_maps_typeless_and_managed_scalars() {
        let tree = TypeTree {
            nodes: vec![
                node("Root", "Base", 0),
                node("string", "title", 1),
                node("Array", "Array", 2),
                node("int", "size", 3),
                node("char", "data", 3),
                node("vector", "values", 1),
                node("Array", "Array", 2),
                node("int", "size", 3),
                node("SInt32", "data", 3),
                node("map", "lookup", 1),
                node("Array", "Array", 2),
                node("int", "size", 3),
                node("pair", "data", 3),
                node("string", "first", 4),
                node("Array", "Array", 5),
                node("int", "size", 6),
                node("char", "data", 6),
                node("bool", "second", 4),
                node("TypelessData", "blob", 1),
                node("int", "size", 2),
                node("UInt8", "data", 2),
                node("float", "positive", 1),
                node("double", "infinite", 1),
            ],
            string_buffer: Vec::new(),
        };
        let value = TypeValue::Object(vec![
            field("title", TypeValue::String("A\"B".to_owned())),
            field(
                "values",
                TypeValue::Array(vec![TypeValue::Signed(-2), TypeValue::Signed(7)]),
            ),
            field(
                "lookup",
                TypeValue::Map(vec![TypeMapEntry {
                    key: TypeValue::String("flag".to_owned()),
                    value: TypeValue::Boolean(true),
                }]),
            ),
            field("blob", TypeValue::TypelessData { offset: 4, size: 3 }),
            field("positive", TypeValue::Float32(1.25)),
            field("infinite", TypeValue::Float(f64::INFINITY)),
        ]);
        let mut output = Vec::new();
        let written = write_type_tree_dump(&tree, &value, &mut output, 4096).unwrap();
        assert_eq!(written, u64::try_from(output.len()).unwrap());
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Root Base\r\n\
\tstring title = \"A\"B\"\r\n\
\tvector values\r\n\
\t\tArray Array\r\n\
\t\tint size = 2\r\n\
\t\t\t[0]\r\n\
\t\t\tSInt32 data = -2\r\n\
\t\t\t[1]\r\n\
\t\t\tSInt32 data = 7\r\n\
\tmap lookup\r\n\
\t\tArray Array\r\n\
\t\tint size = 1\r\n\
\t\t\t[0]\r\n\
\t\t\tpair data\r\n\
\t\t\t\tstring first = \"flag\"\r\n\
\t\t\t\tbool second = True\r\n\
\tTypelessData blob\r\n\
\tint size = 3\r\n\
\tfloat positive = 1.25\r\n\
\tdouble infinite = Infinity\r\n"
        );
    }

    #[test]
    fn rejects_output_limits_and_mismatched_values() {
        let tree = TypeTree {
            nodes: vec![node("SInt32", "value", 0)],
            string_buffer: Vec::new(),
        };
        let mut output = Vec::new();
        let error = write_type_tree_dump(&tree, &TypeValue::Signed(7), &mut output, 2).unwrap_err();
        assert!(error.to_string().contains("output limit"));

        let error = write_type_tree_dump(
            &tree,
            &TypeValue::String("wrong".to_owned()),
            &mut Vec::new(),
            1024,
        )
        .unwrap_err();
        assert!(error.to_string().contains("root covers") || error.to_string().contains("match"));

        let malformed_string = TypeTree {
            nodes: vec![node("string", "value", 0)],
            string_buffer: Vec::new(),
        };
        let error = write_type_tree_dump(
            &malformed_string,
            &TypeValue::String("value".to_owned()),
            &mut Vec::new(),
            1024,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Array child"));

        let primitive_with_child = TypeTree {
            nodes: vec![node("SInt32", "value", 0), node("SInt32", "child", 1)],
            string_buffer: Vec::new(),
        };
        let error = write_type_tree_dump(
            &primitive_with_child,
            &TypeValue::Signed(7),
            &mut Vec::new(),
            1024,
        )
        .unwrap_err();
        assert!(error.to_string().contains("has child nodes"));
    }

    fn node(type_name: &str, field_name: &str, level: u32) -> TypeTreeNode {
        TypeTreeNode {
            type_name: type_name.to_owned(),
            field_name: field_name.to_owned(),
            byte_size: -1,
            index: 0,
            type_flags: 0,
            version: 1,
            meta_flags: 0,
            level,
            type_string_offset: None,
            name_string_offset: None,
            reference_type_hash: 0,
        }
    }

    fn field(name: &str, value: TypeValue) -> TypeField {
        TypeField {
            name: name.to_owned(),
            value,
        }
    }
}
