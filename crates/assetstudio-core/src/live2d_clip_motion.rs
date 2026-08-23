//! Bounded projection of Unity `AnimationClip` bindings into Cubism `motion3.json`.

use std::collections::{HashMap, hash_map::Entry};
use std::io::Write;

use crate::acl::{
    AclCompressedTracksLimits, AclDecodeLimits, AclDecodedClip, AclDecoder, AclDecoderInputLimits,
};
use crate::animation_clip::{
    ANIMATION_CLIP_CLASS_ID, AnimationClip, AnimationClipReadLimits, ClipMuscleConstant,
    GenericBinding, MuscleClipData, read_animation_clip,
};
use crate::live2d_motion::{
    CubismMotionKeyframe, CubismMotionTargetNames, MotionCurveView, MotionDocumentView,
    MotionEventView, write_motion_document,
};
use crate::loader::AssetCollection;
use crate::model_animation::unity_crc32;
use crate::monobehaviour::{MONO_SCRIPT_CLASS_ID, MonoBehaviourReadLimits, read_mono_script};
use crate::scene::resolve_object_reference;
use crate::scene_hierarchy::SceneObjectKey;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CubismClipMotionReadLimits {
    pub maximum_curves: usize,
    pub maximum_keyframes: usize,
    pub maximum_events: usize,
    pub maximum_streamed_frames: usize,
    pub maximum_streamed_keys: usize,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_output_bytes: u64,
    pub clip: AnimationClipReadLimits,
    pub mono_script: MonoBehaviourReadLimits,
}

impl Default for CubismClipMotionReadLimits {
    fn default() -> Self {
        Self {
            maximum_curves: 1_000_000,
            maximum_keyframes: 10_000_000,
            maximum_events: 1_000_000,
            maximum_streamed_frames: 2_000_000,
            maximum_streamed_keys: 10_000_000,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 128 * 1024 * 1024,
            maximum_output_bytes: 512 * 1024 * 1024,
            clip: AnimationClipReadLimits::default(),
            mono_script: MonoBehaviourReadLimits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CubismClipMotionCurve {
    pub target: String,
    pub id: String,
    pub keyframes: Vec<CubismMotionKeyframe>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CubismClipMotionEvent {
    pub time: f32,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CubismClipMotion {
    pub object: SceneObjectKey,
    pub name: String,
    pub duration: f32,
    pub fps: f32,
    pub curves: Vec<CubismClipMotionCurve>,
    pub events: Vec<CubismClipMotionEvent>,
}

impl CubismClipMotion {
    pub fn write_motion3_json(
        &self,
        force_bezier: bool,
        output: &mut impl Write,
        maximum_bytes: u64,
    ) -> Result<u64> {
        let mut curves = Vec::new();
        curves.try_reserve(self.curves.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate clip-motion curve views: {error}"))
        })?;
        for curve in &self.curves {
            curves.push(MotionCurveView {
                target: &curve.target,
                id: &curve.id,
                fade_in_time: -1.0,
                fade_out_time: -1.0,
                keyframes: &curve.keyframes,
            });
        }
        let mut events = Vec::new();
        events.try_reserve(self.events.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate clip-motion event views: {error}"))
        })?;
        for event in &self.events {
            events.push(MotionEventView {
                time: event.time,
                value: &event.value,
            });
        }
        write_motion_document(
            &MotionDocumentView {
                duration: self.duration,
                fps: self.fps,
                fade_in_time: 0.0,
                fade_out_time: 0.0,
                curves: &curves,
                events: &events,
                initial_time_is_zero: true,
            },
            force_bezier,
            output,
            maximum_bytes,
        )
    }
}

/// Reads one real Unity `AnimationClip` and projects only bindings understood
/// by Cubism's V2 parameter/part/script convention.
pub fn read_cubism_clip_motion(
    collection: &AssetCollection,
    file_index: usize,
    object_index: usize,
    targets: &CubismMotionTargetNames,
    limits: CubismClipMotionReadLimits,
) -> Result<CubismClipMotion> {
    read_cubism_clip_motion_with_acl_decoder(
        collection,
        file_index,
        object_index,
        targets,
        limits,
        None,
    )
}

pub fn read_cubism_clip_motion_with_acl_decoder(
    collection: &AssetCollection,
    file_index: usize,
    object_index: usize,
    targets: &CubismMotionTargetNames,
    limits: CubismClipMotionReadLimits,
    acl_decoder: Option<&dyn AclDecoder>,
) -> Result<CubismClipMotion> {
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
    if object.class_id != ANIMATION_CLIP_CLASS_ID {
        return Err(Error::invalid_data(format!(
            "object {} has class {}, expected AnimationClip ({ANIMATION_CLIP_CLASS_ID})",
            object.path_id, object.class_id
        )));
    }
    let clip = read_animation_clip(&loaded.file, object_index, limits.clip)?;
    project_cubism_clip_motion_with_acl_decoder(
        collection,
        file_index,
        &clip,
        targets,
        limits,
        acl_decoder,
    )
}

pub fn project_cubism_clip_motion(
    collection: &AssetCollection,
    file_index: usize,
    clip: &AnimationClip,
    targets: &CubismMotionTargetNames,
    limits: CubismClipMotionReadLimits,
) -> Result<CubismClipMotion> {
    project_cubism_clip_motion_with_acl_decoder(collection, file_index, clip, targets, limits, None)
}

pub fn project_cubism_clip_motion_with_acl_decoder(
    collection: &AssetCollection,
    file_index: usize,
    clip: &AnimationClip,
    targets: &CubismMotionTargetNames,
    limits: CubismClipMotionReadLimits,
    acl_decoder: Option<&dyn AclDecoder>,
) -> Result<CubismClipMotion> {
    let muscle = clip.muscle_clip.as_ref().ok_or_else(|| {
        Error::unsupported(
            "Cubism motion projection requires an AnimationClip muscle clip; legacy Tuanjie m_AnimData contains only explicit curves",
        )
    })?;
    let decoded_acl = muscle
        .clip
        .acl
        .as_ref()
        .map(|acl| {
            let decoder = acl_decoder.ok_or_else(|| {
                Error::unsupported(
                    "Cubism motion projection requires an injected Tuanjie ACL decoder",
                )
            })?;
            acl.decode_with(
                decoder,
                AclDecodeLimits {
                    input: AclDecoderInputLimits {
                        compressed_tracks: AclCompressedTracksLimits {
                            maximum_compressed_bytes: limits.clip.maximum_packed_bytes,
                            ..AclCompressedTracksLimits::default()
                        },
                        maximum_decoder_map_entries: limits.clip.maximum_array_elements,
                        maximum_materialized_bytes: limits.clip.maximum_total_packed_bytes,
                    },
                    maximum_frames: limits.maximum_keyframes,
                    maximum_curves: limits.maximum_curves,
                    maximum_values: limits.maximum_keyframes,
                },
            )
        })
        .transpose()?;
    let target_index = TargetIndex::build(targets)?;
    let layout = BindingLayout::build(&clip.binding_constant.generic_bindings)?;
    let mut builder = ClipBuilder::new(collection, file_index, limits, target_index, layout)?;
    builder.append_muscle_clip(muscle, decoded_acl.as_ref())?;
    let name = builder.copy_string(&clip.name, "AnimationClip name")?;
    let mut events = Vec::new();
    if clip.events.len() > limits.maximum_events {
        return Err(limit_error(
            "events",
            clip.events.len(),
            limits.maximum_events,
        ));
    }
    events.try_reserve(clip.events.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate Cubism motion events: {error}"))
    })?;
    for event in &clip.events {
        let time = finite_f32(event.time, "AnimationClip event time")?;
        let value = builder.copy_string(&event.data, "AnimationClip event data")?;
        events.push(CubismClipMotionEvent { time, value });
    }
    let duration = finite_f32(muscle.stop_time, "AnimationClip stop time")?;
    let fps = finite_f32(clip.sample_rate, "AnimationClip sample rate")?;
    if fps <= 0.0 {
        return Err(Error::invalid_data(
            "AnimationClip sample rate must be positive for motion3 output",
        ));
    }
    let motion = CubismClipMotion {
        object: SceneObjectKey {
            file_index,
            path_id: clip.path_id,
        },
        name,
        duration,
        fps,
        curves: builder.curves,
        events,
    };
    crate::error::output_validation(motion.write_motion3_json(
        false,
        &mut std::io::sink(),
        limits.maximum_output_bytes,
    ))?;
    Ok(motion)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MotionTargetKey {
    Named(usize),
    Opacity,
    EyeBlink,
    LipSync,
}

#[derive(Debug, Clone, Copy)]
struct NamedTarget<'a> {
    target: &'static str,
    id: &'a str,
}

struct TargetIndex<'a> {
    named: Vec<NamedTarget<'a>>,
    by_hash: HashMap<u32, Option<MotionTargetKey>>,
}

impl<'a> TargetIndex<'a> {
    fn build(targets: &'a CubismMotionTargetNames) -> Result<Self> {
        let capacity = targets
            .parameters
            .len()
            .checked_add(targets.parts.len())
            .ok_or_else(|| Error::invalid_data("Cubism target count overflowed"))?;
        let mut named = Vec::new();
        named.try_reserve(capacity).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Cubism target names: {error}"))
        })?;
        for id in &targets.parameters {
            push_named_target(&mut named, "Parameter", id);
        }
        for id in &targets.parts {
            push_named_target(&mut named, "PartOpacity", id);
        }
        let mut by_hash = HashMap::new();
        by_hash
            .try_reserve(named.len().saturating_mul(2))
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Cubism target hash index: {error}"))
            })?;
        for (index, target) in named.iter().enumerate() {
            let prefix = if target.target == "Parameter" {
                "Parameters/"
            } else {
                "Parts/"
            };
            let mut path = String::new();
            path.try_reserve(prefix.len().saturating_add(target.id.len()))
                .map_err(|error| {
                    Error::invalid_data(format!("cannot allocate Cubism binding path: {error}"))
                })?;
            path.push_str(prefix);
            path.push_str(target.id);
            loop {
                insert_target_hash(
                    &mut by_hash,
                    unity_crc32(path.as_bytes()),
                    MotionTargetKey::Named(index),
                );
                let Some(slash) = path.find('/') else {
                    break;
                };
                path.drain(..=slash);
            }
        }
        Ok(Self { named, by_hash })
    }

    fn by_path(&self, hash: u32) -> Option<MotionTargetKey> {
        self.by_hash.get(&hash).copied().flatten()
    }

    fn describe(&self, key: MotionTargetKey) -> (&'static str, &str) {
        match key {
            MotionTargetKey::Named(index) => {
                let target = self.named[index];
                (target.target, target.id)
            }
            MotionTargetKey::Opacity => ("Model", "Opacity"),
            MotionTargetKey::EyeBlink => ("Model", "EyeBlink"),
            MotionTargetKey::LipSync => ("Model", "LipSync"),
        }
    }
}

fn push_named_target<'a>(targets: &mut Vec<NamedTarget<'a>>, target: &'static str, id: &'a str) {
    if !targets
        .iter()
        .any(|candidate| candidate.target == target && candidate.id == id)
    {
        targets.push(NamedTarget { target, id });
    }
}

fn insert_target_hash(
    index: &mut HashMap<u32, Option<MotionTargetKey>>,
    hash: u32,
    target: MotionTargetKey,
) {
    match index.entry(hash) {
        Entry::Vacant(entry) => {
            entry.insert(Some(target));
        }
        Entry::Occupied(mut entry) if entry.get() != &Some(target) => {
            entry.insert(None);
        }
        Entry::Occupied(_) => {}
    }
}

#[derive(Debug, Clone, Copy)]
struct BindingSpan {
    end: usize,
    binding: GenericBinding,
}

struct BindingLayout {
    spans: Vec<BindingSpan>,
}

impl BindingLayout {
    fn build(bindings: &[GenericBinding]) -> Result<Self> {
        let mut spans = Vec::new();
        spans.try_reserve(bindings.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Cubism binding layout: {error}"))
        })?;
        let mut end = 0_usize;
        for binding in bindings {
            end = end
                .checked_add(binding_width(*binding))
                .ok_or_else(|| Error::invalid_data("Cubism binding width overflowed"))?;
            spans.push(BindingSpan {
                end,
                binding: *binding,
            });
        }
        Ok(Self { spans })
    }

    fn find(&self, index: usize) -> Option<GenericBinding> {
        let position = self.spans.partition_point(|span| span.end <= index);
        self.spans.get(position).map(|span| span.binding)
    }
}

const fn binding_width(binding: GenericBinding) -> usize {
    if binding.type_id == crate::scene::TRANSFORM_CLASS_ID {
        match binding.attribute {
            1 | 3 | 4 => 3,
            2 => 4,
            _ => 1,
        }
    } else {
        1
    }
}

#[derive(Debug, Clone)]
struct StreamedKey {
    index: usize,
    coefficients: [f32; 4],
    value: f32,
    in_slope: f32,
    out_slope: f32,
}

#[derive(Debug, Clone)]
struct StreamedFrame {
    time: f32,
    keys: Vec<StreamedKey>,
}

struct ClipBuilder<'a> {
    collection: &'a AssetCollection,
    file_index: usize,
    limits: CubismClipMotionReadLimits,
    targets: TargetIndex<'a>,
    bindings: BindingLayout,
    curves: Vec<CubismClipMotionCurve>,
    curve_index: HashMap<MotionTargetKey, usize>,
    script_targets: HashMap<SceneObjectKey, Option<MotionTargetKey>>,
    keyframes: usize,
    string_bytes: usize,
}

impl<'a> ClipBuilder<'a> {
    fn new(
        collection: &'a AssetCollection,
        file_index: usize,
        limits: CubismClipMotionReadLimits,
        targets: TargetIndex<'a>,
        bindings: BindingLayout,
    ) -> Result<Self> {
        let mut curve_index = HashMap::new();
        curve_index
            .try_reserve(bindings.spans.len().min(limits.maximum_curves))
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Cubism curve index: {error}"))
            })?;
        let mut script_targets = HashMap::new();
        script_targets
            .try_reserve(bindings.spans.len().min(limits.maximum_curves))
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Cubism script index: {error}"))
            })?;
        Ok(Self {
            collection,
            file_index,
            limits,
            targets,
            bindings,
            curves: Vec::new(),
            curve_index,
            script_targets,
            keyframes: 0,
            string_bytes: 0,
        })
    }

    fn append_muscle_clip(
        &mut self,
        muscle: &ClipMuscleConstant,
        decoded_acl: Option<&AclDecodedClip>,
    ) -> Result<()> {
        let following_curve_offset = if let Some(decoded) = decoded_acl {
            self.append_acl(decoded)?;
            usize::try_from(decoded.following_curve_offset)
                .map_err(|_| Error::invalid_data("ACL curve offset does not fit usize"))?
        } else {
            0
        };
        let words = muscle
            .clip
            .streamed
            .data
            .read_values(self.limits.clip.maximum_streamed_words)?;
        let mut frames = parse_streamed_frames(&words, self.limits)?;
        calculate_streamed_in_slopes(&mut frames)?;
        if frames.len() >= 2 {
            for frame in &frames[1..frames.len() - 1] {
                for key in &frame.keys {
                    self.append_sample(
                        following_curve_offset
                            .checked_add(key.index)
                            .ok_or_else(|| {
                                Error::invalid_data("streamed curve index overflowed")
                            })?,
                        frame.time,
                        key.value,
                        key.in_slope,
                        key.out_slope,
                    )?;
                }
            }
        }

        let stream_count = usize::try_from(muscle.clip.streamed.curve_count)
            .map_err(|_| Error::invalid_data("streamed curve count does not fit usize"))?;
        let dense_offset = following_curve_offset
            .checked_add(stream_count)
            .ok_or_else(|| Error::invalid_data("dense curve offset overflowed"))?;
        self.append_dense(&muscle.clip, dense_offset)?;
        let dense_count = usize::try_from(muscle.clip.dense.curve_count)
            .map_err(|_| Error::invalid_data("dense curve count does not fit usize"))?;
        self.append_constant(
            &muscle.clip,
            muscle.stop_time,
            following_curve_offset
                .checked_add(stream_count)
                .and_then(|value| value.checked_add(dense_count))
                .ok_or_else(|| Error::invalid_data("constant curve offset overflowed"))?,
        )
    }

    fn append_acl(&mut self, decoded: &AclDecodedClip) -> Result<()> {
        let curve_count = decoded.binding_indices.len();
        for (frame_index, &time) in decoded.times.iter().enumerate() {
            let frame_offset = frame_index
                .checked_mul(curve_count)
                .ok_or_else(|| Error::invalid_data("ACL frame offset overflowed"))?;
            for (column, &binding_index) in decoded.binding_indices.iter().enumerate() {
                let index = usize::try_from(binding_index)
                    .map_err(|_| Error::invalid_data("ACL binding index does not fit usize"))?;
                self.append_sample(index, time, decoded.values[frame_offset + column], 0.0, 0.0)?;
            }
        }
        Ok(())
    }

    fn append_dense(&mut self, clip: &MuscleClipData, stream_count: usize) -> Result<()> {
        let dense = &clip.dense;
        let curve_count = usize::try_from(dense.curve_count)
            .map_err(|_| Error::invalid_data("dense curve count does not fit usize"))?;
        let samples = dense
            .samples
            .read_values(self.limits.clip.maximum_sample_values)?;
        let required = dense
            .frame_count
            .checked_mul(curve_count)
            .ok_or_else(|| Error::invalid_data("dense sample count overflowed"))?;
        if samples.len() < required {
            return Err(Error::invalid_data(format!(
                "dense clip has {} samples, fewer than required {required}",
                samples.len()
            )));
        }
        let sample_rate = dense.sample_rate();
        let begin_time = dense.begin_time();
        if !sample_rate.is_finite() || sample_rate <= 0.0 || !begin_time.is_finite() {
            return Err(Error::invalid_data(
                "dense clip has an invalid sample rate or begin time",
            ));
        }
        for frame_index in 0..dense.frame_count {
            let frame_u32 = u32::try_from(frame_index)
                .map_err(|_| Error::invalid_data("dense frame index does not fit u32"))?;
            let time = begin_time + u32_to_f32(frame_u32) / sample_rate;
            let offset = frame_index * curve_count;
            for curve_index in 0..curve_count {
                self.append_sample(
                    stream_count + curve_index,
                    time,
                    samples[offset + curve_index],
                    0.0,
                    0.0,
                )?;
            }
        }
        Ok(())
    }

    fn append_constant(
        &mut self,
        clip: &MuscleClipData,
        stop_time: f32,
        offset: usize,
    ) -> Result<()> {
        let values = clip
            .constant
            .values
            .read_values(self.limits.clip.maximum_sample_values)?;
        for time in [0.0, stop_time] {
            for (curve_index, value) in values.iter().copied().enumerate() {
                self.append_sample(offset + curve_index, time, value, 0.0, 0.0)?;
            }
        }
        Ok(())
    }

    fn append_sample(
        &mut self,
        binding_index: usize,
        time: f32,
        value: f32,
        in_slope: f32,
        out_slope: f32,
    ) -> Result<()> {
        let Some(binding) = self.bindings.find(binding_index) else {
            return Ok(());
        };
        let Some(target) = self.resolve_binding(binding)? else {
            return Ok(());
        };
        let keyframe = CubismMotionKeyframe {
            time: finite_f32(time, "AnimationClip key time")?,
            value: finite_f32(value, "AnimationClip key value")?,
            in_slope: slope_f32(in_slope, "AnimationClip key in-slope")?,
            out_slope: slope_f32(out_slope, "AnimationClip key out-slope")?,
        };
        self.keyframes = self
            .keyframes
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("Cubism keyframe count overflowed"))?;
        if self.keyframes > self.limits.maximum_keyframes {
            return Err(limit_error(
                "keyframes",
                self.keyframes,
                self.limits.maximum_keyframes,
            ));
        }
        let curve = if let Some(index) = self.curve_index.get(&target).copied() {
            index
        } else {
            self.insert_curve(target)?
        };
        self.curves[curve]
            .keyframes
            .try_reserve(1)
            .map_err(|error| Error::invalid_data(format!("cannot grow Cubism curve: {error}")))?;
        self.curves[curve].keyframes.push(keyframe);
        Ok(())
    }

    fn insert_curve(&mut self, key: MotionTargetKey) -> Result<usize> {
        if self.curves.len() >= self.limits.maximum_curves {
            return Err(limit_error(
                "curves",
                self.curves.len().saturating_add(1),
                self.limits.maximum_curves,
            ));
        }
        let (target, id) = self.targets.describe(key);
        let target_length = target.len();
        let id_length = id.len();
        let mut target_string = String::new();
        target_string
            .try_reserve_exact(target.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate motion target: {error}"))
            })?;
        target_string.push_str(target);
        let mut id_string = String::new();
        id_string.try_reserve_exact(id.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate motion target ID: {error}"))
        })?;
        id_string.push_str(id);
        self.charge_strings(target_length, "motion target")?;
        self.charge_strings(id_length, "motion target ID")?;
        self.curves.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow Cubism clip curves: {error}"))
        })?;
        self.curve_index.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow Cubism clip curve index: {error}"))
        })?;
        let index = self.curves.len();
        self.curves.push(CubismClipMotionCurve {
            target: target_string,
            id: id_string,
            keyframes: Vec::new(),
        });
        self.curve_index.insert(key, index);
        Ok(index)
    }

    fn resolve_binding(&mut self, binding: GenericBinding) -> Result<Option<MotionTargetKey>> {
        if binding.path != 0
            && let Some(target) = self.targets.by_path(binding.path)
        {
            return Ok(Some(target));
        }
        if binding.script.path_id == 0 {
            return Ok(None);
        }
        let Some(target) =
            resolve_object_reference(self.collection, self.file_index, binding.script)
                .ok()
                .flatten()
        else {
            return Ok(None);
        };
        if target.object.class_id != MONO_SCRIPT_CLASS_ID {
            return Ok(None);
        }
        let key = SceneObjectKey {
            file_index: target.file_index,
            path_id: target.object.path_id,
        };
        if let Some(cached) = self.script_targets.get(&key) {
            return Ok(*cached);
        }
        let script = read_mono_script(target.file, target.object_index, self.limits.mono_script)?;
        let resolved = match script.class_name.as_str() {
            "CubismRenderController" => Some(MotionTargetKey::Opacity),
            "CubismEyeBlinkController" => Some(MotionTargetKey::EyeBlink),
            "CubismMouthController" => Some(MotionTargetKey::LipSync),
            _ => None,
        };
        self.script_targets.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow Cubism script cache: {error}"))
        })?;
        self.script_targets.insert(key, resolved);
        Ok(resolved)
    }

    fn copy_string(&mut self, value: &str, field: &str) -> Result<String> {
        if value.len() > self.limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {} bytes, exceeding limit {}",
                value.len(),
                self.limits.maximum_string_bytes
            )));
        }
        self.charge_strings(value.len(), field)?;
        let mut output = String::new();
        output
            .try_reserve_exact(value.len())
            .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
        output.push_str(value);
        Ok(output)
    }

    fn charge_strings(&mut self, length: usize, field: &str) -> Result<()> {
        if length > self.limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_string_bytes
            )));
        }
        self.string_bytes = self
            .string_bytes
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data("Cubism string byte count overflowed"))?;
        if self.string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "Cubism strings use {} bytes, exceeding limit {}",
                self.string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        Ok(())
    }
}

fn parse_streamed_frames(
    words: &[u32],
    limits: CubismClipMotionReadLimits,
) -> Result<Vec<StreamedFrame>> {
    let mut frames = Vec::new();
    let mut cursor = 0_usize;
    let mut total_keys = 0_usize;
    while cursor < words.len() {
        if frames.len() >= limits.maximum_streamed_frames {
            return Err(limit_error(
                "streamed frames",
                frames.len().saturating_add(1),
                limits.maximum_streamed_frames,
            ));
        }
        let header_end = cursor
            .checked_add(2)
            .ok_or_else(|| Error::invalid_data("streamed frame header overflowed"))?;
        if header_end > words.len() {
            return Err(Error::invalid_data("streamed frame header is truncated"));
        }
        let time = f32::from_bits(words[cursor]);
        let signed_count = i32::from_ne_bytes(words[cursor + 1].to_ne_bytes());
        let count = usize::try_from(signed_count)
            .map_err(|_| Error::invalid_data("streamed frame key count is negative"))?;
        total_keys = total_keys
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("streamed key count overflowed"))?;
        if total_keys > limits.maximum_streamed_keys {
            return Err(limit_error(
                "streamed keys",
                total_keys,
                limits.maximum_streamed_keys,
            ));
        }
        cursor = header_end;
        let end = cursor
            .checked_add(
                count
                    .checked_mul(5)
                    .ok_or_else(|| Error::invalid_data("streamed key size overflowed"))?,
            )
            .ok_or_else(|| Error::invalid_data("streamed frame range overflowed"))?;
        if end > words.len() {
            return Err(Error::invalid_data("streamed frame keys are truncated"));
        }
        let mut keys = Vec::new();
        keys.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!("cannot allocate streamed keys: {error}"))
        })?;
        while cursor < end {
            let signed_index = i32::from_ne_bytes(words[cursor].to_ne_bytes());
            let index = usize::try_from(signed_index)
                .map_err(|_| Error::invalid_data("streamed curve index is negative"))?;
            let coefficients = [
                f32::from_bits(words[cursor + 1]),
                f32::from_bits(words[cursor + 2]),
                f32::from_bits(words[cursor + 3]),
                f32::from_bits(words[cursor + 4]),
            ];
            keys.push(StreamedKey {
                index,
                value: coefficients[3],
                in_slope: 0.0,
                out_slope: coefficients[2],
                coefficients,
            });
            cursor += 5;
        }
        frames.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow streamed frames: {error}"))
        })?;
        frames.push(StreamedFrame { time, keys });
    }
    Ok(frames)
}

fn calculate_streamed_in_slopes(frames: &mut [StreamedFrame]) -> Result<()> {
    let capacity = frames.iter().try_fold(0_usize, |total, frame| {
        total
            .checked_add(frame.keys.len())
            .ok_or_else(|| Error::invalid_data("streamed key index capacity overflowed"))
    })?;
    let mut previous = HashMap::<usize, (f32, StreamedKey)>::new();
    previous.try_reserve(capacity).map_err(|error| {
        Error::invalid_data(format!("cannot allocate streamed key history: {error}"))
    })?;
    let last = frames.len().saturating_sub(1);
    for (frame_index, frame) in frames.iter_mut().enumerate() {
        for key in &mut frame.keys {
            if (2..last).contains(&frame_index)
                && let Some((previous_time, previous_key)) = previous.get(&key.index)
            {
                key.in_slope =
                    calculate_next_in_slope(*previous_time, previous_key, frame.time, key);
            }
            previous.insert(key.index, (frame.time, key.clone()));
        }
    }
    Ok(())
}

fn calculate_next_in_slope(
    left_time: f32,
    left: &StreamedKey,
    right_time: f32,
    right: &StreamedKey,
) -> f32 {
    if left.coefficients[0] == 0.0 && left.coefficients[1] == 0.0 && left.coefficients[2] == 0.0 {
        return f32::INFINITY;
    }
    let dx = (right_time - left_time).max(0.0001);
    let dy = right.value - left.value;
    let length = 1.0 / (dx * dx);
    let d1 = left.out_slope * dx;
    let d2 = dy + dy + dy - d1 - d1 - left.coefficients[1] / length;
    d2 / dx
}

fn finite_f32(value: f32, field: &str) -> Result<f32> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::invalid_data(format!("{field} is not finite")))
    }
}

fn slope_f32(value: f32, field: &str) -> Result<f32> {
    if value.is_nan() || value == f32::NEG_INFINITY {
        Err(Error::invalid_data(format!("{field} is not supported")))
    } else {
        Ok(value)
    }
}

fn u32_to_f32(value: u32) -> f32 {
    let high = u16::try_from(value >> 16).expect("upper half always fits u16");
    let low = u16::try_from(value & 0xffff).expect("lower half always fits u16");
    f32::from(high) * 65_536.0 + f32::from(low)
}

fn limit_error(field: &str, actual: usize, maximum: usize) -> Error {
    Error::invalid_data(format!(
        "Cubism AnimationClip has {actual} {field}, limit is {maximum}"
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        BindingLayout, ClipBuilder, CubismClipMotion, CubismClipMotionCurve, CubismClipMotionEvent,
        CubismClipMotionReadLimits, MotionTargetKey, TargetIndex, calculate_streamed_in_slopes,
        parse_streamed_frames,
    };
    use crate::acl::AclDecodedClip;
    use crate::animation_clip::GenericBinding;
    use crate::live2d_motion::{CubismMotionKeyframe, CubismMotionTargetNames};
    use crate::loader::AssetCollection;
    use crate::model_animation::unity_crc32;
    use crate::scene_hierarchy::SceneObjectKey;
    use crate::serialized::ObjectReference;

    #[test]
    fn resolves_parameter_part_suffixes_and_rejects_hash_ambiguity() {
        let targets = CubismMotionTargetNames {
            parameters: vec!["ParamAngleX".to_owned()],
            parts: vec!["PartArm".to_owned()],
        };
        let index = TargetIndex::build(&targets).unwrap();
        assert_eq!(
            index.by_path(unity_crc32(b"Parameters/ParamAngleX")),
            Some(MotionTargetKey::Named(0))
        );
        assert_eq!(
            index.by_path(unity_crc32(b"PartArm")),
            Some(MotionTargetKey::Named(1))
        );
    }

    #[test]
    fn reads_streamed_coefficients_and_reconstructs_slopes() {
        let words = [
            0.0_f32.to_bits(),
            1,
            0,
            1.0_f32.to_bits(),
            0.0_f32.to_bits(),
            2.0_f32.to_bits(),
            3.0_f32.to_bits(),
            1.0_f32.to_bits(),
            1,
            0,
            1.0_f32.to_bits(),
            0.0_f32.to_bits(),
            2.0_f32.to_bits(),
            5.0_f32.to_bits(),
            2.0_f32.to_bits(),
            1,
            0,
            1.0_f32.to_bits(),
            0.0_f32.to_bits(),
            2.0_f32.to_bits(),
            7.0_f32.to_bits(),
            3.0_f32.to_bits(),
            0,
        ];
        let mut frames =
            parse_streamed_frames(&words, CubismClipMotionReadLimits::default()).unwrap();
        calculate_streamed_in_slopes(&mut frames).unwrap();
        assert_eq!(frames.len(), 4);
        assert_eq!(frames[1].keys[0].value.to_bits(), 5.0_f32.to_bits());
        assert_eq!(frames[2].keys[0].in_slope.to_bits(), 2.0_f32.to_bits());
    }

    #[test]
    fn maps_validated_acl_columns_into_cubism_parameter_curves() {
        let collection = AssetCollection::from_loaded_parts(Vec::new(), Vec::new());
        let targets = CubismMotionTargetNames {
            parameters: vec!["ParamAngleX".to_owned()],
            parts: Vec::new(),
        };
        let target_index = TargetIndex::build(&targets).unwrap();
        let bindings = BindingLayout::build(&[GenericBinding {
            path: unity_crc32(b"Parameters/ParamAngleX"),
            attribute: 0,
            script: ObjectReference {
                file_id: 0,
                path_id: 0,
            },
            type_id: 0,
            custom_type: 0,
            is_pptr_curve: 0,
            is_int_curve: None,
            is_serialize_reference_curve: None,
        }])
        .unwrap();
        let mut builder = ClipBuilder::new(
            &collection,
            0,
            CubismClipMotionReadLimits::default(),
            target_index,
            bindings,
        )
        .unwrap();
        builder
            .append_acl(&AclDecodedClip {
                times: vec![0.0, 0.5],
                binding_indices: vec![0],
                values: vec![1.25, 2.5],
                following_curve_offset: 1,
            })
            .unwrap();

        assert_eq!(builder.curves.len(), 1);
        assert_eq!(builder.curves[0].target, "Parameter");
        assert_eq!(builder.curves[0].id, "ParamAngleX");
        assert_eq!(builder.curves[0].keyframes.len(), 2);
        assert_eq!(
            builder.curves[0].keyframes[0].value.to_bits(),
            1.25_f32.to_bits()
        );
        assert_eq!(
            builder.curves[0].keyframes[1].time.to_bits(),
            0.5_f32.to_bits()
        );
        assert_eq!(
            builder.curves[0].keyframes[1].value.to_bits(),
            2.5_f32.to_bits()
        );
    }

    #[test]
    fn writes_animation_clip_metadata_segments_and_unicode_event_size() {
        let motion = CubismClipMotion {
            object: SceneObjectKey {
                file_index: 0,
                path_id: 74,
            },
            name: "Idle".to_owned(),
            duration: 1.0,
            fps: 60.0,
            curves: vec![CubismClipMotionCurve {
                target: "Parameter".to_owned(),
                id: "ParamAngleX".to_owned(),
                keyframes: vec![
                    CubismMotionKeyframe {
                        time: 0.25,
                        value: 1.0,
                        in_slope: 0.0,
                        out_slope: 0.0,
                    },
                    CubismMotionKeyframe {
                        time: 1.0,
                        value: 2.0,
                        in_slope: 0.0,
                        out_slope: 0.0,
                    },
                ],
            }],
            events: vec![CubismClipMotionEvent {
                time: 0.5,
                value: "😀".to_owned(),
            }],
        };
        let mut output = Vec::new();
        motion
            .write_motion3_json(false, &mut output, 16 * 1024)
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(json["Meta"]["Fps"], 60.0);
        assert_eq!(json["Meta"]["TotalUserDataSize"], 2);
        assert_eq!(json["Curves"][0]["FadeInTime"], -1.0);
        assert_eq!(json["Curves"][0]["Segments"][0], 0.0);
        assert_eq!(json["UserData"][0]["Value"], "😀");
    }
}
