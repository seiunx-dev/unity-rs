//! Maps a model onto the binary FBX node tree.
//!
//! The ASCII writer emits its scene straight to text; this builds the same
//! scene as records for [`crate::fbx_binary`] to encode. Keeping the two
//! emitters separate leaves the ASCII path, which the managed differential
//! validates, untouched.
//!
//! The scene content is the ASCII writer's own plans, so geometry, transforms,
//! material colours and connections come from code the differential already
//! covers. What is new here is only how that content is laid out as records.
//!
//! This covers models, geometry, materials, textures, deformers and the
//! connections between them, plus skin deformers, blend shapes and animation
//! stacks with their curves.

use std::io::Write;

use crate::fbx_binary::{FbxNode, FbxProperty};
use crate::fbx_scene_ascii::{GeometryPlan, MaterialProperties, StaticScene, polygon_end};
use crate::model_ir::ModelIr;
use crate::{Error, Result};

/// Cumulative bytes copied into input-derived FBX string properties before
/// the binary encoder gets a chance to enforce its final output limit.
struct SceneStringBudget {
    maximum_bytes: u64,
    used_bytes: u64,
}

impl SceneStringBudget {
    const fn new(maximum_bytes: u64) -> Self {
        Self {
            maximum_bytes,
            used_bytes: 0,
        }
    }

    fn composed(&mut self, prefix: &str, value: &str, suffix: &str, label: &str) -> Result<String> {
        let length = prefix
            .len()
            .checked_add(value.len())
            .and_then(|length| length.checked_add(suffix.len()))
            .ok_or_else(|| Error::invalid_data(format!("binary FBX {label} length overflowed")))?;
        let length_u64 = u64::try_from(length)
            .map_err(|_| Error::invalid_data(format!("binary FBX {label} length overflowed")))?;
        let total = self
            .used_bytes
            .checked_add(length_u64)
            .ok_or_else(|| Error::invalid_data("binary FBX scene string budget overflowed"))?;
        if total > self.maximum_bytes {
            return Err(Error::invalid_data(format!(
                "binary FBX scene strings need {total} bytes, exceeding the {} byte output limit",
                self.maximum_bytes
            )));
        }
        let mut output = String::new();
        output.try_reserve_exact(length).map_err(|error| {
            Error::invalid_data(format!("cannot allocate binary FBX {label}: {error}"))
        })?;
        output.push_str(prefix);
        output.push_str(value);
        output.push_str(suffix);
        self.used_bytes = total;
        Ok(output)
    }

    fn prefixed(&mut self, prefix: &str, value: &str, label: &str) -> Result<String> {
        self.composed(prefix, value, "", label)
    }

    fn copy(&mut self, value: &str, label: &str) -> Result<String> {
        self.composed("", value, "", label)
    }
}

fn reserve_exact<T>(values: &mut Vec<T>, additional: usize, label: &str) -> Result<()> {
    values.try_reserve_exact(additional).map_err(|error| {
        Error::invalid_data(format!("cannot allocate binary FBX {label}: {error}"))
    })
}

fn scaled_length(count: usize, factor: usize, label: &str) -> Result<usize> {
    count
        .checked_mul(factor)
        .ok_or_else(|| Error::invalid_data(format!("binary FBX {label} length overflowed")))
}

fn push_node(values: &mut Vec<FbxNode>, node: FbxNode, label: &str) -> Result<()> {
    values.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot allocate binary FBX {label}: {error}"))
    })?;
    values.push(node);
    Ok(())
}

fn extend_nodes(values: &mut Vec<FbxNode>, mut nodes: Vec<FbxNode>, label: &str) -> Result<()> {
    reserve_exact(values, nodes.len(), label)?;
    values.append(&mut nodes);
    Ok(())
}

fn f32_values(values: &[f32], label: &str) -> Result<Vec<f64>> {
    let mut output = Vec::new();
    reserve_exact(&mut output, values.len(), label)?;
    output.extend(values.iter().map(|value| f64::from(*value)));
    Ok(output)
}

fn matrix_values(values: &[f64; 16], label: &str) -> Result<Vec<f64>> {
    let mut output = Vec::new();
    reserve_exact(&mut output, values.len(), label)?;
    output.extend_from_slice(values);
    Ok(output)
}

/// Writes a model's static scene as binary FBX 7.4.
///
/// Refuses a model carrying anything this layout does not yet emit, rather
/// than writing a file that silently drops it.
pub fn write_model_ir_fbx_binary<W: Write>(
    model: &ModelIr,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    write_model_ir_fbx_binary_with_textures(model, None, output, maximum_output_bytes)
}

/// Writes a model's static scene with its material textures.
///
/// The records reference each texture by file name, so the caller has to write
/// the set beside the model for those references to resolve, exactly as with
/// the ASCII writer.
pub fn write_model_ir_fbx_binary_with_textures<W: Write>(
    model: &ModelIr,
    textures: Option<&crate::scene_textures::SceneTextureSet>,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    write_model_ir_fbx_binary_full(model, None, textures, output, maximum_output_bytes)
}

/// Writes a model with its animation tracks and its material textures.
pub fn write_model_ir_fbx_binary_full<W: Write>(
    model: &ModelIr,
    animations: Option<&crate::model_animation::ModelAnimationSet>,
    textures: Option<&crate::scene_textures::SceneTextureSet>,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    let scene = StaticScene::from_model(model, animations, textures, maximum_output_bytes)?;
    let mut strings = SceneStringBudget::new(maximum_output_bytes);
    let roots = build_scene_nodes(&scene, &mut strings)?;
    let bytes = crate::fbx_binary::read_fbx_binary(&roots, maximum_output_bytes)?;
    output.write_all(&bytes)?;
    u64::try_from(bytes.len()).map_err(|_| Error::invalid_data("binary FBX length overflowed"))
}

/// Materializes the same bytes under an explicit cap.
pub fn read_model_ir_fbx_binary(model: &ModelIr, maximum_output_bytes: u64) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    write_model_ir_fbx_binary(model, &mut output, maximum_output_bytes)?;
    Ok(output)
}

/// The complete top-level record list.
fn build_scene_nodes(
    scene: &StaticScene<'_>,
    strings: &mut SceneStringBudget,
) -> Result<Vec<FbxNode>> {
    let mut objects = FbxNode::new("Objects");
    for node in &scene.nodes {
        push_node(
            &mut objects.children,
            model_node(node, strings)?,
            "object records",
        )?;
    }
    for geometry in &scene.geometries {
        push_node(
            &mut objects.children,
            geometry_node(geometry, strings)?,
            "object records",
        )?;
    }
    for material in &scene.materials {
        push_node(
            &mut objects.children,
            material_node(material, strings)?,
            "object records",
        )?;
    }
    for texture in &scene.textures {
        push_node(
            &mut objects.children,
            texture_node(texture, strings)?,
            "object records",
        )?;
        push_node(
            &mut objects.children,
            video_node(texture, strings)?,
            "object records",
        )?;
    }
    for geometry in &scene.geometries {
        if let Some(morph) = &geometry.morph {
            for channel in &morph.channels {
                for shape in &channel.shapes {
                    push_node(
                        &mut objects.children,
                        shape_geometry_node(geometry.mesh, shape, strings)?,
                        "object records",
                    )?;
                }
            }
        }
    }
    for geometry in &scene.geometries {
        if let Some(skin) = &geometry.skin {
            push_node(
                &mut objects.children,
                skin_node(skin, strings)?,
                "object records",
            )?;
            for cluster in &skin.clusters {
                push_node(
                    &mut objects.children,
                    cluster_node(cluster, strings)?,
                    "object records",
                )?;
            }
        }
        if let Some(morph) = &geometry.morph {
            push_node(
                &mut objects.children,
                morph_node(morph, strings)?,
                "object records",
            )?;
            for channel in &morph.channels {
                push_node(
                    &mut objects.children,
                    morph_channel_node(channel, strings)?,
                    "object records",
                )?;
            }
        }
    }

    let mut animation_objects = Vec::new();
    for animation in &scene.animations {
        extend_nodes(
            &mut animation_objects,
            animation_nodes(animation, strings)?,
            "animation object records",
        )?;
    }
    extend_nodes(&mut objects.children, animation_objects, "object records")?;

    Ok(vec![
        header_extension(),
        global_settings(),
        documents(),
        FbxNode::new("References"),
        definitions(scene)?,
        objects,
        connections(scene)?,
        FbxNode::new("Takes")
            .child(FbxNode::new("Current").with(FbxProperty::String(String::new()))),
    ])
}

fn header_extension() -> FbxNode {
    FbxNode::new("FBXHeaderExtension")
        .child(FbxNode::new("FBXHeaderVersion").with(FbxProperty::I32(1003)))
        .child(FbxNode::new("FBXVersion").with(FbxProperty::I32(7400)))
        .child(FbxNode::new("EncryptionType").with(FbxProperty::I32(0)))
        .child(
            FbxNode::new("Creator")
                .with(FbxProperty::String("unity-rs binary FBX writer".to_owned())),
        )
}

/// The axis system, which decides how an importer orients the scene.
///
/// The same values the ASCII writer emits: Y up, Z front with a negative sign,
/// X across, and unit scale.
fn global_settings() -> FbxNode {
    let mut properties = FbxNode::new("Properties70");
    for (name, value) in [
        ("UpAxis", 1),
        ("UpAxisSign", 1),
        ("FrontAxis", 2),
        ("FrontAxisSign", -1),
        ("CoordAxis", 0),
        ("CoordAxisSign", 1),
    ] {
        properties.children.push(
            FbxNode::new("P")
                .with(FbxProperty::String(name.to_owned()))
                .with(FbxProperty::String("int".to_owned()))
                .with(FbxProperty::String("Integer".to_owned()))
                .with(FbxProperty::String(String::new()))
                .with(FbxProperty::I32(value)),
        );
    }
    properties.children.push(
        FbxNode::new("P")
            .with(FbxProperty::String("UnitScaleFactor".to_owned()))
            .with(FbxProperty::String("double".to_owned()))
            .with(FbxProperty::String("Number".to_owned()))
            .with(FbxProperty::String(String::new()))
            .with(FbxProperty::F64(1.0)),
    );
    FbxNode::new("GlobalSettings")
        .child(FbxNode::new("Version").with(FbxProperty::I32(1000)))
        .child(properties)
}

fn documents() -> FbxNode {
    FbxNode::new("Documents")
        .child(FbxNode::new("Count").with(FbxProperty::I32(1)))
        .child(
            FbxNode::new("Document")
                .with(FbxProperty::I64(1))
                .with(FbxProperty::String("Scene".to_owned()))
                .with(FbxProperty::String("Scene".to_owned()))
                .child(FbxNode::new("RootNode").with(FbxProperty::I64(0))),
        )
}

fn definitions(scene: &StaticScene<'_>) -> Result<FbxNode> {
    let mut definitions = FbxNode::new("Definitions")
        .child(FbxNode::new("Version").with(FbxProperty::I32(100)))
        .child(FbxNode::new("Count").with(FbxProperty::I32(10)));
    for (name, count) in [
        ("Model", scene.nodes.len()),
        ("Geometry", scene.geometries.len()),
        ("Material", scene.materials.len()),
        ("Texture", scene.textures.len()),
        ("Video", scene.textures.len()),
        ("Deformer", deformer_count(scene)?),
        ("AnimationStack", scene.animations.len()),
        ("AnimationLayer", scene.animations.len()),
        ("AnimationCurveNode", curve_node_count(scene)?),
        ("AnimationCurve", curve_count(scene)?),
    ] {
        let count = i32::try_from(count)
            .map_err(|_| Error::invalid_data(format!("binary FBX {name} count exceeds i32")))?;
        push_node(
            &mut definitions.children,
            FbxNode::new("ObjectType")
                .with(FbxProperty::String(name.to_owned()))
                .child(FbxNode::new("Count").with(FbxProperty::I32(count))),
            "definition records",
        )?;
    }
    Ok(definitions)
}

fn model_node(
    node: &crate::fbx_scene_ascii::NodePlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    let kind = if node.has_geometry {
        "Mesh"
    } else if node.is_bone {
        "LimbNode"
    } else {
        "Null"
    };
    let mut properties = FbxNode::new("Properties70");
    for (name, values) in [
        ("Lcl Translation", node.transform.translation),
        ("Lcl Rotation", node.transform.rotation),
        ("Lcl Scaling", node.transform.scale),
    ] {
        properties.children.push(
            FbxNode::new("P")
                .with(FbxProperty::String(name.to_owned()))
                .with(FbxProperty::String(name.to_owned()))
                .with(FbxProperty::String(String::new()))
                .with(FbxProperty::String("A".to_owned()))
                .with(FbxProperty::F64(f64::from(values[0])))
                .with(FbxProperty::F64(f64::from(values[1])))
                .with(FbxProperty::F64(f64::from(values[2]))),
        );
    }
    Ok(FbxNode::new("Model")
        .with(FbxProperty::I64(node.id))
        .with(FbxProperty::String(strings.prefixed(
            "Model::",
            node.name,
            "model name",
        )?))
        .with(FbxProperty::String(kind.to_owned()))
        .child(FbxNode::new("Version").with(FbxProperty::I32(232)))
        .child(properties)
        .child(FbxNode::new("Shading").with(FbxProperty::Bool(true)))
        .child(FbxNode::new("Culling").with(FbxProperty::String("CullingOff".to_owned()))))
}

/// One geometry record: mirrored vertices and the polygon index run.
///
/// X is mirrored and the winding reversed, exactly as the ASCII writer does,
/// and the last index of every polygon is stored as its negative-one's
/// complement, which is how FBX marks a polygon's end.
fn geometry_node(
    geometry: &crate::fbx_scene_ascii::GeometryPlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    let mesh = geometry.mesh;
    let mut vertices = Vec::new();
    reserve_exact(
        &mut vertices,
        scaled_length(mesh.vertices.len(), 3, "vertex scalar")?,
        "vertex scalars",
    )?;
    for vertex in &mesh.vertices {
        vertices.push(f64::from(-vertex[0]));
        vertices.push(f64::from(vertex[1]));
        vertices.push(f64::from(vertex[2]));
    }

    let mut indices = Vec::new();
    let index_count = mesh.sub_meshes.iter().try_fold(0_usize, |total, submesh| {
        total
            .checked_add(submesh.indices.len())
            .ok_or_else(|| Error::invalid_data("binary FBX polygon index count overflowed"))
    })?;
    reserve_exact(&mut indices, index_count, "polygon indices")?;
    for submesh in &mesh.sub_meshes {
        for triangle in submesh.indices.chunks_exact(3) {
            indices.push(
                i32::try_from(triangle[2]).map_err(|_| {
                    Error::invalid_data("binary FBX polygon index does not fit i32")
                })?,
            );
            indices.push(
                i32::try_from(triangle[1]).map_err(|_| {
                    Error::invalid_data("binary FBX polygon index does not fit i32")
                })?,
            );
            indices.push(
                i32::try_from(polygon_end(triangle[0])).map_err(|_| {
                    Error::invalid_data("binary FBX polygon index does not fit i32")
                })?,
            );
        }
    }

    Ok(FbxNode::new("Geometry")
        .with(FbxProperty::I64(geometry.id))
        .with(FbxProperty::String(strings.prefixed(
            "Geometry::",
            &mesh.name,
            "geometry name",
        )?))
        .with(FbxProperty::String("Mesh".to_owned()))
        .child(FbxNode::new("GeometryVersion").with(FbxProperty::I32(124)))
        .child(FbxNode::new("Vertices").with(FbxProperty::F64Array(vertices)))
        .child(FbxNode::new("PolygonVertexIndex").with(FbxProperty::I32Array(indices))))
}

fn material_node(
    material: &crate::fbx_scene_ascii::MaterialPlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    let name = material
        .material
        .map_or("DefaultMaterial", |material| &material.name);
    let colours = MaterialProperties::from_material(material.material);
    let mut properties = FbxNode::new("Properties70");
    for (property, value) in [
        ("DiffuseColor", colours.diffuse),
        ("AmbientColor", colours.ambient),
        ("EmissiveColor", colours.emissive),
        ("SpecularColor", colours.specular),
        ("ReflectionColor", colours.reflection),
    ] {
        properties.children.push(
            FbxNode::new("P")
                .with(FbxProperty::String(property.to_owned()))
                .with(FbxProperty::String("Color".to_owned()))
                .with(FbxProperty::String(String::new()))
                .with(FbxProperty::String("A".to_owned()))
                .with(FbxProperty::F64(f64::from(value[0])))
                .with(FbxProperty::F64(f64::from(value[1])))
                .with(FbxProperty::F64(f64::from(value[2]))),
        );
    }
    for (property, value) in [
        ("Shininess", colours.shininess),
        ("TransparencyFactor", colours.transparency),
    ] {
        properties.children.push(
            FbxNode::new("P")
                .with(FbxProperty::String(property.to_owned()))
                .with(FbxProperty::String("double".to_owned()))
                .with(FbxProperty::String("Number".to_owned()))
                .with(FbxProperty::String("A".to_owned()))
                .with(FbxProperty::F64(f64::from(value))),
        );
    }
    Ok(FbxNode::new("Material")
        .with(FbxProperty::I64(material.id))
        .with(FbxProperty::String(strings.prefixed(
            "Material::",
            name,
            "material name",
        )?))
        .with(FbxProperty::String(String::new()))
        .child(FbxNode::new("Version").with(FbxProperty::I32(102)))
        .child(FbxNode::new("ShadingModel").with(FbxProperty::String("phong".to_owned())))
        .child(FbxNode::new("MultiLayer").with(FbxProperty::I32(0)))
        .child(properties))
}

/// One `Texture` record, carrying the UV transform FBX keeps on the texture
/// rather than on the binding.
fn texture_node(
    texture: &crate::fbx_scene_ascii::TexturePlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    let mut properties = FbxNode::new("Properties70");
    properties.children.push(
        FbxNode::new("P")
            .with(FbxProperty::String("UVSet".to_owned()))
            .with(FbxProperty::String("KString".to_owned()))
            .with(FbxProperty::String(String::new()))
            .with(FbxProperty::String(String::new()))
            .with(FbxProperty::String("UVChannel_0".to_owned())),
    );
    properties.children.push(
        FbxNode::new("P")
            .with(FbxProperty::String("UseMaterial".to_owned()))
            .with(FbxProperty::String("bool".to_owned()))
            .with(FbxProperty::String(String::new()))
            .with(FbxProperty::String(String::new()))
            .with(FbxProperty::I32(1)),
    );
    for (name, values, third) in [
        ("Translation", texture.translation, 0.0),
        ("Scaling", texture.scaling, 1.0),
    ] {
        properties.children.push(
            FbxNode::new("P")
                .with(FbxProperty::String(name.to_owned()))
                .with(FbxProperty::String("Vector".to_owned()))
                .with(FbxProperty::String(String::new()))
                .with(FbxProperty::String("A".to_owned()))
                .with(FbxProperty::F64(f64::from(values[0])))
                .with(FbxProperty::F64(f64::from(values[1])))
                .with(FbxProperty::F64(third)),
        );
    }
    let texture_name = strings.prefixed("Texture::", texture.file_name, "texture object name")?;
    let texture_property_name =
        strings.prefixed("Texture::", texture.file_name, "texture property name")?;
    let media_name = strings.prefixed("Video::", texture.file_name, "texture media name")?;
    let file_name = strings.copy(texture.file_name, "texture file name")?;
    let relative_file_name = strings.copy(texture.file_name, "texture relative file name")?;
    Ok(FbxNode::new("Texture")
        .with(FbxProperty::I64(texture.id))
        .with(FbxProperty::String(texture_name))
        .with(FbxProperty::String(String::new()))
        .child(FbxNode::new("Type").with(FbxProperty::String("TextureVideoClip".to_owned())))
        .child(FbxNode::new("Version").with(FbxProperty::I32(202)))
        .child(FbxNode::new("TextureName").with(FbxProperty::String(texture_property_name)))
        .child(properties)
        .child(FbxNode::new("Media").with(FbxProperty::String(media_name)))
        .child(FbxNode::new("FileName").with(FbxProperty::String(file_name)))
        .child(FbxNode::new("RelativeFilename").with(FbxProperty::String(relative_file_name))))
}

/// The `Video` clip a `Texture` reads its bytes through.
fn video_node(
    texture: &crate::fbx_scene_ascii::TexturePlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    let path = strings.copy(texture.file_name, "video path")?;
    let mut properties = FbxNode::new("Properties70");
    properties.children.push(
        FbxNode::new("P")
            .with(FbxProperty::String("Path".to_owned()))
            .with(FbxProperty::String("KString".to_owned()))
            .with(FbxProperty::String("XRefUrl".to_owned()))
            .with(FbxProperty::String(String::new()))
            .with(FbxProperty::String(path)),
    );
    let video_name = strings.prefixed("Video::", texture.file_name, "video object name")?;
    let file_name = strings.copy(texture.file_name, "video file name")?;
    let relative_file_name = strings.copy(texture.file_name, "video relative file name")?;
    Ok(FbxNode::new("Video")
        .with(FbxProperty::I64(texture.video_id))
        .with(FbxProperty::String(video_name))
        .with(FbxProperty::String("Clip".to_owned()))
        .child(FbxNode::new("Type").with(FbxProperty::String("Clip".to_owned())))
        .child(properties)
        .child(FbxNode::new("UseMipMap").with(FbxProperty::I32(0)))
        .child(FbxNode::new("Filename").with(FbxProperty::String(file_name)))
        .child(FbxNode::new("RelativeFilename").with(FbxProperty::String(relative_file_name))))
}

/// Every skin and cluster record the scene will emit.
fn deformer_count(scene: &StaticScene<'_>) -> Result<usize> {
    scene
        .geometries
        .iter()
        .try_fold(0_usize, |total, geometry| {
            let skin_count = geometry
                .skin
                .as_ref()
                .map_or(0, |skin| skin.clusters.len().saturating_add(1));
            let morph_count = geometry
                .morph
                .as_ref()
                .map_or(0, |morph| morph.channels.len().saturating_add(1));
            total
                .checked_add(skin_count)
                .and_then(|value| value.checked_add(morph_count))
                .ok_or_else(|| Error::invalid_data("binary FBX deformer count overflowed"))
        })
}

/// The skin deformer a mesh's clusters hang from.
fn skin_node(
    skin: &crate::fbx_scene_ascii::SkinPlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    Ok(FbxNode::new("Deformer")
        .with(FbxProperty::I64(skin.id))
        .with(FbxProperty::String(strings.prefixed(
            "Deformer::",
            skin.name,
            "skin name",
        )?))
        .with(FbxProperty::String("Skin".to_owned()))
        .child(FbxNode::new("Version").with(FbxProperty::I32(101)))
        .child(FbxNode::new("Link_DeformAcuracy").with(FbxProperty::F64(50.0))))
}

/// One cluster: the vertices a bone influences, their weights, and the two
/// matrices that place the bone against the mesh at bind time.
fn cluster_node(
    cluster: &crate::fbx_scene_ascii::ClusterPlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    let mut indices = Vec::new();
    reserve_exact(&mut indices, cluster.indices.len(), "cluster indices")?;
    indices.extend(
        cluster
            .indices
            .iter()
            .map(|index| i32::try_from(*index).unwrap_or(i32::MAX)),
    );
    let weights = f32_values(&cluster.weights, "cluster weights")?;
    let transform = matrix_values(&cluster.transform.0, "cluster transform")?;
    let transform_link = matrix_values(&cluster.transform_link.0, "cluster link transform")?;
    Ok(FbxNode::new("Deformer")
        .with(FbxProperty::I64(cluster.id))
        .with(FbxProperty::String(strings.composed(
            "SubDeformer::",
            cluster.bone_name,
            "Cluster",
            "skin cluster name",
        )?))
        .with(FbxProperty::String("Cluster".to_owned()))
        .child(FbxNode::new("Version").with(FbxProperty::I32(100)))
        .child(
            FbxNode::new("UserData")
                .with(FbxProperty::String(String::new()))
                .with(FbxProperty::String(String::new())),
        )
        .child(FbxNode::new("Indexes").with(FbxProperty::I32Array(indices)))
        .child(FbxNode::new("Weights").with(FbxProperty::F64Array(weights)))
        .child(FbxNode::new("Transform").with(FbxProperty::F64Array(transform)))
        .child(FbxNode::new("TransformLink").with(FbxProperty::F64Array(transform_link))))
}

/// The blend-shape deformer a mesh's channels hang from.
fn morph_node(
    morph: &crate::fbx_scene_ascii::MorphPlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    Ok(FbxNode::new("Deformer")
        .with(FbxProperty::I64(morph.id))
        .with(FbxProperty::String(strings.composed(
            "Deformer::",
            morph.name,
            "BlendShape",
            "blend-shape name",
        )?))
        .with(FbxProperty::String("BlendShape".to_owned()))
        .child(FbxNode::new("Version").with(FbxProperty::I32(100))))
}

/// One channel: a named target and the weights at which its shapes apply.
fn morph_channel_node(
    channel: &crate::fbx_scene_ascii::MorphChannelPlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    let weights = f32_values(channel.full_weights, "blend-shape weights")?;
    Ok(FbxNode::new("Deformer")
        .with(FbxProperty::I64(channel.id))
        .with(FbxProperty::String(strings.prefixed(
            "SubDeformer::",
            channel.name,
            "blend-shape channel name",
        )?))
        .with(FbxProperty::String("BlendShapeChannel".to_owned()))
        .child(FbxNode::new("Version").with(FbxProperty::I32(100)))
        .child(FbxNode::new("DeformPercent").with(FbxProperty::F64(0.0)))
        .child(FbxNode::new("FullWeights").with(FbxProperty::F64Array(weights))))
}

/// One target shape's geometry.
///
/// FBX stores a target as per-index offsets from the base control points named
/// in `Indexes`, not as absolute positions: an importer adds these to the base
/// vertex. X is mirrored to match the base geometry.
fn shape_geometry_node(
    mesh: &crate::mesh::Mesh,
    shape: &crate::fbx_scene_ascii::ShapePlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<FbxNode> {
    let mut indices = Vec::new();
    reserve_exact(&mut indices, shape.vertices.len(), "target-shape indices")?;
    let mut offsets = Vec::new();
    reserve_exact(
        &mut offsets,
        scaled_length(shape.vertices.len(), 3, "target-shape offset")?,
        "target-shape offsets",
    )?;
    for vertex in shape.vertices {
        let index = usize::try_from(vertex.index)
            .map_err(|_| Error::invalid_data("FBX target-shape vertex index does not fit usize"))?;
        if index >= mesh.vertices.len() {
            return Err(Error::invalid_data(
                "FBX target-shape vertex index is out of range",
            ));
        }
        indices.push(
            i32::try_from(vertex.index).map_err(|_| {
                Error::invalid_data("FBX target-shape vertex index does not fit i32")
            })?,
        );
        offsets.push(f64::from(-vertex.vertex[0]));
        offsets.push(f64::from(vertex.vertex[1]));
        offsets.push(f64::from(vertex.vertex[2]));
    }
    let mut node = FbxNode::new("Geometry")
        .with(FbxProperty::I64(shape.id))
        .with(FbxProperty::String(strings.prefixed(
            "Geometry::",
            &shape.name,
            "target-shape name",
        )?))
        .with(FbxProperty::String("Shape".to_owned()))
        .child(FbxNode::new("Version").with(FbxProperty::I32(100)))
        .child(FbxNode::new("Indexes").with(FbxProperty::I32Array(indices)))
        .child(FbxNode::new("Vertices").with(FbxProperty::F64Array(offsets)));
    if shape.frame.has_normals {
        let mut normals = Vec::new();
        reserve_exact(
            &mut normals,
            scaled_length(shape.vertices.len(), 3, "target-shape normal")?,
            "target-shape normals",
        )?;
        for vertex in shape.vertices {
            normals.push(f64::from(-vertex.normal[0]));
            normals.push(f64::from(vertex.normal[1]));
            normals.push(f64::from(vertex.normal[2]));
        }
        node = node.child(FbxNode::new("Normals").with(FbxProperty::F64Array(normals)));
    }
    Ok(node)
}

/// One curve node per animated property, plus one per blend-shape channel.
fn curve_node_count(scene: &StaticScene<'_>) -> Result<usize> {
    scene
        .animations
        .iter()
        .try_fold(0_usize, |total, animation| {
            total
                .checked_add(animation.properties.len())
                .and_then(|value| value.checked_add(animation.blend_shapes.len()))
                .ok_or_else(|| {
                    Error::invalid_data("binary FBX animation curve-node count overflowed")
                })
        })
}

/// Three curves per vector property, one per blend-shape channel.
fn curve_count(scene: &StaticScene<'_>) -> Result<usize> {
    scene
        .animations
        .iter()
        .try_fold(0_usize, |total, animation| {
            let property_curves =
                scaled_length(animation.properties.len(), 3, "animation property-curve")?;
            total
                .checked_add(property_curves)
                .and_then(|value| value.checked_add(animation.blend_shapes.len()))
                .ok_or_else(|| Error::invalid_data("binary FBX animation curve count overflowed"))
        })
}

/// A clip's stack, its layer, and every curve node and curve beneath them.
fn animation_nodes(
    animation: &crate::fbx_scene_ascii::AnimationPlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<Vec<FbxNode>> {
    let mut nodes = Vec::new();
    let property_nodes = scaled_length(animation.properties.len(), 4, "animation property object")?;
    let blend_shape_nodes = scaled_length(
        animation.blend_shapes.len(),
        2,
        "blend-shape animation object",
    )?;
    let node_count = 2_usize
        .checked_add(property_nodes)
        .and_then(|value| value.checked_add(blend_shape_nodes))
        .ok_or_else(|| Error::invalid_data("binary FBX animation object count overflowed"))?;
    reserve_exact(&mut nodes, node_count, "animation object records")?;
    let mut stack_properties = FbxNode::new("Properties70");
    for (name, value) in [("LocalStart", 0), ("LocalStop", animation.stop_time)] {
        stack_properties.children.push(
            FbxNode::new("P")
                .with(FbxProperty::String(name.to_owned()))
                .with(FbxProperty::String("KTime".to_owned()))
                .with(FbxProperty::String("Time".to_owned()))
                .with(FbxProperty::String(String::new()))
                .with(FbxProperty::I64(value)),
        );
    }
    push_node(
        &mut nodes,
        FbxNode::new("AnimationStack")
            .with(FbxProperty::I64(animation.stack_id))
            .with(FbxProperty::String(strings.prefixed(
                "AnimStack::",
                animation.name,
                "animation stack name",
            )?))
            .with(FbxProperty::String(String::new()))
            .child(stack_properties),
        "animation object records",
    )?;
    push_node(
        &mut nodes,
        FbxNode::new("AnimationLayer")
            .with(FbxProperty::I64(animation.layer_id))
            .with(FbxProperty::String("AnimLayer::Base Layer".to_owned()))
            .with(FbxProperty::String(String::new()))
            .child(FbxNode::new("Version").with(FbxProperty::I32(100)))
            .child(FbxNode::new("Weight").with(FbxProperty::F64(100.0)))
            .child(FbxNode::new("BlendMode").with(FbxProperty::I32(0))),
        "animation object records",
    )?;

    extend_nodes(
        &mut nodes,
        property_animation_nodes(animation, strings)?,
        "animation object records",
    )?;
    extend_nodes(
        &mut nodes,
        blend_shape_animation_nodes(animation, strings)?,
        "animation object records",
    )?;
    Ok(nodes)
}

/// One curve node and three curves per animated vector property.
fn property_animation_nodes(
    animation: &crate::fbx_scene_ascii::AnimationPlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<Vec<FbxNode>> {
    let mut nodes = Vec::new();
    reserve_exact(
        &mut nodes,
        scaled_length(animation.properties.len(), 4, "animation property object")?,
        "animation property records",
    )?;
    for property in &animation.properties {
        let token = property.kind.token();
        let defaults = property.kind.defaults();
        let mut curve_properties = FbxNode::new("Properties70");
        for (component, default) in defaults.iter().enumerate() {
            curve_properties.children.push(
                FbxNode::new("P")
                    .with(FbxProperty::String(format!(
                        "d|{}",
                        crate::fbx_scene_ascii::animation_component_name(component)
                    )))
                    .with(FbxProperty::String("Number".to_owned()))
                    .with(FbxProperty::String(String::new()))
                    .with(FbxProperty::String("A".to_owned()))
                    .with(FbxProperty::F64(f64::from(*default))),
            );
        }
        push_node(
            &mut nodes,
            FbxNode::new("AnimationCurveNode")
                .with(FbxProperty::I64(property.node_id))
                .with(FbxProperty::String(strings.prefixed(
                    "AnimCurveNode::",
                    token,
                    "animation curve-node name",
                )?))
                .with(FbxProperty::String(String::new()))
                .child(curve_properties),
            "animation property records",
        )?;
        for (component, id) in property.curve_ids.iter().enumerate() {
            let mut times = Vec::new();
            reserve_exact(&mut times, property.keys.len(), "animation key times")?;
            let mut values = Vec::new();
            reserve_exact(&mut values, property.keys.len(), "animation key values")?;
            for key in property.keys {
                times.push(crate::fbx_scene_ascii::fbx_key_time(key.time)?);
                values.push(key.value[component]);
            }
            let suffix = match component {
                0 => "_X",
                1 => "_Y",
                _ => "_Z",
            };
            let name = strings.composed("AnimCurve::", token, suffix, "animation curve name")?;
            push_node(
                &mut nodes,
                curve_node(*id, name, defaults[component], times, values),
                "animation property records",
            )?;
        }
    }

    Ok(nodes)
}

/// One curve node and one curve per animated blend-shape channel.
fn blend_shape_animation_nodes(
    animation: &crate::fbx_scene_ascii::AnimationPlan<'_>,
    strings: &mut SceneStringBudget,
) -> Result<Vec<FbxNode>> {
    let mut nodes = Vec::new();
    reserve_exact(
        &mut nodes,
        scaled_length(
            animation.blend_shapes.len(),
            2,
            "blend-shape animation object",
        )?,
        "blend-shape animation records",
    )?;
    for blend_shape in &animation.blend_shapes {
        push_node(
            &mut nodes,
            FbxNode::new("AnimationCurveNode")
                .with(FbxProperty::I64(blend_shape.node_id))
                .with(FbxProperty::String(strings.prefixed(
                    "AnimCurveNode::",
                    blend_shape.channel_name,
                    "blend-shape curve-node name",
                )?))
                .with(FbxProperty::String(String::new()))
                .child(
                    FbxNode::new("Properties70").child(
                        FbxNode::new("P")
                            .with(FbxProperty::String("d|DeformPercent".to_owned()))
                            .with(FbxProperty::String("Number".to_owned()))
                            .with(FbxProperty::String(String::new()))
                            .with(FbxProperty::String("A".to_owned()))
                            .with(FbxProperty::F64(0.0)),
                    ),
                ),
            "blend-shape animation records",
        )?;
        let mut times = Vec::new();
        reserve_exact(&mut times, blend_shape.keys.len(), "blend-shape key times")?;
        let mut values = Vec::new();
        reserve_exact(
            &mut values,
            blend_shape.keys.len(),
            "blend-shape key values",
        )?;
        for key in blend_shape.keys {
            times.push(crate::fbx_scene_ascii::fbx_key_time(key.time)?);
            values.push(key.value);
        }
        push_node(
            &mut nodes,
            curve_node(
                blend_shape.curve_id,
                strings.composed(
                    "AnimCurve::",
                    blend_shape.channel_name,
                    "_DeformPercent",
                    "blend-shape curve name",
                )?,
                0.0,
                times,
                values,
            ),
            "blend-shape animation records",
        )?;
    }
    Ok(nodes)
}

/// One curve: its key times, values and the per-key attribute arrays a reader
/// expects even when every key shares the same interpolation.
fn curve_node(id: i64, name: String, default: f32, times: Vec<i64>, values: Vec<f32>) -> FbxNode {
    // 24836 is the cubic-auto flag the ASCII writer emits for every key. The
    // attribute arrays are indexed by KeyAttrRefCount runs, so one entry
    // covering every key is what a uniform curve looks like.
    const CUBIC_AUTO: i32 = 24_836;
    let key_count = i32::try_from(times.len()).unwrap_or(i32::MAX);
    FbxNode::new("AnimationCurve")
        .with(FbxProperty::I64(id))
        .with(FbxProperty::String(name))
        .with(FbxProperty::String(String::new()))
        .child(FbxNode::new("Default").with(FbxProperty::F64(f64::from(default))))
        .child(FbxNode::new("KeyVer").with(FbxProperty::I32(4008)))
        .child(FbxNode::new("KeyTime").with(FbxProperty::I64Array(times)))
        .child(FbxNode::new("KeyValueFloat").with(FbxProperty::F32Array(values)))
        .child(FbxNode::new("KeyAttrFlags").with(FbxProperty::I32Array(vec![CUBIC_AUTO])))
        .child(FbxNode::new("KeyAttrDataFloat").with(FbxProperty::F32Array(vec![0.0; 4])))
        .child(FbxNode::new("KeyAttrRefCount").with(FbxProperty::I32Array(vec![key_count])))
}

/// Object-to-object links, in the same order the ASCII writer emits them.
fn push_connection(
    records: &mut Vec<FbxNode>,
    kind: &str,
    child: i64,
    parent: i64,
    property: Option<&str>,
) -> Result<()> {
    let mut node = FbxNode::new("C")
        .with(FbxProperty::String(kind.to_owned()))
        .with(FbxProperty::I64(child))
        .with(FbxProperty::I64(parent));
    if let Some(property) = property {
        node = node.with(FbxProperty::String(property.to_owned()));
    }
    push_node(records, node, "connection records")
}

fn append_object_connections(scene: &StaticScene<'_>, records: &mut Vec<FbxNode>) -> Result<()> {
    for node in &scene.nodes {
        push_connection(records, "OO", node.id, node.parent_id, None)?;
    }
    for geometry in &scene.geometries {
        push_connection(records, "OO", geometry.id, geometry.model_id, None)?;
        for material_id in &geometry.material_ids {
            push_connection(records, "OO", *material_id, geometry.model_id, None)?;
        }
    }
    for texture in &scene.textures {
        push_connection(records, "OO", texture.video_id, texture.id, None)?;
    }
    for geometry in &scene.geometries {
        append_geometry_deformer_connections(geometry, records)?;
    }
    Ok(())
}

fn append_geometry_deformer_connections(
    geometry: &GeometryPlan<'_>,
    records: &mut Vec<FbxNode>,
) -> Result<()> {
    if let Some(skin) = &geometry.skin {
        push_connection(records, "OO", skin.id, geometry.id, None)?;
        for cluster in &skin.clusters {
            push_connection(records, "OO", cluster.id, skin.id, None)?;
            push_connection(records, "OO", cluster.bone_model_id, cluster.id, None)?;
        }
    }
    if let Some(morph) = &geometry.morph {
        push_connection(records, "OO", morph.id, geometry.id, None)?;
        for channel in &morph.channels {
            push_connection(records, "OO", channel.id, morph.id, None)?;
            for shape in &channel.shapes {
                push_connection(records, "OO", shape.id, channel.id, None)?;
            }
        }
    }
    Ok(())
}

fn animation_component_property(component: usize) -> &'static str {
    match component {
        0 => "d|X",
        1 => "d|Y",
        _ => "d|Z",
    }
}

fn append_animation_connections(scene: &StaticScene<'_>, records: &mut Vec<FbxNode>) -> Result<()> {
    for animation in &scene.animations {
        push_connection(records, "OO", animation.layer_id, animation.stack_id, None)?;
        for property in &animation.properties {
            push_connection(records, "OO", property.node_id, animation.layer_id, None)?;
            push_connection(
                records,
                "OP",
                property.node_id,
                property.model_id,
                Some(property.kind.fbx_property()),
            )?;
            for (component, id) in property.curve_ids.iter().enumerate() {
                push_connection(
                    records,
                    "OP",
                    *id,
                    property.node_id,
                    Some(animation_component_property(component)),
                )?;
            }
        }
        for blend_shape in &animation.blend_shapes {
            push_connection(records, "OO", blend_shape.node_id, animation.layer_id, None)?;
            push_connection(
                records,
                "OP",
                blend_shape.node_id,
                blend_shape.channel_id,
                Some("DeformPercent"),
            )?;
            push_connection(
                records,
                "OP",
                blend_shape.curve_id,
                blend_shape.node_id,
                Some("d|DeformPercent"),
            )?;
        }
    }
    Ok(())
}

fn append_material_connections(scene: &StaticScene<'_>, records: &mut Vec<FbxNode>) -> Result<()> {
    // Object-to-property links name the material channel the texture drives.
    for material in &scene.materials {
        for texture in &material.textures {
            push_connection(
                records,
                "OP",
                texture.texture_id,
                material.id,
                Some(texture.slot.fbx_property()),
            )?;
        }
    }
    Ok(())
}

fn connections(scene: &StaticScene<'_>) -> Result<FbxNode> {
    let mut records: Vec<FbxNode> = Vec::new();
    append_object_connections(scene, &mut records)?;
    append_animation_connections(scene, &mut records)?;
    append_material_connections(scene, &mut records)?;
    let mut connections = FbxNode::new("Connections");
    connections.children = records;
    Ok(connections)
}

#[cfg(test)]
mod tests {
    use super::{
        read_model_ir_fbx_binary, reserve_exact, scaled_length, write_model_ir_fbx_binary,
        write_model_ir_fbx_binary_with_textures,
    };
    use crate::fbx_ascii::write_model_ir_fbx_ascii;
    use crate::fbx_binary::{FbxProperty, parse_fbx_binary};

    use crate::mesh::{Mesh, MeshBoneWeight, MeshSubMesh};
    use crate::model_ir::{
        ModelIr, ModelLocalTransform, ModelMesh, ModelNode, ModelRendererBinding, ModelRendererKind,
    };
    use crate::scene::{Quaternion, Vector3};
    use crate::scene_hierarchy::SceneObjectKey;

    const fn key(path_id: i64) -> SceneObjectKey {
        SceneObjectKey {
            file_index: 0,
            path_id,
        }
    }

    fn triangle_mesh(skin: Option<Vec<MeshBoneWeight>>) -> Mesh {
        Mesh {
            path_id: 51,
            name: "quad".to_owned(),
            vertices: vec![[0.5, 0.0, 0.0], [1.0, 2.0, 0.0], [0.0, 1.0, 3.0]],
            normals: None,
            uv0: None,
            bind_poses: if skin.is_some() {
                vec![[
                    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
                ]]
            } else {
                Vec::new()
            },
            bone_name_hashes: if skin.is_some() { vec![1] } else { Vec::new() },
            root_bone_name_hash: 0,
            skin,
            blend_shapes: None,
            sub_meshes: vec![MeshSubMesh {
                first_byte: 0,
                index_count: 3,
                first_vertex: 0,
                vertex_count: 3,
                indices: vec![0, 1, 2],
            }],
        }
    }

    fn model_with(mesh: Mesh, kind: ModelRendererKind) -> ModelIr {
        let root = key(1);
        let mut node = ModelNode {
            object: root,
            name: "root".to_owned(),
            export_content: true,
            parent: None,
            children: Vec::new(),
            transform: Some(ModelLocalTransform {
                component: key(11),
                local_rotation: Quaternion {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    w: 1.0,
                },
                local_position: Vector3 {
                    x: 2.0,
                    y: 3.0,
                    z: 4.0,
                },
                local_scale: Vector3 {
                    x: 1.0,
                    y: 1.0,
                    z: 1.0,
                },
            }),
            renderers: Vec::new(),
            animator: None,
        };
        node.renderers.push(ModelRendererBinding {
            component: key(31),
            kind,
            mesh: Some(key(51)),
            materials: Vec::new(),
        });
        ModelIr::from_test_parts(
            vec![node],
            vec![root],
            vec![ModelMesh {
                object: key(51),
                mesh,
            }],
            Vec::new(),
        )
    }

    /// The same model with one material slot, so a texture has something to
    /// bind to.
    fn material_model_fixture() -> ModelIr {
        use crate::material::{Material, MaterialPropertySheet};
        use crate::model_ir::ModelMaterial;
        use crate::serialized::ObjectReference;

        let model = model_fixture();
        let material_key = key(61);
        let material = Material {
            path_id: material_key.path_id,
            name: "skin".to_owned(),
            shader: ObjectReference {
                file_id: 0,
                path_id: 0,
            },
            legacy_shader_keywords: Vec::new(),
            valid_keywords: Vec::new(),
            invalid_keywords: Vec::new(),
            lightmap_flags: None,
            enable_instancing_variants: None,
            custom_render_queue: None,
            string_tags: Vec::new(),
            disabled_shader_passes: Vec::new(),
            saved_properties: MaterialPropertySheet::default(),
            trailing_bytes: 0,
        };
        let mut node = model.nodes[0].clone();
        node.renderers[0].materials = vec![Some(material_key)];
        ModelIr::from_test_parts(
            vec![node],
            vec![key(1)],
            model.meshes.clone(),
            vec![ModelMaterial {
                object: material_key,
                material,
            }],
        )
    }

    fn model_fixture() -> ModelIr {
        model_with(
            triangle_mesh(None),
            ModelRendererKind::MeshRenderer { mesh_filter: None },
        )
    }

    /// A mesh carrying a blend shape, which this layout does not emit.
    fn blend_shape_model_fixture() -> ModelIr {
        use crate::mesh::{
            MeshBlendShapeChannel, MeshBlendShapeFrame, MeshBlendShapeVertex, MeshBlendShapes,
        };

        let mut mesh = triangle_mesh(None);
        mesh.blend_shapes = Some(MeshBlendShapes {
            vertices: vec![MeshBlendShapeVertex {
                vertex: [0.1, 0.0, 0.0],
                normal: [0.0, 0.0, 0.0],
                tangent: [0.0, 0.0, 0.0],
                index: 0,
            }],
            frames: vec![MeshBlendShapeFrame {
                first_vertex: 0,
                vertex_count: 1,
                has_normals: false,
                has_tangents: false,
            }],
            channels: vec![MeshBlendShapeChannel {
                name: "smile".to_owned(),
                name_hash: 1,
                frame_index: 0,
                frame_count: 1,
            }],
            full_weights: vec![100.0],
        });
        model_with(mesh, ModelRendererKind::MeshRenderer { mesh_filter: None })
    }

    /// A skinned model whose bone is a separate node, so the scene builder
    /// resolves a skin rather than refusing the model outright.
    fn skinned_model_fixture() -> ModelIr {
        let root = key(1);
        let bone = key(2);
        let mesh = triangle_mesh(Some(vec![
            MeshBoneWeight {
                weights: [1.0, 0.0, 0.0, 0.0],
                bone_indices: [0, 0, 0, 0],
            };
            3
        ]));
        let transform = |component: i64, position: [f32; 3]| ModelLocalTransform {
            component: key(component),
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
        };
        let mut root_node = ModelNode {
            object: root,
            name: "root".to_owned(),
            export_content: true,
            parent: None,
            children: vec![bone],
            transform: Some(transform(11, [0.0, 0.0, 0.0])),
            renderers: Vec::new(),
            animator: None,
        };
        root_node.renderers.push(ModelRendererBinding {
            component: key(31),
            // The renderer names Transform components, not GameObjects.
            kind: ModelRendererKind::SkinnedMeshRenderer {
                bones: vec![Some(key(12))],
            },
            mesh: Some(key(51)),
            materials: Vec::new(),
        });
        let bone_node = ModelNode {
            object: bone,
            name: "bone".to_owned(),
            export_content: true,
            parent: Some(root),
            children: Vec::new(),
            transform: Some(transform(12, [0.0, 1.0, 0.0])),
            renderers: Vec::new(),
            animator: None,
        };
        ModelIr::from_test_parts(
            vec![root_node, bone_node],
            vec![root],
            vec![ModelMesh {
                object: key(51),
                mesh,
            }],
            Vec::new(),
        )
    }

    /// Finds one record by name among a node's children.
    fn child<'a>(
        node: &'a crate::fbx_binary::FbxNode,
        name: &str,
    ) -> &'a crate::fbx_binary::FbxNode {
        node.children
            .iter()
            .find(|child| child.name == name)
            .unwrap_or_else(|| panic!("no {name} record"))
    }

    #[test]
    fn carries_the_same_geometry_the_ascii_writer_emits() {
        // The ASCII emitter's scene content is what the managed differential
        // checks, so agreeing with it is what makes the binary scene content
        // trustworthy rather than merely self-consistent.
        let model = model_fixture();
        let bytes = read_model_ir_fbx_binary(&model, 64 * 1024).unwrap();
        let roots = parse_fbx_binary(&bytes).unwrap();

        let objects = roots
            .iter()
            .find(|node| node.name == "Objects")
            .expect("an Objects record");
        let geometry = objects
            .children
            .iter()
            .find(|node| node.name == "Geometry")
            .expect("a Geometry record");
        let vertices = match &child(geometry, "Vertices").properties[0] {
            FbxProperty::F64Array(values) => values.clone(),
            other => panic!("vertices are not a double array: {other:?}"),
        };

        let mut ascii = Vec::new();
        write_model_ir_fbx_ascii(&model, &mut ascii, 64 * 1024).unwrap();
        let ascii = String::from_utf8(ascii).unwrap();
        // The ASCII form spells the same values out; parsing the one array back
        // is enough to catch a mirrored axis or a reordered component.
        let start = ascii.find("Vertices: *").expect("an ASCII vertex array");
        let line = ascii[start..]
            .lines()
            .nth(1)
            .expect("the array's values")
            .trim()
            .trim_start_matches("a: ");
        let expected: Vec<f64> = line
            .split(',')
            .map(|value| value.trim().parse().expect("a number"))
            .collect();
        assert_eq!(vertices, expected);
    }

    #[test]
    fn emits_animation_stacks_layers_and_their_curves() {
        use crate::model_animation::{
            ModelAnimationClip, ModelAnimationSet, ModelAnimationTrack, ModelVectorKeyframe,
        };

        let model = model_fixture();
        let animations = ModelAnimationSet {
            clips: vec![ModelAnimationClip {
                object: key(74),
                name: "idle".to_owned(),
                sample_rate: 30.0,
                tracks: vec![ModelAnimationTrack {
                    node: key(1),
                    translations: vec![
                        ModelVectorKeyframe {
                            time: 0.0,
                            value: [0.0, 0.0, 0.0],
                        },
                        ModelVectorKeyframe {
                            time: 1.0,
                            value: [1.0, 2.0, 3.0],
                        },
                    ],
                    rotations: Vec::new(),
                    scalings: Vec::new(),
                }],
                blend_shapes: Vec::new(),
            }],
        };

        let mut output = Vec::new();
        super::write_model_ir_fbx_binary_full(
            &model,
            Some(&animations),
            None,
            &mut output,
            64 * 1024,
        )
        .unwrap();
        let roots = parse_fbx_binary(&output).unwrap();
        let objects = roots
            .iter()
            .find(|node| node.name == "Objects")
            .expect("an Objects record");

        assert!(
            objects
                .children
                .iter()
                .any(|node| node.name == "AnimationStack")
        );
        assert!(
            objects
                .children
                .iter()
                .any(|node| node.name == "AnimationLayer")
        );
        // One curve node for the translation property, three curves under it.
        assert_eq!(
            objects
                .children
                .iter()
                .filter(|node| node.name == "AnimationCurveNode")
                .count(),
            1
        );
        let curves: Vec<&crate::fbx_binary::FbxNode> = objects
            .children
            .iter()
            .filter(|node| node.name == "AnimationCurve")
            .collect();
        assert_eq!(curves.len(), 3);

        // Key times are FBX ticks, not seconds: one second is 46186158000 of
        // them, and emitting seconds would collapse the whole clip onto frame
        // zero while still parsing.
        match &child(curves[0], "KeyTime").properties[0] {
            FbxProperty::I64Array(times) => {
                assert_eq!(times, &[0, 46_186_158_000]);
            }
            other => panic!("key times are not a long array: {other:?}"),
        }
        // The X curve carries the X component of each key.
        match &child(curves[0], "KeyValueFloat").properties[0] {
            FbxProperty::F32Array(values) => assert_eq!(values, &[0.0, 1.0]),
            other => panic!("key values are not a float array: {other:?}"),
        }

        let connections = roots
            .iter()
            .find(|node| node.name == "Connections")
            .expect("a Connections record");
        let mut components: Vec<&str> = connections
            .children
            .iter()
            .filter_map(|node| match node.properties.last() {
                Some(FbxProperty::String(value))
                    if matches!(value.as_str(), "d|X" | "d|Y" | "d|Z") =>
                {
                    Some(value.as_str())
                }
                _ => None,
            })
            .collect();
        components.sort_unstable();
        assert_eq!(components, ["d|X", "d|Y", "d|Z"]);
    }

    #[test]
    fn writes_a_scene_whose_records_a_reader_can_walk() {
        let model = model_fixture();
        let mut output = Vec::new();
        let written = write_model_ir_fbx_binary(&model, &mut output, 64 * 1024).unwrap();
        assert_eq!(written, u64::try_from(output.len()).unwrap());

        let roots = parse_fbx_binary(&output).unwrap();
        let names: Vec<&str> = roots.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "FBXHeaderExtension",
                "GlobalSettings",
                "Documents",
                "References",
                "Definitions",
                "Objects",
                "Connections",
                "Takes"
            ]
        );

        let objects = &roots[5];
        assert!(objects.children.iter().any(|node| node.name == "Model"));
        assert!(objects.children.iter().any(|node| node.name == "Geometry"));
        // Every object is linked to something; an unlinked object would not
        // appear in an importer's scene at all.
        assert!(!roots[6].children.is_empty());
    }

    #[test]
    fn rejects_cumulative_scene_strings_before_encoding() {
        let model = model_fixture();
        let mut output = Vec::new();
        let error = write_model_ir_fbx_binary(&model, &mut output, 20).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("scene strings need 25 bytes, exceeding the 20 byte output limit"),
            "{error}"
        );
        assert!(
            output.is_empty(),
            "the writer published bytes before the scene budget failed"
        );
    }

    #[test]
    fn carries_textures_and_the_material_channels_they_drive() {
        use crate::scene_textures::{
            SceneTexture, SceneTextureBinding, SceneTextureSet, TextureSlot,
        };

        let model = model_fixture();
        let mut set = SceneTextureSet::default();
        let texture = set
            .push_texture(SceneTexture {
                file_name: "Body.png".to_owned(),
                object: key(81),
                encoded: Vec::new(),
            })
            .unwrap();
        // The fixture's renderer has no material slots, so nothing binds and no
        // Texture record should appear; a writer that emitted one anyway would
        // be inventing a reference the scene does not have.
        let _ = SceneTextureBinding {
            property: "_MainTex".to_owned(),
            texture,
            slot: Some(TextureSlot::Diffuse),
            offset: [0.0, 0.0],
            scale: [1.0, 1.0],
        };
        let mut output = Vec::new();
        write_model_ir_fbx_binary_with_textures(&model, Some(&set), &mut output, 64 * 1024)
            .unwrap();
        let roots = parse_fbx_binary(&output).unwrap();
        let objects = roots
            .iter()
            .find(|node| node.name == "Objects")
            .expect("an Objects record");
        assert!(!objects.children.iter().any(|node| node.name == "Texture"));
        assert!(!objects.children.iter().any(|node| node.name == "Video"));
    }

    #[test]
    fn emits_a_texture_video_pair_and_the_channel_it_drives() {
        use crate::scene_textures::{
            SceneTexture, SceneTextureBinding, SceneTextureSet, TextureSlot,
        };

        let model = material_model_fixture();
        let mut set = SceneTextureSet::default();
        let texture = set
            .push_texture(SceneTexture {
                file_name: "Body.png".to_owned(),
                object: key(81),
                encoded: Vec::new(),
            })
            .unwrap();
        set.bind(
            key(61),
            SceneTextureBinding {
                property: "_MainTex".to_owned(),
                texture,
                slot: Some(TextureSlot::Diffuse),
                offset: [0.25, 0.5],
                scale: [2.0, 4.0],
            },
        )
        .unwrap();

        let mut output = Vec::new();
        write_model_ir_fbx_binary_with_textures(&model, Some(&set), &mut output, 64 * 1024)
            .unwrap();
        let roots = parse_fbx_binary(&output).unwrap();
        let objects = roots
            .iter()
            .find(|node| node.name == "Objects")
            .expect("an Objects record");

        let texture_record = objects
            .children
            .iter()
            .find(|node| node.name == "Texture")
            .expect("a Texture record");
        assert_eq!(
            texture_record.properties[1],
            FbxProperty::String("Texture::Body.png".to_owned())
        );
        assert_eq!(
            child(texture_record, "RelativeFilename").properties[0],
            FbxProperty::String("Body.png".to_owned())
        );
        assert!(objects.children.iter().any(|node| node.name == "Video"));

        // The texture drives the material's diffuse channel through an
        // object-to-property link, which is what makes it show up at all.
        let connections = roots
            .iter()
            .find(|node| node.name == "Connections")
            .expect("a Connections record");
        assert!(connections.children.iter().any(|node| {
            node.properties.first() == Some(&FbxProperty::String("OP".to_owned()))
                && node.properties.last() == Some(&FbxProperty::String("DiffuseColor".to_owned()))
        }));
    }

    #[test]
    fn emits_a_skin_deformer_and_its_clusters() {
        let model = skinned_model_fixture();
        let bytes = read_model_ir_fbx_binary(&model, 64 * 1024).unwrap();
        let roots = parse_fbx_binary(&bytes).unwrap();
        let objects = roots
            .iter()
            .find(|node| node.name == "Objects")
            .expect("an Objects record");

        let deformers: Vec<&crate::fbx_binary::FbxNode> = objects
            .children
            .iter()
            .filter(|node| node.name == "Deformer")
            .collect();
        // One skin plus one cluster for the single bone.
        assert_eq!(deformers.len(), 2);
        assert_eq!(
            deformers[0].properties[2],
            FbxProperty::String("Skin".to_owned())
        );
        let cluster = deformers[1];
        assert_eq!(
            cluster.properties[2],
            FbxProperty::String("Cluster".to_owned())
        );
        // Every vertex is fully weighted to the one bone, so all three appear.
        assert_eq!(
            child(cluster, "Indexes").properties[0],
            FbxProperty::I32Array(vec![0, 1, 2])
        );
        assert_eq!(
            child(cluster, "Weights").properties[0],
            FbxProperty::F64Array(vec![1.0, 1.0, 1.0])
        );
        // Both bind matrices are sixteen values, which is what a reader indexes
        // into; a short one would be read as garbage rather than rejected.
        for name in ["Transform", "TransformLink"] {
            match &child(cluster, name).properties[0] {
                FbxProperty::F64Array(values) => assert_eq!(values.len(), 16),
                other => panic!("{name} is not a double array: {other:?}"),
            }
        }
    }

    #[test]
    fn emits_blend_shape_targets_as_offsets_from_their_base_vertices() {
        let model = blend_shape_model_fixture();
        let bytes = read_model_ir_fbx_binary(&model, 64 * 1024).unwrap();
        let roots = parse_fbx_binary(&bytes).unwrap();
        let objects = roots
            .iter()
            .find(|node| node.name == "Objects")
            .expect("an Objects record");

        // The target's own geometry: which control points move, and by how
        // much. FBX stores offsets, not absolute positions, so a writer that
        // emitted positions would move every vertex to near the origin.
        let shape = objects
            .children
            .iter()
            .find(|node| {
                node.name == "Geometry"
                    && node.properties.get(2) == Some(&FbxProperty::String("Shape".to_owned()))
            })
            .expect("a Shape geometry");
        assert_eq!(
            child(shape, "Indexes").properties[0],
            FbxProperty::I32Array(vec![0])
        );
        // The fixture's delta is 0.1 along X, which the mirror negates.
        match &child(shape, "Vertices").properties[0] {
            FbxProperty::F64Array(values) => {
                assert_eq!(values.len(), 3);
                assert!(
                    (values[0] + 0.1).abs() < 1e-6,
                    "unexpected delta: {values:?}"
                );
            }
            other => panic!("shape vertices are not a double array: {other:?}"),
        }

        // The channel names the target and carries the weight it applies at.
        let channel = objects
            .children
            .iter()
            .find(|node| {
                node.properties.get(2) == Some(&FbxProperty::String("BlendShapeChannel".to_owned()))
            })
            .expect("a BlendShapeChannel");
        assert_eq!(
            child(channel, "FullWeights").properties[0],
            FbxProperty::F64Array(vec![100.0])
        );
        assert!(objects.children.iter().any(|node| {
            node.properties.get(2) == Some(&FbxProperty::String("BlendShape".to_owned()))
        }));
    }

    #[test]
    fn rejects_scene_projection_allocation_overflow_before_growth() {
        assert!(scaled_length(usize::MAX, 2, "test values").is_err());

        let mut values = Vec::<u8>::new();
        assert!(reserve_exact(&mut values, usize::MAX, "test values").is_err());
        assert!(values.is_empty());
    }
}
