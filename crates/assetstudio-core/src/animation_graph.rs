//! Collection-wide animation bindings assembled from scene and controller references.
//!
//! This layer mirrors the managed model converter's `PPtr.TryGet` behavior:
//! null, missing, external-file, and wrongly typed targets are ignored. Once a
//! correctly typed target resolves, its bounded parser remains strict.

use std::collections::{BTreeMap, BTreeSet};

use crate::animation_clip::ANIMATION_CLIP_CLASS_ID;
use crate::animation_component::{
    ANIMATION_CLASS_ID, ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID, AnimationComponentReadLimits,
    read_animator_override_controller, read_legacy_animation_component,
};
use crate::animator_controller::{
    ANIMATOR_CONTROLLER_CLASS_ID, AnimatorControllerReadLimits, read_animator_controller,
};
use crate::loader::AssetCollection;
use crate::object_name::{ObjectNameReadLimits, read_object_name_metadata};
use crate::scene::{GAME_OBJECT_CLASS_ID, resolve_object_reference};
use crate::scene_hierarchy::{SceneHierarchy, SceneObjectKey};
use crate::serialized::ObjectReference;
use crate::{Error, Result};

const AVATAR_CLASS_ID: i32 = 90;

/// Collection-wide budgets for graph assembly in addition to the object-reader limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationGraphLimits {
    pub maximum_animator_bindings: usize,
    pub maximum_legacy_animations: usize,
    pub maximum_controllers: usize,
    pub maximum_clips: usize,
    pub maximum_edges: usize,
    pub maximum_total_name_bytes: usize,
    pub component: AnimationComponentReadLimits,
    pub controller: AnimatorControllerReadLimits,
    pub object_name: ObjectNameReadLimits,
}

impl Default for AnimationGraphLimits {
    fn default() -> Self {
        Self {
            maximum_animator_bindings: 1_000_000,
            maximum_legacy_animations: 1_000_000,
            maximum_controllers: 1_000_000,
            maximum_clips: 2_000_000,
            maximum_edges: 10_000_000,
            maximum_total_name_bytes: 256 * 1024 * 1024,
            component: AnimationComponentReadLimits::default(),
            controller: AnimatorControllerReadLimits::default(),
            object_name: ObjectNameReadLimits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimatorAnimationBinding {
    pub game_object: SceneObjectKey,
    pub animator: SceneObjectKey,
    pub avatar: Option<SceneObjectKey>,
    pub controller: Option<SceneObjectKey>,
    /// Clips selected by the existing managed model path. An override
    /// controller contributes its base `AnimatorController` clip list; the
    /// override pairs are preserved separately on the controller node.
    pub bound_clips: Vec<Option<SceneObjectKey>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyAnimationBinding {
    pub component: SceneObjectKey,
    pub game_object: Option<SceneObjectKey>,
    pub default_clip: Option<SceneObjectKey>,
    pub clips: Vec<Option<SceneObjectKey>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipOverrideBinding {
    pub original: Option<SceneObjectKey>,
    pub replacement: Option<SceneObjectKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimationControllerKind {
    AnimatorController {
        clips: Vec<Option<SceneObjectKey>>,
    },
    AnimatorOverrideController {
        base_controller: Option<SceneObjectKey>,
        overrides: Vec<ClipOverrideBinding>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationControllerNode {
    pub object: SceneObjectKey,
    pub name: String,
    pub kind: AnimationControllerKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationClipNode {
    pub object: SceneObjectKey,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct AnimationGraph {
    pub animators: Vec<AnimatorAnimationBinding>,
    pub legacy_animations: Vec<LegacyAnimationBinding>,
    pub controllers: Vec<AnimationControllerNode>,
    pub clips: Vec<AnimationClipNode>,
    controller_index: BTreeMap<SceneObjectKey, usize>,
    clip_index: BTreeMap<SceneObjectKey, usize>,
}

impl AnimationGraph {
    #[must_use]
    pub fn controller(&self, key: SceneObjectKey) -> Option<&AnimationControllerNode> {
        self.controller_index
            .get(&key)
            .and_then(|index| self.controllers.get(*index))
    }

    #[must_use]
    pub fn clip(&self, key: SceneObjectKey) -> Option<&AnimationClipNode> {
        self.clip_index
            .get(&key)
            .and_then(|index| self.clips.get(*index))
    }

    #[cfg(test)]
    pub(crate) fn from_test_bindings(
        animators: Vec<AnimatorAnimationBinding>,
        legacy_animations: Vec<LegacyAnimationBinding>,
    ) -> Self {
        Self {
            animators,
            legacy_animations,
            controllers: Vec::new(),
            clips: Vec::new(),
            controller_index: BTreeMap::new(),
            clip_index: BTreeMap::new(),
        }
    }
}

/// Builds animation bindings for an already assembled scene hierarchy.
pub fn build_animation_graph(
    collection: &AssetCollection,
    hierarchy: &SceneHierarchy,
    limits: AnimationGraphLimits,
) -> Result<AnimationGraph> {
    let mut state = GraphBuildState::new(collection, limits);
    state.collect_animators(hierarchy)?;
    state.collect_legacy_animations()?;
    state.read_pending_controllers()?;
    state.attach_managed_bound_clips();
    Ok(state.finish())
}

#[derive(Debug, Clone, Copy)]
struct PendingController {
    key: SceneObjectKey,
    object_index: usize,
    class_id: i32,
}

struct GraphBuildState<'a> {
    collection: &'a AssetCollection,
    limits: AnimationGraphLimits,
    animators: Vec<AnimatorAnimationBinding>,
    legacy_animations: Vec<LegacyAnimationBinding>,
    controllers: Vec<AnimationControllerNode>,
    clips: Vec<AnimationClipNode>,
    controller_index: BTreeMap<SceneObjectKey, usize>,
    clip_index: BTreeMap<SceneObjectKey, usize>,
    queued_controllers: BTreeSet<SceneObjectKey>,
    pending_controllers: Vec<PendingController>,
    pending_index: usize,
    edges: usize,
    name_bytes: usize,
}

impl<'a> GraphBuildState<'a> {
    fn new(collection: &'a AssetCollection, limits: AnimationGraphLimits) -> Self {
        Self {
            collection,
            limits,
            animators: Vec::new(),
            legacy_animations: Vec::new(),
            controllers: Vec::new(),
            clips: Vec::new(),
            controller_index: BTreeMap::new(),
            clip_index: BTreeMap::new(),
            queued_controllers: BTreeSet::new(),
            pending_controllers: Vec::new(),
            pending_index: 0,
            edges: 0,
            name_bytes: 0,
        }
    }

    fn collect_animators(&mut self, hierarchy: &SceneHierarchy) -> Result<()> {
        for node in &hierarchy.nodes {
            let Some(animator) = &node.animator else {
                continue;
            };
            if self.animators.len() >= self.limits.maximum_animator_bindings {
                return Err(Error::invalid_data(format!(
                    "animation graph exceeds {} Animator bindings",
                    self.limits.maximum_animator_bindings
                )));
            }
            self.charge_edges(2, "Animator avatar/controller references")?;
            let avatar = self.resolve_typed_key(
                animator.component.file_index,
                animator.avatar,
                &[AVATAR_CLASS_ID],
            );
            let controller = self.queue_controller_reference(
                animator.component.file_index,
                animator.controller,
                &[
                    ANIMATOR_CONTROLLER_CLASS_ID,
                    ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID,
                ],
            )?;
            self.animators.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow Animator bindings: {error}"))
            })?;
            self.animators.push(AnimatorAnimationBinding {
                game_object: node.object,
                animator: animator.component,
                avatar,
                controller,
                bound_clips: Vec::new(),
            });
        }
        Ok(())
    }

    fn collect_legacy_animations(&mut self) -> Result<()> {
        for (file_index, loaded) in self.collection.serialized_files.iter().enumerate() {
            for (object_index, object) in loaded.file.objects.iter().enumerate() {
                if object.class_id != ANIMATION_CLASS_ID {
                    continue;
                }
                if self.legacy_animations.len() >= self.limits.maximum_legacy_animations {
                    return Err(Error::invalid_data(format!(
                        "animation graph exceeds {} legacy Animation components",
                        self.limits.maximum_legacy_animations
                    )));
                }
                let animation = read_legacy_animation_component(
                    &loaded.file,
                    object_index,
                    self.limits.component,
                )?;
                let edge_count =
                    animation.clips.len().checked_add(2).ok_or_else(|| {
                        Error::invalid_data("legacy animation edge count overflowed")
                    })?;
                self.charge_edges(edge_count, "legacy Animation references")?;
                let game_object = self.resolve_typed_key(
                    file_index,
                    animation.behaviour.component.game_object,
                    &[GAME_OBJECT_CLASS_ID],
                );
                let default_clip = self.register_clip(file_index, animation.default_clip)?;
                let mut clips = reserve_vec(animation.clips.len(), "legacy Animation clips")?;
                for reference in animation.clips {
                    clips.push(self.register_clip(file_index, reference)?);
                }
                self.legacy_animations.try_reserve(1).map_err(|error| {
                    Error::invalid_data(format!("cannot grow legacy Animation bindings: {error}"))
                })?;
                self.legacy_animations.push(LegacyAnimationBinding {
                    component: SceneObjectKey {
                        file_index,
                        path_id: animation.path_id,
                    },
                    game_object,
                    default_clip,
                    clips,
                });
            }
        }
        Ok(())
    }

    fn read_pending_controllers(&mut self) -> Result<()> {
        while let Some(pending) = self.pending_controllers.get(self.pending_index).copied() {
            self.pending_index = self
                .pending_index
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("pending controller index overflowed"))?;
            let file = &self.collection.serialized_files[pending.key.file_index].file;
            let node = match pending.class_id {
                ANIMATOR_CONTROLLER_CLASS_ID => {
                    let controller = read_animator_controller(
                        file,
                        pending.object_index,
                        self.limits.controller,
                    )?;
                    self.charge_name(controller.name.len(), "AnimatorController name")?;
                    self.charge_edges(
                        controller.animation_clips.len(),
                        "AnimatorController clip references",
                    )?;
                    let mut clips =
                        reserve_vec(controller.animation_clips.len(), "AnimatorController clips")?;
                    for reference in controller.animation_clips {
                        clips.push(self.register_clip(pending.key.file_index, reference)?);
                    }
                    AnimationControllerNode {
                        object: pending.key,
                        name: controller.name,
                        kind: AnimationControllerKind::AnimatorController { clips },
                    }
                }
                ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID => {
                    let controller = read_animator_override_controller(
                        file,
                        pending.object_index,
                        self.limits.component,
                    )?;
                    self.charge_name(controller.name.len(), "AnimatorOverrideController name")?;
                    let edge_count = controller
                        .clips
                        .len()
                        .checked_mul(2)
                        .and_then(|count| count.checked_add(1))
                        .ok_or_else(|| Error::invalid_data("animation override edge overflowed"))?;
                    self.charge_edges(edge_count, "AnimatorOverrideController references")?;
                    let base_controller = self.queue_controller_reference(
                        pending.key.file_index,
                        controller.controller,
                        &[ANIMATOR_CONTROLLER_CLASS_ID],
                    )?;
                    let mut overrides =
                        reserve_vec(controller.clips.len(), "animation clip overrides")?;
                    for pair in controller.clips {
                        overrides.push(ClipOverrideBinding {
                            original: self
                                .register_clip(pending.key.file_index, pair.original_clip)?,
                            replacement: self
                                .register_clip(pending.key.file_index, pair.override_clip)?,
                        });
                    }
                    AnimationControllerNode {
                        object: pending.key,
                        name: controller.name,
                        kind: AnimationControllerKind::AnimatorOverrideController {
                            base_controller,
                            overrides,
                        },
                    }
                }
                _ => unreachable!("only typed controller targets are queued"),
            };
            if self.controllers.len() >= self.limits.maximum_controllers {
                return Err(Error::invalid_data(format!(
                    "animation graph exceeds {} controllers",
                    self.limits.maximum_controllers
                )));
            }
            let index = self.controllers.len();
            if self.controller_index.insert(node.object, index).is_some() {
                return Err(Error::invalid_data(format!(
                    "animation controller {:?} was parsed more than once",
                    node.object
                )));
            }
            self.controllers.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow animation controllers: {error}"))
            })?;
            self.controllers.push(node);
        }
        Ok(())
    }

    fn attach_managed_bound_clips(&mut self) {
        for binding in &mut self.animators {
            let Some(controller_key) = binding.controller else {
                continue;
            };
            let Some(controller_index) = self.controller_index.get(&controller_key).copied() else {
                continue;
            };
            let direct_key = match &self.controllers[controller_index].kind {
                AnimationControllerKind::AnimatorController { .. } => Some(controller_key),
                AnimationControllerKind::AnimatorOverrideController {
                    base_controller, ..
                } => *base_controller,
            };
            let Some(direct_index) =
                direct_key.and_then(|key| self.controller_index.get(&key).copied())
            else {
                continue;
            };
            if let AnimationControllerKind::AnimatorController { clips } =
                &self.controllers[direct_index].kind
            {
                binding.bound_clips.clone_from(clips);
            }
        }
    }

    fn queue_controller_reference(
        &mut self,
        source_file_index: usize,
        reference: ObjectReference,
        expected_classes: &[i32],
    ) -> Result<Option<SceneObjectKey>> {
        let Some(target) = try_resolve_target(self.collection, source_file_index, reference) else {
            return Ok(None);
        };
        if !expected_classes.contains(&target.class_id) {
            return Ok(None);
        }
        let key = SceneObjectKey {
            file_index: target.file_index,
            path_id: target.path_id,
        };
        if self.queued_controllers.insert(key) {
            if self.queued_controllers.len() > self.limits.maximum_controllers {
                return Err(Error::invalid_data(format!(
                    "animation graph exceeds {} controllers",
                    self.limits.maximum_controllers
                )));
            }
            self.pending_controllers.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow pending controllers: {error}"))
            })?;
            self.pending_controllers.push(PendingController {
                key,
                object_index: target.object_index,
                class_id: target.class_id,
            });
        }
        Ok(Some(key))
    }

    fn register_clip(
        &mut self,
        source_file_index: usize,
        reference: ObjectReference,
    ) -> Result<Option<SceneObjectKey>> {
        let Some(target) = try_resolve_target(self.collection, source_file_index, reference) else {
            return Ok(None);
        };
        if target.class_id != ANIMATION_CLIP_CLASS_ID {
            return Ok(None);
        }
        let key = SceneObjectKey {
            file_index: target.file_index,
            path_id: target.path_id,
        };
        if self.clip_index.contains_key(&key) {
            return Ok(Some(key));
        }
        if self.clips.len() >= self.limits.maximum_clips {
            return Err(Error::invalid_data(format!(
                "animation graph exceeds {} AnimationClips",
                self.limits.maximum_clips
            )));
        }
        let loaded = &self.collection.serialized_files[target.file_index];
        let name = if let Some(name) = self
            .collection
            .object_metadata(target.file_index, target.path_id)
            .and_then(|metadata| metadata.name.clone())
        {
            name
        } else {
            read_object_name_metadata(&loaded.file, target.object_index, self.limits.object_name)?
                .and_then(|metadata| metadata.name)
                .ok_or_else(|| {
                    Error::invalid_data(format!(
                        "AnimationClip {key:?} has no readable NamedObject prefix"
                    ))
                })?
        };
        self.charge_name(name.len(), "AnimationClip name")?;
        let index = self.clips.len();
        self.clip_index.insert(key, index);
        self.clips.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow AnimationClip nodes: {error}"))
        })?;
        self.clips.push(AnimationClipNode { object: key, name });
        Ok(Some(key))
    }

    fn resolve_typed_key(
        &self,
        source_file_index: usize,
        reference: ObjectReference,
        expected_classes: &[i32],
    ) -> Option<SceneObjectKey> {
        let target = try_resolve_target(self.collection, source_file_index, reference)?;
        expected_classes
            .contains(&target.class_id)
            .then_some(SceneObjectKey {
                file_index: target.file_index,
                path_id: target.path_id,
            })
    }

    fn charge_edges(&mut self, additional: usize, field: &str) -> Result<()> {
        self.edges = self
            .edges
            .checked_add(additional)
            .ok_or_else(|| Error::invalid_data("animation graph edge count overflowed"))?;
        if self.edges > self.limits.maximum_edges {
            return Err(Error::invalid_data(format!(
                "{field} raise the graph to {} edges, exceeding limit {}",
                self.edges, self.limits.maximum_edges
            )));
        }
        Ok(())
    }

    fn charge_name(&mut self, additional: usize, field: &str) -> Result<()> {
        self.name_bytes = self
            .name_bytes
            .checked_add(additional)
            .ok_or_else(|| Error::invalid_data("animation graph name bytes overflowed"))?;
        if self.name_bytes > self.limits.maximum_total_name_bytes {
            return Err(Error::invalid_data(format!(
                "{field} raise graph names to {} bytes, exceeding limit {}",
                self.name_bytes, self.limits.maximum_total_name_bytes
            )));
        }
        Ok(())
    }

    fn finish(self) -> AnimationGraph {
        AnimationGraph {
            animators: self.animators,
            legacy_animations: self.legacy_animations,
            controllers: self.controllers,
            clips: self.clips,
            controller_index: self.controller_index,
            clip_index: self.clip_index,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ResolvedTarget {
    file_index: usize,
    object_index: usize,
    path_id: i64,
    class_id: i32,
}

fn try_resolve_target(
    collection: &AssetCollection,
    source_file_index: usize,
    reference: ObjectReference,
) -> Option<ResolvedTarget> {
    resolve_object_reference(collection, source_file_index, reference)
        .ok()
        .flatten()
        .map(|target| ResolvedTarget {
            file_index: target.file_index,
            object_index: target.object_index,
            path_id: target.object.path_id,
            class_id: target.object.class_id,
        })
}

fn reserve_vec<T>(length: usize, field: &str) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(length).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {length} {field}: {error}"))
    })?;
    Ok(values)
}

#[cfg(test)]
mod tests {
    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::scene::{ANIMATOR_CLASS_ID, GAME_OBJECT_CLASS_ID};
    use crate::scene_hierarchy::{SceneHierarchyLimits, build_scene_hierarchy};
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{
        ANIMATION_CLASS_ID, ANIMATION_CLIP_CLASS_ID, ANIMATOR_CONTROLLER_CLASS_ID,
        ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID, AnimationControllerKind, AnimationGraphLimits,
        build_animation_graph,
    };

    #[test]
    fn assembles_animator_override_base_clips_and_legacy_animation_stably() {
        let collection = fixture_collection(false, false);
        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();

        let graph = build_animation_graph(&collection, &hierarchy, AnimationGraphLimits::default())
            .unwrap();

        assert_eq!(graph.animators.len(), 1);
        assert_eq!(graph.animators[0].game_object.path_id, 1);
        assert_eq!(graph.animators[0].animator.path_id, 2);
        assert_eq!(graph.animators[0].controller.unwrap().path_id, 5);
        assert_eq!(
            graph.animators[0]
                .bound_clips
                .iter()
                .map(|clip| clip.map(|key| key.path_id))
                .collect::<Vec<_>>(),
            [Some(4)]
        );

        assert_eq!(graph.controllers.len(), 2);
        assert_eq!(graph.controllers[0].object.path_id, 5);
        assert_eq!(graph.controllers[0].name, "Override");
        let AnimationControllerKind::AnimatorOverrideController {
            base_controller,
            overrides,
        } = &graph.controllers[0].kind
        else {
            panic!("first controller should preserve the override node");
        };
        assert_eq!(base_controller.unwrap().path_id, 3);
        assert_eq!(overrides[0].original.unwrap().path_id, 4);
        assert_eq!(overrides[0].replacement.unwrap().path_id, 7);
        assert_eq!(graph.controllers[1].name, "Base");

        assert_eq!(graph.clips.len(), 2);
        assert_eq!(graph.clips[0].name, "Walk");
        assert_eq!(graph.clips[1].name, "Run");
        assert_eq!(graph.clip(graph.clips[1].object).unwrap().name, "Run");
        assert_eq!(
            graph.controller(graph.controllers[1].object).unwrap().name,
            "Base"
        );

        assert_eq!(graph.legacy_animations.len(), 1);
        let legacy = &graph.legacy_animations[0];
        assert_eq!(legacy.component.path_id, 6);
        assert_eq!(legacy.game_object.unwrap().path_id, 1);
        assert_eq!(legacy.default_clip.unwrap().path_id, 4);
        assert_eq!(
            legacy
                .clips
                .iter()
                .map(|clip| clip.map(|key| key.path_id))
                .collect::<Vec<_>>(),
            [Some(4), Some(7)]
        );
    }

    #[test]
    fn applies_try_get_semantics_but_rejects_resolved_corruption_and_budgets() {
        let missing = fixture_collection(true, false);
        let hierarchy = build_scene_hierarchy(&missing, SceneHierarchyLimits::default()).unwrap();
        let graph =
            build_animation_graph(&missing, &hierarchy, AnimationGraphLimits::default()).unwrap();
        assert_eq!(graph.animators[0].controller, None);
        assert!(graph.animators[0].bound_clips.is_empty());

        let collection = fixture_collection(false, false);
        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();
        for limits in [
            AnimationGraphLimits {
                maximum_edges: 1,
                ..AnimationGraphLimits::default()
            },
            AnimationGraphLimits {
                maximum_clips: 1,
                ..AnimationGraphLimits::default()
            },
            AnimationGraphLimits {
                maximum_total_name_bytes: 3,
                ..AnimationGraphLimits::default()
            },
        ] {
            assert!(build_animation_graph(&collection, &hierarchy, limits).is_err());
        }

        let corrupt = fixture_collection(false, true);
        let hierarchy = build_scene_hierarchy(&corrupt, SceneHierarchyLimits::default()).unwrap();
        assert!(
            build_animation_graph(&corrupt, &hierarchy, AnimationGraphLimits::default()).is_err()
        );
    }

    fn fixture_collection(missing_controller: bool, corrupt_controller: bool) -> AssetCollection {
        let controller_path = if missing_controller { 999 } else { 5 };
        let mut objects = vec![
            (GAME_OBJECT_CLASS_ID, 1, game_object()),
            (ANIMATOR_CLASS_ID, 2, animator(controller_path)),
            (
                ANIMATOR_CONTROLLER_CLASS_ID,
                3,
                if corrupt_controller {
                    aligned_string("Broken")
                } else {
                    animator_controller("Base", 4)
                },
            ),
            (ANIMATION_CLIP_CLASS_ID, 4, aligned_string("Walk")),
            (
                ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID,
                5,
                override_controller(),
            ),
            (ANIMATION_CLASS_ID, 6, legacy_animation()),
            (ANIMATION_CLIP_CLASS_ID, 7, aligned_string("Run")),
        ];
        objects.sort_unstable_by_key(|object| object.1);
        let file = SerializedFile::open(Region::from_bytes(synthetic_v22(&objects))).unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "animation.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    fn game_object() -> Vec<u8> {
        let mut output = Vec::new();
        push_i32(&mut output, 2);
        push_pptr(&mut output, 0, 2);
        push_pptr(&mut output, 0, 6);
        push_i32(&mut output, 0);
        push_aligned_string(&mut output, "Hero");
        output
    }

    fn animator(controller: i64) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, 0, 1);
        output.push(1);
        align(&mut output, 4);
        push_pptr(&mut output, 0, 0);
        push_pptr(&mut output, 0, controller);
        output
    }

    fn animator_controller(name: &str, clip: i64) -> Vec<u8> {
        let mut output = aligned_string(name);
        output.extend_from_slice(&0_u32.to_le_bytes());
        for _ in 0..9 {
            push_i32(&mut output, 0);
        }
        push_i32(&mut output, 0);
        push_i32(&mut output, 1);
        push_pptr(&mut output, 0, clip);
        // The state-machine-behaviour tail every real controller carries.
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        output.push(1);
        while output.len() % 4 != 0 {
            output.push(0);
        }
        output
    }

    fn override_controller() -> Vec<u8> {
        let mut output = aligned_string("Override");
        push_pptr(&mut output, 0, 3);
        push_i32(&mut output, 1);
        push_pptr(&mut output, 0, 4);
        push_pptr(&mut output, 0, 7);
        output
    }

    fn legacy_animation() -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, 0, 1);
        output.push(1);
        align(&mut output, 4);
        push_pptr(&mut output, 0, 4);
        push_i32(&mut output, 2);
        push_pptr(&mut output, 0, 4);
        push_pptr(&mut output, 0, 7);
        output
    }

    fn aligned_string(value: &str) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, value);
        output
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        push_i32(output, file_id);
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn synthetic_v22(objects: &[(i32, i64, Vec<u8>)]) -> Vec<u8> {
        let mut classes: Vec<i32> = objects.iter().map(|object| object.0).collect();
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
            metadata.extend_from_slice(&[0_u8; 16]);
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

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (48 + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + data.len();
        let mut output = vec![0_u8; 48];
        output[8..12].copy_from_slice(&22_u32.to_be_bytes());
        output[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        output.extend_from_slice(&metadata);
        output.resize(data_offset, 0);
        output.extend_from_slice(&data);
        output
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
