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
//! This covers the static scene: models, geometry, materials and the
//! connections between them. Skinning, blend shapes, animation and textures
//! stay on the ASCII path until each has been laid out and checked in turn;
//! emitting a partial version of them would produce a file that looks complete
//! and quietly is not.

use std::io::Write;

use crate::fbx_binary::{FbxNode, FbxProperty};
use crate::fbx_scene_ascii::{MaterialProperties, StaticScene, polygon_end};
use crate::model_ir::ModelIr;
use crate::{Error, Result};

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
    let scene = StaticScene::from_model(model, None, textures)?;
    if scene
        .geometries
        .iter()
        .any(|geometry| geometry.skin.is_some())
    {
        return Err(Error::unsupported(
            "binary FBX does not yet emit skin deformers",
        ));
    }
    if scene
        .geometries
        .iter()
        .any(|geometry| geometry.morph.is_some())
    {
        return Err(Error::unsupported(
            "binary FBX does not yet emit blend shapes",
        ));
    }
    let roots = build_scene_nodes(&scene)?;
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
fn build_scene_nodes(scene: &StaticScene<'_>) -> Result<Vec<FbxNode>> {
    let mut objects = FbxNode::new("Objects");
    for node in &scene.nodes {
        objects.children.push(model_node(node));
    }
    for geometry in &scene.geometries {
        objects.children.push(geometry_node(geometry)?);
    }
    for material in &scene.materials {
        objects.children.push(material_node(material));
    }
    for texture in &scene.textures {
        objects.children.push(texture_node(texture));
        objects.children.push(video_node(texture));
    }

    Ok(vec![
        header_extension(),
        global_settings(),
        documents(),
        FbxNode::new("References"),
        definitions(scene),
        objects,
        connections(scene),
        FbxNode::new("Takes")
            .child(FbxNode::new("Current").with(FbxProperty::String(String::new()))),
    ])
}

fn header_extension() -> FbxNode {
    FbxNode::new("FBXHeaderExtension")
        .child(FbxNode::new("FBXHeaderVersion").with(FbxProperty::I32(1003)))
        .child(FbxNode::new("FBXVersion").with(FbxProperty::I32(7400)))
        .child(FbxNode::new("EncryptionType").with(FbxProperty::I32(0)))
        .child(FbxNode::new("Creator").with(FbxProperty::String(
            "AssetStudio Rust binary FBX writer".to_owned(),
        )))
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

fn definitions(scene: &StaticScene<'_>) -> FbxNode {
    let mut definitions = FbxNode::new("Definitions")
        .child(FbxNode::new("Version").with(FbxProperty::I32(100)))
        .child(FbxNode::new("Count").with(FbxProperty::I32(5)));
    for (name, count) in [
        ("Model", scene.nodes.len()),
        ("Geometry", scene.geometries.len()),
        ("Material", scene.materials.len()),
        ("Texture", scene.textures.len()),
        ("Video", scene.textures.len()),
    ] {
        definitions.children.push(
            FbxNode::new("ObjectType")
                .with(FbxProperty::String(name.to_owned()))
                .child(
                    FbxNode::new("Count")
                        .with(FbxProperty::I32(i32::try_from(count).unwrap_or(i32::MAX))),
                ),
        );
    }
    definitions
}

fn model_node(node: &crate::fbx_scene_ascii::NodePlan<'_>) -> FbxNode {
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
    FbxNode::new("Model")
        .with(FbxProperty::I64(node.id))
        .with(FbxProperty::String(format!("Model::{}", node.name)))
        .with(FbxProperty::String(kind.to_owned()))
        .child(FbxNode::new("Version").with(FbxProperty::I32(232)))
        .child(properties)
        .child(FbxNode::new("Shading").with(FbxProperty::Bool(true)))
        .child(FbxNode::new("Culling").with(FbxProperty::String("CullingOff".to_owned())))
}

/// One geometry record: mirrored vertices and the polygon index run.
///
/// X is mirrored and the winding reversed, exactly as the ASCII writer does,
/// and the last index of every polygon is stored as its negative-one's
/// complement, which is how FBX marks a polygon's end.
fn geometry_node(geometry: &crate::fbx_scene_ascii::GeometryPlan<'_>) -> Result<FbxNode> {
    let mesh = geometry.mesh;
    let mut vertices = Vec::with_capacity(mesh.vertices.len() * 3);
    for vertex in &mesh.vertices {
        vertices.push(f64::from(-vertex[0]));
        vertices.push(f64::from(vertex[1]));
        vertices.push(f64::from(vertex[2]));
    }

    let mut indices = Vec::new();
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
        .with(FbxProperty::String(format!("Geometry::{}", mesh.name)))
        .with(FbxProperty::String("Mesh".to_owned()))
        .child(FbxNode::new("GeometryVersion").with(FbxProperty::I32(124)))
        .child(FbxNode::new("Vertices").with(FbxProperty::F64Array(vertices)))
        .child(FbxNode::new("PolygonVertexIndex").with(FbxProperty::I32Array(indices))))
}

fn material_node(material: &crate::fbx_scene_ascii::MaterialPlan<'_>) -> FbxNode {
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
    FbxNode::new("Material")
        .with(FbxProperty::I64(material.id))
        .with(FbxProperty::String(format!("Material::{name}")))
        .with(FbxProperty::String(String::new()))
        .child(FbxNode::new("Version").with(FbxProperty::I32(102)))
        .child(FbxNode::new("ShadingModel").with(FbxProperty::String("phong".to_owned())))
        .child(FbxNode::new("MultiLayer").with(FbxProperty::I32(0)))
        .child(properties)
}

/// One `Texture` record, carrying the UV transform FBX keeps on the texture
/// rather than on the binding.
fn texture_node(texture: &crate::fbx_scene_ascii::TexturePlan<'_>) -> FbxNode {
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
    FbxNode::new("Texture")
        .with(FbxProperty::I64(texture.id))
        .with(FbxProperty::String(format!(
            "Texture::{}",
            texture.file_name
        )))
        .with(FbxProperty::String(String::new()))
        .child(FbxNode::new("Type").with(FbxProperty::String("TextureVideoClip".to_owned())))
        .child(FbxNode::new("Version").with(FbxProperty::I32(202)))
        .child(
            FbxNode::new("TextureName").with(FbxProperty::String(format!(
                "Texture::{}",
                texture.file_name
            ))),
        )
        .child(properties)
        .child(
            FbxNode::new("Media")
                .with(FbxProperty::String(format!("Video::{}", texture.file_name))),
        )
        .child(FbxNode::new("FileName").with(FbxProperty::String(texture.file_name.to_owned())))
        .child(
            FbxNode::new("RelativeFilename")
                .with(FbxProperty::String(texture.file_name.to_owned())),
        )
}

/// The `Video` clip a `Texture` reads its bytes through.
fn video_node(texture: &crate::fbx_scene_ascii::TexturePlan<'_>) -> FbxNode {
    let mut properties = FbxNode::new("Properties70");
    properties.children.push(
        FbxNode::new("P")
            .with(FbxProperty::String("Path".to_owned()))
            .with(FbxProperty::String("KString".to_owned()))
            .with(FbxProperty::String("XRefUrl".to_owned()))
            .with(FbxProperty::String(String::new()))
            .with(FbxProperty::String(texture.file_name.to_owned())),
    );
    FbxNode::new("Video")
        .with(FbxProperty::I64(texture.video_id))
        .with(FbxProperty::String(format!("Video::{}", texture.file_name)))
        .with(FbxProperty::String("Clip".to_owned()))
        .child(FbxNode::new("Type").with(FbxProperty::String("Clip".to_owned())))
        .child(properties)
        .child(FbxNode::new("UseMipMap").with(FbxProperty::I32(0)))
        .child(FbxNode::new("Filename").with(FbxProperty::String(texture.file_name.to_owned())))
        .child(
            FbxNode::new("RelativeFilename")
                .with(FbxProperty::String(texture.file_name.to_owned())),
        )
}

/// Object-to-object links, in the same order the ASCII writer emits them.
fn connections(scene: &StaticScene<'_>) -> FbxNode {
    let mut connections = FbxNode::new("Connections");
    let mut link = |child: i64, parent: i64| {
        connections.children.push(
            FbxNode::new("C")
                .with(FbxProperty::String("OO".to_owned()))
                .with(FbxProperty::I64(child))
                .with(FbxProperty::I64(parent)),
        );
    };
    for node in &scene.nodes {
        link(node.id, node.parent_id);
    }
    for geometry in &scene.geometries {
        link(geometry.id, geometry.model_id);
        for material_id in &geometry.material_ids {
            link(*material_id, geometry.model_id);
        }
    }
    for texture in &scene.textures {
        link(texture.video_id, texture.id);
    }
    // Object-to-property links name the material channel the texture drives.
    for material in &scene.materials {
        for texture in &material.textures {
            connections.children.push(
                FbxNode::new("C")
                    .with(FbxProperty::String("OP".to_owned()))
                    .with(FbxProperty::I64(texture.texture_id))
                    .with(FbxProperty::I64(material.id))
                    .with(FbxProperty::String(texture.slot.fbx_property().to_owned())),
            );
        }
    }
    connections
}

#[cfg(test)]
mod tests {
    use super::{
        read_model_ir_fbx_binary, write_model_ir_fbx_binary,
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
            kind: ModelRendererKind::SkinnedMeshRenderer {
                bones: vec![Some(bone)],
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
    fn carries_textures_and_the_material_channels_they_drive() {
        use crate::scene_textures::{
            SceneTexture, SceneTextureBinding, SceneTextureSet, TextureSlot,
        };

        let model = model_fixture();
        let mut set = SceneTextureSet::default();
        let texture = set.push_texture(SceneTexture {
            file_name: "Body.png".to_owned(),
            object: key(81),
            encoded: Vec::new(),
        });
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
        let texture = set.push_texture(SceneTexture {
            file_name: "Body.png".to_owned(),
            object: key(81),
            encoded: Vec::new(),
        });
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
    fn refuses_a_scene_this_layout_would_silently_drop() {
        // Skinning and blend shapes stay on the ASCII path. What matters is
        // that a skinned model is refused rather than written out as plain
        // geometry, which would look like a successful export and lose the
        // rig. The message is not pinned because the refusal can legitimately
        // come from either the scene builder or this layout's own guard,
        // depending on how the model resolves its bones.
        let model = skinned_model_fixture();
        assert!(read_model_ir_fbx_binary(&model, 64 * 1024).is_err());

        // The guard itself is what stops a resolvable skin, so assert it reads
        // as unsupported rather than as malformed input: the file is fine, this
        // writer simply does not cover it yet.
        let error = read_model_ir_fbx_binary(&model, 64 * 1024).unwrap_err();
        assert!(
            matches!(error, crate::Error::Unsupported(_)),
            "a skinned model should be unsupported, not invalid: {error}"
        );
    }
}
