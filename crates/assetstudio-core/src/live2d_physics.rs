//! Bounded `CubismPhysicsController` projection and `physics3.json` writer.

use std::io::{self, Write};

use crate::monobehaviour::MONO_BEHAVIOUR_CLASS_ID;
use crate::serialized::SerializedFile;
use crate::type_tree::{TypeTreeReadLimits, TypeValue};
use crate::{Error, Result};

/// Limits for one embedded-schema Cubism physics rig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CubismPhysicsReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_sub_rigs: usize,
    pub maximum_inputs: usize,
    pub maximum_outputs: usize,
    pub maximum_particles: usize,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_output_bytes: u64,
    pub type_tree: TypeTreeReadLimits,
}

impl Default for CubismPhysicsReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 256 * 1024 * 1024,
            maximum_sub_rigs: 1_000_000,
            maximum_inputs: 1_000_000,
            maximum_outputs: 1_000_000,
            maximum_particles: 1_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 128 * 1024 * 1024,
            maximum_output_bytes: 256 * 1024 * 1024,
            type_tree: TypeTreeReadLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubismPhysicsVec2 {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CubismPhysicsSourceComponent {
    X,
    Y,
    Angle,
}

impl CubismPhysicsSourceComponent {
    const fn name(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Angle => "Angle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubismPhysicsNormalizationValue {
    pub minimum: f32,
    pub default: f32,
    pub maximum: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubismPhysicsNormalization {
    pub position: CubismPhysicsNormalizationValue,
    pub angle: CubismPhysicsNormalizationValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CubismPhysicsInput {
    pub source_id: String,
    pub weight: f32,
    pub source_component: CubismPhysicsSourceComponent,
    pub inverted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CubismPhysicsOutput {
    pub destination_id: String,
    pub particle_index: i32,
    pub scale: f32,
    pub weight: f32,
    pub source_component: CubismPhysicsSourceComponent,
    pub inverted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubismPhysicsParticle {
    pub initial_position: CubismPhysicsVec2,
    pub mobility: f32,
    pub delay: f32,
    pub acceleration: f32,
    pub radius: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CubismPhysicsSubRig {
    pub inputs: Vec<CubismPhysicsInput>,
    pub outputs: Vec<CubismPhysicsOutput>,
    pub particles: Vec<CubismPhysicsParticle>,
    pub normalization: CubismPhysicsNormalization,
}

/// Typed physics rig projected from a `CubismPhysicsController`.
#[derive(Debug, Clone, PartialEq)]
pub struct CubismPhysicsRig {
    pub path_id: i64,
    pub sub_rigs: Vec<CubismPhysicsSubRig>,
    pub gravity: CubismPhysicsVec2,
    pub wind: CubismPhysicsVec2,
    pub fps: f32,
}

impl CubismPhysicsRig {
    /// Writes the semantic equivalent of `AssetStudio`'s `physics3.json`.
    pub fn write_physics3_json<W: Write>(
        &self,
        motion_fps: f32,
        output: &mut W,
        maximum_bytes: u64,
    ) -> Result<u64> {
        let fps = if self.fps != 0.0 {
            self.fps
        } else if motion_fps != 0.0 {
            motion_fps
        } else {
            30.0
        };
        finite(fps, "Cubism physics Fps")?;
        let totals = PhysicsTotals::from_rig(self)?;
        let mut writer = BoundedWriter::new(output, maximum_bytes);
        writer.write_all(b"{\n  \"Version\": 3,\n  \"Meta\": {\n")?;
        writeln!(
            writer,
            "    \"PhysicsSettingCount\": {},",
            self.sub_rigs.len()
        )?;
        writeln!(writer, "    \"TotalInputCount\": {},", totals.inputs)?;
        writeln!(writer, "    \"TotalOutputCount\": {},", totals.outputs)?;
        writeln!(writer, "    \"VertexCount\": {},", totals.particles)?;
        writer.write_all(b"    \"Fps\": ")?;
        write_number(&mut writer, fps)?;
        writer.write_all(b",\n    \"EffectiveForces\": {\n      \"Gravity\": ")?;
        write_vec2(&mut writer, self.gravity)?;
        writer.write_all(b",\n      \"Wind\": ")?;
        write_vec2(&mut writer, self.wind)?;
        writer.write_all(b"\n    },\n    \"PhysicsDictionary\": [")?;
        for index in 0..self.sub_rigs.len() {
            if index == 0 {
                writer.write_all(b"\n      {\n")?;
            } else {
                writer.write_all(b",\n      {\n")?;
            }
            write!(
                writer,
                "        \"Id\": \"PhysicsSetting{}\",\n        \"Name\": \"Dummy{}\"\n      }}",
                index + 1,
                index + 1
            )?;
        }
        if !self.sub_rigs.is_empty() {
            writer.write_all(b"\n    ")?;
        }
        writer.write_all(b"]\n  },\n  \"PhysicsSettings\": [")?;
        for (index, rig) in self.sub_rigs.iter().enumerate() {
            write_sub_rig(&mut writer, index, rig)?;
        }
        if !self.sub_rigs.is_empty() {
            writer.write_all(b"\n  ")?;
        }
        writer.write_all(b"]\n}\n")?;
        Ok(writer.written)
    }
}

/// Reads and projects one embedded-schema `CubismPhysicsController`.
pub fn read_cubism_physics(
    file: &SerializedFile,
    object_index: usize,
    limits: CubismPhysicsReadLimits,
) -> Result<CubismPhysicsRig> {
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
            "Cubism physics object size {} exceeds limit {}",
            object.byte_size, limits.maximum_object_bytes
        )));
    }
    let value = file.read_type_tree_value_with_limits(object_index, limits.type_tree)?;
    project_cubism_physics(object.path_id, &value, limits)
}

/// Projects a previously decoded Cubism physics type-tree value.
pub fn project_cubism_physics(
    path_id: i64,
    value: &TypeValue,
    limits: CubismPhysicsReadLimits,
) -> Result<CubismPhysicsRig> {
    let rig = field(value, "_rig", "CubismPhysicsController")?;
    let sub_rigs = array_field(rig, "SubRigs", "CubismPhysicsRig")?;
    if sub_rigs.len() > limits.maximum_sub_rigs {
        return Err(limit_error(
            "sub-rigs",
            sub_rigs.len(),
            limits.maximum_sub_rigs,
        ));
    }
    let mut strings = StringBudget::new(limits);
    let mut counts = PhysicsCounts::default();
    let mut projected = Vec::new();
    projected.try_reserve(sub_rigs.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Cubism physics sub-rigs: {error}"))
    })?;
    for (index, sub_rig) in sub_rigs.iter().enumerate() {
        projected.push(project_sub_rig(
            sub_rig,
            index,
            limits,
            &mut counts,
            &mut strings,
        )?);
    }
    let result = CubismPhysicsRig {
        path_id,
        sub_rigs: projected,
        gravity: vec2_field(rig, "Gravity", "CubismPhysicsRig")?,
        wind: vec2_field(rig, "Wind", "CubismPhysicsRig")?,
        fps: number_field(rig, "Fps", "CubismPhysicsRig")?,
    };
    result.write_physics3_json(0.0, &mut io::sink(), limits.maximum_output_bytes)?;
    Ok(result)
}

fn project_sub_rig(
    value: &TypeValue,
    index: usize,
    limits: CubismPhysicsReadLimits,
    counts: &mut PhysicsCounts,
    strings: &mut StringBudget,
) -> Result<CubismPhysicsSubRig> {
    let owner = format!("Cubism physics sub-rig {index}");
    let input_values = array_field(value, "Input", &owner)?;
    let output_values = array_field(value, "Output", &owner)?;
    let particle_values = array_field(value, "Particles", &owner)?;
    counts.inputs = charge(
        counts.inputs,
        input_values.len(),
        limits.maximum_inputs,
        "inputs",
    )?;
    counts.outputs = charge(
        counts.outputs,
        output_values.len(),
        limits.maximum_outputs,
        "outputs",
    )?;
    counts.particles = charge(
        counts.particles,
        particle_values.len(),
        limits.maximum_particles,
        "particles",
    )?;
    let mut inputs = Vec::new();
    inputs.try_reserve(input_values.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Cubism physics inputs: {error}"))
    })?;
    for input in input_values {
        inputs.push(CubismPhysicsInput {
            source_id: strings.copy(string_field(input, "SourceId", "physics input")?)?,
            weight: number_field(input, "Weight", "physics input")?,
            source_component: source_component(input, "physics input")?,
            inverted: boolean_field(input, "IsInverted", "physics input")?,
        });
    }
    let mut outputs = Vec::new();
    outputs.try_reserve(output_values.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Cubism physics outputs: {error}"))
    })?;
    for output in output_values {
        let particle_index = integer_field(output, "ParticleIndex", "physics output")?;
        outputs.push(CubismPhysicsOutput {
            destination_id: strings.copy(string_field(
                output,
                "DestinationId",
                "physics output",
            )?)?,
            particle_index: i32::try_from(particle_index).map_err(|_| {
                Error::invalid_data("Cubism physics output ParticleIndex does not fit i32")
            })?,
            scale: number_field(output, "AngleScale", "physics output")?,
            weight: number_field(output, "Weight", "physics output")?,
            source_component: source_component(output, "physics output")?,
            inverted: boolean_field(output, "IsInverted", "physics output")?,
        });
    }
    let mut particles = Vec::new();
    particles
        .try_reserve(particle_values.len())
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate Cubism physics particles: {error}"))
        })?;
    for particle in particle_values {
        particles.push(CubismPhysicsParticle {
            initial_position: vec2_field(particle, "InitialPosition", "physics particle")?,
            mobility: number_field(particle, "Mobility", "physics particle")?,
            delay: number_field(particle, "Delay", "physics particle")?,
            acceleration: number_field(particle, "Acceleration", "physics particle")?,
            radius: number_field(particle, "Radius", "physics particle")?,
        });
    }
    let normalization = field(value, "Normalization", &owner)?;
    Ok(CubismPhysicsSubRig {
        inputs,
        outputs,
        particles,
        normalization: CubismPhysicsNormalization {
            position: normalization_value(normalization, "Position")?,
            angle: normalization_value(normalization, "Angle")?,
        },
    })
}

fn normalization_value(value: &TypeValue, name: &str) -> Result<CubismPhysicsNormalizationValue> {
    let value = field(value, name, "Cubism physics normalization")?;
    Ok(CubismPhysicsNormalizationValue {
        minimum: number_field(value, "Minimum", "physics normalization")?,
        default: number_field(value, "Default", "physics normalization")?,
        maximum: number_field(value, "Maximum", "physics normalization")?,
    })
}

fn write_sub_rig(writer: &mut impl Write, index: usize, rig: &CubismPhysicsSubRig) -> Result<()> {
    if index == 0 {
        writer.write_all(b"\n    {\n")?;
    } else {
        writer.write_all(b",\n    {\n")?;
    }
    writeln!(writer, "      \"Id\": \"PhysicsSetting{}\",", index + 1)?;
    writer.write_all(b"      \"Input\": [")?;
    for (entry_index, input) in rig.inputs.iter().enumerate() {
        write_input(writer, entry_index, input)?;
    }
    close_array(writer, &rig.inputs)?;
    writer.write_all(b",\n      \"Output\": [")?;
    for (entry_index, output) in rig.outputs.iter().enumerate() {
        write_output(writer, entry_index, output)?;
    }
    close_array(writer, &rig.outputs)?;
    writer.write_all(b",\n      \"Vertices\": [")?;
    for (entry_index, particle) in rig.particles.iter().enumerate() {
        write_particle(writer, entry_index, *particle)?;
    }
    close_array(writer, &rig.particles)?;
    writer.write_all(b",\n      \"Normalization\": {\n        \"Position\": ")?;
    write_normalization(writer, rig.normalization.position)?;
    writer.write_all(b",\n        \"Angle\": ")?;
    write_normalization(writer, rig.normalization.angle)?;
    writer.write_all(b"\n      }\n    }")?;
    Ok(())
}

fn write_input(writer: &mut impl Write, index: usize, input: &CubismPhysicsInput) -> Result<()> {
    entry_start(writer, index)?;
    writer.write_all(b"        \"Source\": { \"Target\": \"Parameter\", \"Id\": ")?;
    write_json_string(writer, &input.source_id)?;
    writer.write_all(b" },\n        \"Weight\": ")?;
    write_number(writer, input.weight)?;
    writer.write_all(b",\n        \"Type\": ")?;
    write_json_string(writer, input.source_component.name())?;
    write!(
        writer,
        ",\n        \"Reflect\": {}\n      }}",
        input.inverted
    )?;
    Ok(())
}

fn write_output(writer: &mut impl Write, index: usize, output: &CubismPhysicsOutput) -> Result<()> {
    entry_start(writer, index)?;
    writer.write_all(b"        \"Destination\": { \"Target\": \"Parameter\", \"Id\": ")?;
    write_json_string(writer, &output.destination_id)?;
    write!(
        writer,
        " }},\n        \"VertexIndex\": {},\n        \"Scale\": ",
        output.particle_index
    )?;
    write_number(writer, output.scale)?;
    writer.write_all(b",\n        \"Weight\": ")?;
    write_number(writer, output.weight)?;
    writer.write_all(b",\n        \"Type\": ")?;
    write_json_string(writer, output.source_component.name())?;
    write!(
        writer,
        ",\n        \"Reflect\": {}\n      }}",
        output.inverted
    )?;
    Ok(())
}

fn write_particle(
    writer: &mut impl Write,
    index: usize,
    particle: CubismPhysicsParticle,
) -> Result<()> {
    entry_start(writer, index)?;
    writer.write_all(b"        \"Position\": ")?;
    write_vec2(writer, particle.initial_position)?;
    for (name, value) in [
        ("Mobility", particle.mobility),
        ("Delay", particle.delay),
        ("Acceleration", particle.acceleration),
        ("Radius", particle.radius),
    ] {
        write!(writer, ",\n        \"{name}\": ")?;
        write_number(writer, value)?;
    }
    writer.write_all(b"\n      }")?;
    Ok(())
}

fn write_normalization(
    writer: &mut impl Write,
    value: CubismPhysicsNormalizationValue,
) -> Result<()> {
    writer.write_all(b"{ \"Minimum\": ")?;
    write_number(writer, value.minimum)?;
    writer.write_all(b", \"Default\": ")?;
    write_number(writer, value.default)?;
    writer.write_all(b", \"Maximum\": ")?;
    write_number(writer, value.maximum)?;
    writer.write_all(b" }")?;
    Ok(())
}

fn write_vec2(writer: &mut impl Write, value: CubismPhysicsVec2) -> Result<()> {
    writer.write_all(b"{ \"X\": ")?;
    write_number(writer, value.x)?;
    writer.write_all(b", \"Y\": ")?;
    write_number(writer, value.y)?;
    writer.write_all(b" }")?;
    Ok(())
}

fn entry_start(writer: &mut impl Write, index: usize) -> Result<()> {
    if index == 0 {
        writer.write_all(b"\n      {\n")?;
    } else {
        writer.write_all(b",\n      {\n")?;
    }
    Ok(())
}

fn close_array<T>(writer: &mut impl Write, values: &[T]) -> Result<()> {
    if !values.is_empty() {
        writer.write_all(b"\n      ")?;
    }
    writer.write_all(b"]")?;
    Ok(())
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
        TypeValue::Array(values) => Ok(values),
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

fn integer_field(value: &TypeValue, name: &str, owner: &str) -> Result<i64> {
    match field(value, name, owner)? {
        TypeValue::Signed(value) => Ok(*value),
        TypeValue::Unsigned(value) => i64::try_from(*value)
            .map_err(|_| Error::invalid_data(format!("{owner} {name} does not fit i64"))),
        _ => Err(Error::invalid_data(format!(
            "{owner} {name} is not an integer"
        ))),
    }
}

/// Reads a numeric field, staying at the width Unity serialized.
///
/// Widening to `f64` here is numerically lossless and textually not: the
/// shortest form that round-trips a widened `0.8f` is `0.800000011920929`,
/// which is what physics3.json then carries where every other tool writes
/// `0.8`. `TypeValue` keeps the two widths apart for this reason.
fn number_field(value: &TypeValue, name: &str, owner: &str) -> Result<f32> {
    let value = match field(value, name, owner)? {
        TypeValue::Float32(value) => *value,
        #[expect(
            clippy::cast_possible_truncation,
            reason = "physics3.json is a float document; a double field is out of spec already"
        )]
        TypeValue::Float(value) => *value as f32,
        TypeValue::Signed(value) => value.to_string().parse::<f32>().map_err(|error| {
            Error::invalid_data(format!("{owner} {name} is not numeric: {error}"))
        })?,
        TypeValue::Unsigned(value) => value.to_string().parse::<f32>().map_err(|error| {
            Error::invalid_data(format!("{owner} {name} is not numeric: {error}"))
        })?,
        _ => {
            return Err(Error::invalid_data(format!(
                "{owner} {name} is not numeric"
            )));
        }
    };
    finite(value, &format!("{owner} {name}"))?;
    Ok(value)
}

fn boolean_field(value: &TypeValue, name: &str, owner: &str) -> Result<bool> {
    match field(value, name, owner)? {
        TypeValue::Boolean(value) => Ok(*value),
        TypeValue::Signed(0) | TypeValue::Unsigned(0) => Ok(false),
        TypeValue::Signed(1) | TypeValue::Unsigned(1) => Ok(true),
        _ => Err(Error::invalid_data(format!(
            "{owner} {name} is not a boolean"
        ))),
    }
}

fn vec2_field(value: &TypeValue, name: &str, owner: &str) -> Result<CubismPhysicsVec2> {
    let value = field(value, name, owner)?;
    Ok(CubismPhysicsVec2 {
        x: number_field(value, "X", name)?,
        y: number_field(value, "Y", name)?,
    })
}

fn source_component(value: &TypeValue, owner: &str) -> Result<CubismPhysicsSourceComponent> {
    match integer_field(value, "SourceComponent", owner)? {
        0 => Ok(CubismPhysicsSourceComponent::X),
        1 => Ok(CubismPhysicsSourceComponent::Y),
        2 => Ok(CubismPhysicsSourceComponent::Angle),
        ordinal => Err(Error::invalid_data(format!(
            "{owner} has unknown SourceComponent ordinal {ordinal}"
        ))),
    }
}

fn finite(value: f32, field: &str) -> Result<()> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(Error::invalid_data(format!("{field} is not finite")))
    }
}

fn write_number(output: &mut impl Write, value: f32) -> Result<()> {
    finite(value, "Cubism physics JSON number")?;
    output
        .write_all(format_physics_number(value).as_bytes())
        .map_err(|error| Error::invalid_data(format!("cannot write physics number: {error}")))
}

/// Formats a number the way the managed extractor writes physics3.json.
///
/// It serializes every float through .NET's `"0.###"`, so matching it is what
/// makes the documents comparable rather than merely equivalent. Three rules
/// fall out of that format, and none of them is what Rust's own float
/// formatting does:
///
/// * the value is first reduced to seven significant digits, .NET's legacy
///   single-precision display width;
/// * then rounded to at most three decimals, with halves going away from zero
///   rather than to even;
/// * then trailing zeros and a bare decimal point are dropped, so an integral
///   value prints without one.
///
/// The rounding works on the shortest decimal form rather than on the binary
/// value, which is visible at a half: 0.0025f is really 0.00249999994, and
/// rounding the binary value gives 0.002 where .NET gives 0.003.
///
/// A negative value keeps its sign even when it rounds to zero, which is
/// `-0.0004` printing as `-0` and not `0`.
fn format_physics_number(value: f32) -> String {
    const SIGNIFICANT: usize = 7;
    const DECIMALS: i32 = 3;

    let text = format!("{value}");
    let (negative, magnitude) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.as_str()),
    };
    let (whole, fraction) = match magnitude.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (magnitude, ""),
    };

    // Normalize to `0.d1d2... * 10^point` with a nonzero leading digit, so that
    // rounding by significant digits and by decimal place are the same
    // operation at different offsets.
    let mut digits: Vec<u8> = whole
        .bytes()
        .chain(fraction.bytes())
        .map(|byte| byte - b'0')
        .collect();
    let mut point = i32::try_from(whole.len()).expect("a formatted float is short");
    let leading = digits.iter().take_while(|digit| **digit == 0).count();
    digits.drain(..leading);
    point -= i32::try_from(leading).expect("a formatted float is short");
    while digits.last() == Some(&0) {
        digits.pop();
    }
    if digits.is_empty() {
        return if negative {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }

    round_significant(&mut digits, &mut point, SIGNIFICANT);
    let keep = point + DECIMALS;
    let keep = usize::try_from(keep.max(0)).expect("a clamped count fits");
    if keep == 0 && point + DECIMALS == 0 {
        // Exactly at the rounding position: the first digit decides whether
        // anything survives at all.
        if digits[0] >= 5 {
            digits = vec![1];
            point += 1;
        } else {
            digits.clear();
        }
    } else if point + DECIMALS < 0 {
        digits.clear();
    } else {
        round_significant(&mut digits, &mut point, keep);
    }

    render_decimal(negative, &digits, point)
}

/// Rounds `digits` to `keep` significant digits, halves away from zero.
fn round_significant(digits: &mut Vec<u8>, point: &mut i32, keep: usize) {
    if keep >= digits.len() {
        return;
    }
    let round_up = digits[keep] >= 5;
    digits.truncate(keep);
    if round_up {
        let mut index = keep;
        loop {
            if index == 0 {
                digits.insert(0, 1);
                *point += 1;
                break;
            }
            index -= 1;
            if digits[index] == 9 {
                digits[index] = 0;
            } else {
                digits[index] += 1;
                break;
            }
        }
    }
    while digits.last() == Some(&0) {
        digits.pop();
    }
}

/// Renders `0.digits * 10^point` in plain decimal, without trailing zeros.
fn render_decimal(negative: bool, digits: &[u8], point: i32) -> String {
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    if digits.is_empty() {
        output.push('0');
        return output;
    }
    if point <= 0 {
        output.push_str("0.");
        for _ in 0..-point {
            output.push('0');
        }
        for digit in digits {
            output.push(char::from(b'0' + digit));
        }
        return output;
    }

    let whole = usize::try_from(point).expect("a positive point fits");
    for index in 0..whole {
        output.push(char::from(b'0' + digits.get(index).copied().unwrap_or(0)));
    }
    if digits.len() > whole {
        output.push('.');
        for digit in &digits[whole..] {
            output.push(char::from(b'0' + digit));
        }
    }
    output
}

fn write_json_string(output: &mut impl Write, value: &str) -> Result<()> {
    serde_json::to_writer(output, value)
        .map_err(|error| Error::invalid_data(format!("cannot write physics string: {error}")))
}

fn charge(current: usize, amount: usize, maximum: usize, field: &str) -> Result<usize> {
    let next = current
        .checked_add(amount)
        .ok_or_else(|| Error::invalid_data(format!("Cubism physics {field} count overflowed")))?;
    if next > maximum {
        return Err(limit_error(field, next, maximum));
    }
    Ok(next)
}

fn limit_error(field: &str, count: usize, maximum: usize) -> Error {
    Error::invalid_data(format!(
        "Cubism physics has {count} {field}, limit is {maximum}"
    ))
}

#[derive(Default)]
struct PhysicsCounts {
    inputs: usize,
    outputs: usize,
    particles: usize,
}

struct PhysicsTotals {
    inputs: usize,
    outputs: usize,
    particles: usize,
}

impl PhysicsTotals {
    fn from_rig(rig: &CubismPhysicsRig) -> Result<Self> {
        let mut totals = Self {
            inputs: 0,
            outputs: 0,
            particles: 0,
        };
        for sub_rig in &rig.sub_rigs {
            totals.inputs = totals
                .inputs
                .checked_add(sub_rig.inputs.len())
                .ok_or_else(|| {
                    Error::invalid_data("Cubism physics total input count overflowed")
                })?;
            totals.outputs = totals
                .outputs
                .checked_add(sub_rig.outputs.len())
                .ok_or_else(|| {
                    Error::invalid_data("Cubism physics total output count overflowed")
                })?;
            totals.particles = totals
                .particles
                .checked_add(sub_rig.particles.len())
                .ok_or_else(|| Error::invalid_data("Cubism physics vertex count overflowed"))?;
        }
        Ok(totals)
    }
}

struct StringBudget {
    maximum_string_bytes: usize,
    maximum_total_string_bytes: usize,
    total: usize,
}

impl StringBudget {
    const fn new(limits: CubismPhysicsReadLimits) -> Self {
        Self {
            maximum_string_bytes: limits.maximum_string_bytes,
            maximum_total_string_bytes: limits.maximum_total_string_bytes,
            total: 0,
        }
    }

    fn copy(&mut self, value: &str) -> Result<String> {
        if value.len() > self.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "Cubism physics string length {} exceeds limit {}",
                value.len(),
                self.maximum_string_bytes
            )));
        }
        self.total = self
            .total
            .checked_add(value.len())
            .ok_or_else(|| Error::invalid_data("Cubism physics total string bytes overflowed"))?;
        if self.total > self.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "Cubism physics strings total {} bytes, limit is {}",
                self.total, self.maximum_total_string_bytes
            )));
        }
        let mut copied = String::new();
        copied.try_reserve_exact(value.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Cubism physics string: {error}"))
        })?;
        copied.push_str(value);
        Ok(copied)
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
            .map_err(|_| io::Error::other("physics JSON write length does not fit u64"))?;
        let end = self
            .written
            .checked_add(length)
            .ok_or_else(|| io::Error::other("physics JSON length overflowed"))?;
        if end > self.maximum {
            return Err(io::Error::other(format!(
                "physics JSON exceeds {} bytes",
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
    use super::{CubismPhysicsReadLimits, format_physics_number, project_cubism_physics};
    use crate::type_tree::{TypeField, TypeValue};

    /// Every case below is `float.ToString("0.###", InvariantCulture)` run on
    /// .NET 10, which is the formatter the managed extractor hands to
    /// Newtonsoft for physics3.json. The expectations are that program's
    /// output, not a reading of the documentation.
    #[test]
    fn physics_numbers_format_like_the_managed_extractor() {
        const CASES: &[(f32, &str)] = &[
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (100.0, "100"),
            (0.8, "0.8"),
            (0.95, "0.95"),
            (1.5, "1.5"),
            (0.25, "0.25"),
            // Rounding happens at three decimals, halves away from zero.
            (0.1234, "0.123"),
            (0.1235, "0.124"),
            (0.12349, "0.123"),
            (2.0005, "2.001"),
            (2.0015, "2.002"),
            (1.0005, "1.001"),
            (-2.0005, "-2.001"),
            // On the shortest decimal form, not the binary value: 0.0025f is
            // really 0.00249999994, which would round the other way.
            (0.0015, "0.002"),
            (0.0025, "0.003"),
            (0.0005, "0.001"),
            // Below half a thousandth nothing survives, but the sign does.
            (0.00049, "0"),
            (1e-5, "0"),
            (1e-10, "0"),
            (-0.0004, "-0"),
            // Seven significant digits first, which only shows above a million.
            (1234.5678, "1234.568"),
            (99999.99, "99999.99"),
            (1_000_000.06, "1000000"),
            (1_234_567.8, "1234568"),
            (12_345_678.0, "12345680"),
            (16_777_216.0, "16777220"),
            (123_456_792.0, "123456800"),
            (1e10, "10000000000"),
            (2.5, "2.5"),
            (3.5, "3.5"),
            (0.5, "0.5"),
        ];

        let wrong: Vec<String> = CASES
            .iter()
            .filter_map(|(value, expected)| {
                let actual = format_physics_number(*value);
                (actual != *expected)
                    .then(|| format!("{value:?}: expected {expected}, produced {actual}"))
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "{} case(s) differ:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }

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

    fn vec2(x: f64, y: f64) -> TypeValue {
        object(vec![("X", TypeValue::Float(x)), ("Y", TypeValue::Float(y))])
    }

    fn normalization(minimum: f64, default: f64, maximum: f64) -> TypeValue {
        object(vec![
            ("Minimum", TypeValue::Float(minimum)),
            ("Default", TypeValue::Float(default)),
            ("Maximum", TypeValue::Float(maximum)),
        ])
    }

    fn physics_value() -> TypeValue {
        let input = object(vec![
            ("SourceId", TypeValue::String("ParamAngleX".to_owned())),
            ("Weight", TypeValue::Float(80.0)),
            ("SourceComponent", TypeValue::Signed(0)),
            ("IsInverted", TypeValue::Boolean(false)),
        ]);
        let output = object(vec![
            ("DestinationId", TypeValue::String("ParamHair".to_owned())),
            ("ParticleIndex", TypeValue::Signed(1)),
            ("AngleScale", TypeValue::Float(2.5)),
            ("Weight", TypeValue::Float(90.0)),
            ("SourceComponent", TypeValue::Signed(2)),
            ("IsInverted", TypeValue::Boolean(true)),
        ]);
        let particle = object(vec![
            ("InitialPosition", vec2(0.0, 1.0)),
            ("Mobility", TypeValue::Float(0.8)),
            ("Delay", TypeValue::Float(0.2)),
            ("Acceleration", TypeValue::Float(1.0)),
            ("Radius", TypeValue::Float(10.0)),
        ]);
        let sub_rig = object(vec![
            ("Input", TypeValue::Array(vec![input])),
            ("Output", TypeValue::Array(vec![output])),
            ("Particles", TypeValue::Array(vec![particle])),
            (
                "Normalization",
                object(vec![
                    ("Position", normalization(-10.0, 0.0, 10.0)),
                    ("Angle", normalization(-30.0, 0.0, 30.0)),
                ]),
            ),
        ]);
        object(vec![(
            "_rig",
            object(vec![
                ("SubRigs", TypeValue::Array(vec![sub_rig])),
                ("Gravity", vec2(0.0, -1.0)),
                ("Wind", vec2(0.5, 0.0)),
                ("Fps", TypeValue::Float(0.0)),
            ]),
        )])
    }

    #[test]
    fn projects_and_writes_physics3_json_with_fps_fallback() {
        let rig = project_cubism_physics(17, &physics_value(), CubismPhysicsReadLimits::default())
            .unwrap();
        assert_eq!(rig.path_id, 17);
        assert_eq!(rig.sub_rigs[0].outputs[0].particle_index, 1);
        let mut json = Vec::new();
        let written = rig.write_physics3_json(60.0, &mut json, 64 * 1024).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed["Meta"]["Fps"], 60.0);
        assert_eq!(parsed["Meta"]["TotalInputCount"], 1);
        assert_eq!(parsed["PhysicsSettings"][0]["Input"][0]["Type"], "X");
        assert_eq!(parsed["PhysicsSettings"][0]["Output"][0]["Type"], "Angle");
        assert_eq!(parsed["PhysicsSettings"][0]["Vertices"][0]["Radius"], 10.0);
        assert!(
            rig.write_physics3_json(60.0, &mut Vec::new(), written - 1)
                .is_err()
        );
    }

    #[test]
    fn rejects_unknown_components_nonfinite_values_and_count_budgets() {
        let mut value = physics_value();
        let TypeValue::Object(root) = &mut value else {
            unreachable!()
        };
        let TypeValue::Object(rig) = &mut root[0].value else {
            unreachable!()
        };
        let TypeValue::Array(sub_rigs) = &mut rig[0].value else {
            unreachable!()
        };
        let TypeValue::Object(sub_rig) = &mut sub_rigs[0] else {
            unreachable!()
        };
        let TypeValue::Array(inputs) = &mut sub_rig[0].value else {
            unreachable!()
        };
        let TypeValue::Object(input) = &mut inputs[0] else {
            unreachable!()
        };
        input[2].value = TypeValue::Signed(3);
        assert!(project_cubism_physics(1, &value, CubismPhysicsReadLimits::default()).is_err());

        let limits = CubismPhysicsReadLimits {
            maximum_inputs: 0,
            ..CubismPhysicsReadLimits::default()
        };
        assert!(project_cubism_physics(1, &physics_value(), limits).is_err());

        let mut value = physics_value();
        let TypeValue::Object(root) = &mut value else {
            unreachable!()
        };
        let TypeValue::Object(rig) = &mut root[0].value else {
            unreachable!()
        };
        rig[3].value = TypeValue::Float(f64::NAN);
        assert!(project_cubism_physics(1, &value, CubismPhysicsReadLimits::default()).is_err());
    }
}
