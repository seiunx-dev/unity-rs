//! External, non-executing schemas for stripped `MonoBehaviour` objects.
//!
//! Providers return a complete Unity `TypeTree` for the serialized object.
//! The Core never opens or executes the named managed assembly.

use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;

use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::endian::{Endian, EndianReader};
use crate::json::write_type_value_json;
use crate::loader::AssetCollection;
use crate::monobehaviour::{
    MONO_BEHAVIOUR_CLASS_ID, MONO_SCRIPT_CLASS_ID, MonoBehaviourReadLimits, MonoScript,
    read_mono_behaviour, read_mono_script,
};
use crate::scene::resolve_object_reference;
use crate::serialized::{ObjectReference, TypeTree, TypeTreeNode};
use crate::type_tree::{
    TypeValue, read_type_tree_from_reader_with_reference_types, validate_tree_shape,
};
use crate::unity_version::UnityVersion;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct MonoBehaviourSchemaIdentity<'a> {
    pub unity_version: &'a str,
    pub assembly_name: &'a str,
    pub namespace: &'a str,
    pub class_name: &'a str,
}

/// Supplies complete object schemas reconstructed by a trusted external tool.
/// Implementations must not execute asset-controlled code during lookup.
pub trait MonoBehaviourSchemaProvider: Send + Sync {
    fn schema(&self, identity: MonoBehaviourSchemaIdentity<'_>) -> Result<Option<&TypeTree>>;

    /// Whether a schema from this provider outranks a type tree the file
    /// carries.
    ///
    /// Extraction wants the default: Unity wrote the embedded tree and it is
    /// the authority on that file. Overriding exists so a generated schema can
    /// be checked against a build that still ships trees -- reading the same
    /// object both ways and comparing is the only way to test a generator
    /// against Unity's answer rather than against itself.
    fn overrides_embedded_tree(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonoBehaviourSchemaEntry {
    pub assembly_name: String,
    pub namespace: String,
    pub class_name: String,
    /// Exact Unity generator version. `None` is a fallback for every version.
    pub unity_version: Option<String>,
    pub tree: TypeTree,
}

/// Limits for one external `MonoBehaviour` schema document.
///
/// The document is trusted layout data, but it is still caller-controlled
/// input at the Rust/CLI boundary. These limits keep parsing it from becoming
/// an unbounded allocation path before any asset is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonoBehaviourSchemaDocumentLimits {
    pub maximum_document_bytes: usize,
    pub maximum_entries: usize,
    pub maximum_nodes_per_entry: usize,
    pub maximum_total_nodes: usize,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
}

impl Default for MonoBehaviourSchemaDocumentLimits {
    fn default() -> Self {
        Self {
            maximum_document_bytes: 256 * 1024 * 1024,
            maximum_entries: 100_000,
            maximum_nodes_per_entry: 100_000,
            maximum_total_nodes: 1_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonoBehaviourSchemaRegistry {
    entries: Vec<MonoBehaviourSchemaEntry>,
    overrides_embedded_tree: bool,
}

impl MonoBehaviourSchemaRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            overrides_embedded_tree: false,
        }
    }

    /// See [`MonoBehaviourSchemaProvider::overrides_embedded_tree`].
    pub const fn set_overrides_embedded_tree(&mut self, overrides: bool) {
        self.overrides_embedded_tree = overrides;
    }

    pub fn push(&mut self, entry: MonoBehaviourSchemaEntry) -> Result<()> {
        self.entries.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!(
                "cannot grow MonoBehaviour schema registry: {error}"
            ))
        })?;
        self.entries.push(entry);
        Ok(())
    }

    /// Appends another registry's entries after this one's.
    ///
    /// Order decides ties, so schemas loaded first keep priority over later
    /// ones for the same class.
    pub fn extend(&mut self, other: Self) -> Result<()> {
        self.entries
            .try_reserve(other.entries.len())
            .map_err(|error| {
                Error::invalid_data(format!(
                    "cannot grow MonoBehaviour schema registry: {error}"
                ))
            })?;
        self.entries.extend(other.entries);
        Ok(())
    }

    #[must_use]
    pub fn entries(&self) -> &[MonoBehaviourSchemaEntry] {
        &self.entries
    }

    /// Loads schemas from the document a generator writes.
    ///
    /// The Core never opens a managed assembly, so the schemas have to arrive
    /// as data. `tools/monoschema` produces this document from a game's dummy
    /// DLLs using the managed converter; anything else that can name a class
    /// and lay out its serialized fields can produce it too.
    ///
    /// ```json
    /// {
    ///   "version": 1,
    ///   "entries": [
    ///     {
    ///       "assembly": "Assembly-CSharp.dll",
    ///       "namespace": "",
    ///       "class": "PlayerConfig",
    ///       "unity_version": "6000.3.12f1",
    ///       "nodes": [
    ///         { "level": 0, "type": "PlayerConfig", "name": "Base" },
    ///         { "level": 1, "type": "int", "name": "m_Value" }
    ///       ]
    ///     }
    ///   ]
    /// }
    /// ```
    ///
    /// `unity_version` is optional and an entry without one applies to every
    /// version. Each node needs a `level`, a `type` and a `name`; `meta_flags`
    /// carries Unity's align bit and the remaining fields default the way a
    /// reconstructed tree wants them, since a schema describes a layout rather
    /// than a region of a file.
    pub fn from_json(document: &[u8]) -> Result<Self> {
        Self::from_json_with_limits(document, MonoBehaviourSchemaDocumentLimits::default())
    }

    /// Loads one generated schema document under explicit structural and
    /// decoded-string budgets.
    pub fn from_json_with_limits(
        document: &[u8],
        limits: MonoBehaviourSchemaDocumentLimits,
    ) -> Result<Self> {
        if document.len() > limits.maximum_document_bytes {
            return Err(Error::invalid_data(format!(
                "MonoBehaviour schema document is {} bytes, exceeding limit {}",
                document.len(),
                limits.maximum_document_bytes
            )));
        }
        preflight_schema_json_strings(document, limits.maximum_string_bytes)?;
        let mut deserializer = serde_json::Deserializer::from_slice(document);
        let registry = SchemaDocumentSeed { limits }
            .deserialize(&mut deserializer)
            .map_err(|error| {
                Error::invalid_data(format!(
                    "MonoBehaviour schema document is not JSON: {error}"
                ))
            })?;
        deserializer.end().map_err(|error| {
            Error::invalid_data(format!(
                "MonoBehaviour schema document is not JSON: {error}"
            ))
        })?;
        Ok(registry)
    }
}

#[derive(Debug, Default)]
struct SchemaDocumentBudget {
    nodes: usize,
    string_bytes: usize,
}

const LONGEST_SCHEMA_FIELD_NAME_BYTES: usize = "unity_version".len();

/// Bounds `serde_json`'s one-string unescape scratch before deserialization.
///
/// The retained-string limits below are enforced by the custom visitors. An
/// escaped JSON string is decoded into `serde_json`'s scratch before a visitor
/// sees it, though, so a lexical pass has to reject an oversized token first.
/// Schema field names remain accepted even when a caller deliberately sets a
/// smaller retained-value limit for a boundary test.
fn preflight_schema_json_strings(document: &[u8], maximum_string_bytes: usize) -> Result<()> {
    let maximum = maximum_string_bytes.max(LONGEST_SCHEMA_FIELD_NAME_BYTES);
    let mut cursor = 0usize;
    while cursor < document.len() {
        if document[cursor] != b'"' {
            cursor += 1;
            continue;
        }
        let Some((next, decoded_bytes)) = scan_json_string(document, cursor + 1) else {
            // Syntax and UTF-8 diagnostics stay with serde_json. A malformed
            // string cannot reach a later large token because parsing stops at
            // this one first.
            return Ok(());
        };
        if decoded_bytes > maximum {
            return Err(Error::invalid_data(format!(
                "MonoBehaviour schema JSON string is {decoded_bytes} bytes, exceeding preflight limit {maximum}"
            )));
        }
        cursor = next;
    }
    Ok(())
}

fn scan_json_string(document: &[u8], mut cursor: usize) -> Option<(usize, usize)> {
    let mut decoded_bytes = 0usize;
    while cursor < document.len() {
        match document[cursor] {
            b'"' => return Some((cursor + 1, decoded_bytes)),
            b'\\' => {
                cursor += 1;
                let escape = *document.get(cursor)?;
                match escape {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                        decoded_bytes = decoded_bytes.checked_add(1)?;
                        cursor += 1;
                    }
                    b'u' => {
                        let first = json_hex_u16(document.get(cursor + 1..cursor + 5)?)?;
                        cursor += 5;
                        if (0xD800..=0xDBFF).contains(&first) {
                            if document.get(cursor..cursor + 2)? != b"\\u" {
                                return None;
                            }
                            let second = json_hex_u16(document.get(cursor + 2..cursor + 6)?)?;
                            if !(0xDC00..=0xDFFF).contains(&second) {
                                return None;
                            }
                            decoded_bytes = decoded_bytes.checked_add(4)?;
                            cursor += 6;
                        } else if (0xDC00..=0xDFFF).contains(&first) {
                            return None;
                        } else {
                            decoded_bytes = decoded_bytes
                                .checked_add(char::from_u32(u32::from(first))?.len_utf8())?;
                        }
                    }
                    _ => return None,
                }
            }
            byte if byte < 0x20 => return None,
            _ => {
                // For both ASCII and an already UTF-8-encoded scalar, the
                // decoded UTF-8 byte length is the raw byte length.
                decoded_bytes = decoded_bytes.checked_add(1)?;
                cursor += 1;
            }
        }
    }
    None
}

fn json_hex_u16(bytes: &[u8]) -> Option<u16> {
    if bytes.len() != 4 {
        return None;
    }
    bytes.iter().try_fold(0u16, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u16::from(byte - b'0'),
            b'a'..=b'f' => u16::from(byte - b'a' + 10),
            b'A'..=b'F' => u16::from(byte - b'A' + 10),
            _ => return None,
        };
        value.checked_mul(16)?.checked_add(digit)
    })
}

#[derive(Clone, Copy)]
enum SchemaField {
    Version,
    Entries,
    Assembly,
    Namespace,
    Class,
    UnityVersion,
    Nodes,
    Level,
    Type,
    Name,
    ByteSize,
    Index,
    TypeFlags,
    MetaFlags,
    Other,
}

impl<'de> Deserialize<'de> for SchemaField {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_identifier(SchemaFieldVisitor)
    }
}

struct SchemaFieldVisitor;

impl Visitor<'_> for SchemaFieldVisitor {
    type Value = SchemaField;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a MonoBehaviour schema field name")
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(match value {
            "version" => SchemaField::Version,
            "entries" => SchemaField::Entries,
            "assembly" => SchemaField::Assembly,
            "namespace" => SchemaField::Namespace,
            "class" => SchemaField::Class,
            "unity_version" => SchemaField::UnityVersion,
            "nodes" => SchemaField::Nodes,
            "level" => SchemaField::Level,
            "type" => SchemaField::Type,
            "name" => SchemaField::Name,
            "byte_size" => SchemaField::ByteSize,
            "index" => SchemaField::Index,
            "type_flags" => SchemaField::TypeFlags,
            "meta_flags" => SchemaField::MetaFlags,
            _ => SchemaField::Other,
        })
    }

    fn visit_borrowed_str<E>(self, value: &'_ str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(value)
    }
}

struct SchemaDocumentSeed {
    limits: MonoBehaviourSchemaDocumentLimits,
}

impl<'de> DeserializeSeed<'de> for SchemaDocumentSeed {
    type Value = MonoBehaviourSchemaRegistry;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(SchemaDocumentVisitor {
            limits: self.limits,
        })
    }
}

struct SchemaDocumentVisitor {
    limits: MonoBehaviourSchemaDocumentLimits,
}

impl<'de> Visitor<'de> for SchemaDocumentVisitor {
    type Value = MonoBehaviourSchemaRegistry;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a MonoBehaviour schema document object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut version = None;
        let mut entries = None;
        let mut budget = SchemaDocumentBudget::default();
        while let Some(field) = map.next_key::<SchemaField>()? {
            match field {
                SchemaField::Version => {
                    if version.is_some() {
                        return Err(de::Error::custom(
                            "MonoBehaviour schema document repeats version",
                        ));
                    }
                    version = Some(map.next_value_seed(SchemaVersionSeed)?);
                }
                SchemaField::Entries => {
                    if entries.is_some() {
                        return Err(de::Error::custom(
                            "MonoBehaviour schema document repeats entries",
                        ));
                    }
                    entries = Some(map.next_value_seed(SchemaEntriesSeed {
                        limits: self.limits,
                        budget: &mut budget,
                    })?);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        let version = version.flatten();
        if version != Some(1) {
            return Err(de::Error::custom(format_args!(
                "MonoBehaviour schema document declares version {version:?}, expected 1"
            )));
        }
        entries
            .ok_or_else(|| de::Error::custom("MonoBehaviour schema document has no entries array"))
    }
}

struct SchemaVersionSeed;

impl<'de> DeserializeSeed<'de> for SchemaVersionSeed {
    type Value = Option<u64>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SchemaVersionVisitor)
    }
}

struct SchemaVersionVisitor;

impl<'de> Visitor<'de> for SchemaVersionVisitor {
    type Value = Option<u64>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("schema version 1")
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(Some(value))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(u64::try_from(value).ok())
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

struct SchemaEntriesSeed<'a> {
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl<'de> DeserializeSeed<'de> for SchemaEntriesSeed<'_> {
    type Value = MonoBehaviourSchemaRegistry;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(SchemaEntriesVisitor {
            limits: self.limits,
            budget: self.budget,
        })
    }
}

struct SchemaEntriesVisitor<'a> {
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl<'de> Visitor<'de> for SchemaEntriesVisitor<'_> {
    type Value = MonoBehaviourSchemaRegistry;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the MonoBehaviour schema entries array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut registry = MonoBehaviourSchemaRegistry::new();
        let mut index = 0usize;
        loop {
            let entry = sequence.next_element_seed(SchemaEntrySeed {
                index,
                limits: self.limits,
                budget: self.budget,
            })?;
            let Some(entry) = entry else {
                break;
            };
            registry.push(entry).map_err(de::Error::custom)?;
            index = index
                .checked_add(1)
                .ok_or_else(|| de::Error::custom("MonoBehaviour schema entry count overflowed"))?;
        }
        Ok(registry)
    }
}

struct SchemaEntrySeed<'a> {
    index: usize,
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl<'de> DeserializeSeed<'de> for SchemaEntrySeed<'_> {
    type Value = MonoBehaviourSchemaEntry;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        if self.index >= self.limits.maximum_entries {
            return Err(de::Error::custom(format_args!(
                "MonoBehaviour schema entries exceed limit {}",
                self.limits.maximum_entries
            )));
        }
        deserializer.deserialize_map(SchemaEntryVisitor {
            index: self.index,
            limits: self.limits,
            budget: self.budget,
        })
    }
}

struct SchemaEntryVisitor<'a> {
    index: usize,
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl<'de> Visitor<'de> for SchemaEntryVisitor<'_> {
    type Value = MonoBehaviourSchemaEntry;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a MonoBehaviour schema entry object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut assembly_name = None;
        let mut namespace = None;
        let mut class_name = None;
        let mut unity_version = None;
        let mut has_unity_version = false;
        let mut nodes = None;
        while let Some(field) = map.next_key::<SchemaField>()? {
            match field {
                SchemaField::Assembly => {
                    reject_duplicate::<A::Error>(assembly_name.is_some(), self.index, "assembly")?;
                    assembly_name = Some(map.next_value_seed(SchemaStringSeed {
                        label: SchemaStringLabel::Entry(self.index, "assembly"),
                        limits: self.limits,
                        budget: self.budget,
                    })?);
                }
                SchemaField::Namespace => {
                    reject_duplicate::<A::Error>(namespace.is_some(), self.index, "namespace")?;
                    namespace = Some(map.next_value_seed(SchemaStringSeed {
                        label: SchemaStringLabel::Entry(self.index, "namespace"),
                        limits: self.limits,
                        budget: self.budget,
                    })?);
                }
                SchemaField::Class => {
                    reject_duplicate::<A::Error>(class_name.is_some(), self.index, "class")?;
                    class_name = Some(map.next_value_seed(SchemaStringSeed {
                        label: SchemaStringLabel::Entry(self.index, "class"),
                        limits: self.limits,
                        budget: self.budget,
                    })?);
                }
                SchemaField::UnityVersion => {
                    reject_duplicate::<A::Error>(has_unity_version, self.index, "unity_version")?;
                    has_unity_version = true;
                    let value = map.next_value_seed(SchemaStringSeed {
                        label: SchemaStringLabel::Entry(self.index, "unity_version"),
                        limits: self.limits,
                        budget: self.budget,
                    })?;
                    UnityVersion::from_str(&value).map_err(|error| {
                        de::Error::custom(format_args!(
                            "MonoBehaviour schema entry {} has invalid unity_version: {error}",
                            self.index
                        ))
                    })?;
                    unity_version = Some(value);
                }
                SchemaField::Nodes => {
                    reject_duplicate::<A::Error>(nodes.is_some(), self.index, "nodes")?;
                    nodes = Some(map.next_value_seed(SchemaNodesSeed {
                        entry: self.index,
                        limits: self.limits,
                        budget: self.budget,
                    })?);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        let nodes = nodes.ok_or_else(|| {
            de::Error::custom(format_args!(
                "MonoBehaviour schema entry {} has no nodes",
                self.index
            ))
        })?;
        if nodes.is_empty() {
            return Err(de::Error::custom(format_args!(
                "MonoBehaviour schema entry {} has an empty node list, which describes nothing",
                self.index
            )));
        }
        if nodes[0].level != 0 {
            return Err(de::Error::custom(format_args!(
                "MonoBehaviour schema entry {} starts at level {}, not 0",
                self.index, nodes[0].level
            )));
        }
        validate_tree_shape(&nodes).map_err(|error| {
            de::Error::custom(format_args!(
                "MonoBehaviour schema entry {} is not one tree: {error}",
                self.index
            ))
        })?;
        Ok(MonoBehaviourSchemaEntry {
            assembly_name: required_entry_string(assembly_name, self.index, "assembly")?,
            namespace: namespace.unwrap_or_default(),
            class_name: required_entry_string(class_name, self.index, "class")?,
            unity_version,
            tree: TypeTree {
                nodes,
                string_buffer: Vec::new(),
            },
        })
    }
}

fn reject_duplicate<E: de::Error>(
    duplicate: bool,
    entry: usize,
    field: &str,
) -> std::result::Result<(), E> {
    if duplicate {
        return Err(de::Error::custom(format_args!(
            "MonoBehaviour schema entry {entry} repeats {field}"
        )));
    }
    Ok(())
}

fn required_entry_string<E: de::Error>(
    value: Option<String>,
    entry: usize,
    field: &str,
) -> std::result::Result<String, E> {
    value.ok_or_else(|| {
        de::Error::custom(format_args!(
            "MonoBehaviour schema entry {entry} has no {field}"
        ))
    })
}

struct SchemaNodesSeed<'a> {
    entry: usize,
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl<'de> DeserializeSeed<'de> for SchemaNodesSeed<'_> {
    type Value = Vec<TypeTreeNode>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(SchemaNodesVisitor {
            entry: self.entry,
            limits: self.limits,
            budget: self.budget,
        })
    }
}

struct SchemaNodesVisitor<'a> {
    entry: usize,
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl<'de> Visitor<'de> for SchemaNodesVisitor<'_> {
    type Value = Vec<TypeTreeNode>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a MonoBehaviour schema node array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut nodes = Vec::new();
        let mut position = 0usize;
        loop {
            let node = sequence.next_element_seed(SchemaNodeSeed {
                entry: self.entry,
                position,
                limits: self.limits,
                budget: self.budget,
            })?;
            let Some(node) = node else {
                break;
            };
            nodes.try_reserve(1).map_err(|error| {
                de::Error::custom(format_args!("cannot allocate schema nodes: {error}"))
            })?;
            nodes.push(node);
            self.budget.nodes =
                self.budget.nodes.checked_add(1).ok_or_else(|| {
                    de::Error::custom("MonoBehaviour schema node count overflowed")
                })?;
            position = position.checked_add(1).ok_or_else(|| {
                de::Error::custom("MonoBehaviour schema node position overflowed")
            })?;
        }
        Ok(nodes)
    }
}

struct SchemaNodeSeed<'a> {
    entry: usize,
    position: usize,
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl<'de> DeserializeSeed<'de> for SchemaNodeSeed<'_> {
    type Value = TypeTreeNode;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        if self.position >= self.limits.maximum_nodes_per_entry {
            return Err(de::Error::custom(format_args!(
                "MonoBehaviour schema entry {} nodes exceed per-entry limit {}",
                self.entry, self.limits.maximum_nodes_per_entry
            )));
        }
        if self.budget.nodes >= self.limits.maximum_total_nodes {
            return Err(de::Error::custom(format_args!(
                "MonoBehaviour schema document nodes exceed total limit {}",
                self.limits.maximum_total_nodes
            )));
        }
        deserializer.deserialize_map(SchemaNodeVisitor {
            entry: self.entry,
            position: self.position,
            limits: self.limits,
            budget: self.budget,
        })
    }
}

struct SchemaNodeVisitor<'a> {
    entry: usize,
    position: usize,
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl<'de> Visitor<'de> for SchemaNodeVisitor<'_> {
    type Value = TypeTreeNode;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a MonoBehaviour schema node object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let fields = read_schema_node_fields(
            &mut map,
            self.entry,
            self.position,
            self.limits,
            self.budget,
        )?;
        let level = fields.level.unwrap_or(-1);
        let level = u32::try_from(level).map_err(|_| {
            de::Error::custom(format_args!(
                "MonoBehaviour schema entry {} node {} has level {level}, which is not a depth",
                self.entry, self.position
            ))
        })?;
        Ok(TypeTreeNode {
            type_name: required_node_string(fields.type_name, self.entry, self.position, "type")?,
            field_name: required_node_string(fields.field_name, self.entry, self.position, "name")?,
            byte_size: narrow_node_integer(
                fields.byte_size.unwrap_or(-1),
                self.entry,
                self.position,
                "byte_size",
            )?,
            index: narrow_node_integer(
                fields.index.unwrap_or(0),
                self.entry,
                self.position,
                "index",
            )?,
            type_flags: narrow_node_integer(
                fields.type_flags.unwrap_or(0),
                self.entry,
                self.position,
                "type_flags",
            )?,
            version: narrow_node_integer(
                fields.version.unwrap_or(1),
                self.entry,
                self.position,
                "version",
            )?,
            meta_flags: narrow_node_integer(
                fields.meta_flags.unwrap_or(0),
                self.entry,
                self.position,
                "meta_flags",
            )?,
            level,
            type_string_offset: None,
            name_string_offset: None,
            reference_type_hash: 0,
        })
    }
}

#[derive(Default)]
struct SchemaNodeFields {
    type_name: Option<String>,
    field_name: Option<String>,
    level: Option<i64>,
    byte_size: Option<i64>,
    index: Option<i64>,
    type_flags: Option<i64>,
    version: Option<i64>,
    meta_flags: Option<i64>,
}

fn read_schema_node_fields<'de, A: MapAccess<'de>>(
    map: &mut A,
    entry: usize,
    position: usize,
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &mut SchemaDocumentBudget,
) -> std::result::Result<SchemaNodeFields, A::Error> {
    let mut fields = SchemaNodeFields::default();
    while let Some(field) = map.next_key::<SchemaField>()? {
        match field {
            SchemaField::Type => {
                reject_node_duplicate::<A::Error>(
                    fields.type_name.is_some(),
                    entry,
                    position,
                    "type",
                )?;
                fields.type_name = Some(map.next_value_seed(SchemaStringSeed {
                    label: SchemaStringLabel::Node(entry, position, "type"),
                    limits,
                    budget,
                })?);
            }
            SchemaField::Name => {
                reject_node_duplicate::<A::Error>(
                    fields.field_name.is_some(),
                    entry,
                    position,
                    "name",
                )?;
                fields.field_name = Some(map.next_value_seed(SchemaStringSeed {
                    label: SchemaStringLabel::Node(entry, position, "name"),
                    limits,
                    budget,
                })?);
            }
            SchemaField::Level => {
                read_node_integer(map, &mut fields.level, entry, position, "level")?;
            }
            SchemaField::ByteSize => {
                read_node_integer(map, &mut fields.byte_size, entry, position, "byte_size")?;
            }
            SchemaField::Index => {
                read_node_integer(map, &mut fields.index, entry, position, "index")?;
            }
            SchemaField::TypeFlags => {
                read_node_integer(map, &mut fields.type_flags, entry, position, "type_flags")?;
            }
            SchemaField::Version => {
                read_node_integer(map, &mut fields.version, entry, position, "version")?;
            }
            SchemaField::MetaFlags => {
                read_node_integer(map, &mut fields.meta_flags, entry, position, "meta_flags")?;
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
            }
        }
    }
    Ok(fields)
}

fn read_node_integer<'de, A: MapAccess<'de>>(
    map: &mut A,
    destination: &mut Option<i64>,
    entry: usize,
    position: usize,
    field: &'static str,
) -> std::result::Result<(), A::Error> {
    reject_node_duplicate::<A::Error>(destination.is_some(), entry, position, field)?;
    *destination = Some(map.next_value_seed(SchemaIntegerSeed {
        entry,
        position,
        field,
    })?);
    Ok(())
}

fn reject_node_duplicate<E: de::Error>(
    duplicate: bool,
    entry: usize,
    position: usize,
    field: &str,
) -> std::result::Result<(), E> {
    if duplicate {
        return Err(de::Error::custom(format_args!(
            "MonoBehaviour schema entry {entry} node {position} repeats {field}"
        )));
    }
    Ok(())
}

fn required_node_string<E: de::Error>(
    value: Option<String>,
    entry: usize,
    position: usize,
    field: &str,
) -> std::result::Result<String, E> {
    value.ok_or_else(|| {
        de::Error::custom(format_args!(
            "MonoBehaviour schema entry {entry} node {position} has no {field}"
        ))
    })
}

fn narrow_node_integer<E: de::Error>(
    value: i64,
    entry: usize,
    position: usize,
    field: &str,
) -> std::result::Result<i32, E> {
    i32::try_from(value).map_err(|_| {
        de::Error::custom(format_args!(
            "MonoBehaviour schema entry {entry} node {position} has {field} {value}, which does not fit"
        ))
    })
}

struct SchemaIntegerSeed {
    entry: usize,
    position: usize,
    field: &'static str,
}

impl<'de> DeserializeSeed<'de> for SchemaIntegerSeed {
    type Value = i64;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SchemaIntegerVisitor {
            entry: self.entry,
            position: self.position,
            field: self.field,
        })
    }
}

struct SchemaIntegerVisitor {
    entry: usize,
    position: usize,
    field: &'static str,
}

impl SchemaIntegerVisitor {
    fn invalid<E: de::Error>(&self) -> E {
        de::Error::custom(format_args!(
            "MonoBehaviour schema entry {} node {} has a non-integer {}",
            self.entry, self.position, self.field
        ))
    }
}

impl Visitor<'_> for SchemaIntegerVisitor {
    type Value = i64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "an integer {} for MonoBehaviour schema entry {} node {}",
            self.field, self.entry, self.position
        )
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(value)
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        i64::try_from(value).map_err(|_| self.invalid())
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.invalid())
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.invalid())
    }

    fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.invalid())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.invalid())
    }
}

#[derive(Clone, Copy)]
enum SchemaStringLabel {
    Entry(usize, &'static str),
    Node(usize, usize, &'static str),
}

impl fmt::Display for SchemaStringLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Entry(entry, field) => {
                write!(formatter, "MonoBehaviour schema entry {entry} {field}")
            }
            Self::Node(entry, position, field) => write!(
                formatter,
                "MonoBehaviour schema entry {entry} node {position} {field}"
            ),
        }
    }
}

struct SchemaStringSeed<'a> {
    label: SchemaStringLabel,
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl<'de> DeserializeSeed<'de> for SchemaStringSeed<'_> {
    type Value = String;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SchemaStringVisitor {
            label: self.label,
            limits: self.limits,
            budget: self.budget,
        })
    }
}

struct SchemaStringVisitor<'a> {
    label: SchemaStringLabel,
    limits: MonoBehaviourSchemaDocumentLimits,
    budget: &'a mut SchemaDocumentBudget,
}

impl SchemaStringVisitor<'_> {
    fn not_string<E: de::Error>(&self) -> E {
        de::Error::custom(format_args!("{} is not a string", self.label))
    }

    fn validate<E: de::Error>(&self, length: usize) -> std::result::Result<usize, E> {
        if length > self.limits.maximum_string_bytes {
            return Err(de::Error::custom(format_args!(
                "{} is {length} bytes, exceeding limit {}",
                self.label, self.limits.maximum_string_bytes
            )));
        }
        let total = self
            .budget
            .string_bytes
            .checked_add(length)
            .ok_or_else(|| de::Error::custom("MonoBehaviour schema string budget overflowed"))?;
        if total > self.limits.maximum_total_string_bytes {
            return Err(de::Error::custom(format_args!(
                "MonoBehaviour schema strings total {total} bytes, exceeding limit {}",
                self.limits.maximum_total_string_bytes
            )));
        }
        Ok(total)
    }

    fn copy<E: de::Error>(self, value: &str) -> std::result::Result<String, E> {
        let total = self.validate(value.len())?;
        let mut copied = String::new();
        copied.try_reserve_exact(value.len()).map_err(|error| {
            de::Error::custom(format_args!("cannot allocate {}: {error}", self.label))
        })?;
        copied.push_str(value);
        self.budget.string_bytes = total;
        Ok(copied)
    }
}

impl<'de> Visitor<'de> for SchemaStringVisitor<'_> {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} string", self.label)
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.copy(value)
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.copy(value)
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        let total = self.validate(value.len())?;
        self.budget.string_bytes = total;
        Ok(value)
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.not_string())
    }

    fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.not_string())
    }

    fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.not_string())
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.not_string())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.not_string())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(self.not_string())
    }

    fn visit_seq<A>(self, _: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        Err(self.not_string())
    }

    fn visit_map<A>(self, _: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        Err(self.not_string())
    }
}

impl MonoBehaviourSchemaProvider for MonoBehaviourSchemaRegistry {
    fn schema(&self, identity: MonoBehaviourSchemaIdentity<'_>) -> Result<Option<&TypeTree>> {
        let mut fallback = None;
        for entry in &self.entries {
            if !assembly_names_equal(&entry.assembly_name, identity.assembly_name)
                || entry.namespace != identity.namespace
                || entry.class_name != identity.class_name
            {
                continue;
            }
            match entry.unity_version.as_deref() {
                Some(version) if version == identity.unity_version => return Ok(Some(&entry.tree)),
                None if fallback.is_none() => fallback = Some(&entry.tree),
                _ => {}
            }
        }
        Ok(fallback)
    }

    fn overrides_embedded_tree(&self) -> bool {
        self.overrides_embedded_tree
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonoBehaviourSchemaSource {
    Embedded,
    External,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMonoBehaviourValue {
    pub value: TypeValue,
    pub script: Option<MonoScript>,
    pub source: MonoBehaviourSchemaSource,
}

/// Reads an embedded tree when present, otherwise resolves `m_Script` and asks
/// the provider for a complete matching object tree.
pub fn read_mono_behaviour_value_with_provider(
    collection: &AssetCollection,
    file_index: usize,
    object_index: usize,
    provider: &dyn MonoBehaviourSchemaProvider,
    limits: MonoBehaviourReadLimits,
) -> Result<ResolvedMonoBehaviourValue> {
    let loaded = collection.serialized_files.get(file_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized file index {file_index} is out of range"
        ))
    })?;
    let object = loaded.file.objects.get(object_index).ok_or_else(|| {
        Error::invalid_data(format!(
            "serialized object index {object_index} is out of range"
        ))
    })?;
    if object.class_id != MONO_BEHAVIOUR_CLASS_ID {
        return Err(Error::invalid_data(format!(
            "object {} has class {}, expected MonoBehaviour ({MONO_BEHAVIOUR_CLASS_ID})",
            object.path_id, object.class_id
        )));
    }
    let embedded = object
        .serialized_type_index
        .and_then(|index| loaded.file.types.get(index))
        .and_then(|kind| kind.type_tree.as_ref())
        .filter(|tree| !tree.nodes.is_empty());
    if embedded.is_some() && !provider.overrides_embedded_tree() {
        return Ok(ResolvedMonoBehaviourValue {
            value: loaded
                .file
                .read_type_tree_value_with_limits(object_index, limits.type_tree)?,
            script: None,
            source: MonoBehaviourSchemaSource::Embedded,
        });
    }

    let behaviour = read_mono_behaviour(&loaded.file, object_index, limits)?;
    if behaviour.script.is_null() {
        return Err(Error::unsupported(format!(
            "MonoBehaviour {} has no embedded TypeTree and a null MonoScript reference",
            object.path_id
        )));
    }
    let reference = ObjectReference {
        file_id: behaviour.script.file_id,
        path_id: behaviour.script.path_id,
    };
    let target = resolve_object_reference(collection, file_index, reference)?.ok_or_else(|| {
        Error::unsupported(format!(
            "MonoBehaviour {} cannot resolve its MonoScript",
            object.path_id
        ))
    })?;
    if target.object.class_id != MONO_SCRIPT_CLASS_ID {
        return Err(Error::invalid_data(format!(
            "MonoBehaviour {} m_Script resolves to class {}, not MonoScript ({MONO_SCRIPT_CLASS_ID})",
            object.path_id, target.object.class_id
        )));
    }
    let script = read_mono_script(target.file, target.object_index, limits)?;
    let identity = MonoBehaviourSchemaIdentity {
        unity_version: &loaded.file.unity_version_string,
        assembly_name: &script.assembly_name,
        namespace: &script.namespace,
        class_name: &script.class_name,
    };
    let matched = provider.schema(identity)?;
    let Some(tree) = matched else {
        // Overriding means preferring the provider, not discarding a tree the
        // file already carries for a class the provider never heard of.
        if embedded.is_some() {
            return Ok(ResolvedMonoBehaviourValue {
                value: loaded
                    .file
                    .read_type_tree_value_with_limits(object_index, limits.type_tree)?,
                script: Some(script),
                source: MonoBehaviourSchemaSource::Embedded,
            });
        }
        return Err(Error::unsupported(format!(
            "no external MonoBehaviour schema matches {}::{} in {} for Unity {}",
            script.namespace,
            script.class_name,
            script.assembly_name,
            loaded.file.unity_version_string
        )));
    };
    let payload = loaded.file.object_region(object_index)?;
    let endian = if loaded.file.header.endianness == 0 {
        Endian::Little
    } else {
        Endian::Big
    };
    let value = read_type_tree_from_reader_with_reference_types(
        tree,
        EndianReader::new(payload.cursor(), endian),
        object.byte_start,
        limits.type_tree,
        // A supplied schema describes the object body; a SerializeReference
        // field in it still stores through the file's own reference types.
        &loaded.file.reference_types,
    )?;
    Ok(ResolvedMonoBehaviourValue {
        value,
        script: Some(script),
        source: MonoBehaviourSchemaSource::External,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMonoBehaviourJson {
    pub json: String,
    /// Which tree the JSON was read through. Worth reporting: a value read
    /// through a supplied schema is only as good as that schema, and a caller
    /// cannot tell the two apart from the JSON alone.
    pub source: MonoBehaviourSchemaSource,
}

pub fn read_mono_behaviour_json_with_provider(
    collection: &AssetCollection,
    file_index: usize,
    object_index: usize,
    provider: &dyn MonoBehaviourSchemaProvider,
    pretty: bool,
    limits: MonoBehaviourReadLimits,
) -> Result<ResolvedMonoBehaviourJson> {
    let resolved = read_mono_behaviour_value_with_provider(
        collection,
        file_index,
        object_index,
        provider,
        limits,
    )?;
    let mut output = BoundedJsonOutput::new(limits.maximum_json_bytes);
    let write_result = write_type_value_json(&resolved.value, &mut output, pretty);
    if output.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "MonoBehaviour JSON exceeds the {} byte limit",
            limits.maximum_json_bytes
        )));
    }
    write_result?;
    let json = String::from_utf8(output.bytes).map_err(|error| {
        Error::invalid_data(format!("MonoBehaviour JSON is not UTF-8: {error}"))
    })?;
    Ok(ResolvedMonoBehaviourJson {
        json,
        source: resolved.source,
    })
}

/// Compares assembly names the way the two sides actually spell them.
///
/// A `MonoScript` names its assembly without a suffix -- `Fwk`, `App.Runtime`
/// -- while a generator walking a directory names the file, `Fwk.dll`. Both
/// spellings mean the same assembly, so the suffix is trimmed from either side
/// before comparing; without this every schema misses and the whole document
/// silently does nothing.
fn assembly_names_equal(left: &str, right: &str) -> bool {
    trim_assembly_extension(portable_file_name(left))
        .eq_ignore_ascii_case(trim_assembly_extension(portable_file_name(right)))
}

fn trim_assembly_extension(value: &str) -> &str {
    let Some(cut) = value.len().checked_sub(4) else {
        return value;
    };
    // `get` rather than indexing: a name is caller data, and slicing four
    // bytes back can land inside a multi-byte character.
    match value.get(cut..) {
        Some(suffix) if suffix.eq_ignore_ascii_case(".dll") => &value[..cut],
        _ => value,
    }
}

fn portable_file_name(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}

struct BoundedJsonOutput {
    bytes: Vec<u8>,
    maximum: usize,
    limit_exceeded: bool,
}

impl BoundedJsonOutput {
    const fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            limit_exceeded: false,
        }
    }
}

impl Write for BoundedJsonOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(end) = self.bytes.len().checked_add(buffer.len()) else {
            self.limit_exceeded = true;
            return Err(io::Error::other("MonoBehaviour JSON size overflowed"));
        };
        if end > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::other("MonoBehaviour JSON limit exceeded"));
        }
        self.bytes.try_reserve(buffer.len()).map_err(|error| {
            io::Error::other(format!("cannot allocate MonoBehaviour JSON: {error}"))
        })?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::serialized::{SerializedFile, TypeTree, TypeTreeNode};
    use crate::source::Region;

    use super::{
        MonoBehaviourSchemaDocumentLimits, MonoBehaviourSchemaEntry, MonoBehaviourSchemaRegistry,
        MonoBehaviourSchemaSource, read_mono_behaviour_json_with_provider,
        read_mono_behaviour_value_with_provider,
    };
    use crate::monobehaviour::{
        MONO_BEHAVIOUR_CLASS_ID, MONO_SCRIPT_CLASS_ID, MonoBehaviourReadLimits,
    };

    #[test]
    fn external_full_tree_resolves_monoscript_and_reads_bounded_json() {
        let collection = fixture();
        let mut registry = MonoBehaviourSchemaRegistry::new();
        registry
            .push(MonoBehaviourSchemaEntry {
                assembly_name: "folder/assembly-csharp.DLL".to_owned(),
                namespace: "Game".to_owned(),
                class_name: "Stats".to_owned(),
                unity_version: None,
                tree: full_tree(),
            })
            .unwrap();

        let resolved = read_mono_behaviour_value_with_provider(
            &collection,
            0,
            0,
            &registry,
            MonoBehaviourReadLimits::default(),
        )
        .unwrap();
        assert_eq!(resolved.source, MonoBehaviourSchemaSource::External);
        assert_eq!(resolved.script.unwrap().class_name, "Stats");
        let json = read_mono_behaviour_json_with_provider(
            &collection,
            0,
            0,
            &registry,
            false,
            MonoBehaviourReadLimits::default(),
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&json.json).unwrap();
        assert_eq!(json["m_Name"], "Hero");
        assert_eq!(json["score"], 123);
    }

    #[test]
    fn exact_version_wins_and_missing_or_tiny_output_is_rejected() {
        let collection = fixture();
        let mut missing = MonoBehaviourSchemaRegistry::new();
        missing
            .push(MonoBehaviourSchemaEntry {
                assembly_name: "Other.dll".to_owned(),
                namespace: "Game".to_owned(),
                class_name: "Stats".to_owned(),
                unity_version: None,
                tree: full_tree(),
            })
            .unwrap();
        assert!(
            read_mono_behaviour_value_with_provider(
                &collection,
                0,
                0,
                &missing,
                MonoBehaviourReadLimits::default(),
            )
            .unwrap_err()
            .to_string()
            .contains("no external")
        );

        let mut exact = MonoBehaviourSchemaRegistry::new();
        exact
            .push(MonoBehaviourSchemaEntry {
                assembly_name: "Assembly-CSharp.dll".to_owned(),
                namespace: "Game".to_owned(),
                class_name: "Stats".to_owned(),
                unity_version: Some("2022.3.62f1".to_owned()),
                tree: full_tree(),
            })
            .unwrap();
        let limits = MonoBehaviourReadLimits {
            maximum_json_bytes: 1,
            ..MonoBehaviourReadLimits::default()
        };
        assert!(
            read_mono_behaviour_json_with_provider(&collection, 0, 0, &exact, false, limits)
                .is_err()
        );
    }

    #[test]
    fn assembly_name_matches_with_or_without_the_dll_suffix() {
        // A MonoScript in a shipped file names the assembly without a suffix
        // while a generator walking a directory names the file. Both spellings
        // have to reach the same schema, in both directions.
        for script_name in ["Assembly-CSharp", "Assembly-CSharp.dll"] {
            for schema_name in ["Assembly-CSharp", "Assembly-CSharp.dll"] {
                let collection = fixture_named(script_name);
                let mut registry = MonoBehaviourSchemaRegistry::new();
                registry
                    .push(MonoBehaviourSchemaEntry {
                        assembly_name: schema_name.to_owned(),
                        namespace: "Game".to_owned(),
                        class_name: "Stats".to_owned(),
                        unity_version: None,
                        tree: full_tree(),
                    })
                    .unwrap();
                let resolved = read_mono_behaviour_value_with_provider(
                    &collection,
                    0,
                    0,
                    &registry,
                    MonoBehaviourReadLimits::default(),
                )
                .unwrap_or_else(|error| panic!("{script_name} against {schema_name}: {error}"));
                assert_eq!(resolved.source, MonoBehaviourSchemaSource::External);
            }
        }

        // The trimming stops at a real suffix: a different assembly whose name
        // merely ends the same way must not match.
        let collection = fixture_named("Assembly-CSharp");
        let mut registry = MonoBehaviourSchemaRegistry::new();
        registry
            .push(MonoBehaviourSchemaEntry {
                assembly_name: "Other-CSharp".to_owned(),
                namespace: "Game".to_owned(),
                class_name: "Stats".to_owned(),
                unity_version: None,
                tree: full_tree(),
            })
            .unwrap();
        assert!(
            read_mono_behaviour_value_with_provider(
                &collection,
                0,
                0,
                &registry,
                MonoBehaviourReadLimits::default(),
            )
            .is_err()
        );
    }

    #[test]
    fn schema_document_reads_the_same_object_as_a_hand_built_tree() {
        let collection = fixture();
        let registry = MonoBehaviourSchemaRegistry::from_json(SCHEMA_DOCUMENT.as_bytes()).unwrap();
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(
            registry.entries()[0].unity_version.as_deref(),
            Some("2022.3.62f1")
        );
        assert_eq!(registry.entries()[0].tree.nodes, full_tree().nodes);

        let json = read_mono_behaviour_json_with_provider(
            &collection,
            0,
            0,
            &registry,
            false,
            MonoBehaviourReadLimits::default(),
        )
        .unwrap();
        assert_eq!(json.source, MonoBehaviourSchemaSource::External);
        let json: serde_json::Value = serde_json::from_str(&json.json).unwrap();
        assert_eq!(json["m_Name"], "Hero");
        assert_eq!(json["score"], 123);
    }

    #[test]
    fn schema_document_rejects_every_shape_that_would_describe_nothing() {
        for (document, expected) in [
            ("not json at all", "not JSON"),
            (r#"{"entries": []}"#, "version"),
            (r#"{"version": 2, "entries": []}"#, "version"),
            (r#"{"version": 1}"#, "no entries array"),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "nodes": []}]}"#,
                "empty node list",
            ),
            (
                r#"{"version": 1, "entries": [{"class": "C", "nodes": [{"level": 0, "type": "T", "name": "Base"}]}]}"#,
                "no assembly",
            ),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "nodes": [{"level": 0, "type": "T", "name": "Base"}]}]}"#,
                "no class",
            ),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "unity_version": 42, "nodes": [{"level": 0, "type": "T", "name": "Base"}]}]}"#,
                "unity_version is not a string",
            ),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "unity_version": "not-a-version", "nodes": [{"level": 0, "type": "T", "name": "Base"}]}]}"#,
                "invalid unity_version",
            ),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "nodes": [{"type": "T", "name": "Base"}]}]}"#,
                "not a depth",
            ),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "nodes": [{"level": 1, "type": "T", "name": "Base"}]}]}"#,
                "starts at level 1",
            ),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "nodes": [{"level": 0, "name": "Base"}]}]}"#,
                "no type",
            ),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "nodes": [{"level": 0, "type": "T"}]}]}"#,
                "no name",
            ),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "nodes": [{"level": "deep", "type": "T", "name": "Base"}]}]}"#,
                "non-integer level",
            ),
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "nodes": [{"level": 0, "type": "T", "name": "Base", "byte_size": 99999999999}]}]}"#,
                "does not fit",
            ),
            // A level jump: the document loads and every read through it fails
            // unless the shape is checked here.
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "nodes": [{"level": 0, "type": "T", "name": "Base"}, {"level": 2, "type": "int", "name": "deep"}]}]}"#,
                "not one tree",
            ),
            // Two roots in one entry describe two classes, not one.
            (
                r#"{"version": 1, "entries": [{"assembly": "A", "class": "C", "nodes": [{"level": 0, "type": "T", "name": "Base"}, {"level": 0, "type": "U", "name": "Other"}]}]}"#,
                "not one tree",
            ),
        ] {
            let error = MonoBehaviourSchemaRegistry::from_json(document.as_bytes())
                .expect_err(&format!("{document} should have been refused"))
                .to_string();
            assert!(
                error.contains(expected),
                "{document}
  expected {expected:?}, got {error:?}"
            );
        }
    }

    #[test]
    fn schema_document_enforces_input_structure_and_decoded_string_budgets() {
        let defaults = MonoBehaviourSchemaDocumentLimits::default();
        let error = MonoBehaviourSchemaRegistry::from_json_with_limits(
            SCHEMA_DOCUMENT.as_bytes(),
            MonoBehaviourSchemaDocumentLimits {
                maximum_document_bytes: SCHEMA_DOCUMENT.len() - 1,
                ..defaults
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("document is"), "{error}");

        let error = MonoBehaviourSchemaRegistry::from_json_with_limits(
            SCHEMA_DOCUMENT.as_bytes(),
            MonoBehaviourSchemaDocumentLimits {
                maximum_entries: 0,
                ..defaults
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("entries"), "{error}");

        let error = MonoBehaviourSchemaRegistry::from_json_with_limits(
            SCHEMA_DOCUMENT.as_bytes(),
            MonoBehaviourSchemaDocumentLimits {
                maximum_nodes_per_entry: 1,
                ..defaults
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("per-entry"), "{error}");

        let two_trees = br#"{"version":1,"entries":[
            {"assembly":"A","class":"C","nodes":[{"level":0,"type":"T","name":"N"}]},
            {"assembly":"B","class":"D","nodes":[{"level":0,"type":"U","name":"M"}]}
        ]}"#;
        let error = MonoBehaviourSchemaRegistry::from_json_with_limits(
            two_trees,
            MonoBehaviourSchemaDocumentLimits {
                maximum_nodes_per_entry: 1,
                maximum_total_nodes: 1,
                ..defaults
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("total limit 1"), "{error}");

        let unicode = br#"{"version":1,"entries":[{"assembly":"\u4e16","class":"C","nodes":[{"level":0,"type":"T","name":"N"}]}]}"#;
        let error = MonoBehaviourSchemaRegistry::from_json_with_limits(
            unicode,
            MonoBehaviourSchemaDocumentLimits {
                maximum_string_bytes: 2,
                ..defaults
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("3 bytes"), "{error}");

        let one_tree = br#"{"version":1,"entries":[{"assembly":"A","class":"C","nodes":[{"level":0,"type":"T","name":"N"}]}]}"#;
        let error = MonoBehaviourSchemaRegistry::from_json_with_limits(
            one_tree,
            MonoBehaviourSchemaDocumentLimits {
                maximum_total_string_bytes: 3,
                ..defaults
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("strings total 4"), "{error}");
        MonoBehaviourSchemaRegistry::from_json_with_limits(
            one_tree,
            MonoBehaviourSchemaDocumentLimits {
                maximum_total_string_bytes: 4,
                ..defaults
            },
        )
        .unwrap();

        let non_string_namespace = br#"{"version":1,"entries":[{"assembly":"A","namespace":42,"class":"C","nodes":[{"level":0,"type":"T","name":"N"}]}]}"#;
        let error = MonoBehaviourSchemaRegistry::from_json(non_string_namespace).unwrap_err();
        assert!(
            error.to_string().contains("namespace is not a string"),
            "{error}"
        );
    }

    #[test]
    fn schema_document_rejects_limits_before_materializing_later_values() {
        let defaults = MonoBehaviourSchemaDocumentLimits::default();
        // The retained string limit is deliberately zero. The entry ceiling
        // must fire before serde descends into the first entry and tries to
        // decode or retain any of its strings.
        let entry = br#"{"version":1,"entries":[{"assembly":"A","class":"C","nodes":[]}]}"#;
        let error = MonoBehaviourSchemaRegistry::from_json_with_limits(
            entry,
            MonoBehaviourSchemaDocumentLimits {
                maximum_entries: 0,
                maximum_string_bytes: 0,
                maximum_total_string_bytes: 0,
                ..defaults
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("entries exceed limit 0"),
            "{error}"
        );

        // Put nodes first so the node ceiling is likewise checked before any
        // entry or node String is decoded into the returned registry.
        let node_first = br#"{"version":1,"entries":[{"nodes":[{"level":0,"type":"T","name":"N"}],"assembly":"A","class":"C"}]}"#;
        let error = MonoBehaviourSchemaRegistry::from_json_with_limits(
            node_first,
            MonoBehaviourSchemaDocumentLimits {
                maximum_nodes_per_entry: 0,
                maximum_string_bytes: 0,
                maximum_total_string_bytes: 0,
                ..defaults
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("per-entry limit 0"), "{error}");

        // Escaped strings require serde_json scratch space. Fifteen decoded
        // UTF-8 bytes exceed max(2, longest field name=13), so the lexical
        // pass rejects this valid token before serde_json allocates that
        // scratch buffer or enters the retained-string visitor.
        let escaped = br#"{"version":1,"entries":[{"assembly":"\u4e16\u4e16\u4e16\u4e16\u4e16","class":"C","nodes":[]}]}"#;
        let error = MonoBehaviourSchemaRegistry::from_json_with_limits(
            escaped,
            MonoBehaviourSchemaDocumentLimits {
                maximum_string_bytes: 2,
                ..defaults
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("JSON string is 15 bytes, exceeding preflight limit 13"),
            "{error}"
        );
    }

    #[test]
    fn overriding_prefers_the_schema_but_keeps_a_tree_it_has_no_class_for() {
        // The fixture's object carries no embedded tree, so the interesting
        // case is the other one: a provider that overrides and holds nothing
        // must not throw away what the file already says.
        let collection = fixture();
        let mut registry = MonoBehaviourSchemaRegistry::new();
        registry.set_overrides_embedded_tree(true);
        registry
            .push(MonoBehaviourSchemaEntry {
                assembly_name: "Assembly-CSharp".to_owned(),
                namespace: "Game".to_owned(),
                class_name: "Stats".to_owned(),
                unity_version: None,
                tree: full_tree(),
            })
            .unwrap();
        let resolved = read_mono_behaviour_value_with_provider(
            &collection,
            0,
            0,
            &registry,
            MonoBehaviourReadLimits::default(),
        )
        .unwrap();
        assert_eq!(resolved.source, MonoBehaviourSchemaSource::External);

        let mut empty = MonoBehaviourSchemaRegistry::new();
        empty.set_overrides_embedded_tree(true);
        assert!(
            read_mono_behaviour_value_with_provider(
                &collection,
                0,
                0,
                &empty,
                MonoBehaviourReadLimits::default(),
            )
            .unwrap_err()
            .to_string()
            .contains("no external")
        );
    }

    #[test]
    fn registries_join_and_the_first_document_keeps_priority() {
        let mut first = MonoBehaviourSchemaRegistry::from_json(SCHEMA_DOCUMENT.as_bytes()).unwrap();
        let second = MonoBehaviourSchemaRegistry::from_json(SCHEMA_DOCUMENT.as_bytes()).unwrap();
        first.extend(second).unwrap();
        assert_eq!(first.entries().len(), 2);
        let collection = fixture();
        let json = read_mono_behaviour_json_with_provider(
            &collection,
            0,
            0,
            &first,
            false,
            MonoBehaviourReadLimits::default(),
        )
        .unwrap();
        assert_eq!(json.source, MonoBehaviourSchemaSource::External);
    }

    /// The same tree `full_tree` builds, spelled the way a generator writes it.
    const SCHEMA_DOCUMENT: &str = r#"{
        "version": 1,
        "generated_for": "2022.3.62f1",
        "entries": [{
            "assembly": "Assembly-CSharp",
            "namespace": "Game",
            "class": "Stats",
            "unity_version": "2022.3.62f1",
            "nodes": [
                {"level": 0, "type": "MonoBehaviour", "name": "Base"},
                {"level": 1, "type": "PPtr<GameObject>", "name": "m_GameObject"},
                {"level": 2, "type": "int", "name": "m_FileID"},
                {"level": 2, "type": "SInt64", "name": "m_PathID"},
                {"level": 1, "type": "UInt8", "name": "m_Enabled", "meta_flags": 16384},
                {"level": 1, "type": "PPtr<MonoScript>", "name": "m_Script"},
                {"level": 2, "type": "int", "name": "m_FileID"},
                {"level": 2, "type": "SInt64", "name": "m_PathID"},
                {"level": 1, "type": "string", "name": "m_Name"},
                {"level": 2, "type": "Array", "name": "Array"},
                {"level": 3, "type": "int", "name": "size"},
                {"level": 3, "type": "char", "name": "data"},
                {"level": 1, "type": "SInt32", "name": "score"}
            ]
        }]
    }"#;

    fn fixture() -> AssetCollection {
        fixture_named("Assembly-CSharp.dll")
    }

    fn fixture_named(assembly_name: &str) -> AssetCollection {
        let behaviour = mono_behaviour();
        let script = mono_script(assembly_name);
        let file = SerializedFile::open(Region::from_bytes(synthetic_v22(&[
            (MONO_BEHAVIOUR_CLASS_ID, 7, behaviour),
            (MONO_SCRIPT_CLASS_ID, 8, script),
        ])))
        .unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "schema.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    fn mono_behaviour() -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, 0, 1);
        output.push(1);
        align(&mut output, 4);
        push_pptr(&mut output, 0, 8);
        push_string(&mut output, "Hero");
        push_i32(&mut output, 123);
        output
    }

    fn mono_script(assembly_name: &str) -> Vec<u8> {
        let mut output = Vec::new();
        push_string(&mut output, "Stats script");
        push_i32(&mut output, 0);
        output.extend_from_slice(&[0; 16]);
        push_string(&mut output, "Stats");
        push_string(&mut output, "Game");
        push_string(&mut output, assembly_name);
        output
    }

    fn full_tree() -> TypeTree {
        TypeTree {
            nodes: vec![
                node("MonoBehaviour", "Base", 0, 0),
                node("PPtr<GameObject>", "m_GameObject", 1, 0),
                node("int", "m_FileID", 2, 0),
                node("SInt64", "m_PathID", 2, 0),
                node("UInt8", "m_Enabled", 1, 0x4000),
                node("PPtr<MonoScript>", "m_Script", 1, 0),
                node("int", "m_FileID", 2, 0),
                node("SInt64", "m_PathID", 2, 0),
                node("string", "m_Name", 1, 0),
                node("Array", "Array", 2, 0),
                node("int", "size", 3, 0),
                node("char", "data", 3, 0),
                node("SInt32", "score", 1, 0),
            ],
            string_buffer: Vec::new(),
        }
    }

    fn node(type_name: &str, field_name: &str, level: u32, meta_flags: i32) -> TypeTreeNode {
        TypeTreeNode {
            type_name: type_name.to_owned(),
            field_name: field_name.to_owned(),
            byte_size: -1,
            index: 0,
            type_flags: 0,
            version: 1,
            meta_flags,
            level,
            type_string_offset: None,
            name_string_offset: None,
            reference_type_hash: 0,
        }
    }

    fn synthetic_v22(objects: &[(i32, i64, Vec<u8>)]) -> Vec<u8> {
        let mut classes = objects.iter().map(|value| value.0).collect::<Vec<_>>();
        classes.sort_unstable();
        classes.dedup();
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        push_i32(&mut metadata, 13);
        metadata.push(0);
        push_i32(&mut metadata, i32::try_from(classes.len()).unwrap());
        for class_id in &classes {
            push_i32(&mut metadata, *class_id);
            metadata.push(0);
            metadata.extend_from_slice(&(-1_i16).to_le_bytes());
            if *class_id == MONO_BEHAVIOUR_CLASS_ID {
                metadata.extend_from_slice(&[0; 16]);
            }
            metadata.extend_from_slice(&[0; 16]);
        }
        let mut data = Vec::new();
        let mut records = Vec::new();
        for (class_id, path_id, payload) in objects {
            align(&mut data, 4);
            records.push((
                *path_id,
                i64::try_from(data.len()).unwrap(),
                u32::try_from(payload.len()).unwrap(),
                i32::try_from(classes.iter().position(|value| value == class_id).unwrap()).unwrap(),
            ));
            data.extend_from_slice(payload);
        }
        push_i32(&mut metadata, i32::try_from(records.len()).unwrap());
        for (path_id, offset, size, type_index) in records {
            align_with_base(&mut metadata, 48, 4);
            metadata.extend_from_slice(&path_id.to_le_bytes());
            metadata.extend_from_slice(&offset.to_le_bytes());
            metadata.extend_from_slice(&size.to_le_bytes());
            push_i32(&mut metadata, type_index);
        }
        for _ in 0..3 {
            push_i32(&mut metadata, 0);
        }
        metadata.push(0);
        let data_offset = (48 + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + data.len();
        let mut output = vec![0; 48];
        output[8..12].copy_from_slice(&22_u32.to_be_bytes());
        output[20..24].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
        output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        output.extend_from_slice(&metadata);
        output.resize(data_offset, 0);
        output.extend_from_slice(&data);
        output
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        push_i32(output, file_id);
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn push_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        align(output, 4);
    }

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn align(output: &mut Vec<u8>, alignment: usize) {
        while !output.len().is_multiple_of(alignment) {
            output.push(0);
        }
    }

    fn align_with_base(output: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + output.len()).is_multiple_of(alignment) {
            output.push(0);
        }
    }
}
