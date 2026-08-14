//! Bounded planning for per-`GameObject` and per-Animator FBX workflows.

use std::collections::HashMap;

use crate::scene_hierarchy::{SceneHierarchy, SceneObjectKey};
use crate::{Error, Result};

/// Collection-wide limits for enumerating independently exported models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelExportPlanLimits {
    pub maximum_hierarchy_nodes: usize,
    pub maximum_candidates: usize,
    pub maximum_total_name_bytes: usize,
}

impl Default for ModelExportPlanLimits {
    fn default() -> Self {
        Self {
            maximum_hierarchy_nodes: 1_000_000,
            maximum_candidates: 1_000_000,
            maximum_total_name_bytes: 256 * 1024 * 1024,
        }
    }
}

/// One independently writable model selected from a scene hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelExportCandidate {
    pub game_object: SceneObjectKey,
    pub animator: Option<SceneObjectKey>,
    pub name: String,
}

/// Mirrors the managed `SplitObjects` candidate rule: each hierarchy root
/// whose descendant subtree contains a `MeshFilter` or
/// `SkinnedMeshRenderer` is independently exportable.
pub fn plan_split_object_exports(
    hierarchy: &SceneHierarchy,
    limits: ModelExportPlanLimits,
) -> Result<Vec<ModelExportCandidate>> {
    if hierarchy.nodes.len() > limits.maximum_hierarchy_nodes {
        return Err(Error::invalid_data(format!(
            "FBX split-object planning sees {} hierarchy nodes, exceeding limit {}",
            hierarchy.nodes.len(),
            limits.maximum_hierarchy_nodes
        )));
    }
    let mut contains_geometry = HashMap::new();
    contains_geometry
        .try_reserve(hierarchy.nodes.len())
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate FBX geometry index: {error}"))
        })?;
    let mut stack = Vec::new();
    stack
        .try_reserve(hierarchy.nodes.len().saturating_mul(2))
        .map_err(|error| {
            Error::invalid_data(format!("cannot allocate FBX hierarchy traversal: {error}"))
        })?;
    for root in hierarchy.roots.iter().rev().copied() {
        stack.push((root, false));
    }
    while let Some((key, visited)) = stack.pop() {
        let node = hierarchy.node(key).ok_or_else(|| {
            Error::invalid_data(format!(
                "FBX split-object hierarchy references missing GameObject {key:?}"
            ))
        })?;
        if visited {
            let mut has_geometry =
                node.mesh_filter.is_some() || node.skinned_mesh_renderer.is_some();
            for child in &node.children {
                has_geometry |= contains_geometry.get(child).copied().ok_or_else(|| {
                    Error::invalid_data(format!("FBX split-object child {child:?} was not visited"))
                })?;
            }
            contains_geometry.insert(key, has_geometry);
            continue;
        }
        stack.push((key, true));
        stack.extend(node.children.iter().rev().map(|child| (*child, false)));
    }
    if contains_geometry.len() != hierarchy.nodes.len() {
        return Err(Error::invalid_data(
            "FBX split-object hierarchy contains nodes unreachable from its roots",
        ));
    }

    let mut candidates = Vec::new();
    let mut total_name_bytes = 0_usize;
    for root in &hierarchy.roots {
        if !contains_geometry.get(root).copied().unwrap_or(false) {
            continue;
        }
        let node = hierarchy.node(*root).ok_or_else(|| {
            Error::invalid_data(format!(
                "FBX split-object root {root:?} is absent from the hierarchy"
            ))
        })?;
        push_candidate(&mut candidates, &mut total_name_bytes, node, limits)?;
    }
    Ok(candidates)
}

/// Enumerates every resolved Animator component in stable serialized-object
/// order, retaining the owning `GameObject` key used for model selection.
pub fn plan_animator_exports(
    hierarchy: &SceneHierarchy,
    limits: ModelExportPlanLimits,
) -> Result<Vec<ModelExportCandidate>> {
    if hierarchy.nodes.len() > limits.maximum_hierarchy_nodes {
        return Err(Error::invalid_data(format!(
            "FBX Animator planning sees {} hierarchy nodes, exceeding limit {}",
            hierarchy.nodes.len(),
            limits.maximum_hierarchy_nodes
        )));
    }
    collect_candidates(hierarchy, limits, |_, node| node.animator.is_some())
}

fn collect_candidates(
    hierarchy: &SceneHierarchy,
    limits: ModelExportPlanLimits,
    mut include: impl FnMut(SceneObjectKey, &crate::scene_hierarchy::SceneHierarchyNode) -> bool,
) -> Result<Vec<ModelExportCandidate>> {
    let mut candidates = Vec::new();
    let mut total_name_bytes = 0_usize;
    for node in &hierarchy.nodes {
        if !include(node.object, node) {
            continue;
        }
        push_candidate(&mut candidates, &mut total_name_bytes, node, limits)?;
    }
    Ok(candidates)
}

fn push_candidate(
    candidates: &mut Vec<ModelExportCandidate>,
    total_name_bytes: &mut usize,
    node: &crate::scene_hierarchy::SceneHierarchyNode,
    limits: ModelExportPlanLimits,
) -> Result<()> {
    if candidates.len() >= limits.maximum_candidates {
        return Err(Error::invalid_data(format!(
            "FBX export candidates exceed limit {}",
            limits.maximum_candidates
        )));
    }
    *total_name_bytes = total_name_bytes
        .checked_add(node.name.len())
        .ok_or_else(|| Error::invalid_data("FBX candidate name bytes overflowed"))?;
    if *total_name_bytes > limits.maximum_total_name_bytes {
        return Err(Error::invalid_data(format!(
            "FBX candidate names use {total_name_bytes} bytes, exceeding limit {}",
            limits.maximum_total_name_bytes
        )));
    }
    candidates.try_reserve(1).map_err(|error| {
        Error::invalid_data(format!("cannot grow FBX export candidates: {error}"))
    })?;
    candidates.push(ModelExportCandidate {
        game_object: node.object,
        animator: node.animator.as_ref().map(|animator| animator.component),
        name: clone_name(&node.name)?,
    });
    Ok(())
}

fn clone_name(value: &str) -> Result<String> {
    let mut output = String::new();
    output.try_reserve_exact(value.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate FBX candidate name: {error}"))
    })?;
    output.push_str(value);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use crate::scene_hierarchy::{SceneHierarchy, SceneHierarchyNode, SceneObjectKey};

    use super::{ModelExportPlanLimits, plan_animator_exports, plan_split_object_exports};

    #[test]
    fn plans_split_subtrees_and_animators_in_stable_node_order() {
        let hierarchy = SceneHierarchy::from_test_parts(
            vec![
                node(1, "root", None, &[2, 3], false, false),
                node(3, "sibling", Some(1), &[], false, true),
                node(2, "branch", Some(1), &[4], false, false),
                node(4, "mesh", Some(2), &[], true, false),
            ],
            vec![key(1)],
        );

        let split =
            plan_split_object_exports(&hierarchy, ModelExportPlanLimits::default()).unwrap();
        assert_eq!(
            split
                .iter()
                .map(|candidate| candidate.game_object)
                .collect::<Vec<_>>(),
            [key(1)]
        );
        let animators =
            plan_animator_exports(&hierarchy, ModelExportPlanLimits::default()).unwrap();
        assert_eq!(animators.len(), 1);
        assert_eq!(animators[0].game_object, key(3));
        assert_eq!(animators[0].animator, Some(key(103)));
        assert_eq!(animators[0].name, "sibling");
    }

    #[test]
    fn rejects_candidate_traversal_and_name_budgets() {
        let hierarchy = SceneHierarchy::from_test_parts(
            vec![node(1, "root", None, &[], true, true)],
            vec![key(1)],
        );
        let defaults = ModelExportPlanLimits::default();
        for limits in [
            ModelExportPlanLimits {
                maximum_hierarchy_nodes: 0,
                ..defaults
            },
            ModelExportPlanLimits {
                maximum_candidates: 0,
                ..defaults
            },
            ModelExportPlanLimits {
                maximum_total_name_bytes: 3,
                ..defaults
            },
        ] {
            assert!(plan_split_object_exports(&hierarchy, limits).is_err());
            assert!(plan_animator_exports(&hierarchy, limits).is_err());
        }
    }

    fn node(
        path_id: i64,
        name: &str,
        parent: Option<i64>,
        children: &[i64],
        geometry: bool,
        animator: bool,
    ) -> SceneHierarchyNode {
        SceneHierarchyNode {
            object: key(path_id),
            name: name.to_owned(),
            transform: None,
            mesh_filter: geometry.then(|| crate::scene_hierarchy::MeshFilterBinding {
                component: key(path_id + 10),
                mesh: None,
            }),
            mesh_renderer: None,
            skinned_mesh_renderer: None,
            animator: animator.then(|| crate::scene_hierarchy::AnimatorBinding {
                component: key(path_id + 100),
                avatar: crate::serialized::ObjectReference {
                    file_id: 0,
                    path_id: 0,
                },
                controller: crate::serialized::ObjectReference {
                    file_id: 0,
                    path_id: 0,
                },
            }),
            parent: parent.map(key),
            children: children.iter().copied().map(key).collect(),
        }
    }

    const fn key(path_id: i64) -> SceneObjectKey {
        SceneObjectKey {
            file_index: 0,
            path_id,
        }
    }
}
