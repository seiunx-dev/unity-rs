use std::io::{Read, Seek};

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::{SerializedFile, TypeTree, TypeTreeNode};
use crate::{Error, Result};

const ALIGN_BYTES_FLAG: i32 = 0x4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeTreeReadLimits {
    pub maximum_depth: usize,
    pub maximum_values: usize,
    pub maximum_array_elements: usize,
    pub maximum_string_bytes: usize,
    pub maximum_typeless_bytes: usize,
    /// Conservative upper bound for heap work retained by the materialized
    /// value tree. This includes value/vector slots, cloned field names,
    /// decoded strings, and their fallible capacities.
    pub maximum_materialized_bytes: usize,
}

impl Default for TypeTreeReadLimits {
    fn default() -> Self {
        Self {
            maximum_depth: 128,
            maximum_values: 1_000_000,
            maximum_array_elements: 1_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_typeless_bytes: 256 * 1024 * 1024,
            maximum_materialized_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Lossless-enough dynamic representation of a value read from a Unity type
/// tree. Objects and maps use ordered vectors because Unity metadata order is
/// significant and duplicate field names must not be silently overwritten.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeValue {
    Signed(i64),
    Unsigned(u64),
    Character(u16),
    Float(f64),
    Boolean(bool),
    String(String),
    TypelessData { offset: u64, size: u64 },
    Array(Vec<Self>),
    Object(Vec<TypeField>),
    Map(Vec<TypeMapEntry>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeField {
    pub name: String,
    pub value: TypeValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeMapEntry {
    pub key: TypeValue,
    pub value: TypeValue,
}

impl SerializedFile {
    pub fn read_type_tree_value(&self, object_index: usize) -> Result<TypeValue> {
        self.read_type_tree_value_with_limits(object_index, TypeTreeReadLimits::default())
    }

    pub fn read_type_tree_value_with_limits(
        &self,
        object_index: usize,
        limits: TypeTreeReadLimits,
    ) -> Result<TypeValue> {
        let object = self.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        let type_index = object.serialized_type_index.ok_or_else(|| {
            Error::unsupported(format!(
                "serialized object {} has no matching type metadata",
                object.path_id
            ))
        })?;
        let tree = self
            .types
            .get(type_index)
            .and_then(|kind| kind.type_tree.as_ref())
            .ok_or_else(|| {
                Error::unsupported(format!(
                    "serialized object {} has no type tree",
                    object.path_id
                ))
            })?;
        let payload = self.object_region(object_index)?;
        let endian = if self.header.endianness == 0 {
            Endian::Little
        } else {
            Endian::Big
        };
        read_type_tree_from_reader(
            tree,
            EndianReader::new(payload.cursor(), endian),
            object.byte_start,
            limits,
        )
    }
}

pub fn read_type_tree_from_reader<R: Read + Seek>(
    tree: &TypeTree,
    reader: EndianReader<R>,
    absolute_start: u64,
    limits: TypeTreeReadLimits,
) -> Result<TypeValue> {
    validate_tree_shape(&tree.nodes)?;
    let mut parser = TypeTreeValueReader {
        nodes: &tree.nodes,
        reader,
        absolute_start,
        limits,
        values_read: 0,
        materialized_bytes: 0,
    };
    let (value, next) = parser.read_node(0, 0)?;
    if next != tree.nodes.len() {
        return Err(Error::invalid_data(format!(
            "type tree root covers {next} of {} nodes",
            tree.nodes.len()
        )));
    }
    let consumed = parser.reader.position()?;
    let length = parser.reader.len()?;
    if consumed != length {
        return Err(Error::invalid_data(format!(
            "type tree consumed {consumed} of {length} object bytes"
        )));
    }
    Ok(value)
}

struct TypeTreeValueReader<'a, R> {
    nodes: &'a [TypeTreeNode],
    reader: EndianReader<R>,
    absolute_start: u64,
    limits: TypeTreeReadLimits,
    values_read: usize,
    materialized_bytes: usize,
}

impl<R: Read + Seek> TypeTreeValueReader<'_, R> {
    fn read_node(&mut self, index: usize, depth: usize) -> Result<(TypeValue, usize)> {
        if depth > self.limits.maximum_depth {
            return Err(Error::invalid_data(format!(
                "object type tree exceeds depth limit {}",
                self.limits.maximum_depth
            )));
        }
        self.values_read = self
            .values_read
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("type tree value count overflowed"))?;
        if self.values_read > self.limits.maximum_values {
            return Err(Error::invalid_data(format!(
                "object type tree exceeds value limit {}",
                self.limits.maximum_values
            )));
        }
        self.charge_materialized(std::mem::size_of::<TypeValue>(), "type tree value storage")?;

        let node = self.nodes.get(index).ok_or_else(|| {
            Error::invalid_data(format!("type tree node index {index} is out of range"))
        })?;
        let kind = ValueKind::from_type_name(&node.type_name);
        let mut align = node.meta_flags & ALIGN_BYTES_FLAG != 0;
        let subtree_end = subtree_end(self.nodes, index);

        let (value, next) = match kind {
            ValueKind::Signed8 => (
                TypeValue::Signed(i64::from(self.reader.read_i8()?)),
                index + 1,
            ),
            ValueKind::Unsigned8 => (
                TypeValue::Unsigned(u64::from(self.reader.read_u8()?)),
                index + 1,
            ),
            ValueKind::Character => (TypeValue::Character(self.reader.read_u16()?), index + 1),
            ValueKind::Unsigned16 => (
                TypeValue::Unsigned(u64::from(self.reader.read_u16()?)),
                index + 1,
            ),
            ValueKind::Signed16 => (
                TypeValue::Signed(i64::from(self.reader.read_i16()?)),
                index + 1,
            ),
            ValueKind::Signed32 => (
                TypeValue::Signed(i64::from(self.reader.read_i32()?)),
                index + 1,
            ),
            ValueKind::Unsigned32 => (
                TypeValue::Unsigned(u64::from(self.reader.read_u32()?)),
                index + 1,
            ),
            ValueKind::Signed64 => (TypeValue::Signed(self.reader.read_i64()?), index + 1),
            ValueKind::Unsigned64 => (TypeValue::Unsigned(self.reader.read_u64()?), index + 1),
            ValueKind::Float32 => (
                TypeValue::Float(f64::from(self.reader.read_f32()?)),
                index + 1,
            ),
            ValueKind::Float64 => (TypeValue::Float(self.reader.read_f64()?), index + 1),
            ValueKind::Boolean => (TypeValue::Boolean(self.reader.read_bool()?), index + 1),
            ValueKind::String => {
                validate_array_shape(self.nodes, index)?;
                let value = self.read_aligned_string()?;
                (TypeValue::String(value), subtree_end)
            }
            ValueKind::Map => {
                let (value, array_align) = self.read_map(index, depth)?;
                align |= array_align;
                (value, subtree_end)
            }
            ValueKind::TypelessData => {
                validate_typeless_shape(self.nodes, index)?;
                let length =
                    self.read_length(self.limits.maximum_typeless_bytes, "TypelessData byte")?;
                let offset = self.reader.position()?;
                let size = u64::try_from(length).expect("TypelessData length fits in u64");
                let end = offset
                    .checked_add(size)
                    .ok_or_else(|| Error::invalid_data("TypelessData range overflowed"))?;
                if end > self.reader.len()? {
                    return Err(Error::invalid_data(
                        "TypelessData extends past the object payload",
                    ));
                }
                self.reader.set_position(end)?;
                (TypeValue::TypelessData { offset, size }, subtree_end)
            }
            ValueKind::Other if is_array_parent(self.nodes, index) => {
                let (value, array_align) = self.read_array(index, depth)?;
                align |= array_align;
                (value, subtree_end)
            }
            ValueKind::Other => (self.read_class(index, depth)?, subtree_end),
        };

        if align {
            self.align(4)?;
        }
        Ok((value, next))
    }

    fn read_class(&mut self, index: usize, depth: usize) -> Result<TypeValue> {
        let parent_level = self.nodes[index].level;
        let end = subtree_end(self.nodes, index);
        let mut child_count = 0_usize;
        let mut child = index + 1;
        while child < end {
            let node = &self.nodes[child];
            if node.level != parent_level + 1 {
                return Err(Error::invalid_data(format!(
                    "type tree class child at index {child} has level {}, expected {}",
                    node.level,
                    parent_level + 1
                )));
            }
            child_count = child_count
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("type tree field count overflowed"))?;
            child = subtree_end(self.nodes, child);
        }
        self.charge_capacity::<TypeField>(child_count, "type tree field storage")?;
        let mut fields = Vec::new();
        fields.try_reserve_exact(child_count).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate {child_count} type tree fields: {error}"
            ))
        })?;

        child = index + 1;
        while child < end {
            let node = &self.nodes[child];
            let name = self.clone_field_name(&node.field_name)?;
            let (value, next) = self.read_node(child, depth + 1)?;
            fields.push(TypeField { name, value });
            child = next;
        }
        Ok(TypeValue::Object(fields))
    }

    fn read_array(&mut self, index: usize, depth: usize) -> Result<(TypeValue, bool)> {
        let shape = validate_array_shape(self.nodes, index)?;
        let count = self.read_length(self.limits.maximum_array_elements, "array element")?;
        self.charge_capacity::<TypeValue>(count, "type tree array storage")?;
        let mut values = Vec::new();
        values.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!("cannot allocate {count} array values: {error}"))
        })?;
        for _ in 0..count {
            values.push(self.read_node(shape.data_index, depth + 1)?.0);
        }
        Ok((TypeValue::Array(values), shape.align))
    }

    fn read_map(&mut self, index: usize, depth: usize) -> Result<(TypeValue, bool)> {
        let shape = validate_map_shape(self.nodes, index)?;
        let count = self.read_length(self.limits.maximum_array_elements, "map entry")?;
        self.charge_capacity::<TypeMapEntry>(count, "type tree map storage")?;
        let mut entries = Vec::new();
        entries.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!("cannot allocate {count} map entries: {error}"))
        })?;
        for _ in 0..count {
            let key = self.read_node(shape.first_index, depth + 1)?.0;
            let value = self.read_node(shape.second_index, depth + 1)?.0;
            entries.push(TypeMapEntry { key, value });
        }
        Ok((TypeValue::Map(entries), shape.align))
    }

    fn read_length(&mut self, maximum: usize, field: &str) -> Result<usize> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        if length > maximum {
            return Err(Error::invalid_data(format!(
                "{field} count {length} exceeds limit {maximum}"
            )));
        }
        Ok(length)
    }

    fn read_aligned_string(&mut self) -> Result<String> {
        let length = self.read_length(self.limits.maximum_string_bytes, "type tree string byte")?;
        let value = self.reader.read_utf8(length)?;
        self.charge_materialized(value.capacity(), "decoded type tree string bytes")?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn clone_field_name(&mut self, value: &str) -> Result<String> {
        self.charge_materialized(value.len(), "type tree field-name bytes")?;
        let mut output = String::new();
        output.try_reserve_exact(value.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate a type tree field name: {error}"))
        })?;
        output.push_str(value);
        Ok(output)
    }

    fn charge_capacity<T>(&mut self, count: usize, field: &str) -> Result<()> {
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(|| Error::invalid_data(format!("{field} overflowed")))?;
        self.charge_materialized(bytes, field)
    }

    fn charge_materialized(&mut self, additional: usize, field: &str) -> Result<()> {
        self.materialized_bytes = self
            .materialized_bytes
            .checked_add(additional)
            .ok_or_else(|| Error::invalid_data(format!("{field} budget overflowed")))?;
        if self.materialized_bytes > self.limits.maximum_materialized_bytes {
            return Err(Error::invalid_data(format!(
                "{field} raises materialized type tree bytes to {}, exceeding limit {}",
                self.materialized_bytes, self.limits.maximum_materialized_bytes
            )));
        }
        Ok(())
    }

    fn align(&mut self, alignment: u64) -> Result<()> {
        let relative_position = self.reader.position()?;
        let absolute_position = self
            .absolute_start
            .checked_add(relative_position)
            .ok_or_else(|| Error::invalid_data("object alignment position overflowed"))?;
        let remainder = absolute_position % alignment;
        if remainder == 0 {
            return Ok(());
        }
        let target = relative_position
            .checked_add(alignment - remainder)
            .ok_or_else(|| Error::invalid_data("object aligned position overflowed"))?;
        if target > self.reader.len()? {
            return Err(Error::invalid_data("object alignment exceeds its payload"));
        }
        self.reader.set_position(target)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Signed8,
    Unsigned8,
    Character,
    Signed16,
    Unsigned16,
    Signed32,
    Unsigned32,
    Signed64,
    Unsigned64,
    Float32,
    Float64,
    Boolean,
    String,
    Map,
    TypelessData,
    Other,
}

impl ValueKind {
    fn from_type_name(value: &str) -> Self {
        match value {
            "SInt8" => Self::Signed8,
            "UInt8" => Self::Unsigned8,
            "char" => Self::Character,
            "short" | "SInt16" => Self::Signed16,
            "UInt16" | "unsigned short" => Self::Unsigned16,
            "int" | "SInt32" => Self::Signed32,
            "UInt32" | "unsigned int" | "Type*" => Self::Unsigned32,
            "long long" | "SInt64" => Self::Signed64,
            "UInt64" | "unsigned long long" | "FileSize" => Self::Unsigned64,
            "float" => Self::Float32,
            "double" => Self::Float64,
            "bool" => Self::Boolean,
            "string" => Self::String,
            "map" => Self::Map,
            "TypelessData" => Self::TypelessData,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ArrayShape {
    data_index: usize,
    align: bool,
}

#[derive(Debug, Clone, Copy)]
struct MapShape {
    first_index: usize,
    second_index: usize,
    align: bool,
}

fn is_array_parent(nodes: &[TypeTreeNode], index: usize) -> bool {
    nodes.get(index + 1).is_some_and(|node| {
        node.type_name == "Array" && node.level == nodes[index].level.saturating_add(1)
    })
}

fn validate_array_shape(nodes: &[TypeTreeNode], index: usize) -> Result<ArrayShape> {
    let parent = nodes
        .get(index)
        .ok_or_else(|| Error::invalid_data("array parent is outside the type tree"))?;
    let array_index = index
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("array node index overflowed"))?;
    let size_index = index
        .checked_add(2)
        .ok_or_else(|| Error::invalid_data("array size node index overflowed"))?;
    let data_index = index
        .checked_add(3)
        .ok_or_else(|| Error::invalid_data("array data node index overflowed"))?;
    let array = nodes.get(array_index).ok_or_else(|| {
        Error::invalid_data(format!("type tree node {index} is missing its Array node"))
    })?;
    let size = nodes.get(size_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "type tree node {index} is missing its array size node"
        ))
    })?;
    let data = nodes.get(data_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "type tree node {index} is missing its array data node"
        ))
    })?;
    let child_level = parent.level.saturating_add(1);
    let element_level = child_level.saturating_add(1);
    if array.type_name != "Array"
        || array.level != child_level
        || size.level != element_level
        || !matches!(size.type_name.as_str(), "int" | "SInt32")
        || data.level != element_level
    {
        return Err(Error::invalid_data(format!(
            "type tree node {index} has a malformed array schema"
        )));
    }
    if subtree_end(nodes, data_index) != subtree_end(nodes, index) {
        return Err(Error::invalid_data(format!(
            "type tree node {index} has fields after its array data schema"
        )));
    }
    Ok(ArrayShape {
        data_index,
        align: array.meta_flags & ALIGN_BYTES_FLAG != 0,
    })
}

fn validate_map_shape(nodes: &[TypeTreeNode], index: usize) -> Result<MapShape> {
    let array = validate_array_shape(nodes, index)?;
    let pair_index = array.data_index;
    let pair = &nodes[pair_index];
    if pair.type_name != "pair" {
        return Err(Error::invalid_data(format!(
            "type tree map node {index} has {:?} instead of pair data",
            pair.type_name
        )));
    }
    let first_index = pair_index
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("map first node index overflowed"))?;
    let first = nodes
        .get(first_index)
        .ok_or_else(|| Error::invalid_data("type tree map is missing its first value"))?;
    if first.level != pair.level.saturating_add(1) {
        return Err(Error::invalid_data(
            "type tree map first value has an invalid level",
        ));
    }
    let second_index = subtree_end(nodes, first_index);
    let second = nodes
        .get(second_index)
        .ok_or_else(|| Error::invalid_data("type tree map is missing its second value"))?;
    if second.level != pair.level.saturating_add(1)
        || subtree_end(nodes, second_index) != subtree_end(nodes, pair_index)
    {
        return Err(Error::invalid_data(
            "type tree map second value has an invalid shape",
        ));
    }
    Ok(MapShape {
        first_index,
        second_index,
        align: array.align,
    })
}

fn validate_typeless_shape(nodes: &[TypeTreeNode], index: usize) -> Result<()> {
    let parent = nodes
        .get(index)
        .ok_or_else(|| Error::invalid_data("TypelessData parent is outside the type tree"))?;
    let size_index = index
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("TypelessData size node index overflowed"))?;
    let data_index = index
        .checked_add(2)
        .ok_or_else(|| Error::invalid_data("TypelessData data node index overflowed"))?;
    let size = nodes.get(size_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "TypelessData node {index} is missing its size field"
        ))
    })?;
    let data = nodes.get(data_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "TypelessData node {index} is missing its data field"
        ))
    })?;
    let child_level = parent.level.saturating_add(1);
    if size.level != child_level
        || !matches!(size.type_name.as_str(), "int" | "SInt32")
        || data.level != child_level
        || !matches!(data.type_name.as_str(), "UInt8" | "SInt8" | "char")
        || subtree_end(nodes, data_index) != subtree_end(nodes, index)
    {
        return Err(Error::invalid_data(format!(
            "TypelessData node {index} has a malformed byte-array schema"
        )));
    }
    Ok(())
}

fn subtree_end(nodes: &[TypeTreeNode], index: usize) -> usize {
    let Some(root) = nodes.get(index) else {
        return nodes.len();
    };
    nodes[index + 1..]
        .iter()
        .position(|node| node.level <= root.level)
        .map_or(nodes.len(), |relative| index + 1 + relative)
}

fn validate_tree_shape(nodes: &[TypeTreeNode]) -> Result<()> {
    let root = nodes
        .first()
        .ok_or_else(|| Error::invalid_data("type tree has no root node"))?;
    if root.level != 0 {
        return Err(Error::invalid_data(format!(
            "type tree root has level {}, expected 0",
            root.level
        )));
    }
    let mut previous = root.level;
    for (index, node) in nodes.iter().enumerate().skip(1) {
        if node.level == 0 {
            return Err(Error::invalid_data(format!(
                "type tree has a second root at node {index}"
            )));
        }
        if node.level > previous.saturating_add(1) {
            return Err(Error::invalid_data(format!(
                "type tree level jumps from {previous} to {} at node {index}",
                node.level
            )));
        }
        previous = node.level;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::endian::{Endian, EndianReader};
    use crate::serialized::{TypeTree, TypeTreeNode};

    use super::{TypeField, TypeTreeReadLimits, TypeValue, read_type_tree_from_reader};

    #[test]
    fn reads_classes_strings_and_aligned_arrays() {
        let tree = TypeTree {
            nodes: vec![
                node("Root", "Base", 0, false),
                node("int", "m_Count", 1, false),
                node("string", "m_Name", 1, false),
                node("Array", "Array", 2, false),
                node("int", "size", 3, false),
                node("char", "data", 3, false),
                node("vector", "m_Values", 1, false),
                node("Array", "Array", 2, true),
                node("int", "size", 3, false),
                node("SInt16", "data", 3, false),
            ],
            string_buffer: Vec::new(),
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&42_i32.to_le_bytes());
        bytes.extend_from_slice(&3_i32.to_le_bytes());
        bytes.extend_from_slice(b"abc");
        bytes.push(0);
        bytes.extend_from_slice(&2_i32.to_le_bytes());
        bytes.extend_from_slice(&(-1_i16).to_le_bytes());
        bytes.extend_from_slice(&7_i16.to_le_bytes());

        let value = read_type_tree_from_reader(
            &tree,
            EndianReader::new(Cursor::new(bytes), Endian::Little),
            0,
            TypeTreeReadLimits::default(),
        )
        .unwrap();
        assert_eq!(
            value,
            TypeValue::Object(vec![
                TypeField {
                    name: "m_Count".to_owned(),
                    value: TypeValue::Signed(42),
                },
                TypeField {
                    name: "m_Name".to_owned(),
                    value: TypeValue::String("abc".to_owned()),
                },
                TypeField {
                    name: "m_Values".to_owned(),
                    value: TypeValue::Array(vec![TypeValue::Signed(-1), TypeValue::Signed(7),]),
                },
            ])
        );
    }

    #[test]
    fn reads_maps_without_losing_order() {
        let tree = TypeTree {
            nodes: vec![
                node("map", "m_Map", 0, false),
                node("Array", "Array", 1, false),
                node("int", "size", 2, false),
                node("pair", "data", 2, false),
                node("int", "first", 3, false),
                node("UInt8", "second", 3, false),
            ],
            string_buffer: Vec::new(),
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2_i32.to_be_bytes());
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        bytes.push(2);
        bytes.extend_from_slice(&3_i32.to_be_bytes());
        bytes.push(4);

        let value = read_type_tree_from_reader(
            &tree,
            EndianReader::new(Cursor::new(bytes), Endian::Big),
            0,
            TypeTreeReadLimits::default(),
        )
        .unwrap();
        let TypeValue::Map(entries) = value else {
            panic!("expected a map");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key, TypeValue::Signed(1));
        assert_eq!(entries[0].value, TypeValue::Unsigned(2));
        assert_eq!(entries[1].key, TypeValue::Signed(3));
        assert_eq!(entries[1].value, TypeValue::Unsigned(4));
    }

    #[test]
    fn rejects_bad_shapes_limits_and_trailing_payload() {
        let malformed = TypeTree {
            nodes: vec![node("vector", "m_Data", 0, false)],
            string_buffer: Vec::new(),
        };
        let reader = EndianReader::new(Cursor::new(0_i32.to_le_bytes()), Endian::Little);
        assert!(
            read_type_tree_from_reader(&malformed, reader, 0, TypeTreeReadLimits::default())
                .is_err()
        );

        let primitive = TypeTree {
            nodes: vec![node("int", "value", 0, false)],
            string_buffer: Vec::new(),
        };
        let mut bytes = 1_i32.to_le_bytes().to_vec();
        bytes.push(0);
        let reader = EndianReader::new(Cursor::new(bytes), Endian::Little);
        assert!(
            read_type_tree_from_reader(&primitive, reader, 0, TypeTreeReadLimits::default())
                .is_err()
        );

        let long_field_name = "x".repeat(4_096);
        let repeated_class = TypeTree {
            nodes: vec![
                node("vector", "m_Items", 0, false),
                node("Array", "Array", 1, false),
                node("int", "size", 2, false),
                node("Item", "data", 2, false),
                node("int", &long_field_name, 3, false),
            ],
            string_buffer: Vec::new(),
        };
        let mut payload = Vec::new();
        payload.extend_from_slice(&128_i32.to_le_bytes());
        for value in 0_i32..128 {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        let bounded_materialization = TypeTreeReadLimits {
            maximum_materialized_bytes: 32 * 1024,
            ..TypeTreeReadLimits::default()
        };
        let error = read_type_tree_from_reader(
            &repeated_class,
            EndianReader::new(Cursor::new(payload), Endian::Little),
            0,
            bounded_materialization,
        )
        .unwrap_err();
        assert!(error.to_string().contains("materialized type tree bytes"));
    }

    fn node(type_name: &str, field_name: &str, level: u32, align: bool) -> TypeTreeNode {
        TypeTreeNode {
            type_name: type_name.to_owned(),
            field_name: field_name.to_owned(),
            byte_size: -1,
            index: 0,
            type_flags: 0,
            version: 1,
            meta_flags: if align { 0x4000 } else { 0 },
            level,
            type_string_offset: None,
            name_string_offset: None,
            reference_type_hash: 0,
        }
    }
}
