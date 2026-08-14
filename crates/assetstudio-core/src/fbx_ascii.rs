//! Deterministic, bounded static ASCII FBX 7.4 emission.
//!
//! The public writer handles general transform hierarchies, resident triangle
//! meshes, submeshes, material slots, normals, UV0, non-identity local TRS,
//! and direct-bone skin clusters. The original one-triangle emitter remains
//! below as a byte-level oracle. Bone-name-hash fallback, animation, blend
//! shapes, and textures remain explicit gaps.

use std::fmt;
use std::io::{self, Write};

use crate::material::Material;
use crate::mesh::Mesh;
use crate::model_animation::ModelAnimationSet;
use crate::model_ir::{ModelCoordinateConvention, ModelIr, ModelLocalTransform, ModelRendererKind};
use crate::scene::Vector3;
use crate::scene_textures::SceneTextureSet;
use crate::{Error, Result};

const MODEL_ID: i64 = 1_000_000;
const GEOMETRY_ID: i64 = 1_000_001;
const MATERIAL_ID: i64 = 1_000_002;
const MAXIMUM_NAME_BYTES: usize = 4 * 1024;

/// Writes the supported static [`ModelIr`] as ASCII FBX 7.4.
///
/// Unity source coordinates are converted as the managed `ModelConverter`
/// does: position/normal X is mirrored, quaternion Y/Z are negated before
/// conversion to Euler degrees, and triangle winding is reversed. UV
/// coordinates are retained without a V flip.
/// Emission streams directly; a budget or I/O failure can leave a valid prefix
/// in `output`. Filesystem callers that require atomicity should use a temporary
/// file and publish it only after this function succeeds.
pub fn write_model_ir_fbx_ascii<W: Write>(
    model: &ModelIr,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    crate::fbx_scene_ascii::write_model_ir_fbx_ascii(model, output, maximum_output_bytes)
}

/// Writes a supported model plus verified animation tracks as ASCII FBX 7.4.
///
/// The current animation slice accepts explicit legacy Transform curves.
/// Streamed/dense muscle clips and compressed rotation curves are rejected by
/// the animation-IR builder until their sampling contracts are implemented.
pub fn write_model_ir_fbx_ascii_with_animations<W: Write>(
    model: &ModelIr,
    animations: &ModelAnimationSet,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    crate::fbx_scene_ascii::write_model_ir_fbx_ascii_with_animations(
        model,
        Some(animations),
        output,
        maximum_output_bytes,
    )
}

/// Writes a model, its animations and its material textures as ASCII FBX 7.4.
///
/// The `Texture` and `Video` objects reference each image by file name only, so
/// the caller must write the set beside the FBX with
/// [`SceneTextureSet::write_to_directory`](crate::scene_textures::SceneTextureSet::write_to_directory)
/// for the references to resolve.
pub fn write_model_ir_fbx_ascii_with_textures<W: Write>(
    model: &ModelIr,
    animations: &ModelAnimationSet,
    textures: &SceneTextureSet,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    crate::fbx_scene_ascii::write_model_ir_fbx_ascii_with_textures(
        model,
        Some(animations),
        Some(textures),
        output,
        maximum_output_bytes,
    )
}

/// Writes the original single-triangle compatibility fixture.
///
/// This is retained for the byte-level writer oracle while the public entry
/// point above emits the general static scene representation.
#[doc(hidden)]
pub fn write_single_triangle_model_ir_fbx_ascii<W: Write>(
    model: &ModelIr,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    let triangle = StaticTriangle::from_model(model)?;
    emit_triangle(&triangle, output, maximum_output_bytes)
}

fn emit_triangle<W: Write>(
    triangle: &StaticTriangle<'_>,
    output: &mut W,
    maximum_output_bytes: u64,
) -> Result<u64> {
    let mut bounded = BoundedWriter::new(output, maximum_output_bytes);
    let result = write_triangle(triangle, &mut bounded);
    if bounded.limit_exceeded {
        return Err(Error::invalid_data(format!(
            "ASCII FBX exceeds the {maximum_output_bytes} byte output limit"
        )));
    }
    result?;
    Ok(bounded.written)
}

struct StaticTriangle<'a> {
    node_name: &'a str,
    translation: Vector3,
    mesh: &'a Mesh,
    material: Option<&'a Material>,
}

impl<'a> StaticTriangle<'a> {
    fn from_model(model: &'a ModelIr) -> Result<Self> {
        if model.coordinate_convention != ModelCoordinateConvention::UnitySource {
            return Err(Error::unsupported(
                "ASCII FBX input must use the UnitySource coordinate convention",
            ));
        }
        if model.roots.len() != 1 || model.nodes.len() != 1 {
            return Err(Error::unsupported(format!(
                "ASCII FBX first slice requires one root and one node, found {} roots and {} nodes",
                model.roots.len(),
                model.nodes.len()
            )));
        }
        let node = &model.nodes[0];
        if model.roots[0] != node.object || node.parent.is_some() || !node.children.is_empty() {
            return Err(Error::invalid_data(
                "single-node model hierarchy is internally inconsistent",
            ));
        }
        if node.animator.is_some() || !model.avatars.is_empty() {
            return Err(Error::unsupported(
                "animation and Avatar metadata are not supported by the static ASCII FBX writer",
            ));
        }
        let transform = node.transform.ok_or_else(|| {
            Error::unsupported("ASCII FBX first slice requires a Transform on its root node")
        })?;
        validate_static_transform(transform)?;
        if node.renderers.len() != 1 {
            return Err(Error::unsupported(format!(
                "ASCII FBX first slice requires one renderer, found {}",
                node.renderers.len()
            )));
        }
        let renderer = &node.renderers[0];
        if !matches!(renderer.kind, ModelRendererKind::MeshRenderer { .. }) {
            return Err(Error::unsupported(
                "skinned meshes are not supported by the static ASCII FBX writer",
            ));
        }
        let mesh_key = renderer
            .mesh
            .ok_or_else(|| Error::unsupported("ASCII FBX first slice requires a resolved Mesh"))?;
        let mesh = &model
            .mesh(mesh_key)
            .ok_or_else(|| Error::invalid_data("renderer Mesh is absent from the model IR"))?
            .mesh;
        validate_mesh(mesh)?;
        if renderer.materials.len() > 1 {
            return Err(Error::unsupported(
                "ASCII FBX first slice supports at most one material slot",
            ));
        }
        let material = renderer
            .materials
            .first()
            .copied()
            .flatten()
            .map(|key| {
                model
                    .material(key)
                    .map(|entry| &entry.material)
                    .ok_or_else(|| {
                        Error::invalid_data("renderer Material is absent from the model IR")
                    })
            })
            .transpose()?;
        MaterialProperties::from_material(material).validate()?;
        validate_name(&node.name, "node")?;
        validate_name(&mesh.name, "mesh")?;
        if let Some(material) = material {
            validate_name(&material.name, "material")?;
        }
        Ok(Self {
            node_name: &node.name,
            translation: transform.local_position,
            mesh,
            material,
        })
    }
}

fn validate_static_transform(transform: ModelLocalTransform) -> Result<()> {
    let rotation = transform.local_rotation;
    if ![
        rotation.x,
        rotation.y,
        rotation.z,
        rotation.w,
        transform.local_position.x,
        transform.local_position.y,
        transform.local_position.z,
        transform.local_scale.x,
        transform.local_scale.y,
        transform.local_scale.z,
    ]
    .into_iter()
    .all(f32::is_finite)
    {
        return Err(Error::invalid_data(
            "model Transform contains a non-finite value",
        ));
    }
    if !is_zero(rotation.x)
        || !is_zero(rotation.y)
        || !is_zero(rotation.z)
        || rotation.w.to_bits() != 1.0_f32.to_bits()
    {
        return Err(Error::unsupported(
            "ASCII FBX first slice supports the canonical identity local rotation only",
        ));
    }
    if transform.local_scale.x.to_bits() != 1.0_f32.to_bits()
        || transform.local_scale.y.to_bits() != 1.0_f32.to_bits()
        || transform.local_scale.z.to_bits() != 1.0_f32.to_bits()
    {
        return Err(Error::unsupported(
            "ASCII FBX first slice supports unit local scale only",
        ));
    }
    Ok(())
}

const fn is_zero(value: f32) -> bool {
    matches!(value.to_bits(), 0 | 0x8000_0000)
}

fn validate_mesh(mesh: &Mesh) -> Result<()> {
    if mesh.vertices.len() != 3 || mesh.sub_meshes.len() != 1 {
        return Err(Error::unsupported(format!(
            "ASCII FBX first slice requires three vertices and one submesh, found {} vertices and {} submeshes",
            mesh.vertices.len(),
            mesh.sub_meshes.len()
        )));
    }
    let indices = &mesh.sub_meshes[0].indices;
    if indices.len() != 3 {
        return Err(Error::unsupported(format!(
            "ASCII FBX first slice requires one triangle, found {} indices",
            indices.len()
        )));
    }
    if let Some(index) = indices.iter().copied().find(|index| *index >= 3) {
        return Err(Error::invalid_data(format!(
            "triangle index {index} exceeds its three control points"
        )));
    }
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
    if let Some(normals) = &mesh.normals {
        if normals.len() != 3 {
            return Err(Error::invalid_data(format!(
                "Mesh has {} normals for three control points",
                normals.len()
            )));
        }
        if normals.iter().flatten().any(|value| !value.is_finite()) {
            return Err(Error::invalid_data(
                "Mesh normals contain a non-finite value",
            ));
        }
    }
    if let Some(uv) = &mesh.uv0 {
        if uv.len() != 3 {
            return Err(Error::invalid_data(format!(
                "Mesh has {} UV0 values for three control points",
                uv.len()
            )));
        }
        if uv.iter().flatten().any(|value| !value.is_finite()) {
            return Err(Error::invalid_data("Mesh UV0 contains a non-finite value"));
        }
    }
    Ok(())
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

fn write_triangle<W: Write>(triangle: &StaticTriangle<'_>, output: &mut W) -> io::Result<()> {
    write_preamble_and_model(triangle, output)?;
    write_geometry(triangle, output)?;
    write_layers(triangle.mesh, output)?;
    write_material_and_connections(triangle.material, output)
}

fn write_preamble_and_model<W: Write>(
    triangle: &StaticTriangle<'_>,
    output: &mut W,
) -> io::Result<()> {
    let node_name = SanitizedName(triangle.node_name, "Node");
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
    Count: 3\n\
    ObjectType: \"Model\" {{ Count: 1 }}\n\
    ObjectType: \"Geometry\" {{ Count: 1 }}\n\
    ObjectType: \"Material\" {{ Count: 1 }}\n\
}}\n\
Objects:  {{\n\
    Model: {MODEL_ID}, \"Model::{node_name}\", \"Mesh\" {{\n\
        Version: 232\n\
        Properties70:  {{\n\
            P: \"Lcl Translation\", \"Lcl Translation\", \"\", \"A\",{},{},{}\n\
            P: \"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",0,0,0\n\
            P: \"Lcl Scaling\", \"Lcl Scaling\", \"\", \"A\",1,1,1\n\
        }}\n\
        Shading: T\n\
        Culling: \"CullingOff\"\n\
    }}\n",
        FbxFloat(-triangle.translation.x),
        FbxFloat(triangle.translation.y),
        FbxFloat(triangle.translation.z),
    )
}

fn write_geometry<W: Write>(triangle: &StaticTriangle<'_>, output: &mut W) -> io::Result<()> {
    let mesh_name = SanitizedName(&triangle.mesh.name, "Mesh");
    write!(
        output,
        "    Geometry: {GEOMETRY_ID}, \"Geometry::{mesh_name}\", \"Mesh\" {{\n\
        GeometryVersion: 124\n\
        Vertices: *9 {{\n\
            a: {},{},{},{},{},{},{},{},{}\n\
        }}\n\
        PolygonVertexIndex: *3 {{\n\
            a: {},{},{}\n\
        }}\n",
        FbxFloat(-triangle.mesh.vertices[0][0]),
        FbxFloat(triangle.mesh.vertices[0][1]),
        FbxFloat(triangle.mesh.vertices[0][2]),
        FbxFloat(-triangle.mesh.vertices[1][0]),
        FbxFloat(triangle.mesh.vertices[1][1]),
        FbxFloat(triangle.mesh.vertices[1][2]),
        FbxFloat(-triangle.mesh.vertices[2][0]),
        FbxFloat(triangle.mesh.vertices[2][1]),
        FbxFloat(triangle.mesh.vertices[2][2]),
        triangle.mesh.sub_meshes[0].indices[2],
        triangle.mesh.sub_meshes[0].indices[1],
        polygon_end(triangle.mesh.sub_meshes[0].indices[0]),
    )
}

fn write_layers<W: Write>(mesh: &Mesh, output: &mut W) -> io::Result<()> {
    if let Some(normals) = &mesh.normals {
        write!(
            output,
            "        LayerElementNormal: 0 {{\n\
                Version: 101\n\
                Name: \"\"\n\
                MappingInformationType: \"ByVertice\"\n\
                ReferenceInformationType: \"Direct\"\n\
                Normals: *9 {{\n\
                    a: {},{},{},{},{},{},{},{},{}\n\
                }}\n\
            }}\n",
            FbxFloat(-normals[0][0]),
            FbxFloat(normals[0][1]),
            FbxFloat(normals[0][2]),
            FbxFloat(-normals[1][0]),
            FbxFloat(normals[1][1]),
            FbxFloat(normals[1][2]),
            FbxFloat(-normals[2][0]),
            FbxFloat(normals[2][1]),
            FbxFloat(normals[2][2]),
        )?;
    }
    if let Some(uv) = &mesh.uv0 {
        write!(
            output,
            "        LayerElementUV: 0 {{\n\
                Version: 101\n\
                Name: \"UV0\"\n\
                MappingInformationType: \"ByVertice\"\n\
                ReferenceInformationType: \"Direct\"\n\
                UV: *6 {{\n\
                    a: {},{},{},{},{},{}\n\
                }}\n\
            }}\n",
            FbxFloat(uv[0][0]),
            FbxFloat(uv[0][1]),
            FbxFloat(uv[1][0]),
            FbxFloat(uv[1][1]),
            FbxFloat(uv[2][0]),
            FbxFloat(uv[2][1]),
        )?;
    }
    write!(
        output,
        "        LayerElementMaterial: 0 {{\n\
            Version: 101\n\
            Name: \"\"\n\
            MappingInformationType: \"AllSame\"\n\
            ReferenceInformationType: \"IndexToDirect\"\n\
            Materials: *1 {{ a: 0 }}\n\
        }}\n\
        Layer: 0 {{\n\
            Version: 100\n"
    )?;
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
    write!(
        output,
        "            LayerElement: {{ Type: \"LayerElementMaterial\", TypedIndex: 0 }}\n\
        }}\n\
    }}\n"
    )
}

fn write_material_and_connections<W: Write>(
    material: Option<&Material>,
    output: &mut W,
) -> io::Result<()> {
    let material_name = SanitizedName(
        material.map_or("DefaultMaterial", |value| &value.name),
        "DefaultMaterial",
    );
    let properties = MaterialProperties::from_material(material);
    write!(
        output,
        "    Material: {MATERIAL_ID}, \"Material::{material_name}\", \"\" {{\n\
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
    }}\n\
}}\n\
Connections:  {{\n\
    C: \"OO\",{MODEL_ID},0\n\
    C: \"OO\",{GEOMETRY_ID},{MODEL_ID}\n\
    C: \"OO\",{MATERIAL_ID},{MODEL_ID}\n\
}}\n\
Takes:  {{\n\
    Current: \"\"\n\
}}\n",
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

fn polygon_end(index: u32) -> i64 {
    -(i64::from(index) + 1)
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
    use std::io::{self, Write};

    use crate::Error;
    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::mesh::{Mesh, MeshSubMesh};
    use crate::model_ir::{ModelIrLimits, ModelLocalTransform, build_model_ir};
    use crate::renderer::MESH_RENDERER_CLASS_ID;
    use crate::scene::{
        GAME_OBJECT_CLASS_ID, MESH_FILTER_CLASS_ID, Quaternion, TRANSFORM_CLASS_ID, Vector3,
    };
    use crate::scene_hierarchy::{SceneHierarchyLimits, SceneObjectKey, build_scene_hierarchy};
    use crate::serialized::{ObjectReference, SerializedFile};
    use crate::source::Region;

    use super::{
        MaterialProperties, StaticTriangle, emit_triangle, validate_mesh,
        validate_static_transform, write_model_ir_fbx_ascii,
    };

    const NULL: ObjectReference = ObjectReference {
        file_id: 0,
        path_id: 0,
    };

    fn triangle_mesh() -> Mesh {
        Mesh {
            path_id: 17,
            name: "Tri:/\\Bad".to_owned(),
            vertices: vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]],
            normals: Some(vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
            uv0: Some(vec![[0.0, 0.25], [0.5, 0.75], [1.0, 1.0]]),
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

    fn render(maximum: u64) -> crate::Result<Vec<u8>> {
        let mesh = triangle_mesh();
        let triangle = StaticTriangle {
            node_name: "Root:\"Bad",
            translation: Vector3 {
                x: 2.0,
                y: 3.0,
                z: 4.0,
            },
            mesh: &mesh,
            material: None,
        };
        let mut output = Vec::new();
        emit_triangle(&triangle, &mut output, maximum)?;
        Ok(output)
    }

    #[test]
    fn static_triangle_has_exact_golden_and_parseable_structure() {
        let output = render(64 * 1024).unwrap();
        let text = std::str::from_utf8(&output).unwrap();

        // Length plus FNV-1a locks every emitted byte without a large opaque
        // fixture; whitespace is compact because FBX ASCII indentation has no
        // semantic meaning, and the structural assertions keep failures local.
        assert_eq!(
            (output.len(), fnv1a64(&output)),
            (2453, 9_022_500_223_057_990_588)
        );
        assert!(text.starts_with("; FBX 7.4.0 project file\n"));
        assert!(text.ends_with("Current: \"\"\n}\n"));
        assert_eq!(brace_balance(text), Some(0));
        assert!(text.contains("Model::Root__Bad"));
        assert!(text.contains("Geometry::Tri___Bad"));
        assert!(text.contains("P: \"Lcl Translation\", \"Lcl Translation\", \"\", \"A\",-2,3,4"));
        assert!(text.contains("a: -1,2,3,-4,5,6,-7,8,9"));
        assert!(text.contains("a: 2,1,-1"));
        assert!(text.contains("a: 0,0.25,0.5,0.75,1,1"));
        assert!(text.contains("a: -1,0,0,0,1,0,0,0,1"));
    }

    #[test]
    fn output_budget_is_enforced_before_an_over_limit_write() {
        let full = render(64 * 1024).unwrap();
        let error = render(u64::try_from(full.len() - 1).unwrap()).unwrap_err();
        assert!(matches!(error, Error::InvalidData(message) if message.contains("output limit")));
    }

    #[test]
    fn unsupported_and_corrupt_triangle_shapes_are_rejected() {
        let mut mesh = triangle_mesh();
        mesh.vertices.push([0.0; 3]);
        assert!(matches!(validate_mesh(&mesh), Err(Error::Unsupported(_))));

        let mut mesh = triangle_mesh();
        mesh.sub_meshes[0].indices[2] = 3;
        assert!(matches!(validate_mesh(&mesh), Err(Error::InvalidData(_))));

        let mut mesh = triangle_mesh();
        mesh.uv0.as_mut().unwrap().pop();
        assert!(matches!(validate_mesh(&mesh), Err(Error::InvalidData(_))));

        let transform = ModelLocalTransform {
            component: SceneObjectKey {
                file_index: 0,
                path_id: 1,
            },
            local_rotation: Quaternion {
                x: 0.0,
                y: 0.0,
                z: 0.5,
                w: 0.5,
            },
            local_position: Vector3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            local_scale: Vector3 {
                x: 1.0,
                y: 1.0,
                z: 1.0,
            },
        };
        assert!(matches!(
            validate_static_transform(transform),
            Err(Error::Unsupported(_))
        ));

        let mut properties = MaterialProperties::from_material(None);
        properties.shininess = f32::NAN;
        assert!(matches!(properties.validate(), Err(Error::InvalidData(_))));
    }

    #[test]
    fn underlying_io_errors_are_not_misreported_as_budget_errors() {
        let mesh = triangle_mesh();
        let triangle = StaticTriangle {
            node_name: "root",
            translation: Vector3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            mesh: &mesh,
            material: None,
        };
        let mut output = FailingWriter { remaining: 16 };
        let error = emit_triangle(&triangle, &mut output, 64 * 1024).unwrap_err();
        assert!(matches!(error, Error::Io(error) if error.kind() == io::ErrorKind::BrokenPipe));
    }

    #[test]
    fn public_api_writes_a_model_ir_built_from_a_real_collection() {
        let model = model_fixture([0.0, 0.0, 0.0, 1.0]);
        let mut output = Vec::new();
        let written = write_model_ir_fbx_ascii(&model, &mut output, 64 * 1024).unwrap();
        assert_eq!(written, u64::try_from(output.len()).unwrap());
        let text = std::str::from_utf8(&output).unwrap();
        assert!(text.contains("Model::root"));
        assert!(text.contains("a: 2,1,-1"));

        let rotated = model_fixture([0.0, 0.0, 0.5, 0.5]);
        let mut rotated_output = Vec::new();
        write_model_ir_fbx_ascii(&rotated, &mut rotated_output, 64 * 1024).unwrap();
        let rotated_text = std::str::from_utf8(&rotated_output).unwrap();
        assert!(
            rotated_text.contains("P: \"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",0,0,-90")
        );
    }

    #[test]
    fn public_writer_emits_general_hierarchy_and_non_identity_trs() {
        let model = hierarchy_model_fixture();
        let mut output = Vec::new();
        write_model_ir_fbx_ascii(&model, &mut output, 128 * 1024).unwrap();
        let text = std::str::from_utf8(&output).unwrap();

        assert!(text.contains("ObjectType: \"Model\" { Count: 2 }"));
        assert!(text.contains("ObjectType: \"Geometry\" { Count: 1 }"));
        assert!(text.contains("Model::root\", \"Null\""));
        assert!(text.contains("Model::child\", \"Mesh\""));
        assert!(text.contains("C: \"OO\",1000000001,1000000000"));
        assert!(text.contains("P: \"Lcl Translation\", \"Lcl Translation\", \"\", \"A\",-5,6,7"));
        assert!(text.contains("P: \"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",0,0,-45"));
        assert!(text.contains("P: \"Lcl Scaling\", \"Lcl Scaling\", \"\", \"A\",2,3,4"));
        assert_eq!(brace_balance(text), Some(0));
    }

    struct FailingWriter {
        remaining: usize,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "synthetic failure",
                ));
            }
            let written = self.remaining.min(bytes.len()).min(3);
            self.remaining -= written;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn model_fixture(rotation: [f32; 4]) -> crate::model_ir::ModelIr {
        let objects = [
            (
                GAME_OBJECT_CLASS_ID,
                1,
                game_object(&[reference(11), reference(21), reference(31)]),
            ),
            (TRANSFORM_CLASS_ID, 11, transform_object(rotation)),
            (MESH_FILTER_CLASS_ID, 21, mesh_filter_object()),
            (MESH_RENDERER_CLASS_ID, 31, renderer_object()),
            (crate::mesh::MESH_CLASS_ID, 51, mesh_serialized_object()),
        ];
        let file = SerializedFile::open(Region::from_bytes(synthetic_v22(&objects))).unwrap();
        let collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "fbx.assets".to_owned(),
                file,
            }],
            Vec::new(),
        );
        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();
        build_model_ir(&collection, &hierarchy, ModelIrLimits::default()).unwrap()
    }

    fn hierarchy_model_fixture() -> crate::model_ir::ModelIr {
        let objects = [
            (
                GAME_OBJECT_CLASS_ID,
                1,
                named_game_object("root", &[reference(11)]),
            ),
            (
                GAME_OBJECT_CLASS_ID,
                2,
                named_game_object("child", &[reference(12), reference(22), reference(32)]),
            ),
            (
                TRANSFORM_CLASS_ID,
                11,
                transform_object_with(
                    reference(1),
                    [0.0, 0.0, 0.0, 1.0],
                    [1.0, 2.0, 3.0],
                    [1.0, 1.0, 1.0],
                    &[reference(12)],
                    NULL,
                ),
            ),
            (
                TRANSFORM_CLASS_ID,
                12,
                transform_object_with(
                    reference(2),
                    [0.0, 0.0, 0.382_683_43, 0.923_879_5],
                    [5.0, 6.0, 7.0],
                    [2.0, 3.0, 4.0],
                    &[],
                    reference(11),
                ),
            ),
            (
                MESH_FILTER_CLASS_ID,
                22,
                mesh_filter_object_with(reference(2), reference(51)),
            ),
            (
                MESH_RENDERER_CLASS_ID,
                32,
                renderer_object_with(reference(2)),
            ),
            (crate::mesh::MESH_CLASS_ID, 51, mesh_serialized_object()),
        ];
        let file = SerializedFile::open(Region::from_bytes(synthetic_v22(&objects))).unwrap();
        let collection = AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "hierarchy.assets".to_owned(),
                file,
            }],
            Vec::new(),
        );
        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();
        build_model_ir(&collection, &hierarchy, ModelIrLimits::default()).unwrap()
    }

    const fn reference(path_id: i64) -> ObjectReference {
        ObjectReference {
            file_id: 0,
            path_id,
        }
    }

    fn game_object(components: &[ObjectReference]) -> Vec<u8> {
        named_game_object("root", components)
    }

    fn named_game_object(name: &str, components: &[ObjectReference]) -> Vec<u8> {
        let mut output = Vec::new();
        push_i32(&mut output, i32::try_from(components.len()).unwrap());
        for component in components {
            push_pptr(&mut output, *component);
        }
        push_i32(&mut output, 0);
        push_aligned_string(&mut output, name);
        output
    }

    fn transform_object(rotation: [f32; 4]) -> Vec<u8> {
        transform_object_with(
            reference(1),
            rotation,
            [2.0, 3.0, 4.0],
            [1.0, 1.0, 1.0],
            &[],
            NULL,
        )
    }

    fn transform_object_with(
        owner: ObjectReference,
        rotation: [f32; 4],
        position: [f32; 3],
        scale: [f32; 3],
        children: &[ObjectReference],
        father: ObjectReference,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, owner);
        push_f32s(&mut output, &rotation);
        push_f32s(&mut output, &position);
        push_f32s(&mut output, &scale);
        push_i32(&mut output, i32::try_from(children.len()).unwrap());
        for child in children {
            push_pptr(&mut output, *child);
        }
        push_pptr(&mut output, father);
        output
    }

    fn mesh_filter_object() -> Vec<u8> {
        mesh_filter_object_with(reference(1), reference(51))
    }

    fn mesh_filter_object_with(owner: ObjectReference, mesh: ObjectReference) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, owner);
        push_pptr(&mut output, mesh);
        output
    }

    fn renderer_object() -> Vec<u8> {
        renderer_object_with(reference(1))
    }

    fn renderer_object_with(owner: ObjectReference) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, owner);
        output.extend_from_slice(&[1, 2, 1, 0, 0, 0, 0, 0, 0, 0]);
        align(&mut output, 4);
        output.extend_from_slice(&u32::MAX.to_le_bytes());
        push_i32(&mut output, 0);
        output.extend_from_slice(&[0_u8; 36]);
        push_i32(&mut output, 0);
        output.extend_from_slice(&[0_u8; 4]);
        for _ in 0..3 {
            push_pptr(&mut output, NULL);
        }
        output.extend_from_slice(&[0_u8; 8]);
        align(&mut output, 4);
        output
    }

    fn mesh_serialized_object() -> Vec<u8> {
        let mut output = Vec::new();
        push_mesh_header(&mut output);
        push_mesh_vertex_data(&mut output);
        push_mesh_tail(&mut output);
        output
    }

    fn push_mesh_header(output: &mut Vec<u8>) {
        push_aligned_string(output, "tri");
        push_i32(output, 1);
        for value in [0_u32, 3, 0, 0, 0, 3] {
            push_u32(output, value);
        }
        output.extend_from_slice(&[0_u8; 24]);
        for _ in 0..3 {
            push_i32(output, 0);
        }
        push_u32(output, 0);
        for _ in 0..5 {
            push_i32(output, 0);
        }
        output.extend_from_slice(&[0, 1, 0, 0]);
        align(output, 4);
        push_i32(output, 0);
        push_i32(output, 6);
        for index in 0..3_u16 {
            output.extend_from_slice(&index.to_le_bytes());
        }
        align(output, 4);
    }

    fn push_mesh_vertex_data(output: &mut Vec<u8>) {
        push_u32(output, 3);
        push_i32(output, 5);
        output.extend_from_slice(&[0, 0, 0, 3]);
        for _ in 0..4 {
            output.extend_from_slice(&[0_u8; 4]);
        }
        push_i32(output, 36);
        for vertex in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            push_f32s(output, &vertex);
        }
        align(output, 4);
    }

    fn push_mesh_tail(output: &mut Vec<u8>) {
        for _ in 0..4 {
            push_empty_packed_float(output);
        }
        for _ in 0..3 {
            push_empty_packed_int(output);
        }
        push_empty_packed_float(output);
        for _ in 0..2 {
            push_empty_packed_int(output);
        }
        push_u32(output, 0);
        output.extend_from_slice(&[0_u8; 24]);
        for _ in 0..3 {
            push_i32(output, 0);
        }
        align(output, 4);
        push_i32(output, 0);
        align(output, 4);
        output.extend_from_slice(&[0_u8; 8]);
        align(output, 4);
        output.extend_from_slice(&0_i64.to_le_bytes());
        push_u32(output, 0);
        push_aligned_string(output, "");
    }

    fn push_empty_packed_float(output: &mut Vec<u8>) {
        push_u32(output, 0);
        push_f32s(output, &[0.0, 0.0]);
        push_i32(output, 0);
        align(output, 4);
        output.push(0);
        align(output, 4);
    }

    fn push_empty_packed_int(output: &mut Vec<u8>) {
        push_u32(output, 0);
        push_i32(output, 0);
        align(output, 4);
        output.push(0);
        align(output, 4);
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
        push_i32(&mut metadata, 0);
        push_i32(&mut metadata, 0);
        push_i32(&mut metadata, 0);
        metadata.push(0);
        finish_v22(&metadata, &data)
    }

    fn finish_v22(metadata: &[u8], data: &[u8]) -> Vec<u8> {
        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (48_u64 + u64::from(metadata_size)).next_multiple_of(16);
        let file_size = data_offset + u64::try_from(data.len()).unwrap();
        let mut output = vec![0_u8; 48];
        output[8..12].copy_from_slice(&22_u32.to_be_bytes());
        output[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        output.extend_from_slice(metadata);
        output.resize(usize::try_from(data_offset).unwrap(), 0);
        output.extend_from_slice(data);
        output
    }

    fn push_pptr(output: &mut Vec<u8>, reference: ObjectReference) {
        push_i32(output, reference.file_id);
        output.extend_from_slice(&reference.path_id.to_le_bytes());
    }

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_f32s(output: &mut Vec<u8>, values: &[f32]) {
        for value in values {
            output.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        if !value.is_empty() {
            align(output, 4);
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

    fn fnv1a64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    fn brace_balance(text: &str) -> Option<i32> {
        text.bytes().try_fold(0_i32, |depth, byte| match byte {
            b'{' => depth.checked_add(1),
            b'}' if depth > 0 => depth.checked_sub(1),
            b'}' => None,
            _ => Some(depth),
        })
    }
}
