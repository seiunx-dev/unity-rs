//! Bounded Cubism fade-motion projection and `motion3.json` writer.

use std::io::{self, Write};

use crate::monobehaviour::MONO_BEHAVIOUR_CLASS_ID;
use crate::serialized::SerializedFile;
use crate::type_tree::{TypeTreeReadLimits, TypeValue};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CubismFadeMotionReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_curves: usize,
    pub maximum_keyframes: usize,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_output_bytes: u64,
    pub type_tree: TypeTreeReadLimits,
}

impl Default for CubismFadeMotionReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 256 * 1024 * 1024,
            maximum_curves: 1_000_000,
            maximum_keyframes: 10_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 128 * 1024 * 1024,
            maximum_output_bytes: 512 * 1024 * 1024,
            type_tree: TypeTreeReadLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubismMotionKeyframe {
    pub time: f32,
    pub value: f32,
    pub in_slope: f32,
    pub out_slope: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CubismFadeMotionCurve {
    pub parameter_id: String,
    pub fade_in_time: f32,
    pub fade_out_time: f32,
    pub keyframes: Vec<CubismMotionKeyframe>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CubismFadeMotion {
    pub path_id: i64,
    pub source_name: String,
    pub motion_name: String,
    pub fade_in_time: f32,
    pub fade_out_time: f32,
    pub motion_length: f32,
    pub curves: Vec<CubismFadeMotionCurve>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CubismMotionTargetNames {
    pub parameters: Vec<String>,
    pub parts: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MotionCurveView<'a> {
    pub target: &'a str,
    pub id: &'a str,
    pub fade_in_time: f32,
    pub fade_out_time: f32,
    pub keyframes: &'a [CubismMotionKeyframe],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MotionEventView<'a> {
    pub time: f32,
    pub value: &'a str,
}

#[derive(Debug)]
pub(crate) struct MotionDocumentView<'a> {
    pub duration: f32,
    pub fps: f32,
    pub fade_in_time: f32,
    pub fade_out_time: f32,
    pub curves: &'a [MotionCurveView<'a>],
    pub events: &'a [MotionEventView<'a>],
    pub initial_time_is_zero: bool,
}

impl CubismFadeMotion {
    pub fn write_motion3_json<W: Write>(
        &self,
        targets: &CubismMotionTargetNames,
        force_bezier: bool,
        output: &mut W,
        maximum_bytes: u64,
    ) -> Result<u64> {
        let mut curves = Vec::new();
        curves.try_reserve(self.curves.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Cubism motion curves: {error}"))
        })?;
        for curve in &self.curves {
            if curve.keyframes.is_empty() {
                continue;
            }
            curves.push(MotionCurveView {
                target: target_for(&curve.parameter_id, targets),
                id: &curve.parameter_id,
                fade_in_time: curve.fade_in_time,
                fade_out_time: curve.fade_out_time,
                keyframes: &curve.keyframes,
            });
        }
        write_motion_document(
            &MotionDocumentView {
                duration: self.motion_length,
                fps: 30.0,
                fade_in_time: self.fade_in_time,
                fade_out_time: self.fade_out_time,
                curves: &curves,
                events: &[],
                initial_time_is_zero: false,
            },
            force_bezier,
            output,
            maximum_bytes,
        )
    }
}

pub(crate) fn write_motion_document(
    document: &MotionDocumentView<'_>,
    force_bezier: bool,
    output: &mut impl Write,
    maximum_bytes: u64,
) -> Result<u64> {
    ensure_finite(document.duration, "motion duration")?;
    ensure_finite(document.fps, "motion frame rate")?;
    ensure_finite(document.fade_in_time, "motion fade-in time")?;
    ensure_finite(document.fade_out_time, "motion fade-out time")?;
    let mut projected = Vec::new();
    projected
        .try_reserve(document.curves.len())
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate projected motion curves: {error}"))
        })?;
    for curve in document.curves {
        if !curve.keyframes.is_empty() {
            projected.push(ProjectedCurve::new(
                *curve,
                force_bezier,
                document.initial_time_is_zero,
            )?);
        }
    }
    let totals = MotionTotals::from_curves(&projected)?;
    let user_data_size = document.events.iter().try_fold(0_usize, |total, event| {
        ensure_finite(event.time, "motion event time")?;
        total
            .checked_add(event.value.encode_utf16().count())
            .ok_or_else(|| Error::invalid_data("motion user-data size overflowed"))
    })?;
    let mut writer = BoundedWriter::new(output, maximum_bytes);
    writer.write_all(b"{\n  \"Version\": 3,\n  \"Meta\": {\n")?;
    write_meta(
        &mut writer,
        document,
        projected.len(),
        totals,
        user_data_size,
    )?;
    writer.write_all(b"  },\n  \"Curves\": [")?;
    for (index, curve) in projected.iter().enumerate() {
        write_curve(&mut writer, index, curve)?;
    }
    if !projected.is_empty() {
        writer.write_all(b"\n  ")?;
    }
    writer.write_all(b"],\n  \"UserData\": [")?;
    for (index, event) in document.events.iter().enumerate() {
        if index == 0 {
            writer.write_all(b"\n    { \"Time\": ")?;
        } else {
            writer.write_all(b",\n    { \"Time\": ")?;
        }
        write_number(&mut writer, event.time)?;
        writer.write_all(b", \"Value\": ")?;
        write_json_string(&mut writer, event.value)?;
        writer.write_all(b" }")?;
    }
    if !document.events.is_empty() {
        writer.write_all(b"\n  ")?;
    }
    writer.write_all(b"]\n}\n")?;
    Ok(writer.written)
}

pub fn read_cubism_fade_motion(
    file: &SerializedFile,
    object_index: usize,
    limits: CubismFadeMotionReadLimits,
) -> Result<CubismFadeMotion> {
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
            "Cubism fade-motion object size {} exceeds limit {}",
            object.byte_size, limits.maximum_object_bytes
        )));
    }
    let value = file.read_type_tree_value_with_limits(object_index, limits.type_tree)?;
    project_cubism_fade_motion(object.path_id, &value, limits)
}

pub fn project_cubism_fade_motion(
    path_id: i64,
    value: &TypeValue,
    limits: CubismFadeMotionReadLimits,
) -> Result<CubismFadeMotion> {
    let mut strings = StringBudget::new(limits);
    let source_name = strings.copy(string_field(value, "m_Name", "fade motion")?)?;
    let motion_name = strings.copy(string_field(value, "MotionName", "fade motion")?)?;
    let fade_in_time = finite_number_field(value, "FadeInTime", "fade motion")?;
    let fade_out_time = finite_number_field(value, "FadeOutTime", "fade motion")?;
    let motion_length = finite_number_field(value, "MotionLength", "fade motion")?;
    let ids = array_field(value, "ParameterIds", "fade motion")?;
    let curves = array_field(value, "ParameterCurves", "fade motion")?;
    let fade_ins = array_field(value, "ParameterFadeInTimes", "fade motion")?;
    let fade_outs = array_field(value, "ParameterFadeOutTimes", "fade motion")?;
    validate_parallel_lengths(ids.len(), curves.len(), fade_ins.len(), fade_outs.len())?;
    if ids.len() > limits.maximum_curves {
        return Err(limit_error("curves", ids.len(), limits.maximum_curves));
    }
    let mut total_keyframes = 0_usize;
    let mut projected = Vec::new();
    projected.try_reserve(ids.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate fade-motion curves: {error}"))
    })?;
    for index in 0..ids.len() {
        let parameter_id = match &ids[index] {
            TypeValue::String(value) => strings.copy(value)?,
            _ => {
                return Err(Error::invalid_data(format!(
                    "fade-motion ParameterIds[{index}] is not a string"
                )));
            }
        };
        let key_values = array_field(&curves[index], "m_Curve", "fade-motion curve")?;
        total_keyframes = charge(
            total_keyframes,
            key_values.len(),
            limits.maximum_keyframes,
            "keyframes",
        )?;
        let mut keyframes = Vec::new();
        keyframes.try_reserve(key_values.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate fade-motion keyframes: {error}"))
        })?;
        for key in key_values {
            keyframes.push(project_keyframe(key)?);
        }
        projected.push(CubismFadeMotionCurve {
            parameter_id,
            fade_in_time: finite_number(&fade_ins[index], "parameter FadeInTime")?,
            fade_out_time: finite_number(&fade_outs[index], "parameter FadeOutTime")?,
            keyframes,
        });
    }
    let motion = CubismFadeMotion {
        path_id,
        source_name,
        motion_name,
        fade_in_time,
        fade_out_time,
        motion_length,
        curves: projected,
    };
    motion.write_motion3_json(
        &CubismMotionTargetNames::default(),
        false,
        &mut io::sink(),
        limits.maximum_output_bytes,
    )?;
    Ok(motion)
}

fn project_keyframe(value: &TypeValue) -> Result<CubismMotionKeyframe> {
    let time = finite_number_field(value, "time", "motion keyframe")?;
    let key_value = finite_number_field(value, "value", "motion keyframe")?;
    let in_slope = slope_field(value, "inSlope")?;
    let out_slope = slope_field(value, "outSlope")?;
    Ok(CubismMotionKeyframe {
        time,
        value: key_value,
        in_slope,
        out_slope,
    })
}

fn slope_field(value: &TypeValue, name: &str) -> Result<f32> {
    let value = number(field(value, name, "motion keyframe")?, name)?;
    if value.is_nan() || value == f32::NEG_INFINITY {
        return Err(Error::invalid_data(format!(
            "motion keyframe {name} is not supported"
        )));
    }
    Ok(value)
}

struct ProjectedCurve<'a> {
    target: &'a str,
    id: &'a str,
    fade_in_time: f32,
    fade_out_time: f32,
    segments: Vec<f32>,
    segment_count: usize,
    point_count: usize,
}

impl<'a> ProjectedCurve<'a> {
    fn new(
        source: MotionCurveView<'a>,
        force_bezier: bool,
        initial_time_is_zero: bool,
    ) -> Result<Self> {
        let mut segments = Vec::new();
        segments
            .try_reserve(source.keyframes.len().saturating_mul(7))
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate motion segments: {error}"))
            })?;
        segments.push(if initial_time_is_zero {
            0.0
        } else {
            source.keyframes[0].time
        });
        segments.push(source.keyframes[0].value);
        let mut index = 1_usize;
        let mut segment_count = 0_usize;
        let mut point_count = 1_usize;
        while index < source.keyframes.len() {
            let current = source.keyframes[index];
            let previous = source.keyframes[index - 1];
            let next = source
                .keyframes
                .get(index + 1)
                .copied()
                .unwrap_or(CubismMotionKeyframe {
                    time: 0.0,
                    value: 0.0,
                    in_slope: 0.0,
                    out_slope: 0.0,
                });
            if is_inverse_stepped(current, previous, next) {
                segments.extend_from_slice(&[3.0, next.time, next.value]);
                index = index
                    .checked_add(2)
                    .ok_or_else(|| Error::invalid_data("motion keyframe index overflowed"))?;
                point_count = checked_add(point_count, 1, "motion points")?;
            } else if current.in_slope == f32::INFINITY {
                segments.extend_from_slice(&[2.0, current.time, current.value]);
                index += 1;
                point_count = checked_add(point_count, 1, "motion points")?;
            } else if previous.out_slope == 0.0 && current.in_slope.abs() < 0.0001 && !force_bezier
            {
                segments.extend_from_slice(&[0.0, current.time, current.value]);
                index += 1;
                point_count = checked_add(point_count, 1, "motion points")?;
            } else {
                let tangent = (current.time - previous.time) / 3.0;
                let values = [
                    1.0,
                    previous.time + tangent,
                    previous.out_slope * tangent + previous.value,
                    current.time - tangent,
                    current.value - current.in_slope * tangent,
                    current.time,
                    current.value,
                ];
                for value in values {
                    ensure_finite(value, "motion Bezier segment")?;
                }
                segments.extend_from_slice(&values);
                index += 1;
                point_count = checked_add(point_count, 3, "motion points")?;
            }
            segment_count = checked_add(segment_count, 1, "motion segments")?;
        }
        Ok(Self {
            target: source.target,
            id: source.id,
            fade_in_time: source.fade_in_time,
            fade_out_time: source.fade_out_time,
            segments,
            segment_count,
            point_count,
        })
    }
}

fn is_inverse_stepped(
    current: CubismMotionKeyframe,
    previous: CubismMotionKeyframe,
    next: CubismMotionKeyframe,
) -> bool {
    (current.time - previous.time - 0.01).abs() < 0.0001
        && next.value.to_bits() == current.value.to_bits()
}

fn target_for(id: &str, targets: &CubismMotionTargetNames) -> &'static str {
    if matches!(id, "Opacity" | "EyeBlink" | "LipSync") {
        "Model"
    } else if contains_name(&targets.parameters, id) {
        "Parameter"
    } else if contains_name(&targets.parts, id) || id.to_ascii_lowercase().contains("part") {
        "PartOpacity"
    } else {
        "Parameter"
    }
}

fn contains_name(values: &[String], target: &str) -> bool {
    values.iter().any(|value| value == target)
}

#[derive(Clone, Copy)]
struct MotionTotals {
    segments: usize,
    points: usize,
}

impl MotionTotals {
    fn from_curves(curves: &[ProjectedCurve<'_>]) -> Result<Self> {
        let mut totals = Self {
            segments: 0,
            points: 0,
        };
        for curve in curves {
            totals.segments = checked_add(totals.segments, curve.segment_count, "motion segments")?;
            totals.points = checked_add(totals.points, curve.point_count, "motion points")?;
        }
        Ok(totals)
    }
}

fn write_meta(
    writer: &mut impl Write,
    document: &MotionDocumentView<'_>,
    curve_count: usize,
    totals: MotionTotals,
    user_data_size: usize,
) -> Result<()> {
    writer.write_all(b"    \"Duration\": ")?;
    write_number(writer, document.duration)?;
    writer.write_all(b",\n    \"Fps\": ")?;
    write_number(writer, document.fps)?;
    writer.write_all(b",\n    \"Loop\": true,\n")?;
    writer.write_all(b"    \"AreBeziersRestricted\": true,\n    \"FadeInTime\": ")?;
    write_number(writer, document.fade_in_time)?;
    writer.write_all(b",\n    \"FadeOutTime\": ")?;
    write_number(writer, document.fade_out_time)?;
    writeln!(writer, ",\n    \"CurveCount\": {curve_count},")?;
    writeln!(writer, "    \"TotalSegmentCount\": {},", totals.segments)?;
    writeln!(writer, "    \"TotalPointCount\": {},", totals.points)?;
    writeln!(writer, "    \"UserDataCount\": {},", document.events.len())?;
    writeln!(writer, "    \"TotalUserDataSize\": {user_data_size}")?;
    Ok(())
}

fn write_curve(writer: &mut impl Write, index: usize, curve: &ProjectedCurve<'_>) -> Result<()> {
    if index == 0 {
        writer.write_all(b"\n    {\n")?;
    } else {
        writer.write_all(b",\n    {\n")?;
    }
    writer.write_all(b"      \"Target\": ")?;
    write_json_string(writer, curve.target)?;
    writer.write_all(b",\n      \"Id\": ")?;
    write_json_string(writer, curve.id)?;
    writer.write_all(b",\n      \"FadeInTime\": ")?;
    write_number(writer, curve.fade_in_time)?;
    writer.write_all(b",\n      \"FadeOutTime\": ")?;
    write_number(writer, curve.fade_out_time)?;
    writer.write_all(b",\n      \"Segments\": [")?;
    for (value_index, value) in curve.segments.iter().enumerate() {
        if value_index != 0 {
            writer.write_all(b", ")?;
        }
        write_segment_number(writer, *value)?;
    }
    writer.write_all(b"]\n    }")?;
    Ok(())
}

fn validate_parallel_lengths(
    ids: usize,
    curves: usize,
    fade_ins: usize,
    fade_outs: usize,
) -> Result<()> {
    if ids == curves && ids == fade_ins && ids == fade_outs {
        Ok(())
    } else {
        Err(Error::invalid_data(format!(
            "fade-motion parallel arrays differ: ids={ids}, curves={curves}, fade-ins={fade_ins}, fade-outs={fade_outs}"
        )))
    }
}

fn field<'a>(value: &'a TypeValue, name: &str, owner: &str) -> Result<&'a TypeValue> {
    let TypeValue::Object(fields) = value else {
        return Err(Error::invalid_data(format!("{owner} is not an object")));
    };
    fields
        .iter()
        .find(|candidate| candidate.name.eq_ignore_ascii_case(name))
        .map(|candidate| &candidate.value)
        .ok_or_else(|| Error::invalid_data(format!("{owner} has no {name}")))
}

fn array_field<'a>(value: &'a TypeValue, name: &str, owner: &str) -> Result<&'a [TypeValue]> {
    match field(value, name, owner)? {
        TypeValue::Array(value) => Ok(value),
        _ => Err(Error::invalid_data(format!(
            "{owner} {name} is not an array"
        ))),
    }
}

fn string_field<'a>(value: &'a TypeValue, name: &str, owner: &str) -> Result<&'a str> {
    match field(value, name, owner)? {
        TypeValue::String(value) => Ok(value),
        _ => Err(Error::invalid_data(format!(
            "{owner} {name} is not a string"
        ))),
    }
}

fn finite_number_field(value: &TypeValue, name: &str, owner: &str) -> Result<f32> {
    finite_number(field(value, name, owner)?, name)
}

fn finite_number(value: &TypeValue, field: &str) -> Result<f32> {
    let value = number(value, field)?;
    ensure_finite(value, field)?;
    Ok(value)
}

fn number(value: &TypeValue, field: &str) -> Result<f32> {
    match value {
        TypeValue::Float32(value) => Ok(*value),
        // A double here is out of spec for these documents already; the format
        // they are written in is a float format.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the Cubism documents are float documents"
        )]
        TypeValue::Float(value) => Ok(*value as f32),
        TypeValue::Signed(value) => value
            .to_string()
            .parse()
            .map_err(|error| Error::invalid_data(format!("{field} is not numeric: {error}"))),
        TypeValue::Unsigned(value) => value
            .to_string()
            .parse()
            .map_err(|error| Error::invalid_data(format!("{field} is not numeric: {error}"))),
        _ => Err(Error::invalid_data(format!("{field} is not numeric"))),
    }
}

fn ensure_finite(value: f32, field: &str) -> Result<()> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(Error::invalid_data(format!("{field} is not finite")))
    }
}

/// Writes a scalar field, which Newtonsoft serializes with its default float
/// format.
fn write_number(output: &mut impl Write, value: f32) -> Result<()> {
    ensure_finite(value, "motion JSON number")?;
    output
        .write_all(crate::live2d_number::managed_float(value).as_bytes())
        .map_err(|error| Error::invalid_data(format!("cannot write motion number: {error}")))
}

/// Writes a segment value, which the managed converter routes through its
/// `List<float>` converter and .NET's `"0.###"` instead.
///
/// Two formats in one document is not an accident of this port: the managed
/// side registers a converter that only claims `List<float>`, and `Segments` is
/// the only field of that type.
fn write_segment_number(output: &mut impl Write, value: f32) -> Result<()> {
    ensure_finite(value, "motion JSON number")?;
    output
        .write_all(crate::live2d_number::three_decimals(value).as_bytes())
        .map_err(|error| Error::invalid_data(format!("cannot write motion number: {error}")))
}

fn write_json_string(output: &mut impl Write, value: &str) -> Result<()> {
    serde_json::to_writer(output, value)
        .map_err(|error| Error::invalid_data(format!("cannot write motion string: {error}")))
}

fn charge(current: usize, amount: usize, maximum: usize, field: &str) -> Result<usize> {
    let next = checked_add(current, amount, field)?;
    if next > maximum {
        return Err(limit_error(field, next, maximum));
    }
    Ok(next)
}

fn checked_add(current: usize, amount: usize, field: &str) -> Result<usize> {
    current
        .checked_add(amount)
        .ok_or_else(|| Error::invalid_data(format!("{field} count overflowed")))
}

fn limit_error(field: &str, count: usize, maximum: usize) -> Error {
    Error::invalid_data(format!(
        "Cubism fade motion has {count} {field}, limit is {maximum}"
    ))
}

struct StringBudget {
    per_string: usize,
    total_limit: usize,
    total: usize,
}

impl StringBudget {
    const fn new(limits: CubismFadeMotionReadLimits) -> Self {
        Self {
            per_string: limits.maximum_string_bytes,
            total_limit: limits.maximum_total_string_bytes,
            total: 0,
        }
    }

    fn copy(&mut self, value: &str) -> Result<String> {
        if value.len() > self.per_string {
            return Err(Error::invalid_data(format!(
                "fade-motion string length {} exceeds limit {}",
                value.len(),
                self.per_string
            )));
        }
        self.total = checked_add(self.total, value.len(), "fade-motion string bytes")?;
        if self.total > self.total_limit {
            return Err(Error::invalid_data(format!(
                "fade-motion strings total {} bytes, limit is {}",
                self.total, self.total_limit
            )));
        }
        let mut output = String::new();
        output.try_reserve_exact(value.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate fade-motion string: {error}"))
        })?;
        output.push_str(value);
        Ok(output)
    }
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
            .map_err(|_| io::Error::other("motion JSON write length does not fit u64"))?;
        let end = self
            .written
            .checked_add(length)
            .ok_or_else(|| io::Error::other("motion JSON length overflowed"))?;
        if end > self.maximum {
            return Err(io::Error::other(format!(
                "motion JSON exceeds {} bytes",
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
    use super::{CubismFadeMotionReadLimits, CubismMotionTargetNames, project_cubism_fade_motion};
    use crate::type_tree::{TypeField, TypeValue};

    fn object(fields: Vec<(&str, TypeValue)>) -> TypeValue {
        TypeValue::Object(
            fields
                .into_iter()
                .map(|(name, value)| TypeField {
                    name: name.to_owned(),
                    value,
                })
                .collect(),
        )
    }

    fn key(time: f32, value: f32, in_slope: f32, out_slope: f32) -> TypeValue {
        object(vec![
            ("time", TypeValue::Float32(time)),
            ("value", TypeValue::Float32(value)),
            ("inSlope", TypeValue::Float32(in_slope)),
            ("outSlope", TypeValue::Float32(out_slope)),
        ])
    }

    fn motion_value() -> TypeValue {
        let curve = object(vec![(
            "m_Curve",
            TypeValue::Array(vec![
                key(0.0, 0.0, 0.0, 0.0),
                key(0.5, 1.0, 0.0, 0.0),
                key(1.0, 0.0, f32::INFINITY, 0.0),
            ]),
        )]);
        object(vec![
            ("m_Name", TypeValue::String("idle.fade.asset".to_owned())),
            ("MotionName", TypeValue::String("idle".to_owned())),
            ("FadeInTime", TypeValue::Float(0.2)),
            ("FadeOutTime", TypeValue::Float(0.3)),
            (
                "ParameterIds",
                TypeValue::Array(vec![TypeValue::String("ParamAngleX".to_owned())]),
            ),
            ("ParameterCurves", TypeValue::Array(vec![curve])),
            (
                "ParameterFadeInTimes",
                TypeValue::Array(vec![TypeValue::Float(0.4)]),
            ),
            (
                "ParameterFadeOutTimes",
                TypeValue::Array(vec![TypeValue::Float(0.5)]),
            ),
            ("MotionLength", TypeValue::Float(1.0)),
        ])
    }

    #[test]
    fn projects_linear_and_stepped_segments_and_writes_motion3() {
        let motion =
            project_cubism_fade_motion(11, &motion_value(), CubismFadeMotionReadLimits::default())
                .unwrap();
        let targets = CubismMotionTargetNames {
            parameters: vec!["ParamAngleX".to_owned()],
            parts: Vec::new(),
        };
        let mut json = Vec::new();
        let written = motion
            .write_motion3_json(&targets, false, &mut json, 64 * 1024)
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed["Meta"]["CurveCount"], 1);
        assert_eq!(parsed["Meta"]["TotalSegmentCount"], 2);
        assert_eq!(parsed["Meta"]["TotalPointCount"], 3);
        assert_eq!(parsed["Curves"][0]["Target"], "Parameter");
        // Segments print through .NET's "0.###", so an integral value has no
        // decimal point. This expectation used to read [0.0, ...] -- which was
        // this crate's own float formatting rather than the managed
        // extractor's, and only the differential could tell the two apart.
        assert_eq!(
            parsed["Curves"][0]["Segments"],
            serde_json::json!([0, 0, 0, 0.5, 1, 2, 1, 0])
        );
        assert!(
            String::from_utf8_lossy(&json).contains("\"Segments\": [0, 0, 0, 0.5"),
            "the segment list is written without trailing .0"
        );
        assert!(
            motion
                .write_motion3_json(&targets, false, &mut Vec::new(), written - 1)
                .is_err()
        );
    }

    #[test]
    fn rejects_parallel_mismatch_key_budget_and_nan() {
        let limits = CubismFadeMotionReadLimits {
            maximum_keyframes: 2,
            ..CubismFadeMotionReadLimits::default()
        };
        assert!(project_cubism_fade_motion(1, &motion_value(), limits).is_err());

        let mut value = motion_value();
        let TypeValue::Object(fields) = &mut value else {
            unreachable!()
        };
        fields[6].value = TypeValue::Array(Vec::new());
        assert!(
            project_cubism_fade_motion(1, &value, CubismFadeMotionReadLimits::default()).is_err()
        );

        let mut value = motion_value();
        let TypeValue::Object(fields) = &mut value else {
            unreachable!()
        };
        fields[8].value = TypeValue::Float32(f32::NAN);
        assert!(
            project_cubism_fade_motion(1, &value, CubismFadeMotionReadLimits::default()).is_err()
        );
    }
}
