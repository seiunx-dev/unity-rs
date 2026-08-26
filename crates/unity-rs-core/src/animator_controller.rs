//! Bounded parsing for Unity and Tuanjie `AnimatorController` objects.
//!
//! The verified layout covers Unity 2017.3 through 2023.x, Unity 6000.0-6000.3,
//! and Tuanjie 2022.3.x. Large numeric arrays stay source-bound and are
//! materialized only through the bounded array helpers shared with
//! `AnimationClip`.

use std::mem::size_of;

use crate::animation_clip::{ByteRegion, F32Array, I32Array, U32Array};
use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::{ObjectReference, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::{Error, Result};

pub const ANIMATOR_CONTROLLER_CLASS_ID: i32 = 91;

const NO_TARGET_PLATFORM: i32 = -2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimatorControllerReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_array_elements: usize,
    pub maximum_total_array_elements: usize,
    pub maximum_reference_bytes: u64,
    pub maximum_total_allocation_bytes: u64,
}

impl Default for AnimatorControllerReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 512 * 1024 * 1024,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 64 * 1024 * 1024,
            maximum_array_elements: 2_000_000,
            maximum_total_array_elements: 10_000_000,
            maximum_reference_bytes: 256 * 1024 * 1024,
            maximum_total_allocation_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloatVectorArray {
    pub values: F32Array,
    pub vector_count: usize,
    pub dimensions: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoolArray {
    pub data: ByteRegion,
    pub count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HumanPoseMask {
    pub words: [u32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkeletonMaskElement {
    pub path_hash: u32,
    pub weight: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayerConstant {
    pub state_machine_index: u32,
    pub state_machine_motion_set_index: u32,
    pub body_mask: HumanPoseMask,
    pub skeleton_mask: Vec<SkeletonMaskElement>,
    pub binding: u32,
    pub layer_blending_mode: i32,
    pub default_weight: f32,
    pub ik_pass: bool,
    pub synced_layer_affects_timing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConditionConstant {
    pub condition_mode: u32,
    pub event_id: u32,
    pub event_threshold: f32,
    pub exit_time: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransitionConstant {
    pub conditions: Vec<ConditionConstant>,
    pub destination_state: u32,
    pub full_path_id: u32,
    pub id: u32,
    pub user_id: u32,
    pub transition_duration: f32,
    pub transition_offset: f32,
    pub exit_time: f32,
    pub interruption_source: i32,
    pub flags: TransitionFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionFlags(u8);

impl TransitionFlags {
    const HAS_EXIT_TIME: u8 = 1;
    const HAS_FIXED_DURATION: u8 = 1 << 1;
    const ORDERED_INTERRUPTION: u8 = 1 << 2;
    const CAN_TRANSITION_TO_SELF: u8 = 1 << 3;

    const fn from_bools(values: [bool; 4]) -> Self {
        let [
            has_exit_time,
            has_fixed_duration,
            ordered_interruption,
            can_transition_to_self,
        ] = values;
        Self(
            ((has_exit_time as u8) * Self::HAS_EXIT_TIME)
                | ((has_fixed_duration as u8) * Self::HAS_FIXED_DURATION)
                | ((ordered_interruption as u8) * Self::ORDERED_INTERRUPTION)
                | ((can_transition_to_self as u8) * Self::CAN_TRANSITION_TO_SELF),
        )
    }

    #[must_use]
    pub const fn has_exit_time(self) -> bool {
        self.0 & Self::HAS_EXIT_TIME != 0
    }

    #[must_use]
    pub const fn has_fixed_duration(self) -> bool {
        self.0 & Self::HAS_FIXED_DURATION != 0
    }

    #[must_use]
    pub const fn ordered_interruption(self) -> bool {
        self.0 & Self::ORDERED_INTERRUPTION != 0
    }

    #[must_use]
    pub const fn can_transition_to_self(self) -> bool {
        self.0 & Self::CAN_TRANSITION_TO_SELF != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionNeighborList {
    pub neighbors: U32Array,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blend2dDataConstant {
    pub child_positions: FloatVectorArray,
    pub child_magnitudes: F32Array,
    pub child_pair_vectors: FloatVectorArray,
    pub child_pair_average_magnitude_inverses: F32Array,
    pub child_neighbor_lists: Vec<MotionNeighborList>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlendDirectDataConstant {
    pub child_blend_event_ids: U32Array,
    pub normalized_blend_values: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlendTreeNodeConstant {
    pub blend_type: u32,
    pub blend_event_id: u32,
    pub blend_event_y_id: u32,
    pub child_indices: U32Array,
    pub blend_1d_thresholds: F32Array,
    pub blend_2d: Blend2dDataConstant,
    pub blend_direct: BlendDirectDataConstant,
    pub clip_id: u32,
    pub duration: f32,
    pub cycle_offset: f32,
    pub mirror: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlendTreeConstant {
    pub nodes: Vec<BlendTreeNodeConstant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateConstant {
    pub transitions: Vec<TransitionConstant>,
    pub blend_tree_indices: I32Array,
    pub blend_trees: Vec<BlendTreeConstant>,
    pub name_id: u32,
    pub path_id: u32,
    pub full_path_id: u32,
    pub tag_id: u32,
    pub speed_parameter_id: u32,
    pub mirror_parameter_id: u32,
    pub cycle_offset_parameter_id: u32,
    pub time_parameter_id: u32,
    pub speed: f32,
    pub cycle_offset: f32,
    pub flags: StateFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateFlags(u8);

impl StateFlags {
    const IK_ON_FEET: u8 = 1;
    const WRITE_DEFAULT_VALUES: u8 = 1 << 1;
    const LOOPED: u8 = 1 << 2;
    const MIRROR: u8 = 1 << 3;

    const fn from_bools(values: [bool; 4]) -> Self {
        let [ik_on_feet, write_default_values, looped, mirror] = values;
        Self(
            ((ik_on_feet as u8) * Self::IK_ON_FEET)
                | ((write_default_values as u8) * Self::WRITE_DEFAULT_VALUES)
                | ((looped as u8) * Self::LOOPED)
                | ((mirror as u8) * Self::MIRROR),
        )
    }

    #[must_use]
    pub const fn ik_on_feet(self) -> bool {
        self.0 & Self::IK_ON_FEET != 0
    }

    #[must_use]
    pub const fn write_default_values(self) -> bool {
        self.0 & Self::WRITE_DEFAULT_VALUES != 0
    }

    #[must_use]
    pub const fn looped(self) -> bool {
        self.0 & Self::LOOPED != 0
    }

    #[must_use]
    pub const fn mirror(self) -> bool {
        self.0 & Self::MIRROR != 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectorTransitionConstant {
    pub destination: u32,
    pub conditions: Vec<ConditionConstant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectorStateConstant {
    pub transitions: Vec<SelectorTransitionConstant>,
    pub full_path_id: u32,
    pub is_entry: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateMachineConstant {
    pub states: Vec<StateConstant>,
    pub any_state_transitions: Vec<TransitionConstant>,
    pub selector_states: Vec<SelectorStateConstant>,
    pub default_state: u32,
    pub motion_set_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueConstant {
    pub id: u32,
    pub value_type: u32,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueArrayConstant {
    pub values: Vec<ValueConstant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultValueArray {
    pub positions: FloatVectorArray,
    pub quaternions: FloatVectorArray,
    pub scales: FloatVectorArray,
    pub floats: F32Array,
    pub integers: I32Array,
    pub booleans: BoolArray,
    /// Added to the standard engine layout in Unity 6000.2.
    pub entity_ids: Option<I32Array>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ControllerConstant {
    pub layers: Vec<LayerConstant>,
    pub state_machines: Vec<StateMachineConstant>,
    pub values: ValueArrayConstant,
    pub default_values: DefaultValueArray,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TosEntry {
    pub key: u32,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnimatorController {
    pub path_id: i64,
    pub name: String,
    pub controller_size: u32,
    pub controller: ControllerConstant,
    pub tos: Vec<TosEntry>,
    pub animation_clips: Vec<ObjectReference>,
}

pub fn read_animator_controller(
    file: &SerializedFile,
    object_index: usize,
    limits: AnimatorControllerReadLimits,
) -> Result<AnimatorController> {
    validate_supported_version(file)?;
    let mut reader = ControllerReader::new(file, object_index, limits)?;
    let name = reader.read_named_object()?;
    let controller_size = reader.reader.read_u32()?;
    let controller = reader.read_controller_constant()?;
    let tos = reader.read_tos()?;
    let animation_clips = reader.read_animation_clips()?;
    reader.read_state_machine_behaviour_tail()?;

    // Now this means something: everything the object carries has been
    // accounted for, either read or checked and found empty.
    let trailing = reader.reader.remaining()?;
    if trailing != 0 {
        return Err(Error::invalid_data(format!(
            "AnimatorController object contains {trailing} unparsed trailing bytes"
        )));
    }
    Ok(AnimatorController {
        path_id: reader.path_id,
        name,
        controller_size,
        controller,
        tos,
        animation_clips,
    })
}

#[derive(Debug, Default)]
struct ParseBudget {
    strings: usize,
    elements: usize,
    references: u64,
    allocations: u64,
}

struct ControllerReader {
    region: Region,
    reader: EndianReader<RegionCursor>,
    endian: Endian,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    version: (u32, u32, u32),
    limits: AnimatorControllerReadLimits,
    budget: ParseBudget,
}

impl ControllerReader {
    fn new(
        file: &SerializedFile,
        object_index: usize,
        limits: AnimatorControllerReadLimits,
    ) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != ANIMATOR_CONTROLLER_CLASS_ID {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, expected AnimatorController ({ANIMATOR_CONTROLLER_CLASS_ID})",
                object.path_id, object.class_id
            )));
        }
        if object.byte_size > limits.maximum_object_bytes {
            return Err(Error::invalid_data(format!(
                "AnimatorController object is {} bytes, exceeding limit {}",
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
            endian,
            absolute_start: object.byte_start,
            path_id: object.path_id,
            target_platform: file.target_platform,
            format_version: file.header.version.0,
            version: file.unity_version.components(),
            limits,
            budget: ParseBudget::default(),
        })
    }

    fn read_named_object(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            let _hide_flags = self.reader.read_u32()?;
            let _prefab_parent = self.read_pptr("prefab parent object")?;
            let _prefab_internal = self.read_pptr("prefab internal")?;
        }
        self.read_aligned_string("AnimatorController name")
    }

    fn read_controller_constant(&mut self) -> Result<ControllerConstant> {
        let layer_count = self.read_count("AnimatorController layer", 32)?;
        let mut layers = self.reserve::<LayerConstant>(layer_count, "AnimatorController layers")?;
        for _ in 0..layer_count {
            layers.push(self.read_layer()?);
        }
        let machine_count = self.read_count("AnimatorController state machine", 16)?;
        let mut state_machines = self
            .reserve::<StateMachineConstant>(machine_count, "AnimatorController state machines")?;
        for _ in 0..machine_count {
            state_machines.push(self.read_state_machine()?);
        }
        Ok(ControllerConstant {
            layers,
            state_machines,
            values: self.read_value_array_constant()?,
            default_values: self.read_default_value_array()?,
        })
    }

    fn read_layer(&mut self) -> Result<LayerConstant> {
        let state_machine_index = self.reader.read_u32()?;
        let state_machine_motion_set_index = self.reader.read_u32()?;
        let body_mask = HumanPoseMask {
            words: [
                self.reader.read_u32()?,
                self.reader.read_u32()?,
                self.reader.read_u32()?,
            ],
        };
        let skeleton_count = self.read_count("AnimatorController skeleton mask", 8)?;
        let mut skeleton_mask = self
            .reserve::<SkeletonMaskElement>(skeleton_count, "AnimatorController skeleton mask")?;
        for _ in 0..skeleton_count {
            skeleton_mask.push(SkeletonMaskElement {
                path_hash: self.reader.read_u32()?,
                weight: self.reader.read_f32()?,
            });
        }
        let result = LayerConstant {
            state_machine_index,
            state_machine_motion_set_index,
            body_mask,
            skeleton_mask,
            binding: self.reader.read_u32()?,
            layer_blending_mode: self.reader.read_i32()?,
            default_weight: self.reader.read_f32()?,
            ik_pass: self.reader.read_bool()?,
            synced_layer_affects_timing: self.reader.read_bool()?,
        };
        self.align(4)?;
        Ok(result)
    }

    fn read_condition(&mut self) -> Result<ConditionConstant> {
        Ok(ConditionConstant {
            condition_mode: self.reader.read_u32()?,
            event_id: self.reader.read_u32()?,
            event_threshold: self.reader.read_f32()?,
            exit_time: self.reader.read_f32()?,
        })
    }

    fn read_transition(&mut self) -> Result<TransitionConstant> {
        let count = self.read_count("AnimatorController transition condition", 16)?;
        let mut conditions =
            self.reserve::<ConditionConstant>(count, "AnimatorController transition conditions")?;
        for _ in 0..count {
            conditions.push(self.read_condition()?);
        }
        let destination_state = self.reader.read_u32()?;
        let full_path_id = self.reader.read_u32()?;
        let id = self.reader.read_u32()?;
        let user_id = self.reader.read_u32()?;
        let transition_duration = self.reader.read_f32()?;
        let transition_offset = self.reader.read_f32()?;
        let exit_time = self.reader.read_f32()?;
        let has_exit_time = self.reader.read_bool()?;
        let has_fixed_duration = self.reader.read_bool()?;
        self.align(4)?;
        let interruption_source = self.reader.read_i32()?;
        let ordered_interruption = self.reader.read_bool()?;
        let can_transition_to_self = self.reader.read_bool()?;
        self.align(4)?;
        Ok(TransitionConstant {
            conditions,
            destination_state,
            full_path_id,
            id,
            user_id,
            transition_duration,
            transition_offset,
            exit_time,
            interruption_source,
            flags: TransitionFlags::from_bools([
                has_exit_time,
                has_fixed_duration,
                ordered_interruption,
                can_transition_to_self,
            ]),
        })
    }

    fn read_blend_tree_node(&mut self) -> Result<BlendTreeNodeConstant> {
        let blend_type = self.reader.read_u32()?;
        let blend_event_id = self.reader.read_u32()?;
        let blend_event_y_id = self.reader.read_u32()?;
        let child_indices = self.read_u32_array("AnimatorController blend child indices")?;
        let blend_1d_thresholds = self.read_f32_array("AnimatorController 1D thresholds")?;
        let child_positions = self.read_vector_array("AnimatorController 2D child positions", 2)?;
        let child_magnitudes = self.read_f32_array("AnimatorController child magnitudes")?;
        let child_pair_vectors =
            self.read_vector_array("AnimatorController child pair vectors", 2)?;
        let child_pair_average_magnitude_inverses =
            self.read_f32_array("AnimatorController child pair inverse magnitudes")?;
        let neighbor_count = self.read_count("AnimatorController neighbor list", 4)?;
        let mut child_neighbor_lists = self
            .reserve::<MotionNeighborList>(neighbor_count, "AnimatorController neighbor lists")?;
        for _ in 0..neighbor_count {
            child_neighbor_lists.push(MotionNeighborList {
                neighbors: self.read_u32_array("AnimatorController neighbors")?,
            });
        }
        let blend_2d = Blend2dDataConstant {
            child_positions,
            child_magnitudes,
            child_pair_vectors,
            child_pair_average_magnitude_inverses,
            child_neighbor_lists,
        };
        let child_blend_event_ids =
            self.read_u32_array("AnimatorController direct blend event IDs")?;
        let normalized_blend_values = self.reader.read_bool()?;
        self.align(4)?;
        let blend_direct = BlendDirectDataConstant {
            child_blend_event_ids,
            normalized_blend_values,
        };
        let clip_id = self.reader.read_u32()?;
        let duration = self.reader.read_f32()?;
        let cycle_offset = self.reader.read_f32()?;
        let mirror = self.reader.read_bool()?;
        self.align(4)?;
        Ok(BlendTreeNodeConstant {
            blend_type,
            blend_event_id,
            blend_event_y_id,
            child_indices,
            blend_1d_thresholds,
            blend_2d,
            blend_direct,
            clip_id,
            duration,
            cycle_offset,
            mirror,
        })
    }

    fn read_blend_tree(&mut self) -> Result<BlendTreeConstant> {
        let count = self.read_count("AnimatorController blend tree node", 12)?;
        let mut nodes =
            self.reserve::<BlendTreeNodeConstant>(count, "AnimatorController blend tree nodes")?;
        for _ in 0..count {
            nodes.push(self.read_blend_tree_node()?);
        }
        Ok(BlendTreeConstant { nodes })
    }

    fn read_state(&mut self) -> Result<StateConstant> {
        let transition_count = self.read_count("AnimatorController state transition", 4)?;
        let mut transitions = self.reserve::<TransitionConstant>(
            transition_count,
            "AnimatorController state transitions",
        )?;
        for _ in 0..transition_count {
            transitions.push(self.read_transition()?);
        }
        let blend_tree_indices =
            self.read_i32_array("AnimatorController blend tree constant indices")?;
        let blend_count = self.read_count("AnimatorController blend tree", 4)?;
        let mut blend_trees =
            self.reserve::<BlendTreeConstant>(blend_count, "AnimatorController blend trees")?;
        for _ in 0..blend_count {
            blend_trees.push(self.read_blend_tree()?);
        }
        let name_id = self.reader.read_u32()?;
        let path_id = self.reader.read_u32()?;
        let full_path_id = self.reader.read_u32()?;
        let tag_id = self.reader.read_u32()?;
        let speed_parameter_id = self.reader.read_u32()?;
        let mirror_parameter_id = self.reader.read_u32()?;
        let cycle_offset_parameter_id = self.reader.read_u32()?;
        let time_parameter_id = self.reader.read_u32()?;
        let speed = self.reader.read_f32()?;
        let cycle_offset = self.reader.read_f32()?;
        let ik_on_feet = self.reader.read_bool()?;
        let write_default_values = self.reader.read_bool()?;
        let looped = self.reader.read_bool()?;
        let mirror = self.reader.read_bool()?;
        self.align(4)?;
        Ok(StateConstant {
            transitions,
            blend_tree_indices,
            blend_trees,
            name_id,
            path_id,
            full_path_id,
            tag_id,
            speed_parameter_id,
            mirror_parameter_id,
            cycle_offset_parameter_id,
            time_parameter_id,
            speed,
            cycle_offset,
            flags: StateFlags::from_bools([ik_on_feet, write_default_values, looped, mirror]),
        })
    }

    fn read_selector_transition(&mut self) -> Result<SelectorTransitionConstant> {
        let destination = self.reader.read_u32()?;
        let count = self.read_count("AnimatorController selector condition", 16)?;
        let mut conditions =
            self.reserve::<ConditionConstant>(count, "AnimatorController selector conditions")?;
        for _ in 0..count {
            conditions.push(self.read_condition()?);
        }
        Ok(SelectorTransitionConstant {
            destination,
            conditions,
        })
    }

    fn read_selector_state(&mut self) -> Result<SelectorStateConstant> {
        let count = self.read_count("AnimatorController selector transition", 8)?;
        let mut transitions = self.reserve::<SelectorTransitionConstant>(
            count,
            "AnimatorController selector transitions",
        )?;
        for _ in 0..count {
            transitions.push(self.read_selector_transition()?);
        }
        let full_path_id = self.reader.read_u32()?;
        let is_entry = self.reader.read_bool()?;
        self.align(4)?;
        Ok(SelectorStateConstant {
            transitions,
            full_path_id,
            is_entry,
        })
    }

    fn read_state_machine(&mut self) -> Result<StateMachineConstant> {
        let state_count = self.read_count("AnimatorController state", 4)?;
        let mut states = self.reserve::<StateConstant>(state_count, "AnimatorController states")?;
        for _ in 0..state_count {
            states.push(self.read_state()?);
        }
        let any_count = self.read_count("AnimatorController any-state transition", 4)?;
        let mut any_state_transitions = self
            .reserve::<TransitionConstant>(any_count, "AnimatorController any-state transitions")?;
        for _ in 0..any_count {
            any_state_transitions.push(self.read_transition()?);
        }
        let selector_count = self.read_count("AnimatorController selector state", 8)?;
        let mut selector_states = self.reserve::<SelectorStateConstant>(
            selector_count,
            "AnimatorController selector states",
        )?;
        for _ in 0..selector_count {
            selector_states.push(self.read_selector_state()?);
        }
        Ok(StateMachineConstant {
            states,
            any_state_transitions,
            selector_states,
            default_state: self.reader.read_u32()?,
            motion_set_count: self.reader.read_u32()?,
        })
    }

    fn read_value_array_constant(&mut self) -> Result<ValueArrayConstant> {
        let count = self.read_count("AnimatorController value constant", 12)?;
        let mut values =
            self.reserve::<ValueConstant>(count, "AnimatorController value constants")?;
        for _ in 0..count {
            values.push(ValueConstant {
                id: self.reader.read_u32()?,
                value_type: self.reader.read_u32()?,
                index: self.reader.read_u32()?,
            });
        }
        Ok(ValueArrayConstant { values })
    }

    fn read_default_value_array(&mut self) -> Result<DefaultValueArray> {
        let positions = self.read_vector_array("AnimatorController default positions", 3)?;
        let quaternions = self.read_vector_array("AnimatorController default quaternions", 4)?;
        let scales = self.read_vector_array("AnimatorController default scales", 3)?;
        let floats = self.read_f32_array("AnimatorController default floats")?;
        let integers = self.read_i32_array("AnimatorController default integers")?;
        let booleans = self.read_bool_array("AnimatorController default booleans")?;
        self.align(4)?;
        let entity_ids = if self.version >= (6000, 2, 0) {
            Some(self.read_i32_array("AnimatorController default entity IDs")?)
        } else {
            None
        };
        Ok(DefaultValueArray {
            positions,
            quaternions,
            scales,
            floats,
            integers,
            booleans,
            entity_ids,
        })
    }

    fn read_tos(&mut self) -> Result<Vec<TosEntry>> {
        let count = self.read_count("AnimatorController TOS entry", 8)?;
        let mut values = self.reserve::<TosEntry>(count, "AnimatorController TOS entries")?;
        for _ in 0..count {
            values.push(TosEntry {
                key: self.reader.read_u32()?,
                value: self.read_aligned_string("AnimatorController TOS value")?,
            });
        }
        Ok(values)
    }

    fn read_animation_clips(&mut self) -> Result<Vec<ObjectReference>> {
        let pptr_size = self.pptr_size();
        let count = self.read_count("AnimatorController animation clip", pptr_size)?;
        self.check_reference_budget(count, "AnimatorController animation clips")?;
        let mut clips =
            self.reserve::<ObjectReference>(count, "AnimatorController animation clips")?;
        for _ in 0..count {
            clips.push(self.read_pptr("AnimatorController animation clip")?);
        }
        Ok(clips)
    }

    /// Reads the state-machine-behaviour tail that follows the animation clips.
    ///
    /// The managed reader stops at the animation clips and never looks at what
    /// follows, so this reader did too -- and then refused every real file,
    /// because all 212 `AnimatorController` objects in a Unity 2022.3 game
    /// carry exactly sixteen more bytes: two empty collections in
    /// `m_StateMachineBehaviourVectorDescription`, an empty
    /// `m_StateMachineBehaviours`, and `m_MultiThreadedStateMachine` with its
    /// padding.
    ///
    /// Reading the counts rather than skipping sixteen bytes is what keeps the
    /// trailing-byte check meaningful. Non-empty collections are reported as
    /// unsupported and named, because no file available here has one: the
    /// element layouts would be a guess, and a guess that parses is worse than
    /// a refusal that explains itself.
    fn read_state_machine_behaviour_tail(&mut self) -> Result<()> {
        for field in [
            "AnimatorController state machine behaviour ranges",
            "AnimatorController state machine behaviour indices",
            "AnimatorController state machine behaviours",
        ] {
            let count = self.reader.read_u32()?;
            if count != 0 {
                return Err(Error::unsupported(format!(
                    "{field} has {count} entries, a shape this reader does not model"
                )));
            }
        }
        self.reader.read_u8()?;
        self.align(4)?;
        Ok(())
    }

    fn read_aligned_string(&mut self, field: &str) -> Result<String> {
        let length = checked_length(self.reader.read_i32()?, field)?;
        if length > self.limits.maximum_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} is {length} bytes, exceeding limit {}",
                self.limits.maximum_string_bytes
            )));
        }
        self.budget.strings =
            self.budget.strings.checked_add(length).ok_or_else(|| {
                Error::invalid_data("AnimatorController string budget overflowed")
            })?;
        if self.budget.strings > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "AnimatorController strings total {} bytes, exceeding limit {}",
                self.budget.strings, self.limits.maximum_total_string_bytes
            )));
        }
        self.charge_allocation(length, field)?;
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn read_count(&mut self, field: &str, minimum_size: usize) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > self.limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {}",
                self.limits.maximum_array_elements
            )));
        }
        self.budget.elements = self
            .budget
            .elements
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("AnimatorController array budget overflowed"))?;
        if self.budget.elements > self.limits.maximum_total_array_elements {
            return Err(Error::invalid_data(format!(
                "AnimatorController arrays total {} elements while reading {field}, exceeding limit {}",
                self.budget.elements, self.limits.maximum_total_array_elements
            )));
        }
        let bytes = count
            .checked_mul(minimum_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        if u64::try_from(bytes).unwrap_or(u64::MAX) > self.reader.remaining()? {
            return Err(Error::invalid_data(format!(
                "{field} needs at least {bytes} bytes beyond the bounded object payload"
            )));
        }
        Ok(count)
    }

    fn read_f32_array(&mut self, field: &str) -> Result<F32Array> {
        let count = self.read_count(field, 4)?;
        let region = self.take_array_region(count, 4, field)?;
        Ok(F32Array {
            region,
            count,
            endian: self.endian,
        })
    }

    fn read_u32_array(&mut self, field: &str) -> Result<U32Array> {
        let count = self.read_count(field, 4)?;
        let region = self.take_array_region(count, 4, field)?;
        Ok(U32Array {
            region,
            count,
            endian: self.endian,
        })
    }

    fn read_i32_array(&mut self, field: &str) -> Result<I32Array> {
        let count = self.read_count(field, 4)?;
        let region = self.take_array_region(count, 4, field)?;
        Ok(I32Array {
            region,
            count,
            endian: self.endian,
        })
    }

    fn read_vector_array(&mut self, field: &str, dimensions: u8) -> Result<FloatVectorArray> {
        let count = self.read_count(field, usize::from(dimensions) * 4)?;
        let scalar_count = count
            .checked_mul(usize::from(dimensions))
            .ok_or_else(|| Error::invalid_data(format!("{field} scalar count overflowed")))?;
        let region = self.take_array_region(scalar_count, 4, field)?;
        Ok(FloatVectorArray {
            values: F32Array {
                region,
                count: scalar_count,
                endian: self.endian,
            },
            vector_count: count,
            dimensions,
        })
    }

    fn read_bool_array(&mut self, field: &str) -> Result<BoolArray> {
        let count = self.read_count(field, 1)?;
        let length = u64::try_from(count)
            .map_err(|_| Error::invalid_data(format!("{field} length does not fit in u64")))?;
        let region = self.take_region(length, field)?;
        Ok(BoolArray {
            data: ByteRegion {
                region,
                byte_length: length,
            },
            count,
        })
    }

    fn take_array_region(&mut self, count: usize, width: usize, field: &str) -> Result<Region> {
        let length = count
            .checked_mul(width)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| Error::invalid_data(format!("{field} byte length overflowed")))?;
        self.take_region(length, field)
    }

    fn take_region(&mut self, length: u64, field: &str) -> Result<Region> {
        let position = self.reader.position()?;
        let result = self.region.subregion(position, length).map_err(|error| {
            Error::invalid_data(format!(
                "{field} exceeds the bounded AnimatorController payload: {error}"
            ))
        })?;
        self.skip(length, field)?;
        Ok(result)
    }

    fn read_pptr(&mut self, field: &str) -> Result<ObjectReference> {
        let bytes = u64::try_from(self.pptr_size())
            .map_err(|_| Error::invalid_data("AnimatorController PPtr size overflowed"))?;
        self.budget.references =
            self.budget.references.checked_add(bytes).ok_or_else(|| {
                Error::invalid_data("AnimatorController reference budget overflowed")
            })?;
        if self.budget.references > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "AnimatorController references total {} bytes while reading {field}, exceeding limit {}",
                self.budget.references, self.limits.maximum_reference_bytes
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

    fn check_reference_budget(&self, count: usize, field: &str) -> Result<()> {
        let bytes = count
            .checked_mul(self.pptr_size())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| Error::invalid_data(format!("{field} size overflowed")))?;
        let total =
            self.budget.references.checked_add(bytes).ok_or_else(|| {
                Error::invalid_data("AnimatorController reference budget overflowed")
            })?;
        if total > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "AnimatorController references would total {total} bytes while reading {field}, exceeding limit {}",
                self.limits.maximum_reference_bytes
            )));
        }
        Ok(())
    }

    fn reserve<T>(&mut self, count: usize, field: &str) -> Result<Vec<T>> {
        let bytes = count
            .checked_mul(size_of::<T>())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| Error::invalid_data(format!("{field} allocation size overflowed")))?;
        self.charge_allocation_u64(bytes, field)?;
        let mut values = Vec::new();
        values.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!("cannot allocate {count} {field} entries: {error}"))
        })?;
        Ok(values)
    }

    fn charge_allocation(&mut self, bytes: usize, field: &str) -> Result<()> {
        let bytes = u64::try_from(bytes)
            .map_err(|_| Error::invalid_data(format!("{field} allocation does not fit in u64")))?;
        self.charge_allocation_u64(bytes, field)
    }

    fn charge_allocation_u64(&mut self, bytes: u64, field: &str) -> Result<()> {
        self.budget.allocations = self.budget.allocations.checked_add(bytes).ok_or_else(|| {
            Error::invalid_data("AnimatorController allocation budget overflowed")
        })?;
        if self.budget.allocations > self.limits.maximum_total_allocation_bytes {
            return Err(Error::invalid_data(format!(
                "AnimatorController allocations total {} bytes while reading {field}, exceeding limit {}",
                self.budget.allocations, self.limits.maximum_total_allocation_bytes
            )));
        }
        Ok(())
    }

    const fn pptr_size(&self) -> usize {
        if self.format_version < 14 { 8 } else { 12 }
    }

    fn align(&mut self, alignment: u64) -> Result<()> {
        let absolute = self
            .absolute_start
            .checked_add(self.reader.position()?)
            .ok_or_else(|| Error::invalid_data("AnimatorController alignment overflowed"))?;
        let remainder = absolute % alignment;
        if remainder != 0 {
            self.skip(alignment - remainder, "AnimatorController alignment")?;
        }
        Ok(())
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

fn validate_supported_version(file: &SerializedFile) -> Result<()> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "AnimatorController requires a Unity version for its nested layout",
        ));
    }
    let version = file.unity_version.components();
    let supported = if file.unity_version.is_tuanjie() {
        version.0 == 2022 && version.1 == 3 && version.2 >= 2
    } else {
        // 6000.3 checked rather than assumed: the managed reader adds nothing
        // to AnimatorController after 6000.2's default entity IDs, and the
        // 6000.3.12f1 differential fixture locks that shared layout.
        ((2017, 3, 0)..(2024, 0, 0)).contains(&version)
            || (file.unity_version.major == 6000 && file.unity_version.minor <= 3)
    };
    if !supported {
        return Err(Error::unsupported(format!(
            "AnimatorController Unity version {} is outside the verified 2017.3-2023.x, 6000.0-6000.3, and Tuanjie 2022.3.x ranges",
            file.unity_version
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{
        ANIMATOR_CONTROLLER_CLASS_ID, AnimatorControllerReadLimits, read_animator_controller,
    };

    #[test]
    fn reads_deep_unity_2017_controller() {
        let endian = TestEndian::Little;
        let object = deep_controller_object(endian, false);
        let file = parse_asset(17, endian, 13, "2017.3.0f1", &object);

        let value =
            read_animator_controller(&file, 0, AnimatorControllerReadLimits::default()).unwrap();

        assert_eq!(value.path_id, 7);
        assert_eq!(value.name, "deep");
        assert_eq!(value.controller_size, 0x1234);
        assert_eq!(value.controller.layers.len(), 1);
        assert_eq!(value.controller.layers[0].body_mask.words, [1, 2, 3]);
        assert_eq!(value.controller.layers[0].skeleton_mask[0].path_hash, 44);
        let machine = &value.controller.state_machines[0];
        let state = &machine.states[0];
        assert_eq!(state.transitions[0].conditions[0].event_id, 102);
        assert_eq!(state.time_parameter_id, 808);
        let node = &state.blend_trees[0].nodes[0];
        assert_eq!(node.blend_type, 11);
        assert_eq!(node.child_indices.read_values(4).unwrap(), [4]);
        assert_eq!(node.blend_1d_thresholds.read_values(4).unwrap(), [0.25]);
        assert_eq!(node.blend_2d.child_positions.vector_count, 1);
        assert_eq!(
            node.blend_2d.child_positions.values.read_values(4).unwrap(),
            [1.0, 2.0]
        );
        assert!(node.blend_direct.normalized_blend_values);
        assert_eq!(machine.selector_states[0].transitions[0].destination, 901);
        assert_eq!(value.controller.values.values[0].id, 1001);
        assert_eq!(
            value
                .controller
                .default_values
                .positions
                .values
                .read_values(4)
                .unwrap(),
            [1.0, 2.0, 3.0]
        );
        assert_eq!(
            value
                .controller
                .default_values
                .booleans
                .data
                .read(3)
                .unwrap(),
            [1, 0, 1]
        );
        assert_eq!(value.tos[0].key, 0xdead_beef);
        assert_eq!(value.tos[0].value, "Root/Body");
        assert_eq!(value.animation_clips[0].file_id, 1);
        assert_eq!(value.animation_clips[0].path_id, 77);
    }

    #[test]
    fn reads_big_endian_unity_2022_no_target_controller() {
        let endian = TestEndian::Big;
        let object = minimal_controller_object(endian, true, "wide", 91, None);
        let file = parse_asset(22, endian, -2, "2022.3.62f1", &object);

        let value =
            read_animator_controller(&file, 0, AnimatorControllerReadLimits::default()).unwrap();

        assert_eq!(value.name, "wide");
        assert!(value.controller.layers.is_empty());
        assert_eq!(value.tos, []);
        assert_eq!(value.animation_clips[0].path_id, 91);
    }

    #[test]
    fn reads_tuanjie_2022_3_controller_with_the_managed_layout() {
        let endian = TestEndian::Big;
        for version in ["2022.3.2t1", "2022.3.55t4", "2022.3.61t1"] {
            let object = minimal_controller_object(endian, true, "tuanjie", 74, None);
            let file = parse_asset(22, endian, -2, version, &object);
            let value = read_animator_controller(&file, 0, AnimatorControllerReadLimits::default())
                .unwrap_or_else(|error| panic!("{version}: {error}"));
            assert_eq!(value.name, "tuanjie", "{version}");
            assert!(value.controller.layers.is_empty(), "{version}");
            assert!(value.controller.default_values.entity_ids.is_none());
            assert_eq!(value.animation_clips[0].path_id, 74, "{version}");
        }
    }

    #[test]
    fn reads_unity_2023_and_6000_entity_id_layouts() {
        let endian = TestEndian::Little;
        for version in ["2023.2.20f1", "6000.0.60f1", "6000.1.12f1"] {
            let object = minimal_controller_object(endian, false, "modern", 91, None);
            let file = parse_asset(22, endian, 13, version, &object);
            let value = read_animator_controller(&file, 0, AnimatorControllerReadLimits::default())
                .unwrap();
            assert!(value.controller.default_values.entity_ids.is_none());
            assert_eq!(value.animation_clips[0].path_id, 91);
        }

        for version in ["6000.2.0f1", "6000.3.0f1"] {
            let object = minimal_controller_object(endian, false, "entity", 92, Some(&[17, -9]));
            let file = parse_asset(22, endian, 13, version, &object);
            let value = read_animator_controller(&file, 0, AnimatorControllerReadLimits::default())
                .unwrap();
            assert_eq!(
                value
                    .controller
                    .default_values
                    .entity_ids
                    .as_ref()
                    .unwrap()
                    .read_values(2)
                    .unwrap(),
                [17, -9],
                "{version}"
            );
            assert_eq!(value.animation_clips[0].path_id, 92, "{version}");
        }

        let missing = minimal_controller_object(endian, false, "missing", 93, None);
        let missing = parse_asset(22, endian, 13, "6000.2.0f1", &missing);
        assert!(
            read_animator_controller(&missing, 0, AnimatorControllerReadLimits::default()).is_err()
        );
    }

    #[test]
    fn rejects_versions_truncation_negative_counts_trailing_bytes_and_budgets() {
        let endian = TestEndian::Little;
        let object = minimal_controller_object(endian, false, "limits", 91, None);
        // The upper bound moved to 6000.3 on the evidence in
        // `validate_supported_version`, so the refusal case moves with it.
        for version in ["2017.2.9f1", "2024.1.0f1", "6000.4.0f1", "2023.1.0t1"] {
            let file = parse_asset(22, endian, 13, version, &object);
            assert!(
                read_animator_controller(&file, 0, AnimatorControllerReadLimits::default())
                    .is_err(),
                "{version} should be unsupported"
            );
        }

        let file = parse_asset(22, endian, 13, "2022.3.62f1", &object);
        let object_limit = AnimatorControllerReadLimits {
            maximum_object_bytes: u64::try_from(object.len() - 1).unwrap(),
            ..AnimatorControllerReadLimits::default()
        };
        assert!(read_animator_controller(&file, 0, object_limit).is_err());
        let string_limit = AnimatorControllerReadLimits {
            maximum_string_bytes: 2,
            ..AnimatorControllerReadLimits::default()
        };
        assert!(read_animator_controller(&file, 0, string_limit).is_err());
        let reference_limit = AnimatorControllerReadLimits {
            maximum_reference_bytes: 8,
            ..AnimatorControllerReadLimits::default()
        };
        assert!(read_animator_controller(&file, 0, reference_limit).is_err());
        let allocation_limit = AnimatorControllerReadLimits {
            maximum_total_allocation_bytes: 1,
            ..AnimatorControllerReadLimits::default()
        };
        assert!(read_animator_controller(&file, 0, allocation_limit).is_err());
        let element_limit = AnimatorControllerReadLimits {
            maximum_total_array_elements: 0,
            ..AnimatorControllerReadLimits::default()
        };
        assert!(read_animator_controller(&file, 0, element_limit).is_err());

        let truncated = parse_asset(22, endian, 13, "2022.3.62f1", &object[..object.len() - 1]);
        assert!(
            read_animator_controller(&truncated, 0, AnimatorControllerReadLimits::default())
                .is_err()
        );
        let mut trailing = object.clone();
        trailing.push(0);
        let trailing = parse_asset(22, endian, 13, "2022.3.62f1", &trailing);
        assert!(
            read_animator_controller(&trailing, 0, AnimatorControllerReadLimits::default())
                .is_err()
        );

        let mut negative = Vec::new();
        push_named_object(&mut negative, endian, false, "negative");
        endian.push_u32(&mut negative, 0);
        endian.push_i32(&mut negative, -1);
        let negative = parse_asset(22, endian, 13, "2022.3.62f1", &negative);
        assert!(
            read_animator_controller(&negative, 0, AnimatorControllerReadLimits::default())
                .is_err()
        );
    }

    #[allow(clippy::too_many_lines)]
    fn deep_controller_object(endian: TestEndian, no_target: bool) -> Vec<u8> {
        let mut out = Vec::new();
        push_named_object(&mut out, endian, no_target, "deep");
        endian.push_u32(&mut out, 0x1234);

        endian.push_i32(&mut out, 1);
        for value in [10, 20, 1, 2, 3] {
            endian.push_u32(&mut out, value);
        }
        endian.push_i32(&mut out, 1);
        endian.push_u32(&mut out, 44);
        endian.push_f32(&mut out, 0.75);
        endian.push_u32(&mut out, 55);
        endian.push_i32(&mut out, 2);
        endian.push_f32(&mut out, 0.5);
        out.extend_from_slice(&[1, 0]);
        align(&mut out, 4);

        endian.push_i32(&mut out, 1);
        endian.push_i32(&mut out, 1);
        endian.push_i32(&mut out, 1);
        push_transition(&mut out, endian, 100);
        push_i32_array(&mut out, endian, &[0]);
        endian.push_i32(&mut out, 1);
        endian.push_i32(&mut out, 1);
        for value in [11, 12, 13] {
            endian.push_u32(&mut out, value);
        }
        push_u32_array(&mut out, endian, &[4]);
        push_f32_array(&mut out, endian, &[0.25]);
        push_vector_array(&mut out, endian, 2, &[1.0, 2.0]);
        push_f32_array(&mut out, endian, &[3.0]);
        push_vector_array(&mut out, endian, 2, &[4.0, 5.0]);
        push_f32_array(&mut out, endian, &[6.0]);
        endian.push_i32(&mut out, 1);
        push_u32_array(&mut out, endian, &[7]);
        push_u32_array(&mut out, endian, &[8]);
        out.push(1);
        align(&mut out, 4);
        endian.push_u32(&mut out, 14);
        endian.push_f32(&mut out, 1.5);
        endian.push_f32(&mut out, 0.125);
        out.push(1);
        align(&mut out, 4);
        for value in 801..=808 {
            endian.push_u32(&mut out, value);
        }
        endian.push_f32(&mut out, 1.25);
        endian.push_f32(&mut out, 0.5);
        out.extend_from_slice(&[1, 1, 0, 1]);
        align(&mut out, 4);

        endian.push_i32(&mut out, 0);
        endian.push_i32(&mut out, 1);
        endian.push_i32(&mut out, 1);
        endian.push_u32(&mut out, 901);
        endian.push_i32(&mut out, 1);
        push_condition(&mut out, endian, 902);
        endian.push_u32(&mut out, 903);
        out.push(1);
        align(&mut out, 4);
        endian.push_u32(&mut out, 904);
        endian.push_u32(&mut out, 905);

        endian.push_i32(&mut out, 1);
        for value in [1001, 1002, 1003] {
            endian.push_u32(&mut out, value);
        }
        push_vector_array(&mut out, endian, 3, &[1.0, 2.0, 3.0]);
        push_vector_array(&mut out, endian, 4, &[0.0, 0.0, 0.0, 1.0]);
        push_vector_array(&mut out, endian, 3, &[1.0, 1.0, 1.0]);
        push_f32_array(&mut out, endian, &[0.75]);
        push_i32_array(&mut out, endian, &[42]);
        endian.push_i32(&mut out, 3);
        out.extend_from_slice(&[1, 0, 1]);
        align(&mut out, 4);

        endian.push_i32(&mut out, 1);
        endian.push_u32(&mut out, 0xdead_beef);
        push_aligned_string(&mut out, endian, "Root/Body");
        endian.push_i32(&mut out, 1);
        push_pptr(&mut out, endian, 1, 77);
        push_state_machine_behaviour_tail(&mut out, endian);
        out
    }

    fn minimal_controller_object(
        endian: TestEndian,
        no_target: bool,
        name: &str,
        clip_path_id: i64,
        entity_ids: Option<&[i32]>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        push_named_object(&mut out, endian, no_target, name);
        endian.push_u32(&mut out, 0);
        endian.push_i32(&mut out, 0);
        endian.push_i32(&mut out, 0);
        endian.push_i32(&mut out, 0);
        for _ in 0..6 {
            endian.push_i32(&mut out, 0);
        }
        if let Some(entity_ids) = entity_ids {
            push_i32_array(&mut out, endian, entity_ids);
        }
        endian.push_i32(&mut out, 0);
        endian.push_i32(&mut out, 1);
        push_pptr(&mut out, endian, 0, clip_path_id);
        push_state_machine_behaviour_tail(&mut out, endian);
        out
    }

    fn push_named_object(out: &mut Vec<u8>, endian: TestEndian, no_target: bool, name: &str) {
        if no_target {
            endian.push_u32(out, 0x0102_0304);
            push_pptr(out, endian, 0, 10);
            push_pptr(out, endian, 0, 11);
        }
        push_aligned_string(out, endian, name);
    }

    fn push_transition(out: &mut Vec<u8>, endian: TestEndian, base: u32) {
        endian.push_i32(out, 1);
        push_condition(out, endian, base);
        for value in (base + 3)..=(base + 6) {
            endian.push_u32(out, value);
        }
        for value in [0.5, 0.25, 0.75] {
            endian.push_f32(out, value);
        }
        out.extend_from_slice(&[1, 0]);
        align(out, 4);
        endian.push_i32(out, 2);
        out.extend_from_slice(&[1, 1]);
        align(out, 4);
    }

    fn push_condition(out: &mut Vec<u8>, endian: TestEndian, base: u32) {
        endian.push_u32(out, base + 1);
        endian.push_u32(out, base + 2);
        endian.push_f32(out, 0.5);
        endian.push_f32(out, 0.25);
    }

    fn push_u32_array(out: &mut Vec<u8>, endian: TestEndian, values: &[u32]) {
        endian.push_i32(out, i32::try_from(values.len()).unwrap());
        for value in values {
            endian.push_u32(out, *value);
        }
    }

    /// The sixteen bytes every real `AnimatorController` ends with.
    ///
    /// These fixtures used to stop at the animation clips, because that is
    /// where the managed reader stops. No Unity has ever written such a file:
    /// all 212 controllers in a real 2022.3 game carry this tail, and the
    /// reader's trailing-byte check is only worth anything if the fixtures
    /// carry it too.
    fn push_state_machine_behaviour_tail(out: &mut Vec<u8>, endian: TestEndian) {
        endian.push_i32(out, 0);
        endian.push_i32(out, 0);
        endian.push_i32(out, 0);
        out.push(1);
        align(out, 4);
    }

    fn push_i32_array(out: &mut Vec<u8>, endian: TestEndian, values: &[i32]) {
        endian.push_i32(out, i32::try_from(values.len()).unwrap());
        for value in values {
            endian.push_i32(out, *value);
        }
    }

    fn push_f32_array(out: &mut Vec<u8>, endian: TestEndian, values: &[f32]) {
        endian.push_i32(out, i32::try_from(values.len()).unwrap());
        for value in values {
            endian.push_f32(out, *value);
        }
    }

    fn push_vector_array(out: &mut Vec<u8>, endian: TestEndian, dimensions: usize, values: &[f32]) {
        assert_eq!(values.len() % dimensions, 0);
        endian.push_i32(out, i32::try_from(values.len() / dimensions).unwrap());
        for value in values {
            endian.push_f32(out, *value);
        }
    }

    fn push_aligned_string(out: &mut Vec<u8>, endian: TestEndian, value: &str) {
        endian.push_i32(out, i32::try_from(value.len()).unwrap());
        out.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(out, 4);
        }
    }

    fn push_pptr(out: &mut Vec<u8>, endian: TestEndian, file_id: i32, path_id: i64) {
        endian.push_i32(out, file_id);
        endian.push_i64(out, path_id);
    }

    fn parse_asset(
        format_version: u32,
        endian: TestEndian,
        target_platform: i32,
        unity_version: &str,
        object: &[u8],
    ) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_asset(
            format_version,
            endian,
            target_platform,
            unity_version,
            object,
        )))
        .unwrap()
    }

    fn synthetic_asset(
        format_version: u32,
        endian: TestEndian,
        target_platform: i32,
        unity_version: &str,
        object: &[u8],
    ) -> Vec<u8> {
        assert!(matches!(format_version, 17 | 22));
        let metadata_base = if format_version == 22 { 48 } else { 20 };
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        endian.push_i32(&mut metadata, target_platform);
        metadata.push(0);
        endian.push_i32(&mut metadata, 1);
        endian.push_i32(&mut metadata, ANIMATOR_CONTROLLER_CLASS_ID);
        metadata.push(0);
        metadata.extend_from_slice(&(-1_i16).to_ne_bytes());
        metadata.extend_from_slice(&[0_u8; 16]);
        endian.push_i32(&mut metadata, 1);
        align_with_base(&mut metadata, metadata_base, 4);
        endian.push_i64(&mut metadata, 7);
        if format_version == 22 {
            endian.push_i64(&mut metadata, 0);
        } else {
            endian.push_u32(&mut metadata, 0);
        }
        endian.push_u32(&mut metadata, u32::try_from(object.len()).unwrap());
        endian.push_i32(&mut metadata, 0);
        endian.push_i32(&mut metadata, 0);
        endian.push_i32(&mut metadata, 0);
        if format_version == 22 {
            endian.push_i32(&mut metadata, 0);
        }
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (metadata_base + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + object.len();
        let mut bytes = vec![0_u8; metadata_base];
        bytes[0..4].copy_from_slice(&metadata_size.to_be_bytes());
        bytes[4..8].copy_from_slice(&u32::try_from(file_size).unwrap().to_be_bytes());
        bytes[8..12].copy_from_slice(&format_version.to_be_bytes());
        bytes[12..16].copy_from_slice(&u32::try_from(data_offset).unwrap().to_be_bytes());
        bytes[16] = endian.marker();
        if format_version == 22 {
            bytes[20..24].copy_from_slice(&metadata_size.to_be_bytes());
            bytes[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
            bytes[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        }
        bytes.extend_from_slice(&metadata);
        bytes.resize(data_offset, 0);
        bytes.extend_from_slice(object);
        bytes
    }

    fn align(out: &mut Vec<u8>, alignment: usize) {
        while !out.len().is_multiple_of(alignment) {
            out.push(0);
        }
    }

    fn align_with_base(out: &mut Vec<u8>, base: usize, alignment: usize) {
        while !(base + out.len()).is_multiple_of(alignment) {
            out.push(0);
        }
    }

    #[derive(Clone, Copy)]
    enum TestEndian {
        Little,
        Big,
    }

    impl TestEndian {
        const fn marker(self) -> u8 {
            match self {
                Self::Little => 0,
                Self::Big => 1,
            }
        }

        fn push_i32(self, out: &mut Vec<u8>, value: i32) {
            match self {
                Self::Little => out.extend_from_slice(&value.to_le_bytes()),
                Self::Big => out.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_u32(self, out: &mut Vec<u8>, value: u32) {
            match self {
                Self::Little => out.extend_from_slice(&value.to_le_bytes()),
                Self::Big => out.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_i64(self, out: &mut Vec<u8>, value: i64) {
            match self {
                Self::Little => out.extend_from_slice(&value.to_le_bytes()),
                Self::Big => out.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_f32(self, out: &mut Vec<u8>, value: f32) {
            self.push_u32(out, value.to_bits());
        }
    }
}
