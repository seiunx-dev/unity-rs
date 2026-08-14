//! Collection-wide assembly of Unity's `GameObject`/component hierarchy.
//!
//! This layer connects the bounded object readers without decoding Mesh data or
//! emitting a model format. Component ordering follows the managed loader: the
//! last component of a supported kind attached to a `GameObject` wins. Parent
//! ownership follows `Transform.m_GameObject`, and cycles are rejected before
//! callers recurse the tree.

use std::collections::BTreeMap;

use crate::loader::AssetCollection;
use crate::material::MATERIAL_CLASS_ID;
use crate::mesh::MESH_CLASS_ID;
use crate::renderer::{
    MESH_RENDERER_CLASS_ID, RendererReadLimits, SKINNED_MESH_RENDERER_CLASS_ID, read_mesh_renderer,
    read_skinned_mesh_renderer,
};
use crate::scene::{
    ANIMATOR_CLASS_ID, GAME_OBJECT_CLASS_ID, MESH_FILTER_CLASS_ID, Quaternion,
    RECT_TRANSFORM_CLASS_ID, ResolvedObject, SceneReadLimits, TRANSFORM_CLASS_ID, Vector3,
    read_animator, read_game_object, read_mesh_filter, read_transform, resolve_object_reference,
};
use crate::serialized::ObjectReference;
use crate::{Error, Result};

/// Stable identity for one object inside an `AssetCollection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SceneObjectKey {
    pub file_index: usize,
    pub path_id: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransformBinding {
    pub component: SceneObjectKey,
    pub local_rotation: Quaternion,
    pub local_position: Vector3,
    pub local_scale: Vector3,
    pub parent_transform: Option<SceneObjectKey>,
    pub declared_children: Vec<Option<SceneObjectKey>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshFilterBinding {
    pub component: SceneObjectKey,
    pub mesh: Option<SceneObjectKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererBinding {
    pub component: SceneObjectKey,
    pub materials: Vec<Option<SceneObjectKey>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinnedRendererBinding {
    pub component: SceneObjectKey,
    pub mesh: Option<SceneObjectKey>,
    pub materials: Vec<Option<SceneObjectKey>>,
    pub bones: Vec<Option<SceneObjectKey>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimatorBinding {
    pub component: SceneObjectKey,
    pub avatar: ObjectReference,
    pub controller: ObjectReference,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneHierarchyNode {
    pub object: SceneObjectKey,
    pub name: String,
    pub transform: Option<TransformBinding>,
    pub mesh_filter: Option<MeshFilterBinding>,
    pub mesh_renderer: Option<RendererBinding>,
    pub skinned_mesh_renderer: Option<SkinnedRendererBinding>,
    pub animator: Option<AnimatorBinding>,
    pub parent: Option<SceneObjectKey>,
    pub children: Vec<SceneObjectKey>,
}

#[derive(Debug, Clone)]
pub struct SceneHierarchy {
    pub nodes: Vec<SceneHierarchyNode>,
    pub roots: Vec<SceneObjectKey>,
    index_by_object: BTreeMap<SceneObjectKey, usize>,
}

impl SceneHierarchy {
    #[must_use]
    pub fn node(&self, key: SceneObjectKey) -> Option<&SceneHierarchyNode> {
        self.index_by_object
            .get(&key)
            .and_then(|index| self.nodes.get(*index))
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts(
        nodes: Vec<SceneHierarchyNode>,
        roots: Vec<SceneObjectKey>,
    ) -> Self {
        let index_by_object = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.object, index))
            .collect();
        Self {
            nodes,
            roots,
            index_by_object,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneHierarchyLimits {
    pub maximum_game_objects: usize,
    pub maximum_total_components: usize,
    pub maximum_total_transform_child_references: usize,
    pub maximum_total_material_references: usize,
    pub maximum_total_bone_references: usize,
    pub maximum_hierarchy_edges: usize,
    pub scene: SceneReadLimits,
    pub renderer: RendererReadLimits,
}

impl Default for SceneHierarchyLimits {
    fn default() -> Self {
        Self {
            maximum_game_objects: 1_000_000,
            maximum_total_components: 10_000_000,
            maximum_total_transform_child_references: 10_000_000,
            maximum_total_material_references: 10_000_000,
            maximum_total_bone_references: 10_000_000,
            maximum_hierarchy_edges: 1_000_000,
            scene: SceneReadLimits::default(),
            renderer: RendererReadLimits::default(),
        }
    }
}

/// Builds the model-relevant hierarchy for standard Unity 2017.3-6000.2 files.
///
/// Unsupported component classes and unresolved or wrongly typed pointers are
/// ignored, matching managed `PPtr.TryGet`. Once a supported component target
/// is found, a corrupt payload or renderer version outside the verified range
/// remains an error rather than being silently detached.
pub fn build_scene_hierarchy(
    collection: &AssetCollection,
    limits: SceneHierarchyLimits,
) -> Result<SceneHierarchy> {
    let mut nodes = Vec::new();
    let mut state = SceneBuildState::default();

    for (file_index, loaded) in collection.serialized_files.iter().enumerate() {
        for (object_index, object) in loaded.file.objects.iter().enumerate() {
            if object.class_id != GAME_OBJECT_CLASS_ID {
                continue;
            }
            if nodes.len() >= limits.maximum_game_objects {
                return Err(Error::invalid_data(format!(
                    "scene hierarchy exceeds {} GameObjects",
                    limits.maximum_game_objects
                )));
            }
            let node = read_scene_node(collection, file_index, object_index, limits, &mut state)?;
            nodes.try_reserve(1).map_err(|error| {
                Error::invalid_data(format!("cannot grow scene GameObjects: {error}"))
            })?;
            nodes.push(node);
        }
    }
    finish_hierarchy(nodes, limits.maximum_hierarchy_edges)
}

#[derive(Default)]
struct SceneBuildState {
    total_components: usize,
    total_transform_children: usize,
    total_materials: usize,
    total_bones: usize,
    transform_owners: BTreeMap<SceneObjectKey, Option<SceneObjectKey>>,
}

fn read_scene_node(
    collection: &AssetCollection,
    file_index: usize,
    object_index: usize,
    limits: SceneHierarchyLimits,
    state: &mut SceneBuildState,
) -> Result<SceneHierarchyNode> {
    let loaded = &collection.serialized_files[file_index];
    let game_object = read_game_object(&loaded.file, object_index, limits.scene)?;
    state.total_components = charge_total(
        state.total_components,
        game_object.components.len(),
        limits.maximum_total_components,
        "scene components",
    )?;
    let mut node = SceneHierarchyNode {
        object: SceneObjectKey {
            file_index,
            path_id: game_object.path_id,
        },
        name: game_object.name,
        transform: None,
        mesh_filter: None,
        mesh_renderer: None,
        skinned_mesh_renderer: None,
        animator: None,
        parent: None,
        children: Vec::new(),
    };
    for component in game_object.components {
        bind_component(
            collection,
            file_index,
            component.component,
            limits,
            state,
            &mut node,
        )?;
    }
    Ok(node)
}

fn bind_component(
    collection: &AssetCollection,
    source_file_index: usize,
    reference: ObjectReference,
    limits: SceneHierarchyLimits,
    state: &mut SceneBuildState,
    node: &mut SceneHierarchyNode,
) -> Result<()> {
    let Some(target) = try_resolve_target(collection, source_file_index, reference) else {
        return Ok(());
    };
    let key = object_key(&target);
    match target.object.class_id {
        TRANSFORM_CLASS_ID | RECT_TRANSFORM_CLASS_ID => {
            bind_transform(collection, &target, key, limits, state, node)?;
        }
        MESH_FILTER_CLASS_ID => {
            let filter = read_mesh_filter(target.file, target.object_index, limits.scene)?;
            node.mesh_filter = Some(MeshFilterBinding {
                component: key,
                mesh: try_resolve_typed_key(
                    collection,
                    target.file_index,
                    filter.mesh,
                    &[MESH_CLASS_ID],
                ),
            });
        }
        MESH_RENDERER_CLASS_ID => {
            bind_mesh_renderer(collection, &target, key, limits, state, node)?;
        }
        SKINNED_MESH_RENDERER_CLASS_ID => {
            bind_skinned_renderer(collection, &target, key, limits, state, node)?;
        }
        ANIMATOR_CLASS_ID => {
            let animator = read_animator(target.file, target.object_index, limits.scene)?;
            node.animator = Some(AnimatorBinding {
                component: key,
                avatar: animator.avatar,
                controller: animator.controller,
            });
        }
        _ => {}
    }
    Ok(())
}

fn bind_transform(
    collection: &AssetCollection,
    target: &ResolvedObject<'_>,
    key: SceneObjectKey,
    limits: SceneHierarchyLimits,
    state: &mut SceneBuildState,
    node: &mut SceneHierarchyNode,
) -> Result<()> {
    let transform = read_transform(target.file, target.object_index, limits.scene)?;
    if !state.transform_owners.contains_key(&key) {
        state.total_transform_children = charge_total(
            state.total_transform_children,
            transform.children.len(),
            limits.maximum_total_transform_child_references,
            "scene Transform child references",
        )?;
    }
    let owner = try_resolve_typed_key(
        collection,
        target.file_index,
        transform.component.game_object,
        &[GAME_OBJECT_CLASS_ID],
    );
    state.transform_owners.insert(key, owner);
    let parent_transform =
        try_resolve_transform_key(collection, target.file_index, transform.father);
    node.parent = resolve_transform_owner(collection, parent_transform, limits, state)?;
    let mut declared_children =
        reserve_vec(transform.children.len(), "Transform declared children")?;
    for child in transform.children {
        declared_children.push(try_resolve_transform_key(
            collection,
            target.file_index,
            child,
        ));
    }
    node.transform = Some(TransformBinding {
        component: key,
        local_rotation: transform.local_rotation,
        local_position: transform.local_position,
        local_scale: transform.local_scale,
        parent_transform,
        declared_children,
    });
    Ok(())
}

fn bind_mesh_renderer(
    collection: &AssetCollection,
    target: &ResolvedObject<'_>,
    key: SceneObjectKey,
    limits: SceneHierarchyLimits,
    state: &mut SceneBuildState,
    node: &mut SceneHierarchyNode,
) -> Result<()> {
    let renderer = read_mesh_renderer(target.file, target.object_index, limits.renderer)?;
    state.total_materials = charge_total(
        state.total_materials,
        renderer.renderer.materials.len(),
        limits.maximum_total_material_references,
        "scene material references",
    )?;
    node.mesh_renderer = Some(RendererBinding {
        component: key,
        materials: resolve_materials(collection, target.file_index, renderer.renderer.materials)?,
    });
    Ok(())
}

fn bind_skinned_renderer(
    collection: &AssetCollection,
    target: &ResolvedObject<'_>,
    key: SceneObjectKey,
    limits: SceneHierarchyLimits,
    state: &mut SceneBuildState,
    node: &mut SceneHierarchyNode,
) -> Result<()> {
    let renderer = read_skinned_mesh_renderer(target.file, target.object_index, limits.renderer)?;
    state.total_materials = charge_total(
        state.total_materials,
        renderer.renderer.materials.len(),
        limits.maximum_total_material_references,
        "scene material references",
    )?;
    state.total_bones = charge_total(
        state.total_bones,
        renderer.bones.len(),
        limits.maximum_total_bone_references,
        "scene bone references",
    )?;
    let mut bones = reserve_vec(renderer.bones.len(), "scene bones")?;
    for bone in renderer.bones {
        bones.push(try_resolve_transform_key(
            collection,
            target.file_index,
            bone,
        ));
    }
    node.skinned_mesh_renderer = Some(SkinnedRendererBinding {
        component: key,
        mesh: try_resolve_typed_key(
            collection,
            target.file_index,
            renderer.mesh,
            &[MESH_CLASS_ID],
        ),
        materials: resolve_materials(collection, target.file_index, renderer.renderer.materials)?,
        bones,
    });
    Ok(())
}

fn finish_hierarchy(
    mut nodes: Vec<SceneHierarchyNode>,
    maximum_hierarchy_edges: usize,
) -> Result<SceneHierarchy> {
    let mut index_by_object = BTreeMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if index_by_object.insert(node.object, index).is_some() {
            return Err(Error::invalid_data(format!(
                "duplicate GameObject identity {:?}",
                node.object
            )));
        }
    }

    let mut parents = Vec::new();
    parents
        .try_reserve_exact(nodes.len())
        .map_err(|error| Error::invalid_data(format!("cannot allocate scene parents: {error}")))?;
    for node in &nodes {
        let parent = node
            .parent
            .map(|parent_object| {
                index_by_object.get(&parent_object).copied().ok_or_else(|| {
                    Error::invalid_data(format!(
                        "parent GameObject {parent_object:?} is absent from the loaded hierarchy"
                    ))
                })
            })
            .transpose()?;
        parents.push(parent);
    }
    reject_parent_cycles(&parents)?;

    let edge_count = parents.iter().filter(|parent| parent.is_some()).count();
    if edge_count > maximum_hierarchy_edges {
        return Err(Error::invalid_data(format!(
            "scene hierarchy has {edge_count} edges, exceeding limit {maximum_hierarchy_edges}"
        )));
    }
    let mut roots = reserve_vec(nodes.len() - edge_count, "scene roots")?;
    for (index, parent_index) in parents.into_iter().enumerate() {
        if let Some(parent_index) = parent_index {
            let child_key = nodes[index].object;
            let parent_key = nodes[parent_index].object;
            nodes[index].parent = Some(parent_key);
            nodes[parent_index]
                .children
                .try_reserve(1)
                .map_err(|error| {
                    Error::invalid_data(format!("cannot grow scene child list: {error}"))
                })?;
            nodes[parent_index].children.push(child_key);
        } else {
            roots.push(nodes[index].object);
        }
    }
    Ok(SceneHierarchy {
        nodes,
        roots,
        index_by_object,
    })
}

fn reject_parent_cycles(parents: &[Option<usize>]) -> Result<()> {
    let mut state = reserve_vec(parents.len(), "scene cycle states")?;
    state.resize(parents.len(), 0_u8);
    let mut path = Vec::new();
    for start in 0..parents.len() {
        if state[start] != 0 {
            continue;
        }
        path.clear();
        let mut current = Some(start);
        while let Some(index) = current {
            match state[index] {
                0 => {
                    state[index] = 1;
                    path.try_reserve(1).map_err(|error| {
                        Error::invalid_data(format!("cannot grow scene cycle stack: {error}"))
                    })?;
                    path.push(index);
                    current = parents[index];
                }
                1 => {
                    return Err(Error::invalid_data(format!(
                        "scene Transform parent cycle reaches GameObject index {index}"
                    )));
                }
                _ => break,
            }
        }
        for index in path.drain(..) {
            state[index] = 2;
        }
    }
    Ok(())
}

fn resolve_materials(
    collection: &AssetCollection,
    source_file_index: usize,
    references: Vec<ObjectReference>,
) -> Result<Vec<Option<SceneObjectKey>>> {
    let mut materials = reserve_vec(references.len(), "scene materials")?;
    for reference in references {
        materials.push(try_resolve_typed_key(
            collection,
            source_file_index,
            reference,
            &[MATERIAL_CLASS_ID],
        ));
    }
    Ok(materials)
}

fn resolve_transform_owner(
    collection: &AssetCollection,
    transform_key: Option<SceneObjectKey>,
    limits: SceneHierarchyLimits,
    state: &mut SceneBuildState,
) -> Result<Option<SceneObjectKey>> {
    let Some(transform_key) = transform_key else {
        return Ok(None);
    };
    if let Some(owner) = state.transform_owners.get(&transform_key) {
        return Ok(*owner);
    }
    let Some(target) = try_resolve_target(
        collection,
        transform_key.file_index,
        ObjectReference {
            file_id: 0,
            path_id: transform_key.path_id,
        },
    ) else {
        return Ok(None);
    };
    let transform = read_transform(target.file, target.object_index, limits.scene)?;
    state.total_transform_children = charge_total(
        state.total_transform_children,
        transform.children.len(),
        limits.maximum_total_transform_child_references,
        "scene Transform child references",
    )?;
    let owner = try_resolve_typed_key(
        collection,
        target.file_index,
        transform.component.game_object,
        &[GAME_OBJECT_CLASS_ID],
    );
    state.transform_owners.insert(transform_key, owner);
    Ok(owner)
}

fn try_resolve_transform_key(
    collection: &AssetCollection,
    source_file_index: usize,
    reference: ObjectReference,
) -> Option<SceneObjectKey> {
    try_resolve_typed_key(
        collection,
        source_file_index,
        reference,
        &[TRANSFORM_CLASS_ID, RECT_TRANSFORM_CLASS_ID],
    )
}

fn try_resolve_typed_key(
    collection: &AssetCollection,
    source_file_index: usize,
    reference: ObjectReference,
    expected_class_ids: &[i32],
) -> Option<SceneObjectKey> {
    let target = try_resolve_target(collection, source_file_index, reference)?;
    expected_class_ids
        .contains(&target.object.class_id)
        .then(|| object_key(&target))
}

fn try_resolve_target(
    collection: &AssetCollection,
    source_file_index: usize,
    reference: ObjectReference,
) -> Option<ResolvedObject<'_>> {
    resolve_object_reference(collection, source_file_index, reference)
        .ok()
        .flatten()
}

const fn object_key(target: &ResolvedObject<'_>) -> SceneObjectKey {
    SceneObjectKey {
        file_index: target.file_index,
        path_id: target.object.path_id,
    }
}

fn charge_total(current: usize, additional: usize, maximum: usize, field: &str) -> Result<usize> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data(format!("{field} count overflowed")))?;
    if total > maximum {
        return Err(Error::invalid_data(format!(
            "{field} total {total} exceeds limit {maximum}"
        )));
    }
    Ok(total)
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
    use crate::material::MATERIAL_CLASS_ID;
    use crate::mesh::MESH_CLASS_ID;
    use crate::renderer::{MESH_RENDERER_CLASS_ID, SKINNED_MESH_RENDERER_CLASS_ID};
    use crate::scene::{GAME_OBJECT_CLASS_ID, MESH_FILTER_CLASS_ID, TRANSFORM_CLASS_ID};
    use crate::serialized::{ObjectReference, SerializedFile};
    use crate::source::Region;

    use super::{SceneHierarchyLimits, SceneObjectKey, build_scene_hierarchy};

    const NULL: ObjectReference = ObjectReference {
        file_id: 0,
        path_id: 0,
    };

    #[test]
    fn assembles_v22_model_bindings_with_last_wins_and_try_get_semantics() {
        let collection = model_fixture();

        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();
        let child_key = key(0, 1);
        let parent_key = key(0, 2);
        let bone_object_key = key(0, 3);
        let child = hierarchy.node(child_key).unwrap();

        assert_eq!(hierarchy.nodes.len(), 3);
        assert_eq!(hierarchy.roots, [parent_key, bone_object_key]);
        assert_eq!(child.name, "child");
        assert_eq!(child.parent, Some(parent_key));
        assert_eq!(child.transform.as_ref().unwrap().component, key(0, 11));
        assert_eq!(
            child.transform.as_ref().unwrap().parent_transform,
            Some(key(0, 12))
        );
        assert_eq!(child.mesh_filter.as_ref().unwrap().component, key(0, 22));
        assert_eq!(child.mesh_filter.as_ref().unwrap().mesh, Some(key(0, 52)));
        assert_eq!(child.mesh_renderer.as_ref().unwrap().component, key(0, 32));
        assert_eq!(
            child.mesh_renderer.as_ref().unwrap().materials,
            [Some(key(0, 62)), None, None, None]
        );

        let skinned = child.skinned_mesh_renderer.as_ref().unwrap();
        assert_eq!(skinned.component, key(0, 41));
        assert_eq!(skinned.mesh, Some(key(0, 53)));
        assert_eq!(skinned.materials, [Some(key(0, 61)), None, None]);
        assert_eq!(skinned.bones, [Some(key(0, 13)), None, None, None]);
        assert_eq!(hierarchy.node(parent_key).unwrap().children, [child_key]);
        assert_eq!(
            hierarchy
                .node(parent_key)
                .unwrap()
                .transform
                .as_ref()
                .unwrap()
                .declared_children,
            [Some(key(0, 11)), None, None, None]
        );
    }

    #[test]
    fn resolves_cross_file_parent_ownership_and_mesh_targets() {
        let source = parse_asset(
            &[
                (
                    GAME_OBJECT_CLASS_ID,
                    1,
                    game_object("external-child", &[reference(0, 11), reference(0, 21)]),
                ),
                (
                    TRANSFORM_CLASS_ID,
                    11,
                    transform(reference(0, 1), &[], reference(1, 12)),
                ),
                (
                    MESH_FILTER_CLASS_ID,
                    21,
                    mesh_filter(reference(0, 1), reference(1, 51)),
                ),
            ],
            &["archive:/shared/PARENT.ASSETS"],
        );
        // The father Transform deliberately is not in the parent GameObject's
        // component list. Managed BuildTreeStructure follows father.m_GameObject.
        let target = parse_asset(
            &[
                (GAME_OBJECT_CLASS_ID, 2, game_object("parent", &[])),
                (
                    TRANSFORM_CLASS_ID,
                    12,
                    transform(reference(0, 2), &[reference(1, 999)], NULL),
                ),
                (MESH_CLASS_ID, 51, Vec::new()),
            ],
            &["missing.assets"],
        );
        let collection = collection(vec![
            ("root/source.assets", source),
            ("bundle::folder/parent.assets", target),
        ]);

        let hierarchy =
            build_scene_hierarchy(&collection, SceneHierarchyLimits::default()).unwrap();
        let child = hierarchy.node(key(0, 1)).unwrap();

        assert_eq!(child.parent, Some(key(1, 2)));
        assert_eq!(child.mesh_filter.as_ref().unwrap().mesh, Some(key(1, 51)));
        assert_eq!(hierarchy.roots, [key(1, 2)]);
        assert_eq!(hierarchy.node(key(1, 2)).unwrap().children, [key(0, 1)]);

        let hidden_parent_children_limit = SceneHierarchyLimits {
            maximum_total_transform_child_references: 0,
            ..SceneHierarchyLimits::default()
        };
        let error = build_scene_hierarchy(&collection, hidden_parent_children_limit).unwrap_err();
        assert!(error.to_string().contains("Transform child references"));
    }

    #[test]
    fn rejects_parent_cycles_and_corrupt_resolved_components() {
        let cycle = collection(vec![(
            "cycle.assets",
            parse_asset(
                &[
                    (
                        GAME_OBJECT_CLASS_ID,
                        1,
                        game_object("a", &[reference(0, 11)]),
                    ),
                    (
                        GAME_OBJECT_CLASS_ID,
                        2,
                        game_object("b", &[reference(0, 12)]),
                    ),
                    (
                        TRANSFORM_CLASS_ID,
                        11,
                        transform(reference(0, 1), &[], reference(0, 12)),
                    ),
                    (
                        TRANSFORM_CLASS_ID,
                        12,
                        transform(reference(0, 2), &[], reference(0, 11)),
                    ),
                ],
                &[],
            ),
        )]);
        let error = build_scene_hierarchy(&cycle, SceneHierarchyLimits::default()).unwrap_err();
        assert!(error.to_string().contains("cycle"));

        let corrupt = collection(vec![(
            "corrupt.assets",
            parse_asset(
                &[
                    (
                        GAME_OBJECT_CLASS_ID,
                        1,
                        game_object("broken", &[reference(0, 11)]),
                    ),
                    (TRANSFORM_CLASS_ID, 11, vec![0_u8; 4]),
                ],
                &[],
            ),
        )]);
        assert!(build_scene_hierarchy(&corrupt, SceneHierarchyLimits::default()).is_err());
    }

    #[test]
    fn enforces_collection_wide_scene_totals() {
        let collection = model_fixture();
        let exact_transform_child_limit = SceneHierarchyLimits {
            maximum_total_transform_child_references: 4,
            ..SceneHierarchyLimits::default()
        };
        assert!(build_scene_hierarchy(&collection, exact_transform_child_limit).is_ok());
        let short_transform_child_limit = SceneHierarchyLimits {
            maximum_total_transform_child_references: 3,
            ..SceneHierarchyLimits::default()
        };
        assert!(
            build_scene_hierarchy(&collection, short_transform_child_limit)
                .unwrap_err()
                .to_string()
                .contains("Transform child references")
        );
        let cases = [
            (
                SceneHierarchyLimits {
                    maximum_game_objects: 2,
                    ..SceneHierarchyLimits::default()
                },
                "GameObjects",
            ),
            (
                SceneHierarchyLimits {
                    maximum_total_components: 0,
                    ..SceneHierarchyLimits::default()
                },
                "scene components",
            ),
            (
                SceneHierarchyLimits {
                    maximum_total_transform_child_references: 0,
                    ..SceneHierarchyLimits::default()
                },
                "Transform child references",
            ),
            (
                SceneHierarchyLimits {
                    maximum_total_material_references: 0,
                    ..SceneHierarchyLimits::default()
                },
                "material references",
            ),
            (
                SceneHierarchyLimits {
                    maximum_total_bone_references: 0,
                    ..SceneHierarchyLimits::default()
                },
                "bone references",
            ),
            (
                SceneHierarchyLimits {
                    maximum_hierarchy_edges: 0,
                    ..SceneHierarchyLimits::default()
                },
                "edges",
            ),
        ];
        for (limits, expected) in cases {
            let error = build_scene_hierarchy(&collection, limits).unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} in {error}"
            );
        }
    }

    fn model_fixture() -> AssetCollection {
        let ignored_component_references = [
            reference(0, 999),
            reference(0, 51),
            reference(1, 11),
            ObjectReference {
                file_id: -1,
                path_id: 11,
            },
            NULL,
        ];
        let mut child_components = vec![
            reference(0, 15),
            reference(0, 11),
            reference(0, 21),
            reference(0, 22),
            reference(0, 31),
            reference(0, 32),
            reference(0, 41),
        ];
        child_components.extend_from_slice(&ignored_component_references);
        let objects = vec![
            (
                GAME_OBJECT_CLASS_ID,
                1,
                game_object("child", &child_components),
            ),
            (
                GAME_OBJECT_CLASS_ID,
                2,
                game_object("parent", &[reference(0, 12)]),
            ),
            (
                GAME_OBJECT_CLASS_ID,
                3,
                game_object("bone", &[reference(0, 13)]),
            ),
            (
                TRANSFORM_CLASS_ID,
                15,
                transform(reference(0, 1), &[], reference(0, 999)),
            ),
            (
                TRANSFORM_CLASS_ID,
                11,
                transform(reference(0, 1), &[], reference(0, 12)),
            ),
            (
                TRANSFORM_CLASS_ID,
                12,
                transform(
                    reference(0, 2),
                    &[reference(0, 11), NULL, reference(0, 999), reference(0, 52)],
                    NULL,
                ),
            ),
            (
                TRANSFORM_CLASS_ID,
                13,
                transform(reference(0, 3), &[], reference(0, 52)),
            ),
            (
                MESH_FILTER_CLASS_ID,
                21,
                mesh_filter(reference(0, 1), reference(0, 51)),
            ),
            (
                MESH_FILTER_CLASS_ID,
                22,
                mesh_filter(reference(0, 1), reference(0, 52)),
            ),
            (
                MESH_RENDERER_CLASS_ID,
                31,
                renderer(reference(0, 1), &[reference(0, 61)]),
            ),
            (
                MESH_RENDERER_CLASS_ID,
                32,
                renderer(
                    reference(0, 1),
                    &[reference(0, 62), NULL, reference(0, 999), reference(0, 52)],
                ),
            ),
            (
                SKINNED_MESH_RENDERER_CLASS_ID,
                41,
                skinned_renderer(
                    reference(0, 1),
                    &[reference(0, 61), reference(0, 52), NULL],
                    reference(0, 53),
                    &[reference(0, 13), reference(0, 999), reference(0, 2), NULL],
                ),
            ),
            (MESH_CLASS_ID, 51, Vec::new()),
            (MESH_CLASS_ID, 52, Vec::new()),
            (MESH_CLASS_ID, 53, Vec::new()),
            (MATERIAL_CLASS_ID, 61, Vec::new()),
            (MATERIAL_CLASS_ID, 62, Vec::new()),
        ];
        collection(vec![("model.assets", parse_asset(&objects, &[]))])
    }

    const fn key(file_index: usize, path_id: i64) -> SceneObjectKey {
        SceneObjectKey {
            file_index,
            path_id,
        }
    }

    const fn reference(file_id: i32, path_id: i64) -> ObjectReference {
        ObjectReference { file_id, path_id }
    }

    fn collection(files: Vec<(&str, SerializedFile)>) -> AssetCollection {
        AssetCollection::from_loaded_parts(
            files
                .into_iter()
                .map(|(path, file)| LoadedSerializedFile {
                    path: path.to_owned(),
                    file,
                })
                .collect(),
            Vec::new(),
        )
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
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, game_object);
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
            output.extend_from_slice(&value.to_le_bytes());
        }
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
        output.extend_from_slice(&[0_u8; 4 + 32]);
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

    fn push_pptr(output: &mut Vec<u8>, reference: ObjectReference) {
        push_i32(output, reference.file_id);
        output.extend_from_slice(&reference.path_id.to_le_bytes());
    }

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        push_i32(output, i32::try_from(value.len()).unwrap());
        output.extend_from_slice(value.as_bytes());
        align(output, 4);
    }

    fn parse_asset(objects: &[(i32, i64, Vec<u8>)], externals: &[&str]) -> SerializedFile {
        SerializedFile::open(Region::from_bytes(synthetic_v22(objects, externals))).unwrap()
    }

    fn synthetic_v22(objects: &[(i32, i64, Vec<u8>)], externals: &[&str]) -> Vec<u8> {
        let mut classes: Vec<i32> = objects.iter().map(|entry| entry.0).collect();
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
