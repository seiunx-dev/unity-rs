use std::io::{Read, Seek};

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::{SerializedFile, SerializedType, TypeTree, TypeTreeNode};
use crate::{Error, Result};

const ALIGN_BYTES_FLAG: i32 = 0x4000;

/// The node names Unity gives the managed-references registry it writes after
/// an object body for `SerializeReference` fields.
const MANAGED_REFERENCES_REGISTRY: &str = "ManagedReferencesRegistry";
const REFERENCED_MANAGED_TYPE: &str = "ReferencedManagedType";
const REFERENCED_OBJECT_DATA: &str = "ReferencedObjectData";

/// Borrows the class, namespace and assembly one registry entry names.
///
/// These strings already live in the materialized `ReferencedManagedType`
/// value. Keeping a second owned copy until the data field is reached can add
/// three maximum-sized strings per entry outside the materialization budget.
fn managed_type_identity(value: &TypeValue) -> Result<(&str, &str, &str)> {
    let TypeValue::Object(fields) = value else {
        return Err(Error::invalid_data(
            "managed reference type is not a record of class, namespace and assembly",
        ));
    };
    let text = |name: &str| -> Result<&str> {
        match fields.iter().find(|field| field.name == name) {
            Some(TypeField {
                value: TypeValue::String(value),
                ..
            }) => Ok(value.as_str()),
            _ => Err(Error::invalid_data(format!(
                "managed reference type has no {name} string"
            ))),
        }
    };
    Ok((text("class")?, text("ns")?, text("asm")?))
}

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
            // A million of either is below what ordinary game content holds: a
            // Live2D model in a shipping Unity 6000.3 build carries a single
            // array of 3,892,672 elements, and the count alone is a poor guard
            // anyway -- `maximum_materialized_bytes` is what actually bounds
            // the heap, and it bounds it by what was allocated rather than by
            // how many things were counted. These stay as a coarse ceiling on
            // absurd counts, set where real assets fit under them.
            maximum_values: 32_000_000,
            maximum_array_elements: 32_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_typeless_bytes: 256 * 1024 * 1024,
            // Set from a measurement, like the two above. A Live2D
            // MonoBehaviour in a shipping Unity 6000.3 build materializes to
            // between 384 and 512 MiB of value tree -- the same content whose
            // package materialization was measured at 439 MB -- and at 256 MiB
            // an otherwise clean 11,639-object export failed on two of them.
            // This is the bound that actually decides peak heap for one
            // object, so raising it raises that peak: 512 MiB covers what has
            // been measured here, and is not a proof that nothing needs more.
            maximum_materialized_bytes: 512 * 1024 * 1024,
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
    /// A serialized `float`, kept at its source width.
    ///
    /// Widening to `f64` on decode would be lossless numerically but not
    /// textually: the shortest round-trip form of the widened value is the
    /// double expansion, so `0.1f` would serialize as `0.10000000149011612`.
    Float32(f32),
    /// A serialized `double`.
    Float(f64),
    Boolean(bool),
    String(String),
    TypelessData {
        offset: u64,
        size: u64,
    },
    Array(Vec<Self>),
    Object(Vec<TypeField>),
    Map(Vec<TypeMapEntry>),
}

impl TypeValue {
    /// Returns the numeric value of either floating-point variant.
    ///
    /// Consumers that only need the number, such as the Cubism schema readers,
    /// should use this rather than matching one variant and silently rejecting
    /// the other.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float32(value) => Some(f64::from(*value)),
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }
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
        read_type_tree_from_reader_with_reference_types(
            tree,
            EndianReader::new(payload.cursor(), endian),
            object.byte_start,
            limits,
            &self.reference_types,
        )
    }

    /// Reads one immediate field from an object's root record while still
    /// walking and validating the complete `TypeTree` payload.
    ///
    /// This is intentionally crate-private: it is a bounded projection used by
    /// readers that need one stable field, not a second public dynamic-object
    /// API. Fields that are not selected are consumed without retaining their
    /// value tree, so a small pointer field does not require materializing an
    /// unrelated multi-million-element array.
    pub(crate) fn read_type_tree_root_field_with_limits(
        &self,
        object_index: usize,
        field_name: &str,
        limits: TypeTreeReadLimits,
    ) -> Result<Option<TypeValue>> {
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
        read_type_tree_root_field_from_reader_with_reference_types(
            tree,
            EndianReader::new(payload.cursor(), endian),
            object.byte_start,
            limits,
            &self.reference_types,
            field_name,
        )
    }
}

pub fn read_type_tree_from_reader<R: Read + Seek>(
    tree: &TypeTree,
    reader: EndianReader<R>,
    absolute_start: u64,
    limits: TypeTreeReadLimits,
) -> Result<TypeValue> {
    read_type_tree_from_reader_with_reference_types(tree, reader, absolute_start, limits, &[])
}

/// The same read, with the reference types the serialized file declares.
///
/// A `SerializeReference` field stores nothing where it is declared: the value
/// lives in a managed-references registry written after the object body, keyed
/// by `rid`. The registry's own shape is in the type tree, but the shape of
/// each stored object is not -- that comes from the file's reference types,
/// matched by class, namespace and assembly. Without them such an object can
/// be walked to its last node and still have most of its bytes left over: one
/// `CriWare` sound component in a shipping Unity 6000.3 build has 176 bytes of
/// typed fields and 712,292 bytes of registry behind a single `rid`.
pub fn read_type_tree_from_reader_with_reference_types<R: Read + Seek>(
    tree: &TypeTree,
    reader: EndianReader<R>,
    absolute_start: u64,
    limits: TypeTreeReadLimits,
    reference_types: &[SerializedType],
) -> Result<TypeValue> {
    validate_tree_shape(&tree.nodes)?;
    let mut parser = TypeTreeValueReader {
        nodes: &tree.nodes,
        reader,
        absolute_start,
        limits,
        values_read: 0,
        materialized_bytes: 0,
        reference_types,
        reference_type_lookup: None,
        validated_reference_types: None,
        has_registry: false,
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
        // The tree was walked to its last node, so the layout it describes was
        // read in full and what remains is data the tree does not describe.
        // The managed-references registry a `SerializeReference` field writes
        // after the body is read above when the tree declares one, so what is
        // left here is a tree that does not match its object -- most often a
        // supplied schema built from an assembly that has since changed, or
        // one whose generator dropped a field. Declining says that, where a
        // byte mismatch would describe the reader instead of the asset.
        return Err(Error::unsupported(format!(
            "type tree describes {consumed} of {length} object bytes; the tree does \
             not match this object"
        )));
    }
    Ok(value)
}

fn read_type_tree_root_field_from_reader_with_reference_types<R: Read + Seek>(
    tree: &TypeTree,
    reader: EndianReader<R>,
    absolute_start: u64,
    limits: TypeTreeReadLimits,
    reference_types: &[SerializedType],
    field_name: &str,
) -> Result<Option<TypeValue>> {
    validate_tree_shape(&tree.nodes)?;
    let mut parser = TypeTreeValueReader {
        nodes: &tree.nodes,
        reader,
        absolute_start,
        limits,
        values_read: 0,
        materialized_bytes: 0,
        reference_types,
        reference_type_lookup: None,
        validated_reference_types: None,
        has_registry: false,
    };
    let (value, next) = parser.read_root_field(0, field_name)?;
    if next != tree.nodes.len() {
        return Err(Error::invalid_data(format!(
            "type tree root covers {next} of {} nodes",
            tree.nodes.len()
        )));
    }
    let consumed = parser.reader.position()?;
    let length = parser.reader.len()?;
    if consumed != length {
        return Err(Error::unsupported(format!(
            "type tree describes {consumed} of {length} object bytes; the tree does \
             not match this object"
        )));
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy)]
struct ReferenceTypeLookupEntry<'a> {
    class_name: &'a str,
    namespace: &'a str,
    assembly_name: &'a str,
    index: usize,
}

impl ReferenceTypeLookupEntry<'_> {
    fn key(&self) -> (&str, &str, &str) {
        (self.class_name, self.namespace, self.assembly_name)
    }
}

struct TypeTreeValueReader<'a, R> {
    nodes: &'a [TypeTreeNode],
    reader: EndianReader<R>,
    absolute_start: u64,
    limits: TypeTreeReadLimits,
    values_read: usize,
    materialized_bytes: usize,
    /// The file's reference types, which is where the layout of a
    /// `SerializeReference` value comes from.
    reference_types: &'a [SerializedType],
    /// Built only when a registry entry names a non-null type. Entries are
    /// sorted by identity and then source index, so binary lookup preserves the
    /// old first-declaration-wins behavior without scanning all reference types
    /// for every `rid`.
    reference_type_lookup: Option<Vec<ReferenceTypeLookupEntry<'a>>>,
    /// One bit per reference type, built lazily. A sorted `Vec<usize>` still
    /// makes files whose entries each name a distinct type quadratic when
    /// checking whether that type's tree has already been validated.
    validated_reference_types: Option<Vec<bool>>,
    /// Unity writes one registry per object, at the outermost level. A
    /// reference type's own tree can declare another, and reading that one
    /// would consume bytes that are not there.
    has_registry: bool,
}

impl<'a, R: Read + Seek> TypeTreeValueReader<'a, R> {
    fn read_node(&mut self, index: usize, depth: usize) -> Result<(TypeValue, usize)> {
        self.visit_node(depth, true)?;

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
            ValueKind::Float32 => (TypeValue::Float32(self.reader.read_f32()?), index + 1),
            ValueKind::Float64 => (TypeValue::Float(self.reader.read_f64()?), index + 1),
            ValueKind::Boolean => (TypeValue::Boolean(self.reader.read_bool()?), index + 1),
            ValueKind::String if is_array_parent(self.nodes, index) => {
                validate_array_shape(self.nodes, index)?;
                let value = self.read_aligned_string()?;
                (TypeValue::String(value), subtree_end)
            }
            // A node named `string` is only Unity's string when an `Array` child
            // carries its bytes. Unity also emits `string` as an ordinary class
            // name -- `PropertyName`, the payload of `ExposedReference<T>`, is
            // one, and every Timeline asset that points at a scene object has
            // one. Both managed readers guard on it -- `TypeTreeHelper.cs:280`
            // and, in the JSON writer this payload is compared against,
            // `AssetStudioSession.cs:1013`
            // (`when i + 1 < nodes.Count && nodes[i + 1].m_Type == "Array"`) --
            // and let the rest fall through to their class branch. Treating them
            // all as strings rejected objects the reference reads.
            ValueKind::String => (self.read_class(index, depth)?, subtree_end),
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
                self.skip_bytes(length, "TypelessData")?;
                let size = u64::try_from(length).expect("TypelessData length fits in u64");
                (TypeValue::TypelessData { offset, size }, subtree_end)
            }
            ValueKind::ReferencedObject => {
                (self.read_referenced_object(index, depth)?, subtree_end)
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

    fn visit_node(&mut self, depth: usize, materialize: bool) -> Result<()> {
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
        if materialize {
            self.charge_materialized(std::mem::size_of::<TypeValue>(), "type tree value storage")?;
        }
        Ok(())
    }

    fn read_root_field(
        &mut self,
        index: usize,
        field_name: &str,
    ) -> Result<(Option<TypeValue>, usize)> {
        self.visit_node(0, false)?;
        let root = self.nodes.get(index).ok_or_else(|| {
            Error::invalid_data(format!("type tree node index {index} is out of range"))
        })?;
        let is_class = matches!(ValueKind::from_type_name(&root.type_name), ValueKind::Other)
            || matches!(
                ValueKind::from_type_name(&root.type_name),
                ValueKind::String
            ) && !is_array_parent(self.nodes, index);
        if !is_class {
            return Err(Error::invalid_data(
                "type tree root-field projection requires a record root",
            ));
        }

        let root_level = root.level;
        let align = root.meta_flags & ALIGN_BYTES_FLAG != 0;
        let end = subtree_end(self.nodes, index);
        let mut selected = None;
        let mut child = index + 1;
        while child < end {
            let node = &self.nodes[child];
            if node.level != root_level + 1 {
                return Err(Error::invalid_data(format!(
                    "type tree class child at index {child} has level {}, expected {}",
                    node.level,
                    root_level + 1
                )));
            }
            if node.type_name == MANAGED_REFERENCES_REGISTRY {
                if self.has_registry {
                    child = subtree_end(self.nodes, child);
                    continue;
                }
                self.has_registry = true;
            }
            if selected.is_none() && node.field_name == field_name {
                let (value, next) = self.read_node(child, 1)?;
                selected = Some(value);
                child = next;
            } else {
                child = self.skip_node(child, 1)?;
            }
        }
        if align {
            self.align(4)?;
        }
        Ok((selected, end))
    }

    fn skip_node(&mut self, index: usize, depth: usize) -> Result<usize> {
        self.visit_node(depth, false)?;
        let node = self.nodes.get(index).ok_or_else(|| {
            Error::invalid_data(format!("type tree node index {index} is out of range"))
        })?;
        let kind = ValueKind::from_type_name(&node.type_name);
        let mut align = node.meta_flags & ALIGN_BYTES_FLAG != 0;
        let end = subtree_end(self.nodes, index);
        let next = match kind {
            ValueKind::Signed8 => {
                self.reader.read_i8()?;
                index + 1
            }
            ValueKind::Unsigned8 | ValueKind::Boolean => {
                self.reader.read_u8()?;
                index + 1
            }
            ValueKind::Character | ValueKind::Unsigned16 => {
                self.reader.read_u16()?;
                index + 1
            }
            ValueKind::Signed16 => {
                self.reader.read_i16()?;
                index + 1
            }
            ValueKind::Signed32 => {
                self.reader.read_i32()?;
                index + 1
            }
            ValueKind::Unsigned32 | ValueKind::Float32 => {
                self.reader.read_u32()?;
                index + 1
            }
            ValueKind::Signed64 => {
                self.reader.read_i64()?;
                index + 1
            }
            ValueKind::Unsigned64 | ValueKind::Float64 => {
                self.reader.read_u64()?;
                index + 1
            }
            ValueKind::String if is_array_parent(self.nodes, index) => {
                validate_array_shape(self.nodes, index)?;
                self.skip_aligned_string()?;
                end
            }
            ValueKind::String => {
                self.skip_class(index, depth)?;
                end
            }
            ValueKind::Map => {
                align |= self.skip_map(index, depth)?;
                end
            }
            ValueKind::TypelessData => {
                validate_typeless_shape(self.nodes, index)?;
                let length =
                    self.read_length(self.limits.maximum_typeless_bytes, "TypelessData byte")?;
                self.skip_bytes(length, "TypelessData")?;
                end
            }
            ValueKind::ReferencedObject => {
                self.skip_referenced_object(index, depth)?;
                end
            }
            ValueKind::Other if is_array_parent(self.nodes, index) => {
                align |= self.skip_array(index, depth)?;
                end
            }
            ValueKind::Other => {
                self.skip_class(index, depth)?;
                end
            }
        };
        if align {
            self.align(4)?;
        }
        Ok(next)
    }

    fn skip_class(&mut self, index: usize, depth: usize) -> Result<()> {
        let parent_level = self.nodes[index].level;
        let end = subtree_end(self.nodes, index);
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
            if node.type_name == MANAGED_REFERENCES_REGISTRY {
                if self.has_registry {
                    child = subtree_end(self.nodes, child);
                    continue;
                }
                self.has_registry = true;
            }
            child = self.skip_node(child, depth + 1)?;
        }
        Ok(())
    }

    fn skip_referenced_object(&mut self, index: usize, depth: usize) -> Result<()> {
        let parent_level = self.nodes[index].level;
        let end = subtree_end(self.nodes, index);
        let mut reference_type_index = None;
        let mut child = index + 1;
        while child < end {
            let node = &self.nodes[child];
            if node.level != parent_level + 1 {
                return Err(Error::invalid_data(format!(
                    "managed reference child at index {child} has level {}, expected {}",
                    node.level,
                    parent_level + 1
                )));
            }
            if node.type_name == REFERENCED_OBJECT_DATA {
                let reference_type_index = reference_type_index.ok_or_else(|| {
                    Error::invalid_data("managed reference stores its data before naming its type")
                })?;
                if let Some(tree_index) = reference_type_index {
                    self.skip_reference_type(tree_index, depth + 1)?;
                }
                child = subtree_end(self.nodes, child);
                continue;
            }
            if node.type_name == REFERENCED_MANAGED_TYPE {
                let (value, next) = self.read_node(child, depth + 1)?;
                reference_type_index =
                    Some(self.reference_type_index(managed_type_identity(&value)?)?);
                child = next;
            } else {
                child = self.skip_node(child, depth + 1)?;
            }
        }
        Ok(())
    }

    fn skip_reference_type(&mut self, tree_index: usize, depth: usize) -> Result<()> {
        self.ensure_reference_type_validation_cache()?;
        let tree = self.reference_types[tree_index]
            .type_tree
            .as_ref()
            .ok_or_else(|| {
                Error::unsupported(format!(
                    "reference type {tree_index} carries no type tree, so its stored \
                     value has no stated layout"
                ))
            })?;
        let already_validated = self
            .validated_reference_types
            .as_ref()
            .is_some_and(|validated| validated[tree_index]);
        if !already_validated {
            validate_tree_shape(&tree.nodes)?;
            self.validated_reference_types
                .as_mut()
                .expect("reference-type validation cache was initialized")[tree_index] = true;
        }
        let outer = self.nodes;
        self.nodes = &tree.nodes;
        let result = self.skip_node(0, depth);
        self.nodes = outer;
        let next = result?;
        if next != tree.nodes.len() {
            return Err(Error::invalid_data(format!(
                "reference type root covers {next} of {} nodes",
                tree.nodes.len()
            )));
        }
        Ok(())
    }

    fn skip_array(&mut self, index: usize, depth: usize) -> Result<bool> {
        let shape = validate_array_shape(self.nodes, index)?;
        let count = self.read_length(self.limits.maximum_array_elements, "array element")?;
        for _ in 0..count {
            self.skip_node(shape.data_index, depth + 1)?;
        }
        Ok(shape.align)
    }

    fn skip_map(&mut self, index: usize, depth: usize) -> Result<bool> {
        let shape = validate_map_shape(self.nodes, index)?;
        let count = self.read_length(self.limits.maximum_array_elements, "map entry")?;
        for _ in 0..count {
            self.skip_node(shape.first_index, depth + 1)?;
            self.skip_node(shape.second_index, depth + 1)?;
        }
        Ok(shape.align)
    }

    fn skip_aligned_string(&mut self) -> Result<()> {
        let length = self.read_length(self.limits.maximum_string_bytes, "type tree string byte")?;
        self.skip_bytes(length, "type tree string")?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(())
    }

    fn skip_bytes(&mut self, length: usize, field: &str) -> Result<()> {
        let start = self.reader.position()?;
        let length = u64::try_from(length)
            .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data(format!("{field} range overflowed")))?;
        if end > self.reader.len()? {
            return Err(Error::invalid_data(format!(
                "{field} extends past the object payload"
            )));
        }
        self.reader.set_position(end)
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
            if node.type_name == MANAGED_REFERENCES_REGISTRY {
                if self.has_registry {
                    // Unity wrote one registry, at the outermost level. This is
                    // a second declaration of the same thing, and reading it
                    // would consume bytes belonging to whatever follows.
                    child = subtree_end(self.nodes, child);
                    continue;
                }
                self.has_registry = true;
            }
            let name = self.clone_field_name(&node.field_name)?;
            let (value, next) = self.read_node(child, depth + 1)?;
            fields.push(TypeField { name, value });
            child = next;
        }
        Ok(TypeValue::Object(fields))
    }

    /// Reads one registry entry: its `rid`, the managed type it names, and the
    /// stored value laid out by that type's own tree.
    fn read_referenced_object(&mut self, index: usize, depth: usize) -> Result<TypeValue> {
        let parent_level = self.nodes[index].level;
        let end = subtree_end(self.nodes, index);
        let mut fields = Vec::new();
        // `Some(None)` is a declared null reference; outer `None` means the
        // entry has not named its type yet.
        let mut reference_type_index = None;
        let mut child = index + 1;
        while child < end {
            let node = &self.nodes[child];
            if node.level != parent_level + 1 {
                return Err(Error::invalid_data(format!(
                    "managed reference child at index {child} has level {}, expected {}",
                    node.level,
                    parent_level + 1
                )));
            }
            let name = self.clone_field_name(&node.field_name)?;
            if node.type_name == REFERENCED_OBJECT_DATA {
                let reference_type_index = reference_type_index.ok_or_else(|| {
                    Error::invalid_data("managed reference stores its data before naming its type")
                })?;
                // An entry naming no class is Unity's null reference. It
                // stores nothing, and the field is left out rather than
                // invented.
                if let Some(tree_index) = reference_type_index {
                    let value = self.read_reference_type(tree_index, depth + 1)?;
                    self.push_field(&mut fields, TypeField { name, value })?;
                }
                child = subtree_end(self.nodes, child);
                continue;
            }
            let (value, next) = self.read_node(child, depth + 1)?;
            if node.type_name == REFERENCED_MANAGED_TYPE {
                reference_type_index =
                    Some(self.reference_type_index(managed_type_identity(&value)?)?);
            }
            self.push_field(&mut fields, TypeField { name, value })?;
            child = next;
        }
        Ok(TypeValue::Object(fields))
    }

    fn push_field(&mut self, fields: &mut Vec<TypeField>, field: TypeField) -> Result<()> {
        self.charge_materialized(std::mem::size_of::<TypeField>(), "managed reference field")?;
        fields.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow managed reference fields: {error}"))
        })?;
        fields.push(field);
        Ok(())
    }

    /// Finds the reference type an entry names, or `None` for a null entry.
    fn reference_type_index(
        &mut self,
        (class_name, namespace, assembly_name): (&str, &str, &str),
    ) -> Result<Option<usize>> {
        if class_name.is_empty() {
            return Ok(None);
        }
        self.ensure_reference_type_lookup()?;
        let lookup = self
            .reference_type_lookup
            .as_ref()
            .expect("reference-type lookup was initialized");
        let key = (class_name, namespace, assembly_name);
        let offset = lookup.partition_point(|entry| entry.key() < key);
        match lookup.get(offset).filter(|entry| entry.key() == key) {
            Some(entry) => Ok(Some(entry.index)),
            None => {
                // Declining rather than skipping: the entry's bytes are in the
                // stream and their length is only known from the layout, so
                // there is no way to step over what cannot be read.
                Err(Error::unsupported(format!(
                    "managed reference names an undeclared type (namespace {} bytes, class {} bytes, assembly {} bytes), which the file does not declare",
                    namespace.len(),
                    class_name.len(),
                    assembly_name.len()
                )))
            }
        }
    }

    fn ensure_reference_type_lookup(&mut self) -> Result<()> {
        if self.reference_type_lookup.is_some() {
            return Ok(());
        }
        let count = self
            .reference_types
            .iter()
            .filter(|kind| {
                kind.class_name.is_some()
                    && kind.namespace.is_some()
                    && kind.assembly_name.is_some()
            })
            .count();
        self.charge_capacity::<ReferenceTypeLookupEntry<'a>>(
            count,
            "reference-type lookup storage",
        )?;
        let mut lookup = Vec::new();
        lookup.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate {count} reference-type lookup entries: {error}"
            ))
        })?;
        for (index, kind) in self.reference_types.iter().enumerate() {
            let (Some(class_name), Some(namespace), Some(assembly_name)) = (
                kind.class_name.as_deref(),
                kind.namespace.as_deref(),
                kind.assembly_name.as_deref(),
            ) else {
                continue;
            };
            lookup.push(ReferenceTypeLookupEntry {
                class_name,
                namespace,
                assembly_name,
                index,
            });
        }
        lookup.sort_unstable_by(|left, right| {
            left.key()
                .cmp(&right.key())
                .then_with(|| left.index.cmp(&right.index))
        });
        self.reference_type_lookup = Some(lookup);
        Ok(())
    }

    fn ensure_reference_type_validation_cache(&mut self) -> Result<()> {
        if self.validated_reference_types.is_some() {
            return Ok(());
        }
        let count = self.reference_types.len();
        self.charge_capacity::<bool>(count, "reference-type validation cache")?;
        let mut validated = Vec::new();
        validated.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate {count} reference-type validation entries: {error}"
            ))
        })?;
        validated.resize(count, false);
        self.validated_reference_types = Some(validated);
        Ok(())
    }

    fn read_reference_type(&mut self, tree_index: usize, depth: usize) -> Result<TypeValue> {
        self.ensure_reference_type_validation_cache()?;
        let tree = self.reference_types[tree_index]
            .type_tree
            .as_ref()
            .ok_or_else(|| {
                Error::unsupported(format!(
                    "reference type {tree_index} carries no type tree, so its stored \
                     value has no stated layout"
                ))
            })?;
        let already_validated = self
            .validated_reference_types
            .as_ref()
            .is_some_and(|validated| validated[tree_index]);
        if !already_validated {
            validate_tree_shape(&tree.nodes)?;
            self.validated_reference_types
                .as_mut()
                .expect("reference-type validation cache was initialized")[tree_index] = true;
        }
        // The stored value is laid out by another tree entirely, so the node
        // slice is swapped for the duration and put back however this ends.
        let outer = self.nodes;
        self.nodes = &tree.nodes;
        let result = self.read_node(0, depth);
        self.nodes = outer;
        let (value, next) = result?;
        if next != tree.nodes.len() {
            return Err(Error::invalid_data(format!(
                "reference type root covers {next} of {} nodes",
                tree.nodes.len()
            )));
        }
        Ok(value)
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
    /// One entry of a managed-references registry. Its `data` field has no
    /// children: the layout comes from the file's reference types.
    ReferencedObject,
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
            "ReferencedObject" => Self::ReferencedObject,
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

/// True when `index` carries its elements in an `Array` child, which is what
/// separates Unity's `string` from a class that merely happens to be named one.
///
/// The managed guards compare only the next node's type name; requiring it to
/// sit one level deeper additionally rejects an `Array` that is a sibling rather
/// than a child, a shape Unity does not emit.
pub(crate) fn is_array_parent(nodes: &[TypeTreeNode], index: usize) -> bool {
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

/// Checks that a node list describes one tree: a single level-0 root, and no
/// level jumping by more than one.
pub fn validate_tree_shape(nodes: &[TypeTreeNode]) -> Result<()> {
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
    use crate::serialized::{SerializedType, TypeTree, TypeTreeNode};

    use super::{
        ReferenceTypeLookupEntry, TypeField, TypeTreeReadLimits, TypeTreeValueReader, TypeValue,
        read_type_tree_from_reader, read_type_tree_from_reader_with_reference_types,
        read_type_tree_root_field_from_reader_with_reference_types,
    };

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

    /// Unity names the payload of `ExposedReference<T>` `string`, but it is
    /// `PropertyName` -- a class with a real string inside it, not a string.
    /// Reading it as one rejected every Timeline object that points at a scene
    /// object; the managed reader takes its class branch here.
    #[test]
    fn reads_a_string_named_class_as_a_class() {
        let tree = TypeTree {
            nodes: vec![
                node("Root", "Base", 0, false),
                node("string", "exposedName", 1, false),
                node("string", "id", 2, false),
                node("Array", "Array", 3, false),
                node("int", "size", 4, false),
                node("char", "data", 4, false),
                node("int", "m_Trailing", 1, false),
            ],
            string_buffer: Vec::new(),
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3_i32.to_le_bytes());
        bytes.extend_from_slice(b"259");
        bytes.push(0);
        bytes.extend_from_slice(&9_i32.to_le_bytes());

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
                    name: "exposedName".to_owned(),
                    value: TypeValue::Object(vec![TypeField {
                        name: "id".to_owned(),
                        value: TypeValue::String("259".to_owned()),
                    }]),
                },
                TypeField {
                    name: "m_Trailing".to_owned(),
                    value: TypeValue::Signed(9),
                },
            ])
        );
    }

    /// The same rule at its degenerate end: a `string` node with no children at
    /// all is an empty class and consumes nothing, which is what the managed
    /// class branch does when its node list holds only the node itself.
    #[test]
    fn reads_a_childless_string_node_as_an_empty_class() {
        let tree = TypeTree {
            nodes: vec![
                node("Root", "Base", 0, false),
                node("string", "m_Empty", 1, false),
                node("int", "m_Trailing", 1, false),
            ],
            string_buffer: Vec::new(),
        };

        let value = read_type_tree_from_reader(
            &tree,
            EndianReader::new(Cursor::new(5_i32.to_le_bytes().to_vec()), Endian::Little),
            0,
            TypeTreeReadLimits::default(),
        )
        .unwrap();
        assert_eq!(
            value,
            TypeValue::Object(vec![
                TypeField {
                    name: "m_Empty".to_owned(),
                    value: TypeValue::Object(Vec::new()),
                },
                TypeField {
                    name: "m_Trailing".to_owned(),
                    value: TypeValue::Signed(5),
                },
            ])
        );
    }

    /// The one way the class branch can consume different bytes than the string
    /// branch it replaced is the node's own align flag, which is captured before
    /// the split and applied after it. Managed does the same, and neither side
    /// takes an align flag from an array child that is not there.
    #[test]
    fn aligns_after_a_string_named_class_that_sets_the_align_flag() {
        let tree = TypeTree {
            nodes: vec![
                node("Root", "Base", 0, false),
                node("UInt8", "a", 1, false),
                node("string", "exposedName", 1, true),
                node("int", "b", 1, false),
            ],
            string_buffer: Vec::new(),
        };
        let bytes = vec![0x01, 0xaa, 0xaa, 0xaa, 0x02, 0x00, 0x00, 0x00];

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
                    name: "a".to_owned(),
                    value: TypeValue::Unsigned(1),
                },
                TypeField {
                    name: "exposedName".to_owned(),
                    value: TypeValue::Object(Vec::new()),
                },
                TypeField {
                    name: "b".to_owned(),
                    value: TypeValue::Signed(2),
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

    #[test]
    fn projects_one_root_field_without_materializing_unrelated_values() {
        let long_field_name = "unrelated_".repeat(512);
        let tree = TypeTree {
            nodes: vec![
                node("Root", "Base", 0, false),
                node("vector", "m_Unrelated", 1, false),
                node("Array", "Array", 2, false),
                node("int", "size", 3, false),
                node("Item", "data", 3, false),
                node("int", &long_field_name, 4, false),
                node("PPtr<MonoBehaviour>", "_moc", 1, false),
                node("int", "m_FileID", 2, false),
                node("SInt64", "m_PathID", 2, false),
                node("int", "m_Trailing", 1, false),
            ],
            string_buffer: Vec::new(),
        };
        let mut payload = Vec::new();
        payload.extend_from_slice(&128_i32.to_le_bytes());
        for value in 0_i32..128 {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        payload.extend_from_slice(&2_i32.to_le_bytes());
        payload.extend_from_slice(&77_i64.to_le_bytes());
        payload.extend_from_slice(&9_i32.to_le_bytes());
        let limits = TypeTreeReadLimits {
            maximum_materialized_bytes: 1_024,
            ..TypeTreeReadLimits::default()
        };

        let full_error = read_type_tree_from_reader(
            &tree,
            EndianReader::new(Cursor::new(payload.clone()), Endian::Little),
            0,
            limits,
        )
        .unwrap_err();
        assert!(
            full_error
                .to_string()
                .contains("materialized type tree bytes")
        );

        let projected = read_type_tree_root_field_from_reader_with_reference_types(
            &tree,
            EndianReader::new(Cursor::new(payload.clone()), Endian::Little),
            0,
            limits,
            &[],
            "_moc",
        )
        .unwrap();
        assert_eq!(
            projected,
            Some(TypeValue::Object(vec![
                TypeField {
                    name: "m_FileID".to_owned(),
                    value: TypeValue::Signed(2),
                },
                TypeField {
                    name: "m_PathID".to_owned(),
                    value: TypeValue::Signed(77),
                },
            ]))
        );

        payload.truncate(payload.len() - std::mem::size_of::<i32>());
        assert!(
            read_type_tree_root_field_from_reader_with_reference_types(
                &tree,
                EndianReader::new(Cursor::new(payload), Endian::Little),
                0,
                limits,
                &[],
                "_moc",
            )
            .is_err(),
            "projection must validate the skipped tail after the selected field"
        );
    }

    #[test]
    fn indexes_many_reference_types_once_and_preserves_first_declaration() {
        const REFERENCE_COUNT: usize = 16_384;
        let first_target = REFERENCE_COUNT - 2;
        let mut references = Vec::new();
        references.reserve_exact(REFERENCE_COUNT);
        for index in 0..first_target {
            references.push(reference_type(
                &format!("NearMiss{index:05}"),
                vec![node("int", "value", 0, false)],
            ));
        }
        references.push(reference_type(
            "Target",
            vec![node("int", "value", 0, false)],
        ));
        references.push(reference_type(
            "Target",
            vec![node("int", "duplicate_layout", 0, false)],
        ));
        let payload = vec![0_u8; REFERENCE_COUNT * std::mem::size_of::<i32>()];
        let mut parser = TypeTreeValueReader {
            nodes: &[],
            reader: EndianReader::new(Cursor::new(payload), Endian::Little),
            absolute_start: 0,
            limits: TypeTreeReadLimits::default(),
            values_read: 0,
            materialized_bytes: 0,
            reference_types: &references,
            reference_type_lookup: None,
            validated_reference_types: None,
            has_registry: false,
        };

        assert_eq!(
            parser
                .reference_type_index(("Target", "Game", "Game.dll"))
                .unwrap(),
            Some(first_target)
        );
        let lookup_bytes = parser.materialized_bytes;
        for _ in 1..REFERENCE_COUNT {
            assert_eq!(
                parser
                    .reference_type_index(("Target", "Game", "Game.dll"))
                    .unwrap(),
                Some(first_target)
            );
        }
        assert_eq!(parser.materialized_bytes, lookup_bytes);
        assert_eq!(
            parser.reference_type_lookup.as_ref().map(Vec::len),
            Some(REFERENCE_COUNT)
        );

        for index in 0..REFERENCE_COUNT {
            assert_eq!(
                parser.read_reference_type(index, 0).unwrap(),
                TypeValue::Signed(0)
            );
        }
        assert!(
            parser
                .validated_reference_types
                .as_ref()
                .is_some_and(|validated| validated.iter().all(|value| *value))
        );
    }

    #[test]
    fn bounds_reference_type_lookup_and_validation_storage_before_allocation() {
        let references = (0..8)
            .map(|index| {
                reference_type(
                    &format!("Type{index}"),
                    vec![node("int", "value", 0, false)],
                )
            })
            .collect::<Vec<_>>();
        let lookup_bytes = references.len() * std::mem::size_of::<ReferenceTypeLookupEntry<'_>>();
        let mut lookup_parser = TypeTreeValueReader {
            nodes: &[],
            reader: EndianReader::new(Cursor::new(Vec::new()), Endian::Little),
            absolute_start: 0,
            limits: TypeTreeReadLimits {
                maximum_materialized_bytes: lookup_bytes - 1,
                ..TypeTreeReadLimits::default()
            },
            values_read: 0,
            materialized_bytes: 0,
            reference_types: &references,
            reference_type_lookup: None,
            validated_reference_types: None,
            has_registry: false,
        };
        let lookup_error = lookup_parser
            .reference_type_index(("Type7", "Game", "Game.dll"))
            .unwrap_err();
        assert!(
            lookup_error
                .to_string()
                .contains("reference-type lookup storage")
        );
        assert!(lookup_parser.reference_type_lookup.is_none());

        let mut validation_parser = TypeTreeValueReader {
            nodes: &[],
            reader: EndianReader::new(Cursor::new(0_i32.to_le_bytes()), Endian::Little),
            absolute_start: 0,
            limits: TypeTreeReadLimits {
                maximum_materialized_bytes: references.len() - 1,
                ..TypeTreeReadLimits::default()
            },
            values_read: 0,
            materialized_bytes: 0,
            reference_types: &references,
            reference_type_lookup: None,
            validated_reference_types: None,
            has_registry: false,
        };
        let validation_error = validation_parser.read_reference_type(0, 0).unwrap_err();
        assert!(
            validation_error
                .to_string()
                .contains("reference-type validation cache")
        );
        assert!(validation_parser.validated_reference_types.is_none());
    }

    /// The registry Unity writes after an object body for a
    /// `SerializeReference` field, spelled exactly as a shipping Unity 6000.3
    /// bundle spells it.
    fn registry_nodes(level: u32) -> Vec<TypeTreeNode> {
        vec![
            node("ManagedReferencesRegistry", "references", level, true),
            node("int", "version", level + 1, false),
            node("vector", "RefIds", level + 1, true),
            node("Array", "Array", level + 2, true),
            node("int", "size", level + 3, false),
            node("ReferencedObject", "data", level + 3, true),
            node("SInt64", "rid", level + 4, false),
            node("ReferencedManagedType", "type", level + 5 - 1, true),
            node("string", "class", level + 5, true),
            node("Array", "Array", level + 6, true),
            node("int", "size", level + 7, false),
            node("char", "data", level + 7, false),
            node("string", "ns", level + 5, true),
            node("Array", "Array", level + 6, true),
            node("int", "size", level + 7, false),
            node("char", "data", level + 7, false),
            node("string", "asm", level + 5, true),
            node("Array", "Array", level + 6, true),
            node("int", "size", level + 7, false),
            node("char", "data", level + 7, false),
            node("ReferencedObjectData", "data", level + 4, false),
        ]
    }

    fn reference_type(class_name: &str, nodes: Vec<TypeTreeNode>) -> SerializedType {
        named_reference_type(class_name, "Game", "Game.dll", nodes)
    }

    fn named_reference_type(
        class_name: &str,
        namespace: &str,
        assembly_name: &str,
        nodes: Vec<TypeTreeNode>,
    ) -> SerializedType {
        SerializedType {
            class_id: 114,
            is_stripped_type: false,
            script_type_index: -1,
            script_id: None,
            old_type_hash: None,
            type_tree: Some(TypeTree {
                nodes,
                string_buffer: Vec::new(),
            }),
            type_dependencies: Vec::new(),
            class_name: Some(class_name.to_owned()),
            namespace: Some(namespace.to_owned()),
            assembly_name: Some(assembly_name.to_owned()),
        }
    }

    fn push_registry_entry(bytes: &mut Vec<u8>, rid: i64, class_name: &str) {
        let string = |bytes: &mut Vec<u8>, value: &str| {
            bytes.extend_from_slice(&i32::try_from(value.len()).unwrap().to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
            while !bytes.len().is_multiple_of(4) {
                bytes.push(0);
            }
        };
        bytes.extend_from_slice(&rid.to_le_bytes());
        string(bytes, class_name);
        string(bytes, if class_name.is_empty() { "" } else { "Game" });
        string(
            bytes,
            if class_name.is_empty() {
                ""
            } else {
                "Game.dll"
            },
        );
    }

    #[test]
    fn reads_a_serialize_reference_registry_through_the_file_reference_types() {
        let mut nodes = vec![
            node("Root", "Base", 0, false),
            node("int", "m_Value", 1, false),
        ];
        nodes.extend(registry_nodes(1));
        let tree = TypeTree {
            nodes,
            string_buffer: Vec::new(),
        };
        // All three name the class the entry names. Only one is a match, and
        // picking a near miss reads a different number of bytes from here on,
        // so the trailing-byte check turns any confusion into a failure.
        let references = [
            named_reference_type(
                "Payload",
                "Game",
                "Other.dll",
                vec![
                    node("Payload", "Base", 0, true),
                    node("SInt64", "m_Stored", 1, false),
                ],
            ),
            named_reference_type(
                "Payload",
                "Other",
                "Game.dll",
                vec![
                    node("Payload", "Base", 0, true),
                    node("int", "m_Stored", 1, false),
                    node("int", "m_Extra", 1, false),
                ],
            ),
            reference_type(
                "Payload",
                vec![
                    node("Payload", "Base", 0, true),
                    node("int", "m_Stored", 1, false),
                ],
            ),
        ];

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&7_i32.to_le_bytes()); // m_Value
        bytes.extend_from_slice(&2_i32.to_le_bytes()); // registry version
        bytes.extend_from_slice(&1_i32.to_le_bytes()); // one entry
        push_registry_entry(&mut bytes, 1000, "Payload");
        // Distinctive rather than small: a near miss that reads a narrower
        // field would otherwise recover the same number from the low bytes.
        bytes.extend_from_slice(&0x0102_0304_i32.to_le_bytes()); // the stored value

        let value = read_type_tree_from_reader_with_reference_types(
            &tree,
            EndianReader::new(Cursor::new(bytes.clone()), Endian::Little),
            0,
            TypeTreeReadLimits::default(),
            &references,
        )
        .unwrap();

        let TypeValue::Object(fields) = &value else {
            panic!("root is {value:?}")
        };
        assert_eq!(fields[0].value, TypeValue::Signed(7));
        let TypeValue::Object(registry) = &fields[1].value else {
            panic!("registry is {:?}", fields[1].value)
        };
        assert_eq!(registry[0].value, TypeValue::Signed(2));
        let TypeValue::Array(entries) = &registry[1].value else {
            panic!("RefIds is {:?}", registry[1].value)
        };
        let TypeValue::Object(entry) = &entries[0] else {
            panic!("entry is {:?}", entries[0])
        };
        assert_eq!(entry[0].value, TypeValue::Signed(1000));
        // The stored value is laid out by the reference type, not by anything
        // in the object's own tree.
        assert_eq!(entry[2].name, "data");
        assert_eq!(
            entry[2].value,
            TypeValue::Object(vec![TypeField {
                name: "m_Stored".to_owned(),
                value: TypeValue::Signed(0x0102_0304),
            }])
        );

        let projected = read_type_tree_root_field_from_reader_with_reference_types(
            &tree,
            EndianReader::new(Cursor::new(bytes), Endian::Little),
            0,
            TypeTreeReadLimits::default(),
            &references,
            "m_Value",
        )
        .unwrap();
        assert_eq!(projected, Some(TypeValue::Signed(7)));
    }

    #[test]
    fn a_null_registry_entry_stores_nothing_and_an_undeclared_one_is_declined() {
        let mut nodes = vec![node("Root", "Base", 0, false)];
        nodes.extend(registry_nodes(1));
        let tree = TypeTree {
            nodes,
            string_buffer: Vec::new(),
        };
        let references = [reference_type(
            "Payload",
            vec![
                node("Payload", "Base", 0, true),
                node("int", "m_Stored", 1, false),
            ],
        )];

        // An entry naming no class is Unity's null reference: it occupies its
        // rid and identity and nothing more, and the read has to end exactly
        // at the end of the object to prove that.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_i32.to_le_bytes());
        bytes.extend_from_slice(&1_i32.to_le_bytes());
        push_registry_entry(&mut bytes, -2, "");
        let value = read_type_tree_from_reader_with_reference_types(
            &tree,
            EndianReader::new(Cursor::new(bytes), Endian::Little),
            0,
            TypeTreeReadLimits::default(),
            &references,
        )
        .unwrap();
        let TypeValue::Object(fields) = &value else {
            panic!("root is {value:?}")
        };
        let TypeValue::Object(registry) = &fields[0].value else {
            panic!("registry is {:?}", fields[0].value)
        };
        let TypeValue::Array(entries) = &registry[1].value else {
            panic!("RefIds is {:?}", registry[1].value)
        };
        let TypeValue::Object(entry) = &entries[0] else {
            panic!("entry is {:?}", entries[0])
        };
        assert_eq!(entry.len(), 2, "a null entry stores no data field");

        // A named type the file never declared cannot be stepped over: its
        // length is only known from a layout that is not there.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_i32.to_le_bytes());
        bytes.extend_from_slice(&1_i32.to_le_bytes());
        push_registry_entry(&mut bytes, 5, "Absent");
        bytes.extend_from_slice(&0_i32.to_le_bytes());
        let error = read_type_tree_from_reader_with_reference_types(
            &tree,
            EndianReader::new(Cursor::new(bytes), Endian::Little),
            0,
            TypeTreeReadLimits::default(),
            &references,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("which the file does not declare"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("namespace 4 bytes, class 6 bytes, assembly 8 bytes"),
            "{error}"
        );
        assert!(
            !error.to_string().contains("Absent"),
            "the error must not copy an asset-controlled type name: {error}"
        );
    }

    #[test]
    fn only_the_outermost_registry_declaration_is_read() {
        // A reference type's own tree can declare a registry; Unity still
        // wrote one, at the outermost level. Reading the inner declaration
        // would consume four bytes that belong to nothing.
        let mut nodes = vec![node("Root", "Base", 0, false)];
        nodes.extend(registry_nodes(1));
        let tree = TypeTree {
            nodes,
            string_buffer: Vec::new(),
        };
        let mut inner = vec![
            node("Payload", "Base", 0, true),
            node("int", "m_Stored", 1, false),
        ];
        inner.extend(registry_nodes(1));
        let references = [reference_type("Payload", inner)];

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2_i32.to_le_bytes());
        bytes.extend_from_slice(&1_i32.to_le_bytes());
        push_registry_entry(&mut bytes, 1000, "Payload");
        bytes.extend_from_slice(&99_i32.to_le_bytes());

        let value = read_type_tree_from_reader_with_reference_types(
            &tree,
            EndianReader::new(Cursor::new(bytes), Endian::Little),
            0,
            TypeTreeReadLimits::default(),
            &references,
        )
        .unwrap();
        let TypeValue::Object(fields) = &value else {
            panic!("root is {value:?}")
        };
        let TypeValue::Object(registry) = &fields[0].value else {
            panic!("registry is {:?}", fields[0].value)
        };
        let TypeValue::Array(entries) = &registry[1].value else {
            panic!("RefIds is {:?}", registry[1].value)
        };
        let TypeValue::Object(entry) = &entries[0] else {
            panic!("entry is {:?}", entries[0])
        };
        let TypeValue::Object(stored) = &entry[2].value else {
            panic!("stored is {:?}", entry[2].value)
        };
        assert_eq!(stored.len(), 1, "the inner registry was read: {stored:?}");
        assert_eq!(stored[0].value, TypeValue::Signed(99));
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
