//! Bounded projections of embedded Cubism `MonoBehaviour` type trees.
//!
//! These readers consume only schema-described fields. They do not infer
//! managed fields from opaque object tails, so callers can use the same API
//! with embedded trees today and a future external schema provider later.

use std::io::{self, Write};

use crate::monobehaviour::MONO_BEHAVIOUR_CLASS_ID;
use crate::serialized::SerializedFile;
use crate::type_tree::{TypeTreeReadLimits, TypeValue};
use crate::{Error, Result};

/// Limits for one Cubism expression projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CubismExpressionReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_parameters: usize,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_output_bytes: u64,
    pub type_tree: TypeTreeReadLimits,
}

impl Default for CubismExpressionReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 256 * 1024 * 1024,
            maximum_parameters: 1_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 128 * 1024 * 1024,
            maximum_output_bytes: 256 * 1024 * 1024,
            type_tree: TypeTreeReadLimits::default(),
        }
    }
}

/// Cubism expression blend mode, matching the managed `BlendType` ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CubismExpressionBlend {
    Add,
    Multiply,
    Overwrite,
}

impl CubismExpressionBlend {
    const fn ordinal(self) -> u8 {
        match self {
            Self::Add => 0,
            Self::Multiply => 1,
            Self::Overwrite => 2,
        }
    }
}

/// One parameter entry in a Cubism expression.
#[derive(Debug, Clone, PartialEq)]
pub struct CubismExpressionParameter {
    pub id: String,
    pub value: f32,
    pub blend: CubismExpressionBlend,
}

/// Typed, bounded projection of a `CubismExpressionData` object.
#[derive(Debug, Clone, PartialEq)]
pub struct CubismExpression {
    pub path_id: i64,
    pub source_name: String,
    pub expression_type: String,
    pub fade_in_time: f32,
    pub fade_out_time: f32,
    pub parameters: Vec<CubismExpressionParameter>,
}

impl CubismExpression {
    /// Writes the same ordered fields used by `AssetStudio`'s `exp3.json`
    /// projection. Blend modes remain their managed numeric enum ordinals.
    pub fn write_exp3_json<W: Write>(&self, output: &mut W, maximum_bytes: u64) -> Result<u64> {
        let mut writer = BoundedWriter::new(output, maximum_bytes);
        writer.write_all(b"{\n  \"Type\": ")?;
        write_json_string(&mut writer, &self.expression_type)?;
        writer.write_all(b",\n  \"FadeInTime\": ")?;
        write_number(&mut writer, self.fade_in_time)?;
        writer.write_all(b",\n  \"FadeOutTime\": ")?;
        write_number(&mut writer, self.fade_out_time)?;
        writer.write_all(b",\n  \"Parameters\": [")?;
        for (index, parameter) in self.parameters.iter().enumerate() {
            if index == 0 {
                writer.write_all(b"\n    {")?;
            } else {
                writer.write_all(b",\n    {")?;
            }
            writer.write_all(b"\n      \"Id\": ")?;
            write_json_string(&mut writer, &parameter.id)?;
            writer.write_all(b",\n      \"Value\": ")?;
            write_number(&mut writer, parameter.value)?;
            writer.write_all(b",\n      \"Blend\": ")?;
            write!(writer, "{}", parameter.blend.ordinal())?;
            writer.write_all(b"\n    }")?;
        }
        if !self.parameters.is_empty() {
            writer.write_all(b"\n  ")?;
        }
        writer.write_all(b"]\n}")?;
        Ok(writer.written)
    }
}

/// Reads one class-114 object through its embedded `TypeTree` and projects the
/// fields used by `CubismExpressionData`.
pub fn read_cubism_expression(
    file: &SerializedFile,
    object_index: usize,
    limits: CubismExpressionReadLimits,
) -> Result<CubismExpression> {
    let object = file.objects.get(object_index).ok_or_else(|| {
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
    if object.byte_size > limits.maximum_object_bytes {
        return Err(Error::invalid_data(format!(
            "Cubism expression object size {} exceeds limit {}",
            object.byte_size, limits.maximum_object_bytes
        )));
    }
    let value = file.read_type_tree_value_with_limits(object_index, limits.type_tree)?;
    project_cubism_expression(object.path_id, &value, limits)
}

/// Projects a previously decoded Cubism expression type-tree value.
pub fn project_cubism_expression(
    path_id: i64,
    value: &TypeValue,
    limits: CubismExpressionReadLimits,
) -> Result<CubismExpression> {
    let mut strings = StringBudget::new(limits);
    let source_name = strings.copy(required_string(value, &["m_Name"], "m_Name")?)?;
    let expression_type = strings.copy(required_string(value, &["Type"], "Type")?)?;
    let fade_in_time = required_number(value, &["FadeInTime"], "FadeInTime")?;
    let fade_out_time = required_number(value, &["FadeOutTime"], "FadeOutTime")?;
    let parameters = required_array(value, &["Parameters"], "Parameters")?;
    if parameters.len() > limits.maximum_parameters {
        return Err(Error::invalid_data(format!(
            "Cubism expression has {} parameters, limit is {}",
            parameters.len(),
            limits.maximum_parameters
        )));
    }
    let mut projected = Vec::new();
    projected.try_reserve(parameters.len()).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate Cubism expression parameters: {error}"
        ))
    })?;
    for (index, parameter) in parameters.iter().enumerate() {
        let id = strings.copy(required_string(parameter, &["Id"], "parameter Id")?)?;
        let value = required_number(parameter, &["Value"], "parameter Value")?;
        let blend = required_integer(parameter, &["Blend"], "parameter Blend")?;
        let blend = match blend {
            0 => CubismExpressionBlend::Add,
            1 => CubismExpressionBlend::Multiply,
            2 => CubismExpressionBlend::Overwrite,
            _ => {
                return Err(Error::invalid_data(format!(
                    "Cubism expression parameter {index} has unknown blend ordinal {blend}"
                )));
            }
        };
        projected.push(CubismExpressionParameter { id, value, blend });
    }
    let expression = CubismExpression {
        path_id,
        source_name,
        expression_type,
        fade_in_time,
        fade_out_time,
        parameters: projected,
    };
    expression.write_exp3_json(&mut io::sink(), limits.maximum_output_bytes)?;
    Ok(expression)
}

/// Limits shared by pose and display-info schema projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CubismAuxiliaryReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_links: usize,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub type_tree: TypeTreeReadLimits,
}

impl Default for CubismAuxiliaryReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 256 * 1024 * 1024,
            maximum_links: 1_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 128 * 1024 * 1024,
            type_tree: TypeTreeReadLimits::default(),
        }
    }
}

/// Schema-described fields of one `CubismPosePart` component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CubismPosePart {
    pub path_id: i64,
    pub group_index: i32,
    pub links: Vec<String>,
}

/// Schema-described display name attached to one parameter or part object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CubismDisplayInfo {
    pub path_id: i64,
    pub name: String,
    pub display_name: Option<String>,
}

/// One control node in a generated Cubism pose group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CubismPoseNode {
    pub id: String,
    pub links: Vec<String>,
}

/// One entry in a generated Cubism display-info file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CubismDisplayEntry {
    pub id: String,
    pub name: String,
}

impl CubismDisplayInfo {
    /// `DisplayName` overrides `Name` only when it is present and non-empty.
    #[must_use]
    pub fn effective_name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.name)
    }
}

/// Reads one embedded-schema `CubismPosePart` object.
pub fn read_cubism_pose_part(
    file: &SerializedFile,
    object_index: usize,
    limits: CubismAuxiliaryReadLimits,
) -> Result<CubismPosePart> {
    let (path_id, value) = read_auxiliary_value(file, object_index, limits, "CubismPosePart")?;
    project_cubism_pose_part(path_id, &value, limits)
}

/// Projects a previously decoded `CubismPosePart` value.
pub fn project_cubism_pose_part(
    path_id: i64,
    value: &TypeValue,
    limits: CubismAuxiliaryReadLimits,
) -> Result<CubismPosePart> {
    let group_index = auxiliary_integer(value, "GroupIndex", "CubismPosePart")?;
    let group_index = i32::try_from(group_index)
        .map_err(|_| Error::invalid_data("CubismPosePart GroupIndex does not fit i32"))?;
    let links = auxiliary_array(value, "Link", "CubismPosePart")?;
    if links.len() > limits.maximum_links {
        return Err(Error::invalid_data(format!(
            "CubismPosePart has {} links, limit is {}",
            links.len(),
            limits.maximum_links
        )));
    }
    let mut budget = AuxiliaryStringBudget::new(limits);
    let mut projected = Vec::new();
    projected.try_reserve(links.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate CubismPosePart links: {error}"))
    })?;
    for (index, link) in links.iter().enumerate() {
        let TypeValue::String(link) = link else {
            return Err(Error::invalid_data(format!(
                "CubismPosePart Link[{index}] is not a string"
            )));
        };
        projected.push(budget.copy(link, "CubismPosePart link")?);
    }
    Ok(CubismPosePart {
        path_id,
        group_index,
        links: projected,
    })
}

/// Reads one embedded-schema Cubism display-info component.
pub fn read_cubism_display_info(
    file: &SerializedFile,
    object_index: usize,
    limits: CubismAuxiliaryReadLimits,
) -> Result<CubismDisplayInfo> {
    let (path_id, value) = read_auxiliary_value(file, object_index, limits, "CubismDisplayInfo")?;
    project_cubism_display_info(path_id, &value, limits)
}

/// Projects a previously decoded Cubism display-info value.
pub fn project_cubism_display_info(
    path_id: i64,
    value: &TypeValue,
    limits: CubismAuxiliaryReadLimits,
) -> Result<CubismDisplayInfo> {
    let mut budget = AuxiliaryStringBudget::new(limits);
    let name = budget.copy(
        auxiliary_string(value, "Name", "CubismDisplayInfo")?,
        "Cubism display name",
    )?;
    let display_name = auxiliary_optional_string(value, "DisplayName", "CubismDisplayInfo")?
        .map(|value| budget.copy(value, "Cubism display name"))
        .transpose()?;
    Ok(CubismDisplayInfo {
        path_id,
        name,
        display_name,
    })
}

/// Writes a deterministic Cubism `pose3.json` from already grouped nodes.
pub fn write_cubism_pose3_json<W: Write>(
    groups: &[Vec<CubismPoseNode>],
    output: &mut W,
    maximum_bytes: u64,
) -> Result<u64> {
    let mut writer = BoundedWriter::new(output, maximum_bytes);
    writer.write_all(b"{\n  \"Type\": \"Live2D Pose\",\n  \"Groups\": [")?;
    for (group_index, group) in groups.iter().enumerate() {
        if group_index == 0 {
            writer.write_all(b"\n    [")?;
        } else {
            writer.write_all(b",\n    [")?;
        }
        for (node_index, node) in group.iter().enumerate() {
            if node_index == 0 {
                writer.write_all(b"\n      {\n        \"Id\": ")?;
            } else {
                writer.write_all(b",\n      {\n        \"Id\": ")?;
            }
            write_json_string(&mut writer, &node.id)?;
            writer.write_all(b",\n        \"Link\": [")?;
            write_string_array(&mut writer, &node.links, 10)?;
            writer.write_all(b"]\n      }")?;
        }
        if !group.is_empty() {
            writer.write_all(b"\n    ")?;
        }
        writer.write_all(b"]")?;
    }
    if !groups.is_empty() {
        writer.write_all(b"\n  ")?;
    }
    writer.write_all(b"]\n}")?;
    Ok(writer.written)
}

/// Writes a deterministic Cubism `cdi3.json`.
pub fn write_cubism_cdi3_json<W: Write>(
    parameters: &[CubismDisplayEntry],
    parts: &[CubismDisplayEntry],
    output: &mut W,
    maximum_bytes: u64,
) -> Result<u64> {
    let mut writer = BoundedWriter::new(output, maximum_bytes);
    writer.write_all(b"{\n  \"Version\": 3,\n  \"Parameters\": [")?;
    write_display_entries(&mut writer, parameters, true)?;
    writer.write_all(b"],\n  \"ParameterGroups\": [],\n  \"Parts\": [")?;
    write_display_entries(&mut writer, parts, false)?;
    writer.write_all(b"]\n}")?;
    Ok(writer.written)
}

/// Writes a string array the way `Formatting.Indented` does: one value per
/// line at `indent` spaces, the closing bracket two spaces out, and `[]` for an
/// empty array.
fn write_string_array(output: &mut impl Write, values: &[String], indent: usize) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            output.write_all(b",")?;
        }
        write_newline_indent(output, indent)?;
        write_json_string(output, value)?;
    }
    if !values.is_empty() {
        write_newline_indent(output, indent - 2)?;
    }
    Ok(())
}

fn write_newline_indent(output: &mut impl Write, indent: usize) -> Result<()> {
    output.write_all(b"\n")?;
    for _ in 0..indent {
        output.write_all(b" ")?;
    }
    Ok(())
}

fn write_display_entries(
    output: &mut impl Write,
    entries: &[CubismDisplayEntry],
    parameter: bool,
) -> Result<()> {
    for (index, entry) in entries.iter().enumerate() {
        if index == 0 {
            output.write_all(b"\n    {\n      \"Id\": ")?;
        } else {
            output.write_all(b",\n    {\n      \"Id\": ")?;
        }
        write_json_string(output, &entry.id)?;
        if parameter {
            output.write_all(b",\n      \"GroupId\": \"\"")?;
        }
        output.write_all(b",\n      \"Name\": ")?;
        write_json_string(output, &entry.name)?;
        output.write_all(b"\n    }")?;
    }
    if !entries.is_empty() {
        output.write_all(b"\n  ")?;
    }
    Ok(())
}

fn read_auxiliary_value(
    file: &SerializedFile,
    object_index: usize,
    limits: CubismAuxiliaryReadLimits,
    field: &str,
) -> Result<(i64, TypeValue)> {
    let object = file.objects.get(object_index).ok_or_else(|| {
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
    if object.byte_size > limits.maximum_object_bytes {
        return Err(Error::invalid_data(format!(
            "{field} object size {} exceeds limit {}",
            object.byte_size, limits.maximum_object_bytes
        )));
    }
    let value = file.read_type_tree_value_with_limits(object_index, limits.type_tree)?;
    Ok((object.path_id, value))
}

fn auxiliary_field<'a>(
    value: &'a TypeValue,
    name: &str,
    owner: &str,
) -> Result<Option<&'a TypeValue>> {
    let TypeValue::Object(fields) = value else {
        return Err(Error::invalid_data(format!(
            "{owner} root is not an object"
        )));
    };
    Ok(fields
        .iter()
        .find(|field| field.name.eq_ignore_ascii_case(name))
        .map(|field| &field.value))
}

fn auxiliary_string<'a>(value: &'a TypeValue, name: &str, owner: &str) -> Result<&'a str> {
    match auxiliary_field(value, name, owner)? {
        Some(TypeValue::String(value)) => Ok(value),
        Some(_) => Err(Error::invalid_data(format!(
            "{owner} {name} is not a string"
        ))),
        None => Err(Error::invalid_data(format!("{owner} has no {name}"))),
    }
}

fn auxiliary_optional_string<'a>(
    value: &'a TypeValue,
    name: &str,
    owner: &str,
) -> Result<Option<&'a str>> {
    match auxiliary_field(value, name, owner)? {
        Some(TypeValue::String(value)) => Ok(Some(value)),
        Some(_) => Err(Error::invalid_data(format!(
            "{owner} {name} is not a string"
        ))),
        None => Ok(None),
    }
}

fn auxiliary_integer(value: &TypeValue, name: &str, owner: &str) -> Result<i64> {
    match auxiliary_field(value, name, owner)? {
        Some(TypeValue::Signed(value)) => Ok(*value),
        Some(TypeValue::Unsigned(value)) => i64::try_from(*value)
            .map_err(|_| Error::invalid_data(format!("{owner} {name} does not fit i64"))),
        Some(_) => Err(Error::invalid_data(format!(
            "{owner} {name} is not an integer"
        ))),
        None => Err(Error::invalid_data(format!("{owner} has no {name}"))),
    }
}

fn auxiliary_array<'a>(value: &'a TypeValue, name: &str, owner: &str) -> Result<&'a [TypeValue]> {
    match auxiliary_field(value, name, owner)? {
        Some(TypeValue::Array(value)) => Ok(value),
        Some(_) => Err(Error::invalid_data(format!(
            "{owner} {name} is not an array"
        ))),
        None => Err(Error::invalid_data(format!("{owner} has no {name}"))),
    }
}

struct AuxiliaryStringBudget {
    maximum_string_bytes: usize,
    maximum_total_string_bytes: usize,
    total: usize,
}

impl AuxiliaryStringBudget {
    const fn new(limits: CubismAuxiliaryReadLimits) -> Self {
        Self {
            maximum_string_bytes: limits.maximum_string_bytes,
            maximum_total_string_bytes: limits.maximum_total_string_bytes,
            total: 0,
        }
    }

    fn copy(&mut self, value: &str, field: &str) -> Result<String> {
        if value.len() > self.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} length {} exceeds limit {}",
                value.len(),
                self.maximum_string_bytes
            )));
        }
        self.total = self
            .total
            .checked_add(value.len())
            .ok_or_else(|| Error::invalid_data("Cubism auxiliary string bytes overflowed"))?;
        if self.total > self.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "Cubism auxiliary strings total {} bytes, limit is {}",
                self.total, self.maximum_total_string_bytes
            )));
        }
        let mut output = String::new();
        output
            .try_reserve_exact(value.len())
            .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
        output.push_str(value);
        Ok(output)
    }
}

fn required_field<'a>(
    value: &'a TypeValue,
    names: &[&str],
    description: &str,
) -> Result<&'a TypeValue> {
    let TypeValue::Object(fields) = value else {
        return Err(Error::invalid_data(format!(
            "Cubism expression {description} owner is not an object"
        )));
    };
    fields
        .iter()
        .find(|field| {
            names
                .iter()
                .any(|name| field.name.eq_ignore_ascii_case(name))
        })
        .map(|field| &field.value)
        .ok_or_else(|| Error::invalid_data(format!("Cubism expression has no {description}")))
}

fn required_string<'a>(value: &'a TypeValue, names: &[&str], description: &str) -> Result<&'a str> {
    match required_field(value, names, description)? {
        TypeValue::String(value) => Ok(value),
        _ => Err(Error::invalid_data(format!(
            "Cubism expression {description} is not a string"
        ))),
    }
}

fn required_number(value: &TypeValue, names: &[&str], description: &str) -> Result<f32> {
    let value = match required_field(value, names, description)? {
        TypeValue::Float32(value) => *value,
        // A double is out of spec for this document, which is a float
        // document; Unity writes these fields as floats.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the Cubism documents are float documents"
        )]
        TypeValue::Float(value) => *value as f32,
        TypeValue::Signed(value) => value.to_string().parse::<f32>().map_err(|error| {
            Error::invalid_data(format!(
                "Cubism expression {description} cannot be converted to a number: {error}"
            ))
        })?,
        TypeValue::Unsigned(value) => value.to_string().parse::<f32>().map_err(|error| {
            Error::invalid_data(format!(
                "Cubism expression {description} cannot be converted to a number: {error}"
            ))
        })?,
        _ => {
            return Err(Error::invalid_data(format!(
                "Cubism expression {description} is not numeric"
            )));
        }
    };
    if !value.is_finite() {
        return Err(Error::invalid_data(format!(
            "Cubism expression {description} is not finite"
        )));
    }
    Ok(value)
}

fn required_integer(value: &TypeValue, names: &[&str], description: &str) -> Result<i64> {
    match required_field(value, names, description)? {
        TypeValue::Signed(value) => Ok(*value),
        TypeValue::Unsigned(value) => i64::try_from(*value).map_err(|_| {
            Error::invalid_data(format!(
                "Cubism expression {description} does not fit a signed integer"
            ))
        }),
        _ => Err(Error::invalid_data(format!(
            "Cubism expression {description} is not an integer"
        ))),
    }
}

fn required_array<'a>(
    value: &'a TypeValue,
    names: &[&str],
    description: &str,
) -> Result<&'a [TypeValue]> {
    match required_field(value, names, description)? {
        TypeValue::Array(value) => Ok(value),
        _ => Err(Error::invalid_data(format!(
            "Cubism expression {description} is not an array"
        ))),
    }
}

struct StringBudget {
    maximum_string_bytes: usize,
    maximum_total_string_bytes: usize,
    total: usize,
}

impl StringBudget {
    const fn new(limits: CubismExpressionReadLimits) -> Self {
        Self {
            maximum_string_bytes: limits.maximum_string_bytes,
            maximum_total_string_bytes: limits.maximum_total_string_bytes,
            total: 0,
        }
    }

    fn copy(&mut self, value: &str) -> Result<String> {
        if value.len() > self.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "Cubism expression string length {} exceeds limit {}",
                value.len(),
                self.maximum_string_bytes
            )));
        }
        self.total = self
            .total
            .checked_add(value.len())
            .ok_or_else(|| Error::invalid_data("Cubism expression string byte count overflowed"))?;
        if self.total > self.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "Cubism expression strings total {} bytes, limit is {}",
                self.total, self.maximum_total_string_bytes
            )));
        }
        let mut output = String::new();
        output.try_reserve_exact(value.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Cubism expression string: {error}"))
        })?;
        output.push_str(value);
        Ok(output)
    }
}

fn write_json_string(output: &mut impl Write, value: &str) -> Result<()> {
    serde_json::to_writer(output, value)
        .map_err(|error| Error::invalid_data(format!("cannot write Cubism JSON string: {error}")))
}

/// The managed side serializes exp3.json with no custom converter, so its
/// floats take Newtonsoft's default format rather than the `"0.###"` the
/// physics and motion documents use for some of theirs.
fn write_number(output: &mut impl Write, value: f32) -> Result<()> {
    if !value.is_finite() {
        return Err(Error::invalid_data("Cubism JSON number is not finite"));
    }
    output
        .write_all(crate::live2d_number::managed_float(value).as_bytes())
        .map_err(|error| Error::invalid_data(format!("cannot write Cubism JSON number: {error}")))
}

struct BoundedWriter<'a, W> {
    output: &'a mut W,
    maximum: u64,
    written: u64,
}

impl<'a, W> BoundedWriter<'a, W> {
    const fn new(output: &'a mut W, maximum: u64) -> Self {
        Self {
            output,
            maximum,
            written: 0,
        }
    }
}

impl<W: Write> Write for BoundedWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(buffer.len())
            .map_err(|_| io::Error::other("Cubism JSON write length does not fit u64"))?;
        let end = self
            .written
            .checked_add(length)
            .ok_or_else(|| io::Error::other("Cubism JSON length overflowed"))?;
        if end > self.maximum {
            return Err(io::Error::other(format!(
                "Cubism JSON exceeds {} bytes",
                self.maximum
            )));
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
    use crate::type_tree::{TypeField, TypeValue};

    use super::{
        CubismAuxiliaryReadLimits, CubismDisplayEntry, CubismExpressionBlend,
        CubismExpressionReadLimits, CubismPoseNode, project_cubism_display_info,
        project_cubism_expression, project_cubism_pose_part, write_cubism_cdi3_json,
        write_cubism_pose3_json,
    };

    #[test]
    fn projects_and_writes_managed_expression_contract() {
        let value = expression_value(vec![parameter("ParamAngleX", 0.5, 0)]);
        let expression =
            project_cubism_expression(17, &value, CubismExpressionReadLimits::default()).unwrap();

        assert_eq!(expression.path_id, 17);
        assert_eq!(expression.source_name, "smile.exp3");
        assert_eq!(expression.expression_type, "Live2D Expression");
        assert_eq!(expression.parameters[0].blend, CubismExpressionBlend::Add);
        let mut json = Vec::new();
        let written = expression.write_exp3_json(&mut json, 1024).unwrap();
        assert_eq!(written, u64::try_from(json.len()).unwrap());
        let parsed: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed["Type"], "Live2D Expression");
        assert_eq!(parsed["FadeInTime"], 0.5);
        assert_eq!(parsed["Parameters"][0]["Id"], "ParamAngleX");
        assert_eq!(parsed["Parameters"][0]["Blend"], 0);
        assert!(
            expression
                .write_exp3_json(&mut Vec::new(), written - 1)
                .is_err()
        );
    }

    #[test]
    fn rejects_unknown_blends_nonfinite_values_and_allocation_budgets() {
        let invalid_blend = expression_value(vec![parameter("Param", 1.0, 3)]);
        assert!(
            project_cubism_expression(1, &invalid_blend, CubismExpressionReadLimits::default())
                .is_err()
        );

        let nonfinite = expression_value(vec![parameter("Param", f64::NAN, 0)]);
        assert!(
            project_cubism_expression(1, &nonfinite, CubismExpressionReadLimits::default())
                .is_err()
        );

        let value = expression_value(vec![parameter("Param", 1.0, 0)]);
        let limits = CubismExpressionReadLimits {
            maximum_parameters: 0,
            ..CubismExpressionReadLimits::default()
        };
        assert!(project_cubism_expression(1, &value, limits).is_err());
        let limits = CubismExpressionReadLimits {
            maximum_total_string_bytes: 3,
            ..CubismExpressionReadLimits::default()
        };
        assert!(project_cubism_expression(1, &value, limits).is_err());
        let limits = CubismExpressionReadLimits {
            maximum_output_bytes: 1,
            ..CubismExpressionReadLimits::default()
        };
        assert!(project_cubism_expression(1, &value, limits).is_err());
    }

    #[test]
    fn projects_pose_links_and_display_name_override_with_limits() {
        let pose_value = TypeValue::Object(vec![
            field("GroupIndex", TypeValue::Signed(3)),
            field(
                "Link",
                TypeValue::Array(vec![
                    TypeValue::String("PartArmL".to_owned()),
                    TypeValue::String("PartArmR".to_owned()),
                ]),
            ),
        ]);
        let pose =
            project_cubism_pose_part(8, &pose_value, CubismAuxiliaryReadLimits::default()).unwrap();
        assert_eq!(pose.path_id, 8);
        assert_eq!(pose.group_index, 3);
        assert_eq!(pose.links, ["PartArmL", "PartArmR"]);
        let groups = vec![vec![CubismPoseNode {
            id: "PartBody".to_owned(),
            links: pose.links.clone(),
        }]];
        let mut pose_json = Vec::new();
        let written = write_cubism_pose3_json(&groups, &mut pose_json, 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&pose_json).unwrap();
        assert_eq!(parsed["Type"], "Live2D Pose");
        assert_eq!(parsed["Groups"][0][0]["Id"], "PartBody");
        assert_eq!(parsed["Groups"][0][0]["Link"][1], "PartArmR");
        assert!(write_cubism_pose3_json(&groups, &mut Vec::new(), written - 1).is_err());

        let display_value = TypeValue::Object(vec![
            field("Name", TypeValue::String("Angle X".to_owned())),
            field("DisplayName", TypeValue::String("Face Angle".to_owned())),
        ]);
        let display =
            project_cubism_display_info(9, &display_value, CubismAuxiliaryReadLimits::default())
                .unwrap();
        assert_eq!(display.path_id, 9);
        assert_eq!(display.effective_name(), "Face Angle");
        let entries = vec![CubismDisplayEntry {
            id: "ParamAngleX".to_owned(),
            name: display.effective_name().to_owned(),
        }];
        let mut cdi_json = Vec::new();
        let parts = vec![CubismDisplayEntry {
            id: "PartBody".to_owned(),
            name: "Body".to_owned(),
        }];
        let written = write_cubism_cdi3_json(&entries, &parts, &mut cdi_json, 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&cdi_json).unwrap();
        assert_eq!(parsed["Version"], 3);
        assert_eq!(parsed["Parameters"][0]["GroupId"], "");
        assert_eq!(parsed["Parameters"][0]["Name"], "Face Angle");
        assert_eq!(parsed["Parts"][0]["Id"], "PartBody");
        assert_eq!(parsed["Parts"][0]["Name"], "Body");
        assert!(write_cubism_cdi3_json(&entries, &parts, &mut Vec::new(), written - 1).is_err());

        let empty_override = TypeValue::Object(vec![
            field("Name", TypeValue::String("Angle X".to_owned())),
            field("DisplayName", TypeValue::String(String::new())),
        ]);
        let display =
            project_cubism_display_info(9, &empty_override, CubismAuxiliaryReadLimits::default())
                .unwrap();
        assert_eq!(display.effective_name(), "Angle X");

        let limits = CubismAuxiliaryReadLimits {
            maximum_links: 1,
            ..CubismAuxiliaryReadLimits::default()
        };
        assert!(project_cubism_pose_part(8, &pose_value, limits).is_err());
        let limits = CubismAuxiliaryReadLimits {
            maximum_total_string_bytes: 1,
            ..CubismAuxiliaryReadLimits::default()
        };
        assert!(project_cubism_display_info(9, &display_value, limits).is_err());
    }

    fn expression_value(parameters: Vec<TypeValue>) -> TypeValue {
        TypeValue::Object(vec![
            field("m_Name", TypeValue::String("smile.exp3".to_owned())),
            field("Type", TypeValue::String("Live2D Expression".to_owned())),
            field("FadeInTime", TypeValue::Float(0.5)),
            field("FadeOutTime", TypeValue::Float(0.75)),
            field("Parameters", TypeValue::Array(parameters)),
        ])
    }

    fn parameter(id: &str, value: f64, blend: i64) -> TypeValue {
        TypeValue::Object(vec![
            field("Id", TypeValue::String(id.to_owned())),
            field("Value", TypeValue::Float(value)),
            field("Blend", TypeValue::Signed(blend)),
        ])
    }

    fn field(name: &str, value: TypeValue) -> TypeField {
        TypeField {
            name: name.to_owned(),
            value,
        }
    }
}
