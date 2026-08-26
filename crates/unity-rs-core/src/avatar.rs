//! Complete, source-bound parsing for Unity and Tuanjie `Avatar` objects.
//!
//! The engine layout is verified for Unity 2017.3 through 2023.x, Unity 6000.0
//! through 6000.3, and Tuanjie 2022.3.x.
//! Unity 2018.2 removes the legacy handle/collider arrays, while Unity
//! 2019.1.0b1 adds the serialized `HumanDescription` tail. Unlike the managed
//! reader, this module consumes that tail and rejects any bytes left inside the
//! bounded object payload.

use std::mem::size_of;

use crate::endian::{Endian, EndianReader, checked_length};
use crate::serialized::{ObjectReference, SerializedFile};
use crate::source::{Region, RegionCursor};
use crate::version_gate::{VersionGateOutcome, finish_lenient};
use crate::{Error, Result};

pub const AVATAR_CLASS_ID: i32 = 90;

const NO_TARGET_PLATFORM: i32 = -2;

/// Defensive limits for one source-bound `Avatar` object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvatarReadLimits {
    pub maximum_object_bytes: u64,
    pub maximum_string_bytes: usize,
    pub maximum_total_string_bytes: usize,
    pub maximum_array_elements: usize,
    pub maximum_total_array_elements: usize,
    pub maximum_nested_objects: usize,
    pub maximum_total_allocation_bytes: u64,
    pub maximum_reference_bytes: u64,
}

impl Default for AvatarReadLimits {
    fn default() -> Self {
        Self {
            maximum_object_bytes: 512 * 1024 * 1024,
            maximum_string_bytes: 16 * 1024 * 1024,
            maximum_total_string_bytes: 64 * 1024 * 1024,
            maximum_array_elements: 2_000_000,
            maximum_total_array_elements: 10_000_000,
            maximum_nested_objects: 4_000_000,
            maximum_total_allocation_bytes: 256 * 1024 * 1024,
            maximum_reference_bytes: 64,
        }
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xform {
    pub translation: Vector3,
    pub rotation: Vector4,
    pub scale: Vector3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub parent_id: i32,
    pub axes_id: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    pub minimum: Vector3,
    pub maximum: Vector3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axes {
    pub pre_q: Vector4,
    pub post_q: Vector4,
    pub sign: Vector3,
    pub limit: Limit,
    pub length: f32,
    pub axes_type: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skeleton {
    pub nodes: Vec<Node>,
    pub ids: U32Array,
    pub axes: Vec<Axes>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkeletonPose {
    pub xforms: Vec<Xform>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hand {
    pub bone_indices: I32Array,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Handle {
    pub xform: Xform,
    pub parent_human_index: u32,
    pub id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Collider {
    pub xform: Xform,
    pub collider_type: u32,
    pub x_motion_type: u32,
    pub y_motion_type: u32,
    pub z_motion_type: u32,
    pub minimum_limit_x: f32,
    pub maximum_limit_x: f32,
    pub maximum_limit_y: f32,
    pub maximum_limit_z: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Human {
    pub root_xform: Xform,
    pub skeleton: Skeleton,
    pub skeleton_pose: SkeletonPose,
    pub left_hand: Hand,
    pub right_hand: Hand,
    /// Present through Unity 2018.1 and absent from Unity 2018.2 onward.
    pub handles: Option<Vec<Handle>>,
    /// Present through Unity 2018.1 and absent from Unity 2018.2 onward.
    pub colliders: Option<Vec<Collider>>,
    pub human_bone_indices: I32Array,
    pub human_bone_masses: F32Array,
    /// Present through Unity 2018.1 and absent from Unity 2018.2 onward.
    pub collider_indices: Option<I32Array>,
    pub scale: f32,
    pub arm_twist: f32,
    pub forearm_twist: f32,
    pub upper_leg_twist: f32,
    pub leg_twist: f32,
    pub arm_stretch: f32,
    pub leg_stretch: f32,
    pub feet_spacing: f32,
    pub has_left_hand: bool,
    pub has_right_hand: bool,
    pub has_translation_degrees_of_freedom: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AvatarConstant {
    pub avatar_skeleton: Skeleton,
    pub avatar_skeleton_pose: SkeletonPose,
    pub default_pose: SkeletonPose,
    pub skeleton_name_ids: U32Array,
    pub human: Human,
    pub human_skeleton_indices: I32Array,
    pub human_skeleton_reverse_indices: I32Array,
    pub root_motion_bone_index: i32,
    pub root_motion_bone_xform: Xform,
    pub root_motion_skeleton: Skeleton,
    pub root_motion_skeleton_pose: SkeletonPose,
    pub root_motion_skeleton_indices: I32Array,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkeletonBoneLimit {
    pub minimum: Vector3,
    pub maximum: Vector3,
    pub value: Vector3,
    pub length: f32,
    pub modified: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HumanBone {
    pub bone_name: String,
    pub human_name: String,
    pub limit: SkeletonBoneLimit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkeletonBone {
    pub name: String,
    pub parent_name: String,
    pub position: Vector3,
    pub rotation: Vector4,
    pub scale: Vector3,
}

/// Tail added to `Avatar` serialization in Unity 2019.1.0b1.
#[derive(Debug, Clone, PartialEq)]
pub struct HumanDescription {
    pub human_bones: Vec<HumanBone>,
    pub skeleton_bones: Vec<SkeletonBone>,
    pub arm_twist: f32,
    pub forearm_twist: f32,
    pub upper_leg_twist: f32,
    pub leg_twist: f32,
    pub arm_stretch: f32,
    pub leg_stretch: f32,
    pub feet_spacing: f32,
    pub global_scale: f32,
    pub root_motion_bone_name: String,
    pub has_translation_degrees_of_freedom: bool,
    pub has_extra_root: bool,
    pub skeleton_has_parents: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarPath {
    pub hash: u32,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Avatar {
    pub path_id: i64,
    pub name: String,
    pub declared_avatar_size: u32,
    pub constant: AvatarConstant,
    /// Ordered entries are retained so duplicate hashes keep Unity's first-hit behavior.
    pub paths: Vec<AvatarPath>,
    pub human_description: Option<HumanDescription>,
}

impl Avatar {
    #[must_use]
    pub fn find_bone_path(&self, hash: u32) -> Option<&str> {
        self.paths
            .iter()
            .find(|entry| entry.hash == hash)
            .map(|entry| entry.path.as_str())
    }
}

/// Reads a complete class-90 object, including the Unity 2019.1.0b1+ description tail.
pub fn read_avatar(
    file: &SerializedFile,
    object_index: usize,
    limits: AvatarReadLimits,
) -> Result<Avatar> {
    let outcome = validate_supported_version(file)?;
    let result = (|| -> Result<Avatar> {
        let mut reader = AvatarReader::new(file, object_index, limits)?;
        let name = reader.read_named_object()?;
        let declared_avatar_size = reader.reader.read_u32()?;
        let constant = reader.read_avatar_constant()?;
        let paths = reader.read_paths()?;
        let human_description = if has_human_description(file) {
            Some(reader.read_human_description()?)
        } else {
            None
        };
        let trailing = reader.reader.remaining()?;
        if trailing != 0 {
            return Err(Error::invalid_data(format!(
                "Avatar object contains {trailing} trailing bytes after its verified layout"
            )));
        }
        Ok(Avatar {
            path_id: reader.path_id,
            name,
            declared_avatar_size,
            constant,
            paths,
            human_description,
        })
    })();
    finish_lenient(outcome, "Avatar", &file.unity_version, result)
}

fn has_human_description(file: &SerializedFile) -> bool {
    let version = &file.unity_version;
    version.components() > (2019, 1, 0)
        || (version.components() == (2019, 1, 0)
            && !version.is_alpha()
            && (!version.is_beta() || version.build >= 1))
}

fn validate_supported_version(file: &SerializedFile) -> Result<VersionGateOutcome> {
    if file.unity_version.is_stripped() {
        return Err(Error::unsupported(
            "Avatar requires a Unity version because its Human layout is version-dependent",
        ));
    }
    let version = file.unity_version.components();
    if file.unity_version.is_tuanjie() {
        if version.0 == 2022 && version.1 == 3 && version.2 >= 2 {
            return Ok(VersionGateOutcome::Verified);
        }
    } else {
        // 6000.3 checked rather than assumed: the managed reader carries no
        // Avatar branch newer than 2019.1's human description, and the
        // 6000.3.12f1 differential fixture locks that shared layout.
        if ((2017, 3, 0)..(2024, 0, 0)).contains(&version) || (version.0 == 6000 && version.1 <= 3)
        {
            return Ok(VersionGateOutcome::Verified);
        }
        // Leniency applies only above the verified standard-Unity range;
        // versions below the floor keep the historical rejection.
        if version >= (2024, 0, 0) && !file.strict_unity_versions {
            return Ok(VersionGateOutcome::AboveVerifiedRange);
        }
    }
    Err(Error::unsupported(format!(
        "Avatar Unity version {} is outside the verified 2017.3 through 2023.x, 6000.0 through 6000.3, and Tuanjie 2022.3.x ranges",
        file.unity_version
    )))
}

#[derive(Debug, Default)]
struct AvatarBudget {
    string_bytes: usize,
    array_elements: usize,
    nested_objects: usize,
    allocation_bytes: u64,
    reference_bytes: u64,
}

struct AvatarReader {
    region: Region,
    reader: EndianReader<RegionCursor>,
    endian: Endian,
    absolute_start: u64,
    path_id: i64,
    target_platform: i32,
    format_version: u32,
    version: (u32, u32, u32),
    limits: AvatarReadLimits,
    budget: AvatarBudget,
}

impl AvatarReader {
    fn new(file: &SerializedFile, object_index: usize, limits: AvatarReadLimits) -> Result<Self> {
        let object = file.objects.get(object_index).ok_or_else(|| {
            Error::invalid_data(format!(
                "serialized object index {object_index} is out of range"
            ))
        })?;
        if object.class_id != AVATAR_CLASS_ID {
            return Err(Error::unsupported(format!(
                "object {} has class ID {}, expected {AVATAR_CLASS_ID} (Avatar)",
                object.path_id, object.class_id
            )));
        }
        if object.byte_size > limits.maximum_object_bytes {
            return Err(Error::invalid_data(format!(
                "Avatar object is {} bytes, exceeding limit {}",
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
            budget: AvatarBudget::default(),
        })
    }

    fn read_named_object(&mut self) -> Result<String> {
        if self.target_platform == NO_TARGET_PLATFORM {
            let _hide_flags = self.reader.read_u32()?;
            let _prefab_parent = self.read_pptr("Avatar prefab parent")?;
            let _prefab_internal = self.read_pptr("Avatar prefab internal")?;
        }
        self.read_aligned_string("Avatar name")
    }

    fn read_avatar_constant(&mut self) -> Result<AvatarConstant> {
        Ok(AvatarConstant {
            avatar_skeleton: self.read_skeleton("Avatar skeleton")?,
            avatar_skeleton_pose: self.read_skeleton_pose("Avatar skeleton pose")?,
            default_pose: self.read_skeleton_pose("Avatar default pose")?,
            skeleton_name_ids: self.read_u32_array("Avatar skeleton name IDs")?,
            human: self.read_human()?,
            human_skeleton_indices: self.read_i32_array("Avatar human skeleton indices")?,
            human_skeleton_reverse_indices: self
                .read_i32_array("Avatar human skeleton reverse indices")?,
            root_motion_bone_index: self.reader.read_i32()?,
            root_motion_bone_xform: self.read_xform()?,
            root_motion_skeleton: self.read_skeleton("Avatar root-motion skeleton")?,
            root_motion_skeleton_pose: self
                .read_skeleton_pose("Avatar root-motion skeleton pose")?,
            root_motion_skeleton_indices: self
                .read_i32_array("Avatar root-motion skeleton indices")?,
        })
    }

    fn read_skeleton(&mut self, field: &str) -> Result<Skeleton> {
        let node_field = format!("{field} nodes");
        let node_count = self.read_complex_count::<Node>(&node_field, 8)?;
        let mut nodes = Self::reserve_vec(node_count, &node_field)?;
        for _ in 0..node_count {
            nodes.push(Node {
                parent_id: self.reader.read_i32()?,
                axes_id: self.reader.read_i32()?,
            });
        }
        let ids = self.read_u32_array(&format!("{field} IDs"))?;
        let axes_field = format!("{field} axes");
        let axes_count = self.read_complex_count::<Axes>(&axes_field, 76)?;
        let mut axes = Self::reserve_vec(axes_count, &axes_field)?;
        for _ in 0..axes_count {
            axes.push(self.read_axes()?);
        }
        Ok(Skeleton { nodes, ids, axes })
    }

    fn read_skeleton_pose(&mut self, field: &str) -> Result<SkeletonPose> {
        let count = self.read_complex_count::<Xform>(field, 40)?;
        let mut xforms = Self::reserve_vec(count, field)?;
        for _ in 0..count {
            xforms.push(self.read_xform()?);
        }
        Ok(SkeletonPose { xforms })
    }

    fn read_axes(&mut self) -> Result<Axes> {
        Ok(Axes {
            pre_q: self.read_vector4()?,
            post_q: self.read_vector4()?,
            sign: self.read_vector3()?,
            limit: Limit {
                minimum: self.read_vector3()?,
                maximum: self.read_vector3()?,
            },
            length: self.reader.read_f32()?,
            axes_type: self.reader.read_u32()?,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn read_human(&mut self) -> Result<Human> {
        let root_xform = self.read_xform()?;
        let skeleton = self.read_skeleton("Avatar human skeleton")?;
        let skeleton_pose = self.read_skeleton_pose("Avatar human skeleton pose")?;
        let left_hand = Hand {
            bone_indices: self.read_i32_array("Avatar left-hand bone indices")?,
        };
        let right_hand = Hand {
            bone_indices: self.read_i32_array("Avatar right-hand bone indices")?,
        };
        let legacy_colliders = self.version < (2018, 2, 0);
        let handles = if legacy_colliders {
            let count = self.read_complex_count::<Handle>("Avatar handles", 48)?;
            let mut values = Self::reserve_vec(count, "Avatar handles")?;
            for _ in 0..count {
                values.push(Handle {
                    xform: self.read_xform()?,
                    parent_human_index: self.reader.read_u32()?,
                    id: self.reader.read_u32()?,
                });
            }
            Some(values)
        } else {
            None
        };
        let colliders = if legacy_colliders {
            let count = self.read_complex_count::<Collider>("Avatar colliders", 72)?;
            let mut values = Self::reserve_vec(count, "Avatar colliders")?;
            for _ in 0..count {
                values.push(Collider {
                    xform: self.read_xform()?,
                    collider_type: self.reader.read_u32()?,
                    x_motion_type: self.reader.read_u32()?,
                    y_motion_type: self.reader.read_u32()?,
                    z_motion_type: self.reader.read_u32()?,
                    minimum_limit_x: self.reader.read_f32()?,
                    maximum_limit_x: self.reader.read_f32()?,
                    maximum_limit_y: self.reader.read_f32()?,
                    maximum_limit_z: self.reader.read_f32()?,
                });
            }
            Some(values)
        } else {
            None
        };
        let human_bone_indices = self.read_i32_array("Avatar human bone indices")?;
        let human_bone_masses = self.read_f32_array("Avatar human bone masses")?;
        let collider_indices = legacy_colliders
            .then(|| self.read_i32_array("Avatar collider indices"))
            .transpose()?;
        let scale = self.reader.read_f32()?;
        let arm_twist = self.reader.read_f32()?;
        let forearm_twist = self.reader.read_f32()?;
        let upper_leg_twist = self.reader.read_f32()?;
        let leg_twist = self.reader.read_f32()?;
        let arm_stretch = self.reader.read_f32()?;
        let leg_stretch = self.reader.read_f32()?;
        let feet_spacing = self.reader.read_f32()?;
        let has_left_hand = self.reader.read_bool()?;
        let has_right_hand = self.reader.read_bool()?;
        let has_translation_degrees_of_freedom = self.reader.read_bool()?;
        self.align(4)?;
        Ok(Human {
            root_xform,
            skeleton,
            skeleton_pose,
            left_hand,
            right_hand,
            handles,
            colliders,
            human_bone_indices,
            human_bone_masses,
            collider_indices,
            scale,
            arm_twist,
            forearm_twist,
            upper_leg_twist,
            leg_twist,
            arm_stretch,
            leg_stretch,
            feet_spacing,
            has_left_hand,
            has_right_hand,
            has_translation_degrees_of_freedom,
        })
    }

    fn read_paths(&mut self) -> Result<Vec<AvatarPath>> {
        let count = self.read_complex_count::<AvatarPath>("Avatar TOS entries", 8)?;
        let mut paths = Self::reserve_vec(count, "Avatar TOS entries")?;
        for _ in 0..count {
            paths.push(AvatarPath {
                hash: self.reader.read_u32()?,
                path: self.read_aligned_string("Avatar TOS path")?,
            });
        }
        Ok(paths)
    }

    fn read_human_description(&mut self) -> Result<HumanDescription> {
        let human_count =
            self.read_complex_count::<HumanBone>("Avatar description human bones", 49)?;
        let mut human_bones = Self::reserve_vec(human_count, "Avatar description human bones")?;
        for _ in 0..human_count {
            let bone_name = self.read_aligned_string("Avatar description bone name")?;
            let human_name = self.read_aligned_string("Avatar description human name")?;
            let limit = SkeletonBoneLimit {
                minimum: self.read_vector3()?,
                maximum: self.read_vector3()?,
                value: self.read_vector3()?,
                length: self.reader.read_f32()?,
                modified: self.reader.read_bool()?,
            };
            self.align(4)?;
            human_bones.push(HumanBone {
                bone_name,
                human_name,
                limit,
            });
        }
        let skeleton_count =
            self.read_complex_count::<SkeletonBone>("Avatar description skeleton bones", 48)?;
        let mut skeleton_bones =
            Self::reserve_vec(skeleton_count, "Avatar description skeleton bones")?;
        for _ in 0..skeleton_count {
            skeleton_bones.push(SkeletonBone {
                name: self.read_aligned_string("Avatar description skeleton name")?,
                parent_name: self.read_aligned_string("Avatar description skeleton parent name")?,
                position: self.read_vector3()?,
                rotation: self.read_vector4()?,
                scale: self.read_vector3()?,
            });
        }
        let arm_twist = self.reader.read_f32()?;
        let forearm_twist = self.reader.read_f32()?;
        let upper_leg_twist = self.reader.read_f32()?;
        let leg_twist = self.reader.read_f32()?;
        let arm_stretch = self.reader.read_f32()?;
        let leg_stretch = self.reader.read_f32()?;
        let feet_spacing = self.reader.read_f32()?;
        let global_scale = self.reader.read_f32()?;
        let root_motion_bone_name =
            self.read_aligned_string("Avatar description root-motion bone name")?;
        let has_translation_degrees_of_freedom = self.reader.read_bool()?;
        let has_extra_root = self.reader.read_bool()?;
        let skeleton_has_parents = self.reader.read_bool()?;
        self.align(4)?;
        Ok(HumanDescription {
            human_bones,
            skeleton_bones,
            arm_twist,
            forearm_twist,
            upper_leg_twist,
            leg_twist,
            arm_stretch,
            leg_stretch,
            feet_spacing,
            global_scale,
            root_motion_bone_name,
            has_translation_degrees_of_freedom,
            has_extra_root,
            skeleton_has_parents,
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

    fn read_xform(&mut self) -> Result<Xform> {
        Ok(Xform {
            translation: self.read_vector3()?,
            rotation: self.read_vector4()?,
            scale: self.read_vector3()?,
        })
    }

    fn read_i32_array(&mut self, field: &str) -> Result<I32Array> {
        let (region, count) = self.read_numeric_region(field, 4)?;
        Ok(I32Array {
            region,
            count,
            endian: self.endian,
        })
    }

    fn read_u32_array(&mut self, field: &str) -> Result<U32Array> {
        let (region, count) = self.read_numeric_region(field, 4)?;
        Ok(U32Array {
            region,
            count,
            endian: self.endian,
        })
    }

    fn read_f32_array(&mut self, field: &str) -> Result<F32Array> {
        let (region, count) = self.read_numeric_region(field, 4)?;
        Ok(F32Array {
            region,
            count,
            endian: self.endian,
        })
    }

    fn read_numeric_region(&mut self, field: &str, element_size: usize) -> Result<(Region, usize)> {
        let count = self.read_count(field, element_size)?;
        let byte_length = count
            .checked_mul(element_size)
            .ok_or_else(|| Error::invalid_data(format!("{field} byte size overflowed")))?;
        let byte_length = u64::try_from(byte_length)
            .map_err(|_| Error::invalid_data(format!("{field} byte size does not fit in u64")))?;
        let region = self.take_region(byte_length, field)?;
        Ok((region, count))
    }

    fn read_complex_count<T>(&mut self, field: &str, minimum_size: usize) -> Result<usize> {
        let count = self.read_count(field, minimum_size)?;
        self.budget.nested_objects = self
            .budget
            .nested_objects
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("Avatar nested-object budget overflowed"))?;
        if self.budget.nested_objects > self.limits.maximum_nested_objects {
            return Err(Error::invalid_data(format!(
                "Avatar nested objects total {}, exceeding limit {} while reading {field}",
                self.budget.nested_objects, self.limits.maximum_nested_objects
            )));
        }
        let allocation = u64::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(u64::try_from(size_of::<T>()).ok()?))
            .ok_or_else(|| Error::invalid_data(format!("{field} allocation size overflowed")))?;
        self.charge_allocation(allocation, field)?;
        Ok(count)
    }

    fn read_count(&mut self, field: &str, minimum_size: usize) -> Result<usize> {
        let count = checked_length(self.reader.read_i32()?, field)?;
        if count > self.limits.maximum_array_elements {
            return Err(Error::invalid_data(format!(
                "{field} count {count} exceeds limit {}",
                self.limits.maximum_array_elements
            )));
        }
        self.budget.array_elements = self
            .budget
            .array_elements
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("Avatar array-element budget overflowed"))?;
        if self.budget.array_elements > self.limits.maximum_total_array_elements {
            return Err(Error::invalid_data(format!(
                "Avatar arrays total {} elements, exceeding limit {} while reading {field}",
                self.budget.array_elements, self.limits.maximum_total_array_elements
            )));
        }
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
            .ok_or_else(|| Error::invalid_data("Avatar string-byte budget overflowed"))?;
        if self.budget.string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "Avatar strings total {} bytes, exceeding limit {} while reading {field}",
                self.budget.string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        let allocation = u64::try_from(length)
            .ok()
            .and_then(|length| length.checked_mul(4))
            .ok_or_else(|| Error::invalid_data(format!("{field} allocation size overflowed")))?;
        self.charge_allocation(allocation, field)?;
        let value = self.reader.read_utf8(length)?;
        if length != 0 {
            self.align(4)?;
        }
        Ok(value)
    }

    fn read_pptr(&mut self, field: &str) -> Result<ObjectReference> {
        let size = u64::try_from(self.pptr_size())
            .map_err(|_| Error::invalid_data("Avatar PPtr size does not fit in u64"))?;
        self.budget.reference_bytes = self
            .budget
            .reference_bytes
            .checked_add(size)
            .ok_or_else(|| Error::invalid_data("Avatar reference-byte budget overflowed"))?;
        if self.budget.reference_bytes > self.limits.maximum_reference_bytes {
            return Err(Error::invalid_data(format!(
                "Avatar references total {} bytes, exceeding limit {} while reading {field}",
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

    const fn pptr_size(&self) -> usize {
        if self.format_version < 14 { 8 } else { 12 }
    }

    fn reserve_vec<T>(count: usize, field: &str) -> Result<Vec<T>> {
        let mut values = Vec::new();
        values.try_reserve_exact(count).map_err(|error| {
            Error::invalid_data(format!("cannot allocate {count} {field}: {error}"))
        })?;
        Ok(values)
    }

    fn charge_allocation(&mut self, bytes: u64, field: &str) -> Result<()> {
        self.budget.allocation_bytes = self
            .budget
            .allocation_bytes
            .checked_add(bytes)
            .ok_or_else(|| Error::invalid_data("Avatar allocation budget overflowed"))?;
        if self.budget.allocation_bytes > self.limits.maximum_total_allocation_bytes {
            return Err(Error::invalid_data(format!(
                "Avatar allocations total {} bytes, exceeding limit {} while reading {field}",
                self.budget.allocation_bytes, self.limits.maximum_total_allocation_bytes
            )));
        }
        Ok(())
    }

    fn take_region(&mut self, length: u64, field: &str) -> Result<Region> {
        let position = self.reader.position()?;
        let output = self.region.subregion(position, length).map_err(|error| {
            Error::invalid_data(format!("{field} exceeds bounded Avatar object: {error}"))
        })?;
        self.skip(length, field)?;
        Ok(output)
    }

    fn align(&mut self, alignment: u64) -> Result<()> {
        let position = self.reader.position()?;
        let absolute = self
            .absolute_start
            .checked_add(position)
            .ok_or_else(|| Error::invalid_data("Avatar alignment position overflowed"))?;
        let remainder = absolute % alignment;
        if remainder != 0 {
            self.skip(alignment - remainder, "Avatar alignment")?;
        }
        Ok(())
    }

    fn skip(&mut self, length: u64, field: &str) -> Result<()> {
        let position = self.reader.position()?;
        let target = position
            .checked_add(length)
            .ok_or_else(|| Error::invalid_data(format!("{field} position overflowed")))?;
        if target > self.region.len() {
            return Err(Error::invalid_data(format!(
                "{field} ends at {target}, beyond Avatar object size {}",
                self.region.len()
            )));
        }
        self.reader.set_position(target)
    }
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

#[cfg(test)]
mod tests {
    use crate::serialized::SerializedFile;
    use crate::source::Region;

    use super::{AVATAR_CLASS_ID, AvatarReadLimits, read_avatar};

    #[test]
    fn reads_legacy_big_endian_no_target_avatar_with_32_bit_pptrs() {
        let endian = TestEndian::Big;
        let object = legacy_avatar_object(endian, true, 13);
        let file = parse_asset(13, endian, -2, "2017.3.0f3", &object, AVATAR_CLASS_ID);

        let avatar = read_avatar(&file, 0, AvatarReadLimits::default()).unwrap();

        assert_eq!(avatar.path_id, 7);
        assert_eq!(avatar.name, "legacy avatar");
        assert_eq!(avatar.declared_avatar_size, 0x1020_3040);
        assert_eq!(avatar.constant.avatar_skeleton.nodes.len(), 1);
        assert_eq!(avatar.constant.avatar_skeleton.nodes[0].parent_id, -1);
        assert_eq!(
            avatar.constant.avatar_skeleton.ids.read_values(4).unwrap(),
            [0x1122_3344]
        );
        assert_eq!(avatar.constant.avatar_skeleton.axes.len(), 1);
        assert_eq!(
            avatar.constant.avatar_skeleton.axes[0].length.to_bits(),
            7.5_f32.to_bits()
        );
        let human = &avatar.constant.human;
        assert_eq!(human.handles.as_ref().unwrap().len(), 1);
        assert_eq!(human.handles.as_ref().unwrap()[0].id, 99);
        assert_eq!(human.colliders.as_ref().unwrap().len(), 1);
        assert_eq!(human.colliders.as_ref().unwrap()[0].collider_type, 3);
        assert_eq!(
            human
                .collider_indices
                .as_ref()
                .unwrap()
                .read_values(4)
                .unwrap(),
            [5]
        );
        assert!(human.has_left_hand);
        assert!(!human.has_right_hand);
        assert!(human.has_translation_degrees_of_freedom);
        assert_eq!(avatar.find_bone_path(0xAABB_CCDD), Some("Root/Hips"));
        assert_eq!(avatar.find_bone_path(123), None);
        assert!(avatar.human_description.is_none());
    }

    #[test]
    fn reads_2022_avatar_and_complete_human_description_tail() {
        let endian = TestEndian::Little;
        let object = modern_avatar_object(endian);
        let file = parse_asset(22, endian, 13, "2022.3.62f1", &object, AVATAR_CLASS_ID);

        let avatar = read_avatar(&file, 0, AvatarReadLimits::default()).unwrap();

        assert_eq!(avatar.name, "modern avatar");
        assert!(avatar.constant.human.handles.is_none());
        assert!(avatar.constant.human.colliders.is_none());
        assert!(avatar.constant.human.collider_indices.is_none());
        assert_eq!(avatar.paths.len(), 3);
        assert_eq!(avatar.find_bone_path(7), Some("first/path"));
        let description = avatar.human_description.unwrap();
        assert_eq!(description.human_bones.len(), 1);
        assert_eq!(description.human_bones[0].bone_name, "mixamorig:Hips");
        assert_eq!(description.human_bones[0].human_name, "Hips");
        assert!(description.human_bones[0].limit.modified);
        assert_eq!(description.skeleton_bones.len(), 1);
        assert_eq!(description.skeleton_bones[0].parent_name, "Armature");
        assert_eq!(description.root_motion_bone_name, "Hips");
        assert_eq!(description.global_scale.to_bits(), 2.0_f32.to_bits());
        assert!(description.has_translation_degrees_of_freedom);
        assert!(description.has_extra_root);
        assert!(description.skeleton_has_parents);
    }

    #[test]
    fn reads_2023_and_unity_6000_complete_human_description_layouts() {
        let endian = TestEndian::Little;
        let object = modern_avatar_object(endian);
        for version in [
            "2023.3.0f1",
            "6000.0.0f1",
            "6000.1.0f1",
            "6000.2.0f1",
            "6000.3.0f1",
        ] {
            let file = parse_asset(22, endian, 13, version, &object, AVATAR_CLASS_ID);
            let avatar = read_avatar(&file, 0, AvatarReadLimits::default()).unwrap();
            let description = avatar.human_description.unwrap();
            assert_eq!(avatar.name, "modern avatar", "{version}");
            assert_eq!(description.human_bones.len(), 1, "{version}");
            assert_eq!(description.skeleton_bones.len(), 1, "{version}");
            assert_eq!(description.root_motion_bone_name, "Hips", "{version}");
        }
    }

    #[test]
    fn reads_tuanjie_2022_3_avatar_with_the_managed_layout() {
        let endian = TestEndian::Big;
        let object = modern_avatar_object(endian);
        for version in ["2022.3.2t1", "2022.3.55t4", "2022.3.61t1"] {
            let file = parse_asset(22, endian, 13, version, &object, AVATAR_CLASS_ID);
            let avatar = read_avatar(&file, 0, AvatarReadLimits::default())
                .unwrap_or_else(|error| panic!("{version}: {error}"));
            assert_eq!(avatar.name, "modern avatar", "{version}");
            assert_eq!(avatar.find_bone_path(7), Some("first/path"), "{version}");
            let description = avatar.human_description.as_ref().unwrap();
            assert_eq!(description.human_bones.len(), 1, "{version}");
            assert_eq!(description.skeleton_bones.len(), 1, "{version}");
            assert_eq!(description.root_motion_bone_name, "Hips", "{version}");
        }
    }

    #[test]
    fn switches_human_description_exactly_at_2019_1_0_beta_1() {
        let endian = TestEndian::Little;
        let alpha_object = pre_description_avatar_object(endian);
        let alpha = parse_asset(
            22,
            endian,
            13,
            "2019.1.0a13",
            &alpha_object,
            AVATAR_CLASS_ID,
        );
        assert!(
            read_avatar(&alpha, 0, AvatarReadLimits::default())
                .unwrap()
                .human_description
                .is_none()
        );

        let beta_object = modern_avatar_object(endian);
        let beta = parse_asset(22, endian, 13, "2019.1.0b1", &beta_object, AVATAR_CLASS_ID);
        assert!(
            read_avatar(&beta, 0, AvatarReadLimits::default())
                .unwrap()
                .human_description
                .is_some()
        );
    }

    #[test]
    fn switches_legacy_human_arrays_exactly_at_2018_2() {
        let endian = TestEndian::Little;
        let legacy = legacy_avatar_object(endian, false, 22);
        let before = parse_asset(22, endian, 13, "2018.1.9f2", &legacy, AVATAR_CLASS_ID);
        assert!(
            read_avatar(&before, 0, AvatarReadLimits::default())
                .unwrap()
                .constant
                .human
                .handles
                .is_some()
        );

        let modern = pre_description_avatar_object(endian);
        let at_gate = parse_asset(22, endian, 13, "2018.2.0f2", &modern, AVATAR_CLASS_ID);
        let human = read_avatar(&at_gate, 0, AvatarReadLimits::default())
            .unwrap()
            .constant
            .human;
        assert!(human.handles.is_none());
        assert!(human.colliders.is_none());
        assert!(human.collider_indices.is_none());
    }

    #[test]
    fn aligns_against_the_absolute_file_position_for_nonzero_object_offsets() {
        let endian = TestEndian::Little;
        let object = absolute_alignment_avatar_object(endian);
        let file = parse_asset_at_offset(22, endian, 13, "2018.2.0f2", &object, AVATAR_CLASS_ID, 1);

        let avatar = read_avatar(&file, 0, AvatarReadLimits::default()).unwrap();

        assert_eq!(avatar.name, "");
        assert_eq!(avatar.constant.human.scale.to_bits(), 1.0_f32.to_bits());
        assert!(avatar.paths.is_empty());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn rejects_truncation_negative_counts_trailing_bytes_and_all_budgets() {
        let endian = TestEndian::Little;
        let object = modern_avatar_object(endian);
        let file = parse_asset(22, endian, 13, "2022.3.62f1", &object, AVATAR_CLASS_ID);
        let defaults = AvatarReadLimits::default();
        for limits in [
            AvatarReadLimits {
                maximum_object_bytes: u64::try_from(object.len() - 1).unwrap(),
                ..defaults
            },
            AvatarReadLimits {
                maximum_string_bytes: 1,
                ..defaults
            },
            AvatarReadLimits {
                maximum_string_bytes: 16,
                maximum_total_string_bytes: 20,
                ..defaults
            },
            AvatarReadLimits {
                maximum_array_elements: 0,
                ..defaults
            },
            AvatarReadLimits {
                maximum_array_elements: 10,
                maximum_total_array_elements: 1,
                ..defaults
            },
            AvatarReadLimits {
                maximum_nested_objects: 1,
                ..defaults
            },
            AvatarReadLimits {
                maximum_total_allocation_bytes: 60,
                ..defaults
            },
        ] {
            assert!(read_avatar(&file, 0, limits).is_err());
        }

        let truncated = parse_asset(
            22,
            endian,
            13,
            "2022.3.62f1",
            &object[..object.len() - 1],
            AVATAR_CLASS_ID,
        );
        assert!(read_avatar(&truncated, 0, defaults).is_err());

        let mut trailing = object.clone();
        trailing.push(0xA5);
        let trailing_file = parse_asset(22, endian, 13, "2022.3.62f1", &trailing, AVATAR_CLASS_ID);
        assert!(read_avatar(&trailing_file, 0, defaults).is_err());

        let negative = negative_count_object(endian);
        let negative_file = parse_asset(22, endian, 13, "2022.3.62f1", &negative, AVATAR_CLASS_ID);
        assert!(read_avatar(&negative_file, 0, defaults).is_err());

        let legacy = legacy_avatar_object(TestEndian::Big, true, 13);
        let no_target = parse_asset(
            13,
            TestEndian::Big,
            -2,
            "2017.3.0f3",
            &legacy,
            AVATAR_CLASS_ID,
        );
        assert!(
            read_avatar(
                &no_target,
                0,
                AvatarReadLimits {
                    maximum_reference_bytes: 15,
                    ..defaults
                }
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_wrong_class_stripped_and_out_of_range_versions() {
        let endian = TestEndian::Little;
        let object = modern_avatar_object(endian);
        for version in ["0.0.0", "2017.2.5f1", "2023.1.0t1"] {
            let file = parse_asset(22, endian, 13, version, &object, AVATAR_CLASS_ID);
            assert!(
                read_avatar(&file, 0, AvatarReadLimits::default()).is_err(),
                "{version}"
            );
        }
        let wrong_class = parse_asset(22, endian, 13, "2022.3.62f1", &object, 91);
        assert!(read_avatar(&wrong_class, 0, AvatarReadLimits::default()).is_err());

        // Above the verified ceiling (6000.3) the default is lenient: the
        // newest known layout is attempted, a layout mismatch is reported as
        // `Unsupported`, and only `strict_unity_versions` restores the
        // historical rejection.
        for version in ["2024.1.0f1", "6000.4.0f1"] {
            let lenient = parse_asset(22, endian, 13, version, &object, AVATAR_CLASS_ID);
            read_avatar(&lenient, 0, AvatarReadLimits::default())
                .unwrap_or_else(|error| panic!("{version}: {error}"));

            let short = parse_asset(
                22,
                endian,
                13,
                version,
                &object[..object.len() - 8],
                AVATAR_CLASS_ID,
            );
            let error = read_avatar(&short, 0, AvatarReadLimits::default()).unwrap_err();
            let crate::Error::Unsupported(message) = &error else {
                panic!("{version}: expected Unsupported, got {error:?}");
            };
            assert!(
                message.contains("above the verified range"),
                "{version}: {message}"
            );

            let mut strict = parse_asset(22, endian, 13, version, &object, AVATAR_CLASS_ID);
            strict.strict_unity_versions = true;
            let error = read_avatar(&strict, 0, AvatarReadLimits::default()).unwrap_err();
            assert!(
                error.to_string().contains("outside the verified"),
                "{version}: {error}"
            );
        }
    }

    fn legacy_avatar_object(endian: TestEndian, no_target: bool, format_version: u32) -> Vec<u8> {
        let mut output = named_avatar(endian, "legacy avatar", no_target, format_version);
        endian.push_u32(&mut output, 0x1020_3040);
        push_skeleton(&mut output, endian, true);
        push_pose(&mut output, endian, 1);
        push_pose(&mut output, endian, 0);
        push_u32_array(&mut output, endian, &[0x5566_7788]);
        push_xform(&mut output, endian, 10.0);
        push_skeleton(&mut output, endian, false);
        push_pose(&mut output, endian, 0);
        push_i32_array(&mut output, endian, &[1, 2]);
        push_i32_array(&mut output, endian, &[3]);
        endian.push_i32(&mut output, 1);
        push_xform(&mut output, endian, 20.0);
        endian.push_u32(&mut output, 7);
        endian.push_u32(&mut output, 99);
        endian.push_i32(&mut output, 1);
        push_xform(&mut output, endian, 30.0);
        for value in [3_u32, 4, 5, 6] {
            endian.push_u32(&mut output, value);
        }
        for value in [-1.0_f32, 1.0, 2.0, 3.0] {
            endian.push_f32(&mut output, value);
        }
        push_i32_array(&mut output, endian, &[10, 11]);
        push_f32_array(&mut output, endian, &[0.25, 0.75]);
        push_i32_array(&mut output, endian, &[5]);
        for value in [1.0_f32, 0.5, 0.6, 0.7, 0.8, 0.05, 0.06, 0.0] {
            endian.push_f32(&mut output, value);
        }
        output.extend_from_slice(&[1, 0, 1]);
        align(&mut output, 4);
        push_i32_array(&mut output, endian, &[9]);
        push_i32_array(&mut output, endian, &[-1, 0]);
        endian.push_i32(&mut output, 4);
        push_xform(&mut output, endian, 40.0);
        push_skeleton(&mut output, endian, false);
        push_pose(&mut output, endian, 0);
        push_i32_array(&mut output, endian, &[8]);
        endian.push_i32(&mut output, 2);
        endian.push_u32(&mut output, 0xAABB_CCDD);
        push_aligned_string(&mut output, endian, "Root/Hips");
        endian.push_u32(&mut output, 0xAABB_CCDD);
        push_aligned_string(&mut output, endian, "duplicate/path");
        output
    }

    fn pre_description_avatar_object(endian: TestEndian) -> Vec<u8> {
        let mut output = named_avatar(endian, "2018 gate", false, 22);
        endian.push_u32(&mut output, 0);
        push_modern_constant(&mut output, endian);
        endian.push_i32(&mut output, 0);
        output
    }

    fn modern_avatar_object(endian: TestEndian) -> Vec<u8> {
        let mut output = named_avatar(endian, "modern avatar", false, 22);
        endian.push_u32(&mut output, 0xDEAD_BEEF);
        push_modern_constant(&mut output, endian);
        endian.push_i32(&mut output, 3);
        for (hash, path) in [(7, "first/path"), (7, "second/path"), (9, "Root/Hips")] {
            endian.push_u32(&mut output, hash);
            push_aligned_string(&mut output, endian, path);
        }
        push_human_description(&mut output, endian);
        output
    }

    fn absolute_alignment_avatar_object(endian: TestEndian) -> Vec<u8> {
        let mut output = named_avatar(endian, "", false, 22);
        endian.push_u32(&mut output, 0);
        push_skeleton(&mut output, endian, false);
        push_pose(&mut output, endian, 0);
        push_pose(&mut output, endian, 0);
        push_u32_array(&mut output, endian, &[]);
        push_xform(&mut output, endian, 0.0);
        push_skeleton(&mut output, endian, false);
        push_pose(&mut output, endian, 0);
        push_i32_array(&mut output, endian, &[]);
        push_i32_array(&mut output, endian, &[]);
        push_i32_array(&mut output, endian, &[]);
        push_f32_array(&mut output, endian, &[]);
        for value in [1.0_f32, 0.5, 0.5, 0.5, 0.5, 0.05, 0.05, 0.0] {
            endian.push_f32(&mut output, value);
        }
        output.extend_from_slice(&[0, 0, 0]);
        align_with_base(&mut output, 1, 4);
        push_i32_array(&mut output, endian, &[]);
        push_i32_array(&mut output, endian, &[]);
        endian.push_i32(&mut output, -1);
        push_xform(&mut output, endian, 0.0);
        push_skeleton(&mut output, endian, false);
        push_pose(&mut output, endian, 0);
        push_i32_array(&mut output, endian, &[]);
        endian.push_i32(&mut output, 0);
        output
    }

    fn push_modern_constant(output: &mut Vec<u8>, endian: TestEndian) {
        push_skeleton(output, endian, true);
        push_pose(output, endian, 1);
        push_pose(output, endian, 1);
        push_u32_array(output, endian, &[11, 22]);
        push_xform(output, endian, 1.0);
        push_skeleton(output, endian, true);
        push_pose(output, endian, 1);
        push_i32_array(output, endian, &[1, 2, 3]);
        push_i32_array(output, endian, &[4, 5]);
        push_i32_array(output, endian, &[6, 7]);
        push_f32_array(output, endian, &[0.1, 0.2]);
        for value in [1.0_f32, 0.5, 0.5, 0.5, 0.5, 0.05, 0.05, 0.0] {
            endian.push_f32(output, value);
        }
        output.extend_from_slice(&[1, 1, 0]);
        align(output, 4);
        push_i32_array(output, endian, &[0, 1]);
        push_i32_array(output, endian, &[-1, 0, 1]);
        endian.push_i32(output, -1);
        push_xform(output, endian, 2.0);
        push_skeleton(output, endian, false);
        push_pose(output, endian, 1);
        push_i32_array(output, endian, &[2]);
    }

    fn push_human_description(output: &mut Vec<u8>, endian: TestEndian) {
        endian.push_i32(output, 1);
        push_aligned_string(output, endian, "mixamorig:Hips");
        push_aligned_string(output, endian, "Hips");
        push_vector3(output, endian, [-1.0, -2.0, -3.0]);
        push_vector3(output, endian, [1.0, 2.0, 3.0]);
        push_vector3(output, endian, [0.1, 0.2, 0.3]);
        endian.push_f32(output, 0.75);
        output.push(1);
        align(output, 4);
        endian.push_i32(output, 1);
        push_aligned_string(output, endian, "mixamorig:Hips");
        push_aligned_string(output, endian, "Armature");
        push_vector3(output, endian, [4.0, 5.0, 6.0]);
        push_vector4(output, endian, [0.0, 0.0, 0.0, 1.0]);
        push_vector3(output, endian, [1.0, 1.0, 1.0]);
        for value in [0.5_f32, 0.6, 0.7, 0.8, 0.05, 0.06, 0.01, 2.0] {
            endian.push_f32(output, value);
        }
        push_aligned_string(output, endian, "Hips");
        output.extend_from_slice(&[1, 1, 1]);
        align(output, 4);
    }

    fn push_skeleton(output: &mut Vec<u8>, endian: TestEndian, populated: bool) {
        if populated {
            endian.push_i32(output, 1);
            endian.push_i32(output, -1);
            endian.push_i32(output, 0);
            push_u32_array(output, endian, &[0x1122_3344]);
            endian.push_i32(output, 1);
            for value in 0..8 {
                endian.push_f32(output, f32::from(u16::try_from(value).unwrap()) + 0.25);
            }
            push_vector3(output, endian, [1.0, -1.0, 1.0]);
            push_vector3(output, endian, [-2.0, -3.0, -4.0]);
            push_vector3(output, endian, [2.0, 3.0, 4.0]);
            endian.push_f32(output, 7.5);
            endian.push_u32(output, 12);
        } else {
            endian.push_i32(output, 0);
            push_u32_array(output, endian, &[]);
            endian.push_i32(output, 0);
        }
    }

    fn push_pose(output: &mut Vec<u8>, endian: TestEndian, count: usize) {
        endian.push_i32(output, i32::try_from(count).unwrap());
        for index in 0..count {
            push_xform(
                output,
                endian,
                f32::from(u16::try_from(index).unwrap()) + 1.0,
            );
        }
    }

    fn push_xform(output: &mut Vec<u8>, endian: TestEndian, seed: f32) {
        push_vector3(output, endian, [seed, seed + 1.0, seed + 2.0]);
        push_vector4(output, endian, [seed + 3.0, seed + 4.0, seed + 5.0, 1.0]);
        push_vector3(output, endian, [1.0, 1.0, 1.0]);
    }

    fn push_vector3(output: &mut Vec<u8>, endian: TestEndian, values: [f32; 3]) {
        for value in values {
            endian.push_f32(output, value);
        }
    }

    fn push_vector4(output: &mut Vec<u8>, endian: TestEndian, values: [f32; 4]) {
        for value in values {
            endian.push_f32(output, value);
        }
    }

    fn push_i32_array(output: &mut Vec<u8>, endian: TestEndian, values: &[i32]) {
        endian.push_i32(output, i32::try_from(values.len()).unwrap());
        for value in values {
            endian.push_i32(output, *value);
        }
    }

    fn push_u32_array(output: &mut Vec<u8>, endian: TestEndian, values: &[u32]) {
        endian.push_i32(output, i32::try_from(values.len()).unwrap());
        for value in values {
            endian.push_u32(output, *value);
        }
    }

    fn push_f32_array(output: &mut Vec<u8>, endian: TestEndian, values: &[f32]) {
        endian.push_i32(output, i32::try_from(values.len()).unwrap());
        for value in values {
            endian.push_f32(output, *value);
        }
    }

    fn named_avatar(
        endian: TestEndian,
        name: &str,
        no_target: bool,
        format_version: u32,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        if no_target {
            endian.push_u32(&mut output, 0x5566_7788);
            push_pptr(&mut output, endian, 1, 101, format_version);
            push_pptr(&mut output, endian, 2, 202, format_version);
        }
        push_aligned_string(&mut output, endian, name);
        output
    }

    fn negative_count_object(endian: TestEndian) -> Vec<u8> {
        let mut output = named_avatar(endian, "negative", false, 22);
        endian.push_u32(&mut output, 0);
        endian.push_i32(&mut output, -1);
        output
    }

    fn push_pptr(
        output: &mut Vec<u8>,
        endian: TestEndian,
        file_id: i32,
        path_id: i64,
        format_version: u32,
    ) {
        endian.push_i32(output, file_id);
        if format_version < 14 {
            endian.push_i32(output, i32::try_from(path_id).unwrap());
        } else {
            endian.push_i64(output, path_id);
        }
    }

    fn push_aligned_string(output: &mut Vec<u8>, endian: TestEndian, value: &str) {
        endian.push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
        }
    }

    fn parse_asset(
        format_version: u32,
        endian: TestEndian,
        target_platform: i32,
        unity_version: &str,
        object: &[u8],
        class_id: i32,
    ) -> SerializedFile {
        parse_asset_at_offset(
            format_version,
            endian,
            target_platform,
            unity_version,
            object,
            class_id,
            0,
        )
    }

    fn parse_asset_at_offset(
        format_version: u32,
        endian: TestEndian,
        target_platform: i32,
        unity_version: &str,
        object: &[u8],
        class_id: i32,
        object_offset: usize,
    ) -> SerializedFile {
        let bytes = synthetic_asset(
            format_version,
            endian,
            target_platform,
            unity_version,
            object,
            class_id,
            object_offset,
        );
        SerializedFile::open(Region::from_bytes(bytes)).unwrap()
    }

    #[allow(clippy::too_many_lines)]
    fn synthetic_asset(
        format_version: u32,
        endian: TestEndian,
        target_platform: i32,
        unity_version: &str,
        object: &[u8],
        class_id: i32,
        object_offset: usize,
    ) -> Vec<u8> {
        assert!(matches!(format_version, 13 | 22));
        let metadata_base = if format_version == 22 { 48 } else { 20 };
        let mut metadata = Vec::new();
        metadata.extend_from_slice(unity_version.as_bytes());
        metadata.push(0);
        endian.push_i32(&mut metadata, target_platform);
        metadata.push(0);
        endian.push_i32(&mut metadata, 1);
        endian.push_i32(&mut metadata, class_id);
        if format_version == 22 {
            metadata.push(0);
            endian.push_i16(&mut metadata, -1);
        }
        metadata.extend_from_slice(&[0_u8; 16]);
        if format_version == 13 {
            endian.push_i32(&mut metadata, 0);
        }
        endian.push_i32(&mut metadata, 1);
        if format_version == 22 {
            align_with_base(&mut metadata, metadata_base, 4);
            endian.push_i64(&mut metadata, 7);
            endian.push_i64(&mut metadata, i64::try_from(object_offset).unwrap());
        } else {
            endian.push_i32(&mut metadata, 7);
            endian.push_u32(&mut metadata, u32::try_from(object_offset).unwrap());
        }
        endian.push_u32(&mut metadata, u32::try_from(object.len()).unwrap());
        if format_version == 22 {
            endian.push_i32(&mut metadata, 0);
        } else {
            endian.push_i32(&mut metadata, class_id);
            endian.push_u16(&mut metadata, u16::try_from(class_id).unwrap());
            endian.push_i16(&mut metadata, -1);
        }
        endian.push_i32(&mut metadata, 0);
        endian.push_i32(&mut metadata, 0);
        if format_version == 22 {
            endian.push_i32(&mut metadata, 0);
        }
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (metadata_base + metadata.len()).next_multiple_of(16);
        let file_size = data_offset + object_offset + object.len();
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
        bytes.resize(data_offset + object_offset, 0);
        bytes.extend_from_slice(object);
        bytes
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

        fn push_i16(self, output: &mut Vec<u8>, value: i16) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_u16(self, output: &mut Vec<u8>, value: u16) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_i32(self, output: &mut Vec<u8>, value: i32) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_u32(self, output: &mut Vec<u8>, value: u32) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_i64(self, output: &mut Vec<u8>, value: i64) {
            match self {
                Self::Little => output.extend_from_slice(&value.to_le_bytes()),
                Self::Big => output.extend_from_slice(&value.to_be_bytes()),
            }
        }

        fn push_f32(self, output: &mut Vec<u8>, value: f32) {
            self.push_u32(output, value.to_bits());
        }
    }
}
