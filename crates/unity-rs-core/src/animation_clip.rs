//! Bounded, source-bound parsing for Unity and Tuanjie `AnimationClip` objects.
//!
//! The implemented layout covers non-Tuanjie Unity 2017.3 through 2023.x and
//! Unity 6000.0 through 6000.2, plus the Tuanjie 2022.3.x layout. Tuanjie's
//! embedded animation-data, relocated curve, streaming-info, and ACL version
//! gates are parsed explicitly.

use std::mem::{replace, size_of};

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::{ObjectReference, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const ANIMATION_CLIP_CLASS_ID: i32 = 74;

const NO_TARGET_PLATFORM: i32 = -2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationClipReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_curves: usize,
    pub maximum_keyframes_per_curve: usize,
    pub maximum_total_keyframes: usize,
    pub maximum_array_elements: usize,
    pub maximum_total_array_elements: usize,
    pub maximum_packed_bytes: u64,
    pub maximum_total_packed_bytes: u64,
    pub maximum_streamed_words: usize,
    pub maximum_sample_values: usize,
    pub maximum_total_sample_values: usize,
    pub maximum_reference_bytes: u64,
    pub maximum_total_allocation_bytes: u64,
}

impl Default for AnimationClipReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 512 * 1024 * 1024,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 64 * 1024 * 1024,
            maximum_curves: 1_000_000,
            maximum_keyframes_per_curve: 1_000_000,
            maximum_total_keyframes: 2_000_000,
            maximum_array_elements: 2_000_000,
            maximum_total_array_elements: 10_000_000,
            maximum_packed_bytes: 256 * 1024 * 1024,
            maximum_total_packed_bytes: 512 * 1024 * 1024,
            maximum_streamed_words: 64 * 1024 * 1024,
            maximum_sample_values: 64 * 1024 * 1024,
            maximum_total_sample_values: 128 * 1024 * 1024,
            maximum_reference_bytes: 256 * 1024 * 1024,
            maximum_total_allocation_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteRegion {
    pub region: Region,
    pub byte_length: u64,
}

impl ByteRegion {
    pub fn read(&self, maximum_bytes: u64) -> Result<Vec<u8>> {
        self.region.read_to_vec(maximum_bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct F32Array {
    pub region: Region,
    pub count: usize,
    pub endian: Endian,
}

impl F32Array {
    pub fn read_values(&self, maximum_values: usize) -> Result<Vec<f32>> {
        read_numeric_array(
            &self.region,
            self.count,
            maximum_values,
            self.endian,
            EndianReader::read_f32,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct U32Array {
    pub region: Region,
    pub count: usize,
    pub endian: Endian,
}

impl U32Array {
    pub fn read_values(&self, maximum_values: usize) -> Result<Vec<u32>> {
        read_numeric_array(
            &self.region,
            self.count,
            maximum_values,
            self.endian,
            EndianReader::read_u32,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I32Array {
    pub region: Region,
    pub count: usize,
    pub endian: Endian,
}

impl I32Array {
    pub fn read_values(&self, maximum_values: usize) -> Result<Vec<i32>> {
        read_numeric_array(
            &self.region,
            self.count,
            maximum_values,
            self.endian,
            EndianReader::read_i32,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

pub type Quaternion = Vector4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub center: Vector3,
    pub extent: Vector3,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Keyframe<T> {
    pub time: f32,
    pub value: T,
    pub in_slope: T,
    pub out_slope: T,
    pub weighted_mode: Option<i32>,
    pub in_weight: Option<T>,
    pub out_weight: Option<T>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnimationCurve<T> {
    pub keyframes: Vec<Keyframe<T>>,
    pub pre_infinity: i32,
    pub post_infinity: i32,
    pub rotation_order: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuaternionCurve {
    pub curve: AnimationCurve<Quaternion>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Vector3Curve {
    pub curve: AnimationCurve<Vector3>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedIntVector {
    pub item_count: u32,
    pub data: ByteRegion,
    pub bit_size: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedQuaternionVector {
    pub item_count: u32,
    pub data: ByteRegion,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackedFloatVector {
    pub item_count: u32,
    pub range: f32,
    pub start: f32,
    pub data: ByteRegion,
    pub bit_size: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompressedAnimationCurve {
    pub path: String,
    pub times: PackedIntVector,
    pub values: PackedQuaternionVector,
    pub slopes: PackedFloatVector,
    pub pre_infinity: i32,
    pub post_infinity: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FloatCurve {
    pub curve: AnimationCurve<f32>,
    pub attribute: String,
    pub path: String,
    pub class_id: i32,
    pub script: ObjectReference,
    pub flags: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PPtrKeyframe {
    pub time: f32,
    pub value: ObjectReference,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PPtrCurve {
    pub keyframes: Vec<PPtrKeyframe>,
    pub attribute: String,
    pub path: String,
    pub class_id: i32,
    pub script: ObjectReference,
    pub flags: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xform {
    pub translation: Vector3,
    pub rotation: Quaternion,
    pub scale: Vector3,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HandPose {
    pub grab_x: Xform,
    pub degrees_of_freedom: F32Array,
    pub override_value: f32,
    pub close_open: f32,
    pub in_out: f32,
    pub grab: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HumanGoal {
    pub x: Xform,
    pub weight_translation: f32,
    pub weight_rotation: f32,
    pub hint_translation: Vector3,
    pub hint_weight_translation: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HumanPose {
    pub root_x: Xform,
    pub look_at_position: Vector3,
    pub look_at_weight: Vector4,
    pub goals: Vec<HumanGoal>,
    pub left_hand_pose: HandPose,
    pub right_hand_pose: HandPose,
    pub degrees_of_freedom: F32Array,
    pub translation_degrees_of_freedom: Vec<Vector3>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamedClip {
    pub data: U32Array,
    pub curve_count: u32,
    pub discrete_curve_count: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenseClip {
    pub frame_count: usize,
    pub curve_count: u32,
    pub sample_rate_bits: u32,
    pub begin_time_bits: u32,
    pub samples: F32Array,
}

impl DenseClip {
    #[must_use]
    pub const fn sample_rate(&self) -> f32 {
        f32::from_bits(self.sample_rate_bits)
    }

    #[must_use]
    pub const fn begin_time(&self) -> f32 {
        f32::from_bits(self.begin_time_bits)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstantClip {
    pub values: F32Array,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueConstant {
    pub id: u32,
    pub type_id: u32,
    pub value_type: u32,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueArrayConstant {
    pub values: Vec<ValueConstant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuscleClipData {
    pub streamed: StreamedClip,
    pub dense: DenseClip,
    pub constant: ConstantClip,
    pub binding: Option<ValueArrayConstant>,
    pub acl: Option<AclClip>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclClip {
    pub frame_count: u32,
    pub bone_count: u32,
    pub sample_rate_bits: u32,
    pub curve_count: Option<u32>,
    pub tracks: ByteRegion,
    pub decoder_map: U32Array,
    pub use_fast_sample_mode: Option<bool>,
}

impl AclClip {
    #[must_use]
    pub const fn sample_rate(&self) -> f32 {
        f32::from_bits(self.sample_rate_bits)
    }

    /// Validates and inspects the source-bound official ACL 2.x outer blob.
    ///
    /// Tuanjie's decoder map remains separate because its mapping contract is
    /// engine-specific and is not part of the public ACL container format.
    pub fn inspect_compressed_tracks(
        &self,
        limits: crate::acl::AclCompressedTracksLimits,
    ) -> Result<crate::acl::AclCompressedTracks> {
        crate::acl::inspect_compressed_tracks(&self.tracks.region, limits)
    }

    /// Materializes the exact validated bytes and Tuanjie decoder map needed
    /// by an ACL decoder, under a combined input budget.
    pub fn materialize_decoder_input(
        &self,
        limits: crate::acl::AclDecoderInputLimits,
    ) -> Result<crate::acl::AclDecoderInput> {
        let header = self.inspect_compressed_tracks(limits.compressed_tracks)?;
        if self.decoder_map.count > limits.maximum_decoder_map_entries {
            return Err(Error::invalid_data(format!(
                "ACL decoder map contains {} entries, exceeding limit {}",
                self.decoder_map.count, limits.maximum_decoder_map_entries
            )));
        }
        let decoder_map_bytes = u64::try_from(self.decoder_map.count)
            .ok()
            .and_then(|count| count.checked_mul(4))
            .ok_or_else(|| Error::invalid_data("ACL decoder-map byte length overflowed"))?;
        let materialized_bytes = self
            .tracks
            .byte_length
            .checked_add(decoder_map_bytes)
            .ok_or_else(|| Error::invalid_data("ACL decoder input byte length overflowed"))?;
        if materialized_bytes > limits.maximum_materialized_bytes {
            return Err(Error::invalid_data(format!(
                "ACL decoder input is {materialized_bytes} bytes, exceeding materialization limit {}",
                limits.maximum_materialized_bytes
            )));
        }
        let compressed_tracks = self
            .tracks
            .read(limits.compressed_tracks.maximum_compressed_bytes)?;
        let decoder_map = self
            .decoder_map
            .read_values(limits.maximum_decoder_map_entries)?;
        Ok(crate::acl::AclDecoderInput {
            header,
            compressed_tracks,
            decoder_map,
        })
    }

    /// Invokes a safe injected decoder and validates its complete frame-major
    /// output before it can reach model or `Live2D` conversion.
    pub fn decode_with(
        &self,
        decoder: &dyn crate::acl::AclDecoder,
        limits: crate::acl::AclDecodeLimits,
    ) -> Result<crate::acl::AclDecodedClip> {
        let input = self.materialize_decoder_input(limits.input)?;
        let request = crate::acl::AclDecodeRequest {
            frame_count: self.frame_count,
            bone_count: self.bone_count,
            sample_rate_bits: self.sample_rate_bits,
            declared_curve_count: self.curve_count,
            use_fast_sample_mode: self.use_fast_sample_mode,
            input: &input,
        };
        crate::acl::validate_decode_request(&request, limits)?;
        let output = decoder.decode(&request)?;
        crate::acl::validate_decoded_clip(&output, &request, limits)?;
        Ok(output)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueDelta {
    pub start: f32,
    pub stop: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MuscleClipFlags {
    bits: u16,
}

impl MuscleClipFlags {
    const MIRROR: u16 = 1 << 0;
    const LOOP_TIME: u16 = 1 << 1;
    const LOOP_BLEND: u16 = 1 << 2;
    const LOOP_BLEND_ORIENTATION: u16 = 1 << 3;
    const LOOP_BLEND_POSITION_Y: u16 = 1 << 4;
    const LOOP_BLEND_POSITION_XZ: u16 = 1 << 5;
    const START_AT_ORIGIN: u16 = 1 << 6;
    const KEEP_ORIGINAL_ORIENTATION: u16 = 1 << 7;
    const KEEP_ORIGINAL_POSITION_Y: u16 = 1 << 8;
    const KEEP_ORIGINAL_POSITION_XZ: u16 = 1 << 9;
    const HEIGHT_FROM_FEET: u16 = 1 << 10;

    #[must_use]
    pub const fn mirror(self) -> bool {
        self.bits & Self::MIRROR != 0
    }

    #[must_use]
    pub const fn loop_time(self) -> bool {
        self.bits & Self::LOOP_TIME != 0
    }

    #[must_use]
    pub const fn loop_blend(self) -> bool {
        self.bits & Self::LOOP_BLEND != 0
    }

    #[must_use]
    pub const fn loop_blend_orientation(self) -> bool {
        self.bits & Self::LOOP_BLEND_ORIENTATION != 0
    }

    #[must_use]
    pub const fn loop_blend_position_y(self) -> bool {
        self.bits & Self::LOOP_BLEND_POSITION_Y != 0
    }

    #[must_use]
    pub const fn loop_blend_position_xz(self) -> bool {
        self.bits & Self::LOOP_BLEND_POSITION_XZ != 0
    }

    #[must_use]
    pub const fn start_at_origin(self) -> bool {
        self.bits & Self::START_AT_ORIGIN != 0
    }

    #[must_use]
    pub const fn keep_original_orientation(self) -> bool {
        self.bits & Self::KEEP_ORIGINAL_ORIENTATION != 0
    }

    #[must_use]
    pub const fn keep_original_position_y(self) -> bool {
        self.bits & Self::KEEP_ORIGINAL_POSITION_Y != 0
    }

    #[must_use]
    pub const fn keep_original_position_xz(self) -> bool {
        self.bits & Self::KEEP_ORIGINAL_POSITION_XZ != 0
    }

    #[must_use]
    pub const fn height_from_feet(self) -> bool {
        self.bits & Self::HEIGHT_FROM_FEET != 0
    }

    fn push(&mut self, mask: u16, value: bool) {
        if value {
            self.bits |= mask;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClipMuscleConstant {
    pub delta_pose: HumanPose,
    pub start_x: Xform,
    pub stop_x: Xform,
    pub left_foot_start_x: Xform,
    pub right_foot_start_x: Xform,
    pub average_speed: Vector3,
    pub clip: MuscleClipData,
    pub start_time: f32,
    pub stop_time: f32,
    pub orientation_offset_y: f32,
    pub level: f32,
    pub cycle_offset: f32,
    pub average_angular_speed: f32,
    pub indexes: I32Array,
    pub value_deltas: Vec<ValueDelta>,
    pub reference_pose: F32Array,
    pub flags: MuscleClipFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenericBinding {
    pub path: u32,
    pub attribute: u32,
    pub script: ObjectReference,
    pub type_id: i32,
    pub custom_type: u8,
    pub is_pptr_curve: u8,
    pub is_int_curve: Option<u8>,
    pub is_serialize_reference_curve: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationClipBindingConstant {
    pub generic_bindings: Vec<GenericBinding>,
    pub pptr_curve_mapping: Vec<ObjectReference>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnimationEvent {
    pub time: f32,
    pub function_name: String,
    pub data: String,
    pub object_reference_parameter: ObjectReference,
    pub float_parameter: f32,
    pub int_parameter: i32,
    pub message_options: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnimationClip {
    pub path_id: i64,
    pub name: String,
    pub legacy: bool,
    pub compressed: bool,
    pub use_high_quality_curve: bool,
    pub rotation_curves: Vec<QuaternionCurve>,
    pub compressed_rotation_curves: Vec<CompressedAnimationCurve>,
    pub euler_curves: Vec<Vector3Curve>,
    pub position_curves: Vec<Vector3Curve>,
    pub scale_curves: Vec<Vector3Curve>,
    pub float_curves: Vec<FloatCurve>,
    pub pptr_curves: Vec<PPtrCurve>,
    pub sample_rate: f32,
    pub wrap_mode: i32,
    pub bounds: Aabb,
    pub muscle_clip_size: u32,
    /// Absent for pre-2022.3.61 Tuanjie legacy clips or an empty `m_AnimData`.
    pub muscle_clip: Option<ClipMuscleConstant>,
    pub streaming_info: Option<AnimationStreamingInfo>,
    pub binding_constant: AnimationClipBindingConstant,
    pub has_generic_root_transform: Option<bool>,
    pub has_motion_float_curves: Option<bool>,
    pub events: Vec<AnimationEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationStreamingInfo {
    pub offset: i64,
    pub size: u32,
    pub path: String,
}

pub fn read_animation_clip(
    file: &SerializedFile,
    object_index: usize,
    limits: AnimationClipReadLimits,
) -> Result<AnimationClip> {
    validate_supported_version(file)?;
    AnimationClipReader::new(file, object_index, limits)?.read()
}

fn validate_supported_version(file: &SerializedFile) -> Result<()> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "AnimationClip requires a Unity version because every curve layout is version-dependent",
        ));
    }
    let version = file.unity_version.components();
    let supported = if file.unity_version.is_tuanjie() {
        version.0 == 2022 && version.1 == 3 && version.2 >= 2
    } else {
        ((2017, 3, 0)..(2024, 0, 0)).contains(&version) || (version.0 == 6000 && version.1 <= 2)
    };
    if !supported {
        return Err(Error::unsupported(format!(
            "AnimationClip Unity version {} is outside the verified 2017.3 through 2023.x, 6000.0 through 6000.2, and Tuanjie 2022.3.x ranges",
            file.unity_version
        )));
    }
    Ok(())
}

fn uses_split_streamed_curve_counts(version: (u32, u32, u32)) -> bool {
    ((2022, 3, 19)..(2023, 0, 0)).contains(&version) || version >= (2023, 2, 8)
}

fn read_numeric_array<T>(
    region: &Region,
    count: usize,
    maximum_values: usize,
    endian: Endian,
    mut read_value: impl FnMut(&mut EndianReader<RegionCursor>) -> Result<T>,
) -> Result<Vec<T>> {
    if count > maximum_values {
        return Err(Error::invalid_data(format!(
            "numeric array has {count} values, exceeding materialization limit {maximum_values}"
        )));
    }
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {count} numeric values: {error}"))
    })?;
    let mut reader = EndianReader::new(region.cursor(), endian);
    for _ in 0..count {
        values.push(read_value(&mut reader)?);
    }
    if reader.remaining()? != 0 {
        return Err(Error::invalid_data(
            "numeric array region contains trailing bytes",
        ));
    }
    Ok(values)
}

#[derive(Debug, Default)]
struct AnimationClipBudget {
    string_bytes: usize,
    total_keyframes: usize,
    array_elements: usize,
    packed_bytes: u64,
    sample_values: usize,
    reference_bytes: u64,
    allocation_bytes: u64,
}

struct AnimationClipReader {
    reader: EndianReader<RegionCursor>,
    region: Region,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    version: (u32, u32, u32),
    version_build: u32,
    is_tuanjie: bool,
    endian: Endian,
    limits: AnimationClipReadLimits,
    budget: AnimationClipBudget,
}

impl AnimationClipReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        limits: AnimationClipReadLimits,
    ) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != ANIMATION_CLIP_CLASS_ID {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, not AnimationClip ({ANIMATION_CLIP_CLASS_ID})",
                object.path_id, object.class_id
            )));
        }
        if object.byte_size > limits.maximum_object_bytes {
            return Err(Error::invalid_data(format!(
                "AnimationClip object is {} bytes, exceeding limit {}",
                object.byte_size, limits.maximum_object_bytes
            )));
        }
        let region = file.object_region(object_index)?;
        let endian = if file.header.endianness == 0 {
            Endian::Little
        } else {
            Endian::Big
        };
        Ok(Self {
            reader: EndianReader::new(region.cursor(), endian),
            region,
            absolute_start: object.byte_start,
            path_id: object.path_id,
            target_platform: file.target_platform,
            format_version: file.header.version.0,
            version: file.unity_version.components(),
            version_build: file.unity_version.build,
            is_tuanjie: file.unity_version.is_tuanjie(),
            endian,
            limits,
            budget: AnimationClipBudget::default(),
        })
    }

    fn read(mut self) -> Result<AnimationClip> {
        let name = self.read_named_object()?;
        let legacy = self.reader.read_bool()?;
        let compressed = self.reader.read_bool()?;
        let use_high_quality_curve = self.reader.read_bool()?;
        self.align(4)?;

        let rotation_curves = self.read_quaternion_curve_list()?;
        let compressed_rotation_curves = self.read_compressed_curve_list()?;
        let (mut euler_curves, mut position_curves, mut scale_curves) = if self.is_tuanjie {
            (Vec::new(), Vec::new(), Vec::new())
        } else {
            (
                self.read_vector3_curve_list("AnimationClip Euler curve")?,
                self.read_vector3_curve_list("AnimationClip position curve")?,
                self.read_vector3_curve_list("AnimationClip scale curve")?,
            )
        };
        let float_curves = self.read_float_curve_list()?;
        let pptr_curves = self.read_pptr_curve_list()?;

        let sample_rate = self.reader.read_f32()?;
        let wrap_mode = self.reader.read_i32()?;
        let bounds = self.read_aabb()?;
        if self.is_tuanjie && self.version >= (2022, 3, 61) {
            euler_curves = self.read_vector3_curve_list("AnimationClip relocated Euler curve")?;
            position_curves =
                self.read_vector3_curve_list("AnimationClip relocated position curve")?;
            scale_curves = self.read_vector3_curve_list("AnimationClip relocated scale curve")?;
        }
        let muscle_clip_size = self.reader.read_u32()?;
        let (muscle_clip, streaming_info) = self.read_tuanjie_or_standard_muscle(
            muscle_clip_size,
            legacy,
            &mut euler_curves,
            &mut position_curves,
            &mut scale_curves,
        )?;
        let binding_constant = self.read_binding_constant()?;
        let (has_generic_root_transform, has_motion_float_curves) = if self.version >= (2018, 3, 0)
        {
            let generic_root = self.reader.read_bool()?;
            let motion_float = self.reader.read_bool()?;
            self.align(4)?;
            (Some(generic_root), Some(motion_float))
        } else {
            (None, None)
        };
        let events = self.read_events()?;
        self.align(4)?;
        let trailing = self.reader.remaining()?;
        if trailing != 0 {
            return Err(Error::unsupported(format!(
                "AnimationClip has {trailing} trailing bytes after the verified 2017.3-2022 layout"
            )));
        }

        Ok(AnimationClip {
            path_id: self.path_id,
            name,
            legacy,
            compressed,
            use_high_quality_curve,
            rotation_curves,
            compressed_rotation_curves,
            euler_curves,
            position_curves,
            scale_curves,
            float_curves,
            pptr_curves,
            sample_rate,
            wrap_mode,
            bounds,
            muscle_clip_size,
            muscle_clip,
            streaming_info,
            binding_constant,
            has_generic_root_transform,
            has_motion_float_curves,
            events,
        })
    }

    fn read_tuanjie_or_standard_muscle(
        &mut self,
        muscle_clip_size: u32,
        legacy: bool,
        euler_curves: &mut Vec<Vector3Curve>,
        position_curves: &mut Vec<Vector3Curve>,
        scale_curves: &mut Vec<Vector3Curve>,
    ) -> Result<(Option<ClipMuscleConstant>, Option<AnimationStreamingInfo>)> {
        if !self.is_tuanjie {
            return Ok((Some(self.read_clip_muscle_constant()?), None));
        }
        if self.version >= (2022, 3, 61) {
            let muscle = self.read_clip_muscle_constant()?;
            let streaming = self.read_animation_streaming_info()?;
            return Ok((Some(muscle), Some(streaming)));
        }
        if muscle_clip_size == 0 {
            return Ok((None, None));
        }

        let animation_data = self.read_byte_region("AnimationClip m_AnimData", false)?;
        if legacy {
            let (euler, position, scale) = self.with_embedded_animation_data(
                animation_data.region,
                "legacy relocated curves",
                |reader| {
                    Ok((
                        reader.read_vector3_curve_list("AnimationClip relocated Euler curve")?,
                        reader.read_vector3_curve_list("AnimationClip relocated position curve")?,
                        reader.read_vector3_curve_list("AnimationClip relocated scale curve")?,
                    ))
                },
            )?;
            *euler_curves = euler;
            *position_curves = position;
            *scale_curves = scale;
            return Ok((None, None));
        }

        let muscle = self.with_embedded_animation_data(
            animation_data.region,
            "muscle clip",
            Self::read_clip_muscle_constant,
        )?;
        let streaming = self.read_animation_streaming_info()?;
        Ok((Some(muscle), Some(streaming)))
    }

    fn with_embedded_animation_data<T>(
        &mut self,
        region: Region,
        field: &str,
        parse: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let nested_reader = EndianReader::new(region.cursor(), Endian::Little);
        let parent_reader = replace(&mut self.reader, nested_reader);
        let parent_region = replace(&mut self.region, region);
        let parent_absolute_start = replace(&mut self.absolute_start, 0);
        let parent_endian = replace(&mut self.endian, Endian::Little);

        let result = parse(self).and_then(|value| {
            let trailing = self.reader.remaining()?;
            if trailing != 0 {
                return Err(Error::invalid_data(format!(
                    "AnimationClip embedded {field} has {trailing} trailing bytes"
                )));
            }
            Ok(value)
        });

        self.reader = parent_reader;
        self.region = parent_region;
        self.absolute_start = parent_absolute_start;
        self.endian = parent_endian;
        result
    }

    fn read_animation_streaming_info(&mut self) -> Result<AnimationStreamingInfo> {
        Ok(AnimationStreamingInfo {
            offset: self.reader.read_i64()?,
            size: self.reader.read_u32()?,
            path: self.read_aligned_string("AnimationClip streaming path")?,
        })
    }

    fn read_quaternion_curve_list(&mut self) -> Result<Vec<QuaternionCurve>> {
        let count = self.read_count(
            "AnimationClip rotation curve",
            self.limits.maximum_curves,
            20,
        )?;
        let mut curves = self.reserve(count, "AnimationClip rotation curves")?;
        for _ in 0..count {
            curves.push(QuaternionCurve {
                curve: self.read_quaternion_animation_curve()?,
                path: self.read_aligned_string("AnimationClip rotation curve path")?,
            });
        }
        Ok(curves)
    }

    fn read_vector3_curve_list(&mut self, field: &str) -> Result<Vec<Vector3Curve>> {
        let count = self.read_count(field, self.limits.maximum_curves, 20)?;
        let mut curves = self.reserve(count, field)?;
        for _ in 0..count {
            curves.push(Vector3Curve {
                curve: self.read_vector3_animation_curve()?,
                path: self.read_aligned_string(&format!("{field} path"))?,
            });
        }
        Ok(curves)
    }

    fn read_float_curve_list(&mut self) -> Result<Vec<FloatCurve>> {
        let count = self.read_count("AnimationClip float curve", self.limits.maximum_curves, 32)?;
        let mut curves = self.reserve(count, "AnimationClip float curves")?;
        for _ in 0..count {
            let curve = self.read_float_animation_curve()?;
            let attribute = self.read_aligned_string("AnimationClip float curve attribute")?;
            let path = self.read_aligned_string("AnimationClip float curve path")?;
            let class_id = self.reader.read_i32()?;
            let script = self.read_pptr("AnimationClip float curve script")?;
            let flags = if self.version >= (2022, 2, 0) {
                Some(self.reader.read_i32()?)
            } else {
                None
            };
            curves.push(FloatCurve {
                curve,
                attribute,
                path,
                class_id,
                script,
                flags,
            });
        }
        Ok(curves)
    }

    fn read_pptr_curve_list(&mut self) -> Result<Vec<PPtrCurve>> {
        let count = self.read_count("AnimationClip PPtr curve", self.limits.maximum_curves, 24)?;
        let mut curves = self.reserve(count, "AnimationClip PPtr curves")?;
        for _ in 0..count {
            let keyframe_size = self
                .pptr_size()
                .checked_add(4)
                .ok_or_else(|| Error::invalid_data("PPtr keyframe size overflowed"))?;
            let keyframe_count =
                self.read_keyframe_count("AnimationClip PPtr curve keyframe", keyframe_size)?;
            let mut keyframes =
                self.reserve(keyframe_count, "AnimationClip PPtr curve keyframes")?;
            for _ in 0..keyframe_count {
                keyframes.push(PPtrKeyframe {
                    time: self.reader.read_f32()?,
                    value: self.read_pptr("AnimationClip PPtr curve value")?,
                });
            }
            let attribute = self.read_aligned_string("AnimationClip PPtr curve attribute")?;
            let path = self.read_aligned_string("AnimationClip PPtr curve path")?;
            let class_id = self.reader.read_i32()?;
            let script = self.read_pptr("AnimationClip PPtr curve script")?;
            let flags = if self.version >= (2022, 2, 0) {
                Some(self.reader.read_i32()?)
            } else {
                None
            };
            curves.push(PPtrCurve {
                keyframes,
                attribute,
                path,
                class_id,
                script,
                flags,
            });
        }
        Ok(curves)
    }

    fn read_quaternion_animation_curve(&mut self) -> Result<AnimationCurve<Quaternion>> {
        let weighted = self.version.0 >= 2018;
        let record_size = if weighted { 88 } else { 52 };
        let count = self.read_keyframe_count("quaternion curve keyframe", record_size)?;
        let mut keyframes = self.reserve(count, "quaternion curve keyframes")?;
        for _ in 0..count {
            let time = self.reader.read_f32()?;
            let value = self.read_vector4()?;
            let in_slope = self.read_vector4()?;
            let out_slope = self.read_vector4()?;
            let (weighted_mode, in_weight, out_weight) = if weighted {
                (
                    Some(self.reader.read_i32()?),
                    Some(self.read_vector4()?),
                    Some(self.read_vector4()?),
                )
            } else {
                (None, None, None)
            };
            keyframes.push(Keyframe {
                time,
                value,
                in_slope,
                out_slope,
                weighted_mode,
                in_weight,
                out_weight,
            });
        }
        self.finish_animation_curve(keyframes)
    }

    fn read_vector3_animation_curve(&mut self) -> Result<AnimationCurve<Vector3>> {
        let weighted = self.version.0 >= 2018;
        let record_size = if weighted { 68 } else { 40 };
        let count = self.read_keyframe_count("Vector3 curve keyframe", record_size)?;
        let mut keyframes = self.reserve(count, "Vector3 curve keyframes")?;
        for _ in 0..count {
            let time = self.reader.read_f32()?;
            let value = self.read_vector3()?;
            let in_slope = self.read_vector3()?;
            let out_slope = self.read_vector3()?;
            let (weighted_mode, in_weight, out_weight) = if weighted {
                (
                    Some(self.reader.read_i32()?),
                    Some(self.read_vector3()?),
                    Some(self.read_vector3()?),
                )
            } else {
                (None, None, None)
            };
            keyframes.push(Keyframe {
                time,
                value,
                in_slope,
                out_slope,
                weighted_mode,
                in_weight,
                out_weight,
            });
        }
        self.finish_animation_curve(keyframes)
    }

    fn read_float_animation_curve(&mut self) -> Result<AnimationCurve<f32>> {
        let weighted = self.version.0 >= 2018;
        let record_size = if weighted { 28 } else { 16 };
        let count = self.read_keyframe_count("float curve keyframe", record_size)?;
        let mut keyframes = self.reserve(count, "float curve keyframes")?;
        for _ in 0..count {
            let time = self.reader.read_f32()?;
            let value = self.reader.read_f32()?;
            let in_slope = self.reader.read_f32()?;
            let out_slope = self.reader.read_f32()?;
            let (weighted_mode, in_weight, out_weight) = if weighted {
                (
                    Some(self.reader.read_i32()?),
                    Some(self.reader.read_f32()?),
                    Some(self.reader.read_f32()?),
                )
            } else {
                (None, None, None)
            };
            keyframes.push(Keyframe {
                time,
                value,
                in_slope,
                out_slope,
                weighted_mode,
                in_weight,
                out_weight,
            });
        }
        self.finish_animation_curve(keyframes)
    }

    fn finish_animation_curve<T>(
        &mut self,
        keyframes: Vec<Keyframe<T>>,
    ) -> Result<AnimationCurve<T>> {
        Ok(AnimationCurve {
            keyframes,
            pre_infinity: self.reader.read_i32()?,
            post_infinity: self.reader.read_i32()?,
            rotation_order: self.reader.read_i32()?,
        })
    }

    fn read_compressed_curve_list(&mut self) -> Result<Vec<CompressedAnimationCurve>> {
        let count = self.read_count(
            "AnimationClip compressed rotation curve",
            self.limits.maximum_curves,
            48,
        )?;
        let mut curves = self.reserve(count, "AnimationClip compressed rotation curves")?;
        for _ in 0..count {
            curves.push(CompressedAnimationCurve {
                path: self.read_aligned_string("compressed rotation curve path")?,
                times: self.read_packed_int_vector()?,
                values: self.read_packed_quaternion_vector()?,
                slopes: self.read_packed_float_vector()?,
                pre_infinity: self.reader.read_i32()?,
                post_infinity: self.reader.read_i32()?,
            });
        }
        Ok(curves)
    }

    fn read_packed_int_vector(&mut self) -> Result<PackedIntVector> {
        let item_count = self.read_declared_item_count("packed integer item")?;
        let data = self.read_packed_bytes("packed integer data")?;
        let bit_size = self.reader.read_u8()?;
        self.align(4)?;
        Ok(PackedIntVector {
            item_count,
            data,
            bit_size,
        })
    }

    fn read_packed_quaternion_vector(&mut self) -> Result<PackedQuaternionVector> {
        let item_count = self.read_declared_item_count("packed quaternion item")?;
        let data = self.read_packed_bytes("packed quaternion data")?;
        Ok(PackedQuaternionVector { item_count, data })
    }

    fn read_packed_float_vector(&mut self) -> Result<PackedFloatVector> {
        let item_count = self.read_declared_item_count("packed float item")?;
        let range = self.reader.read_f32()?;
        let start = self.reader.read_f32()?;
        let data = self.read_packed_bytes("packed float data")?;
        let bit_size = self.reader.read_u8()?;
        self.align(4)?;
        Ok(PackedFloatVector {
            item_count,
            range,
            start,
            data,
            bit_size,
        })
    }

    fn read_clip_muscle_constant(&mut self) -> Result<ClipMuscleConstant> {
        let delta_pose = self.read_human_pose()?;
        let start_x = self.read_xform()?;
        let stop_x = self.read_xform()?;
        let left_foot_start_x = self.read_xform()?;
        let right_foot_start_x = self.read_xform()?;
        let average_speed = self.read_vector3()?;
        let clip = self.read_muscle_clip_data()?;
        let start_time = self.reader.read_f32()?;
        let stop_time = self.reader.read_f32()?;
        let orientation_offset_y = self.reader.read_f32()?;
        let level = self.reader.read_f32()?;
        let cycle_offset = self.reader.read_f32()?;
        let average_angular_speed = self.reader.read_f32()?;
        let indexes = self.read_i32_array("AnimationClip muscle index")?;

        let delta_count = self.read_count(
            "AnimationClip muscle value delta",
            self.limits.maximum_array_elements,
            8,
        )?;
        let mut value_deltas = self.reserve(delta_count, "AnimationClip muscle value deltas")?;
        for _ in 0..delta_count {
            value_deltas.push(ValueDelta {
                start: self.reader.read_f32()?,
                stop: self.reader.read_f32()?,
            });
        }
        let reference_pose = self.read_f32_array("AnimationClip reference pose", false)?;

        let mut flags = MuscleClipFlags::default();
        for mask in [
            MuscleClipFlags::MIRROR,
            MuscleClipFlags::LOOP_TIME,
            MuscleClipFlags::LOOP_BLEND,
            MuscleClipFlags::LOOP_BLEND_ORIENTATION,
            MuscleClipFlags::LOOP_BLEND_POSITION_Y,
            MuscleClipFlags::LOOP_BLEND_POSITION_XZ,
            MuscleClipFlags::START_AT_ORIGIN,
            MuscleClipFlags::KEEP_ORIGINAL_ORIENTATION,
            MuscleClipFlags::KEEP_ORIGINAL_POSITION_Y,
            MuscleClipFlags::KEEP_ORIGINAL_POSITION_XZ,
            MuscleClipFlags::HEIGHT_FROM_FEET,
        ] {
            flags.push(mask, self.reader.read_bool()?);
        }
        self.align(4)?;

        Ok(ClipMuscleConstant {
            delta_pose,
            start_x,
            stop_x,
            left_foot_start_x,
            right_foot_start_x,
            average_speed,
            clip,
            start_time,
            stop_time,
            orientation_offset_y,
            level,
            cycle_offset,
            average_angular_speed,
            indexes,
            value_deltas,
            reference_pose,
            flags,
        })
    }

    fn read_human_pose(&mut self) -> Result<HumanPose> {
        let root_x = self.read_xform()?;
        let look_at_position = self.read_vector3()?;
        let look_at_weight = self.read_vector4()?;
        let goal_count = self.read_count(
            "AnimationClip human goal",
            self.limits.maximum_array_elements,
            64,
        )?;
        let mut goals = self.reserve(goal_count, "AnimationClip human goals")?;
        for _ in 0..goal_count {
            goals.push(HumanGoal {
                x: self.read_xform()?,
                weight_translation: self.reader.read_f32()?,
                weight_rotation: self.reader.read_f32()?,
                hint_translation: self.read_vector3()?,
                hint_weight_translation: self.reader.read_f32()?,
            });
        }
        let left_hand_pose = self.read_hand_pose()?;
        let right_hand_pose = self.read_hand_pose()?;
        let degrees_of_freedom = self.read_f32_array("AnimationClip human pose DoF", false)?;
        let tdof_count = self.read_count(
            "AnimationClip translation DoF",
            self.limits.maximum_array_elements,
            12,
        )?;
        let mut translation_degrees_of_freedom =
            self.reserve(tdof_count, "AnimationClip translation DoFs")?;
        for _ in 0..tdof_count {
            translation_degrees_of_freedom.push(self.read_vector3()?);
        }
        Ok(HumanPose {
            root_x,
            look_at_position,
            look_at_weight,
            goals,
            left_hand_pose,
            right_hand_pose,
            degrees_of_freedom,
            translation_degrees_of_freedom,
        })
    }

    fn read_hand_pose(&mut self) -> Result<HandPose> {
        Ok(HandPose {
            grab_x: self.read_xform()?,
            degrees_of_freedom: self.read_f32_array("AnimationClip hand pose DoF", false)?,
            override_value: self.reader.read_f32()?,
            close_open: self.reader.read_f32()?,
            in_out: self.reader.read_f32()?,
            grab: self.reader.read_f32()?,
        })
    }

    fn read_muscle_clip_data(&mut self) -> Result<MuscleClipData> {
        let streamed = self.read_streamed_clip()?;
        let dense = self.read_dense_clip()?;
        let constant = ConstantClip {
            values: self.read_f32_array("AnimationClip constant sample", true)?,
        };
        let binding = if self.version < (2018, 3, 0) {
            Some(self.read_value_array_constant()?)
        } else {
            None
        };
        let acl = if self.tuanjie_has_acl() {
            Some(self.read_acl_clip()?)
        } else {
            None
        };
        Ok(MuscleClipData {
            streamed,
            dense,
            constant,
            binding,
            acl,
        })
    }

    fn read_acl_clip(&mut self) -> Result<AclClip> {
        let frame_count = self.reader.read_u32()?;
        self.validate_u32_count(frame_count, "AnimationClip ACL frame count")?;
        let bone_count = self.reader.read_u32()?;
        self.validate_u32_count(bone_count, "AnimationClip ACL bone count")?;
        let sample_rate_bits = self.reader.read_u32()?;
        let curve_count = if self.version >= (2022, 3, 55) {
            let count = self.reader.read_u32()?;
            self.validate_u32_count(count, "AnimationClip ACL curve count")?;
            Some(count)
        } else {
            None
        };
        let tracks =
            self.read_byte_region("AnimationClip ACL tracks", self.version >= (2022, 3, 61))?;
        let decoder_map = self.read_u32_array(
            "AnimationClip ACL decoder map",
            self.limits.maximum_array_elements,
        )?;
        let use_fast_sample_mode = if self.tuanjie_has_acl_fast_sample_mode() {
            let enabled = self.reader.read_bool()?;
            if self.version >= (2022, 3, 61) {
                self.align(4)?;
            }
            Some(enabled)
        } else {
            None
        };
        Ok(AclClip {
            frame_count,
            bone_count,
            sample_rate_bits,
            curve_count,
            tracks,
            decoder_map,
            use_fast_sample_mode,
        })
    }

    fn tuanjie_has_acl(&self) -> bool {
        self.is_tuanjie
            && (self.version > (2022, 3, 48)
                || (self.version == (2022, 3, 48) && self.version_build >= 3))
    }

    fn tuanjie_has_acl_fast_sample_mode(&self) -> bool {
        self.version > (2022, 3, 55) || (self.version == (2022, 3, 55) && self.version_build >= 4)
    }

    fn read_streamed_clip(&mut self) -> Result<StreamedClip> {
        let data = self.read_u32_array(
            "AnimationClip streamed word",
            self.limits.maximum_streamed_words,
        )?;
        let uses_split_counts = uses_split_streamed_curve_counts(self.version);
        let (curve_count, discrete_curve_count) = if uses_split_counts {
            let curve_count = self.reader.read_u16()?;
            (u32::from(curve_count), Some(self.reader.read_u16()?))
        } else {
            (self.reader.read_u32()?, None)
        };
        self.validate_u32_count(curve_count, "AnimationClip streamed curve count")?;
        Ok(StreamedClip {
            data,
            curve_count,
            discrete_curve_count,
        })
    }

    fn read_dense_clip(&mut self) -> Result<DenseClip> {
        let frame_count = checked_length(self.reader.read_i32()?, "AnimationClip dense frame")?;
        if frame_count > self.limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "AnimationClip dense frame count {frame_count} exceeds limit {}",
                self.limits.maximum_array_elements
            )));
        }
        let curve_count = self.reader.read_u32()?;
        self.validate_u32_count(curve_count, "AnimationClip dense curve count")?;
        let sample_rate_bits = self.reader.read_u32()?;
        let begin_time_bits = self.reader.read_u32()?;
        let samples = self.read_f32_array("AnimationClip dense sample", true)?;
        Ok(DenseClip {
            frame_count,
            curve_count,
            sample_rate_bits,
            begin_time_bits,
            samples,
        })
    }

    fn read_value_array_constant(&mut self) -> Result<ValueArrayConstant> {
        let count = self.read_count(
            "AnimationClip value binding",
            self.limits.maximum_array_elements,
            12,
        )?;
        let mut values = self.reserve(count, "AnimationClip value bindings")?;
        for _ in 0..count {
            values.push(ValueConstant {
                id: self.reader.read_u32()?,
                type_id: 0,
                value_type: self.reader.read_u32()?,
                index: self.reader.read_u32()?,
            });
        }
        Ok(ValueArrayConstant { values })
    }

    fn read_binding_constant(&mut self) -> Result<AnimationClipBindingConstant> {
        let count = self.read_count(
            "AnimationClip generic binding",
            self.limits.maximum_array_elements,
            self.minimum_generic_binding_size()?,
        )?;
        let mut generic_bindings = self.reserve(count, "AnimationClip generic bindings")?;
        for _ in 0..count {
            let path = self.reader.read_u32()?;
            let attribute = self.reader.read_u32()?;
            let script = self.read_pptr("AnimationClip generic binding script")?;
            let type_id = self.reader.read_i32()?;
            let custom_type = self.reader.read_u8()?;
            let is_pptr_curve = self.reader.read_u8()?;
            let is_int_curve = if self.version >= (2022, 1, 0) {
                Some(self.reader.read_u8()?)
            } else {
                None
            };
            let is_serialize_reference_curve = if self.version >= (2022, 2, 0) {
                Some(self.reader.read_u8()?)
            } else {
                None
            };
            self.align(4)?;
            generic_bindings.push(GenericBinding {
                path,
                attribute,
                script,
                type_id,
                custom_type,
                is_pptr_curve,
                is_int_curve,
                is_serialize_reference_curve,
            });
        }

        let mapping_count = self.read_count(
            "AnimationClip PPtr curve mapping",
            self.limits.maximum_array_elements,
            self.pptr_size(),
        )?;
        self.check_reference_array_budget(mapping_count, "AnimationClip PPtr curve mapping")?;
        let mut pptr_curve_mapping =
            self.reserve(mapping_count, "AnimationClip PPtr curve mappings")?;
        for _ in 0..mapping_count {
            pptr_curve_mapping.push(self.read_pptr("AnimationClip PPtr curve mapping")?);
        }
        Ok(AnimationClipBindingConstant {
            generic_bindings,
            pptr_curve_mapping,
        })
    }

    fn read_events(&mut self) -> Result<Vec<AnimationEvent>> {
        let minimum_event_size = self
            .pptr_size()
            .checked_add(24)
            .ok_or_else(|| Error::invalid_data("AnimationEvent size overflowed"))?;
        let count = self.read_count(
            "AnimationClip event",
            self.limits.maximum_array_elements,
            minimum_event_size,
        )?;
        let mut events = self.reserve(count, "AnimationClip events")?;
        for _ in 0..count {
            events.push(AnimationEvent {
                time: self.reader.read_f32()?,
                function_name: self.read_aligned_string("AnimationEvent function name")?,
                data: self.read_aligned_string("AnimationEvent data")?,
                object_reference_parameter: self.read_pptr("AnimationEvent object parameter")?,
                float_parameter: self.reader.read_f32()?,
                int_parameter: self.reader.read_i32()?,
                message_options: self.reader.read_i32()?,
            });
        }
        Ok(events)
    }

    fn read_named_object(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            self.skip(4, "AnimationClip object hide flags")?;
            let _prefab_parent = self.read_pptr("AnimationClip prefab parent")?;
            let _prefab_internal = self.read_pptr("AnimationClip prefab internal")?;
        }
        self.read_aligned_string("AnimationClip name")
    }

    fn read_aabb(&mut self) -> Result<Aabb> {
        Ok(Aabb {
            center: self.read_vector3()?,
            extent: self.read_vector3()?,
        })
    }

    fn read_xform(&mut self) -> Result<Xform> {
        Ok(Xform {
            translation: self.read_vector3()?,
            rotation: self.read_vector4()?,
            scale: self.read_vector3()?,
        })
    }

    fn read_vector3(&mut self) -> Result<Vector3> {
        Ok(Vector3 {
            x: self.reader.read_f32()?,
            y: self.reader.read_f32()?,
            z: self.reader.read_f32()?,
        })
    }

    fn read_vector4(&mut self) -> Result<Vector4> {
        Ok(Vector4 {
            x: self.reader.read_f32()?,
            y: self.reader.read_f32()?,
            z: self.reader.read_f32()?,
            w: self.reader.read_f32()?,
        })
    }

    fn read_pptr(&mut self, field: &str) -> Result<ObjectReference> {
        let size = u64::try_from(self.pptr_size())
            .map_err(|_| Error::invalid_data("PPtr size does not fit in u64"))?;
        self.budget.reference_bytes = self
            .budget
            .reference_bytes
            .checked_add(size)
            .ok_or_else(|| Error::invalid_data("AnimationClip reference budget overflowed"))?;
        if self.budget.reference_bytes > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "AnimationClip references total {} bytes while reading {field}, exceeding limit {}",
                self.budget.reference_bytes, self.limits.maximum_reference_bytes
            )));
        }
        let file_id = self.reader.read_i32()?;
        let path_id = if self.format_version < 14 {
            i64::from(self.reader.read_i32()?)
        } else {
            self.reader.read_i64()?
        };
        Ok(ObjectReference { file_id, path_id })
    }

    fn read_aligned_string(&mut self, field: &str) -> Result<String> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        if length > self.limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_string_bytes
            )));
        }
        self.budget.string_bytes = self
            .budget
            .string_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("AnimationClip string budget overflowed"))?;
        if self.budget.string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "AnimationClip strings total {} bytes, exceeding limit {}",
                self.budget.string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        let allocation_bytes = u64::try_from(length)
            .ok()
            .and_then(|length| length.checked_mul(4))
            .ok_or_else(|| Error::invalid_data(format!("{field} allocation size overflowed")))?;
        self.charge_allocation(allocation_bytes, field)?;
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn read_count(&mut self, field: &str, maximum: usize, minimum_size: usize) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > maximum {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {maximum}"
            )));
        }
        self.charge_array_elements(count, field)?;
        let minimum_bytes = count
            .checked_mul(minimum_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        if u64::try_from(minimum_bytes).unwrap_or(u64::MAX) > self.reader.remaining()? {
            return Err(Error::invalid_data(format!(
                "{field} needs at least {minimum_bytes} bytes beyond the bounded object payload"
            )));
        }
        Ok(count)
    }

    fn read_keyframe_count(&mut self, field: &str, record_size: usize) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > self.limits.maximum_keyframes_per_curve {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds per-curve limit {}",
                self.limits.maximum_keyframes_per_curve
            )));
        }
        self.budget.total_keyframes = self
            .budget
            .total_keyframes
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("AnimationClip keyframe budget overflowed"))?;
        if self.budget.total_keyframes > self.limits.maximum_total_keyframes {
            return Err(Error::invalid_data(format!(
                "AnimationClip keyframes total {}, exceeding limit {}",
                self.budget.total_keyframes, self.limits.maximum_total_keyframes
            )));
        }
        self.charge_array_elements(count, field)?;
        let minimum_bytes = count
            .checked_mul(record_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        if u64::try_from(minimum_bytes).unwrap_or(u64::MAX) > self.reader.remaining()? {
            return Err(Error::invalid_data(format!(
                "{field} needs at least {minimum_bytes} bytes beyond the bounded object payload"
            )));
        }
        Ok(count)
    }

    fn read_declared_item_count(&mut self, field: &str) -> Result<u32> {
        let value = self.reader.read_u32()?;
        self.validate_u32_count(value, field)?;
        Ok(value)
    }

    fn validate_u32_count(&mut self, value: u32, field: &str) -> Result<()> {
        let count = usize::try_from(value)
            .map_err(|_| Error::invalid_data(format!("{field} does not fit in usize")))?;
        if count > self.limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "{field} {count} exceeds limit {}",
                self.limits.maximum_array_elements
            )));
        }
        self.charge_array_elements(count, field)
    }

    fn read_packed_bytes(&mut self, field: &str) -> Result<ByteRegion> {
        self.read_byte_region(field, true)
    }

    fn read_byte_region(&mut self, field: &str, align_after: bool) -> Result<ByteRegion> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        let length = u64::try_from(length)
            .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))?;
        if length > self.limits.maximum_packed_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_packed_bytes
            )));
        }
        self.budget.packed_bytes = self
            .budget
            .packed_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("AnimationClip packed-byte budget overflowed"))?;
        if self.budget.packed_bytes > self.limits.maximum_total_packed_bytes {
            return Err(Error::invalid_data(format!(
                "AnimationClip packed data totals {} bytes, exceeding limit {}",
                self.budget.packed_bytes, self.limits.maximum_total_packed_bytes
            )));
        }
        let region = self.take_region(length, field)?;
        if align_after {
            self.align(4)?;
        }
        Ok(ByteRegion {
            region,
            byte_length: length,
        })
    }

    fn read_f32_array(&mut self, field: &str, sample_values: bool) -> Result<F32Array> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if sample_values {
            if count > self.limits.maximum_sample_values {
                return Err(Error::invalid_data(format!(
                    "{field} count {count} exceeds sample limit {}",
                    self.limits.maximum_sample_values
                )));
            }
            self.budget.sample_values = self
                .budget
                .sample_values
                .checked_add(count)
                .ok_or_else(|| Error::invalid_data("AnimationClip sample budget overflowed"))?;
            if self.budget.sample_values > self.limits.maximum_total_sample_values {
                return Err(Error::invalid_data(format!(
                    "AnimationClip samples total {}, exceeding limit {}",
                    self.budget.sample_values, self.limits.maximum_total_sample_values
                )));
            }
        } else {
            if count > self.limits.maximum_array_elements {
                return Err(Error::invalid_data(format!(
                    "{field} count {count} exceeds limit {}",
                    self.limits.maximum_array_elements
                )));
            }
            self.charge_array_elements(count, field)?;
        }
        let byte_length = array_byte_length(count, 4, field)?;
        let region = self.take_region(byte_length, field)?;
        Ok(F32Array {
            region,
            count,
            endian: self.endian,
        })
    }

    fn read_u32_array(&mut self, field: &str, maximum: usize) -> Result<U32Array> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > maximum {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {maximum}"
            )));
        }
        self.charge_array_elements(count, field)?;
        let byte_length = array_byte_length(count, 4, field)?;
        let region = self.take_region(byte_length, field)?;
        Ok(U32Array {
            region,
            count,
            endian: self.endian,
        })
    }

    fn read_i32_array(&mut self, field: &str) -> Result<I32Array> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > self.limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {}",
                self.limits.maximum_array_elements
            )));
        }
        self.charge_array_elements(count, field)?;
        let byte_length = array_byte_length(count, 4, field)?;
        let region = self.take_region(byte_length, field)?;
        Ok(I32Array {
            region,
            count,
            endian: self.endian,
        })
    }

    fn take_region(&mut self, length: u64, field: &str) -> Result<Region> {
        let position = self.reader.position()?;
        let region = self.region.subregion(position, length).map_err(|error| {
            Error::invalid_data(format!(
                "{field} exceeds the bounded object payload: {error}"
            ))
        })?;
        self.skip(length, field)?;
        Ok(region)
    }

    fn charge_array_elements(&mut self, count: usize, field: &str) -> Result<()> {
        self.budget.array_elements = self
            .budget
            .array_elements
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("AnimationClip array budget overflowed"))?;
        if self.budget.array_elements > self.limits.maximum_total_array_elements {
            return Err(Error::invalid_data(format!(
                "AnimationClip arrays total {} elements while reading {field}, exceeding limit {}",
                self.budget.array_elements, self.limits.maximum_total_array_elements
            )));
        }
        Ok(())
    }

    fn charge_allocation(&mut self, bytes: u64, field: &str) -> Result<()> {
        self.budget.allocation_bytes = self
            .budget
            .allocation_bytes
            .checked_add(bytes)
            .ok_or_else(|| Error::invalid_data("AnimationClip allocation budget overflowed"))?;
        if self.budget.allocation_bytes > self.limits.maximum_total_allocation_bytes {
            return Err(Error::invalid_data(format!(
                "AnimationClip allocations total {} bytes while reading {field}, exceeding limit {}",
                self.budget.allocation_bytes, self.limits.maximum_total_allocation_bytes
            )));
        }
        Ok(())
    }

    fn reserve<T>(&mut self, count: usize, field: &str) -> Result<Vec<T>> {
        let bytes = count
            .checked_mul(size_of::<T>())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| Error::invalid_data(format!("{field} allocation size overflowed")))?;
        self.charge_allocation(bytes, field)?;
        let mut values = Vec::new();
        values.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!("cannot allocate {count} {field}: {error}"))
        })?;
        Ok(values)
    }

    fn check_reference_array_budget(&self, count: usize, field: &str) -> Result<()> {
        let bytes = u64::try_from(count)
            .ok()
            .and_then(|count| {
                u64::try_from(self.pptr_size())
                    .ok()
                    .and_then(|size| count.checked_mul(size))
            })
            .ok_or_else(|| Error::invalid_data(format!("{field} reference size overflowed")))?;
        let total = self
            .budget
            .reference_bytes
            .checked_add(bytes)
            .ok_or_else(|| Error::invalid_data("AnimationClip reference budget overflowed"))?;
        if total > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "AnimationClip references would total {total} bytes while reading {field}, exceeding limit {}",
                self.limits.maximum_reference_bytes
            )));
        }
        Ok(())
    }

    fn minimum_generic_binding_size(&self) -> Result<usize> {
        8_usize
            .checked_add(self.pptr_size())
            .and_then(|value| value.checked_add(6))
            .and_then(|value| value.checked_add(usize::from(self.version >= (2022, 1, 0))))
            .and_then(|value| value.checked_add(usize::from(self.version >= (2022, 2, 0))))
            .ok_or_else(|| Error::invalid_data("generic binding size overflowed"))
    }

    const fn pptr_size(&self) -> usize {
        if self.format_version < 14 { 8 } else { 12 }
    }

    fn align(&mut self, alignment: u64) -> Result<()> {
        let position = self.reader.position()?;
        let absolute = self
            .absolute_start
            .checked_add(position)
            .ok_or_else(|| Error::invalid_data("AnimationClip alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder == 0 {
            return Ok(());
        }
        self.skip(alignment - remainder, "AnimationClip alignment")
    }

    fn skip(&mut self, length: u64, field: &str) -> Result<()> {
        let target = self
            .reader
            .position()?
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data(format!("{field} position overflowed")))?;
        if target > self.region.len() {
            return Err(Error::invalid_data(format!(
                "{field} ends at {target}, beyond object size {}",
                self.region.len()
            )));
        }
        self.reader.set_position(target)
    }
}

fn array_byte_length(count: usize, width: usize, field: &str) -> Result<u64> {
    count
        .checked_mul(width)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| Error::invalid_data(format!("{field} byte length overflowed")))
}

#[cfg(test)]
mod tests {
    use crate::endian::Endian;
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{
        ANIMATION_CLIP_CLASS_ID, AnimationClipReadLimits, read_animation_clip,
        uses_split_streamed_curve_counts,
    };

    #[test]
    fn reads_2017_3_no_target_v13_curves_muscle_data_bindings_and_events() {
        let version = (2017, 3, 0);
        let payload = standard_payload(version, 13, Endian::Little, true, true);
        let file = parse_asset(13, Endian::Little, -2, "2017.3.0f1", &payload);

        let clip = read_animation_clip(&file, 0, AnimationClipReadLimits::default()).unwrap();
        assert_eq!(clip.name, "Test Clip");
        assert_eq!(clip.path_id, 7);
        assert_eq!(clip.rotation_curves.len(), 1);
        assert_eq!(clip.rotation_curves[0].curve.keyframes.len(), 1);
        assert_eq!(
            clip.rotation_curves[0].curve.keyframes[0].weighted_mode,
            None
        );
        assert_eq!(clip.compressed_rotation_curves.len(), 1);
        assert_eq!(
            clip.compressed_rotation_curves[0]
                .times
                .data
                .read(2)
                .unwrap(),
            [0x12, 0x34]
        );
        assert_eq!(clip.euler_curves.len(), 1);
        assert_eq!(clip.float_curves[0].script.file_id, 2);
        assert_eq!(clip.float_curves[0].script.path_id, 0x1234_5678);
        assert_eq!(clip.pptr_curves[0].keyframes[0].value.path_id, 0x1234_5678);
        let muscle = clip.muscle_clip.as_ref().unwrap();
        assert_eq!(muscle.clip.streamed.data.count, 2);
        assert_eq!(
            muscle.clip.streamed.data.read_values(2).unwrap(),
            [0x0102_0304, 0x0506_0708]
        );
        assert_eq!(
            muscle
                .clip
                .dense
                .samples
                .read_values(2)
                .unwrap()
                .into_iter()
                .map(f32::to_bits)
                .collect::<Vec<_>>(),
            [1.25_f32.to_bits(), 2.5_f32.to_bits()]
        );
        assert_eq!(
            muscle.clip.constant.values.read_values(1).unwrap()[0].to_bits(),
            3.5_f32.to_bits()
        );
        assert_eq!(muscle.clip.binding.as_ref().unwrap().values.len(), 1);
        assert!(muscle.flags.mirror());
        assert!(muscle.flags.height_from_feet());
        assert_eq!(clip.binding_constant.generic_bindings.len(), 1);
        assert_eq!(
            clip.binding_constant.pptr_curve_mapping[0].path_id,
            0x1234_5678
        );
        assert_eq!(clip.events.len(), 1);
        assert_eq!(clip.events[0].function_name, "OnStep");
    }

    #[test]
    fn reads_2018_3_weighted_curves_and_big_endian_source_regions() {
        let version = (2018, 3, 0);
        let payload = standard_payload(version, 22, Endian::Big, false, true);
        let file = parse_asset(22, Endian::Big, 13, "2018.3.0f1", &payload);

        let clip = read_animation_clip(&file, 0, AnimationClipReadLimits::default()).unwrap();
        let key = &clip.rotation_curves[0].curve.keyframes[0];
        assert_eq!(key.weighted_mode, Some(7));
        assert_eq!(key.in_weight.unwrap().x.to_bits(), 20.0_f32.to_bits());
        assert_eq!(
            clip.float_curves[0].curve.keyframes[0].weighted_mode,
            Some(7)
        );
        let muscle = clip.muscle_clip.as_ref().unwrap();
        assert!(muscle.clip.binding.is_none());
        assert_eq!(clip.has_generic_root_transform, Some(true));
        assert_eq!(clip.has_motion_float_curves, Some(false));
        assert_eq!(muscle.indexes.read_values(1).unwrap(), [0x1020_3040]);
        assert_eq!(clip.pptr_curves[0].script.path_id, 0x1_0000_0042);
    }

    #[test]
    fn reads_2022_2_curve_and_binding_flags() {
        let version = (2022, 2, 0);
        let payload = standard_payload(version, 22, Endian::Little, false, true);
        let file = parse_asset(22, Endian::Little, 13, "2022.2.0f1", &payload);

        let clip = read_animation_clip(&file, 0, AnimationClipReadLimits::default()).unwrap();
        assert_eq!(clip.float_curves[0].flags, Some(0x55));
        assert_eq!(clip.pptr_curves[0].flags, Some(0x66));
        let binding = clip.binding_constant.generic_bindings[0];
        assert_eq!(binding.is_int_curve, Some(1));
        assert_eq!(binding.is_serialize_reference_curve, Some(1));
        assert_eq!(
            clip.muscle_clip
                .as_ref()
                .unwrap()
                .clip
                .streamed
                .discrete_curve_count,
            None
        );
    }

    #[test]
    fn reads_2022_3_19_split_streamed_curve_counts() {
        let version = (2022, 3, 19);
        let payload = standard_payload(version, 22, Endian::Little, false, false);
        let file = parse_asset(22, Endian::Little, 13, "2022.3.19f1", &payload);

        let clip = read_animation_clip(&file, 0, AnimationClipReadLimits::default()).unwrap();
        let muscle = clip.muscle_clip.as_ref().unwrap();
        assert_eq!(muscle.clip.streamed.curve_count, 2);
        assert_eq!(muscle.clip.streamed.discrete_curve_count, Some(1));
    }

    #[test]
    fn switches_2023_streamed_counts_and_reads_unity_6000() {
        for (version, text, split) in [
            ((2023, 0, 0), "2023.1.0f1", false),
            ((2023, 2, 7), "2023.2.7f1", false),
            ((2023, 2, 8), "2023.2.8f1", true),
            ((2023, 3, 0), "2023.3.0f1", true),
            ((6000, 0, 0), "6000.0.0f1", true),
            ((6000, 1, 0), "6000.1.0f1", true),
            ((6000, 2, 0), "6000.2.0f1", true),
        ] {
            let payload = standard_payload(version, 22, Endian::Little, false, false);
            let file = parse_asset(22, Endian::Little, 13, text, &payload);
            let streamed = read_animation_clip(&file, 0, AnimationClipReadLimits::default())
                .unwrap()
                .muscle_clip
                .unwrap()
                .clip
                .streamed;
            if split {
                assert_eq!(streamed.curve_count, 2, "{text}");
                assert_eq!(streamed.discrete_curve_count, Some(1), "{text}");
            } else {
                assert_eq!(streamed.curve_count, 0, "{text}");
                assert_eq!(streamed.discrete_curve_count, None, "{text}");
            }
        }
    }

    #[test]
    fn reads_tuanjie_embedded_muscle_and_acl_build_gates() {
        for (patch, build, expected_acl, expected_curve_count, expected_fast) in [
            (48, 2, false, None, None),
            (48, 3, true, None, None),
            (55, 1, true, Some(7), None),
            (55, 4, true, Some(7), Some(true)),
        ] {
            let version = (2022, 3, patch);
            let version_text = format!("2022.3.{patch}t{build}");
            let payload = tuanjie_payload(version, build, Endian::Big, false, true);
            let file = parse_asset(22, Endian::Big, 13, &version_text, &payload);
            let clip = read_animation_clip(&file, 0, AnimationClipReadLimits::default())
                .unwrap_or_else(|error| panic!("{version_text}: {error}"));
            let muscle = clip.muscle_clip.as_ref().unwrap();
            assert_eq!(
                muscle.clip.streamed.data.read_values(2).unwrap(),
                [0x0102_0304, 0x0506_0708],
                "{version_text} embedded m_AnimData must remain little-endian"
            );
            let acl = muscle.clip.acl.as_ref();
            assert_eq!(acl.is_some(), expected_acl, "{version_text}");
            if let Some(acl) = acl {
                assert_eq!(acl.frame_count, 12, "{version_text}");
                assert_eq!(acl.bone_count, 3, "{version_text}");
                assert_eq!(acl.sample_rate().to_bits(), 30.0_f32.to_bits());
                assert_eq!(acl.curve_count, expected_curve_count, "{version_text}");
                assert_eq!(acl.tracks.read(3).unwrap(), [0xaa, 0xbb, 0xcc]);
                assert_eq!(
                    acl.decoder_map.read_values(2).unwrap(),
                    [0x10, 0x20],
                    "{version_text}"
                );
                assert_eq!(acl.use_fast_sample_mode, expected_fast, "{version_text}");
            }
            let streaming = clip.streaming_info.as_ref().unwrap();
            assert_eq!(streaming.offset, 0x1020_3040_5060_7080);
            assert_eq!(streaming.size, 0x1234);
            assert_eq!(streaming.path, "archive:/animation.resS");
        }
    }

    #[test]
    fn reads_tuanjie_61_relocated_curves_direct_acl_and_alignment() {
        let version = (2022, 3, 61);
        let payload = tuanjie_payload(version, 1, Endian::Big, false, true);
        let file = parse_asset(22, Endian::Big, 13, "2022.3.61t1", &payload);
        let clip = read_animation_clip(&file, 0, AnimationClipReadLimits::default()).unwrap();
        assert_eq!(clip.euler_curves.len(), 1);
        assert_eq!(clip.euler_curves[0].path, "RelocatedEuler");
        assert!(clip.position_curves.is_empty());
        assert!(clip.scale_curves.is_empty());
        let acl = clip
            .muscle_clip
            .as_ref()
            .unwrap()
            .clip
            .acl
            .as_ref()
            .unwrap();
        assert_eq!(acl.curve_count, Some(7));
        assert_eq!(acl.use_fast_sample_mode, Some(true));
        assert_eq!(acl.tracks.read(3).unwrap(), [0xaa, 0xbb, 0xcc]);
        assert_eq!(acl.decoder_map.read_values(2).unwrap(), [0x10, 0x20]);
        assert_eq!(
            clip.streaming_info.as_ref().unwrap().path,
            "archive:/animation.resS"
        );
    }

    #[test]
    fn reads_tuanjie_legacy_embedded_relocated_curves_without_muscle() {
        let version = (2022, 3, 55);
        let payload = tuanjie_payload(version, 4, Endian::Big, true, true);
        let file = parse_asset(22, Endian::Big, 13, "2022.3.55t4", &payload);
        let clip = read_animation_clip(&file, 0, AnimationClipReadLimits::default()).unwrap();
        assert!(clip.legacy);
        assert!(clip.muscle_clip.is_none());
        assert!(clip.streaming_info.is_none());
        assert_eq!(clip.euler_curves.len(), 1);
        assert_eq!(clip.euler_curves[0].path, "LegacyEuler");
        assert!(clip.position_curves.is_empty());
        assert!(clip.scale_curves.is_empty());
    }

    #[test]
    fn enforces_tuanjie_acl_and_embedded_animation_data_limits() {
        let direct_payload = tuanjie_payload((2022, 3, 61), 1, Endian::Little, false, false);
        let direct = parse_asset(22, Endian::Little, 13, "2022.3.61t1", &direct_payload);
        let error = read_animation_clip(
            &direct,
            0,
            AnimationClipReadLimits {
                maximum_packed_bytes: 2,
                ..AnimationClipReadLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("ACL tracks"));

        let embedded_payload = tuanjie_payload((2022, 3, 55), 4, Endian::Little, false, false);
        let embedded = parse_asset(22, Endian::Little, 13, "2022.3.55t4", &embedded_payload);
        let error = read_animation_clip(
            &embedded,
            0,
            AnimationClipReadLimits {
                maximum_packed_bytes: 16,
                ..AnimationClipReadLimits::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("m_AnimData"));
    }

    #[test]
    fn rejects_unverified_versions_tuanjie_truncation_and_trailing_bytes() {
        let payload = standard_payload((2022, 2, 0), 22, Endian::Little, false, false);
        let tuanjie = parse_asset(22, Endian::Little, 13, "2023.1.0t1", &payload);
        assert!(
            read_animation_clip(&tuanjie, 0, AnimationClipReadLimits::default())
                .unwrap_err()
                .to_string()
                .contains("Tuanjie")
        );

        let old = parse_asset(22, Endian::Little, 13, "2017.2.9f1", &payload);
        assert!(read_animation_clip(&old, 0, AnimationClipReadLimits::default()).is_err());
        for version in ["2024.1.0f1", "6000.3.0f1"] {
            let unverified = parse_asset(22, Endian::Little, 13, version, &payload);
            assert!(
                read_animation_clip(&unverified, 0, AnimationClipReadLimits::default()).is_err(),
                "{version}"
            );
        }

        let truncated = parse_asset(
            22,
            Endian::Little,
            13,
            "2022.2.0f1",
            &payload[..payload.len() - 1],
        );
        assert!(read_animation_clip(&truncated, 0, AnimationClipReadLimits::default()).is_err());

        let mut trailing = payload;
        trailing.extend_from_slice(&[0; 4]);
        let trailing = parse_asset(22, Endian::Little, 13, "2022.2.0f1", &trailing);
        let error =
            read_animation_clip(&trailing, 0, AnimationClipReadLimits::default()).unwrap_err();
        assert!(error.to_string().contains("trailing bytes"));
    }

    #[test]
    fn enforces_curve_keyframe_packed_sample_reference_and_allocation_limits() {
        let payload = standard_payload((2022, 2, 0), 22, Endian::Little, true, true);
        let file = parse_asset(22, Endian::Little, -2, "2022.2.0f1", &payload);

        for limits in [
            AnimationClipReadLimits {
                maximum_curves: 0,
                ..AnimationClipReadLimits::default()
            },
            AnimationClipReadLimits {
                maximum_keyframes_per_curve: 0,
                ..AnimationClipReadLimits::default()
            },
            AnimationClipReadLimits {
                maximum_packed_bytes: 1,
                ..AnimationClipReadLimits::default()
            },
            AnimationClipReadLimits {
                maximum_sample_values: 1,
                ..AnimationClipReadLimits::default()
            },
            AnimationClipReadLimits {
                maximum_reference_bytes: 0,
                ..AnimationClipReadLimits::default()
            },
            AnimationClipReadLimits {
                maximum_total_allocation_bytes: 0,
                ..AnimationClipReadLimits::default()
            },
        ] {
            assert!(read_animation_clip(&file, 0, limits).is_err());
        }
    }

    fn standard_payload(
        version: (u32, u32, u32),
        format_version: u32,
        endian: Endian,
        no_target: bool,
        rich: bool,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        if no_target {
            push_u32(&mut output, 0x1122_3344, endian);
            push_pptr(&mut output, 0, 11, format_version, endian);
            push_pptr(&mut output, 0, 12, format_version, endian);
        }
        push_string(&mut output, "Test Clip", endian);
        output.extend_from_slice(&[0, 1, 1]);
        align(&mut output, 4);

        push_i32(&mut output, i32::from(rich), endian);
        if rich {
            push_quaternion_curve(&mut output, version, endian);
            push_string(&mut output, "Root", endian);
        }
        push_i32(&mut output, i32::from(rich), endian);
        if rich {
            push_compressed_curve(&mut output, endian);
        }
        push_i32(&mut output, i32::from(rich), endian);
        if rich {
            push_vector3_curve(&mut output, version, endian);
            push_string(&mut output, "Euler", endian);
        }
        push_i32(&mut output, 0, endian);
        push_i32(&mut output, 0, endian);

        push_i32(&mut output, i32::from(rich), endian);
        if rich {
            push_float_curve(&mut output, version, format_version, endian);
        }
        push_i32(&mut output, i32::from(rich), endian);
        if rich {
            push_pptr_curve(&mut output, version, format_version, endian);
        }
        push_f32(&mut output, 60.0, endian);
        push_i32(&mut output, 2, endian);
        push_floats(&mut output, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], endian);
        push_u32(&mut output, 0, endian);
        push_muscle_constant(&mut output, version, endian, rich);
        push_binding_constant(&mut output, version, format_version, endian, rich);
        if version >= (2018, 3, 0) {
            output.extend_from_slice(&[1, 0]);
            align(&mut output, 4);
        }
        push_i32(&mut output, i32::from(rich), endian);
        if rich {
            push_event(&mut output, format_version, endian);
        }
        align(&mut output, 4);
        output
    }

    fn tuanjie_payload(
        version: (u32, u32, u32),
        version_build: u32,
        endian: Endian,
        legacy: bool,
        rich: bool,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_string(&mut output, "Tuanjie Clip", endian);
        output.extend_from_slice(&[u8::from(legacy), 1, 1]);
        align(&mut output, 4);

        push_i32(&mut output, 0, endian);
        push_i32(&mut output, 0, endian);
        push_i32(&mut output, 0, endian);
        push_i32(&mut output, 0, endian);
        push_f32(&mut output, 60.0, endian);
        push_i32(&mut output, 2, endian);
        push_floats(&mut output, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], endian);

        if version >= (2022, 3, 61) {
            push_i32(&mut output, i32::from(rich), endian);
            if rich {
                push_vector3_curve(&mut output, version, endian);
                push_string(&mut output, "RelocatedEuler", endian);
            }
            push_i32(&mut output, 0, endian);
            push_i32(&mut output, 0, endian);
            push_u32(&mut output, 0, endian);
            push_muscle_constant_for_engine(
                &mut output,
                version,
                version_build,
                true,
                endian,
                rich,
            );
            push_animation_streaming_info(&mut output, endian);
        } else {
            let mut embedded = Vec::new();
            if legacy {
                push_i32(&mut embedded, i32::from(rich), Endian::Little);
                if rich {
                    push_vector3_curve(&mut embedded, version, Endian::Little);
                    push_string(&mut embedded, "LegacyEuler", Endian::Little);
                }
                push_i32(&mut embedded, 0, Endian::Little);
                push_i32(&mut embedded, 0, Endian::Little);
            } else {
                push_muscle_constant_for_engine(
                    &mut embedded,
                    version,
                    version_build,
                    true,
                    Endian::Little,
                    rich,
                );
            }
            push_u32(&mut output, u32::try_from(embedded.len()).unwrap(), endian);
            push_i32(&mut output, i32::try_from(embedded.len()).unwrap(), endian);
            output.extend_from_slice(&embedded);
            if !legacy {
                push_animation_streaming_info(&mut output, endian);
            }
        }

        push_binding_constant(&mut output, version, 22, endian, false);
        output.extend_from_slice(&[1, 0]);
        align(&mut output, 4);
        push_i32(&mut output, 0, endian);
        align(&mut output, 4);
        output
    }

    fn push_animation_streaming_info(output: &mut Vec<u8>, endian: Endian) {
        push_i64(output, 0x1020_3040_5060_7080, endian);
        push_u32(output, 0x1234, endian);
        push_string(output, "archive:/animation.resS", endian);
    }

    fn push_quaternion_curve(output: &mut Vec<u8>, version: (u32, u32, u32), endian: Endian) {
        push_i32(output, 1, endian);
        push_f32(output, 0.25, endian);
        push_floats(
            output,
            &[
                1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0,
            ],
            endian,
        );
        if version.0 >= 2018 {
            push_i32(output, 7, endian);
            push_floats(
                output,
                &[20.0, 21.0, 22.0, 23.0, 24.0, 25.0, 26.0, 27.0],
                endian,
            );
        }
        push_curve_tail(output, endian);
    }

    fn push_vector3_curve(output: &mut Vec<u8>, version: (u32, u32, u32), endian: Endian) {
        push_i32(output, 1, endian);
        push_f32(output, 0.5, endian);
        push_floats(
            output,
            &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
            endian,
        );
        if version.0 >= 2018 {
            push_i32(output, 7, endian);
            push_floats(output, &[20.0, 21.0, 22.0, 23.0, 24.0, 25.0], endian);
        }
        push_curve_tail(output, endian);
    }

    fn push_float_curve(
        output: &mut Vec<u8>,
        version: (u32, u32, u32),
        format_version: u32,
        endian: Endian,
    ) {
        push_i32(output, 1, endian);
        push_floats(output, &[0.0, 1.0, 2.0, 3.0], endian);
        if version.0 >= 2018 {
            push_i32(output, 7, endian);
            push_floats(output, &[0.25, 0.75], endian);
        }
        push_curve_tail(output, endian);
        push_string(output, "speed", endian);
        push_string(output, "Root", endian);
        push_i32(output, 95, endian);
        push_pptr(
            output,
            2,
            reference_path(format_version),
            format_version,
            endian,
        );
        if version >= (2022, 2, 0) {
            push_i32(output, 0x55, endian);
        }
    }

    fn push_pptr_curve(
        output: &mut Vec<u8>,
        version: (u32, u32, u32),
        format_version: u32,
        endian: Endian,
    ) {
        push_i32(output, 1, endian);
        push_f32(output, 0.75, endian);
        push_pptr(
            output,
            1,
            reference_path(format_version),
            format_version,
            endian,
        );
        push_string(output, "sprite", endian);
        push_string(output, "Root", endian);
        push_i32(output, 212, endian);
        push_pptr(
            output,
            3,
            reference_path(format_version),
            format_version,
            endian,
        );
        if version >= (2022, 2, 0) {
            push_i32(output, 0x66, endian);
        }
    }

    fn push_compressed_curve(output: &mut Vec<u8>, endian: Endian) {
        push_string(output, "Compressed", endian);
        push_u32(output, 1, endian);
        push_bytes_array(output, &[0x12, 0x34], endian);
        output.push(8);
        align(output, 4);
        push_u32(output, 1, endian);
        push_bytes_array(output, &[0x56, 0x78, 0x9a], endian);
        push_u32(output, 1, endian);
        push_floats(output, &[2.0, 1.0], endian);
        push_bytes_array(output, &[0xbc, 0xde], endian);
        output.push(8);
        align(output, 4);
        push_i32(output, 1, endian);
        push_i32(output, 2, endian);
    }

    fn push_curve_tail(output: &mut Vec<u8>, endian: Endian) {
        push_i32(output, 1, endian);
        push_i32(output, 2, endian);
        push_i32(output, 3, endian);
    }

    fn push_muscle_constant(
        output: &mut Vec<u8>,
        version: (u32, u32, u32),
        endian: Endian,
        rich: bool,
    ) {
        push_muscle_constant_for_engine(output, version, 1, false, endian, rich);
    }

    fn push_muscle_constant_for_engine(
        output: &mut Vec<u8>,
        version: (u32, u32, u32),
        version_build: u32,
        is_tuanjie: bool,
        endian: Endian,
        rich: bool,
    ) {
        push_xform(output, endian);
        push_floats(output, &[0.0; 7], endian);
        push_i32(output, i32::from(rich), endian);
        if rich {
            push_xform(output, endian);
            push_floats(output, &[0.0; 6], endian);
        }
        push_hand_pose(output, endian, rich);
        push_hand_pose(output, endian, rich);
        push_f32_array(output, if rich { &[2.0] } else { &[] }, endian);
        push_i32(output, i32::from(rich), endian);
        if rich {
            push_floats(output, &[3.0, 4.0, 5.0], endian);
        }

        for _ in 0..4 {
            push_xform(output, endian);
        }
        push_floats(output, &[1.0, 2.0, 3.0], endian);
        let streamed = if rich {
            &[0x0102_0304, 0x0506_0708][..]
        } else {
            &[]
        };
        push_u32_array(output, streamed, endian);
        if uses_split_streamed_curve_counts(version) {
            push_u16(output, 2, endian);
            push_u16(output, 1, endian);
        } else {
            push_u32(output, u32::from(rich), endian);
        }
        push_i32(output, i32::from(rich), endian);
        push_u32(output, u32::from(rich), endian);
        push_floats(output, &[30.0, 0.0], endian);
        push_f32_array(output, if rich { &[1.25, 2.5] } else { &[] }, endian);
        push_f32_array(output, if rich { &[3.5] } else { &[] }, endian);
        if version < (2018, 3, 0) {
            push_i32(output, i32::from(rich), endian);
            if rich {
                push_u32(output, 10, endian);
                push_u32(output, 20, endian);
                push_u32(output, 30, endian);
            }
        }
        if is_tuanjie
            && (version > (2022, 3, 48) || (version == (2022, 3, 48) && version_build >= 3))
        {
            push_u32(output, 12, endian);
            push_u32(output, 3, endian);
            push_f32(output, 30.0, endian);
            if version >= (2022, 3, 55) {
                push_u32(output, 7, endian);
            }
            push_i32(output, 3, endian);
            output.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
            if version >= (2022, 3, 61) {
                align(output, 4);
            }
            push_u32_array(output, &[0x10, 0x20], endian);
            if version > (2022, 3, 55) || (version == (2022, 3, 55) && version_build >= 4) {
                output.push(1);
                if version >= (2022, 3, 61) {
                    align(output, 4);
                }
            }
        }
        push_floats(output, &[0.0; 6], endian);
        push_i32_array(output, if rich { &[0x1020_3040] } else { &[] }, endian);
        push_i32(output, i32::from(rich), endian);
        if rich {
            push_floats(output, &[6.0, 7.0], endian);
        }
        push_f32_array(output, if rich { &[8.0] } else { &[] }, endian);
        output.extend_from_slice(&[1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1]);
        align(output, 4);
    }

    fn push_hand_pose(output: &mut Vec<u8>, endian: Endian, rich: bool) {
        push_xform(output, endian);
        push_f32_array(output, if rich { &[1.0] } else { &[] }, endian);
        push_floats(output, &[0.0; 4], endian);
    }

    fn push_xform(output: &mut Vec<u8>, endian: Endian) {
        push_floats(output, &[0.0; 10], endian);
    }

    fn push_binding_constant(
        output: &mut Vec<u8>,
        version: (u32, u32, u32),
        format_version: u32,
        endian: Endian,
        rich: bool,
    ) {
        push_i32(output, i32::from(rich), endian);
        if rich {
            push_u32(output, 100, endian);
            push_u32(output, 200, endian);
            push_pptr(
                output,
                4,
                reference_path(format_version),
                format_version,
                endian,
            );
            push_i32(output, 4, endian);
            output.extend_from_slice(&[5, 1]);
            if version >= (2022, 1, 0) {
                output.push(1);
            }
            if version >= (2022, 2, 0) {
                output.push(1);
            }
            align(output, 4);
        }
        push_i32(output, i32::from(rich), endian);
        if rich {
            push_pptr(
                output,
                5,
                reference_path(format_version),
                format_version,
                endian,
            );
        }
    }

    fn push_event(output: &mut Vec<u8>, format_version: u32, endian: Endian) {
        push_f32(output, 0.5, endian);
        push_string(output, "OnStep", endian);
        push_string(output, "left", endian);
        push_pptr(
            output,
            6,
            reference_path(format_version),
            format_version,
            endian,
        );
        push_f32(output, 1.5, endian);
        push_i32(output, 9, endian);
        push_i32(output, 2, endian);
    }

    fn parse_asset(
        format_version: u32,
        endian: Endian,
        target_platform: i32,
        unity_version: &str,
        payload: &[u8],
    ) -> SerializedFile {
        let bytes = if format_version == 13 {
            synthetic_v13(endian, target_platform, unity_version, payload)
        } else {
            synthetic_v22(endian, target_platform, unity_version, payload)
        };
        SerializedFile::open(Region::from_bytes(bytes)).unwrap()
    }

    fn synthetic_v22(
        endian: Endian,
        target_platform: i32,
        unity_version: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, target_platform, endian);
        metadata.push(0);
        push_i32(&mut metadata, 1, endian);
        push_i32(&mut metadata, ANIMATION_CLIP_CLASS_ID, endian);
        metadata.push(0);
        push_i16(&mut metadata, -1, endian);
        metadata.extend_from_slice(&[0; 16]);
        push_i32(&mut metadata, 1, endian);
        align_with_base(&mut metadata, 48, 4);
        push_i64(&mut metadata, 7, endian);
        push_i64(&mut metadata, 0, endian);
        push_u32(&mut metadata, u32::try_from(payload.len()).unwrap(), endian);
        push_i32(&mut metadata, 0, endian);
        push_i32(&mut metadata, 0, endian);
        push_i32(&mut metadata, 0, endian);
        push_i32(&mut metadata, 0, endian);
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (48_u64 + u64::from(metadata_size)).div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(payload.len()).unwrap();
        let mut bytes = vec![0; 48];
        bytes[8..12].copy_from_slice(&22_u32.to_be_bytes());
        bytes[16] = u8::from(endian == Endian::Big);
        bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.resize(usize::try_from(data_offset).unwrap(), 0);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn synthetic_v13(
        endian: Endian,
        target_platform: i32,
        unity_version: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        push_i32(&mut metadata, target_platform, endian);
        metadata.push(0);
        push_i32(&mut metadata, 1, endian);
        push_i32(&mut metadata, ANIMATION_CLIP_CLASS_ID, endian);
        metadata.extend_from_slice(&[0; 16]);
        push_i32(&mut metadata, 0, endian);
        push_i32(&mut metadata, 1, endian);
        push_i32(&mut metadata, 7, endian);
        push_u32(&mut metadata, 0, endian);
        push_u32(&mut metadata, u32::try_from(payload.len()).unwrap(), endian);
        push_i32(&mut metadata, ANIMATION_CLIP_CLASS_ID, endian);
        push_u16(
            &mut metadata,
            u16::try_from(ANIMATION_CLIP_CLASS_ID).unwrap(),
            endian,
        );
        push_i16(&mut metadata, -1, endian);
        push_i32(&mut metadata, 0, endian);
        push_i32(&mut metadata, 0, endian);
        metadata.push(0);

        let data_offset = (20 + metadata.len()).div_ceil(16) * 16;
        let file_size = data_offset + payload.len();
        let mut bytes = vec![0; 20];
        bytes[0..4].copy_from_slice(&u32::try_from(metadata.len()).unwrap().to_be_bytes());
        bytes[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
        bytes[8..12].copy_from_slice(&13_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&u32::try_from(data_offset).unwrap().to_be_bytes());
        bytes[16] = u8::from(endian == Endian::Big);
        bytes.extend_from_slice(&metadata);
        bytes.resize(data_offset, 0);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn reference_path(format_version: u32) -> i64 {
        if format_version < 14 {
            0x1234_5678
        } else {
            0x1_0000_0042
        }
    }

    fn push_bytes_array(output: &mut Vec<u8>, values: &[u8], endian: Endian) {
        push_i32(output, i32::try_from(values.len()).unwrap(), endian);
        output.extend_from_slice(values);
        align(output, 4);
    }

    fn push_f32_array(output: &mut Vec<u8>, values: &[f32], endian: Endian) {
        push_i32(output, i32::try_from(values.len()).unwrap(), endian);
        push_floats(output, values, endian);
    }

    fn push_u32_array(output: &mut Vec<u8>, values: &[u32], endian: Endian) {
        push_i32(output, i32::try_from(values.len()).unwrap(), endian);
        for value in values {
            push_u32(output, *value, endian);
        }
    }

    fn push_i32_array(output: &mut Vec<u8>, values: &[i32], endian: Endian) {
        push_i32(output, i32::try_from(values.len()).unwrap(), endian);
        for value in values {
            push_i32(output, *value, endian);
        }
    }

    fn push_pptr(
        output: &mut Vec<u8>,
        file_id: i32,
        path_id: i64,
        format_version: u32,
        endian: Endian,
    ) {
        push_i32(output, file_id, endian);
        if format_version < 14 {
            push_i32(output, i32::try_from(path_id).unwrap(), endian);
        } else {
            push_i64(output, path_id, endian);
        }
    }

    fn push_string(output: &mut Vec<u8>, value: &str, endian: Endian) {
        push_i32(output, i32::try_from(value.len()).unwrap(), endian);
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn push_floats(output: &mut Vec<u8>, values: &[f32], endian: Endian) {
        for value in values {
            push_f32(output, *value, endian);
        }
    }

    fn push_f32(output: &mut Vec<u8>, value: f32, endian: Endian) {
        push_u32(output, value.to_bits(), endian);
    }

    fn push_u16(output: &mut Vec<u8>, value: u16, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn push_i16(output: &mut Vec<u8>, value: i16, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn push_i32(output: &mut Vec<u8>, value: i32, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn push_u32(output: &mut Vec<u8>, value: u32, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn push_i64(output: &mut Vec<u8>, value: i64, endian: Endian) {
        match endian {
            Endian::Little => output.extend_from_slice(&value.to_le_bytes()),
            Endian::Big => output.extend_from_slice(&value.to_be_bytes()),
        }
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
