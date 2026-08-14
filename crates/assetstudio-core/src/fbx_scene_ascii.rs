//! General, deterministic static ASCII FBX 7.4 scene emission.

use std::fmt;
use std::io::{self, Write};

use crate::material::Material;
use crate::mesh::{Mesh, MeshBlendShapeFrame, MeshBlendShapeVertex};
use crate::model_animation::{
    ModelAnimationSet, ModelAnimationTrack, ModelBlendShapeTrack, ModelScalarKeyframe,
    ModelVectorKeyframe, unity_crc32,
};
use crate::model_ir::{ModelCoordinateConvention, ModelIr, ModelLocalTransform, ModelRendererKind};
use crate::scene_hierarchy::SceneObjectKey;
use crate::{Error, Result};

const MODEL_ID_BASE: i64 = 1_000_000_000;
const GEOMETRY_ID_BASE: i64 = 2_000_000_000;
const MATERIAL_ID_BASE: i64 = 3_000_000_000;
const DEFAULT_MATERIAL_ID: i64 = 3_999_999_999;
const SKIN_ID_BASE: i64 = 4_000_000_000;
const CLUSTER_ID_BASE: i64 = 5_000_000_000;
const ANIMATION_STACK_ID_BASE: i64 = 6_000_000_000;
const ANIMATION_LAYER_ID_BASE: i64 = 7_000_000_000;
const ANIMATION_CURVE_NODE_ID_BASE: i64 = 8_000_000_000;
const ANIMATION_CURVE_ID_BASE: i64 = 9_000_000_000;
const BLEND_SHAPE_ID_BASE: i64 = 10_000_000_000;
const BLEND_SHAPE_CHANNEL_ID_BASE: i64 = 11_000_000_000;
const SHAPE_GEOMETRY_ID_BASE: i64 = 12_000_000_000;
const FBX_TIME_SECOND: f64 = 46_186_158_000.0;
const MAXIMUM_NAME_BYTES: usize = 4 * 1024;
const MAXIMUM_BONE_PATH_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_BONE_PATH_HASHES: usize = 10_000_000;

pub(crate) fn write_model_ir_fbx_ascii<W: Write>(
    model: &ModelIr,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    write_model_ir_fbx_ascii_with_animations(model, None, output, maximum_output_bytes)
}

pub(crate) fn write_model_ir_fbx_ascii_with_animations<W: Write>(
    model: &ModelIr,
    animations: Option<&ModelAnimationSet>,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    let scene = StaticScene::from_model(model, animations)?;
    let mut bounded = BoundedWriter::new(output, maximum_output_bytes);
    let result = scene.write(&mut bounded);
    if bounded.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "ASCII FBX exceeds the {maximum_output_bytes} byte output limit"
        )));
    }
    result?;
    Ok(bounded.written)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MaterialReference {
    Default,
    Model(usize),
}

struct StaticScene<'a> {
    nodes: Vec<NodePlan<'a>>,
    geometries: Vec<GeometryPlan<'a>>,
    materials: Vec<MaterialPlan<'a>>,
    animations: Vec<AnimationPlan<'a>>,
}

struct NodePlan<'a> {
    id: i64,
    parent_id: i64,
    name: &'a str,
    transform: ConvertedTransform,
    has_geometry: bool,
    is_bone: bool,
}

struct GeometryPlan<'a> {
    id: i64,
    model_id: i64,
    mesh: &'a Mesh,
    material_ids: Vec<i64>,
    submesh_material_slots: Vec<usize>,
    skin: Option<SkinPlan<'a>>,
    morph: Option<MorphPlan<'a>>,
}

struct MorphPlan<'a> {
    id: i64,
    name: &'a str,
    channels: Vec<MorphChannelPlan<'a>>,
}

struct MorphChannelPlan<'a> {
    id: i64,
    name: &'a str,
    full_weights: &'a [f32],
    shapes: Vec<ShapePlan<'a>>,
}

struct ShapePlan<'a> {
    id: i64,
    name: String,
    frame: &'a MeshBlendShapeFrame,
    vertices: &'a [MeshBlendShapeVertex],
}

struct SkinPlan<'a> {
    id: i64,
    name: &'a str,
    clusters: Vec<ClusterPlan<'a>>,
}

struct ClusterPlan<'a> {
    id: i64,
    bone_model_id: i64,
    bone_name: &'a str,
    indices: Vec<u32>,
    weights: Vec<f32>,
    transform: Matrix4,
    transform_link: Matrix4,
}

struct AnimationPlan<'a> {
    stack_id: i64,
    layer_id: i64,
    name: &'a str,
    stop_time: i64,
    properties: Vec<AnimationPropertyPlan<'a>>,
    blend_shapes: Vec<AnimationBlendShapePlan<'a>>,
}

struct AnimationPropertyPlan<'a> {
    node_id: i64,
    model_id: i64,
    kind: AnimationProperty,
    curve_ids: [i64; 3],
    keys: &'a [ModelVectorKeyframe],
}

struct AnimationBlendShapePlan<'a> {
    node_id: i64,
    curve_id: i64,
    channel_id: i64,
    channel_name: &'a str,
    keys: &'a [ModelScalarKeyframe],
}

#[derive(Clone, Copy)]
enum AnimationProperty {
    Translation,
    Rotation,
    Scaling,
}

struct MaterialPlan<'a> {
    id: i64,
    material: Option<&'a Material>,
}

#[derive(Clone, Copy)]
struct ConvertedTransform {
    translation: [f32; 3],
    rotation: [f32; 3],
    scale: [f32; 3],
}

impl<'a> StaticScene<'a> {
    fn from_model(model: &'a ModelIr, animations: Option<&'a ModelAnimationSet>) -> Result<Self> {
        if model.coordinate_convention != ModelCoordinateConvention::UnitySource {
            return Err(Error::unsupported(
                "ASCII FBX input must use the UnitySource coordinate convention",
            ));
        }
        let mut nodes = reserve_vec(model.nodes.len(), "FBX scene nodes")?;
        let mut geometries = Vec::new();
        let mut used_materials = Vec::new();
        let global_matrices = build_global_matrices(model)?;
        let transform_nodes = build_transform_node_index(model)?;
        let bone_hash_nodes = build_bone_hash_node_index(model)?;
        let mut bone_node_indices = Vec::new();
        let mut next_cluster_index = 0_usize;
        let mut next_blend_shape_channel_index = 0_usize;
        let mut next_shape_geometry_index = 0_usize;
        let mut node_plan_indices = reserve_vec(model.nodes.len(), "FBX node-plan index")?;
        node_plan_indices.resize(model.nodes.len(), None);

        for (node_index, node) in model.nodes.iter().enumerate() {
            let Some(transform) = node.transform else {
                if node
                    .renderers
                    .iter()
                    .any(|renderer| renderer.mesh.is_some())
                {
                    return Err(Error::unsupported(format!(
                        "FBX node {:?} has renderable geometry but no Transform",
                        node.object
                    )));
                }
                continue;
            };
            validate_name(&node.name, "node")?;
            let id = indexed_id(MODEL_ID_BASE, node_index, "FBX model")?;
            let parent_id = node
                .parent
                .and_then(|parent| model.node_index(parent))
                .and_then(|parent_index| {
                    model
                        .nodes
                        .get(parent_index)
                        .and_then(|parent| parent.transform.map(|_| parent_index))
                })
                .map_or(Ok(0), |parent_index| {
                    indexed_id(MODEL_ID_BASE, parent_index, "FBX parent model")
                })?;
            let geometry_before = geometries.len();
            let mut context = GeometryBuildContext {
                model,
                global_matrices: &global_matrices,
                transform_nodes: &transform_nodes,
                bone_hash_nodes: &bone_hash_nodes,
                geometries: &mut geometries,
                used_materials: &mut used_materials,
                bone_node_indices: &mut bone_node_indices,
                next_cluster_index: &mut next_cluster_index,
                next_blend_shape_channel_index: &mut next_blend_shape_channel_index,
                next_shape_geometry_index: &mut next_shape_geometry_index,
            };
            bind_node_geometry(&mut context, node_index, id, &node.renderers)?;
            node_plan_indices[node_index] = Some(nodes.len());
            nodes.push(NodePlan {
                id,
                parent_id,
                name: &node.name,
                transform: ConvertedTransform::from_unity(transform)?,
                has_geometry: geometries.len() != geometry_before,
                is_bone: false,
            });
        }
        if nodes.is_empty() {
            return Err(Error::unsupported(
                "ASCII FBX scene contains no GameObject with a Transform",
            ));
        }
        used_materials.sort_unstable();
        used_materials.dedup();
        bone_node_indices.sort_unstable();
        bone_node_indices.dedup();
        mark_bone_nodes(&mut nodes, &node_plan_indices, bone_node_indices)?;
        let materials = build_material_plans(model, &used_materials)?;
        let animations = animations.map_or_else(
            || Ok(Vec::new()),
            |animations| build_animation_plans(model, &geometries, animations),
        )?;
        Ok(Self {
            nodes,
            geometries,
            materials,
            animations,
        })
    }

    fn write(&self, output: &mut impl Write) -> io::Result<()> {
        self.write_header(output)?;
        for node in &self.nodes {
            write_model(node, output)?;
        }
        for geometry in &self.geometries {
            write_geometry(geometry, output)?;
            if let Some(morph) = &geometry.morph {
                write_shape_geometries(geometry.mesh, morph, output)?;
            }
        }
        for material in &self.materials {
            write_material(material, output)?;
        }
        for geometry in &self.geometries {
            if let Some(skin) = &geometry.skin {
                write_skin(skin, output)?;
            }
            if let Some(morph) = &geometry.morph {
                write_morph(morph, output)?;
            }
        }
        for animation in &self.animations {
            write_animation(animation, output)?;
        }
        output.write_all(b"}\nConnections:  {\n")?;
        for node in &self.nodes {
            writeln!(output, "    C: \"OO\",{},{}", node.id, node.parent_id)?;
        }
        for geometry in &self.geometries {
            writeln!(
                output,
                "    C: \"OO\",{},{}",
                geometry.id, geometry.model_id
            )?;
            for material_id in &geometry.material_ids {
                writeln!(output, "    C: \"OO\",{material_id},{}", geometry.model_id)?;
            }
            if let Some(skin) = &geometry.skin {
                writeln!(output, "    C: \"OO\",{},{}", skin.id, geometry.id)?;
                for cluster in &skin.clusters {
                    writeln!(output, "    C: \"OO\",{},{}", cluster.id, skin.id)?;
                    writeln!(
                        output,
                        "    C: \"OO\",{},{}",
                        cluster.bone_model_id, cluster.id
                    )?;
                }
            }
            if let Some(morph) = &geometry.morph {
                writeln!(output, "    C: \"OO\",{},{}", morph.id, geometry.id)?;
                for channel in &morph.channels {
                    writeln!(output, "    C: \"OO\",{},{}", channel.id, morph.id)?;
                    for shape in &channel.shapes {
                        writeln!(output, "    C: \"OO\",{},{}", shape.id, channel.id)?;
                    }
                }
            }
        }
        for animation in &self.animations {
            write_animation_connections(animation, output)?;
        }
        output.write_all(b"}\nTakes:  {\n    Current: \"\"\n}\n")
    }

    fn write_header(&self, output: &mut impl Write) -> io::Result<()> {
        let counts = self.definition_counts()?;
        write!(
            output,
            "; FBX 7.4.0 project file\n\
FBXHeaderExtension:  {{\n\
    FBXHeaderVersion: 1003\n\
    FBXVersion: 7400\n\
    EncryptionType: 0\n\
    Creator: \"AssetStudio Rust static ASCII FBX writer\"\n\
}}\n\
GlobalSettings:  {{\n\
    Version: 1000\n\
    Properties70:  {{\n\
        P: \"UpAxis\", \"int\", \"Integer\", \"\",1\n\
        P: \"UpAxisSign\", \"int\", \"Integer\", \"\",1\n\
        P: \"FrontAxis\", \"int\", \"Integer\", \"\",2\n\
        P: \"FrontAxisSign\", \"int\", \"Integer\", \"\",-1\n\
        P: \"CoordAxis\", \"int\", \"Integer\", \"\",0\n\
        P: \"CoordAxisSign\", \"int\", \"Integer\", \"\",1\n\
        P: \"UnitScaleFactor\", \"double\", \"Number\", \"\",1\n\
    }}\n\
}}\n\
Documents:  {{\n\
    Count: 1\n\
    Document: 1, \"Scene\", \"Scene\" {{\n\
        RootNode: 0\n\
    }}\n\
}}\n\
References:  {{\n\
}}\n\
Definitions:  {{\n\
    Version: 100\n\
    Count: 8\n\
    ObjectType: \"Model\" {{ Count: {} }}\n\
    ObjectType: \"Geometry\" {{ Count: {} }}\n\
    ObjectType: \"Material\" {{ Count: {} }}\n\
    ObjectType: \"Deformer\" {{ Count: {} }}\n\
    ObjectType: \"AnimationStack\" {{ Count: {} }}\n\
    ObjectType: \"AnimationLayer\" {{ Count: {} }}\n\
    ObjectType: \"AnimationCurveNode\" {{ Count: {} }}\n\
    ObjectType: \"AnimationCurve\" {{ Count: {} }}\n\
}}\n\
Objects:  {{\n",
            self.nodes.len(),
            counts.geometries,
            self.materials.len(),
            counts.deformers,
            self.animations.len(),
            self.animations.len(),
            counts.curve_nodes,
            counts.curves
        )
    }

    fn definition_counts(&self) -> io::Result<DefinitionCounts> {
        let mut counts = DefinitionCounts {
            geometries: self.geometries.len(),
            deformers: 0,
            curve_nodes: 0,
            curves: 0,
        };
        for geometry in &self.geometries {
            if let Some(skin) = &geometry.skin {
                counts.deformers = checked_count_add(counts.deformers, 1, "deformer")?;
                counts.deformers =
                    checked_count_add(counts.deformers, skin.clusters.len(), "deformer")?;
            }
            if let Some(morph) = &geometry.morph {
                counts.deformers = checked_count_add(counts.deformers, 1, "deformer")?;
                counts.deformers =
                    checked_count_add(counts.deformers, morph.channels.len(), "deformer")?;
                for channel in &morph.channels {
                    counts.geometries = checked_count_add(
                        counts.geometries,
                        channel.shapes.len(),
                        "shape geometry",
                    )?;
                }
            }
        }
        for animation in &self.animations {
            let property_count = checked_count_add(
                animation.properties.len(),
                animation.blend_shapes.len(),
                "curve-node",
            )?;
            counts.curve_nodes =
                checked_count_add(counts.curve_nodes, property_count, "curve-node")?;
            let vector_curves = animation
                .properties
                .len()
                .checked_mul(3)
                .ok_or_else(|| io::Error::other("FBX animation curve count overflowed"))?;
            let curve_count = checked_count_add(
                vector_curves,
                animation.blend_shapes.len(),
                "animation curve",
            )?;
            counts.curves = checked_count_add(counts.curves, curve_count, "animation curve")?;
        }
        Ok(counts)
    }
}

struct DefinitionCounts {
    geometries: usize,
    deformers: usize,
    curve_nodes: usize,
    curves: usize,
}

fn checked_count_add(left: usize, right: usize, field: &str) -> io::Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| io::Error::other(format!("FBX {field} count overflowed")))
}

fn mark_bone_nodes(
    nodes: &mut [NodePlan<'_>],
    node_plan_indices: &[Option<usize>],
    bone_node_indices: Vec<usize>,
) -> Result<()> {
    for model_node_index in bone_node_indices {
        let plan_index = node_plan_indices
            .get(model_node_index)
            .copied()
            .flatten()
            .ok_or_else(|| Error::invalid_data("FBX bone has no Transform-backed node"))?;
        let node = nodes
            .get_mut(plan_index)
            .ok_or_else(|| Error::invalid_data("FBX bone node index is outside the scene"))?;
        if node.has_geometry {
            return Err(Error::unsupported(
                "a single FBX node cannot be both a skeleton limb and a mesh node",
            ));
        }
        node.is_bone = true;
    }
    Ok(())
}

struct GeometryBuildContext<'a, 'b> {
    model: &'a ModelIr,
    global_matrices: &'b [Option<Matrix4>],
    transform_nodes: &'b [(SceneObjectKey, usize)],
    bone_hash_nodes: &'b [BoneHashNode],
    geometries: &'b mut Vec<GeometryPlan<'a>>,
    used_materials: &'b mut Vec<MaterialReference>,
    bone_node_indices: &'b mut Vec<usize>,
    next_cluster_index: &'b mut usize,
    next_blend_shape_channel_index: &'b mut usize,
    next_shape_geometry_index: &'b mut usize,
}

fn bind_node_geometry(
    context: &mut GeometryBuildContext<'_, '_>,
    node_index: usize,
    model_id: i64,
    renderers: &[crate::model_ir::ModelRendererBinding],
) -> Result<()> {
    let mut bound_geometry = false;
    for renderer in renderers {
        let Some(mesh_key) = renderer.mesh else {
            continue;
        };
        if bound_geometry {
            return Err(Error::unsupported(format!(
                "FBX node index {node_index} has more than one renderable static MeshRenderer"
            )));
        }
        let mesh = &context
            .model
            .mesh(mesh_key)
            .ok_or_else(|| Error::invalid_data("renderer Mesh is absent from the model IR"))?
            .mesh;
        validate_mesh(mesh)?;
        validate_name(&mesh.name, "mesh")?;
        let (material_ids, submesh_material_slots, references) =
            geometry_materials(context.model, renderer, mesh.sub_meshes.len())?;
        context
            .used_materials
            .try_reserve(references.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot grow used FBX materials: {error}"))
            })?;
        context.used_materials.extend(references);
        let id = indexed_id(GEOMETRY_ID_BASE, context.geometries.len(), "FBX geometry")?;
        let skin = match &renderer.kind {
            ModelRendererKind::MeshRenderer { .. } => None,
            ModelRendererKind::SkinnedMeshRenderer { bones } => {
                let mut context = SkinBuildContext {
                    model: context.model,
                    global_matrices: context.global_matrices,
                    transform_nodes: context.transform_nodes,
                    bone_hash_nodes: context.bone_hash_nodes,
                    bone_node_indices: context.bone_node_indices,
                    next_cluster_index: context.next_cluster_index,
                };
                Some(build_skin_plan(&mut context, node_index, id, mesh, bones)?)
            }
        };
        let morph = build_morph_plan(
            mesh,
            context.geometries.len(),
            context.next_blend_shape_channel_index,
            context.next_shape_geometry_index,
        )?;
        context
            .geometries
            .try_reserve(1)
            .map_err(|error| Error::invalid_data(format!("cannot grow FBX geometries: {error}")))?;
        context.geometries.push(GeometryPlan {
            id,
            model_id,
            mesh,
            material_ids,
            submesh_material_slots,
            skin,
            morph,
        });
        bound_geometry = true;
    }
    Ok(())
}

fn build_morph_plan<'a>(
    mesh: &'a Mesh,
    geometry_index: usize,
    next_channel_index: &mut usize,
    next_shape_index: &mut usize,
) -> Result<Option<MorphPlan<'a>>> {
    let Some(blend_shapes) = &mesh.blend_shapes else {
        return Ok(None);
    };
    if blend_shapes.channels.is_empty() {
        return Ok(None);
    }
    let mut channels = reserve_vec(blend_shapes.channels.len(), "FBX blend-shape channels")?;
    for channel in &blend_shapes.channels {
        let name = channel.name.rsplit('.').next().unwrap_or(&channel.name);
        validate_name(name, "blend-shape channel")?;
        let frame_end = channel
            .frame_index
            .checked_add(channel.frame_count)
            .ok_or_else(|| Error::invalid_data("FBX blend-shape frame range overflowed"))?;
        let frames = blend_shapes
            .frames
            .get(channel.frame_index..frame_end)
            .ok_or_else(|| Error::invalid_data("FBX blend-shape frame range is invalid"))?;
        let full_weights = blend_shapes
            .full_weights
            .get(channel.frame_index..frame_end)
            .ok_or_else(|| Error::invalid_data("FBX blend-shape weight range is invalid"))?;
        let mut shapes = reserve_vec(frames.len(), "FBX target shapes")?;
        for (frame_offset, frame) in frames.iter().enumerate() {
            let vertex_start = usize::try_from(frame.first_vertex).map_err(|_| {
                Error::invalid_data("FBX blend-shape first vertex does not fit usize")
            })?;
            let vertex_count = usize::try_from(frame.vertex_count).map_err(|_| {
                Error::invalid_data("FBX blend-shape vertex count does not fit usize")
            })?;
            let vertex_end = vertex_start
                .checked_add(vertex_count)
                .ok_or_else(|| Error::invalid_data("FBX blend-shape vertex range overflowed"))?;
            let vertices = blend_shapes
                .vertices
                .get(vertex_start..vertex_end)
                .ok_or_else(|| Error::invalid_data("FBX blend-shape vertex range is invalid"))?;
            let shape_name = target_shape_name(name, frame_offset)?;
            shapes.push(ShapePlan {
                id: indexed_id(
                    SHAPE_GEOMETRY_ID_BASE,
                    *next_shape_index,
                    "FBX target shape",
                )?,
                name: shape_name,
                frame,
                vertices,
            });
            *next_shape_index = next_shape_index
                .checked_add(1)
                .ok_or_else(|| Error::invalid_data("FBX target-shape count overflowed"))?;
        }
        channels.push(MorphChannelPlan {
            id: indexed_id(
                BLEND_SHAPE_CHANNEL_ID_BASE,
                *next_channel_index,
                "FBX blend-shape channel",
            )?,
            name,
            full_weights,
            shapes,
        });
        *next_channel_index = next_channel_index
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("FBX blend-shape channel count overflowed"))?;
    }
    Ok(Some(MorphPlan {
        id: indexed_id(BLEND_SHAPE_ID_BASE, geometry_index, "FBX blend shape")?,
        name: &mesh.name,
        channels,
    }))
}

fn target_shape_name(channel_name: &str, frame_offset: usize) -> Result<String> {
    if frame_offset == 0 {
        let mut name = String::new();
        name.try_reserve(channel_name.len()).map_err(|error| {
            Error::invalid_data(format!("cannot allocate FBX target-shape name: {error}"))
        })?;
        name.push_str(channel_name);
        return Ok(name);
    }
    let suffix = frame_offset
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("FBX target-shape name index overflowed"))?
        .to_string();
    let capacity = channel_name
        .len()
        .checked_add(1)
        .and_then(|length| length.checked_add(suffix.len()))
        .ok_or_else(|| Error::invalid_data("FBX target-shape name length overflowed"))?;
    if capacity > MAXIMUM_NAME_BYTES {
        return Err(Error::invalid_data(format!(
            "FBX target-shape name is {capacity} bytes, exceeding limit {MAXIMUM_NAME_BYTES}"
        )));
    }
    let mut name = String::new();
    name.try_reserve(capacity).map_err(|error| {
        Error::invalid_data(format!("cannot allocate FBX target-shape name: {error}"))
    })?;
    name.push_str(channel_name);
    name.push('_');
    name.push_str(&suffix);
    Ok(name)
}

struct SkinBuildContext<'a, 'b> {
    model: &'a ModelIr,
    global_matrices: &'b [Option<Matrix4>],
    transform_nodes: &'b [(SceneObjectKey, usize)],
    bone_hash_nodes: &'b [BoneHashNode],
    bone_node_indices: &'b mut Vec<usize>,
    next_cluster_index: &'b mut usize,
}

fn build_skin_plan<'a>(
    context: &mut SkinBuildContext<'a, '_>,
    mesh_node_index: usize,
    geometry_id: i64,
    mesh: &'a Mesh,
    bones: &[Option<SceneObjectKey>],
) -> Result<SkinPlan<'a>> {
    let skin = validate_skin_source(mesh)?;
    let resolved = resolve_skin_bones(context, mesh, bones)?;
    let influences = build_bone_influences(skin, &resolved)?;
    let mesh_global = context
        .global_matrices
        .get(mesh_node_index)
        .copied()
        .flatten()
        .ok_or_else(|| Error::invalid_data("skinned FBX mesh node has no global Transform"))?;
    let clusters = build_skin_clusters(context, mesh, resolved, &influences, mesh_global)?;
    let geometry_index = usize::try_from(geometry_id - GEOMETRY_ID_BASE)
        .map_err(|_| Error::invalid_data("FBX geometry ID cannot identify a skin"))?;
    Ok(SkinPlan {
        id: indexed_id(SKIN_ID_BASE, geometry_index, "FBX skin")?,
        name: &mesh.name,
        clusters,
    })
}

type ResolvedBone<'a> = Option<(usize, &'a crate::model_ir::ModelNode)>;

fn validate_skin_source(mesh: &Mesh) -> Result<&[crate::mesh::MeshBoneWeight]> {
    let skin = mesh.skin.as_deref().ok_or_else(|| {
        Error::unsupported("SkinnedMeshRenderer Mesh has no decoded vertex skin weights")
    })?;
    if skin.len() != mesh.vertices.len() {
        return Err(Error::invalid_data(format!(
            "skinned Mesh has {} skin records for {} vertices",
            skin.len(),
            mesh.vertices.len()
        )));
    }
    if mesh.bind_poses.is_empty() {
        return Err(Error::unsupported("SkinnedMeshRenderer has no bind poses"));
    }
    Ok(skin)
}

fn resolve_skin_bones<'a>(
    context: &SkinBuildContext<'a, '_>,
    mesh: &Mesh,
    bones: &[Option<SceneObjectKey>],
) -> Result<Vec<ResolvedBone<'a>>> {
    let direct = resolve_direct_bones(context, bones, mesh.bind_poses.len())?;
    let hashed = resolve_hashed_bones(context, mesh)?;
    let direct_count = direct.iter().filter(|bone| bone.is_some()).count();
    let hashed_count = hashed.iter().filter(|bone| bone.is_some()).count();
    if hashed_count > direct_count {
        return Ok(hashed);
    }
    if direct_count > 0 {
        return Ok(direct);
    }
    if hashed_count > 0 {
        return Ok(hashed);
    }
    Err(Error::unsupported(
        "neither SkinnedMeshRenderer direct bones nor Mesh bone-name hashes resolve",
    ))
}

fn resolve_direct_bones<'a>(
    context: &SkinBuildContext<'a, '_>,
    bones: &[Option<SceneObjectKey>],
    expected: usize,
) -> Result<Vec<ResolvedBone<'a>>> {
    let mut resolved = reserve_vec(expected, "FBX direct resolved bones")?;
    resolved.resize(expected, None);
    if bones.len() != expected {
        return Ok(resolved);
    }
    for (slot, bone) in resolved.iter_mut().zip(bones) {
        *slot = bone.and_then(|component| resolve_transform_component(context, component));
    }
    Ok(resolved)
}

fn resolve_hashed_bones<'a>(
    context: &SkinBuildContext<'a, '_>,
    mesh: &Mesh,
) -> Result<Vec<ResolvedBone<'a>>> {
    let expected = mesh.bind_poses.len();
    let mut resolved = reserve_vec(expected, "FBX hash-resolved bones")?;
    resolved.resize(expected, None);
    if mesh.bone_name_hashes.len() != expected {
        return Ok(resolved);
    }
    for (slot, hash) in resolved.iter_mut().zip(&mesh.bone_name_hashes) {
        *slot = lookup_bone_hash_node(context.bone_hash_nodes, *hash).and_then(|node_index| {
            context
                .model
                .nodes
                .get(node_index)
                .and_then(|node| node.transform.map(|_| (node_index, node)))
        });
    }
    Ok(resolved)
}

fn resolve_transform_component<'a>(
    context: &SkinBuildContext<'a, '_>,
    component: SceneObjectKey,
) -> Option<(usize, &'a crate::model_ir::ModelNode)> {
    lookup_transform_node(context.transform_nodes, component).and_then(|node_index| {
        let node = context.model.nodes.get(node_index)?;
        node.transform.map(|_| (node_index, node))
    })
}

fn build_bone_influences(
    skin: &[crate::mesh::MeshBoneWeight],
    resolved: &[ResolvedBone<'_>],
) -> Result<Vec<Vec<(u32, f32)>>> {
    let mut counts = reserve_vec(resolved.len(), "FBX bone influence counts")?;
    counts.resize(resolved.len(), 0_usize);
    for weights in skin {
        for (&bone_index, &weight) in weights.bone_indices.iter().zip(&weights.weights) {
            let Ok(index) = usize::try_from(bone_index) else {
                continue;
            };
            if weight > 0.0 && resolved.get(index).is_some_and(Option::is_some) {
                counts[index] = counts[index]
                    .checked_add(1)
                    .ok_or_else(|| Error::invalid_data("FBX bone influence count overflowed"))?;
            }
        }
    }
    let mut influences = reserve_vec(resolved.len(), "FBX bone influences")?;
    for count in counts {
        influences.push(reserve_vec(count, "FBX per-bone influences")?);
    }
    for (vertex_index, weights) in skin.iter().enumerate() {
        let vertex_index = u32::try_from(vertex_index)
            .map_err(|_| Error::invalid_data("FBX vertex index does not fit in u32"))?;
        for (&bone_index, &weight) in weights.bone_indices.iter().zip(&weights.weights) {
            let Ok(index) = usize::try_from(bone_index) else {
                continue;
            };
            if weight > 0.0 && resolved.get(index).is_some_and(Option::is_some) {
                influences[index].push((vertex_index, weight));
            }
        }
    }
    Ok(influences)
}

fn build_skin_clusters<'a>(
    context: &mut SkinBuildContext<'a, '_>,
    mesh: &'a Mesh,
    resolved: Vec<ResolvedBone<'a>>,
    influences: &[Vec<(u32, f32)>],
    mesh_global: Matrix4,
) -> Result<Vec<ClusterPlan<'a>>> {
    let mut clusters = reserve_vec(
        resolved.iter().filter(|bone| bone.is_some()).count(),
        "FBX skin clusters",
    )?;
    for (bone_index, resolved_bone) in resolved.into_iter().enumerate() {
        let Some((bone_node_index, bone_node)) = resolved_bone else {
            continue;
        };
        context
            .bone_node_indices
            .try_reserve(1)
            .map_err(|error| Error::invalid_data(format!("cannot grow FBX bone nodes: {error}")))?;
        context.bone_node_indices.push(bone_node_index);
        let bind_pose = Matrix4::mirrored_bind_pose(
            *mesh
                .bind_poses
                .get(bone_index)
                .ok_or_else(|| Error::invalid_data("FBX bind-pose index is outside the Mesh"))?,
        )?;
        let transform_link = mesh_global.multiply(bind_pose.inverse()?)?;
        let id = indexed_id(CLUSTER_ID_BASE, *context.next_cluster_index, "FBX cluster")?;
        *context.next_cluster_index = context
            .next_cluster_index
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("FBX cluster count overflowed"))?;
        let mut indices = reserve_vec(
            influences[bone_index].len(),
            "FBX cluster control-point indices",
        )?;
        let mut weights = reserve_vec(
            influences[bone_index].len(),
            "FBX cluster control-point weights",
        )?;
        for &(vertex_index, weight) in &influences[bone_index] {
            indices.push(vertex_index);
            weights.push(weight);
        }
        clusters.push(ClusterPlan {
            id,
            bone_model_id: indexed_id(MODEL_ID_BASE, bone_node_index, "FBX bone model")?,
            bone_name: &bone_node.name,
            indices,
            weights,
            transform: mesh_global,
            transform_link,
        });
    }
    Ok(clusters)
}

fn build_transform_node_index(model: &ModelIr) -> Result<Vec<(SceneObjectKey, usize)>> {
    let mut index = reserve_vec(model.nodes.len(), "FBX Transform index")?;
    for (node_index, node) in model.nodes.iter().enumerate() {
        if let Some(transform) = node.transform {
            index.push((transform.component, node_index));
        }
    }
    index.sort_unstable_by_key(|entry| entry.0);
    if index.windows(2).any(|window| window[0].0 == window[1].0) {
        return Err(Error::invalid_data(
            "duplicate Transform identity in FBX model input",
        ));
    }
    Ok(index)
}

fn lookup_transform_node(index: &[(SceneObjectKey, usize)], key: SceneObjectKey) -> Option<usize> {
    index
        .binary_search_by_key(&key, |entry| entry.0)
        .ok()
        .and_then(|position| index.get(position).map(|entry| entry.1))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BoneHashNode {
    hash: u32,
    sequence: usize,
    node_index: usize,
}

fn build_bone_hash_node_index(model: &ModelIr) -> Result<Vec<BoneHashNode>> {
    let mut count = 0_usize;
    let mut total_bytes = 0_usize;
    for node in &model.nodes {
        if node.transform.is_some() {
            let length = model_node_path_length(model, node.object)?;
            total_bytes = total_bytes
                .checked_add(length)
                .ok_or_else(|| Error::invalid_data("FBX bone path byte count overflowed"))?;
            if total_bytes > MAXIMUM_BONE_PATH_BYTES {
                return Err(Error::invalid_data(format!(
                    "FBX bone paths use {total_bytes} bytes, exceeding limit {MAXIMUM_BONE_PATH_BYTES}"
                )));
            }
            let components = node_path_component_count(model, node.object)?;
            count = count
                .checked_add(components)
                .ok_or_else(|| Error::invalid_data("FBX bone path-hash count overflowed"))?;
            if count > MAXIMUM_BONE_PATH_HASHES {
                return Err(Error::invalid_data(format!(
                    "FBX bone path hashes exceed limit {MAXIMUM_BONE_PATH_HASHES}"
                )));
            }
        }
    }
    let mut index = reserve_vec(count, "FBX bone path hashes")?;
    let mut path = String::new();
    for (node_index, node) in model.nodes.iter().enumerate() {
        if node.transform.is_none() {
            continue;
        }
        build_model_node_path(model, node.object, &mut path)?;
        let mut suffix = path.as_str();
        loop {
            index.push(BoneHashNode {
                hash: unity_crc32(suffix.as_bytes()),
                sequence: index.len(),
                node_index,
            });
            let Some(slash) = suffix.find('/') else {
                break;
            };
            suffix = &suffix[slash + 1..];
        }
    }
    index.sort_unstable_by(|left, right| {
        left.hash
            .cmp(&right.hash)
            .then_with(|| right.sequence.cmp(&left.sequence))
    });
    index.dedup_by_key(|entry| entry.hash);
    Ok(index)
}

fn node_path_component_count(model: &ModelIr, mut key: SceneObjectKey) -> Result<usize> {
    let mut count = 0_usize;
    loop {
        let node = model
            .node(key)
            .ok_or_else(|| Error::invalid_data("FBX bone path references a missing node"))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("FBX bone path depth overflowed"))?;
        if count > model.nodes.len() {
            return Err(Error::invalid_data("cycle in FBX bone path"));
        }
        let Some(parent) = node.parent else {
            return Ok(count);
        };
        key = parent;
    }
}

fn model_node_path_length(model: &ModelIr, mut key: SceneObjectKey) -> Result<usize> {
    let mut length = 0_usize;
    let mut depth = 0_usize;
    loop {
        let node = model
            .node(key)
            .ok_or_else(|| Error::invalid_data("FBX bone path references a missing node"))?;
        length = length
            .checked_add(node.name.len())
            .and_then(|length| length.checked_add(usize::from(node.parent.is_some())))
            .ok_or_else(|| Error::invalid_data("FBX bone path length overflowed"))?;
        depth = depth
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("FBX bone path depth overflowed"))?;
        if depth > model.nodes.len() {
            return Err(Error::invalid_data("cycle in FBX bone path"));
        }
        let Some(parent) = node.parent else {
            return Ok(length);
        };
        key = parent;
    }
}

fn build_model_node_path(
    model: &ModelIr,
    mut key: SceneObjectKey,
    output: &mut String,
) -> Result<()> {
    let length = model_node_path_length(model, key)?;
    output.clear();
    output
        .try_reserve(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate FBX bone path: {error}")))?;
    let mut names = reserve_vec(model.nodes.len().min(256), "FBX bone path components")?;
    loop {
        let node = model
            .node(key)
            .ok_or_else(|| Error::invalid_data("FBX bone path references a missing node"))?;
        names.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow FBX bone path components: {error}"))
        })?;
        names.push(node.name.as_str());
        let Some(parent) = node.parent else {
            break;
        };
        key = parent;
    }
    for (position, name) in names.iter().rev().enumerate() {
        if position != 0 {
            output.push('/');
        }
        output.push_str(name);
    }
    Ok(())
}

fn lookup_bone_hash_node(index: &[BoneHashNode], hash: u32) -> Option<usize> {
    index
        .binary_search_by_key(&hash, |entry| entry.hash)
        .ok()
        .and_then(|position| index.get(position).map(|entry| entry.node_index))
}

fn build_global_matrices(model: &ModelIr) -> Result<Vec<Option<Matrix4>>> {
    let mut matrices = reserve_vec(model.nodes.len(), "FBX global matrices")?;
    matrices.resize(model.nodes.len(), None);
    let mut states = reserve_vec(model.nodes.len(), "FBX matrix traversal state")?;
    states.resize(model.nodes.len(), 0_u8);
    for index in 0..model.nodes.len() {
        let _ = resolve_global_matrix(model, index, &mut states, &mut matrices)?;
    }
    Ok(matrices)
}

fn resolve_global_matrix(
    model: &ModelIr,
    node_index: usize,
    states: &mut [u8],
    matrices: &mut [Option<Matrix4>],
) -> Result<Option<Matrix4>> {
    match states.get(node_index).copied() {
        Some(2) => return Ok(matrices[node_index]),
        Some(1) => {
            return Err(Error::invalid_data(
                "cycle encountered while building FBX global transforms",
            ));
        }
        Some(0) => {}
        _ => {
            return Err(Error::invalid_data(
                "FBX model node index is outside the graph",
            ));
        }
    }
    states[node_index] = 1;
    let node = model
        .nodes
        .get(node_index)
        .ok_or_else(|| Error::invalid_data("FBX model node index is outside the graph"))?;
    let Some(transform) = node.transform else {
        states[node_index] = 2;
        return Ok(None);
    };
    let local = Matrix4::from_unity_transform(transform)?;
    let parent_global = node
        .parent
        .and_then(|parent| model.node_index(parent))
        .map(|parent_index| resolve_global_matrix(model, parent_index, states, matrices))
        .transpose()?
        .flatten();
    let global = parent_global.map_or(Ok(local), |parent| parent.multiply(local))?;
    matrices[node_index] = Some(global);
    states[node_index] = 2;
    Ok(Some(global))
}

fn geometry_materials(
    model: &ModelIr,
    renderer: &crate::model_ir::ModelRendererBinding,
    submesh_count: usize,
) -> Result<(Vec<i64>, Vec<usize>, Vec<MaterialReference>)> {
    let mut by_submesh = reserve_vec(submesh_count, "FBX submesh materials")?;
    for submesh_index in 0..submesh_count {
        let reference = renderer
            .materials
            .get(submesh_index)
            .copied()
            .flatten()
            .map_or(Ok(MaterialReference::Default), |key| {
                model
                    .material_index(key)
                    .map(MaterialReference::Model)
                    .ok_or_else(|| {
                        Error::invalid_data("renderer Material is absent from the model IR")
                    })
            })?;
        by_submesh.push(reference);
    }
    let mut unique = reserve_vec(by_submesh.len(), "FBX unique geometry materials")?;
    unique.extend_from_slice(&by_submesh);
    unique.sort_unstable();
    unique.dedup();
    let mut material_ids = reserve_vec(unique.len(), "FBX geometry material IDs")?;
    for reference in &unique {
        material_ids.push(material_id(*reference)?);
    }
    let mut slots = reserve_vec(by_submesh.len(), "FBX submesh material slots")?;
    for reference in by_submesh {
        slots.push(unique.binary_search(&reference).map_err(|_| {
            Error::invalid_data("FBX submesh material vanished during deterministic deduplication")
        })?);
    }
    Ok((material_ids, slots, unique))
}

fn build_material_plans<'a>(
    model: &'a ModelIr,
    references: &[MaterialReference],
) -> Result<Vec<MaterialPlan<'a>>> {
    let mut plans = reserve_vec(references.len(), "FBX materials")?;
    for reference in references {
        let material = match reference {
            MaterialReference::Default => None,
            MaterialReference::Model(index) => Some(
                &model
                    .materials
                    .get(*index)
                    .ok_or_else(|| Error::invalid_data("FBX Material index is outside ModelIr"))?
                    .material,
            ),
        };
        if let Some(material) = material {
            validate_name(&material.name, "material")?;
        }
        MaterialProperties::from_material(material).validate()?;
        plans.push(MaterialPlan {
            id: material_id(*reference)?,
            material,
        });
    }
    Ok(plans)
}

#[derive(Default)]
struct AnimationIdState {
    curve_nodes: usize,
    curves: usize,
}

fn build_animation_plans<'a>(
    model: &ModelIr,
    geometries: &[GeometryPlan<'_>],
    animations: &'a ModelAnimationSet,
) -> Result<Vec<AnimationPlan<'a>>> {
    let mut plans = reserve_vec(animations.clips.len(), "FBX animation plans")?;
    let mut ids = AnimationIdState::default();
    for (clip_index, clip) in animations.clips.iter().enumerate() {
        validate_name(&clip.name, "animation")?;
        if !clip.sample_rate.is_finite() || clip.sample_rate <= 0.0 {
            return Err(Error::invalid_data(format!(
                "FBX animation {} has invalid sample rate {}",
                clip.name, clip.sample_rate
            )));
        }
        let property_capacity = clip
            .tracks
            .len()
            .checked_mul(3)
            .ok_or_else(|| Error::invalid_data("FBX animation property count overflowed"))?;
        let mut properties = reserve_vec(property_capacity, "FBX animation properties")?;
        for track in &clip.tracks {
            append_animation_track(model, track, &mut ids, &mut properties)?;
        }
        let mut blend_shapes = reserve_vec(
            clip.blend_shapes.len(),
            "FBX blend-shape animation properties",
        )?;
        for track in &clip.blend_shapes {
            blend_shapes.push(build_blend_shape_animation(
                model, geometries, track, &mut ids,
            )?);
        }
        let mut stop_time = 0_i64;
        for key in properties.iter().flat_map(|property| property.keys) {
            stop_time = stop_time.max(fbx_key_time(key.time)?);
        }
        for key in blend_shapes.iter().flat_map(|property| property.keys) {
            stop_time = stop_time.max(fbx_key_time(key.time)?);
        }
        plans.push(AnimationPlan {
            stack_id: indexed_id(ANIMATION_STACK_ID_BASE, clip_index, "FBX animation stack")?,
            layer_id: indexed_id(ANIMATION_LAYER_ID_BASE, clip_index, "FBX animation layer")?,
            name: &clip.name,
            stop_time,
            properties,
            blend_shapes,
        });
    }
    Ok(plans)
}

fn build_blend_shape_animation<'a>(
    model: &ModelIr,
    geometries: &[GeometryPlan<'_>],
    track: &'a ModelBlendShapeTrack,
    ids: &mut AnimationIdState,
) -> Result<AnimationBlendShapePlan<'a>> {
    validate_name(&track.channel, "blend-shape animation channel")?;
    validate_scalar_animation_keys(&track.keys)?;
    let node_index = model.node_index(track.node).ok_or_else(|| {
        Error::invalid_data("FBX blend-shape animation targets a missing model node")
    })?;
    let model_id = indexed_id(MODEL_ID_BASE, node_index, "FBX animated model")?;
    let channel_id = geometries
        .iter()
        .filter(|geometry| geometry.model_id == model_id)
        .filter_map(|geometry| geometry.morph.as_ref())
        .flat_map(|morph| &morph.channels)
        .find(|channel| channel.name == track.channel)
        .map(|channel| channel.id)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "FBX blend-shape animation channel {} is absent from its model geometry",
                track.channel
            ))
        })?;
    let node_id = indexed_id(
        ANIMATION_CURVE_NODE_ID_BASE,
        ids.curve_nodes,
        "FBX animation curve node",
    )?;
    ids.curve_nodes = ids
        .curve_nodes
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("FBX animation curve-node count overflowed"))?;
    let curve_id = indexed_id(ANIMATION_CURVE_ID_BASE, ids.curves, "FBX animation curve")?;
    ids.curves = ids
        .curves
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("FBX animation curve count overflowed"))?;
    Ok(AnimationBlendShapePlan {
        node_id,
        curve_id,
        channel_id,
        channel_name: &track.channel,
        keys: &track.keys,
    })
}

fn append_animation_track<'a>(
    model: &ModelIr,
    track: &'a ModelAnimationTrack,
    ids: &mut AnimationIdState,
    properties: &mut Vec<AnimationPropertyPlan<'a>>,
) -> Result<()> {
    let node_index = model
        .node_index(track.node)
        .ok_or_else(|| Error::invalid_data("FBX animation track targets a missing model node"))?;
    if model.nodes[node_index].transform.is_none() {
        return Err(Error::invalid_data(
            "FBX animation track targets a model node without Transform",
        ));
    }
    let model_id = indexed_id(MODEL_ID_BASE, node_index, "FBX animated model")?;
    for (kind, keys) in [
        (AnimationProperty::Scaling, track.scalings.as_slice()),
        (AnimationProperty::Rotation, track.rotations.as_slice()),
        (
            AnimationProperty::Translation,
            track.translations.as_slice(),
        ),
    ] {
        if keys.is_empty() {
            continue;
        }
        properties.push(build_animation_property(model_id, kind, keys, ids)?);
    }
    Ok(())
}

fn build_animation_property<'a>(
    model_id: i64,
    kind: AnimationProperty,
    keys: &'a [ModelVectorKeyframe],
    ids: &mut AnimationIdState,
) -> Result<AnimationPropertyPlan<'a>> {
    validate_animation_keys(keys)?;
    let node_id = indexed_id(
        ANIMATION_CURVE_NODE_ID_BASE,
        ids.curve_nodes,
        "FBX animation curve node",
    )?;
    ids.curve_nodes = ids
        .curve_nodes
        .checked_add(1)
        .ok_or_else(|| Error::invalid_data("FBX animation curve-node count overflowed"))?;
    let mut curve_ids = [0_i64; 3];
    for id in &mut curve_ids {
        *id = indexed_id(ANIMATION_CURVE_ID_BASE, ids.curves, "FBX animation curve")?;
        ids.curves = ids
            .curves
            .checked_add(1)
            .ok_or_else(|| Error::invalid_data("FBX animation curve count overflowed"))?;
    }
    Ok(AnimationPropertyPlan {
        node_id,
        model_id,
        kind,
        curve_ids,
        keys,
    })
}

fn validate_animation_keys(keys: &[ModelVectorKeyframe]) -> Result<()> {
    let mut previous = None;
    for key in keys {
        let _ = fbx_key_time(key.time)?;
        if key.value.into_iter().any(|value| !value.is_finite()) {
            return Err(Error::invalid_data(
                "FBX animation key contains a non-finite value",
            ));
        }
        if previous.is_some_and(|previous| key.time < previous) {
            return Err(Error::invalid_data(
                "FBX animation key times are not monotonically ordered",
            ));
        }
        previous = Some(key.time);
    }
    Ok(())
}

fn validate_scalar_animation_keys(keys: &[ModelScalarKeyframe]) -> Result<()> {
    let mut previous = None;
    for key in keys {
        let _ = fbx_key_time(key.time)?;
        if !key.value.is_finite() {
            return Err(Error::invalid_data(
                "FBX blend-shape animation key contains a non-finite value",
            ));
        }
        if previous.is_some_and(|previous| key.time < previous) {
            return Err(Error::invalid_data(
                "FBX blend-shape animation key times are not monotonically ordered",
            ));
        }
        previous = Some(key.time);
    }
    Ok(())
}

fn fbx_key_time(seconds: f32) -> Result<i64> {
    if !seconds.is_finite() {
        return Err(Error::invalid_data("FBX animation key time is non-finite"));
    }
    let ticks = f64::from(seconds) * FBX_TIME_SECOND;
    rounded_f64_to_i64(ticks)
}

fn rounded_f64_to_i64(value: f64) -> Result<i64> {
    let rounded = value.round();
    if !rounded.is_finite() {
        return Err(Error::invalid_data(
            "FBX animation key time is outside the KTime range",
        ));
    }
    let bits = rounded.to_bits();
    let negative = bits >> 63 != 0;
    let exponent_bits = u16::try_from((bits >> 52) & 0x7ff)
        .map_err(|_| Error::invalid_data("FBX time exponent does not fit in u16"))?;
    if exponent_bits == 0 {
        return Ok(0);
    }
    let exponent = i32::from(exponent_bits) - 1023;
    if exponent < 0 {
        return Ok(0);
    }
    let exponent = u32::try_from(exponent)
        .map_err(|_| Error::invalid_data("FBX time exponent is negative"))?;
    if exponent > 63 {
        return Err(Error::invalid_data(
            "FBX animation key time is outside the KTime range",
        ));
    }
    let significand = (1_u64 << 52) | (bits & ((1_u64 << 52) - 1));
    let magnitude = if exponent >= 52 {
        significand
            .checked_shl(exponent - 52)
            .ok_or_else(|| Error::invalid_data("FBX time magnitude overflowed"))?
    } else {
        significand >> (52 - exponent)
    };
    if negative {
        if magnitude == 1_u64 << 63 {
            Ok(i64::MIN)
        } else {
            i64::try_from(magnitude)
                .map(|magnitude| -magnitude)
                .map_err(|_| Error::invalid_data("FBX time magnitude exceeds i64"))
        }
    } else {
        i64::try_from(magnitude).map_err(|_| Error::invalid_data("FBX time magnitude exceeds i64"))
    }
}

#[derive(Clone, Copy)]
struct Matrix4([f64; 16]);

impl Matrix4 {
    fn from_unity_transform(transform: ModelLocalTransform) -> Result<Self> {
        let values = [
            transform.local_rotation.x,
            transform.local_rotation.y,
            transform.local_rotation.z,
            transform.local_rotation.w,
            transform.local_position.x,
            transform.local_position.y,
            transform.local_position.z,
            transform.local_scale.x,
            transform.local_scale.y,
            transform.local_scale.z,
        ];
        if !values.into_iter().all(f32::is_finite) {
            return Err(Error::invalid_data(
                "model Transform contains a non-finite value",
            ));
        }
        let mut quaternion = [
            f64::from(transform.local_rotation.x),
            -f64::from(transform.local_rotation.y),
            -f64::from(transform.local_rotation.z),
            f64::from(transform.local_rotation.w),
        ];
        let norm_squared = quaternion.iter().map(|value| value * value).sum::<f64>();
        if !norm_squared.is_finite() || norm_squared < f64::MIN_POSITIVE {
            return Err(Error::invalid_data(
                "model Transform contains a zero-length quaternion",
            ));
        }
        let inverse_norm = norm_squared.sqrt().recip();
        for value in &mut quaternion {
            *value *= inverse_norm;
        }
        let [x, y, z, w] = quaternion;
        let x2 = x + x;
        let y2 = y + y;
        let z2 = z + z;
        let xx = x * x2;
        let yy = y * y2;
        let zz = z * z2;
        let xy = x * y2;
        let xz = x * z2;
        let yz = y * z2;
        let wx = w * x2;
        let wy = w * y2;
        let wz = w * z2;
        let scale = transform.local_scale;
        let mut matrix = Self([0.0; 16]);
        matrix.set(0, 0, (1.0 - (yy + zz)) * f64::from(scale.x));
        matrix.set(1, 0, (xy + wz) * f64::from(scale.x));
        matrix.set(2, 0, (xz - wy) * f64::from(scale.x));
        matrix.set(0, 1, (xy - wz) * f64::from(scale.y));
        matrix.set(1, 1, (1.0 - (xx + zz)) * f64::from(scale.y));
        matrix.set(2, 1, (yz + wx) * f64::from(scale.y));
        matrix.set(0, 2, (xz + wy) * f64::from(scale.z));
        matrix.set(1, 2, (yz - wx) * f64::from(scale.z));
        matrix.set(2, 2, (1.0 - (xx + yy)) * f64::from(scale.z));
        matrix.set(0, 3, -f64::from(transform.local_position.x));
        matrix.set(1, 3, f64::from(transform.local_position.y));
        matrix.set(2, 3, f64::from(transform.local_position.z));
        matrix.set(3, 3, 1.0);
        matrix.validate("FBX local Transform")?;
        Ok(matrix)
    }

    fn mirrored_bind_pose(values: [f32; 16]) -> Result<Self> {
        let mut matrix = Self([0.0; 16]);
        let signs = [-1.0, 1.0, 1.0, 1.0];
        for row in 0..4 {
            for column in 0..4 {
                matrix.set(
                    row,
                    column,
                    f64::from(values[row + column * 4]) * signs[row] * signs[column],
                );
            }
        }
        matrix.validate("FBX bind pose")?;
        Ok(matrix)
    }

    fn multiply(self, right: Self) -> Result<Self> {
        let mut result = Self([0.0; 16]);
        for row in 0..4 {
            for column in 0..4 {
                let mut value = 0.0;
                for index in 0..4 {
                    value += self.get(row, index) * right.get(index, column);
                }
                result.set(row, column, value);
            }
        }
        result.validate("FBX matrix product")?;
        Ok(result)
    }

    fn inverse(self) -> Result<Self> {
        let mut augmented = [[0.0_f64; 8]; 4];
        for (row, values) in augmented.iter_mut().enumerate() {
            for (column, value) in values.iter_mut().take(4).enumerate() {
                *value = self.get(row, column);
            }
            values[4 + row] = 1.0;
        }
        for column in 0..4 {
            let pivot_row = (column..4)
                .max_by(|left, right| {
                    augmented[*left][column]
                        .abs()
                        .total_cmp(&augmented[*right][column].abs())
                })
                .ok_or_else(|| Error::invalid_data("FBX bind pose has no inverse"))?;
            let pivot = augmented[pivot_row][column];
            if pivot.abs() < f64::MIN_POSITIVE || !pivot.is_finite() {
                return Err(Error::invalid_data(
                    "FBX bind pose is singular and cannot form a skin cluster",
                ));
            }
            augmented.swap(column, pivot_row);
            let divisor = augmented[column][column];
            for value in &mut augmented[column] {
                *value /= divisor;
            }
            for row in 0..4 {
                if row == column {
                    continue;
                }
                let factor = augmented[row][column];
                for value_index in 0..8 {
                    augmented[row][value_index] -= factor * augmented[column][value_index];
                }
            }
        }
        let mut result = Self([0.0; 16]);
        for (row, values) in augmented.iter().enumerate() {
            for column in 0..4 {
                result.set(row, column, values[4 + column]);
            }
        }
        result.validate("FBX inverse bind pose")?;
        Ok(result)
    }

    fn validate(self, field: &str) -> Result<()> {
        if self.0.into_iter().all(f64::is_finite) {
            Ok(())
        } else {
            Err(Error::invalid_data(format!(
                "{field} contains a non-finite value"
            )))
        }
    }

    const fn get(self, row: usize, column: usize) -> f64 {
        self.0[row + column * 4]
    }

    fn set(&mut self, row: usize, column: usize, value: f64) {
        self.0[row + column * 4] = value;
    }
}

impl ConvertedTransform {
    fn from_unity(transform: ModelLocalTransform) -> Result<Self> {
        let values = [
            transform.local_rotation.x,
            transform.local_rotation.y,
            transform.local_rotation.z,
            transform.local_rotation.w,
            transform.local_position.x,
            transform.local_position.y,
            transform.local_position.z,
            transform.local_scale.x,
            transform.local_scale.y,
            transform.local_scale.z,
        ];
        if !values.into_iter().all(f32::is_finite) {
            return Err(Error::invalid_data(
                "model Transform contains a non-finite value",
            ));
        }
        Ok(Self {
            translation: [
                -transform.local_position.x,
                transform.local_position.y,
                transform.local_position.z,
            ],
            rotation: quaternion_to_euler_degrees([
                transform.local_rotation.x,
                -transform.local_rotation.y,
                -transform.local_rotation.z,
                transform.local_rotation.w,
            ])?,
            scale: [
                transform.local_scale.x,
                transform.local_scale.y,
                transform.local_scale.z,
            ],
        })
    }
}

pub(crate) fn quaternion_to_euler_degrees(mut quaternion: [f32; 4]) -> Result<[f32; 3]> {
    let norm_squared = quaternion.iter().map(|value| value * value).sum::<f32>();
    if !norm_squared.is_finite() || norm_squared < f32::MIN_POSITIVE {
        return Err(Error::invalid_data(
            "model Transform contains a zero-length quaternion",
        ));
    }
    let inverse_norm = norm_squared.sqrt().recip();
    for value in &mut quaternion {
        *value *= inverse_norm;
    }
    let [x, y, z, w] = quaternion;
    let matrix_00 = 1.0 - 2.0 * (y * y + z * z);
    let matrix_01 = 2.0 * (x * y - z * w);
    let matrix_10 = 2.0 * (x * y + z * w);
    let matrix_11 = 1.0 - 2.0 * (x * x + z * z);
    let matrix_20 = 2.0 * (x * z - y * w);
    let matrix_21 = 2.0 * (y * z + x * w);
    let matrix_22 = 1.0 - 2.0 * (x * x + y * y);
    let cosine_y = (matrix_00 * matrix_00 + matrix_10 * matrix_10).sqrt();
    let rotation_y = (-matrix_20).atan2(cosine_y).to_degrees();
    let (rotation_x, rotation_z) = if cosine_y <= 16.0 * f32::EPSILON {
        let rotation_x = if (-matrix_20).is_sign_positive() {
            matrix_01.atan2(matrix_11)
        } else {
            (-matrix_01).atan2(matrix_11)
        };
        (rotation_x.to_degrees(), 0.0)
    } else {
        (
            matrix_21.atan2(matrix_22).to_degrees(),
            matrix_10.atan2(matrix_00).to_degrees(),
        )
    };
    Ok([rotation_x, rotation_y, rotation_z])
}

fn validate_mesh(mesh: &Mesh) -> Result<()> {
    let vertex_count = mesh.vertices.len();
    vertex_count
        .checked_mul(3)
        .ok_or_else(|| Error::invalid_data("FBX vertex scalar count overflowed"))?;
    if mesh
        .vertices
        .iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(Error::invalid_data(
            "Mesh positions contain a non-finite value",
        ));
    }
    validate_vertex_attribute(mesh.normals.as_deref(), vertex_count, 3, "normals")?;
    validate_vertex_attribute(mesh.uv0.as_deref(), vertex_count, 2, "UV0")?;
    if mesh
        .bind_poses
        .iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(Error::invalid_data(
            "Mesh bind poses contain a non-finite value",
        ));
    }
    if let Some(skin) = &mesh.skin {
        if skin.len() != vertex_count {
            return Err(Error::invalid_data(format!(
                "Mesh has {} skin records for {vertex_count} control points",
                skin.len()
            )));
        }
        if skin
            .iter()
            .flat_map(|weights| weights.weights)
            .any(|weight| !weight.is_finite())
        {
            return Err(Error::invalid_data(
                "Mesh skin weights contain a non-finite value",
            ));
        }
    }
    if let Some(blend_shapes) = &mesh.blend_shapes {
        validate_mesh_blend_shapes(mesh, blend_shapes)?;
    }
    let mut polygon_count = 0_usize;
    for submesh in &mesh.sub_meshes {
        if !submesh.indices.len().is_multiple_of(3) {
            return Err(Error::invalid_data(format!(
                "Mesh submesh has {} indices, which is not a triangle list",
                submesh.indices.len()
            )));
        }
        polygon_count = polygon_count
            .checked_add(submesh.indices.len() / 3)
            .ok_or_else(|| Error::invalid_data("FBX polygon count overflowed"))?;
        if let Some(index) = submesh
            .indices
            .iter()
            .copied()
            .find(|index| usize::try_from(*index).map_or(true, |index| index >= vertex_count))
        {
            return Err(Error::invalid_data(format!(
                "Mesh index {index} exceeds its {vertex_count} control points"
            )));
        }
    }
    let _ = polygon_count;
    Ok(())
}

fn validate_mesh_blend_shapes(mesh: &Mesh, shapes: &crate::mesh::MeshBlendShapes) -> Result<()> {
    if shapes.frames.len() != shapes.full_weights.len() {
        return Err(Error::invalid_data(format!(
            "Mesh has {} blend-shape frames but {} full weights",
            shapes.frames.len(),
            shapes.full_weights.len()
        )));
    }
    if shapes.full_weights.iter().any(|weight| !weight.is_finite()) {
        return Err(Error::invalid_data(
            "Mesh blend-shape weights contain a non-finite value",
        ));
    }
    for (channel_index, channel) in shapes.channels.iter().enumerate() {
        let end = channel
            .frame_index
            .checked_add(channel.frame_count)
            .ok_or_else(|| Error::invalid_data("FBX blend-shape channel range overflowed"))?;
        if end > shapes.frames.len() {
            return Err(Error::invalid_data(format!(
                "Mesh blend-shape channel {channel_index} frame range {}..{end} exceeds {} frames",
                channel.frame_index,
                shapes.frames.len()
            )));
        }
    }
    for (frame_index, frame) in shapes.frames.iter().enumerate() {
        let start = usize::try_from(frame.first_vertex)
            .map_err(|_| Error::invalid_data("FBX blend-shape first vertex does not fit usize"))?;
        let count = usize::try_from(frame.vertex_count)
            .map_err(|_| Error::invalid_data("FBX blend-shape vertex count does not fit usize"))?;
        let end = start
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("FBX blend-shape vertex range overflowed"))?;
        let vertices = shapes.vertices.get(start..end).ok_or_else(|| {
            Error::invalid_data(format!(
                "Mesh blend-shape frame {frame_index} vertex range {start}..{end} exceeds {} vertices",
                shapes.vertices.len()
            ))
        })?;
        for vertex in vertices {
            let index = usize::try_from(vertex.index).map_err(|_| {
                Error::invalid_data("FBX blend-shape vertex index does not fit usize")
            })?;
            let base = mesh.vertices.get(index).ok_or_else(|| {
                Error::invalid_data(format!(
                    "Mesh blend-shape vertex index {} exceeds {} control points",
                    vertex.index,
                    mesh.vertices.len()
                ))
            })?;
            if vertex
                .vertex
                .into_iter()
                .chain(vertex.normal)
                .chain(vertex.tangent)
                .any(|value| !value.is_finite())
            {
                return Err(Error::invalid_data(
                    "Mesh blend-shape data contains a non-finite value",
                ));
            }
            if base
                .iter()
                .zip(vertex.vertex)
                .any(|(base, delta)| (*base + delta).is_infinite())
            {
                return Err(Error::invalid_data(
                    "Mesh blend-shape target position overflowed",
                ));
            }
        }
    }
    Ok(())
}

fn validate_vertex_attribute<const N: usize>(
    values: Option<&[[f32; N]]>,
    vertex_count: usize,
    components: usize,
    name: &str,
) -> Result<()> {
    let Some(values) = values else {
        return Ok(());
    };
    if values.len() != vertex_count {
        return Err(Error::invalid_data(format!(
            "Mesh has {} {name} values for {vertex_count} control points",
            values.len()
        )));
    }
    values
        .len()
        .checked_mul(components)
        .ok_or_else(|| Error::invalid_data(format!("FBX {name} scalar count overflowed")))?;
    if values.iter().flatten().any(|value| !value.is_finite()) {
        return Err(Error::invalid_data(format!(
            "Mesh {name} contains a non-finite value"
        )));
    }
    Ok(())
}

fn write_model(node: &NodePlan<'_>, output: &mut impl Write) -> io::Result<()> {
    let kind = if node.has_geometry {
        "Mesh"
    } else if node.is_bone {
        "LimbNode"
    } else {
        "Null"
    };
    let name = SanitizedName(node.name, "Node");
    write!(
        output,
        "    Model: {}, \"Model::{name}\", \"{kind}\" {{\n\
        Version: 232\n\
        Properties70:  {{\n\
            P: \"Lcl Translation\", \"Lcl Translation\", \"\", \"A\",{},{},{}\n\
            P: \"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",{},{},{}\n\
            P: \"Lcl Scaling\", \"Lcl Scaling\", \"\", \"A\",{},{},{}\n\
        }}\n\
        Shading: T\n\
        Culling: \"CullingOff\"\n\
    }}\n",
        node.id,
        FbxFloat(node.transform.translation[0]),
        FbxFloat(node.transform.translation[1]),
        FbxFloat(node.transform.translation[2]),
        FbxFloat(node.transform.rotation[0]),
        FbxFloat(node.transform.rotation[1]),
        FbxFloat(node.transform.rotation[2]),
        FbxFloat(node.transform.scale[0]),
        FbxFloat(node.transform.scale[1]),
        FbxFloat(node.transform.scale[2]),
    )
}

fn write_geometry(geometry: &GeometryPlan<'_>, output: &mut impl Write) -> io::Result<()> {
    let mesh = geometry.mesh;
    let vertex_scalar_count = mesh.vertices.len() * 3;
    let index_count = mesh
        .sub_meshes
        .iter()
        .map(|submesh| submesh.indices.len())
        .sum::<usize>();
    writeln!(
        output,
        "    Geometry: {}, \"Geometry::{}\", \"Mesh\" {{",
        geometry.id,
        SanitizedName(&mesh.name, "Mesh")
    )?;
    writeln!(output, "        GeometryVersion: 124")?;
    writeln!(output, "        Vertices: *{vertex_scalar_count} {{")?;
    output.write_all(b"            a: ")?;
    let mut first = true;
    for vertex in &mesh.vertices {
        for value in [-vertex[0], vertex[1], vertex[2]] {
            write_array_value(output, &mut first, FbxFloat(value))?;
        }
    }
    output.write_all(b"\n        }\n")?;
    writeln!(output, "        PolygonVertexIndex: *{index_count} {{")?;
    output.write_all(b"            a: ")?;
    first = true;
    for submesh in &mesh.sub_meshes {
        for triangle in submesh.indices.chunks_exact(3) {
            write_array_value(output, &mut first, triangle[2])?;
            write_array_value(output, &mut first, triangle[1])?;
            write_array_value(output, &mut first, polygon_end(triangle[0]))?;
        }
    }
    output.write_all(b"\n        }\n")?;
    write_geometry_layers(geometry, output)?;
    output.write_all(b"    }\n")
}

fn write_shape_geometries(
    mesh: &Mesh,
    morph: &MorphPlan<'_>,
    output: &mut impl Write,
) -> io::Result<()> {
    for channel in &morph.channels {
        for shape in &channel.shapes {
            write_shape_geometry(mesh, shape, output)?;
        }
    }
    Ok(())
}

fn write_shape_geometry(
    mesh: &Mesh,
    shape: &ShapePlan<'_>,
    output: &mut impl Write,
) -> io::Result<()> {
    let scalar_count = shape
        .vertices
        .len()
        .checked_mul(3)
        .ok_or_else(|| io::Error::other("FBX target-shape scalar count overflowed"))?;
    writeln!(
        output,
        "    Geometry: {}, \"Geometry::{}\", \"Shape\" {{",
        shape.id,
        SanitizedName(&shape.name, "Shape")
    )?;
    output.write_all(b"        Version: 100\n")?;
    writeln!(output, "        Indexes: *{} {{", shape.vertices.len())?;
    output.write_all(b"            a: ")?;
    let mut first = true;
    for vertex in shape.vertices {
        let index = usize::try_from(vertex.index)
            .map_err(|_| io::Error::other("FBX target-shape vertex index does not fit usize"))?;
        if index >= mesh.vertices.len() {
            return Err(io::Error::other(
                "FBX target-shape vertex index is out of range",
            ));
        }
        write_array_value(output, &mut first, vertex.index)?;
    }
    output.write_all(b"\n        }\n")?;
    // FBX stores a target shape as per-index offsets from the base control
    // points named in `Indexes`, not as absolute positions: importers add
    // these values to the base vertex. Mirror X to match the base geometry.
    writeln!(output, "        Vertices: *{scalar_count} {{")?;
    output.write_all(b"            a: ")?;
    first = true;
    for vertex in shape.vertices {
        for value in [-vertex.vertex[0], vertex.vertex[1], vertex.vertex[2]] {
            write_array_value(output, &mut first, FbxFloat(value))?;
        }
    }
    output.write_all(b"\n        }\n")?;
    if shape.frame.has_normals {
        writeln!(output, "        Normals: *{scalar_count} {{")?;
        output.write_all(b"            a: ")?;
        first = true;
        for vertex in shape.vertices {
            for value in [-vertex.normal[0], vertex.normal[1], vertex.normal[2]] {
                write_array_value(output, &mut first, FbxFloat(value))?;
            }
        }
        output.write_all(b"\n        }\n")?;
    }
    output.write_all(b"    }\n")
}

fn write_geometry_layers(geometry: &GeometryPlan<'_>, output: &mut impl Write) -> io::Result<()> {
    let mesh = geometry.mesh;
    if let Some(normals) = &mesh.normals {
        writeln!(output, "        LayerElementNormal: 0 {{")?;
        output.write_all(
            b"            Version: 101\n            Name: \"\"\n            MappingInformationType: \"ByVertice\"\n            ReferenceInformationType: \"Direct\"\n",
        )?;
        writeln!(output, "            Normals: *{} {{", normals.len() * 3)?;
        output.write_all(b"                a: ")?;
        let mut first = true;
        for normal in normals {
            for value in [-normal[0], normal[1], normal[2]] {
                write_array_value(output, &mut first, FbxFloat(value))?;
            }
        }
        output.write_all(b"\n            }\n        }\n")?;
    }
    if let Some(uv) = &mesh.uv0 {
        writeln!(output, "        LayerElementUV: 0 {{")?;
        output.write_all(
            b"            Version: 101\n            Name: \"UV0\"\n            MappingInformationType: \"ByVertice\"\n            ReferenceInformationType: \"Direct\"\n",
        )?;
        writeln!(output, "            UV: *{} {{", uv.len() * 2)?;
        output.write_all(b"                a: ")?;
        let mut first = true;
        for uv in uv {
            write_array_value(output, &mut first, FbxFloat(uv[0]))?;
            write_array_value(output, &mut first, FbxFloat(uv[1]))?;
        }
        output.write_all(b"\n            }\n        }\n")?;
    }
    if !geometry.material_ids.is_empty() {
        let polygon_count = mesh
            .sub_meshes
            .iter()
            .map(|submesh| submesh.indices.len() / 3)
            .sum::<usize>();
        output.write_all(
            b"        LayerElementMaterial: 0 {\n            Version: 101\n            Name: \"\"\n            MappingInformationType: \"ByPolygon\"\n            ReferenceInformationType: \"IndexToDirect\"\n",
        )?;
        writeln!(output, "            Materials: *{polygon_count} {{")?;
        output.write_all(b"                a: ")?;
        let mut first = true;
        for (submesh, slot) in mesh.sub_meshes.iter().zip(&geometry.submesh_material_slots) {
            for _ in 0..submesh.indices.len() / 3 {
                write_array_value(output, &mut first, *slot)?;
            }
        }
        output.write_all(b"\n            }\n        }\n")?;
    }
    output.write_all(b"        Layer: 0 {\n            Version: 100\n")?;
    if mesh.normals.is_some() {
        output.write_all(
            b"            LayerElement: { Type: \"LayerElementNormal\", TypedIndex: 0 }\n",
        )?;
    }
    if mesh.uv0.is_some() {
        output.write_all(
            b"            LayerElement: { Type: \"LayerElementUV\", TypedIndex: 0 }\n",
        )?;
    }
    if !geometry.material_ids.is_empty() {
        output.write_all(
            b"            LayerElement: { Type: \"LayerElementMaterial\", TypedIndex: 0 }\n",
        )?;
    }
    output.write_all(b"        }\n")
}

fn write_material(material: &MaterialPlan<'_>, output: &mut impl Write) -> io::Result<()> {
    let name = SanitizedName(
        material
            .material
            .map_or("DefaultMaterial", |material| &material.name),
        "DefaultMaterial",
    );
    let properties = MaterialProperties::from_material(material.material);
    write!(
        output,
        "    Material: {}, \"Material::{name}\", \"\" {{\n\
        Version: 102\n\
        ShadingModel: \"phong\"\n\
        MultiLayer: 0\n\
        Properties70:  {{\n\
            P: \"DiffuseColor\", \"Color\", \"\", \"A\",{},{},{}\n\
            P: \"AmbientColor\", \"Color\", \"\", \"A\",{},{},{}\n\
            P: \"EmissiveColor\", \"Color\", \"\", \"A\",{},{},{}\n\
            P: \"SpecularColor\", \"Color\", \"\", \"A\",{},{},{}\n\
            P: \"ReflectionColor\", \"Color\", \"\", \"A\",{},{},{}\n\
            P: \"Shininess\", \"double\", \"Number\", \"A\",{}\n\
            P: \"TransparencyFactor\", \"double\", \"Number\", \"A\",{}\n\
        }}\n\
    }}\n",
        material.id,
        FbxFloat(properties.diffuse[0]),
        FbxFloat(properties.diffuse[1]),
        FbxFloat(properties.diffuse[2]),
        FbxFloat(properties.ambient[0]),
        FbxFloat(properties.ambient[1]),
        FbxFloat(properties.ambient[2]),
        FbxFloat(properties.emissive[0]),
        FbxFloat(properties.emissive[1]),
        FbxFloat(properties.emissive[2]),
        FbxFloat(properties.specular[0]),
        FbxFloat(properties.specular[1]),
        FbxFloat(properties.specular[2]),
        FbxFloat(properties.reflection[0]),
        FbxFloat(properties.reflection[1]),
        FbxFloat(properties.reflection[2]),
        FbxFloat(properties.shininess),
        FbxFloat(properties.transparency),
    )
}

fn write_skin(skin: &SkinPlan<'_>, output: &mut impl Write) -> io::Result<()> {
    writeln!(
        output,
        "    Deformer: {}, \"Deformer::{}\", \"Skin\" {{",
        skin.id,
        SanitizedName(skin.name, "Skin")
    )?;
    output.write_all(b"        Version: 101\n        Link_DeformAcuracy: 50\n    }\n")?;
    for cluster in &skin.clusters {
        writeln!(
            output,
            "    Deformer: {}, \"SubDeformer::{}Cluster\", \"Cluster\" {{",
            cluster.id,
            SanitizedName(cluster.bone_name, "Bone")
        )?;
        output.write_all(b"        Version: 100\n        UserData: \"\", \"\"\n")?;
        writeln!(output, "        Indexes: *{} {{", cluster.indices.len())?;
        output.write_all(b"            a: ")?;
        let mut first = true;
        for index in &cluster.indices {
            write_array_value(output, &mut first, index)?;
        }
        output.write_all(b"\n        }\n")?;
        writeln!(output, "        Weights: *{} {{", cluster.weights.len())?;
        output.write_all(b"            a: ")?;
        first = true;
        for weight in &cluster.weights {
            write_array_value(output, &mut first, FbxFloat(*weight))?;
        }
        output.write_all(b"\n        }\n")?;
        write_matrix("Transform", cluster.transform, output)?;
        write_matrix("TransformLink", cluster.transform_link, output)?;
        output.write_all(b"    }\n")?;
    }
    Ok(())
}

fn write_morph(morph: &MorphPlan<'_>, output: &mut impl Write) -> io::Result<()> {
    writeln!(
        output,
        "    Deformer: {}, \"Deformer::{}BlendShape\", \"BlendShape\" {{",
        morph.id,
        SanitizedName(morph.name, "Mesh")
    )?;
    output.write_all(b"        Version: 100\n    }\n")?;
    for channel in &morph.channels {
        writeln!(
            output,
            "    Deformer: {}, \"SubDeformer::{}\", \"BlendShapeChannel\" {{",
            channel.id,
            SanitizedName(channel.name, "BlendShape")
        )?;
        output.write_all(b"        Version: 100\n        DeformPercent: 0\n")?;
        writeln!(
            output,
            "        FullWeights: *{} {{",
            channel.full_weights.len()
        )?;
        output.write_all(b"            a: ")?;
        let mut first = true;
        for weight in channel.full_weights {
            write_array_value(output, &mut first, FbxFloat(*weight))?;
        }
        output.write_all(b"\n        }\n    }\n")?;
    }
    Ok(())
}

fn write_matrix(name: &str, matrix: Matrix4, output: &mut impl Write) -> io::Result<()> {
    writeln!(output, "        {name}: *16 {{")?;
    output.write_all(b"            a: ")?;
    let mut first = true;
    for row in 0..4 {
        for column in 0..4 {
            write_array_value(output, &mut first, FbxDouble(matrix.get(row, column)))?;
        }
    }
    output.write_all(b"\n        }\n")
}

fn write_animation(animation: &AnimationPlan<'_>, output: &mut impl Write) -> io::Result<()> {
    writeln!(
        output,
        "    AnimationStack: {}, \"AnimStack::{}\", \"\" {{",
        animation.stack_id,
        SanitizedName(animation.name, "Take")
    )?;
    write!(
        output,
        "        Properties70:  {{\n\
            P: \"LocalStart\", \"KTime\", \"Time\", \"\",0\n\
            P: \"LocalStop\", \"KTime\", \"Time\", \"\",{}\n\
            P: \"ReferenceStart\", \"KTime\", \"Time\", \"\",0\n\
            P: \"ReferenceStop\", \"KTime\", \"Time\", \"\",{}\n\
        }}\n\
    }}\n",
        animation.stop_time, animation.stop_time
    )?;
    writeln!(
        output,
        "    AnimationLayer: {}, \"AnimLayer::Base Layer\", \"\" {{\n        Version: 100\n        Weight: 100\n        Muted: 0\n        Solo: 0\n        Lock: 0\n        Color: 0.8,0.8,0.8\n        BlendMode: 0\n        RotationAccumulationMode: 0\n        ScaleAccumulationMode: 0\n    }}",
        animation.layer_id
    )?;
    for property in &animation.properties {
        write_animation_property(property, output)?;
    }
    for blend_shape in &animation.blend_shapes {
        write_blend_shape_animation(blend_shape, output)?;
    }
    Ok(())
}

fn write_blend_shape_animation(
    property: &AnimationBlendShapePlan<'_>,
    output: &mut impl Write,
) -> io::Result<()> {
    writeln!(
        output,
        "    AnimationCurveNode: {}, \"AnimCurveNode::{}\", \"\" {{",
        property.node_id,
        SanitizedName(property.channel_name, "BlendShape")
    )?;
    output.write_all(
        b"        Properties70:  {\n            P: \"d|DeformPercent\", \"Number\", \"\", \"A\",0\n        }\n    }\n",
    )?;
    write_scalar_animation_curve(property, output)
}

fn write_scalar_animation_curve(
    property: &AnimationBlendShapePlan<'_>,
    output: &mut impl Write,
) -> io::Result<()> {
    writeln!(
        output,
        "    AnimationCurve: {}, \"AnimCurve::{}_DeformPercent\", \"\" {{\n        Default: 0\n        KeyVer: 4008",
        property.curve_id,
        SanitizedName(property.channel_name, "BlendShape")
    )?;
    writeln!(output, "        KeyTime: *{} {{", property.keys.len())?;
    output.write_all(b"            a: ")?;
    let mut first = true;
    for key in property.keys {
        write_array_value(output, &mut first, fbx_key_time_for_write(key.time)?)?;
    }
    output.write_all(b"\n        }\n")?;
    writeln!(output, "        KeyValueFloat: *{} {{", property.keys.len())?;
    output.write_all(b"            a: ")?;
    first = true;
    for key in property.keys {
        write_array_value(output, &mut first, FbxFloat(key.value))?;
    }
    output.write_all(b"\n        }\n")?;
    writeln!(output, "        KeyAttrFlags: *{} {{", property.keys.len())?;
    write_repeated_array(output, property.keys.len(), 24_836)?;
    let attribute_count = property
        .keys
        .len()
        .checked_mul(4)
        .ok_or_else(|| io::Error::other("FBX animation key-attribute count overflowed"))?;
    writeln!(output, "        KeyAttrDataFloat: *{attribute_count} {{")?;
    write_repeated_array(output, attribute_count, 0)?;
    writeln!(
        output,
        "        KeyAttrRefCount: *{} {{",
        property.keys.len()
    )?;
    write_repeated_array(output, property.keys.len(), 1)?;
    output.write_all(b"    }\n")
}

fn write_animation_property(
    property: &AnimationPropertyPlan<'_>,
    output: &mut impl Write,
) -> io::Result<()> {
    let token = property.kind.token();
    let defaults = property.kind.defaults();
    writeln!(
        output,
        "    AnimationCurveNode: {}, \"AnimCurveNode::{token}\", \"\" {{",
        property.node_id
    )?;
    write!(
        output,
        "        Properties70:  {{\n\
            P: \"d|X\", \"Number\", \"\", \"A\",{}\n\
            P: \"d|Y\", \"Number\", \"\", \"A\",{}\n\
            P: \"d|Z\", \"Number\", \"\", \"A\",{}\n\
        }}\n\
    }}\n",
        FbxFloat(defaults[0]),
        FbxFloat(defaults[1]),
        FbxFloat(defaults[2])
    )?;
    for (component, id) in property.curve_ids.iter().enumerate() {
        write_animation_curve(
            *id,
            token,
            component,
            defaults[component],
            property.keys,
            output,
        )?;
    }
    Ok(())
}

fn write_animation_curve(
    id: i64,
    token: &str,
    component: usize,
    default: f32,
    keys: &[ModelVectorKeyframe],
    output: &mut impl Write,
) -> io::Result<()> {
    let component_name = animation_component_name(component);
    writeln!(
        output,
        "    AnimationCurve: {id}, \"AnimCurve::{token}_{component_name}\", \"\" {{\n        Default: {}\n        KeyVer: 4008",
        FbxFloat(default)
    )?;
    writeln!(output, "        KeyTime: *{} {{", keys.len())?;
    output.write_all(b"            a: ")?;
    let mut first = true;
    for key in keys {
        write_array_value(output, &mut first, fbx_key_time_for_write(key.time)?)?;
    }
    output.write_all(b"\n        }\n")?;
    writeln!(output, "        KeyValueFloat: *{} {{", keys.len())?;
    output.write_all(b"            a: ")?;
    first = true;
    for key in keys {
        write_array_value(output, &mut first, FbxFloat(key.value[component]))?;
    }
    output.write_all(b"\n        }\n")?;
    writeln!(output, "        KeyAttrFlags: *{} {{", keys.len())?;
    write_repeated_array(output, keys.len(), 24_836)?;
    let attribute_count = keys
        .len()
        .checked_mul(4)
        .ok_or_else(|| io::Error::other("FBX animation key-attribute count overflowed"))?;
    writeln!(output, "        KeyAttrDataFloat: *{attribute_count} {{")?;
    write_repeated_array(output, attribute_count, 0)?;
    writeln!(output, "        KeyAttrRefCount: *{} {{", keys.len())?;
    write_repeated_array(output, keys.len(), 1)?;
    output.write_all(b"    }\n")
}

fn write_repeated_array(
    output: &mut impl Write,
    count: usize,
    value: impl fmt::Display + Copy,
) -> io::Result<()> {
    output.write_all(b"            a: ")?;
    let mut first = true;
    for _ in 0..count {
        write_array_value(output, &mut first, value)?;
    }
    output.write_all(b"\n        }\n")
}

fn write_animation_connections(
    animation: &AnimationPlan<'_>,
    output: &mut impl Write,
) -> io::Result<()> {
    writeln!(
        output,
        "    C: \"OO\",{},{}",
        animation.layer_id, animation.stack_id
    )?;
    for property in &animation.properties {
        writeln!(
            output,
            "    C: \"OO\",{},{}",
            property.node_id, animation.layer_id
        )?;
        writeln!(
            output,
            "    C: \"OP\",{},{},\"{}\"",
            property.node_id,
            property.model_id,
            property.kind.fbx_property()
        )?;
        for (component, curve_id) in property.curve_ids.iter().enumerate() {
            writeln!(
                output,
                "    C: \"OP\",{},{},\"d|{}\"",
                curve_id,
                property.node_id,
                animation_component_name(component)
            )?;
        }
    }
    for property in &animation.blend_shapes {
        writeln!(
            output,
            "    C: \"OO\",{},{}",
            property.node_id, animation.layer_id
        )?;
        writeln!(
            output,
            "    C: \"OP\",{},{},\"DeformPercent\"",
            property.node_id, property.channel_id
        )?;
        writeln!(
            output,
            "    C: \"OP\",{},{},\"d|DeformPercent\"",
            property.curve_id, property.node_id
        )?;
    }
    Ok(())
}

impl AnimationProperty {
    const fn token(self) -> &'static str {
        match self {
            Self::Translation => "T",
            Self::Rotation => "R",
            Self::Scaling => "S",
        }
    }

    const fn fbx_property(self) -> &'static str {
        match self {
            Self::Translation => "Lcl Translation",
            Self::Rotation => "Lcl Rotation",
            Self::Scaling => "Lcl Scaling",
        }
    }

    const fn defaults(self) -> [f32; 3] {
        match self {
            Self::Scaling => [1.0; 3],
            Self::Translation | Self::Rotation => [0.0; 3],
        }
    }
}

const fn animation_component_name(component: usize) -> &'static str {
    match component {
        0 => "X",
        1 => "Y",
        _ => "Z",
    }
}

fn fbx_key_time_for_write(seconds: f32) -> io::Result<i64> {
    fbx_key_time(seconds).map_err(|error| io::Error::other(error.to_string()))
}

struct MaterialProperties {
    diffuse: [f32; 3],
    ambient: [f32; 3],
    emissive: [f32; 3],
    specular: [f32; 3],
    reflection: [f32; 3],
    shininess: f32,
    transparency: f32,
}

impl MaterialProperties {
    fn from_material(material: Option<&Material>) -> Self {
        let mut value = Self {
            diffuse: [0.8, 0.8, 0.8],
            ambient: [0.2, 0.2, 0.2],
            emissive: [0.0; 3],
            specular: [0.2, 0.2, 0.2],
            reflection: [0.0; 3],
            shininess: 20.0,
            transparency: 0.0,
        };
        let Some(material) = material else {
            return value;
        };
        for property in &material.saved_properties.colors {
            let color = [property.value[0], property.value[1], property.value[2]];
            match property.name.as_str() {
                "_Color" => value.diffuse = color,
                "_SColor" => value.ambient = color,
                "_EmissionColor" => value.emissive = color,
                "_SpecularColor" => value.specular = color,
                "_ReflectColor" => value.reflection = color,
                _ => {}
            }
        }
        for property in &material.saved_properties.floats {
            match property.name.as_str() {
                "_Shininess" => value.shininess = property.value,
                "_Transparency" => value.transparency = property.value,
                _ => {}
            }
        }
        value
    }

    fn validate(&self) -> Result<()> {
        if self
            .diffuse
            .iter()
            .chain(&self.ambient)
            .chain(&self.emissive)
            .chain(&self.specular)
            .chain(&self.reflection)
            .copied()
            .chain([self.shininess, self.transparency])
            .all(f32::is_finite)
        {
            Ok(())
        } else {
            Err(Error::invalid_data(
                "FBX Material properties contain a non-finite value",
            ))
        }
    }
}

fn validate_name(name: &str, field: &str) -> Result<()> {
    if name.len() > MAXIMUM_NAME_BYTES {
        return Err(Error::invalid_data(format!(
            "FBX {field} name is {} bytes, exceeding limit {MAXIMUM_NAME_BYTES}",
            name.len()
        )));
    }
    Ok(())
}

fn indexed_id(base: i64, index: usize, field: &str) -> Result<i64> {
    base.checked_add(
        i64::try_from(index)
            .map_err(|_| Error::invalid_data(format!("{field} index does not fit in i64")))?,
    )
    .ok_or_else(|| Error::invalid_data(format!("{field} ID overflowed")))
}

fn material_id(reference: MaterialReference) -> Result<i64> {
    match reference {
        MaterialReference::Default => Ok(DEFAULT_MATERIAL_ID),
        MaterialReference::Model(index) => indexed_id(MATERIAL_ID_BASE, index, "FBX material"),
    }
}

fn polygon_end(index: u32) -> i64 {
    -(i64::from(index) + 1)
}

fn write_array_value(
    output: &mut impl Write,
    first: &mut bool,
    value: impl fmt::Display,
) -> io::Result<()> {
    if !*first {
        output.write_all(b",")?;
    }
    *first = false;
    write!(output, "{value}")
}

fn reserve_vec<T>(length: usize, field: &str) -> Result<Vec<T>> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|error| Error::invalid_data(format!("cannot allocate {field}: {error}")))?;
    Ok(output)
}

struct SanitizedName<'a>(&'a str, &'static str);

impl fmt::Display for SanitizedName<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut emitted = false;
        for character in self.0.chars() {
            if character.is_ascii_alphanumeric() || matches!(character, ' ' | '_' | '-' | '.') {
                character.fmt(formatter)?;
            } else {
                formatter.write_str("_")?;
            }
            emitted = true;
        }
        if !emitted {
            formatter.write_str(self.1)?;
        }
        Ok(())
    }
}

struct FbxFloat(f32);

impl fmt::Display for FbxFloat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0.0 {
            formatter.write_str("0")
        } else {
            self.0.fmt(formatter)
        }
    }
}

struct FbxDouble(f64);

impl fmt::Display for FbxDouble {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0.0 {
            formatter.write_str("0")
        } else {
            self.0.fmt(formatter)
        }
    }
}

struct BoundedWriter<'a, W> {
    inner: &'a mut W,
    maximum: u64,
    written: u64,
    limit_exceeded: bool,
}

impl<'a, W> BoundedWriter<'a, W> {
    const fn new(inner: &'a mut W, maximum: u64) -> Self {
        Self {
            inner,
            maximum,
            written: 0,
            limit_exceeded: false,
        }
    }
}

impl<W: Write> Write for BoundedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("ASCII FBX write length does not fit in u64"))?;
        let end = self
            .written
            .checked_add(length)
            .ok_or_else(|| io::Error::other("ASCII FBX output length overflowed"))?;
        if end > self.maximum {
            self.limit_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "ASCII FBX output byte limit exceeded",
            ));
        }
        let written = self.inner.write(bytes)?;
        self.written = self
            .written
            .checked_add(u64::try_from(written).expect("write count fits u64"))
            .ok_or_else(|| io::Error::other("ASCII FBX output length overflowed"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use crate::mesh::{
        Mesh, MeshBlendShapeChannel, MeshBlendShapeFrame, MeshBlendShapeVertex, MeshBlendShapes,
        MeshBoneWeight, MeshSubMesh,
    };
    use crate::model_animation::{
        ModelAnimationClip, ModelAnimationSet, ModelAnimationTrack, ModelBlendShapeTrack,
        ModelScalarKeyframe, ModelVectorKeyframe,
    };
    use crate::model_ir::{
        ModelIr, ModelLocalTransform, ModelMesh, ModelNode, ModelRendererBinding, ModelRendererKind,
    };
    use crate::scene::{Quaternion, Vector3};
    use crate::scene_hierarchy::SceneObjectKey;

    use super::{
        ConvertedTransform, GeometryPlan, quaternion_to_euler_degrees, validate_mesh,
        write_geometry, write_model_ir_fbx_ascii, write_model_ir_fbx_ascii_with_animations,
    };

    #[test]
    fn quaternion_conversion_matches_the_fbx_sdk_oracle() {
        let euler = quaternion_to_euler_degrees([0.2, -0.3, 0.1, 0.927_361_85]).unwrap();
        assert!((euler[0] - 22.791_931).abs() < 0.000_01);
        assert!((euler[1] + 36.613_724).abs() < 0.000_01);
        assert!((euler[2] - 4.678_685_7).abs() < 0.000_01);

        let positive_lock =
            quaternion_to_euler_degrees([-0.270_598_05, 0.653_281_5, 0.270_598_05, 0.653_281_5])
                .unwrap();
        assert!((positive_lock[0] + 45.0).abs() < 0.000_01);
        assert!((positive_lock[1] - 90.0).abs() < 0.000_01);
        assert!(positive_lock[2].abs() < 0.000_01);

        let negative_lock =
            quaternion_to_euler_degrees([-0.270_598_05, -0.653_281_5, -0.270_598_05, 0.653_281_5])
                .unwrap();
        assert!((negative_lock[0] + 45.0).abs() < 0.000_01);
        assert!((negative_lock[1] + 90.0).abs() < 0.000_01);
        assert!(negative_lock[2].abs() < 0.000_01);
    }

    #[test]
    fn accepts_general_triangle_lists_and_rejects_corrupt_attributes() {
        let mut mesh = Mesh {
            path_id: 1,
            name: "quad".to_owned(),
            vertices: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            normals: Some(vec![[0.0, 0.0, 1.0]; 4]),
            uv0: Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
            bind_poses: Vec::new(),
            bone_name_hashes: Vec::new(),
            root_bone_name_hash: 0,
            skin: None,
            blend_shapes: None,
            sub_meshes: vec![
                MeshSubMesh {
                    first_byte: 0,
                    index_count: 3,
                    first_vertex: 0,
                    vertex_count: 4,
                    indices: vec![0, 1, 2],
                },
                MeshSubMesh {
                    first_byte: 6,
                    index_count: 3,
                    first_vertex: 0,
                    vertex_count: 4,
                    indices: vec![0, 2, 3],
                },
            ],
        };
        validate_mesh(&mesh).unwrap();
        let geometry = GeometryPlan {
            id: 2_000_000_000,
            model_id: 1_000_000_000,
            mesh: &mesh,
            material_ids: vec![3_000_000_000, 3_000_000_001],
            submesh_material_slots: vec![0, 1],
            skin: None,
            morph: None,
        };
        let mut output = Vec::new();
        write_geometry(&geometry, &mut output).unwrap();
        let text = std::str::from_utf8(&output).unwrap();
        assert!(text.contains("Vertices: *12"));
        assert!(text.contains("a: 2,1,-1,3,2,-1"));
        assert!(text.contains("MappingInformationType: \"ByPolygon\""));
        assert!(text.contains("Materials: *2"));
        assert!(text.contains("a: 0,1"));

        mesh.uv0.as_mut().unwrap().pop();
        assert!(validate_mesh(&mesh).is_err());
    }

    #[test]
    fn converts_non_identity_unity_trs() {
        let converted = ConvertedTransform::from_unity(crate::model_ir::ModelLocalTransform {
            component: SceneObjectKey {
                file_index: 0,
                path_id: 1,
            },
            local_rotation: Quaternion {
                x: 0.0,
                y: 0.0,
                z: 0.382_683_43,
                w: 0.923_879_5,
            },
            local_position: Vector3 {
                x: 2.0,
                y: 3.0,
                z: 4.0,
            },
            local_scale: Vector3 {
                x: 2.0,
                y: 3.0,
                z: 4.0,
            },
        })
        .unwrap();
        assert_eq!(
            converted.translation.map(f32::to_bits),
            [-2.0_f32, 3.0, 4.0].map(f32::to_bits)
        );
        assert!((converted.rotation[2] + 45.0).abs() < 0.000_01);
        assert_eq!(
            converted.scale.map(f32::to_bits),
            [2.0_f32, 3.0, 4.0].map(f32::to_bits)
        );
    }

    #[test]
    fn writes_skin_clusters_with_direct_bones_and_bind_poses() {
        let model = skinned_model_fixture();
        let mut output = Vec::new();
        write_model_ir_fbx_ascii(&model, &mut output, 128 * 1024).unwrap();
        let text = std::str::from_utf8(&output).unwrap();

        assert!(text.contains("ObjectType: \"Deformer\" { Count: 3 }"));
        assert!(text.contains("Model::bone0\", \"LimbNode\""));
        assert!(text.contains("Model::bone1\", \"LimbNode\""));
        assert!(text.contains("Deformer::skin\", \"Skin\""));
        assert!(text.contains("SubDeformer::bone0Cluster\", \"Cluster\""));
        assert!(text.contains("Indexes: *2 {\n            a: 0,1"));
        assert!(text.contains("Weights: *2 {\n            a: 1,0.25"));
        assert!(text.contains("Indexes: *2 {\n            a: 1,2"));
        assert!(text.contains("Weights: *2 {\n            a: 0.75,1"));
        assert!(text.contains("a: 1,0,0,-12,0,1,0,0,0,0,1,0,0,0,0,1"));
        assert!(text.contains("a: 1,0,0,-11,0,1,0,0,0,0,1,0,0,0,0,1"));
        assert!(text.contains("C: \"OO\",4000000000,2000000000"));
        assert!(text.contains("C: \"OO\",5000000000,4000000000"));
        assert!(text.contains("C: \"OO\",1000000002,5000000000"));
        assert!(text.contains("C: \"OO\",1000000003,5000000001"));
    }

    #[test]
    fn writes_blend_shape_geometry_channels_weights_and_connections() {
        let mut model = skinned_model_fixture();
        model.meshes[0].mesh.blend_shapes = Some(MeshBlendShapes {
            vertices: vec![MeshBlendShapeVertex {
                vertex: [0.5, 0.0, 0.0],
                normal: [0.0, 0.25, 0.0],
                tangent: [0.0; 3],
                index: 1,
            }],
            frames: vec![MeshBlendShapeFrame {
                first_vertex: 0,
                vertex_count: 1,
                has_normals: true,
                has_tangents: false,
            }],
            channels: vec![MeshBlendShapeChannel {
                name: "face.Smile".to_owned(),
                name_hash: 0x1234_5678,
                frame_index: 0,
                frame_count: 1,
            }],
            full_weights: vec![100.0],
        });
        let mut output = Vec::new();
        write_model_ir_fbx_ascii(&model, &mut output, 256 * 1024).unwrap();
        let text = std::str::from_utf8(&output).unwrap();

        assert!(text.contains("ObjectType: \"Geometry\" { Count: 2 }"));
        assert!(text.contains("ObjectType: \"Deformer\" { Count: 5 }"));
        assert!(text.contains(
            "Geometry: 12000000000, \"Geometry::Smile\", \"Shape\" {\n        Version: 100"
        ));
        assert!(text.contains("Indexes: *1 {\n            a: 1"));
        // Shape vertices are per-index offsets, so this is the mirrored delta
        // alone. Base control point 1 is (1, 0, 0), so an absolute position
        // would read -1.5 here and land the target at twice its displacement.
        assert!(text.contains("Vertices: *3 {\n            a: -0.5,0,0"));
        assert!(!text.contains("a: -1.5,0,0"));
        assert!(text.contains("Normals: *3 {\n            a: 0,0.25,0"));
        assert!(
            text.contains("Deformer: 10000000000, \"Deformer::skinBlendShape\", \"BlendShape\"")
        );
        assert!(
            text.contains("Deformer: 11000000000, \"SubDeformer::Smile\", \"BlendShapeChannel\"")
        );
        assert!(text.contains("FullWeights: *1 {\n            a: 100"));
        assert!(text.contains("C: \"OO\",10000000000,2000000000"));
        assert!(text.contains("C: \"OO\",11000000000,10000000000"));
        assert!(text.contains("C: \"OO\",12000000000,11000000000"));
    }

    #[test]
    fn rejects_unresolved_bones_and_singular_bind_poses() {
        let mut model = skinned_model_fixture();
        let ModelRendererKind::SkinnedMeshRenderer { bones } =
            &mut model.nodes[1].renderers[0].kind
        else {
            panic!("fixture renderer must be skinned");
        };
        bones.fill(None);
        let error = write_model_ir_fbx_ascii(&model, &mut Vec::new(), 128 * 1024).unwrap_err();
        assert!(matches!(error, crate::Error::Unsupported(message) if message.contains("neither")));

        let mut model = skinned_model_fixture();
        model.meshes[0].mesh.bind_poses[0] = [0.0; 16];
        let error = write_model_ir_fbx_ascii(&model, &mut Vec::new(), 128 * 1024).unwrap_err();
        assert!(
            matches!(error, crate::Error::InvalidData(message) if message.contains("singular"))
        );
    }

    #[test]
    fn falls_back_to_bone_name_hashes_when_they_resolve_more_bones() {
        let mut model = skinned_model_fixture();
        let ModelRendererKind::SkinnedMeshRenderer { bones } =
            &mut model.nodes[1].renderers[0].kind
        else {
            panic!("fixture renderer must be skinned");
        };
        *bones = vec![Some(key(13)), None];
        model.meshes[0].mesh.bone_name_hashes =
            vec![super::unity_crc32(b"bone1"), super::unity_crc32(b"bone0")];
        let mut output = Vec::new();
        write_model_ir_fbx_ascii(&model, &mut output, 128 * 1024).unwrap();
        let text = std::str::from_utf8(&output).unwrap();

        assert!(text.contains("C: \"OO\",1000000003,5000000000"));
        assert!(text.contains("C: \"OO\",1000000002,5000000001"));
        assert!(text.contains("SubDeformer::bone1Cluster"));
        assert!(text.contains("SubDeformer::bone0Cluster"));
    }

    #[test]
    fn writes_animation_stacks_curves_times_and_connections() {
        let model = skinned_model_fixture();
        let animations = animation_fixture();
        let mut output = Vec::new();
        write_model_ir_fbx_ascii_with_animations(
            &model,
            Some(&animations),
            &mut output,
            256 * 1024,
        )
        .unwrap();
        let text = std::str::from_utf8(&output).unwrap();

        assert!(text.contains("ObjectType: \"AnimationStack\" { Count: 1 }"));
        assert!(text.contains("ObjectType: \"AnimationCurveNode\" { Count: 3 }"));
        assert!(text.contains("ObjectType: \"AnimationCurve\" { Count: 9 }"));
        assert!(text.contains("AnimationStack: 6000000000, \"AnimStack::Walk\""));
        assert!(text.contains("P: \"LocalStop\", \"KTime\", \"Time\", \"\",46186158000"));
        assert!(text.contains("KeyTime: *1 {\n            a: 23093079000"));
        assert!(text.contains("KeyTime: *2 {\n            a: 0,46186158000"));
        assert!(text.contains("KeyValueFloat: *2 {\n            a: -1,-4"));
        assert!(text.contains("AnimCurve::R_X\", \"\" {\n        Default: 0"));
        assert!(text.contains("AnimCurve::S_X\", \"\" {\n        Default: 1"));
        assert!(text.contains("C: \"OO\",7000000000,6000000000"));
        assert!(text.contains("C: \"OP\",8000000000,1000000001,\"Lcl Scaling\""));
        assert!(text.contains("C: \"OP\",9000000000,8000000000,\"d|X\""));
        assert!(text.contains("C: \"OP\",8000000001,1000000001,\"Lcl Rotation\""));
        assert!(text.contains("C: \"OP\",8000000002,1000000001,\"Lcl Translation\""));
    }

    #[test]
    fn writes_blend_shape_deform_percent_animation() {
        let mut model = skinned_model_fixture();
        attach_blend_shape(&mut model);
        let mut animations = animation_fixture();
        animations.clips[0].blend_shapes.push(ModelBlendShapeTrack {
            node: key(2),
            channel: "Smile".to_owned(),
            keys: vec![
                ModelScalarKeyframe {
                    time: 0.0,
                    value: 0.0,
                },
                ModelScalarKeyframe {
                    time: 1.0,
                    value: 75.0,
                },
            ],
        });
        let mut output = Vec::new();
        write_model_ir_fbx_ascii_with_animations(
            &model,
            Some(&animations),
            &mut output,
            256 * 1024,
        )
        .unwrap();
        let text = std::str::from_utf8(&output).unwrap();

        assert!(text.contains("ObjectType: \"AnimationCurveNode\" { Count: 4 }"));
        assert!(text.contains("ObjectType: \"AnimationCurve\" { Count: 10 }"));
        assert!(text.contains("AnimationCurveNode: 8000000003, \"AnimCurveNode::Smile\", \"\""));
        assert!(text.contains("P: \"d|DeformPercent\", \"Number\", \"\", \"A\",0"));
        assert!(text.contains("AnimationCurve: 9000000009, \"AnimCurve::Smile_DeformPercent\""));
        assert!(text.contains("KeyValueFloat: *2 {\n            a: 0,75"));
        assert!(text.contains("C: \"OP\",8000000003,11000000000,\"DeformPercent\""));
        assert!(text.contains("C: \"OP\",9000000009,8000000003,\"d|DeformPercent\""));
    }

    fn animation_fixture() -> ModelAnimationSet {
        ModelAnimationSet {
            clips: vec![ModelAnimationClip {
                object: key(70),
                name: "Walk".to_owned(),
                sample_rate: 30.0,
                tracks: vec![ModelAnimationTrack {
                    node: key(2),
                    translations: vec![
                        vector_key(0.0, [-1.0, 2.0, 3.0]),
                        vector_key(1.0, [-4.0, 5.0, 6.0]),
                    ],
                    rotations: vec![vector_key(0.5, [10.0, 20.0, 30.0])],
                    scalings: vec![vector_key(0.0, [1.0, 1.0, 1.0])],
                }],
                blend_shapes: Vec::new(),
            }],
        }
    }

    fn attach_blend_shape(model: &mut ModelIr) {
        model.meshes[0].mesh.blend_shapes = Some(MeshBlendShapes {
            vertices: vec![MeshBlendShapeVertex {
                vertex: [0.5, 0.0, 0.0],
                normal: [0.0, 0.25, 0.0],
                tangent: [0.0; 3],
                index: 1,
            }],
            frames: vec![MeshBlendShapeFrame {
                first_vertex: 0,
                vertex_count: 1,
                has_normals: true,
                has_tangents: false,
            }],
            channels: vec![MeshBlendShapeChannel {
                name: "face.Smile".to_owned(),
                name_hash: 0x1234_5678,
                frame_index: 0,
                frame_count: 1,
            }],
            full_weights: vec![100.0],
        });
    }

    fn skinned_model_fixture() -> ModelIr {
        let root = key(1);
        let mesh_node = key(2);
        let bone0 = key(3);
        let bone1 = key(4);
        let mesh_key = key(51);
        let nodes = vec![
            model_node(
                root,
                None,
                vec![mesh_node, bone0],
                key(11),
                [10.0, 0.0, 0.0],
            ),
            skinned_mesh_node(mesh_node, root, mesh_key),
            model_node(bone0, Some(root), vec![bone1], key(13), [0.0, 1.0, 0.0]),
            model_node(bone1, Some(bone0), Vec::new(), key(14), [0.0, 1.0, 0.0]),
        ];
        let mut second_bind_pose = identity_matrix();
        second_bind_pose[12] = 1.0;
        let mesh = Mesh {
            path_id: mesh_key.path_id,
            name: "skin".to_owned(),
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: None,
            uv0: None,
            bind_poses: vec![identity_matrix(), second_bind_pose],
            bone_name_hashes: vec![1, 2],
            root_bone_name_hash: 0,
            skin: Some(vec![
                bone_weight([1.0, 0.0, 0.0, 0.0], [0, 0, 0, 0]),
                bone_weight([0.25, 0.75, 0.0, 0.0], [0, 1, 0, 0]),
                bone_weight([1.0, 0.0, 0.0, 0.0], [1, 0, 0, 0]),
            ]),
            blend_shapes: None,
            sub_meshes: vec![MeshSubMesh {
                first_byte: 0,
                index_count: 3,
                first_vertex: 0,
                vertex_count: 3,
                indices: vec![0, 1, 2],
            }],
        };
        ModelIr::from_test_parts(
            nodes,
            vec![root],
            vec![ModelMesh {
                object: mesh_key,
                mesh,
            }],
            Vec::new(),
        )
    }

    fn skinned_mesh_node(
        object: SceneObjectKey,
        parent: SceneObjectKey,
        mesh: SceneObjectKey,
    ) -> ModelNode {
        let mut node = model_node(object, Some(parent), Vec::new(), key(12), [2.0, 0.0, 0.0]);
        node.renderers.push(ModelRendererBinding {
            component: key(31),
            kind: ModelRendererKind::SkinnedMeshRenderer {
                bones: vec![Some(key(13)), Some(key(14))],
            },
            mesh: Some(mesh),
            materials: Vec::new(),
        });
        node
    }

    fn model_node(
        object: SceneObjectKey,
        parent: Option<SceneObjectKey>,
        children: Vec<SceneObjectKey>,
        component: SceneObjectKey,
        position: [f32; 3],
    ) -> ModelNode {
        ModelNode {
            object,
            name: match object.path_id {
                1 => "root",
                2 => "mesh",
                3 => "bone0",
                _ => "bone1",
            }
            .to_owned(),
            export_content: true,
            parent,
            children,
            transform: Some(ModelLocalTransform {
                component,
                local_rotation: Quaternion {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    w: 1.0,
                },
                local_position: Vector3 {
                    x: position[0],
                    y: position[1],
                    z: position[2],
                },
                local_scale: Vector3 {
                    x: 1.0,
                    y: 1.0,
                    z: 1.0,
                },
            }),
            renderers: Vec::new(),
            animator: None,
        }
    }

    const fn key(path_id: i64) -> SceneObjectKey {
        SceneObjectKey {
            file_index: 0,
            path_id,
        }
    }

    const fn identity_matrix() -> [f32; 16] {
        [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
    }

    const fn bone_weight(weights: [f32; 4], bone_indices: [u32; 4]) -> MeshBoneWeight {
        MeshBoneWeight {
            weights,
            bone_indices,
        }
    }

    const fn vector_key(time: f32, value: [f32; 3]) -> ModelVectorKeyframe {
        ModelVectorKeyframe { time, value }
    }
}
