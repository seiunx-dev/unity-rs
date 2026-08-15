//! Bounded, collection-wide static model intermediate representation.
//!
//! This module deliberately stops before FBX coordinate conversion and
//! animation sampling. It joins the already verified scene, mesh (including
//! source skin data), material, and avatar readers into a stable graph that a
//! writer can consume without resolving Unity pointers again.

use std::collections::{BTreeMap, HashSet};

use crate::animation_component::ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID;
use crate::animator_controller::ANIMATOR_CONTROLLER_CLASS_ID;
use crate::avatar::{AVATAR_CLASS_ID, Avatar, AvatarReadLimits, read_avatar};
use crate::loader::AssetCollection;
use crate::material::{MATERIAL_CLASS_ID, Material, MaterialReadLimits, read_material};
use crate::mesh::{MESH_CLASS_ID, Mesh, MeshReadLimits, read_mesh_with_collection};
use crate::scene::{Quaternion, Vector3, resolve_object_reference};
use crate::scene_hierarchy::{SceneHierarchy, SceneObjectKey};
use crate::serialized::ObjectReference;
use crate::{Error, Result};

/// Coordinates stored in this IR.
///
/// `AssetStudio`'s managed FBX path later mirrors position X, changes the
/// quaternion signs, and asks the native FBX wrapper for Euler angles. Keeping
/// source coordinates here avoids baking that writer-specific conversion into
/// shared model data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCoordinateConvention {
    /// Unity local position, quaternion, and scale exactly as serialized.
    UnitySource,
}

/// Collection-wide limits in addition to each source-bound object reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelIrLimits {
    pub maximum_nodes: usize,
    pub maximum_roots: usize,
    pub maximum_hierarchy_edges: usize,
    pub maximum_renderers: usize,
    pub maximum_material_references: usize,
    pub maximum_bone_references: usize,
    pub maximum_meshes: usize,
    pub maximum_materials: usize,
    pub maximum_avatars: usize,
    pub maximum_mesh_vertices: usize,
    pub maximum_mesh_indices: usize,
    pub maximum_mesh_sub_meshes: usize,
    pub maximum_material_entries: usize,
    pub maximum_avatar_elements: usize,
    pub maximum_total_string_bytes: usize,
    pub mesh: MeshReadLimits,
    pub material: MaterialReadLimits,
    pub avatar: AvatarReadLimits,
}

impl Default for ModelIrLimits {
    fn default() -> Self {
        Self {
            maximum_nodes: 1_000_000,
            maximum_roots: 1_000_000,
            maximum_hierarchy_edges: 1_000_000,
            maximum_renderers: 2_000_000,
            maximum_material_references: 10_000_000,
            maximum_bone_references: 10_000_000,
            maximum_meshes: 1_000_000,
            maximum_materials: 1_000_000,
            maximum_avatars: 1_000_000,
            maximum_mesh_vertices: 100_000_000,
            maximum_mesh_indices: 300_000_000,
            maximum_mesh_sub_meshes: 10_000_000,
            maximum_material_entries: 10_000_000,
            maximum_avatar_elements: 10_000_000,
            maximum_total_string_bytes: 256 * 1024 * 1024,
            mesh: MeshReadLimits::default(),
            material: MaterialReadLimits::default(),
            avatar: AvatarReadLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelLocalTransform {
    pub component: SceneObjectKey,
    pub local_rotation: Quaternion,
    pub local_position: Vector3,
    pub local_scale: Vector3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRendererKind {
    MeshRenderer { mesh_filter: Option<SceneObjectKey> },
    SkinnedMeshRenderer { bones: Vec<Option<SceneObjectKey>> },
}

/// A renderer binding in the same per-GameObject order as the managed model
/// converter: `MeshRenderer` first, then `SkinnedMeshRenderer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRendererBinding {
    pub component: SceneObjectKey,
    pub kind: ModelRendererKind,
    pub mesh: Option<SceneObjectKey>,
    /// Slots retain unresolved entries as `None`, matching `PPtr.TryGet`.
    pub materials: Vec<Option<SceneObjectKey>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelAnimatorMetadata {
    pub component: SceneObjectKey,
    pub avatar: Option<SceneObjectKey>,
    /// Class 91 or class 221. Controller contents belong to `animation_graph`.
    pub controller: Option<SceneObjectKey>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelNode {
    pub object: SceneObjectKey,
    pub name: String,
    /// `false` marks an ancestor retained only as transform context for a
    /// selected subtree; its components and animation bindings are excluded.
    pub export_content: bool,
    pub parent: Option<SceneObjectKey>,
    pub children: Vec<SceneObjectKey>,
    pub transform: Option<ModelLocalTransform>,
    pub renderers: Vec<ModelRendererBinding>,
    pub animator: Option<ModelAnimatorMetadata>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMesh {
    pub object: SceneObjectKey,
    pub mesh: Mesh,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMaterial {
    pub object: SceneObjectKey,
    pub material: Material,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelAvatar {
    pub object: SceneObjectKey,
    pub avatar: Avatar,
}

/// Stable static model graph. Asset vectors are ordered by first reference
/// while walking `nodes`; lookup maps do not affect observable ordering.
#[derive(Debug, Clone)]
pub struct ModelIr {
    pub coordinate_convention: ModelCoordinateConvention,
    pub nodes: Vec<ModelNode>,
    pub roots: Vec<SceneObjectKey>,
    pub meshes: Vec<ModelMesh>,
    pub materials: Vec<ModelMaterial>,
    pub avatars: Vec<ModelAvatar>,
    node_index: BTreeMap<SceneObjectKey, usize>,
    mesh_index: BTreeMap<SceneObjectKey, usize>,
    material_index: BTreeMap<SceneObjectKey, usize>,
    avatar_index: BTreeMap<SceneObjectKey, usize>,
}

impl ModelIr {
    /// Returns the deterministic node-vector index for a stable object key.
    #[must_use]
    pub fn node_index(&self, key: SceneObjectKey) -> Option<usize> {
        self.node_index.get(&key).copied()
    }

    #[must_use]
    pub fn node(&self, key: SceneObjectKey) -> Option<&ModelNode> {
        self.node_index
            .get(&key)
            .and_then(|index| self.nodes.get(*index))
    }

    #[must_use]
    pub fn mesh(&self, key: SceneObjectKey) -> Option<&ModelMesh> {
        self.mesh_index
            .get(&key)
            .and_then(|index| self.meshes.get(*index))
    }

    /// Returns the deterministic mesh-vector index for a stable object key.
    #[must_use]
    pub fn mesh_index(&self, key: SceneObjectKey) -> Option<usize> {
        self.mesh_index.get(&key).copied()
    }

    #[must_use]
    pub fn material(&self, key: SceneObjectKey) -> Option<&ModelMaterial> {
        self.material_index
            .get(&key)
            .and_then(|index| self.materials.get(*index))
    }

    /// Returns the deterministic material-vector index for a stable object key.
    #[must_use]
    pub fn material_index(&self, key: SceneObjectKey) -> Option<usize> {
        self.material_index.get(&key).copied()
    }

    #[must_use]
    pub fn avatar(&self, key: SceneObjectKey) -> Option<&ModelAvatar> {
        self.avatar_index
            .get(&key)
            .and_then(|index| self.avatars.get(*index))
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts(
        nodes: Vec<ModelNode>,
        roots: Vec<SceneObjectKey>,
        meshes: Vec<ModelMesh>,
        materials: Vec<ModelMaterial>,
    ) -> Self {
        let node_index = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.object, index))
            .collect();
        let mesh_index = meshes
            .iter()
            .enumerate()
            .map(|(index, mesh)| (mesh.object, index))
            .collect();
        let material_index = materials
            .iter()
            .enumerate()
            .map(|(index, material)| (material.object, index))
            .collect();
        Self {
            coordinate_convention: ModelCoordinateConvention::UnitySource,
            nodes,
            roots,
            meshes,
            materials,
            avatars: Vec::new(),
            node_index,
            mesh_index,
            material_index,
            avatar_index: BTreeMap::new(),
        }
    }
}

/// Joins a verified scene hierarchy with referenced, model-relevant objects.
///
/// Missing, malformed, or wrongly typed pointers are ignored as managed
/// `PPtr.TryGet` would ignore them. Once the correct target class resolves,
/// its parser and every collection-wide budget remain strict.
pub fn build_model_ir(
    collection: &AssetCollection,
    hierarchy: &SceneHierarchy,
    limits: ModelIrLimits,
) -> Result<ModelIr> {
    let mut state = ModelBuildState::new(collection, &limits);
    state.copy_hierarchy(hierarchy)?;
    Ok(state.finish())
}

/// Builds the model branch rooted at one `GameObject`.
///
/// The selected object keeps its complete descendant subtree. Its ancestor
/// chain is retained as transform-only context with sibling branches pruned,
/// matching the managed `ModelConverter(GameObject)` frame selection.
pub fn build_model_ir_for_game_object(
    collection: &AssetCollection,
    hierarchy: &SceneHierarchy,
    game_object: SceneObjectKey,
    limits: ModelIrLimits,
) -> Result<ModelIr> {
    let selection = HierarchySelection::build(hierarchy, game_object, limits.maximum_nodes)?;
    let mut state = ModelBuildState::new(collection, &limits);
    state.copy_selected_hierarchy(hierarchy, &selection)?;
    Ok(state.finish())
}

struct HierarchySelection {
    ordered: Vec<SceneObjectKey>,
    included: HashSet<SceneObjectKey>,
    subtree: HashSet<SceneObjectKey>,
    root: SceneObjectKey,
}

impl HierarchySelection {
    fn build(
        hierarchy: &SceneHierarchy,
        game_object: SceneObjectKey,
        maximum_nodes: usize,
    ) -> Result<Self> {
        let selected = hierarchy.node(game_object).ok_or_else(|| {
            Error::invalid_data(format!(
                "model GameObject {game_object:?} is absent from the scene hierarchy"
            ))
        })?;
        let mut ancestors = Vec::new();
        let mut parent = selected.parent;
        while let Some(key) = parent {
            require_next(ancestors.len(), maximum_nodes, "selected model nodes")?;
            ancestors.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow selected model ancestors: {error}"))
            })?;
            ancestors.push(key);
            parent = hierarchy
                .node(key)
                .ok_or_else(|| {
                    Error::invalid_data(format!(
                        "selected model ancestor {key:?} is absent from the hierarchy"
                    ))
                })?
                .parent;
        }
        ancestors.reverse();

        let mut ordered = reserve_vec(ancestors.len().saturating_add(1), "selected model nodes")?;
        let mut included = HashSet::new();
        for key in ancestors {
            push_selected_key(&mut ordered, &mut included, key, maximum_nodes)?;
        }
        let root = ordered.first().copied().unwrap_or(game_object);

        let mut stack = reserve_vec(1, "selected model traversal stack")?;
        let mut subtree = HashSet::new();
        stack.push(game_object);
        while let Some(key) = stack.pop() {
            push_selected_key(&mut ordered, &mut included, key, maximum_nodes)?;
            subtree.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow selected model subtree index: {error}"))
            })?;
            subtree.insert(key);
            let node = hierarchy.node(key).ok_or_else(|| {
                Error::invalid_data(format!(
                    "selected model descendant {key:?} is absent from the hierarchy"
                ))
            })?;
            stack.try_reserve(node.children.len()).map_err(|error| {
                Error::invalid_data(format!("cannot grow selected model traversal: {error}"))
            })?;
            stack.extend(node.children.iter().rev().copied());
        }
        Ok(Self {
            ordered,
            included,
            subtree,
            root,
        })
    }
}

fn push_selected_key(
    ordered: &mut Vec<SceneObjectKey>,
    included: &mut HashSet<SceneObjectKey>,
    key: SceneObjectKey,
    maximum_nodes: usize,
) -> Result<()> {
    if included.contains(&key) {
        return Err(Error::invalid_data(format!(
            "selected model hierarchy repeats GameObject {key:?}"
        )));
    }
    require_next(ordered.len(), maximum_nodes, "selected model nodes")?;
    ordered.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow selected model nodes: {error}"))
    })?;
    included.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow selected model index: {error}"))
    })?;
    included.insert(key);
    ordered.push(key);
    Ok(())
}

#[derive(Default)]
struct ModelTotals {
    hierarchy_edges: usize,
    renderers: usize,
    material_references: usize,
    bone_references: usize,
    mesh_vertices: usize,
    mesh_indices: usize,
    mesh_sub_meshes: usize,
    material_entries: usize,
    avatar_elements: usize,
    string_bytes: usize,
}

struct ModelBuildState<'a> {
    collection: &'a AssetCollection,
    limits: ModelIrLimits,
    nodes: Vec<ModelNode>,
    roots: Vec<SceneObjectKey>,
    meshes: Vec<ModelMesh>,
    materials: Vec<ModelMaterial>,
    avatars: Vec<ModelAvatar>,
    node_index: BTreeMap<SceneObjectKey, usize>,
    mesh_index: BTreeMap<SceneObjectKey, usize>,
    material_index: BTreeMap<SceneObjectKey, usize>,
    avatar_index: BTreeMap<SceneObjectKey, usize>,
    totals: ModelTotals,
}

impl<'a> ModelBuildState<'a> {
    fn new(collection: &'a AssetCollection, limits: &ModelIrLimits) -> Self {
        Self {
            collection,
            limits: *limits,
            nodes: Vec::new(),
            roots: Vec::new(),
            meshes: Vec::new(),
            materials: Vec::new(),
            avatars: Vec::new(),
            node_index: BTreeMap::new(),
            mesh_index: BTreeMap::new(),
            material_index: BTreeMap::new(),
            avatar_index: BTreeMap::new(),
            totals: ModelTotals::default(),
        }
    }

    fn copy_hierarchy(&mut self, hierarchy: &SceneHierarchy) -> Result<()> {
        require_maximum(
            hierarchy.nodes.len(),
            self.limits.maximum_nodes,
            "model nodes",
        )?;
        require_maximum(
            hierarchy.roots.len(),
            self.limits.maximum_roots,
            "model roots",
        )?;
        self.nodes = reserve_vec(hierarchy.nodes.len(), "model nodes")?;
        self.roots = reserve_vec(hierarchy.roots.len(), "model roots")?;
        self.roots.extend_from_slice(&hierarchy.roots);

        for source in &hierarchy.nodes {
            self.copy_node(source, source.parent, &source.children, true)?;
        }
        Ok(())
    }

    fn copy_selected_hierarchy(
        &mut self,
        hierarchy: &SceneHierarchy,
        selection: &HierarchySelection,
    ) -> Result<()> {
        require_maximum(
            selection.ordered.len(),
            self.limits.maximum_nodes,
            "selected model nodes",
        )?;
        require_maximum(1, self.limits.maximum_roots, "selected model roots")?;
        self.nodes = reserve_vec(selection.ordered.len(), "selected model nodes")?;
        self.roots = reserve_vec(1, "selected model roots")?;
        self.roots.push(selection.root);
        for key in &selection.ordered {
            let source = hierarchy.node(*key).ok_or_else(|| {
                Error::invalid_data(format!(
                    "selected model GameObject {key:?} is absent from the hierarchy"
                ))
            })?;
            let parent = source
                .parent
                .filter(|parent| selection.included.contains(parent));
            let mut children = reserve_vec(source.children.len(), "selected model children")?;
            children.extend(
                source
                    .children
                    .iter()
                    .copied()
                    .filter(|child| selection.included.contains(child)),
            );
            self.copy_node(source, parent, &children, selection.subtree.contains(key))?;
        }
        Ok(())
    }

    fn copy_node(
        &mut self,
        source: &crate::scene_hierarchy::SceneHierarchyNode,
        parent: Option<SceneObjectKey>,
        children: &[SceneObjectKey],
        include_components: bool,
    ) -> Result<()> {
        self.charge_string(source.name.len(), "GameObject name")?;
        self.charge_total(
            TotalKind::HierarchyEdges,
            children.len(),
            self.limits.maximum_hierarchy_edges,
            "model hierarchy edges",
        )?;
        let transform = source
            .transform
            .as_ref()
            .map(|binding| ModelLocalTransform {
                component: binding.component,
                local_rotation: binding.local_rotation,
                local_position: binding.local_position,
                local_scale: binding.local_scale,
            });
        let mut renderers = Vec::new();
        let mut animator = None;
        if include_components {
            renderers = reserve_vec(
                usize::from(source.mesh_renderer.is_some())
                    + usize::from(source.skinned_mesh_renderer.is_some()),
                "node renderers",
            )?;
            if let Some(renderer) = &source.mesh_renderer {
                let mesh_filter = source.mesh_filter.as_ref().map(|filter| filter.component);
                let mesh = source.mesh_filter.as_ref().and_then(|filter| filter.mesh);
                renderers.push(self.bind_renderer(
                    renderer.component,
                    ModelRendererKind::MeshRenderer { mesh_filter },
                    mesh,
                    &renderer.materials,
                )?);
            }
            if let Some(renderer) = &source.skinned_mesh_renderer {
                self.charge_total(
                    TotalKind::BoneReferences,
                    renderer.bones.len(),
                    self.limits.maximum_bone_references,
                    "model bone references",
                )?;
                renderers.push(self.bind_renderer(
                    renderer.component,
                    ModelRendererKind::SkinnedMeshRenderer {
                        bones: clone_slice(&renderer.bones, "SkinnedMeshRenderer bones")?,
                    },
                    renderer.mesh,
                    &renderer.materials,
                )?);
            }
            animator = source
                .animator
                .as_ref()
                .map(|binding| {
                    self.bind_animator(binding.component, binding.avatar, binding.controller)
                })
                .transpose()?;
        }
        let index = self.nodes.len();
        if self.node_index.insert(source.object, index).is_some() {
            return Err(Error::invalid_data(format!(
                "duplicate model GameObject identity {:?}",
                source.object
            )));
        }
        self.nodes.push(ModelNode {
            object: source.object,
            name: clone_string(&source.name, "GameObject name")?,
            export_content: include_components,
            parent,
            children: clone_slice(children, "model node children")?,
            transform,
            renderers,
            animator,
        });
        Ok(())
    }

    fn bind_renderer(
        &mut self,
        component: SceneObjectKey,
        kind: ModelRendererKind,
        mesh: Option<SceneObjectKey>,
        materials: &[Option<SceneObjectKey>],
    ) -> Result<ModelRendererBinding> {
        self.charge_total(
            TotalKind::Renderers,
            1,
            self.limits.maximum_renderers,
            "model renderers",
        )?;
        self.charge_total(
            TotalKind::MaterialReferences,
            materials.len(),
            self.limits.maximum_material_references,
            "model material references",
        )?;
        // A renderer whose mesh this reader declines contributes no geometry,
        // and losing it is the right outcome: one empty mesh among a bundle's
        // 152 otherwise cost the whole scene. A malformed mesh still fails --
        // that is a statement about the bytes, not about the asset.
        let mesh = match mesh {
            None => None,
            Some(key) => match self.register_mesh(key) {
                Ok(()) => Some(key),
                Err(Error::Unsupported(_)) => None,
                Err(error) => return Err(error),
            },
        };
        for material in materials.iter().flatten().copied() {
            self.register_material(material)?;
        }
        Ok(ModelRendererBinding {
            component,
            kind,
            mesh,
            materials: clone_slice(materials, "model renderer materials")?,
        })
    }

    fn bind_animator(
        &mut self,
        component: SceneObjectKey,
        avatar_reference: ObjectReference,
        controller_reference: ObjectReference,
    ) -> Result<ModelAnimatorMetadata> {
        let avatar = try_resolve_typed_key(
            self.collection,
            component.file_index,
            avatar_reference,
            &[AVATAR_CLASS_ID],
        );
        if let Some(avatar) = avatar {
            self.register_avatar(avatar)?;
        }
        let controller = try_resolve_typed_key(
            self.collection,
            component.file_index,
            controller_reference,
            &[
                ANIMATOR_CONTROLLER_CLASS_ID,
                ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID,
            ],
        );
        Ok(ModelAnimatorMetadata {
            component,
            avatar,
            controller,
        })
    }

    fn register_mesh(&mut self, key: SceneObjectKey) -> Result<()> {
        if self.mesh_index.contains_key(&key) {
            return Ok(());
        }
        require_next(
            self.meshes.len(),
            self.limits.maximum_meshes,
            "model meshes",
        )?;
        let (file, object_index) = require_key_target(self.collection, key, MESH_CLASS_ID, "Mesh")?;
        let mesh =
            read_mesh_with_collection(self.collection, file, object_index, self.limits.mesh)?;
        self.charge_string(mesh.name.len(), "Mesh name")?;
        self.charge_total(
            TotalKind::MeshVertices,
            mesh.vertices.len(),
            self.limits.maximum_mesh_vertices,
            "model Mesh vertices",
        )?;
        let index_count = mesh
            .sub_meshes
            .iter()
            .try_fold(0_usize, |total, sub_mesh| {
                total
                    .checked_add(sub_mesh.indices.len())
                    .ok_or_else(|| Error::invalid_data("model Mesh index count overflowed"))
            })?;
        self.charge_total(
            TotalKind::MeshIndices,
            index_count,
            self.limits.maximum_mesh_indices,
            "model Mesh indices",
        )?;
        self.charge_total(
            TotalKind::MeshSubMeshes,
            mesh.sub_meshes.len(),
            self.limits.maximum_mesh_sub_meshes,
            "model Mesh submeshes",
        )?;
        let index = self.meshes.len();
        self.meshes.push(ModelMesh { object: key, mesh });
        self.mesh_index.insert(key, index);
        Ok(())
    }

    fn register_material(&mut self, key: SceneObjectKey) -> Result<()> {
        if self.material_index.contains_key(&key) {
            return Ok(());
        }
        require_next(
            self.materials.len(),
            self.limits.maximum_materials,
            "model materials",
        )?;
        let (file, object_index) =
            require_key_target(self.collection, key, MATERIAL_CLASS_ID, "Material")?;
        let material = read_material(file, object_index, self.limits.material)?;
        self.charge_string(material_string_bytes(&material)?, "Material strings")?;
        self.charge_total(
            TotalKind::MaterialEntries,
            material_entry_count(&material)?,
            self.limits.maximum_material_entries,
            "model Material entries",
        )?;
        let index = self.materials.len();
        self.materials.push(ModelMaterial {
            object: key,
            material,
        });
        self.material_index.insert(key, index);
        Ok(())
    }

    fn register_avatar(&mut self, key: SceneObjectKey) -> Result<()> {
        if self.avatar_index.contains_key(&key) {
            return Ok(());
        }
        require_next(
            self.avatars.len(),
            self.limits.maximum_avatars,
            "model Avatars",
        )?;
        let (file, object_index) =
            require_key_target(self.collection, key, AVATAR_CLASS_ID, "Avatar")?;
        let avatar = read_avatar(file, object_index, self.limits.avatar)?;
        self.charge_string(avatar_string_bytes(&avatar)?, "Avatar strings")?;
        self.charge_total(
            TotalKind::AvatarElements,
            avatar_element_count(&avatar)?,
            self.limits.maximum_avatar_elements,
            "model Avatar elements",
        )?;
        let index = self.avatars.len();
        self.avatars.push(ModelAvatar {
            object: key,
            avatar,
        });
        self.avatar_index.insert(key, index);
        Ok(())
    }

    fn charge_string(&mut self, additional: usize, field: &str) -> Result<()> {
        self.totals.string_bytes = self
            .totals
            .string_bytes
            .checked_add(additional)
            .ok_or_else(|| Error::invalid_data("model IR string-byte count overflowed"))?;
        if self.totals.string_bytes > self.limits.maximum_total_string_bytes {
            return Err(Error::invalid_data(format!(
                "{field} raise model strings to {} bytes, exceeding limit {}",
                self.totals.string_bytes, self.limits.maximum_total_string_bytes
            )));
        }
        Ok(())
    }

    fn charge_total(
        &mut self,
        kind: TotalKind,
        additional: usize,
        maximum: usize,
        field: &str,
    ) -> Result<()> {
        let total = match kind {
            TotalKind::HierarchyEdges => &mut self.totals.hierarchy_edges,
            TotalKind::Renderers => &mut self.totals.renderers,
            TotalKind::MaterialReferences => &mut self.totals.material_references,
            TotalKind::BoneReferences => &mut self.totals.bone_references,
            TotalKind::MeshVertices => &mut self.totals.mesh_vertices,
            TotalKind::MeshIndices => &mut self.totals.mesh_indices,
            TotalKind::MeshSubMeshes => &mut self.totals.mesh_sub_meshes,
            TotalKind::MaterialEntries => &mut self.totals.material_entries,
            TotalKind::AvatarElements => &mut self.totals.avatar_elements,
        };
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

    fn finish(self) -> ModelIr {
        ModelIr {
            coordinate_convention: ModelCoordinateConvention::UnitySource,
            nodes: self.nodes,
            roots: self.roots,
            meshes: self.meshes,
            materials: self.materials,
            avatars: self.avatars,
            node_index: self.node_index,
            mesh_index: self.mesh_index,
            material_index: self.material_index,
            avatar_index: self.avatar_index,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TotalKind {
    HierarchyEdges,
    Renderers,
    MaterialReferences,
    BoneReferences,
    MeshVertices,
    MeshIndices,
    MeshSubMeshes,
    MaterialEntries,
    AvatarElements,
}

fn try_resolve_typed_key(
    collection: &AssetCollection,
    source_file_index: usize,
    reference: ObjectReference,
    expected_class_ids: &[i32],
) -> Option<SceneObjectKey> {
    let target = resolve_object_reference(collection, source_file_index, reference)
        .ok()
        .flatten()?;
    expected_class_ids
        .contains(&target.object.class_id)
        .then_some(SceneObjectKey {
            file_index: target.file_index,
            path_id: target.object.path_id,
        })
}

fn require_key_target<'a>(
    collection: &'a AssetCollection,
    key: SceneObjectKey,
    expected_class_id: i32,
    field: &str,
) -> Result<(&'a crate::serialized::SerializedFile, usize)> {
    let loaded = collection
        .serialized_files
        .get(key.file_index)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "{field} file index {} is outside the asset collection",
                key.file_index
            ))
        })?;
    let object_index = loaded
        .file
        .objects
        .iter()
        .position(|object| object.path_id == key.path_id)
        .ok_or_else(|| {
            Error::invalid_data(format!(
                "{field} path ID {} is absent from file {:?}",
                key.path_id, loaded.path
            ))
        })?;
    let class_id = loaded.file.objects[object_index].class_id;
    if class_id != expected_class_id {
        return Err(Error::invalid_data(format!(
            "{field} {key:?} has class ID {class_id}, expected {expected_class_id}"
        )));
    }
    Ok((&loaded.file, object_index))
}

fn material_entry_count(material: &Material) -> Result<usize> {
    [
        material.legacy_shader_keywords.len(),
        material.valid_keywords.len(),
        material.invalid_keywords.len(),
        material.string_tags.len(),
        material.disabled_shader_passes.len(),
        material.saved_properties.texture_environments.len(),
        material.saved_properties.integers.len(),
        material.saved_properties.floats.len(),
        material.saved_properties.colors.len(),
    ]
    .into_iter()
    .try_fold(0_usize, |total, count| {
        total
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("Material entry count overflowed"))
    })
}

fn material_string_bytes(material: &Material) -> Result<usize> {
    let mut total = material.name.len();
    for value in material
        .legacy_shader_keywords
        .iter()
        .chain(&material.valid_keywords)
        .chain(&material.invalid_keywords)
        .chain(&material.disabled_shader_passes)
    {
        total = checked_add_len(total, value.len(), "Material strings")?;
    }
    for (key, value) in &material.string_tags {
        total = checked_add_len(total, key.len(), "Material strings")?;
        total = checked_add_len(total, value.len(), "Material strings")?;
    }
    for name in material
        .saved_properties
        .texture_environments
        .iter()
        .map(|entry| &entry.name)
        .chain(
            material
                .saved_properties
                .integers
                .iter()
                .map(|entry| &entry.name),
        )
        .chain(
            material
                .saved_properties
                .floats
                .iter()
                .map(|entry| &entry.name),
        )
        .chain(
            material
                .saved_properties
                .colors
                .iter()
                .map(|entry| &entry.name),
        )
    {
        total = checked_add_len(total, name.len(), "Material strings")?;
    }
    Ok(total)
}

fn avatar_string_bytes(avatar: &Avatar) -> Result<usize> {
    let mut total = avatar.name.len();
    for path in &avatar.paths {
        total = checked_add_len(total, path.path.len(), "Avatar strings")?;
    }
    if let Some(description) = &avatar.human_description {
        for bone in &description.human_bones {
            total = checked_add_len(total, bone.bone_name.len(), "Avatar strings")?;
            total = checked_add_len(total, bone.human_name.len(), "Avatar strings")?;
        }
        for bone in &description.skeleton_bones {
            total = checked_add_len(total, bone.name.len(), "Avatar strings")?;
            total = checked_add_len(total, bone.parent_name.len(), "Avatar strings")?;
        }
        total = checked_add_len(
            total,
            description.root_motion_bone_name.len(),
            "Avatar strings",
        )?;
    }
    Ok(total)
}

fn avatar_element_count(avatar: &Avatar) -> Result<usize> {
    let constant = &avatar.constant;
    let human = &constant.human;
    let counts = [
        constant.avatar_skeleton.nodes.len(),
        constant.avatar_skeleton.ids.count,
        constant.avatar_skeleton.axes.len(),
        constant.avatar_skeleton_pose.xforms.len(),
        constant.default_pose.xforms.len(),
        constant.skeleton_name_ids.count,
        human.skeleton.nodes.len(),
        human.skeleton.ids.count,
        human.skeleton.axes.len(),
        human.skeleton_pose.xforms.len(),
        human.left_hand.bone_indices.count,
        human.right_hand.bone_indices.count,
        human.handles.as_ref().map_or(0, Vec::len),
        human.colliders.as_ref().map_or(0, Vec::len),
        human.human_bone_indices.count,
        human.human_bone_masses.count,
        human
            .collider_indices
            .as_ref()
            .map_or(0, |values| values.count),
        constant.human_skeleton_indices.count,
        constant.human_skeleton_reverse_indices.count,
        constant.root_motion_skeleton.nodes.len(),
        constant.root_motion_skeleton.ids.count,
        constant.root_motion_skeleton.axes.len(),
        constant.root_motion_skeleton_pose.xforms.len(),
        constant.root_motion_skeleton_indices.count,
        avatar.paths.len(),
    ];
    let mut total = counts.into_iter().try_fold(0_usize, |total, count| {
        total
            .checked_add(count)
            .ok_or_else(|| Error::invalid_data("Avatar element count overflowed"))
    })?;
    if let Some(description) = &avatar.human_description {
        total = total
            .checked_add(description.human_bones.len())
            .and_then(|value| value.checked_add(description.skeleton_bones.len()))
            .ok_or_else(|| Error::invalid_data("Avatar element count overflowed"))?;
    }
    Ok(total)
}

fn checked_add_len(total: usize, additional: usize, field: &str) -> Result<usize> {
    total
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data(format!("{field} byte count overflowed")))
}

fn require_maximum(actual: usize, maximum: usize, field: &str) -> Result<()> {
    if actual > maximum {
        return Err(Error::invalid_data(format!(
            "{field} count {actual} exceeds limit {maximum}"
        )));
    }
    Ok(())
}

fn require_next(current: usize, maximum: usize, field: &str) -> Result<()> {
    if current >= maximum {
        return Err(Error::invalid_data(format!(
            "{field} exceed limit {maximum}"
        )));
    }
    Ok(())
}

fn reserve_vec<T>(capacity: usize, field: &str) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|error| {
        Error::invalid_data(format!("cannot allocate {capacity} {field}: {error}"))
    })?;
    Ok(values)
}

fn clone_slice<T: Copy>(values: &[T], field: &str) -> Result<Vec<T>> {
    let mut output = reserve_vec(values.len(), field)?;
    output.extend_from_slice(values);
    Ok(output)
}

fn clone_string(value: &str, field: &str) -> Result<String> {
    let mut output = String::new();
    output.try_reserve_exact(value.len()).map_err(|error| {
        Error::invalid_data(format!(
            "cannot allocate {} bytes for {field}: {error}",
            value.len()
        ))
    })?;
    output.push_str(value);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use crate::animation_component::ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID;
    use crate::animator_controller::ANIMATOR_CONTROLLER_CLASS_ID;
    use crate::avatar::AVATAR_CLASS_ID;
    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::material::MATERIAL_CLASS_ID;
    use crate::mesh::MESH_CLASS_ID;
    use crate::renderer::{MESH_RENDERER_CLASS_ID, SKINNED_MESH_RENDERER_CLASS_ID};
    use crate::scene::{
        ANIMATOR_CLASS_ID, GAME_OBJECT_CLASS_ID, MESH_FILTER_CLASS_ID, TRANSFORM_CLASS_ID,
    };
    use crate::scene_hierarchy::{SceneHierarchyLimits, SceneObjectKey, build_scene_hierarchy};
    use crate::serialized::{ObjectReference, SerializedFile};
    use crate::source::Region;

    use super::{
        ModelCoordinateConvention, ModelIrLimits, ModelRendererKind, build_model_ir,
        build_model_ir_for_game_object,
    };

    const NULL: ObjectReference = ObjectReference {
        file_id: 0,
        path_id: 0,
    };

    #[test]
    fn assembles_static_model_stably_and_preserves_unity_source_trs() {
        let collection = fixture(Corrupt::None, false);
        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();

        let model = build_model_ir(&collection, &hierarchy, ModelIrLimits::default()).unwrap();

        assert_eq!(
            model.coordinate_convention,
            ModelCoordinateConvention::UnitySource
        );
        assert_eq!(model.roots, [key(1)]);
        assert_eq!(model.nodes.len(), 2);
        let root = model.node(key(1)).unwrap();
        assert_eq!(root.name, "root");
        assert_eq!(root.children, [key(2)]);
        let transform = root.transform.unwrap();
        assert_float(transform.local_position.x, 2.0);
        assert_float(transform.local_position.y, 3.0);
        assert_float(transform.local_position.z, 4.0);
        assert_float(transform.local_rotation.x, 0.1);
        assert_float(transform.local_rotation.y, 0.2);
        assert_float(transform.local_rotation.z, 0.3);
        assert_float(transform.local_rotation.w, 0.4);

        assert_eq!(root.renderers.len(), 1);
        assert_eq!(root.renderers[0].mesh, Some(key(51)));
        assert_eq!(
            root.renderers[0].materials,
            [Some(key(61)), None, None, None]
        );
        assert!(matches!(
            root.renderers[0].kind,
            ModelRendererKind::MeshRenderer {
                mesh_filter: Some(SceneObjectKey { path_id: 21, .. })
            }
        ));
        let animator = root.animator.unwrap();
        assert_eq!(animator.avatar, Some(key(71)));
        assert_eq!(animator.controller, Some(key(81)));

        let child = model.node(key(2)).unwrap();
        assert_eq!(child.parent, Some(key(1)));
        let ModelRendererKind::SkinnedMeshRenderer { bones } = &child.renderers[0].kind else {
            panic!("child renderer should retain skinned metadata");
        };
        assert_eq!(bones, &[Some(key(11)), None]);
        assert_eq!(child.renderers[0].mesh, Some(key(51)));

        assert_eq!(model.meshes.len(), 1);
        assert_eq!(model.mesh(key(51)).unwrap().mesh.name, "tri");
        assert_eq!(model.mesh(key(51)).unwrap().mesh.vertices.len(), 3);
        assert_eq!(model.materials.len(), 1);
        assert_eq!(model.material(key(61)).unwrap().material.name, "mat");
        assert_eq!(model.avatars.len(), 1);
        let avatar = &model.avatar(key(71)).unwrap().avatar;
        assert_eq!(avatar.name, "avatar");
        assert_eq!(avatar.paths[0].path, "Root/Hips");
    }

    #[test]
    fn applies_try_get_semantics_and_keeps_resolved_targets_strict() {
        let missing = fixture(Corrupt::None, true);
        let hierarchy = build_scene_hierarchy(&missing, SceneHierarchyLimits::default()).unwrap();
        let model = build_model_ir(&missing, &hierarchy, ModelIrLimits::default()).unwrap();
        assert!(model.meshes.is_empty());
        assert!(model.materials.is_empty());
        assert!(model.avatars.is_empty());
        let animator = model.node(key(1)).unwrap().animator.unwrap();
        assert_eq!(animator.avatar, None);
        assert_eq!(animator.controller, None);

        for corrupt in [Corrupt::Mesh, Corrupt::Material, Corrupt::Avatar] {
            let collection = fixture(corrupt, false);
            let hierarchy =
                build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();
            assert!(
                build_model_ir(&collection, &hierarchy, ModelIrLimits::default()).is_err(),
                "resolved {corrupt:?} payload must stay strict"
            );
        }
    }

    #[test]
    fn resolves_model_assets_across_serialized_files() {
        let source_objects = vec![
            (
                GAME_OBJECT_CLASS_ID,
                1,
                game_object(
                    "external root",
                    &[
                        reference(0, 11),
                        reference(0, 21),
                        reference(0, 31),
                        reference(0, 41),
                    ],
                ),
            ),
            (
                TRANSFORM_CLASS_ID,
                11,
                transform(
                    reference(0, 1),
                    &[],
                    NULL,
                    [0.0, 0.0, 0.0, 1.0],
                    [0.0, 0.0, 0.0],
                ),
            ),
            (
                MESH_FILTER_CLASS_ID,
                21,
                mesh_filter(reference(0, 1), reference(1, 51)),
            ),
            (
                MESH_RENDERER_CLASS_ID,
                31,
                renderer(reference(0, 1), &[reference(1, 61)]),
            ),
            (
                ANIMATOR_CLASS_ID,
                41,
                animator(reference(0, 1), reference(1, 71), reference(1, 81)),
            ),
        ];
        let target_objects = vec![
            (MESH_CLASS_ID, 51, mesh_object()),
            (MATERIAL_CLASS_ID, 61, material_object()),
            (AVATAR_CLASS_ID, 71, avatar_object()),
            (ANIMATOR_CONTROLLER_CLASS_ID, 81, Vec::new()),
        ];
        let source = SerializedFile::open(Region::from_bytes(synthetic_v22(
            &source_objects,
            &["archive:/folder/TARGET.ASSETS"],
        )))
        .unwrap();
        let target =
            SerializedFile::open(Region::from_bytes(synthetic_v22(&target_objects, &[]))).unwrap();
        let collection = AssetCollection::from_loaded_parts(
            vec![
                LoadedSerializedFile {
                    path: "root/source.assets".to_owned(),
                    file: source,
                },
                LoadedSerializedFile {
                    path: "bundle::target.assets".to_owned(),
                    file: target,
                },
            ],
            Vec::new(),
        );
        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();

        let model = build_model_ir(&collection, &hierarchy, ModelIrLimits::default()).unwrap();

        let external = |path_id| SceneObjectKey {
            file_index: 1,
            path_id,
        };
        assert_eq!(model.meshes[0].object, external(51));
        assert_eq!(model.materials[0].object, external(61));
        assert_eq!(model.avatars[0].object, external(71));
        let animator = model.nodes[0].animator.unwrap();
        assert_eq!(animator.avatar, Some(external(71)));
        assert_eq!(animator.controller, Some(external(81)));
    }

    #[test]
    fn selects_one_game_object_subtree_with_transform_only_ancestors() {
        let collection = fixture(Corrupt::None, false);
        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();

        let model = build_model_ir_for_game_object(
            &collection,
            &hierarchy,
            key(2),
            ModelIrLimits::default(),
        )
        .unwrap();

        assert_eq!(model.roots, [key(1)]);
        assert_eq!(model.nodes.len(), 2);
        let ancestor = model.node(key(1)).unwrap();
        assert!(!ancestor.export_content);
        assert_eq!(ancestor.children, [key(2)]);
        assert!(ancestor.renderers.is_empty());
        assert!(ancestor.animator.is_none());
        let selected = model.node(key(2)).unwrap();
        assert!(selected.export_content);
        assert_eq!(selected.parent, Some(key(1)));
        assert_eq!(selected.renderers.len(), 1);
        assert_eq!(model.meshes.len(), 1);
        assert_eq!(model.materials.len(), 1);
        assert!(model.avatars.is_empty());

        assert!(
            build_model_ir_for_game_object(
                &collection,
                &hierarchy,
                key(999),
                ModelIrLimits::default(),
            )
            .is_err()
        );
        assert!(
            build_model_ir_for_game_object(
                &collection,
                &hierarchy,
                key(2),
                ModelIrLimits {
                    maximum_nodes: 1,
                    ..ModelIrLimits::default()
                },
            )
            .is_err()
        );
    }

    #[test]
    fn enforces_every_collection_wide_budget() {
        let collection = fixture(Corrupt::None, false);
        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();
        let defaults = ModelIrLimits::default();
        let cases = [
            ModelIrLimits {
                maximum_nodes: 1,
                ..defaults
            },
            ModelIrLimits {
                maximum_roots: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_hierarchy_edges: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_renderers: 1,
                ..defaults
            },
            ModelIrLimits {
                maximum_material_references: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_bone_references: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_meshes: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_materials: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_avatars: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_mesh_vertices: 2,
                ..defaults
            },
            ModelIrLimits {
                maximum_mesh_indices: 2,
                ..defaults
            },
            ModelIrLimits {
                maximum_mesh_sub_meshes: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_material_entries: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_avatar_elements: 0,
                ..defaults
            },
            ModelIrLimits {
                maximum_total_string_bytes: 1,
                ..defaults
            },
        ];
        for limits in cases {
            assert!(build_model_ir(&collection, &hierarchy, limits).is_err());
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Corrupt {
        None,
        Mesh,
        Material,
        Avatar,
    }

    #[allow(clippy::too_many_lines)]
    fn fixture(corrupt: Corrupt, missing: bool) -> AssetCollection {
        let mesh_reference = if missing {
            reference(0, 999)
        } else {
            reference(0, 51)
        };
        let material_references = if missing {
            vec![reference(0, 51), reference(-1, 61)]
        } else {
            vec![reference(0, 61), NULL, reference(0, 999), reference(0, 51)]
        };
        let avatar_reference = if missing {
            reference(0, 999)
        } else {
            reference(0, 71)
        };
        let controller_reference = if missing {
            reference(0, 61)
        } else {
            reference(0, 81)
        };
        let mut objects = vec![
            (
                GAME_OBJECT_CLASS_ID,
                1,
                game_object(
                    "root",
                    &[
                        reference(0, 11),
                        reference(0, 21),
                        reference(0, 31),
                        reference(0, 41),
                    ],
                ),
            ),
            (
                GAME_OBJECT_CLASS_ID,
                2,
                game_object("child", &[reference(0, 12), reference(0, 32)]),
            ),
            (
                TRANSFORM_CLASS_ID,
                11,
                transform(
                    reference(0, 1),
                    &[reference(0, 12)],
                    NULL,
                    [0.1, 0.2, 0.3, 0.4],
                    [2.0, 3.0, 4.0],
                ),
            ),
            (
                TRANSFORM_CLASS_ID,
                12,
                transform(
                    reference(0, 2),
                    &[],
                    reference(0, 11),
                    [0.0, 0.0, 0.0, 1.0],
                    [0.0, 0.0, 0.0],
                ),
            ),
            (
                MESH_FILTER_CLASS_ID,
                21,
                mesh_filter(reference(0, 1), mesh_reference),
            ),
            (
                MESH_RENDERER_CLASS_ID,
                31,
                renderer(reference(0, 1), &material_references),
            ),
            (
                SKINNED_MESH_RENDERER_CLASS_ID,
                32,
                skinned_renderer(
                    reference(0, 2),
                    &material_references[..1],
                    mesh_reference,
                    &[reference(0, 11), reference(0, 999)],
                ),
            ),
            (
                ANIMATOR_CLASS_ID,
                41,
                animator(reference(0, 1), avatar_reference, controller_reference),
            ),
            (
                MESH_CLASS_ID,
                51,
                if corrupt == Corrupt::Mesh {
                    vec![0_u8; 4]
                } else {
                    mesh_object()
                },
            ),
            (
                MATERIAL_CLASS_ID,
                61,
                if corrupt == Corrupt::Material {
                    vec![0_u8; 4]
                } else {
                    material_object()
                },
            ),
            (
                AVATAR_CLASS_ID,
                71,
                if corrupt == Corrupt::Avatar {
                    vec![0_u8; 4]
                } else {
                    avatar_object()
                },
            ),
            (ANIMATOR_CONTROLLER_CLASS_ID, 81, Vec::new()),
            (ANIMATOR_OVERRIDE_CONTROLLER_CLASS_ID, 82, Vec::new()),
        ];
        objects.sort_unstable_by_key(|object| object.1);
        let file = SerializedFile::open(Region::from_bytes(synthetic_v22(&objects, &[]))).unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "model.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    const fn key(path_id: i64) -> SceneObjectKey {
        SceneObjectKey {
            file_index: 0,
            path_id,
        }
    }

    const fn reference(file_id: i32, path_id: i64) -> ObjectReference {
        ObjectReference { file_id, path_id }
    }

    fn assert_float(actual: f32, expected: f32) {
        assert!((actual - expected).abs() <= f32::EPSILON);
    }

    fn game_object(name: &str, components: &[ObjectReference]) -> Vec<u8> {
        let mut output = Vec::new();
        push_i32(&mut output, i32::try_from(components.len()).unwrap());
        for component in components {
            push_pptr(&mut output, *component);
        }
        push_i32(&mut output, 0);
        push_aligned_string(&mut output, name);
        output
    }

    fn transform(
        game_object: ObjectReference,
        children: &[ObjectReference],
        father: ObjectReference,
        rotation: [f32; 4],
        position: [f32; 3],
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, game_object);
        push_f32s(&mut output, &rotation);
        push_f32s(&mut output, &position);
        push_f32s(&mut output, &[1.0, 1.0, 1.0]);
        push_i32(&mut output, i32::try_from(children.len()).unwrap());
        for child in children {
            push_pptr(&mut output, *child);
        }
        push_pptr(&mut output, father);
        output
    }

    fn mesh_filter(game_object: ObjectReference, mesh: ObjectReference) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, game_object);
        push_pptr(&mut output, mesh);
        output
    }

    fn renderer(game_object: ObjectReference, materials: &[ObjectReference]) -> Vec<u8> {
        let mut output = renderer_prefix(game_object, materials);
        align(&mut output, 4);
        output
    }

    fn skinned_renderer(
        game_object: ObjectReference,
        materials: &[ObjectReference],
        mesh: ObjectReference,
        bones: &[ObjectReference],
    ) -> Vec<u8> {
        let mut output = renderer_prefix(game_object, materials);
        push_i32(&mut output, 0);
        output.extend_from_slice(&[0, 0]);
        align(&mut output, 4);
        push_pptr(&mut output, mesh);
        push_i32(&mut output, i32::try_from(bones.len()).unwrap());
        for bone in bones {
            push_pptr(&mut output, *bone);
        }
        push_i32(&mut output, 0);
        output
    }

    fn renderer_prefix(game_object: ObjectReference, materials: &[ObjectReference]) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, game_object);
        output.extend_from_slice(&[1, 2, 1, 0, 0, 0, 0, 0, 0, 0]);
        align(&mut output, 4);
        output.extend_from_slice(&u32::MAX.to_le_bytes());
        push_i32(&mut output, 0);
        output.extend_from_slice(&[0_u8; 36]);
        push_i32(&mut output, i32::try_from(materials.len()).unwrap());
        for material in materials {
            push_pptr(&mut output, *material);
        }
        output.extend_from_slice(&[0_u8; 4]);
        for _ in 0..3 {
            push_pptr(&mut output, NULL);
        }
        output.extend_from_slice(&[0_u8; 8]);
        align(&mut output, 4);
        output
    }

    fn animator(
        game_object: ObjectReference,
        avatar: ObjectReference,
        controller: ObjectReference,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, game_object);
        output.push(1);
        align(&mut output, 4);
        push_pptr(&mut output, avatar);
        push_pptr(&mut output, controller);
        output
    }

    #[allow(clippy::too_many_lines)]
    fn mesh_object() -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "tri");
        push_i32(&mut output, 1);
        push_u32(&mut output, 0);
        push_u32(&mut output, 3);
        push_i32(&mut output, 0);
        push_u32(&mut output, 0);
        push_u32(&mut output, 0);
        push_u32(&mut output, 3);
        output.extend_from_slice(&[0_u8; 24]);
        for _ in 0..3 {
            push_i32(&mut output, 0);
        }
        push_u32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_u32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        output.extend_from_slice(&[0, 1, 0, 0]);
        align(&mut output, 4);
        push_i32(&mut output, 0);
        push_i32(&mut output, 6);
        for index in 0..3_u16 {
            output.extend_from_slice(&index.to_le_bytes());
        }
        align(&mut output, 4);
        push_u32(&mut output, 3);
        push_i32(&mut output, 5);
        output.extend_from_slice(&[0, 0, 0, 3]);
        for _ in 0..4 {
            output.extend_from_slice(&[0, 0, 0, 0]);
        }
        let vertices = [
            [0.0_f32, 0.0, 0.0],
            [1.0_f32, 0.0, 0.0],
            [0.0_f32, 1.0, 0.0],
        ];
        push_i32(&mut output, 36);
        for vertex in vertices {
            push_f32s(&mut output, &vertex);
        }
        align(&mut output, 4);
        for _ in 0..4 {
            push_empty_packed_float(&mut output);
        }
        for _ in 0..3 {
            push_empty_packed_int(&mut output);
        }
        push_empty_packed_float(&mut output);
        for _ in 0..2 {
            push_empty_packed_int(&mut output);
        }
        push_u32(&mut output, 0);
        output.extend_from_slice(&[0_u8; 24]);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        align(&mut output, 4);
        push_i32(&mut output, 0);
        align(&mut output, 4);
        output.extend_from_slice(&[0_u8; 8]);
        align(&mut output, 4);
        output.extend_from_slice(&0_i64.to_le_bytes());
        push_u32(&mut output, 0);
        push_aligned_string(&mut output, "");
        output
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

    fn material_object() -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "mat");
        push_pptr(&mut output, NULL);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_u32(&mut output, 0);
        output.push(0);
        align(&mut output, 4);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_i32(&mut output, 1);
        push_aligned_string(&mut output, "_Gloss");
        push_f32s(&mut output, &[0.5]);
        push_i32(&mut output, 0);
        output
    }

    fn avatar_object() -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "avatar");
        push_u32(&mut output, 0);
        push_skeleton(&mut output);
        push_pose(&mut output);
        push_pose(&mut output);
        push_u32_array(&mut output);
        push_xform(&mut output);
        push_skeleton(&mut output);
        push_pose(&mut output);
        push_i32_array(&mut output);
        push_i32_array(&mut output);
        push_i32_array(&mut output);
        push_f32_array(&mut output);
        push_f32s(&mut output, &[1.0, 0.5, 0.5, 0.5, 0.5, 0.05, 0.05, 0.0]);
        output.extend_from_slice(&[0, 0, 0]);
        align(&mut output, 4);
        push_i32_array(&mut output);
        push_i32_array(&mut output);
        push_i32(&mut output, -1);
        push_xform(&mut output);
        push_skeleton(&mut output);
        push_pose(&mut output);
        push_i32_array(&mut output);
        push_i32(&mut output, 1);
        push_u32(&mut output, 123);
        push_aligned_string(&mut output, "Root/Hips");
        push_i32(&mut output, 0);
        push_i32(&mut output, 0);
        push_f32s(&mut output, &[0.0; 8]);
        push_aligned_string(&mut output, "");
        output.extend_from_slice(&[0, 0, 0]);
        align(&mut output, 4);
        output
    }

    fn push_skeleton(output: &mut Vec<u8>) {
        push_i32(output, 0);
        push_u32_array(output);
        push_i32(output, 0);
    }

    fn push_pose(output: &mut Vec<u8>) {
        push_i32(output, 0);
    }

    fn push_xform(output: &mut Vec<u8>) {
        push_f32s(output, &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
    }

    fn push_i32_array(output: &mut Vec<u8>) {
        push_i32(output, 0);
    }

    fn push_u32_array(output: &mut Vec<u8>) {
        push_i32(output, 0);
    }

    fn push_f32_array(output: &mut Vec<u8>) {
        push_i32(output, 0);
    }

    fn synthetic_v22(objects: &[(i32, i64, Vec<u8>)], externals: &[&str]) -> Vec<u8> {
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
        push_i32(&mut metadata, i32::try_from(externals.len()).unwrap());
        for external in externals {
            metadata.push(0);
            metadata.extend_from_slice(&[0_u8; 16]);
            push_i32(&mut metadata, 0);
            metadata.extend_from_slice(external.as_bytes());
            metadata.push(0);
        }
        push_i32(&mut metadata, 0);
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let data_offset = (48_u64 + u64::from(metadata_size)).next_multiple_of(16);
        let file_size = data_offset + u64::try_from(data.len()).unwrap();
        let mut output = vec![0_u8; 48];
        output[8..12].copy_from_slice(&22_u32.to_be_bytes());
        output[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        output.extend_from_slice(&metadata);
        output.resize(usize::try_from(data_offset).unwrap(), 0);
        output.extend_from_slice(&data);
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
}
