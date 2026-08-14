//! Wavefront OBJ export for a whole model, with a companion MTL.
//!
//! [`write_mesh_obj`](crate::mesh::write_mesh_obj) exports one `Mesh` exactly as
//! the managed reader does: object-local coordinates, no materials, no MTL. That
//! is the right shape for exporting a single mesh asset, but it cannot carry a
//! model: OBJ has no hierarchy, so a scene has to arrive with its transforms
//! already applied and its materials named.
//!
//! This module writes that form. Node transforms are baked into world space,
//! each submesh names its material, and the MTL carries the material colours
//! plus `map_Kd`/`map_Bump`/`map_Ks` pointing at the files a
//! [`SceneTextureSet`] writes beside the model.
//!
//! Two deliberate differences from the single-mesh writer, both because this
//! output has no managed counterpart to match:
//!
//! * Face references name only the channels the mesh actually has. The managed
//!   writer always emits `v/vt/vn` even for a mesh with no UVs or normals,
//!   which is malformed and rejected by strict importers.
//! * Vertex indices accumulate across the whole file, since OBJ numbers them
//!   per file rather than per group.

use std::io::{self, Write};

use crate::fbx_scene_ascii::{Matrix4, build_global_matrices};
use crate::material::Material;
use crate::mesh::{BoundedWriter, Mesh, ObjFloat};
use crate::model_ir::ModelIr;
use crate::scene_hierarchy::SceneObjectKey;
use crate::scene_textures::SceneTextureSet;
use crate::{Error, Result};

/// Writes the model as OBJ, optionally referencing a companion MTL.
///
/// `mtl_file_name` becomes the `mtllib` line and must be a single path
/// component: the MTL is expected beside the OBJ. Pass `None` to write geometry
/// with no material references at all.
pub fn write_model_ir_obj<W: Write>(
    model: &ModelIr,
    mtl_file_name: Option<&str>,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    if let Some(name) = mtl_file_name {
        validate_component(name, "MTL file name")?;
    }
    let groups = build_groups(model)?;
    if groups.is_empty() {
        return Err(Error::unsupported(
            "OBJ export found no renderable mesh in the model",
        ));
    }
    let mut bounded = BoundedWriter::new(output, maximum_output_bytes);
    let result = write_obj_inner(&groups, mtl_file_name, &mut bounded);
    if bounded.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "model OBJ exceeds the {maximum_output_bytes} byte output limit"
        )));
    }
    result?;
    Ok(bounded.written)
}

/// Writes the companion MTL for the same model.
///
/// Materials are named exactly as [`write_model_ir_obj`] names them, so the two
/// files must be produced from the same model and texture set.
pub fn write_model_ir_mtl<W: Write>(
    model: &ModelIr,
    textures: &SceneTextureSet,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    let groups = build_groups(model)?;
    let mut bounded = BoundedWriter::new(output, maximum_output_bytes);
    let result = write_mtl_inner(model, &groups, textures, &mut bounded);
    if bounded.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "model MTL exceeds the {maximum_output_bytes} byte output limit"
        )));
    }
    result?;
    Ok(bounded.written)
}

/// One renderable mesh, already placed in world space.
struct ObjGroup<'a> {
    name: &'a str,
    mesh: &'a Mesh,
    transform: Option<Matrix4>,
    /// One material key per submesh slot, `None` where the slot is unresolved.
    materials: Vec<Option<SceneObjectKey>>,
}

fn build_groups(model: &ModelIr) -> Result<Vec<ObjGroup<'_>>> {
    let matrices = build_global_matrices(model)?;
    let mut groups = Vec::new();
    for (node_index, node) in model.nodes.iter().enumerate() {
        if !node.export_content {
            continue;
        }
        for renderer in &node.renderers {
            let Some(mesh_key) = renderer.mesh else {
                continue;
            };
            let mesh = model
                .mesh(mesh_key)
                .ok_or_else(|| Error::invalid_data("OBJ renderer references a missing Mesh"))?;
            groups.push(ObjGroup {
                name: &node.name,
                mesh: &mesh.mesh,
                transform: matrices.get(node_index).copied().flatten(),
                materials: renderer.materials.clone(),
            });
        }
    }
    Ok(groups)
}

fn write_obj_inner<W: Write>(
    groups: &[ObjGroup<'_>],
    mtl_file_name: Option<&str>,
    output: &mut W,
) -> io::Result<()> {
    if let Some(name) = mtl_file_name {
        write!(output, "mtllib {name}\r\n")?;
    }
    // OBJ numbers vertices per file, so each group starts where the last ended.
    let mut vertex_base = 0_u64;
    let mut uv_base = 0_u64;
    let mut normal_base = 0_u64;
    for group in groups {
        let mesh = group.mesh;
        write!(output, "g {}\r\n", ObjName(group.name))?;
        for vertex in &mesh.vertices {
            let placed = place_point(group.transform, *vertex);
            write!(
                output,
                "v {} {} {}\r\n",
                ObjFloat(placed[0]),
                ObjFloat(placed[1]),
                ObjFloat(placed[2])
            )?;
        }
        if let Some(uv0) = &mesh.uv0 {
            for uv in uv0 {
                write!(output, "vt {} {}\r\n", ObjFloat(uv[0]), ObjFloat(uv[1]))?;
            }
        }
        if let Some(normals) = &mesh.normals {
            for normal in normals {
                let placed = place_direction(group.transform, *normal);
                write!(
                    output,
                    "vn {} {} {}\r\n",
                    ObjFloat(placed[0]),
                    ObjFloat(placed[1]),
                    ObjFloat(placed[2])
                )?;
            }
        }
        let has_uv = mesh.uv0.is_some();
        let has_normals = mesh.normals.is_some();
        for (slot, sub_mesh) in mesh.sub_meshes.iter().enumerate() {
            write!(output, "usemtl {}\r\n", MaterialName(group, slot))?;
            for triangle in sub_mesh.indices.chunks_exact(3) {
                output.write_all(b"f")?;
                // Unity winds clockwise and the X mirror above flips handedness,
                // so the triangle is emitted back to front to stay outward.
                for index in [triangle[2], triangle[1], triangle[0]] {
                    let vertex = vertex_base + u64::from(index) + 1;
                    let uv = uv_base + u64::from(index) + 1;
                    let normal = normal_base + u64::from(index) + 1;
                    match (has_uv, has_normals) {
                        (true, true) => write!(output, " {vertex}/{uv}/{normal}")?,
                        (true, false) => write!(output, " {vertex}/{uv}")?,
                        (false, true) => write!(output, " {vertex}//{normal}")?,
                        (false, false) => write!(output, " {vertex}")?,
                    }
                }
                output.write_all(b"\r\n")?;
            }
        }
        vertex_base += mesh.vertices.len() as u64;
        if let Some(uv0) = &mesh.uv0 {
            uv_base += uv0.len() as u64;
        }
        if let Some(normals) = &mesh.normals {
            normal_base += normals.len() as u64;
        }
    }
    Ok(())
}

fn write_mtl_inner<W: Write>(
    model: &ModelIr,
    groups: &[ObjGroup<'_>],
    textures: &SceneTextureSet,
    output: &mut W,
) -> io::Result<()> {
    let mut written: Vec<String> = Vec::new();
    for group in groups {
        for slot in 0..group.mesh.sub_meshes.len() {
            let name = MaterialName(group, slot).to_string();
            if written.contains(&name) {
                continue;
            }
            let key = group.materials.get(slot).copied().flatten();
            let material = key
                .and_then(|key| model.material(key))
                .map(|entry| &entry.material);
            write!(output, "newmtl {name}\r\n")?;
            write_material_body(material, output)?;
            if let Some(key) = key {
                write_material_maps(textures, key, output)?;
            }
            written.push(name);
        }
    }
    Ok(())
}

fn write_material_body<W: Write>(material: Option<&Material>, output: &mut W) -> io::Result<()> {
    let colors = MtlColors::from_material(material);
    write!(
        output,
        "Ka {} {} {}\r\nKd {} {} {}\r\nKs {} {} {}\r\nNs {}\r\nd {}\r\nillum 2\r\n",
        ObjFloat(colors.ambient[0]),
        ObjFloat(colors.ambient[1]),
        ObjFloat(colors.ambient[2]),
        ObjFloat(colors.diffuse[0]),
        ObjFloat(colors.diffuse[1]),
        ObjFloat(colors.diffuse[2]),
        ObjFloat(colors.specular[0]),
        ObjFloat(colors.specular[1]),
        ObjFloat(colors.specular[2]),
        ObjFloat(colors.shininess),
        ObjFloat(colors.opacity),
    )
}

fn write_material_maps<W: Write>(
    textures: &SceneTextureSet,
    material: SceneObjectKey,
    output: &mut W,
) -> io::Result<()> {
    // MTL has one slot per directive, so the first binding that claims a
    // directive keeps it; a shader binding two normal-ish properties would
    // otherwise emit two `map_Bump` lines and leave the winner to the importer.
    let mut used: Vec<&'static str> = Vec::new();
    for binding in textures.bindings_for(material) {
        let Some(slot) = binding.slot else {
            continue;
        };
        let directive = slot.mtl_directive();
        if used.contains(&directive) {
            continue;
        }
        let Some(texture) = textures.textures.get(binding.texture) else {
            continue;
        };
        write!(output, "{directive} {}\r\n", texture.file_name)?;
        used.push(directive);
    }
    Ok(())
}

/// The MTL colours, defaulting to the same neutral material the FBX writer uses
/// when a slot has no resolved `Material`.
struct MtlColors {
    ambient: [f32; 3],
    diffuse: [f32; 3],
    specular: [f32; 3],
    shininess: f32,
    opacity: f32,
}

impl MtlColors {
    fn from_material(material: Option<&Material>) -> Self {
        let mut colors = Self {
            ambient: [0.2, 0.2, 0.2],
            diffuse: [0.8, 0.8, 0.8],
            specular: [0.2, 0.2, 0.2],
            shininess: 20.0,
            opacity: 1.0,
        };
        let Some(material) = material else {
            return colors;
        };
        for property in &material.saved_properties.colors {
            let value = property.value;
            match property.name.as_str() {
                "_Color" => {
                    colors.diffuse = [value[0], value[1], value[2]];
                    colors.opacity = value[3];
                }
                "_SpecColor" => colors.specular = [value[0], value[1], value[2]],
                _ => {}
            }
        }
        for property in &material.saved_properties.floats {
            match property.name.as_str() {
                "_Shininess" => colors.shininess = property.value,
                "_Transparency" => colors.opacity = 1.0 - property.value,
                _ => {}
            }
        }
        colors.sanitize();
        colors
    }

    /// Replaces values OBJ consumers cannot use.
    ///
    /// Material floats come from the asset, so a `NaN` shininess or a negative
    /// opacity is reachable. Writing those produces an MTL importers reject
    /// outright, which would cost the whole model over one bad property.
    fn sanitize(&mut self) {
        for channel in self
            .ambient
            .iter_mut()
            .chain(&mut self.diffuse)
            .chain(&mut self.specular)
        {
            if !channel.is_finite() {
                *channel = 0.0;
            }
            *channel = channel.clamp(0.0, 1.0);
        }
        if !self.shininess.is_finite() {
            self.shininess = 20.0;
        }
        self.shininess = self.shininess.clamp(0.0, 1000.0);
        if !self.opacity.is_finite() {
            self.opacity = 1.0;
        }
        self.opacity = self.opacity.clamp(0.0, 1.0);
    }
}

/// Places a mesh-local position into mirrored world space.
///
/// `build_global_matrices` composes matrices that already carry the X mirror,
/// so they map *mirrored* local coordinates to mirrored world coordinates
/// (`M_mirrored = mirror . M_unity . mirror`). The mirror therefore has to be
/// applied to the vertex before the matrix, not to the result: doing it
/// afterwards flips X twice and cancels out, which put every node on the wrong
/// side of the origin.
fn place_point(transform: Option<Matrix4>, point: [f32; 3]) -> [f32; 3] {
    let mirrored = [-point[0], point[1], point[2]];
    transform.map_or(mirrored, |matrix| matrix.transform_point(mirrored))
}

/// The same composition for a direction, which ignores translation.
fn place_direction(transform: Option<Matrix4>, direction: [f32; 3]) -> [f32; 3] {
    let mirrored = [-direction[0], direction[1], direction[2]];
    transform.map_or(mirrored, |matrix| matrix.transform_direction(mirrored))
}

/// Rejects a name that is not usable as one file-name component.
fn validate_component(name: &str, field: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::invalid_data(format!("{field} is empty")));
    }
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(Error::invalid_data(format!(
            "{field} {name:?} is not a single path component"
        )));
    }
    Ok(())
}

/// An OBJ token: whitespace and newlines end a token, so they become `_`.
struct ObjName<'a>(&'a str);

impl std::fmt::Display for ObjName<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            return formatter.write_str("unnamed");
        }
        for character in self.0.chars() {
            if character.is_whitespace() || character.is_control() {
                formatter.write_str("_")?;
            } else {
                write!(formatter, "{character}")?;
            }
        }
        Ok(())
    }
}

/// The material name for one submesh slot.
///
/// Derived from the material key rather than its name so two materials that
/// share a Unity name stay distinct in the MTL, and so the OBJ and the MTL
/// always agree without threading a name table between them.
struct MaterialName<'a>(&'a ObjGroup<'a>, usize);

impl std::fmt::Display for MaterialName<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.materials.get(self.1).copied().flatten() {
            Some(key) => write!(formatter, "material_{}_{}", key.file_index, key.path_id),
            None => formatter.write_str("DefaultMaterial"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ObjName, write_model_ir_mtl, write_model_ir_obj};
    use crate::material::{
        Material, MaterialPropertySheet, MaterialTextureEnvironment, NamedMaterialProperty,
    };
    use crate::mesh::{Mesh, MeshSubMesh};
    use crate::model_ir::{
        ModelIr, ModelLocalTransform, ModelMaterial, ModelMesh, ModelNode, ModelRendererBinding,
        ModelRendererKind,
    };
    use crate::scene::{Quaternion, Vector3};
    use crate::scene_hierarchy::SceneObjectKey;
    use crate::scene_textures::{SceneTexture, SceneTextureBinding, SceneTextureSet, TextureSlot};
    use crate::serialized::ObjectReference;

    const fn key(path_id: i64) -> SceneObjectKey {
        SceneObjectKey {
            file_index: 0,
            path_id,
        }
    }

    fn triangle(name: &str) -> Mesh {
        Mesh {
            path_id: 51,
            name: name.to_owned(),
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Some(vec![[0.0, 0.0, 1.0]; 3]),
            uv0: Some(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            bind_poses: Vec::new(),
            bone_name_hashes: Vec::new(),
            root_bone_name_hash: 0,
            skin: None,
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

    fn material(name: &str) -> Material {
        Material {
            path_id: 61,
            name: name.to_owned(),
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
            saved_properties: MaterialPropertySheet {
                texture_environments: vec![NamedMaterialProperty {
                    name: "_MainTex".to_owned(),
                    value: MaterialTextureEnvironment {
                        texture: ObjectReference {
                            file_id: 0,
                            path_id: 81,
                        },
                        scale: [1.0, 1.0],
                        offset: [0.0, 0.0],
                    },
                }],
                colors: vec![NamedMaterialProperty {
                    name: "_Color".to_owned(),
                    value: [0.5, 0.25, 0.125, 0.75],
                }],
                ..MaterialPropertySheet::default()
            },
            trailing_bytes: 0,
        }
    }

    fn model(offset: [f32; 3]) -> ModelIr {
        let node_key = key(1);
        let mut node = ModelNode {
            object: node_key,
            name: "body node".to_owned(),
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
                    x: offset[0],
                    y: offset[1],
                    z: offset[2],
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
            kind: ModelRendererKind::MeshRenderer { mesh_filter: None },
            mesh: Some(key(51)),
            materials: vec![Some(key(61))],
        });
        ModelIr::from_test_parts(
            vec![node],
            vec![node_key],
            vec![ModelMesh {
                object: key(51),
                mesh: triangle("body"),
            }],
            vec![ModelMaterial {
                object: key(61),
                material: material("skin"),
            }],
        )
    }

    fn texture_set() -> SceneTextureSet {
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
                offset: [0.0, 0.0],
                scale: [1.0, 1.0],
            },
        )
        .unwrap();
        set
    }

    #[test]
    fn writes_world_space_geometry_that_names_its_material() {
        let model = model([2.0, 0.0, 0.0]);
        let mut output = Vec::new();
        let written = write_model_ir_obj(&model, Some("body.mtl"), &mut output, 64 * 1024).unwrap();
        assert_eq!(written, u64::try_from(output.len()).unwrap());
        let text = std::str::from_utf8(&output).unwrap();

        assert!(text.starts_with("mtllib body.mtl\r\n"));
        // Whitespace ends an OBJ token, so the node name is joined.
        assert!(text.contains("g body_node\r\n"));
        // The node sits at X=2 and OBJ mirrors X, so the first vertex is -2.
        assert!(text.contains("v -2 0 0\r\n"), "{text}");
        assert!(text.contains("v -3 0 0\r\n"), "{text}");
        assert!(text.contains("usemtl material_0_61\r\n"));
        // Full references, and the winding is reversed.
        assert!(text.contains("f 3/3/3 2/2/2 1/1/1\r\n"));
    }

    #[test]
    fn omits_the_channels_the_mesh_does_not_have() {
        // The managed single-mesh writer emits `v/vt/vn` unconditionally, which
        // is malformed without those channels. A model OBJ has no managed
        // counterpart to match, so it names only what exists.
        let mut model = model([0.0, 0.0, 0.0]);
        model.meshes[0].mesh.uv0 = None;
        model.meshes[0].mesh.normals = None;
        let mut output = Vec::new();
        write_model_ir_obj(&model, None, &mut output, 64 * 1024).unwrap();
        let text = std::str::from_utf8(&output).unwrap();
        assert!(!text.contains("mtllib"));
        assert!(!text.contains("vt "));
        assert!(!text.contains("vn "));
        assert!(text.contains("f 3 2 1\r\n"), "{text}");

        model.meshes[0].mesh.normals = Some(vec![[0.0, 0.0, 1.0]; 3]);
        let mut output = Vec::new();
        write_model_ir_obj(&model, None, &mut output, 64 * 1024).unwrap();
        let text = std::str::from_utf8(&output).unwrap();
        assert!(text.contains("f 3//3 2//2 1//1\r\n"), "{text}");
    }

    #[test]
    fn the_mtl_carries_the_material_colours_and_its_texture() {
        let model = model([0.0, 0.0, 0.0]);
        let mut output = Vec::new();
        let written = write_model_ir_mtl(&model, &texture_set(), &mut output, 64 * 1024).unwrap();
        assert_eq!(written, u64::try_from(output.len()).unwrap());
        let text = std::str::from_utf8(&output).unwrap();

        assert!(text.contains("newmtl material_0_61\r\n"));
        // `_Color` supplies both the diffuse channel and the opacity.
        assert!(text.contains("Kd 0.5 0.25 0.125\r\n"), "{text}");
        assert!(text.contains("d 0.75\r\n"), "{text}");
        assert!(text.contains("map_Kd Body.png\r\n"));
    }

    #[test]
    fn replaces_material_values_an_obj_consumer_cannot_use() {
        // A NaN or out-of-range property is reachable from the asset and would
        // otherwise produce an MTL importers reject.
        let mut model = model([0.0, 0.0, 0.0]);
        model.materials[0].material.saved_properties.colors[0].value =
            [f32::NAN, -1.0, 5.0, f32::INFINITY];
        let mut output = Vec::new();
        write_model_ir_mtl(&model, &SceneTextureSet::default(), &mut output, 64 * 1024).unwrap();
        let text = std::str::from_utf8(&output).unwrap();
        assert!(text.contains("Kd 0 0 1\r\n"), "{text}");
        assert!(text.contains("d 1\r\n"), "{text}");
        assert!(!text.contains("NaN"));
        assert!(!text.contains("Infinity"));
    }

    #[test]
    fn accumulates_vertex_indices_across_groups() {
        // OBJ numbers vertices per file, so a second group must not restart at
        // 1 or its faces would reference the first group's geometry.
        let mut model = model([0.0, 0.0, 0.0]);
        let second = key(2);
        let mut node = model.nodes[0].clone();
        node.object = second;
        node.name = "second".to_owned();
        model = ModelIr::from_test_parts(
            vec![model.nodes[0].clone(), node],
            vec![key(1), second],
            model.meshes.clone(),
            model.materials.clone(),
        );
        let mut output = Vec::new();
        write_model_ir_obj(&model, None, &mut output, 64 * 1024).unwrap();
        let text = std::str::from_utf8(&output).unwrap();
        assert!(text.contains("f 3/3/3 2/2/2 1/1/1\r\n"));
        assert!(text.contains("f 6/6/6 5/5/5 4/4/4\r\n"), "{text}");
    }

    #[test]
    fn refuses_an_mtl_name_that_is_not_one_component() {
        let model = model([0.0, 0.0, 0.0]);
        for hostile in ["../evil.mtl", "a/b.mtl", "", ".."] {
            let mut output = Vec::new();
            assert!(
                write_model_ir_obj(&model, Some(hostile), &mut output, 64 * 1024).is_err(),
                "{hostile:?} was accepted"
            );
        }
    }

    #[test]
    fn honours_the_output_budget() {
        let model = model([0.0, 0.0, 0.0]);
        let mut output = Vec::new();
        let error = write_model_ir_obj(&model, None, &mut output, 8).unwrap_err();
        assert!(error.to_string().contains("output limit"));
    }

    #[test]
    fn joins_tokens_a_name_would_otherwise_break() {
        assert_eq!(ObjName("a b").to_string(), "a_b");
        assert_eq!(ObjName("a\nb").to_string(), "a_b");
        assert_eq!(ObjName("").to_string(), "unnamed");
    }
}
