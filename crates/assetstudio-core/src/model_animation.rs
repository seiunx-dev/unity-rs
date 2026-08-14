//! Bounded conversion from collection-wide Unity animation bindings to model tracks.
//!
//! Converts explicit legacy curves and standard streamed, dense, and constant
//! muscle samples for Transform position, quaternion/Euler rotation, scale, and
//! blend-shape weights. Legacy delta-time/packed-quaternion curves use the same
//! bounded IR.

use std::collections::{HashMap, HashSet};

use crate::acl::{
    AclCompressedTracksLimits, AclDecodeLimits, AclDecodedClip, AclDecoder, AclDecoderInputLimits,
};
use crate::animation_clip::{
    ANIMATION_CLIP_CLASS_ID, AnimationClip, AnimationClipReadLimits, CompressedAnimationCurve,
    FloatCurve, GenericBinding, MuscleClipData, QuaternionCurve, Vector3Curve, read_animation_clip,
};
use crate::animation_graph::AnimationGraph;
use crate::fbx_scene_ascii::quaternion_to_euler_degrees;
use crate::loader::AssetCollection;
use crate::model_ir::ModelIr;
use crate::renderer::SKINNED_MESH_RENDERER_CLASS_ID;
use crate::scene_hierarchy::SceneObjectKey;
use crate::{Error, Result};

/// Collection-wide budgets for animation materialization and path binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelAnimationLimits {
    pub maximum_clips: usize,
    pub maximum_tracks: usize,
    pub maximum_keyframes: usize,
    pub maximum_total_streamed_words: usize,
    pub maximum_total_sample_values: usize,
    pub maximum_path_bytes: usize,
    pub maximum_path_hashes: usize,
    pub maximum_blend_shape_channels: usize,
    pub maximum_name_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub clip: AnimationClipReadLimits,
}

impl Default for ModelAnimationLimits {
    fn default() -> Self {
        Self {
            maximum_clips: 1_000_000,
            maximum_tracks: 2_000_000,
            maximum_keyframes: 100_000_000,
            maximum_total_streamed_words: 64 * 1024 * 1024,
            maximum_total_sample_values: 128 * 1024 * 1024,
            maximum_path_bytes: 16 * 1024 * 1024,
            maximum_path_hashes: 10_000_000,
            maximum_blend_shape_channels: 10_000_000,
            maximum_name_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 256 * 1024 * 1024,
            clip: AnimationClipReadLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelVectorKeyframe {
    pub time: f32,
    pub value: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelScalarKeyframe {
    pub time: f32,
    pub value: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelAnimationTrack {
    pub node: SceneObjectKey,
    pub translations: Vec<ModelVectorKeyframe>,
    pub rotations: Vec<ModelVectorKeyframe>,
    pub scalings: Vec<ModelVectorKeyframe>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelBlendShapeTrack {
    pub node: SceneObjectKey,
    pub channel: String,
    pub keys: Vec<ModelScalarKeyframe>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelAnimationClip {
    pub object: SceneObjectKey,
    pub name: String,
    pub sample_rate: f32,
    pub tracks: Vec<ModelAnimationTrack>,
    pub blend_shapes: Vec<ModelBlendShapeTrack>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelAnimationSet {
    pub clips: Vec<ModelAnimationClip>,
}

/// Resolves the clips selected by Animator and legacy Animation bindings.
///
/// Clip identity is de-duplicated in first-reference order. Animator paths are
/// matched as suffixes of model paths, matching `ImportedFrame.FindFrameByPath`.
/// Legacy Animation paths are first prefixed by the component `GameObject` path.
pub fn build_model_animations(
    collection: &AssetCollection,
    model: &ModelIr,
    graph: &AnimationGraph,
    limits: ModelAnimationLimits,
) -> Result<ModelAnimationSet> {
    build_model_animations_with_acl_decoder(collection, model, graph, limits, None)
}

/// Builds model animations and optionally decodes Tuanjie ACL tracks through
/// a safe injected decoder.
pub fn build_model_animations_with_acl_decoder(
    collection: &AssetCollection,
    model: &ModelIr,
    graph: &AnimationGraph,
    limits: ModelAnimationLimits,
    acl_decoder: Option<&dyn AclDecoder>,
) -> Result<ModelAnimationSet> {
    let paths = ModelPathIndex::build(model, &limits)?;
    let blend_shapes = BlendShapeIndex::build(
        model,
        limits.maximum_blend_shape_channels,
        limits.maximum_name_bytes,
        limits
            .maximum_total_string_bytes
            .checked_sub(paths.total_string_bytes)
            .ok_or_else(|| Error::invalid_data("animation string byte budget is exhausted"))?,
    )?;
    let selections = select_clips(graph, model, limits.maximum_clips)?;
    let mut state = AnimationBuildState::new(limits, paths, blend_shapes.total_string_bytes);
    for selection in selections {
        let file = collection
            .serialized_files
            .get(selection.clip.file_index)
            .ok_or_else(|| {
                Error::invalid_data("animation clip file index is outside collection")
            })?;
        let object_index = collection
            .object_index_by_path_id(selection.clip.file_index, selection.clip.path_id)
            .ok_or_else(|| Error::invalid_data("animation clip vanished from collection"))?;
        let object =
            file.file.objects.get(object_index).ok_or_else(|| {
                Error::invalid_data("animation clip object index is outside file")
            })?;
        if object.class_id != ANIMATION_CLIP_CLASS_ID {
            return Err(Error::invalid_data(format!(
                "animation graph clip {:?} has class ID {}",
                selection.clip, object.class_id
            )));
        }
        let clip = read_animation_clip(&file.file, object_index, limits.clip)?;
        state.push_clip(selection, &clip, model, &blend_shapes, acl_decoder)?;
    }
    Ok(ModelAnimationSet { clips: state.clips })
}

#[derive(Debug, Clone, Copy)]
struct ClipSelection {
    clip: SceneObjectKey,
    legacy_base: Option<SceneObjectKey>,
    avatar: Option<SceneObjectKey>,
}

fn select_clips(
    graph: &AnimationGraph,
    model: &ModelIr,
    maximum: usize,
) -> Result<Vec<ClipSelection>> {
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for animator in &graph.animators {
        if model
            .node(animator.game_object)
            .is_none_or(|node| !node.export_content)
        {
            continue;
        }
        for clip in animator.bound_clips.iter().flatten().copied() {
            push_selection(
                &mut selected,
                &mut seen,
                clip,
                None,
                animator.avatar,
                maximum,
            )?;
        }
    }
    for animation in &graph.legacy_animations {
        let Some(game_object) = animation.game_object else {
            continue;
        };
        if model
            .node(game_object)
            .is_none_or(|node| !node.export_content)
        {
            continue;
        }
        for clip in animation.clips.iter().flatten().copied() {
            push_selection(
                &mut selected,
                &mut seen,
                clip,
                Some(game_object),
                None,
                maximum,
            )?;
        }
    }
    Ok(selected)
}

fn push_selection(
    selected: &mut Vec<ClipSelection>,
    seen: &mut HashSet<SceneObjectKey>,
    clip: SceneObjectKey,
    legacy_base: Option<SceneObjectKey>,
    avatar: Option<SceneObjectKey>,
    maximum: usize,
) -> Result<()> {
    if seen.contains(&clip) {
        return Ok(());
    }
    if selected.len() >= maximum {
        return Err(Error::invalid_data(format!(
            "model animation clip count exceeds limit {maximum}"
        )));
    }
    selected
        .try_reserve(1)
        .map_err(|error| Error::invalid_data(format!("cannot grow selected clips: {error}")))?;
    seen.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow selected clip index: {error}"))
    })?;
    seen.insert(clip);
    selected.push(ClipSelection {
        clip,
        legacy_base,
        avatar,
    });
    Ok(())
}

struct AnimationBuildState {
    limits: ModelAnimationLimits,
    paths: ModelPathIndex,
    clips: Vec<ModelAnimationClip>,
    used_names: HashSet<String>,
    total_tracks: usize,
    total_keyframes: usize,
    total_streamed_words: usize,
    total_sample_values: usize,
    total_string_bytes: usize,
}

impl AnimationBuildState {
    fn new(
        limits: ModelAnimationLimits,
        paths: ModelPathIndex,
        blend_shape_string_bytes: usize,
    ) -> Self {
        let total_string_bytes = paths
            .total_string_bytes
            .checked_add(blend_shape_string_bytes)
            .expect("blend-shape index was built within the remaining string budget");
        Self {
            limits,
            paths,
            clips: Vec::new(),
            used_names: HashSet::new(),
            total_tracks: 0,
            total_keyframes: 0,
            total_streamed_words: 0,
            total_sample_values: 0,
            total_string_bytes,
        }
    }

    fn push_clip(
        &mut self,
        selection: ClipSelection,
        clip: &AnimationClip,
        model: &ModelIr,
        blend_shapes: &BlendShapeIndex,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<()> {
        if !clip.sample_rate.is_finite() || clip.sample_rate <= 0.0 {
            return Err(Error::invalid_data(format!(
                "AnimationClip {} has invalid sample rate {}",
                clip.name, clip.sample_rate
            )));
        }
        let name = self.unique_name(&clip.name)?;
        let (tracks, blend_shape_tracks) = if clip.legacy {
            let base_path = selection
                .legacy_base
                .and_then(|key| self.paths.path(key))
                .map(str::to_owned);
            self.convert_explicit_curves(clip, base_path.as_deref(), blend_shapes)?
        } else {
            self.convert_muscle_curves(model, selection.avatar, clip, blend_shapes, acl_decoder)?
        };
        self.clips.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow model animation clips: {error}"))
        })?;
        self.clips.push(ModelAnimationClip {
            object: selection.clip,
            name,
            sample_rate: clip.sample_rate,
            tracks,
            blend_shapes: blend_shape_tracks,
        });
        Ok(())
    }

    fn unique_name(&mut self, source: &str) -> Result<String> {
        if source.len() > self.limits.maximum_name_bytes {
            return Err(Error::invalid_data(format!(
                "animation name is {} bytes, exceeding limit {}",
                source.len(),
                self.limits.maximum_name_bytes
            )));
        }
        let base = if source.is_empty() { "Take" } else { source };
        let mut suffix = 0_usize;
        loop {
            let candidate = if suffix == 0 {
                fallible_string(base, "animation name")?
            } else {
                fallible_format_name(base, suffix)?
            };
            if !self.used_names.contains(&candidate) {
                if candidate.len() > self.limits.maximum_name_bytes {
                    return Err(Error::invalid_data(format!(
                        "animation name is {} bytes, exceeding limit {}",
                        candidate.len(),
                        self.limits.maximum_name_bytes
                    )));
                }
                self.charge_string(candidate.len().checked_mul(2).ok_or_else(|| {
                    Error::invalid_data("animation name allocation budget overflowed")
                })?)?;
                self.used_names.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow used animation names: {error}"))
                })?;
                self.used_names
                    .insert(fallible_string(&candidate, "used animation name")?);
                return Ok(candidate);
            }
            suffix = suffix
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("animation name suffix overflowed"))?;
        }
    }

    fn convert_explicit_curves(
        &mut self,
        clip: &AnimationClip,
        base_path: Option<&str>,
        blend_shapes: &BlendShapeIndex,
    ) -> Result<(Vec<ModelAnimationTrack>, Vec<ModelBlendShapeTrack>)> {
        let mut tracks = Vec::new();
        let mut by_node = HashMap::new();
        let mut blend_shape_tracks = Vec::new();
        let mut by_blend_shape = HashMap::new();
        let curves = ExplicitCurveSlices {
            rotations: &clip.rotation_curves,
            eulers: &clip.euler_curves,
            positions: &clip.position_curves,
            scales: &clip.scale_curves,
        };
        let mut context = CurveBuildContext {
            paths: &self.paths,
            base_path,
            tracks: &mut tracks,
            by_node: &mut by_node,
            total_tracks: &mut self.total_tracks,
            total_keyframes: &mut self.total_keyframes,
            limits: &self.limits,
            blend_shapes,
            blend_shape_tracks: &mut blend_shape_tracks,
            by_blend_shape: &mut by_blend_shape,
            total_string_bytes: &mut self.total_string_bytes,
        };
        for curve in &clip.compressed_rotation_curves {
            let Some(track) = resolve_track(&mut context, &curve.path)? else {
                continue;
            };
            append_compressed_quaternion_keys(
                &mut context.tracks[track].rotations,
                curve,
                context.total_keyframes,
                context.limits,
            )?;
        }
        convert_explicit_curve_slices(&mut context, &curves)?;
        convert_explicit_blend_shapes(&mut context, &clip.float_curves)?;
        Ok((tracks, blend_shape_tracks))
    }

    fn convert_muscle_curves(
        &mut self,
        model: &ModelIr,
        avatar_key: Option<SceneObjectKey>,
        clip: &AnimationClip,
        blend_shapes: &BlendShapeIndex,
        acl_decoder: Option<&dyn AclDecoder>,
    ) -> Result<(Vec<ModelAnimationTrack>, Vec<ModelBlendShapeTrack>)> {
        let muscle = clip.muscle_clip.as_ref().ok_or_else(|| {
            Error::unsupported("model animation conversion requires an AnimationClip muscle clip")
        })?;
        let decoded_acl = muscle
            .clip
            .acl
            .as_ref()
            .map(|acl| {
                let decoder = acl_decoder.ok_or_else(|| {
                    Error::unsupported(
                        "model animation conversion requires an injected Tuanjie ACL decoder",
                    )
                })?;
                acl.decode_with(
                    decoder,
                    AclDecodeLimits {
                        input: AclDecoderInputLimits {
                            compressed_tracks: AclCompressedTracksLimits {
                                maximum_compressed_bytes: self.limits.clip.maximum_packed_bytes,
                                ..AclCompressedTracksLimits::default()
                            },
                            maximum_decoder_map_entries: self.limits.clip.maximum_array_elements,
                            maximum_materialized_bytes: self.limits.clip.maximum_total_packed_bytes,
                        },
                        maximum_frames: self.limits.maximum_total_sample_values,
                        maximum_curves: self.limits.maximum_tracks,
                        maximum_values: self.limits.maximum_total_sample_values,
                    },
                )
            })
            .transpose()?;
        let layout = BindingLayout::build(&clip.binding_constant.generic_bindings)?;
        let avatar = avatar_key.and_then(|key| model.avatar(key));
        let mut tracks = Vec::new();
        let mut by_node = HashMap::new();
        let mut blend_shape_tracks = Vec::new();
        let mut by_blend_shape = HashMap::new();
        let mut context = MuscleBuildContext {
            paths: &self.paths,
            avatar_paths: avatar.map(|entry| entry.avatar.paths.as_slice()),
            layout: &layout,
            tracks: &mut tracks,
            by_node: &mut by_node,
            total_tracks: &mut self.total_tracks,
            total_keyframes: &mut self.total_keyframes,
            limits: &self.limits,
            blend_shapes,
            blend_shape_tracks: &mut blend_shape_tracks,
            by_blend_shape: &mut by_blend_shape,
            total_string_bytes: &mut self.total_string_bytes,
        };
        convert_muscle_clip_with_acl(
            &mut context,
            &muscle.clip,
            muscle.stop_time,
            &mut self.total_streamed_words,
            &mut self.total_sample_values,
            decoded_acl.as_ref(),
        )?;
        Ok((tracks, blend_shape_tracks))
    }

    fn charge_string(&mut self, additional: usize) -> Result<()> {
        self.total_string_bytes = self
            .total_string_bytes
            .checked_add(additional)
            .ok_or_else(|| Error::invalid_data("animation string byte budget overflowed"))?;
        if self.total_string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "animation strings use {} bytes, exceeding limit {}",
                self.total_string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        Ok(())
    }
}

struct ExplicitCurveSlices<'a> {
    rotations: &'a [QuaternionCurve],
    eulers: &'a [Vector3Curve],
    positions: &'a [Vector3Curve],
    scales: &'a [Vector3Curve],
}

struct CurveBuildContext<'a, 'b> {
    paths: &'a ModelPathIndex,
    base_path: Option<&'b str>,
    tracks: &'b mut Vec<ModelAnimationTrack>,
    by_node: &'b mut HashMap<SceneObjectKey, usize>,
    total_tracks: &'b mut usize,
    total_keyframes: &'b mut usize,
    limits: &'a ModelAnimationLimits,
    blend_shapes: &'a BlendShapeIndex,
    blend_shape_tracks: &'b mut Vec<ModelBlendShapeTrack>,
    by_blend_shape: &'b mut HashMap<usize, usize>,
    total_string_bytes: &'b mut usize,
}

#[derive(Debug)]
struct BlendShapeTarget {
    node: SceneObjectKey,
    channel: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BlendShapeLookup {
    node: SceneObjectKey,
    hash: u32,
    target: usize,
}

struct BlendShapeIndex {
    targets: Vec<BlendShapeTarget>,
    by_node_hash: Vec<BlendShapeLookup>,
    by_hash: Vec<(u32, usize)>,
    total_string_bytes: usize,
}

impl BlendShapeIndex {
    fn build(
        model: &ModelIr,
        maximum_channels: usize,
        maximum_name_bytes: usize,
        maximum_string_bytes: usize,
    ) -> Result<Self> {
        let mut targets = Vec::new();
        let mut by_node_hash = Vec::new();
        let mut by_hash = Vec::new();
        let mut total_string_bytes = 0_usize;
        for node in &model.nodes {
            for renderer in &node.renderers {
                let Some(mesh_key) = renderer.mesh else {
                    continue;
                };
                let Some(shapes) = model
                    .mesh(mesh_key)
                    .and_then(|mesh| mesh.mesh.blend_shapes.as_ref())
                else {
                    continue;
                };
                for channel in &shapes.channels {
                    if targets.len() >= maximum_channels {
                        return Err(Error::invalid_data(format!(
                            "model blend-shape channels exceed limit {maximum_channels}"
                        )));
                    }
                    let exported = channel.name.rsplit('.').next().unwrap_or(&channel.name);
                    if exported.len() > maximum_name_bytes {
                        return Err(Error::invalid_data(format!(
                            "blend-shape channel name is {} bytes, exceeding limit {maximum_name_bytes}",
                            exported.len()
                        )));
                    }
                    total_string_bytes = total_string_bytes
                        .checked_add(exported.len())
                        .ok_or_else(|| {
                            Error::invalid_data("blend-shape string budget overflowed")
                        })?;
                    if total_string_bytes > maximum_string_bytes {
                        return Err(Error::invalid_data(format!(
                            "blend-shape strings use {total_string_bytes} bytes, exceeding remaining limit {maximum_string_bytes}"
                        )));
                    }
                    let target = targets.len();
                    targets.try_reserve(1).map_err(|error| {
                        Error::invalid_data(format!("cannot grow blend-shape targets: {error}"))
                    })?;
                    by_node_hash.try_reserve(2).map_err(|error| {
                        Error::invalid_data(format!("cannot grow blend-shape lookup: {error}"))
                    })?;
                    by_hash.try_reserve(2).map_err(|error| {
                        Error::invalid_data(format!(
                            "cannot grow global blend-shape lookup: {error}"
                        ))
                    })?;
                    targets.push(BlendShapeTarget {
                        node: node.object,
                        channel: fallible_string(exported, "blend-shape channel")?,
                    });
                    for hash in [
                        blend_shape_crc32(&channel.name),
                        blend_shape_crc32(exported),
                    ] {
                        by_node_hash.push(BlendShapeLookup {
                            node: node.object,
                            hash,
                            target,
                        });
                        by_hash.push((hash, target));
                    }
                }
            }
        }
        by_node_hash.sort_unstable();
        by_hash.sort_unstable();
        Ok(Self {
            targets,
            by_node_hash,
            by_hash,
            total_string_bytes,
        })
    }

    fn resolve(&self, node: Option<SceneObjectKey>, hash: u32) -> Option<usize> {
        if let Some(node) = node {
            let position = self
                .by_node_hash
                .partition_point(|entry| (entry.node, entry.hash) < (node, hash));
            return self
                .by_node_hash
                .get(position)
                .filter(|entry| entry.node == node && entry.hash == hash)
                .map(|entry| entry.target);
        }
        let position = self.by_hash.partition_point(|entry| entry.0 < hash);
        self.by_hash
            .get(position)
            .filter(|entry| entry.0 == hash)
            .map(|entry| entry.1)
    }
}

#[derive(Debug, Clone, Copy)]
struct BindingSpan {
    start: usize,
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
            Error::invalid_data(format!("cannot allocate animation binding layout: {error}"))
        })?;
        let mut start = 0_usize;
        for binding in bindings {
            let width = binding_width(*binding);
            let end = start
                .checked_add(width)
                .ok_or_else(|| Error::invalid_data("animation binding width overflowed"))?;
            spans.push(BindingSpan {
                start,
                end,
                binding: *binding,
            });
            start = end;
        }
        Ok(Self { spans })
    }

    fn find(&self, index: usize) -> Option<BindingSpan> {
        let position = self.spans.partition_point(|span| span.end <= index);
        self.spans
            .get(position)
            .copied()
            .filter(|span| span.start <= index)
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

struct MuscleBuildContext<'a, 'b> {
    paths: &'a ModelPathIndex,
    avatar_paths: Option<&'a [crate::avatar::AvatarPath]>,
    layout: &'a BindingLayout,
    tracks: &'b mut Vec<ModelAnimationTrack>,
    by_node: &'b mut HashMap<SceneObjectKey, usize>,
    total_tracks: &'b mut usize,
    total_keyframes: &'b mut usize,
    limits: &'a ModelAnimationLimits,
    blend_shapes: &'a BlendShapeIndex,
    blend_shape_tracks: &'b mut Vec<ModelBlendShapeTrack>,
    by_blend_shape: &'b mut HashMap<usize, usize>,
    total_string_bytes: &'b mut usize,
}

#[derive(Debug)]
struct StreamedFrame {
    time: f32,
    keys: Vec<StreamedKey>,
}

#[derive(Debug, Clone, Copy)]
struct StreamedKey {
    index: usize,
    value: f32,
}

fn append_acl_frames(
    context: &mut MuscleBuildContext<'_, '_>,
    decoded: &AclDecodedClip,
    total_sample_values: &mut usize,
) -> Result<()> {
    charge_total(
        decoded.values.len(),
        total_sample_values,
        context.limits.maximum_total_sample_values,
        "ACL animation sample values",
    )?;
    let curve_count = decoded.binding_indices.len();
    for (frame_index, &time) in decoded.times.iter().enumerate() {
        validate_time(time)?;
        let frame_offset = frame_index
            .checked_mul(curve_count)
            .ok_or_else(|| Error::invalid_data("ACL frame value offset overflowed"))?;
        let mut column = 0_usize;
        while column < curve_count {
            let global_index = usize::try_from(decoded.binding_indices[column])
                .map_err(|_| Error::invalid_data("ACL binding index does not fit usize"))?;
            let span = context.layout.find(global_index).ok_or_else(|| {
                Error::invalid_data(format!(
                    "ACL animation curve index {global_index} has no binding"
                ))
            })?;
            if global_index != span.start {
                return Err(Error::invalid_data(
                    "ACL samples start inside a bound curve group",
                ));
            }
            let width = span.end - span.start;
            let column_end = column
                .checked_add(width)
                .ok_or_else(|| Error::invalid_data("ACL sample group range overflowed"))?;
            if column_end > curve_count {
                return Err(Error::invalid_data(
                    "ACL samples end inside a bound curve group",
                ));
            }
            for offset in 0..width {
                let expected = u32::try_from(span.start + offset)
                    .map_err(|_| Error::invalid_data("ACL binding index exceeds u32"))?;
                if decoded.binding_indices[column + offset] != expected {
                    return Err(Error::invalid_data(
                        "ACL decoder omitted part of a bound curve group",
                    ));
                }
            }
            let value_start = frame_offset
                .checked_add(column)
                .ok_or_else(|| Error::invalid_data("ACL frame value range overflowed"))?;
            let value_end = value_start
                .checked_add(width)
                .ok_or_else(|| Error::invalid_data("ACL frame value range overflowed"))?;
            append_bound_sample(
                context,
                span,
                time,
                decoded.values[value_start..value_end].iter().copied(),
            )?;
            column = column_end;
        }
    }
    Ok(())
}

#[cfg(test)]
fn convert_muscle_clip(
    context: &mut MuscleBuildContext<'_, '_>,
    clip: &MuscleClipData,
    stop_time: f32,
    total_streamed_words: &mut usize,
    total_sample_values: &mut usize,
) -> Result<()> {
    convert_muscle_clip_with_acl(
        context,
        clip,
        stop_time,
        total_streamed_words,
        total_sample_values,
        None,
    )
}

fn convert_muscle_clip_with_acl(
    context: &mut MuscleBuildContext<'_, '_>,
    clip: &MuscleClipData,
    stop_time: f32,
    total_streamed_words: &mut usize,
    total_sample_values: &mut usize,
    decoded_acl: Option<&AclDecodedClip>,
) -> Result<()> {
    let following_curve_offset = if let Some(decoded) = decoded_acl {
        append_acl_frames(context, decoded, total_sample_values)?;
        usize::try_from(decoded.following_curve_offset)
            .map_err(|_| Error::invalid_data("ACL following-curve offset does not fit usize"))?
    } else {
        0
    };
    charge_total(
        clip.streamed.data.count,
        total_streamed_words,
        context.limits.maximum_total_streamed_words,
        "streamed animation words",
    )?;
    let words = clip
        .streamed
        .data
        .read_values(context.limits.clip.maximum_streamed_words)?;
    convert_streamed_frames(context, &words, following_curve_offset, total_sample_values)?;

    let stream_count = usize::try_from(clip.streamed.curve_count)
        .map_err(|_| Error::invalid_data("streamed animation curve count does not fit usize"))?;
    let dense_offset = following_curve_offset
        .checked_add(stream_count)
        .ok_or_else(|| Error::invalid_data("dense animation curve offset overflowed"))?;
    convert_dense_clip(context, clip, dense_offset, total_sample_values)?;
    let dense_count = usize::try_from(clip.dense.curve_count)
        .map_err(|_| Error::invalid_data("dense animation curve count does not fit usize"))?;
    convert_constant_clip(
        context,
        clip,
        following_curve_offset
            .checked_add(stream_count)
            .and_then(|value| value.checked_add(dense_count))
            .ok_or_else(|| Error::invalid_data("constant animation curve offset overflowed"))?,
        stop_time,
        total_sample_values,
    )
}

fn convert_streamed_frames(
    context: &mut MuscleBuildContext<'_, '_>,
    words: &[u32],
    binding_offset: usize,
    total_sample_values: &mut usize,
) -> Result<()> {
    let mut cursor = 0_usize;
    let mut previous: Option<(usize, StreamedFrame)> = None;
    let mut frame_index = 0_usize;
    while cursor < words.len() {
        let frame = read_streamed_frame(words, &mut cursor)?;
        charge_total(
            frame.keys.len(),
            total_sample_values,
            context.limits.maximum_total_sample_values,
            "animation sample values",
        )?;
        if let Some((previous_index, previous_frame)) = previous.replace((frame_index, frame)) {
            if previous_index >= 1 {
                append_streamed_frame(context, &previous_frame, binding_offset)?;
            }
        }
        frame_index = frame_index
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("streamed animation frame count overflowed"))?;
    }
    Ok(())
}

fn read_streamed_frame(words: &[u32], cursor: &mut usize) -> Result<StreamedFrame> {
    let header_end = cursor
        .checked_add(2)
        .ok_or_else(|| Error::invalid_data("streamed frame header overflowed"))?;
    if header_end > words.len() {
        return Err(Error::invalid_data(
            "streamed animation frame header is truncated",
        ));
    }
    let time = f32::from_bits(words[*cursor]);
    let signed_count = i32::from_ne_bytes(words[*cursor + 1].to_ne_bytes());
    let count = usize::try_from(signed_count)
        .map_err(|_| Error::invalid_data("streamed animation key count is negative"))?;
    *cursor = header_end;
    let word_count = count
        .checked_mul(5)
        .ok_or_else(|| Error::invalid_data("streamed animation key size overflowed"))?;
    let end = cursor
        .checked_add(word_count)
        .ok_or_else(|| Error::invalid_data("streamed animation frame range overflowed"))?;
    if end > words.len() {
        return Err(Error::invalid_data(
            "streamed animation frame keys are truncated",
        ));
    }
    let mut keys = Vec::new();
    keys.try_reserve_exact(count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate streamed animation frame: {error}"))
    })?;
    while *cursor < end {
        let signed_index = i32::from_ne_bytes(words[*cursor].to_ne_bytes());
        let index = usize::try_from(signed_index)
            .map_err(|_| Error::invalid_data("streamed animation curve index is negative"))?;
        let value = f32::from_bits(words[*cursor + 4]);
        keys.push(StreamedKey { index, value });
        *cursor += 5;
    }
    Ok(StreamedFrame { time, keys })
}

fn append_streamed_frame(
    context: &mut MuscleBuildContext<'_, '_>,
    frame: &StreamedFrame,
    binding_offset: usize,
) -> Result<()> {
    validate_time(frame.time)?;
    let mut cursor = 0_usize;
    while cursor < frame.keys.len() {
        let start = binding_offset
            .checked_add(frame.keys[cursor].index)
            .ok_or_else(|| Error::invalid_data("streamed animation curve index overflowed"))?;
        let span = context.layout.find(start).ok_or_else(|| {
            Error::invalid_data(format!(
                "streamed animation curve index {start} has no binding"
            ))
        })?;
        if start != span.start {
            return Err(Error::invalid_data(
                "streamed animation frame starts inside a bound curve group",
            ));
        }
        let width = span.end - span.start;
        let end = cursor
            .checked_add(width)
            .ok_or_else(|| Error::invalid_data("streamed animation group range overflowed"))?;
        if end > frame.keys.len() {
            return Err(Error::invalid_data(
                "streamed animation frame ends inside a bound curve group",
            ));
        }
        append_bound_sample(
            context,
            span,
            frame.time,
            (0..width).map(|offset| frame.keys[cursor + offset].value),
        )?;
        cursor = end;
    }
    Ok(())
}

fn convert_dense_clip(
    context: &mut MuscleBuildContext<'_, '_>,
    clip: &MuscleClipData,
    stream_count: usize,
    total_sample_values: &mut usize,
) -> Result<()> {
    let dense = &clip.dense;
    charge_total(
        dense.samples.count,
        total_sample_values,
        context.limits.maximum_total_sample_values,
        "animation sample values",
    )?;
    let samples = dense
        .samples
        .read_values(context.limits.clip.maximum_sample_values)?;
    let curve_count = usize::try_from(dense.curve_count)
        .map_err(|_| Error::invalid_data("dense animation curve count does not fit usize"))?;
    let expected = dense
        .frame_count
        .checked_mul(curve_count)
        .ok_or_else(|| Error::invalid_data("dense animation sample count overflowed"))?;
    if samples.len() < expected {
        return Err(Error::invalid_data(format!(
            "dense animation has {} samples, fewer than required {expected}",
            samples.len()
        )));
    }
    let sample_rate = dense.sample_rate();
    let begin_time = dense.begin_time();
    if !sample_rate.is_finite() || sample_rate <= 0.0 || !begin_time.is_finite() {
        return Err(Error::invalid_data(
            "dense animation has an invalid sample rate or begin time",
        ));
    }
    for frame in 0..dense.frame_count {
        let frame_u32 = u32::try_from(frame)
            .map_err(|_| Error::invalid_data("dense frame index does not fit u32"))?;
        let time = begin_time + u32_to_f32(frame_u32) / sample_rate;
        validate_time(time)?;
        let offset = frame * curve_count;
        append_flat_sample_frame(
            context,
            stream_count,
            time,
            &samples[offset..offset + curve_count],
        )?;
    }
    Ok(())
}

fn convert_constant_clip(
    context: &mut MuscleBuildContext<'_, '_>,
    clip: &MuscleClipData,
    curve_offset: usize,
    stop_time: f32,
    total_sample_values: &mut usize,
) -> Result<()> {
    charge_total(
        clip.constant.values.count,
        total_sample_values,
        context.limits.maximum_total_sample_values,
        "animation sample values",
    )?;
    let values = clip
        .constant
        .values
        .read_values(context.limits.clip.maximum_sample_values)?;
    if values.is_empty() {
        return Ok(());
    }
    validate_time(stop_time)?;
    for time in [0.0, stop_time] {
        append_flat_sample_frame(context, curve_offset, time, &values)?;
    }
    Ok(())
}

fn append_flat_sample_frame(
    context: &mut MuscleBuildContext<'_, '_>,
    global_offset: usize,
    time: f32,
    values: &[f32],
) -> Result<()> {
    let mut cursor = 0_usize;
    while cursor < values.len() {
        let global_index = global_offset
            .checked_add(cursor)
            .ok_or_else(|| Error::invalid_data("animation curve index overflowed"))?;
        let span = context.layout.find(global_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "animation curve index {global_index} has no binding"
            ))
        })?;
        if global_index != span.start {
            return Err(Error::invalid_data(
                "animation samples start inside a bound curve group",
            ));
        }
        let width = span.end - span.start;
        let end = cursor
            .checked_add(width)
            .ok_or_else(|| Error::invalid_data("animation sample group range overflowed"))?;
        if end > values.len() {
            return Err(Error::invalid_data(
                "animation samples end inside a bound curve group",
            ));
        }
        append_bound_sample(context, span, time, values[cursor..end].iter().copied())?;
        cursor = end;
    }
    Ok(())
}

fn append_bound_sample(
    context: &mut MuscleBuildContext<'_, '_>,
    span: BindingSpan,
    time: f32,
    mut values: impl Iterator<Item = f32>,
) -> Result<()> {
    if span.binding.type_id == SKINNED_MESH_RENDERER_CLASS_ID {
        let value = values
            .next()
            .ok_or_else(|| Error::invalid_data("blend-shape animation sample is missing"))?;
        let node = context
            .paths
            .resolve_hash(span.binding.path, context.avatar_paths);
        let Some(target) = context.blend_shapes.resolve(node, span.binding.attribute) else {
            return Ok(());
        };
        return append_muscle_blend_shape_sample(context, target, time, value);
    }
    if span.binding.type_id != crate::scene::TRANSFORM_CLASS_ID {
        return Ok(());
    }
    let Some(track_index) = resolve_muscle_track(context, span.binding.path)? else {
        return Ok(());
    };
    let mut components = [0.0_f32; 4];
    let width = span.end - span.start;
    for (slot, value) in components.iter_mut().zip(values).take(width) {
        if !value.is_finite() {
            return Err(Error::invalid_data(
                "animation sample contains a non-finite value",
            ));
        }
        *slot = value;
    }
    let (destination, value) = match span.binding.attribute {
        1 => (
            &mut context.tracks[track_index].translations,
            [-components[0], components[1], components[2]],
        ),
        2 => (
            &mut context.tracks[track_index].rotations,
            quaternion_to_euler_degrees([
                components[0],
                -components[1],
                -components[2],
                components[3],
            ])?,
        ),
        3 => (
            &mut context.tracks[track_index].scalings,
            [components[0], components[1], components[2]],
        ),
        4 => (
            &mut context.tracks[track_index].rotations,
            [components[0], -components[1], -components[2]],
        ),
        _ => return Ok(()),
    };
    charge_keyframes(1, context.total_keyframes, context.limits)?;
    destination
        .try_reserve(1)
        .map_err(|error| Error::invalid_data(format!("cannot grow animation keys: {error}")))?;
    destination.push(ModelVectorKeyframe { time, value });
    Ok(())
}

fn append_muscle_blend_shape_sample(
    context: &mut MuscleBuildContext<'_, '_>,
    target: usize,
    time: f32,
    value: f32,
) -> Result<()> {
    let mut output = BlendShapeOutputContext {
        index: context.blend_shapes,
        tracks: context.blend_shape_tracks,
        by_target: context.by_blend_shape,
        total_tracks: context.total_tracks,
        total_keyframes: context.total_keyframes,
        total_string_bytes: context.total_string_bytes,
        limits: context.limits,
    };
    output.append(target, time, value)
}

fn resolve_muscle_track(
    context: &mut MuscleBuildContext<'_, '_>,
    path_hash: u32,
) -> Result<Option<usize>> {
    let Some(node) = context.paths.resolve_hash(path_hash, context.avatar_paths) else {
        return Ok(None);
    };
    if let Some(index) = context.by_node.get(&node).copied() {
        return Ok(Some(index));
    }
    *context.total_tracks = context
        .total_tracks
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("model animation track count overflowed"))?;
    if *context.total_tracks > context.limits.maximum_tracks {
        return Err(Error::invalid_data(format!(
            "model animation tracks exceed limit {}",
            context.limits.maximum_tracks
        )));
    }
    context
        .tracks
        .try_reserve(1)
        .map_err(|error| Error::invalid_data(format!("cannot grow animation tracks: {error}")))?;
    context.by_node.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow animation track index: {error}"))
    })?;
    let index = context.tracks.len();
    context.tracks.push(ModelAnimationTrack {
        node,
        translations: Vec::new(),
        rotations: Vec::new(),
        scalings: Vec::new(),
    });
    context.by_node.insert(node, index);
    Ok(Some(index))
}

fn charge_total(additional: usize, total: &mut usize, maximum: usize, field: &str) -> Result<()> {
    *total = total
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data(format!("{field} count overflowed")))?;
    if *total > maximum {
        return Err(Error::invalid_data(format!(
            "{field} total {} exceeds limit {maximum}",
            *total
        )));
    }
    Ok(())
}

fn u32_to_f32(value: u32) -> f32 {
    let high = u16::try_from(value >> 16).expect("upper half of u32 always fits u16");
    let low = u16::try_from(value & 0xffff).expect("lower half of u32 always fits u16");
    f32::from(high) * 65_536.0 + f32::from(low)
}

fn convert_explicit_curve_slices(
    context: &mut CurveBuildContext<'_, '_>,
    curves: &ExplicitCurveSlices<'_>,
) -> Result<()> {
    for curve in curves.rotations {
        let Some(track) = resolve_track(context, &curve.path)? else {
            continue;
        };
        append_quaternion_keys(
            &mut context.tracks[track].rotations,
            curve,
            context.total_keyframes,
            context.limits,
        )?;
    }
    for curve in curves.positions {
        let Some(track) = resolve_track(context, &curve.path)? else {
            continue;
        };
        append_vector_keys(
            &mut context.tracks[track].translations,
            curve,
            |value| [-value.x, value.y, value.z],
            context.total_keyframes,
            context.limits,
        )?;
    }
    append_euler_and_scale_curves(context, curves)
}

fn convert_explicit_blend_shapes(
    context: &mut CurveBuildContext<'_, '_>,
    curves: &[FloatCurve],
) -> Result<()> {
    for curve in curves {
        if curve.class_id != SKINNED_MESH_RENDERER_CLASS_ID {
            continue;
        }
        let channel = curve
            .attribute
            .split_once('.')
            .map_or(curve.attribute.as_str(), |(_, channel)| channel);
        let hash = blend_shape_crc32(channel);
        let resolved_path = join_legacy_path(
            context.base_path,
            &curve.path,
            context.limits.maximum_path_bytes,
        )?;
        let node = context.paths.resolve_suffix(&resolved_path);
        let Some(target) = context.blend_shapes.resolve(node, hash) else {
            continue;
        };
        for key in &curve.curve.keyframes {
            append_explicit_blend_shape_sample(context, target, key.time, key.value)?;
        }
    }
    Ok(())
}

fn append_explicit_blend_shape_sample(
    context: &mut CurveBuildContext<'_, '_>,
    target: usize,
    time: f32,
    value: f32,
) -> Result<()> {
    let mut output = BlendShapeOutputContext {
        index: context.blend_shapes,
        tracks: context.blend_shape_tracks,
        by_target: context.by_blend_shape,
        total_tracks: context.total_tracks,
        total_keyframes: context.total_keyframes,
        total_string_bytes: context.total_string_bytes,
        limits: context.limits,
    };
    output.append(target, time, value)
}

struct BlendShapeOutputContext<'a, 'b> {
    index: &'a BlendShapeIndex,
    tracks: &'b mut Vec<ModelBlendShapeTrack>,
    by_target: &'b mut HashMap<usize, usize>,
    total_tracks: &'b mut usize,
    total_keyframes: &'b mut usize,
    total_string_bytes: &'b mut usize,
    limits: &'a ModelAnimationLimits,
}

impl BlendShapeOutputContext<'_, '_> {
    fn append(&mut self, target_index: usize, time: f32, value: f32) -> Result<()> {
        validate_time(time)?;
        if !value.is_finite() {
            return Err(Error::invalid_data(
                "blend-shape animation sample contains a non-finite value",
            ));
        }
        let track_index = if let Some(index) = self.by_target.get(&target_index).copied() {
            index
        } else {
            self.insert_track(target_index)?
        };
        charge_keyframes(1, self.total_keyframes, self.limits)?;
        let track = self
            .tracks
            .get_mut(track_index)
            .ok_or_else(|| Error::invalid_data("blend-shape animation track index is invalid"))?;
        track.keys.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow blend-shape animation keys: {error}"))
        })?;
        track.keys.push(ModelScalarKeyframe { time, value });
        Ok(())
    }

    fn insert_track(&mut self, target_index: usize) -> Result<usize> {
        let target =
            self.index.targets.get(target_index).ok_or_else(|| {
                Error::invalid_data("blend-shape animation target index is invalid")
            })?;
        *self.total_tracks = self
            .total_tracks
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("model animation track count overflowed"))?;
        if *self.total_tracks > self.limits.maximum_tracks {
            return Err(Error::invalid_data(format!(
                "model animation tracks exceed limit {}",
                self.limits.maximum_tracks
            )));
        }
        *self.total_string_bytes = self
            .total_string_bytes
            .checked_add(target.channel.len())
            .ok_or_else(|| Error::invalid_data("animation string byte budget overflowed"))?;
        if *self.total_string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "animation strings use {} bytes, exceeding limit {}",
                *self.total_string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        self.tracks.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow blend-shape animation tracks: {error}"))
        })?;
        self.by_target.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow blend-shape track index: {error}"))
        })?;
        let index = self.tracks.len();
        self.tracks.push(ModelBlendShapeTrack {
            node: target.node,
            channel: fallible_string(&target.channel, "blend-shape animation channel")?,
            keys: Vec::new(),
        });
        self.by_target.insert(target_index, index);
        Ok(index)
    }
}

fn append_euler_and_scale_curves(
    context: &mut CurveBuildContext<'_, '_>,
    curves: &ExplicitCurveSlices<'_>,
) -> Result<()> {
    for curve in curves.scales {
        let Some(track) = resolve_track(context, &curve.path)? else {
            continue;
        };
        append_vector_keys(
            &mut context.tracks[track].scalings,
            curve,
            |value| [value.x, value.y, value.z],
            context.total_keyframes,
            context.limits,
        )?;
    }
    for curve in curves.eulers {
        let Some(track) = resolve_track(context, &curve.path)? else {
            continue;
        };
        append_vector_keys(
            &mut context.tracks[track].rotations,
            curve,
            |value| [value.x, -value.y, -value.z],
            context.total_keyframes,
            context.limits,
        )?;
    }
    Ok(())
}

fn resolve_track(
    context: &mut CurveBuildContext<'_, '_>,
    curve_path: &str,
) -> Result<Option<usize>> {
    let resolved_path = join_legacy_path(
        context.base_path,
        curve_path,
        context.limits.maximum_path_bytes,
    )?;
    let Some(node) = context.paths.resolve_suffix(&resolved_path) else {
        return Ok(None);
    };
    if let Some(index) = context.by_node.get(&node).copied() {
        return Ok(Some(index));
    }
    *context.total_tracks = context
        .total_tracks
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("model animation track count overflowed"))?;
    if *context.total_tracks > context.limits.maximum_tracks {
        return Err(Error::invalid_data(format!(
            "model animation tracks exceed limit {}",
            context.limits.maximum_tracks
        )));
    }
    context
        .tracks
        .try_reserve(1)
        .map_err(|error| Error::invalid_data(format!("cannot grow animation tracks: {error}")))?;
    context.by_node.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow animation track index: {error}"))
    })?;
    let index = context.tracks.len();
    context.tracks.push(ModelAnimationTrack {
        node,
        translations: Vec::new(),
        rotations: Vec::new(),
        scalings: Vec::new(),
    });
    context.by_node.insert(node, index);
    Ok(Some(index))
}

fn append_quaternion_keys(
    destination: &mut Vec<ModelVectorKeyframe>,
    curve: &QuaternionCurve,
    total: &mut usize,
    limits: &ModelAnimationLimits,
) -> Result<()> {
    charge_keyframes(curve.curve.keyframes.len(), total, limits)?;
    destination
        .try_reserve(curve.curve.keyframes.len())
        .map_err(|error| Error::invalid_data(format!("cannot grow rotation keys: {error}")))?;
    for key in &curve.curve.keyframes {
        validate_time(key.time)?;
        let value =
            quaternion_to_euler_degrees([key.value.x, -key.value.y, -key.value.z, key.value.w])?;
        destination.push(ModelVectorKeyframe {
            time: key.time,
            value,
        });
    }
    Ok(())
}

fn append_compressed_quaternion_keys(
    destination: &mut Vec<ModelVectorKeyframe>,
    curve: &CompressedAnimationCurve,
    total: &mut usize,
    limits: &ModelAnimationLimits,
) -> Result<()> {
    let count = usize::try_from(curve.times.item_count)
        .map_err(|_| Error::invalid_data("compressed animation key count does not fit usize"))?;
    let value_count = usize::try_from(curve.values.item_count)
        .map_err(|_| Error::invalid_data("compressed quaternion count does not fit usize"))?;
    if count != value_count {
        return Err(Error::invalid_data(format!(
            "compressed animation has {count} times but {value_count} quaternions"
        )));
    }
    charge_keyframes(count, total, limits)?;
    if count == 0 {
        return Ok(());
    }
    if curve.times.bit_size > 32 {
        return Err(Error::invalid_data(format!(
            "packed animation bit width {} exceeds 32",
            curve.times.bit_size
        )));
    }
    let time_bits = count
        .checked_mul(usize::from(curve.times.bit_size))
        .ok_or_else(|| Error::invalid_data("compressed animation time bit size overflowed"))?;
    let time_bytes = time_bits.div_ceil(8);
    let quaternion_bytes = count
        .checked_mul(4)
        .ok_or_else(|| Error::invalid_data("compressed quaternion byte size overflowed"))?;
    let times_data = read_packed_prefix(
        &curve.times.data,
        time_bytes,
        limits.clip.maximum_packed_bytes,
        "compressed animation times",
    )?;
    let quaternion_data = read_packed_prefix(
        &curve.values.data,
        quaternion_bytes,
        limits.clip.maximum_packed_bytes,
        "compressed animation quaternions",
    )?;
    let mut time_reader = PackedBitReader::new(&times_data);
    let mut quaternion_reader = PackedBitReader::new(&quaternion_data);
    destination.try_reserve(count).map_err(|error| {
        Error::invalid_data(format!("cannot grow compressed rotation keys: {error}"))
    })?;
    let mut centiseconds = 0_u32;
    for _ in 0..count {
        let delta = time_reader.read(curve.times.bit_size)?;
        centiseconds = centiseconds.checked_add(delta).ok_or_else(|| {
            Error::invalid_data("compressed animation time accumulator overflowed")
        })?;
        let time = u32_to_f32(centiseconds) * 0.01;
        validate_time(time)?;
        let quaternion = unpack_quaternion(&mut quaternion_reader)?;
        let value = quaternion_to_euler_degrees([
            quaternion[0],
            -quaternion[1],
            -quaternion[2],
            quaternion[3],
        ])?;
        destination.push(ModelVectorKeyframe { time, value });
    }
    Ok(())
}

fn read_packed_prefix(
    data: &crate::animation_clip::ByteRegion,
    required: usize,
    maximum: u64,
    field: &str,
) -> Result<Vec<u8>> {
    let required = u64::try_from(required)
        .map_err(|_| Error::invalid_data(format!("{field} size does not fit u64")))?;
    if required > maximum {
        return Err(Error::invalid_data(format!(
            "{field} needs {required} bytes, exceeding limit {maximum}"
        )));
    }
    if required > data.byte_length {
        return Err(Error::invalid_data(format!(
            "{field} needs {required} bytes but contains only {}",
            data.byte_length
        )));
    }
    data.region.subregion(0, required)?.read_to_vec(maximum)
}

struct PackedBitReader<'a> {
    bytes: &'a [u8],
    bit_position: usize,
}

impl<'a> PackedBitReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_position: 0,
        }
    }

    fn read(&mut self, bit_count: u8) -> Result<u32> {
        if bit_count > 32 {
            return Err(Error::invalid_data(format!(
                "packed animation bit width {bit_count} exceeds 32"
            )));
        }
        let end = self
            .bit_position
            .checked_add(usize::from(bit_count))
            .ok_or_else(|| Error::invalid_data("packed animation bit range overflowed"))?;
        let available = self
            .bytes
            .len()
            .checked_mul(8)
            .ok_or_else(|| Error::invalid_data("packed animation byte size overflowed"))?;
        if end > available {
            return Err(Error::invalid_data(
                "packed animation data ends inside a value",
            ));
        }
        let mut output = 0_u32;
        for output_bit in 0..usize::from(bit_count) {
            let source_bit = self.bit_position + output_bit;
            let bit = (self.bytes[source_bit / 8] >> (source_bit % 8)) & 1;
            output |= u32::from(bit) << output_bit;
        }
        self.bit_position = end;
        Ok(output)
    }
}

fn unpack_quaternion(reader: &mut PackedBitReader<'_>) -> Result<[f32; 4]> {
    let flags = reader.read(3)?;
    let omitted = usize::try_from(flags & 3)
        .map_err(|_| Error::invalid_data("quaternion omitted component does not fit usize"))?;
    let reduced = (omitted + 1) % 4;
    let mut quaternion = [0.0_f32; 4];
    let mut sum = 0.0_f32;
    for (component, slot) in quaternion.iter_mut().enumerate() {
        if component == omitted {
            continue;
        }
        let bit_size = if component == reduced { 9 } else { 10 };
        let packed = reader.read(bit_size)?;
        let maximum = (1_u32 << bit_size) - 1;
        let value = u32_to_f32(packed) / (0.5 * u32_to_f32(maximum)) - 1.0;
        *slot = value;
        sum += value * value;
    }
    if !sum.is_finite() || sum > 1.0 {
        return Err(Error::invalid_data(
            "packed quaternion components have an invalid squared length",
        ));
    }
    quaternion[omitted] = (1.0 - sum).sqrt();
    if flags & 4 != 0 {
        quaternion[omitted] = -quaternion[omitted];
    }
    Ok(quaternion)
}

fn append_vector_keys(
    destination: &mut Vec<ModelVectorKeyframe>,
    curve: &Vector3Curve,
    convert: impl Fn(crate::animation_clip::Vector3) -> [f32; 3],
    total: &mut usize,
    limits: &ModelAnimationLimits,
) -> Result<()> {
    charge_keyframes(curve.curve.keyframes.len(), total, limits)?;
    destination
        .try_reserve(curve.curve.keyframes.len())
        .map_err(|error| Error::invalid_data(format!("cannot grow vector keys: {error}")))?;
    for key in &curve.curve.keyframes {
        validate_time(key.time)?;
        let value = convert(key.value);
        if value.into_iter().any(|component| !component.is_finite()) {
            return Err(Error::invalid_data(
                "animation keyframe contains a non-finite value",
            ));
        }
        destination.push(ModelVectorKeyframe {
            time: key.time,
            value,
        });
    }
    Ok(())
}

fn charge_keyframes(
    additional: usize,
    total: &mut usize,
    limits: &ModelAnimationLimits,
) -> Result<()> {
    *total = total
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data("model animation keyframe count overflowed"))?;
    if *total > limits.maximum_keyframes {
        return Err(Error::invalid_data(format!(
            "model animation keyframes exceed limit {}",
            limits.maximum_keyframes
        )));
    }
    Ok(())
}

fn validate_time(time: f32) -> Result<()> {
    if time.is_finite() {
        Ok(())
    } else {
        Err(Error::invalid_data("animation keyframe time is non-finite"))
    }
}

fn join_legacy_path(base: Option<&str>, path: &str, maximum_path_bytes: usize) -> Result<String> {
    let length = base.map_or(path.len(), |base| {
        base.len()
            .checked_add(1)
            .and_then(|length| length.checked_add(path.len()))
            .unwrap_or(usize::MAX)
    });
    if length > maximum_path_bytes {
        return Err(Error::invalid_data(format!(
            "animation path is {length} bytes, exceeding limit {maximum_path_bytes}"
        )));
    }
    let mut output = String::new();
    output
        .try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate animation path: {error}")))?;
    if let Some(base) = base {
        output.push_str(base);
        output.push('/');
    }
    output.push_str(path);
    Ok(output)
}

struct ModelPathEntry {
    key: SceneObjectKey,
    path: String,
}

struct ModelPathIndex {
    entries: Vec<ModelPathEntry>,
    hash_nodes: HashMap<u32, SceneObjectKey>,
    total_string_bytes: usize,
}

impl ModelPathIndex {
    fn build(model: &ModelIr, limits: &ModelAnimationLimits) -> Result<Self> {
        let mut builder = ModelPathBuilder {
            model,
            maximum_path_bytes: limits.maximum_path_bytes,
            maximum_total_bytes: limits.maximum_total_string_bytes,
            total_bytes: 0,
            entries: Vec::new(),
            visited: HashSet::new(),
        };
        for root in &model.roots {
            builder.visit(*root, None)?;
        }
        let hash_nodes = build_path_hash_index(&builder.entries, limits.maximum_path_hashes)?;
        Ok(Self {
            entries: builder.entries,
            hash_nodes,
            total_string_bytes: builder.total_bytes,
        })
    }

    fn path(&self, key: SceneObjectKey) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.path.as_str())
    }

    fn resolve_suffix(&self, suffix: &str) -> Option<SceneObjectKey> {
        let name = suffix.rsplit('/').next()?;
        self.entries
            .iter()
            .find(|entry| {
                entry.path.ends_with(suffix) && entry.path.rsplit('/').next() == Some(name)
            })
            .map(|entry| entry.key)
    }

    fn resolve_hash(
        &self,
        hash: u32,
        avatar_paths: Option<&[crate::avatar::AvatarPath]>,
    ) -> Option<SceneObjectKey> {
        self.hash_nodes.get(&hash).copied().or_else(|| {
            avatar_paths
                .and_then(|paths| paths.iter().find(|entry| entry.hash == hash))
                .map(|entry| entry.path.as_str())
                .and_then(|path| self.resolve_suffix(path))
        })
    }
}

fn build_path_hash_index(
    entries: &[ModelPathEntry],
    maximum: usize,
) -> Result<HashMap<u32, SceneObjectKey>> {
    let mut count = 0_usize;
    for entry in entries {
        count = count
            .checked_add(entry.path.bytes().filter(|byte| *byte == b'/').count() + 1)
            .ok_or_else(|| Error::invalid_data("animation path-hash count overflowed"))?;
        if count > maximum {
            return Err(Error::invalid_data(format!(
                "animation path hashes exceed limit {maximum}"
            )));
        }
    }
    let mut output = HashMap::new();
    output.try_reserve(count).map_err(|error| {
        Error::invalid_data(format!("cannot allocate animation path hashes: {error}"))
    })?;
    for entry in entries {
        let mut suffix = entry.path.as_str();
        loop {
            output.insert(unity_crc32(suffix.as_bytes()), entry.key);
            let Some(slash) = suffix.find('/') else {
                break;
            };
            suffix = &suffix[slash + 1..];
        }
    }
    Ok(output)
}

pub(crate) fn unity_crc32(bytes: &[u8]) -> u32 {
    unity_crc32_parts(&[bytes])
}

fn blend_shape_crc32(channel: &str) -> u32 {
    unity_crc32_parts(&[b"blendShape.", channel.as_bytes()])
}

fn unity_crc32_parts(parts: &[&[u8]]) -> u32 {
    let mut crc = u32::MAX;
    for bytes in parts {
        for byte in *bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
            }
        }
    }
    !crc
}

struct ModelPathBuilder<'a> {
    model: &'a ModelIr,
    maximum_path_bytes: usize,
    maximum_total_bytes: usize,
    total_bytes: usize,
    entries: Vec<ModelPathEntry>,
    visited: HashSet<SceneObjectKey>,
}

impl ModelPathBuilder<'_> {
    fn visit(&mut self, key: SceneObjectKey, parent_index: Option<usize>) -> Result<()> {
        if self.visited.contains(&key) {
            return Ok(());
        }
        self.visited.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow visited model nodes: {error}"))
        })?;
        self.visited.insert(key);
        let node = self
            .model
            .node(key)
            .ok_or_else(|| Error::invalid_data("model path references a missing node"))?;
        let parent_path = parent_index
            .map(|index| {
                self.entries
                    .get(index)
                    .map(|entry| entry.path.as_str())
                    .ok_or_else(|| Error::invalid_data("model parent path index is invalid"))
            })
            .transpose()?;
        let path = join_legacy_path(parent_path, &node.name, self.maximum_path_bytes)?;
        self.total_bytes = self
            .total_bytes
            .checked_add(path.len())
            .ok_or_else(|| Error::invalid_data("model path byte budget overflowed"))?;
        if self.total_bytes > self.maximum_total_bytes {
            return Err(Error::invalid_data(format!(
                "model paths use {} bytes, exceeding limit {}",
                self.total_bytes, self.maximum_total_bytes
            )));
        }
        self.entries
            .try_reserve(1)
            .map_err(|error| Error::invalid_data(format!("cannot grow model paths: {error}")))?;
        self.entries.push(ModelPathEntry { key, path });
        let path_index = self.entries.len() - 1;
        let child_count = node.children.len();
        for child_index in 0..child_count {
            let child = self
                .model
                .node(key)
                .and_then(|node| node.children.get(child_index))
                .copied()
                .ok_or_else(|| Error::invalid_data("model child index changed during traversal"))?;
            self.visit(child, Some(path_index))?;
        }
        Ok(())
    }
}

fn fallible_string(value: &str, field: &str) -> Result<String> {
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    output.push_str(value);
    Ok(output)
}

fn fallible_format_name(base: &str, suffix: usize) -> Result<String> {
    use std::fmt::Write as _;

    let mut output = String::new();
    output
        .try_reserve(base.len().saturating_add(24))
        .map_err(|error| Error::invalid_data(format!("cannot allocate animation name: {error}")))?;
    output.push_str(base);
    write!(output, "_{suffix}")
        .map_err(|error| Error::invalid_data(format!("cannot format animation name: {error}")))?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::acl::AclDecodedClip;
    use crate::animation_clip::{
        AnimationCurve, ByteRegion, CompressedAnimationCurve, ConstantClip, DenseClip, F32Array,
        FloatCurve, GenericBinding, Keyframe, MuscleClipData, PackedFloatVector, PackedIntVector,
        PackedQuaternionVector, QuaternionCurve, StreamedClip, U32Array, Vector3, Vector3Curve,
        Vector4,
    };
    use crate::animation_graph::{
        AnimationGraph, AnimatorAnimationBinding, LegacyAnimationBinding,
    };
    use crate::avatar::AvatarPath;
    use crate::endian::Endian;
    use crate::mesh::{Mesh, MeshBlendShapeChannel, MeshBlendShapes};
    use crate::model_ir::{ModelIr, ModelMesh, ModelNode, ModelRendererBinding, ModelRendererKind};
    use crate::scene_hierarchy::SceneObjectKey;
    use crate::serialized::ObjectReference;
    use crate::source::Region;

    use super::{
        BindingLayout, BlendShapeIndex, CurveBuildContext, ExplicitCurveSlices,
        ModelAnimationLimits, ModelPathIndex, MuscleBuildContext, append_bound_sample,
        append_compressed_quaternion_keys, blend_shape_crc32, convert_explicit_blend_shapes,
        convert_explicit_curve_slices, convert_muscle_clip, convert_muscle_clip_with_acl,
        join_legacy_path, select_clips, unity_crc32,
    };

    #[test]
    fn selects_only_animation_bindings_owned_by_the_selected_model() {
        let mut model = model_fixture();
        model.nodes[0].export_content = false;
        let graph = AnimationGraph::from_test_bindings(
            vec![
                AnimatorAnimationBinding {
                    game_object: key(1),
                    animator: key(101),
                    avatar: None,
                    controller: None,
                    bound_clips: vec![Some(key(201))],
                },
                AnimatorAnimationBinding {
                    game_object: key(999),
                    animator: key(102),
                    avatar: None,
                    controller: None,
                    bound_clips: vec![Some(key(202))],
                },
            ],
            vec![
                LegacyAnimationBinding {
                    component: key(111),
                    game_object: Some(key(2)),
                    default_clip: None,
                    clips: vec![Some(key(203))],
                },
                LegacyAnimationBinding {
                    component: key(112),
                    game_object: Some(key(998)),
                    default_clip: None,
                    clips: vec![Some(key(204))],
                },
            ],
        );

        let selected = select_clips(&graph, &model, 10).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].clip, key(203));
        assert_eq!(selected[0].legacy_base, Some(key(2)));
    }

    /// Tolerance for Euler angle comparisons, in degrees.
    ///
    /// These angles come out of f32 `atan2` and `to_degrees`, and libm
    /// implementations are only required to be close to the correctly rounded
    /// result, not identical to each other. For the 45-degree case the correctly
    /// rounded f32 answer is already -45.0000076, so a 1e-5 bound left under two
    /// ulps of headroom: it passed on ARM macOS and failed on x86-64 Linux purely
    /// on an `atan2` ulp. A thousandth of a degree is orders of magnitude below any
    /// geometric significance and survives an implementation difference.
    const EULER_DEGREES_TOLERANCE: f32 = 1e-3;

    #[test]
    fn converts_legacy_transform_curves_and_resolves_suffix_paths() {
        let model = model_fixture();
        let limits = ModelAnimationLimits::default();
        let paths = ModelPathIndex::build(&model, &limits).unwrap();
        let avatar_paths = [AvatarPath {
            hash: 0x1234_5678,
            path: "Root/Arm".to_owned(),
        }];
        assert_eq!(
            paths.resolve_hash(0x1234_5678, Some(&avatar_paths)),
            Some(key(2))
        );
        let curves = curve_fixture();
        let mut tracks = Vec::new();
        let mut by_node = HashMap::new();
        let mut total_tracks = 0;
        let mut total_keyframes = 0;
        let blend_shapes = empty_blend_shapes();
        let mut blend_shape_tracks = Vec::new();
        let mut by_blend_shape = HashMap::new();
        let mut total_string_bytes = paths.total_string_bytes;
        let mut context = CurveBuildContext {
            paths: &paths,
            base_path: None,
            tracks: &mut tracks,
            by_node: &mut by_node,
            total_tracks: &mut total_tracks,
            total_keyframes: &mut total_keyframes,
            limits: &limits,
            blend_shapes: &blend_shapes,
            blend_shape_tracks: &mut blend_shape_tracks,
            by_blend_shape: &mut by_blend_shape,
            total_string_bytes: &mut total_string_bytes,
        };
        convert_explicit_curve_slices(&mut context, &curves).unwrap();

        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].node, key(2));
        assert_eq!(
            tracks[0].translations[0].value.map(f32::to_bits),
            [-1.0_f32, 2.0, 3.0].map(f32::to_bits)
        );
        assert_eq!(
            tracks[0].scalings[0].value.map(f32::to_bits),
            [2.0_f32, 3.0, 4.0].map(f32::to_bits)
        );
        assert_eq!(tracks[0].rotations.len(), 2);
        assert!((tracks[0].rotations[0].value[2] + 45.0).abs() < EULER_DEGREES_TOLERANCE);
        assert_eq!(
            tracks[0].rotations[1].value.map(f32::to_bits),
            [10.0_f32, -20.0, -30.0].map(f32::to_bits)
        );
        assert_eq!(total_tracks, 1);
        assert_eq!(total_keyframes, 4);
    }

    #[test]
    fn legacy_base_paths_and_all_collection_limits_are_strict() {
        let model = model_fixture();
        let mut limits = ModelAnimationLimits::default();
        let paths = ModelPathIndex::build(&model, &limits).unwrap();
        let curves = curve_fixture();
        limits.maximum_keyframes = 3;
        let mut tracks = Vec::new();
        let mut by_node = HashMap::new();
        let mut total_tracks = 0;
        let mut total_keyframes = 0;
        let blend_shapes = empty_blend_shapes();
        let mut blend_shape_tracks = Vec::new();
        let mut by_blend_shape = HashMap::new();
        let mut total_string_bytes = paths.total_string_bytes;
        let mut context = CurveBuildContext {
            paths: &paths,
            base_path: Some("Root"),
            tracks: &mut tracks,
            by_node: &mut by_node,
            total_tracks: &mut total_tracks,
            total_keyframes: &mut total_keyframes,
            limits: &limits,
            blend_shapes: &blend_shapes,
            blend_shape_tracks: &mut blend_shape_tracks,
            by_blend_shape: &mut by_blend_shape,
            total_string_bytes: &mut total_string_bytes,
        };
        assert!(convert_explicit_curve_slices(&mut context, &curves).is_err());

        limits.maximum_path_bytes = 3;
        assert!(ModelPathIndex::build(&model, &limits).is_err());
        assert!(join_legacy_path(Some("Root"), "Arm", 7).is_err());
    }

    #[test]
    fn converts_legacy_and_modern_blend_shape_weights_to_exported_channels() {
        let model = blend_shape_model_fixture();
        let limits = ModelAnimationLimits::default();
        let paths = ModelPathIndex::build(&model, &limits).unwrap();
        let blend_shapes = BlendShapeIndex::build(
            &model,
            limits.maximum_blend_shape_channels,
            limits.maximum_name_bytes,
            limits.maximum_total_string_bytes - paths.total_string_bytes,
        )
        .unwrap();
        let mut tracks = Vec::new();
        let mut by_node = HashMap::new();
        let mut blend_tracks = Vec::new();
        let mut by_blend = HashMap::new();
        let mut total_tracks = 0;
        let mut total_keyframes = 0;
        let mut total_strings = paths.total_string_bytes + blend_shapes.total_string_bytes;
        let mut context = CurveBuildContext {
            paths: &paths,
            base_path: None,
            tracks: &mut tracks,
            by_node: &mut by_node,
            total_tracks: &mut total_tracks,
            total_keyframes: &mut total_keyframes,
            limits: &limits,
            blend_shapes: &blend_shapes,
            blend_shape_tracks: &mut blend_tracks,
            by_blend_shape: &mut by_blend,
            total_string_bytes: &mut total_strings,
        };
        convert_explicit_blend_shapes(
            &mut context,
            &[float_curve("Arm", "blendShape.face.Smile", 0.25, 12.5)],
        )
        .unwrap();
        assert_eq!(blend_tracks.len(), 1);
        assert_eq!(blend_tracks[0].node, key(2));
        assert_eq!(blend_tracks[0].channel, "Smile");
        assert_eq!(blend_tracks[0].keys[0].time.to_bits(), 0.25_f32.to_bits());
        assert_eq!(blend_tracks[0].keys[0].value.to_bits(), 12.5_f32.to_bits());

        let binding = GenericBinding {
            path: unity_crc32(b"Arm"),
            attribute: blend_shape_crc32("face.Smile"),
            script: ObjectReference {
                file_id: 0,
                path_id: 0,
            },
            type_id: crate::renderer::SKINNED_MESH_RENDERER_CLASS_ID,
            custom_type: 0,
            is_pptr_curve: 0,
            is_int_curve: None,
            is_serialize_reference_curve: None,
        };
        let layout = BindingLayout::build(&[binding]).unwrap();
        let mut modern_tracks = Vec::new();
        let mut modern_by_node = HashMap::new();
        let mut modern_blends = Vec::new();
        let mut modern_by_blend = HashMap::new();
        let mut modern_total_tracks = 0;
        let mut modern_total_keys = 0;
        let mut modern_total_strings = paths.total_string_bytes + blend_shapes.total_string_bytes;
        let mut modern = MuscleBuildContext {
            paths: &paths,
            avatar_paths: None,
            layout: &layout,
            tracks: &mut modern_tracks,
            by_node: &mut modern_by_node,
            total_tracks: &mut modern_total_tracks,
            total_keyframes: &mut modern_total_keys,
            limits: &limits,
            blend_shapes: &blend_shapes,
            blend_shape_tracks: &mut modern_blends,
            by_blend_shape: &mut modern_by_blend,
            total_string_bytes: &mut modern_total_strings,
        };
        append_bound_sample(&mut modern, layout.spans[0], 0.5, [75.0].into_iter()).unwrap();
        assert_eq!(modern_blends.len(), 1);
        assert_eq!(modern_blends[0].node, key(2));
        assert_eq!(modern_blends[0].channel, "Smile");
        assert_eq!(modern_blends[0].keys[0].value.to_bits(), 75.0_f32.to_bits());
    }

    #[test]
    fn converts_streamed_dense_and_constant_transform_samples() {
        assert_eq!(unity_crc32(b"123456789"), 0xcbf4_3926);
        let model = model_fixture();
        let limits = ModelAnimationLimits::default();
        let paths = ModelPathIndex::build(&model, &limits).unwrap();
        let bindings = transform_bindings(unity_crc32(b"Arm"));
        let layout = BindingLayout::build(&bindings).unwrap();
        let clip = muscle_fixture();
        let mut tracks = Vec::new();
        let mut by_node = HashMap::new();
        let mut total_tracks = 0;
        let mut total_keyframes = 0;
        let mut streamed_words = 0;
        let mut samples = 0;
        let blend_shapes = empty_blend_shapes();
        let mut blend_shape_tracks = Vec::new();
        let mut by_blend_shape = HashMap::new();
        let mut total_string_bytes = paths.total_string_bytes;
        let mut context = MuscleBuildContext {
            paths: &paths,
            avatar_paths: None,
            layout: &layout,
            tracks: &mut tracks,
            by_node: &mut by_node,
            total_tracks: &mut total_tracks,
            total_keyframes: &mut total_keyframes,
            limits: &limits,
            blend_shapes: &blend_shapes,
            blend_shape_tracks: &mut blend_shape_tracks,
            by_blend_shape: &mut by_blend_shape,
            total_string_bytes: &mut total_string_bytes,
        };
        convert_muscle_clip(&mut context, &clip, 2.0, &mut streamed_words, &mut samples).unwrap();

        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].node, key(2));
        assert_eq!(tracks[0].translations.len(), 1);
        assert_eq!(tracks[0].translations[0].time.to_bits(), 0.25_f32.to_bits());
        assert_eq!(
            tracks[0].translations[0].value.map(f32::to_bits),
            [-1.0_f32, 2.0, 3.0].map(f32::to_bits)
        );
        assert_eq!(tracks[0].scalings.len(), 2);
        assert_eq!(
            tracks[0].scalings[1].value.map(f32::to_bits),
            [2.0_f32, 3.0, 4.0].map(f32::to_bits)
        );
        assert_eq!(tracks[0].rotations.len(), 3);
        assert_eq!(tracks[0].rotations[0].time.to_bits(), 0.5_f32.to_bits());
        assert_eq!(
            tracks[0].rotations[1].value.map(f32::to_bits),
            [10.0_f32, -20.0, -30.0].map(f32::to_bits)
        );
        assert_eq!(tracks[0].rotations[2].time.to_bits(), 2.0_f32.to_bits());
        assert_eq!(total_tracks, 1);
        assert_eq!(total_keyframes, 6);
        assert_eq!(samples, 13);
    }

    #[test]
    fn muscle_conversion_rejects_truncated_frames_and_cumulative_samples() {
        let model = model_fixture();
        let mut limits = ModelAnimationLimits::default();
        let paths = ModelPathIndex::build(&model, &limits).unwrap();
        let bindings = transform_bindings(unity_crc32(b"Arm"));
        let layout = BindingLayout::build(&bindings).unwrap();
        let mut clip = muscle_fixture();
        limits.maximum_total_sample_values = 2;
        assert!(convert_muscle_for_test(&paths, &layout, &clip, &limits).is_err());

        limits.maximum_total_sample_values = usize::MAX;
        clip.streamed.data = u32_array(&[0.0_f32.to_bits(), 1, 0]);
        assert!(convert_muscle_for_test(&paths, &layout, &clip, &limits).is_err());
    }

    #[test]
    fn prepends_validated_acl_frames_and_offsets_standard_curves() {
        let model = model_fixture();
        let limits = ModelAnimationLimits::default();
        let paths = ModelPathIndex::build(&model, &limits).unwrap();
        let layout = BindingLayout::build(&transform_bindings(unity_crc32(b"Arm"))).unwrap();
        let mut clip = muscle_fixture();
        let mut words = Vec::new();
        push_streamed_frame(&mut words, f32::NEG_INFINITY, &[]);
        push_streamed_frame(&mut words, 0.25, &[(0, 0.0), (1, 0.0), (2, 0.0), (3, 1.0)]);
        push_streamed_frame(&mut words, f32::INFINITY, &[]);
        clip.streamed = StreamedClip {
            data: u32_array(&words),
            curve_count: 4,
            discrete_curve_count: None,
        };
        clip.dense.curve_count = 3;
        clip.dense.samples = f32_array(&[2.0, 3.0, 4.0]);
        clip.constant.values = f32_array(&[10.0, 20.0, 30.0]);
        let decoded = AclDecodedClip {
            times: vec![0.0, 0.5],
            binding_indices: vec![0, 1, 2],
            values: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            following_curve_offset: 3,
        };
        let tracks =
            convert_muscle_with_acl_for_test(&paths, &layout, &clip, &limits, &decoded).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].translations.len(), 2);
        assert_eq!(tracks[0].rotations.len(), 3);
        assert_eq!(tracks[0].scalings.len(), 1);
        assert_eq!(
            tracks[0].translations[1].value.map(f32::to_bits),
            [-4.0_f32, 5.0, 6.0].map(f32::to_bits)
        );
    }

    #[test]
    fn decodes_delta_times_and_packed_quaternions_with_strict_bounds() {
        let mut quaternion_bits = Vec::new();
        let mut bit_position = 0;
        push_bits(&mut quaternion_bits, &mut bit_position, 3, 3);
        push_bits(&mut quaternion_bits, &mut bit_position, 256, 9);
        push_bits(&mut quaternion_bits, &mut bit_position, 512, 10);
        push_bits(&mut quaternion_bits, &mut bit_position, 512, 10);
        push_bits(&mut quaternion_bits, &mut bit_position, 3, 3);
        push_bits(&mut quaternion_bits, &mut bit_position, 256, 9);
        push_bits(&mut quaternion_bits, &mut bit_position, 512, 10);
        push_bits(&mut quaternion_bits, &mut bit_position, 873, 10);
        let curve = compressed_curve(vec![0, 50], quaternion_bits);
        let mut keys = Vec::new();
        let mut total = 0;
        append_compressed_quaternion_keys(
            &mut keys,
            &curve,
            &mut total,
            &ModelAnimationLimits::default(),
        )
        .unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].time.to_bits(), 0.0_f32.to_bits());
        assert_eq!(keys[1].time.to_bits(), 0.5_f32.to_bits());
        assert!(keys[0].value.into_iter().all(|value| value.abs() < 0.25));
        assert!((keys[1].value[2] + 90.0).abs() < 0.5);

        let mut truncated = curve;
        truncated.values.data = byte_region(vec![0; 7]);
        let mut truncated_total = 0;
        assert!(
            append_compressed_quaternion_keys(
                &mut Vec::new(),
                &truncated,
                &mut truncated_total,
                &ModelAnimationLimits::default(),
            )
            .is_err()
        );
    }

    fn convert_muscle_for_test(
        paths: &ModelPathIndex,
        layout: &BindingLayout,
        clip: &MuscleClipData,
        limits: &ModelAnimationLimits,
    ) -> crate::Result<()> {
        let mut tracks = Vec::new();
        let mut by_node = HashMap::new();
        let mut total_tracks = 0;
        let mut total_keyframes = 0;
        let mut streamed_words = 0;
        let mut samples = 0;
        let blend_shapes = empty_blend_shapes();
        let mut blend_shape_tracks = Vec::new();
        let mut by_blend_shape = HashMap::new();
        let mut total_string_bytes = paths.total_string_bytes;
        let mut context = MuscleBuildContext {
            paths,
            avatar_paths: None,
            layout,
            tracks: &mut tracks,
            by_node: &mut by_node,
            total_tracks: &mut total_tracks,
            total_keyframes: &mut total_keyframes,
            limits,
            blend_shapes: &blend_shapes,
            blend_shape_tracks: &mut blend_shape_tracks,
            by_blend_shape: &mut by_blend_shape,
            total_string_bytes: &mut total_string_bytes,
        };
        convert_muscle_clip(&mut context, clip, 2.0, &mut streamed_words, &mut samples)
    }

    fn convert_muscle_with_acl_for_test(
        paths: &ModelPathIndex,
        layout: &BindingLayout,
        clip: &MuscleClipData,
        limits: &ModelAnimationLimits,
        decoded: &AclDecodedClip,
    ) -> crate::Result<Vec<super::ModelAnimationTrack>> {
        let mut tracks = Vec::new();
        let mut by_node = HashMap::new();
        let mut total_tracks = 0;
        let mut total_keyframes = 0;
        let mut total_streamed_words = 0;
        let mut total_sample_values = 0;
        let blend_shapes = empty_blend_shapes();
        let mut blend_shape_tracks = Vec::new();
        let mut by_blend_shape = HashMap::new();
        let mut total_string_bytes = paths.total_string_bytes;
        let mut context = MuscleBuildContext {
            paths,
            avatar_paths: None,
            layout,
            tracks: &mut tracks,
            by_node: &mut by_node,
            total_tracks: &mut total_tracks,
            total_keyframes: &mut total_keyframes,
            limits,
            blend_shapes: &blend_shapes,
            blend_shape_tracks: &mut blend_shape_tracks,
            by_blend_shape: &mut by_blend_shape,
            total_string_bytes: &mut total_string_bytes,
        };
        convert_muscle_clip_with_acl(
            &mut context,
            clip,
            2.0,
            &mut total_streamed_words,
            &mut total_sample_values,
            Some(decoded),
        )?;
        Ok(tracks)
    }

    const fn empty_blend_shapes() -> BlendShapeIndex {
        BlendShapeIndex {
            targets: Vec::new(),
            by_node_hash: Vec::new(),
            by_hash: Vec::new(),
            total_string_bytes: 0,
        }
    }

    fn transform_bindings(path: u32) -> [GenericBinding; 4] {
        [
            binding(path, 1),
            binding(path, 2),
            binding(path, 3),
            binding(path, 4),
        ]
    }

    const fn binding(path: u32, attribute: u32) -> GenericBinding {
        GenericBinding {
            path,
            attribute,
            script: ObjectReference {
                file_id: 0,
                path_id: 0,
            },
            type_id: crate::scene::TRANSFORM_CLASS_ID,
            custom_type: 0,
            is_pptr_curve: 0,
            is_int_curve: None,
            is_serialize_reference_curve: None,
        }
    }

    fn muscle_fixture() -> MuscleClipData {
        let mut words = Vec::new();
        push_streamed_frame(&mut words, f32::NEG_INFINITY, &[]);
        push_streamed_frame(&mut words, 0.25, &[(0, 1.0), (1, 2.0), (2, 3.0)]);
        push_streamed_frame(&mut words, f32::INFINITY, &[]);
        MuscleClipData {
            streamed: StreamedClip {
                data: u32_array(&words),
                curve_count: 3,
                discrete_curve_count: None,
            },
            dense: DenseClip {
                frame_count: 1,
                curve_count: 4,
                sample_rate_bits: 30.0_f32.to_bits(),
                begin_time_bits: 0.5_f32.to_bits(),
                samples: f32_array(&[0.0, 0.0, 0.0, 1.0]),
            },
            constant: ConstantClip {
                values: f32_array(&[2.0, 3.0, 4.0, 10.0, 20.0, 30.0]),
            },
            binding: None,
            acl: None,
        }
    }

    fn push_streamed_frame(output: &mut Vec<u32>, time: f32, keys: &[(i32, f32)]) {
        output.push(time.to_bits());
        output.push(u32::try_from(keys.len()).unwrap());
        for (index, value) in keys {
            output.push(u32::from_ne_bytes(index.to_ne_bytes()));
            output.extend([0.0_f32.to_bits(), 0.0_f32.to_bits(), 0.0_f32.to_bits()]);
            output.push(value.to_bits());
        }
    }

    fn u32_array(values: &[u32]) -> U32Array {
        let mut bytes = Vec::with_capacity(values.len() * 4);
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        U32Array {
            region: Region::from_bytes(bytes),
            count: values.len(),
            endian: Endian::Little,
        }
    }

    fn f32_array(values: &[f32]) -> F32Array {
        let mut bytes = Vec::with_capacity(values.len() * 4);
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        F32Array {
            region: Region::from_bytes(bytes),
            count: values.len(),
            endian: Endian::Little,
        }
    }

    fn compressed_curve(times: Vec<u8>, quaternions: Vec<u8>) -> CompressedAnimationCurve {
        CompressedAnimationCurve {
            path: "Arm".to_owned(),
            times: PackedIntVector {
                item_count: 2,
                data: byte_region(times),
                bit_size: 8,
            },
            values: PackedQuaternionVector {
                item_count: 2,
                data: byte_region(quaternions),
            },
            slopes: PackedFloatVector {
                item_count: 0,
                range: 0.0,
                start: 0.0,
                data: byte_region(Vec::new()),
                bit_size: 0,
            },
            pre_infinity: 0,
            post_infinity: 0,
        }
    }

    fn byte_region(bytes: Vec<u8>) -> ByteRegion {
        let byte_length = u64::try_from(bytes.len()).unwrap();
        ByteRegion {
            region: Region::from_bytes(bytes),
            byte_length,
        }
    }

    fn push_bits(output: &mut Vec<u8>, bit_position: &mut usize, value: u32, count: u8) {
        let start = *bit_position;
        let end = start + usize::from(count);
        output.resize(end.div_ceil(8), 0);
        for bit in 0..usize::from(count) {
            if value & (1_u32 << bit) != 0 {
                let position = start + bit;
                output[position / 8] |= 1 << (position % 8);
            }
        }
        *bit_position = end;
    }

    fn curve_fixture() -> ExplicitCurveSlices<'static> {
        let rotations = Box::leak(Box::new([QuaternionCurve {
            curve: quaternion_curve(Vector4 {
                x: 0.0,
                y: 0.0,
                z: 0.382_683_43,
                w: 0.923_879_5,
            }),
            path: "Arm".to_owned(),
        }]));
        let positions = Box::leak(Box::new([
            vector_curve(
                "Arm",
                Vector3 {
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                },
            ),
            vector_curve(
                "Missing",
                Vector3 {
                    x: 9.0,
                    y: 9.0,
                    z: 9.0,
                },
            ),
        ]));
        let scales = Box::leak(Box::new([vector_curve(
            "Arm",
            Vector3 {
                x: 2.0,
                y: 3.0,
                z: 4.0,
            },
        )]));
        let eulers = Box::leak(Box::new([vector_curve(
            "Arm",
            Vector3 {
                x: 10.0,
                y: 20.0,
                z: 30.0,
            },
        )]));
        ExplicitCurveSlices {
            rotations,
            eulers,
            positions,
            scales,
        }
    }

    fn quaternion_curve(value: Vector4) -> AnimationCurve<Vector4> {
        AnimationCurve {
            keyframes: vec![Keyframe {
                time: 0.5,
                value,
                in_slope: value,
                out_slope: value,
                weighted_mode: None,
                in_weight: None,
                out_weight: None,
            }],
            pre_infinity: 0,
            post_infinity: 0,
            rotation_order: 0,
        }
    }

    fn vector_curve(path: &str, value: Vector3) -> Vector3Curve {
        Vector3Curve {
            curve: AnimationCurve {
                keyframes: vec![Keyframe {
                    time: 0.5,
                    value,
                    in_slope: value,
                    out_slope: value,
                    weighted_mode: None,
                    in_weight: None,
                    out_weight: None,
                }],
                pre_infinity: 0,
                post_infinity: 0,
                rotation_order: 0,
            },
            path: path.to_owned(),
        }
    }

    fn float_curve(path: &str, attribute: &str, time: f32, value: f32) -> FloatCurve {
        FloatCurve {
            curve: AnimationCurve {
                keyframes: vec![Keyframe {
                    time,
                    value,
                    in_slope: 0.0,
                    out_slope: 0.0,
                    weighted_mode: None,
                    in_weight: None,
                    out_weight: None,
                }],
                pre_infinity: 0,
                post_infinity: 0,
                rotation_order: 0,
            },
            attribute: attribute.to_owned(),
            path: path.to_owned(),
            class_id: crate::renderer::SKINNED_MESH_RENDERER_CLASS_ID,
            script: ObjectReference {
                file_id: 0,
                path_id: 0,
            },
            flags: None,
        }
    }

    fn blend_shape_model_fixture() -> ModelIr {
        let mesh_key = key(50);
        let mut nodes = model_fixture().nodes;
        nodes[1].renderers.push(ModelRendererBinding {
            component: key(40),
            kind: ModelRendererKind::SkinnedMeshRenderer { bones: Vec::new() },
            mesh: Some(mesh_key),
            materials: Vec::new(),
        });
        let mesh = ModelMesh {
            object: mesh_key,
            mesh: Mesh {
                path_id: mesh_key.path_id,
                name: "face".to_owned(),
                vertices: vec![[0.0; 3]],
                normals: None,
                uv0: None,
                bind_poses: Vec::new(),
                bone_name_hashes: Vec::new(),
                root_bone_name_hash: 0,
                skin: None,
                blend_shapes: Some(MeshBlendShapes {
                    vertices: Vec::new(),
                    frames: Vec::new(),
                    channels: vec![MeshBlendShapeChannel {
                        name: "face.Smile".to_owned(),
                        name_hash: 0,
                        frame_index: 0,
                        frame_count: 0,
                    }],
                    full_weights: Vec::new(),
                }),
                sub_meshes: Vec::new(),
            },
        };
        ModelIr::from_test_parts(nodes, vec![key(1)], vec![mesh], Vec::new())
    }

    fn model_fixture() -> ModelIr {
        ModelIr::from_test_parts(
            vec![
                ModelNode {
                    object: key(1),
                    name: "Root".to_owned(),
                    export_content: true,
                    parent: None,
                    children: vec![key(2)],
                    transform: None,
                    renderers: Vec::new(),
                    animator: None,
                },
                ModelNode {
                    object: key(2),
                    name: "Arm".to_owned(),
                    export_content: true,
                    parent: Some(key(1)),
                    children: Vec::new(),
                    transform: None,
                    renderers: Vec::new(),
                    animator: None,
                },
            ],
            vec![key(1)],
            Vec::new(),
            Vec::new(),
        )
    }

    const fn key(path_id: i64) -> SceneObjectKey {
        SceneObjectKey {
            file_index: 0,
            path_id,
        }
    }
}
