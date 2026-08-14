//! Collection-wide discovery of `Live2D` Cubism `MonoBehaviour` roles.
//!
//! Script classes are resolved through real `MonoScript` pointers. Only an
//! embedded `TypeTree` is used to inspect the script-defined `_moc` reference;
//! stripped objects remain catalogued with an explicit schema gap instead of
//! guessing fields from raw bytes.

use std::collections::HashMap;
use std::sync::Arc;

use crate::loader::AssetCollection;
use crate::monobehaviour::{
    MONO_BEHAVIOUR_CLASS_ID, MONO_SCRIPT_CLASS_ID, MonoBehaviourReadLimits, read_mono_behaviour,
    read_mono_script,
};
use crate::scene_hierarchy::SceneObjectKey;
use crate::serialized::ObjectReference;
use crate::type_tree::{TypeTreeReadLimits, TypeValue};
use crate::{Error, Result};

/// `Live2D` script roles recognized by the managed loader and extractor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CubismRole {
    Moc,
    Model,
    Renderer,
    PhysicsController,
    FadeController,
    ExpressionController,
    DisplayInfoParameterName,
    DisplayInfoPartName,
    PosePart,
    ExpressionData,
    ExpressionList,
    FadeMotionData,
    FadeMotionList,
    EyeBlinkParameter,
    MouthParameter,
    Parameter,
    Part,
    Other,
}

impl CubismRole {
    #[must_use]
    pub fn from_script_class(class_name: &str) -> Self {
        match class_name {
            "CubismMoc" => Self::Moc,
            "CubismModel" => Self::Model,
            "CubismRenderer" => Self::Renderer,
            "CubismPhysicsController" => Self::PhysicsController,
            "CubismFadeController" => Self::FadeController,
            "CubismExpressionController" => Self::ExpressionController,
            "CubismDisplayInfoParameterName" => Self::DisplayInfoParameterName,
            "CubismDisplayInfoPartName" => Self::DisplayInfoPartName,
            "CubismPosePart" => Self::PosePart,
            "CubismExpressionData" => Self::ExpressionData,
            "CubismExpressionList" => Self::ExpressionList,
            "CubismFadeMotionData" => Self::FadeMotionData,
            "CubismFadeMotionList" => Self::FadeMotionList,
            "CubismEyeBlinkParameter" => Self::EyeBlinkParameter,
            "CubismMouthParameter" => Self::MouthParameter,
            "CubismParameter" => Self::Parameter,
            "CubismPart" => Self::Part,
            _ => Self::Other,
        }
    }

    #[must_use]
    pub const fn is_live2d(self) -> bool {
        !matches!(self, Self::Other)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Live2dCatalogLimits {
    pub maximum_mono_behaviours: usize,
    pub maximum_scripts: usize,
    pub maximum_live2d_components: usize,
    pub maximum_model_type_tree_reads: usize,
    pub maximum_total_model_object_bytes: u64,
    pub maximum_total_script_name_bytes: usize,
    pub maximum_indexed_objects: usize,
    pub maximum_indexed_externals: usize,
    pub maximum_reference_index_bytes: usize,
    pub mono_behaviour: MonoBehaviourReadLimits,
    pub type_tree: TypeTreeReadLimits,
}

impl Default for Live2dCatalogLimits {
    fn default() -> Self {
        Self {
            maximum_mono_behaviours: 2_000_000,
            maximum_scripts: 1_000_000,
            maximum_live2d_components: 1_000_000,
            maximum_model_type_tree_reads: 100_000,
            maximum_total_model_object_bytes: 1024 * 1024 * 1024,
            maximum_total_script_name_bytes: 128 * 1024 * 1024,
            maximum_indexed_objects: 10_000_000,
            maximum_indexed_externals: 10_000_000,
            maximum_reference_index_bytes: 512 * 1024 * 1024,
            mono_behaviour: MonoBehaviourReadLimits::default(),
            type_tree: TypeTreeReadLimits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CubismComponent {
    pub object: SceneObjectKey,
    pub game_object: Option<SceneObjectKey>,
    pub script: Option<SceneObjectKey>,
    pub script_class: Arc<str>,
    pub role: CubismRole,
    /// Resolved only for a `CubismModel` whose embedded `TypeTree` exposes a
    /// valid `_moc` `PPtr`. `None` also covers null/missing/wrong-class targets.
    pub moc: Option<SceneObjectKey>,
    /// True when `_moc` cannot be inspected because the model has no embedded
    /// tree. A managed assembly or matching dummy-DLL remains required.
    pub model_schema_missing: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Live2dCatalog {
    pub components: Vec<CubismComponent>,
}

/// Builds a stable, collection-order catalog of recognized Cubism components.
pub fn build_live2d_catalog(
    collection: &AssetCollection,
    limits: Live2dCatalogLimits,
) -> Result<Live2dCatalog> {
    let mut state = CatalogState::new(collection, limits)?;
    for (file_index, loaded) in collection.serialized_files.iter().enumerate() {
        for (object_index, object) in loaded.file.objects.iter().enumerate() {
            if object.class_id != MONO_BEHAVIOUR_CLASS_ID {
                continue;
            }
            state.mono_behaviours = charge(
                state.mono_behaviours,
                1,
                limits.maximum_mono_behaviours,
                "MonoBehaviour candidates",
            )?;
            state.inspect(file_index, object_index)?;
        }
    }
    state.finish()
}

struct CatalogState<'a> {
    collection: &'a AssetCollection,
    limits: Live2dCatalogLimits,
    scripts: HashMap<SceneObjectKey, Arc<str>>,
    references: ReferenceIndex,
    components: Vec<CubismComponent>,
    mono_behaviours: usize,
    model_type_tree_reads: usize,
    model_object_bytes: u64,
    script_name_bytes: usize,
}

impl<'a> CatalogState<'a> {
    fn new(collection: &'a AssetCollection, limits: Live2dCatalogLimits) -> Result<Self> {
        Ok(Self {
            collection,
            limits,
            scripts: HashMap::new(),
            references: ReferenceIndex::new(collection, limits)?,
            components: Vec::new(),
            mono_behaviours: 0,
            model_type_tree_reads: 0,
            model_object_bytes: 0,
            script_name_bytes: 0,
        })
    }

    fn inspect(&mut self, file_index: usize, object_index: usize) -> Result<()> {
        let file = &self.collection.serialized_files[file_index].file;
        let behaviour = read_mono_behaviour(file, object_index, self.limits.mono_behaviour)?;
        let Some((script_key, script_class)) = self.script_class(file_index, behaviour.script)?
        else {
            return Ok(());
        };
        let role = CubismRole::from_script_class(&script_class);
        if !role.is_live2d() {
            return Ok(());
        }
        if self.components.len() >= self.limits.maximum_live2d_components {
            return Err(Error::invalid_data(format!(
                "Live2D catalog exceeds {} components",
                self.limits.maximum_live2d_components
            )));
        }
        let game_object = self.references.try_resolve_key(
            self.collection,
            file_index,
            ObjectReference {
                file_id: behaviour.game_object.file_id,
                path_id: behaviour.game_object.path_id,
            },
            1,
        );
        let (moc, model_schema_missing) = if role == CubismRole::Model {
            self.model_type_tree_reads = charge(
                self.model_type_tree_reads,
                1,
                self.limits.maximum_model_type_tree_reads,
                "CubismModel TypeTree reads",
            )?;
            let object_bytes = file.objects[object_index].byte_size;
            self.model_object_bytes = charge_u64(
                self.model_object_bytes,
                object_bytes,
                self.limits.maximum_total_model_object_bytes,
                "CubismModel object bytes",
            )?;
            match file.read_type_tree_value_with_limits(object_index, self.limits.type_tree) {
                Ok(value) => (
                    find_pptr_field(&value, "_moc").and_then(|reference| {
                        self.references.try_resolve_key(
                            self.collection,
                            file_index,
                            reference,
                            MONO_BEHAVIOUR_CLASS_ID,
                        )
                    }),
                    false,
                ),
                Err(Error::Unsupported(_)) => (None, true),
                Err(error) => return Err(error),
            }
        } else {
            (None, false)
        };
        self.components
            .try_reserve(1)
            .map_err(|error| Error::invalid_data(format!("cannot grow Live2D catalog: {error}")))?;
        self.components.push(CubismComponent {
            object: SceneObjectKey {
                file_index,
                path_id: behaviour.path_id,
            },
            game_object,
            script: Some(script_key),
            script_class,
            role,
            moc,
            model_schema_missing,
        });
        Ok(())
    }

    fn finish(mut self) -> Result<Live2dCatalog> {
        let mut moc_objects = Vec::new();
        moc_objects
            .try_reserve(self.components.len())
            .map_err(|error| {
                Error::invalid_data(format!("cannot grow Live2D MOC index: {error}"))
            })?;
        moc_objects.extend(
            self.components
                .iter()
                .filter(|component| component.role == CubismRole::Moc)
                .map(|component| component.object),
        );
        moc_objects.sort_unstable();
        moc_objects.dedup();
        for component in &mut self.components {
            if component.role == CubismRole::Model
                && component
                    .moc
                    .is_some_and(|key| moc_objects.binary_search(&key).is_err())
            {
                component.moc = None;
            }
        }
        Ok(Live2dCatalog {
            components: self.components,
        })
    }

    fn script_class(
        &mut self,
        source_file_index: usize,
        reference: crate::monobehaviour::ObjectReference,
    ) -> Result<Option<(SceneObjectKey, Arc<str>)>> {
        let reference = ObjectReference {
            file_id: reference.file_id,
            path_id: reference.path_id,
        };
        let Some((target_file_index, target_object_index)) =
            self.references.resolve(source_file_index, reference)
        else {
            return Ok(None);
        };
        let target_file = &self.collection.serialized_files[target_file_index].file;
        let target_object = &target_file.objects[target_object_index];
        if target_object.class_id != MONO_SCRIPT_CLASS_ID {
            return Ok(None);
        }
        let key = SceneObjectKey {
            file_index: target_file_index,
            path_id: target_object.path_id,
        };
        if let Some(class_name) = self.scripts.get(&key) {
            return Ok(Some((key, Arc::clone(class_name))));
        }
        if self.scripts.len() >= self.limits.maximum_scripts {
            return Err(Error::invalid_data(format!(
                "Live2D catalog exceeds {} MonoScripts",
                self.limits.maximum_scripts
            )));
        }
        let script =
            read_mono_script(target_file, target_object_index, self.limits.mono_behaviour)?;
        self.script_name_bytes = charge(
            self.script_name_bytes,
            script.class_name.len(),
            self.limits.maximum_total_script_name_bytes,
            "Live2D script class-name bytes",
        )?;
        self.scripts.try_reserve(1).map_err(|error| {
            Error::invalid_data(format!("cannot grow Live2D script cache: {error}"))
        })?;
        let class_name: Arc<str> = script.class_name.into();
        self.scripts.insert(key, Arc::clone(&class_name));
        Ok(Some((key, class_name)))
    }
}

/// Per-catalog immutable lookup tables keep repeated `PPtr` resolution at
/// O(log n) without changing `AssetCollection`'s public construction API.
struct ReferenceIndex {
    objects: Vec<Vec<(i64, usize)>>,
    external_files: Vec<Vec<Option<usize>>>,
}

impl ReferenceIndex {
    fn new(collection: &AssetCollection, limits: Live2dCatalogLimits) -> Result<Self> {
        let file_count = collection.serialized_files.len();
        let mut retained_bytes = 0_usize;
        charge_index_bytes::<Vec<(i64, usize)>>(
            &mut retained_bytes,
            file_count,
            limits.maximum_reference_index_bytes,
            "Live2D per-file object indexes",
        )?;
        charge_index_bytes::<Vec<Option<usize>>>(
            &mut retained_bytes,
            file_count,
            limits.maximum_reference_index_bytes,
            "Live2D per-file external indexes",
        )?;
        let objects = Self::build_object_indexes(collection, limits, &mut retained_bytes)?;
        let file_names = Self::build_file_name_index(collection, limits, &mut retained_bytes)?;
        let external_files =
            Self::build_external_indexes(collection, &file_names, limits, &mut retained_bytes)?;
        Ok(Self {
            objects,
            external_files,
        })
    }

    fn build_object_indexes(
        collection: &AssetCollection,
        limits: Live2dCatalogLimits,
        retained_bytes: &mut usize,
    ) -> Result<Vec<Vec<(i64, usize)>>> {
        let file_count = collection.serialized_files.len();
        let mut objects = Vec::new();
        objects.try_reserve_exact(file_count).map_err(|error| {
            Error::invalid_data(format!("cannot allocate Live2D object indexes: {error}"))
        })?;
        let mut indexed_objects = 0_usize;
        for loaded in &collection.serialized_files {
            indexed_objects = charge(
                indexed_objects,
                loaded.file.objects.len(),
                limits.maximum_indexed_objects,
                "Live2D indexed objects",
            )?;
            charge_index_bytes::<(i64, usize)>(
                retained_bytes,
                loaded.file.objects.len(),
                limits.maximum_reference_index_bytes,
                "Live2D object index entries",
            )?;
            let mut file_objects = Vec::new();
            file_objects
                .try_reserve_exact(loaded.file.objects.len())
                .map_err(|error| {
                    Error::invalid_data(format!("cannot allocate a Live2D object index: {error}"))
                })?;
            file_objects.extend(
                loaded
                    .file
                    .objects
                    .iter()
                    .enumerate()
                    .map(|(index, object)| (object.path_id, index)),
            );
            file_objects.sort_unstable();
            objects.push(file_objects);
        }
        Ok(objects)
    }

    fn build_file_name_index(
        collection: &AssetCollection,
        limits: Live2dCatalogLimits,
        retained_bytes: &mut usize,
    ) -> Result<Vec<(Vec<u8>, usize)>> {
        let file_count = collection.serialized_files.len();
        let mut file_names = Vec::new();
        charge_index_bytes::<(Vec<u8>, usize)>(
            retained_bytes,
            file_count,
            limits.maximum_reference_index_bytes,
            "Live2D portable file-name index entries",
        )?;
        file_names.try_reserve_exact(file_count).map_err(|error| {
            Error::invalid_data(format!(
                "cannot allocate Live2D portable file-name index: {error}"
            ))
        })?;
        for (file_index, loaded) in collection.serialized_files.iter().enumerate() {
            let key = ascii_lowercase_key(portable_file_name(&loaded.path))?;
            *retained_bytes = charge(
                *retained_bytes,
                key.capacity(),
                limits.maximum_reference_index_bytes,
                "Live2D reference-index bytes",
            )?;
            file_names.push((key, file_index));
        }
        file_names.sort_unstable();
        Ok(file_names)
    }

    fn build_external_indexes(
        collection: &AssetCollection,
        file_names: &[(Vec<u8>, usize)],
        limits: Live2dCatalogLimits,
        retained_bytes: &mut usize,
    ) -> Result<Vec<Vec<Option<usize>>>> {
        let file_count = collection.serialized_files.len();
        let mut external_files = Vec::new();
        external_files
            .try_reserve_exact(file_count)
            .map_err(|error| {
                Error::invalid_data(format!("cannot allocate Live2D external indexes: {error}"))
            })?;
        let mut indexed_externals = 0_usize;
        for loaded in &collection.serialized_files {
            indexed_externals = charge(
                indexed_externals,
                loaded.file.externals.len(),
                limits.maximum_indexed_externals,
                "Live2D indexed external references",
            )?;
            charge_index_bytes::<Option<usize>>(
                retained_bytes,
                loaded.file.externals.len(),
                limits.maximum_reference_index_bytes,
                "Live2D external index entries",
            )?;
            let mut resolved = Vec::new();
            resolved
                .try_reserve_exact(loaded.file.externals.len())
                .map_err(|error| {
                    Error::invalid_data(format!(
                        "cannot allocate a Live2D external-file index: {error}"
                    ))
                })?;
            for external in &loaded.file.externals {
                let key = ascii_lowercase_key(portable_file_name(&external.path))?;
                resolved.push(find_file_index(file_names, &key));
            }
            external_files.push(resolved);
        }
        Ok(external_files)
    }

    fn resolve(
        &self,
        source_file_index: usize,
        reference: ObjectReference,
    ) -> Option<(usize, usize)> {
        if reference.is_null() {
            return None;
        }
        let target_file_index = if reference.file_id == 0 {
            source_file_index
        } else {
            let external_index = usize::try_from(reference.file_id.checked_sub(1)?).ok()?;
            (*self
                .external_files
                .get(source_file_index)?
                .get(external_index)?)?
        };
        let file_objects = self.objects.get(target_file_index)?;
        let position = file_objects.partition_point(|(path_id, _)| *path_id < reference.path_id);
        let &(path_id, object_index) = file_objects.get(position)?;
        (path_id == reference.path_id).then_some((target_file_index, object_index))
    }

    fn try_resolve_key(
        &self,
        collection: &AssetCollection,
        source_file_index: usize,
        reference: ObjectReference,
        expected_class: i32,
    ) -> Option<SceneObjectKey> {
        let (file_index, object_index) = self.resolve(source_file_index, reference)?;
        let object = collection
            .serialized_files
            .get(file_index)?
            .file
            .objects
            .get(object_index)?;
        (object.class_id == expected_class).then_some(SceneObjectKey {
            file_index,
            path_id: object.path_id,
        })
    }
}

fn charge_index_bytes<T>(
    current: &mut usize,
    count: usize,
    maximum: usize,
    field: &str,
) -> Result<()> {
    let bytes = count
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| Error::invalid_data(format!("{field} overflowed")))?;
    *current = charge(*current, bytes, maximum, field)?;
    Ok(())
}

fn find_file_index(file_names: &[(Vec<u8>, usize)], key: &[u8]) -> Option<usize> {
    let position = file_names.partition_point(|(name, _)| name.as_slice() < key);
    let (name, index) = file_names.get(position)?;
    (name.as_slice() == key).then_some(*index)
}

fn ascii_lowercase_key(value: &str) -> Result<Vec<u8>> {
    let mut key = Vec::new();
    key.try_reserve_exact(value.len()).map_err(|error| {
        Error::invalid_data(format!("cannot allocate a portable file-name key: {error}"))
    })?;
    key.extend(value.bytes().map(|byte| byte.to_ascii_lowercase()));
    Ok(key)
}

fn portable_file_name(path: &str) -> &str {
    let component = path.rsplit_once("::").map_or(path, |(_, name)| name);
    component.rsplit(['/', '\\']).next().unwrap_or(component)
}

fn find_pptr_field(value: &TypeValue, field_name: &str) -> Option<ObjectReference> {
    let TypeValue::Object(fields) = value else {
        return None;
    };
    fields
        .iter()
        .find(|field| field.name == field_name)
        .and_then(|field| parse_pptr(&field.value))
}

fn parse_pptr(value: &TypeValue) -> Option<ObjectReference> {
    let TypeValue::Object(fields) = value else {
        return None;
    };
    let file_id = fields
        .iter()
        .find(|field| field.name == "m_FileID")
        .and_then(integer_value)
        .and_then(|value| i32::try_from(value).ok())?;
    let path_id = fields
        .iter()
        .find(|field| field.name == "m_PathID")
        .and_then(integer_value)?;
    Some(ObjectReference { file_id, path_id })
}

fn integer_value(field: &crate::type_tree::TypeField) -> Option<i64> {
    match &field.value {
        TypeValue::Signed(value) => Some(*value),
        TypeValue::Unsigned(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

fn charge(current: usize, additional: usize, maximum: usize, field: &str) -> Result<usize> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data(format!("{field} overflowed")))?;
    if total > maximum {
        return Err(Error::invalid_data(format!(
            "{field} total {total} exceeds limit {maximum}"
        )));
    }
    Ok(total)
}

fn charge_u64(current: u64, additional: u64, maximum: u64, field: &str) -> Result<u64> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| Error::invalid_data(format!("{field} overflowed")))?;
    if total > maximum {
        return Err(Error::invalid_data(format!(
            "{field} total {total} exceeds limit {maximum}"
        )));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::loader::{AssetCollection, LoadedSerializedFile};
    use crate::scene_hierarchy::SceneObjectKey;
    use crate::serialized::ObjectReference;
    use crate::serialized::SerializedFile;
    use crate::source::Region;
    use crate::type_tree::{TypeField, TypeValue};

    use super::{
        CubismComponent, CubismRole, Live2dCatalogLimits, build_live2d_catalog, charge, charge_u64,
        find_pptr_field,
    };

    const MONO_BEHAVIOUR_CLASS_ID: i32 = 114;
    const MONO_SCRIPT_CLASS_ID: i32 = 115;

    #[test]
    fn recognizes_only_the_managed_cubism_role_names() {
        let roles = [
            ("CubismMoc", CubismRole::Moc),
            ("CubismModel", CubismRole::Model),
            ("CubismRenderer", CubismRole::Renderer),
            ("CubismPhysicsController", CubismRole::PhysicsController),
            ("CubismFadeController", CubismRole::FadeController),
            (
                "CubismExpressionController",
                CubismRole::ExpressionController,
            ),
            (
                "CubismDisplayInfoParameterName",
                CubismRole::DisplayInfoParameterName,
            ),
            ("CubismDisplayInfoPartName", CubismRole::DisplayInfoPartName),
            ("CubismPosePart", CubismRole::PosePart),
            ("CubismExpressionData", CubismRole::ExpressionData),
            ("CubismExpressionList", CubismRole::ExpressionList),
            ("CubismFadeMotionData", CubismRole::FadeMotionData),
            ("CubismFadeMotionList", CubismRole::FadeMotionList),
            ("CubismEyeBlinkParameter", CubismRole::EyeBlinkParameter),
            ("CubismMouthParameter", CubismRole::MouthParameter),
            ("CubismParameter", CubismRole::Parameter),
            ("CubismPart", CubismRole::Part),
        ];
        for (script_class, expected) in roles {
            assert_eq!(CubismRole::from_script_class(script_class), expected);
        }
        assert_eq!(
            CubismRole::from_script_class("cubismmoc"),
            CubismRole::Other
        );
        assert!(!CubismRole::Other.is_live2d());
    }

    #[test]
    fn extracts_only_a_well_formed_direct_moc_pointer() {
        let root = TypeValue::Object(vec![
            TypeField {
                name: "noise".to_owned(),
                value: TypeValue::Signed(3),
            },
            TypeField {
                name: "_moc".to_owned(),
                value: TypeValue::Object(vec![
                    TypeField {
                        name: "m_PathID".to_owned(),
                        value: TypeValue::Signed(77),
                    },
                    TypeField {
                        name: "m_FileID".to_owned(),
                        value: TypeValue::Unsigned(2),
                    },
                ]),
            },
        ]);
        assert_eq!(
            find_pptr_field(&root, "_moc"),
            Some(ObjectReference {
                file_id: 2,
                path_id: 77,
            })
        );
        assert_eq!(find_pptr_field(&root, "moc"), None);

        let malformed = TypeValue::Object(vec![TypeField {
            name: "_moc".to_owned(),
            value: TypeValue::Object(vec![TypeField {
                name: "m_FileID".to_owned(),
                value: TypeValue::Unsigned(u64::MAX),
            }]),
        }]);
        assert_eq!(find_pptr_field(&malformed, "_moc"), None);
    }

    #[test]
    fn catalog_limits_and_shared_script_names_remain_bounded() {
        let limits = Live2dCatalogLimits::default();
        assert_eq!(charge(9, 1, 10, "items").unwrap(), 10);
        assert!(charge(10, 1, 10, "items").is_err());
        assert!(charge(usize::MAX, 1, usize::MAX, "items").is_err());
        assert_eq!(charge_u64(9, 1, 10, "bytes").unwrap(), 10);
        assert!(charge_u64(10, 1, 10, "bytes").is_err());
        assert!(limits.maximum_model_type_tree_reads < limits.maximum_mono_behaviours);

        let shared: Arc<str> = Arc::from("CubismMoc");
        let component = |path_id| CubismComponent {
            object: SceneObjectKey {
                file_index: 0,
                path_id,
            },
            game_object: None,
            script: Some(SceneObjectKey {
                file_index: 0,
                path_id: 1,
            }),
            script_class: Arc::clone(&shared),
            role: CubismRole::Moc,
            moc: None,
            model_schema_missing: false,
        };
        let first = component(2);
        let second = component(3);
        assert!(Arc::ptr_eq(&first.script_class, &second.script_class));
    }

    #[test]
    fn builds_a_stable_catalog_and_verifies_model_moc_targets() {
        let collection = synthetic_collection(8);

        let catalog = build_live2d_catalog(&collection, Live2dCatalogLimits::default()).unwrap();

        assert_eq!(catalog.components.len(), 3);
        let model = &catalog.components[0];
        assert_eq!(model.object.path_id, 7);
        assert_eq!(model.role, CubismRole::Model);
        assert_eq!(model.script_class.as_ref(), "CubismModel");
        assert_eq!(model.moc.map(|key| key.path_id), Some(8));
        assert!(!model.model_schema_missing);
        let first_moc = &catalog.components[1];
        let second_moc = &catalog.components[2];
        assert_eq!(first_moc.object.path_id, 8);
        assert_eq!(second_moc.object.path_id, 11);
        assert_eq!(first_moc.role, CubismRole::Moc);
        assert!(Arc::ptr_eq(
            &first_moc.script_class,
            &second_moc.script_class
        ));

        let wrong_target = synthetic_collection(10);
        let catalog = build_live2d_catalog(&wrong_target, Live2dCatalogLimits::default()).unwrap();
        assert_eq!(catalog.components[0].moc, None);
    }

    #[test]
    fn resolves_cross_file_scripts_and_moc_with_portable_ascii_names() {
        let source_objects = vec![(7, 0, mono_behaviour_with_references(1, 10, 1, 8))];
        let target_objects = vec![
            (8, 0, mono_behaviour(9, 0)),
            (9, 1, mono_script("CubismMoc")),
            (10, 1, mono_script("CubismModel")),
        ];
        let source = SerializedFile::open(Region::from_bytes(synthetic_v22(
            &source_objects,
            &["archive:/MOC.ASSETS"],
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
                    path: "bundle::folder/moc.assets".to_owned(),
                    file: target,
                },
            ],
            Vec::new(),
        );

        let catalog = build_live2d_catalog(&collection, Live2dCatalogLimits::default()).unwrap();

        assert_eq!(catalog.components.len(), 2);
        assert_eq!(
            catalog.components[0].script,
            Some(SceneObjectKey {
                file_index: 1,
                path_id: 10,
            })
        );
        assert_eq!(
            catalog.components[0].moc,
            Some(SceneObjectKey {
                file_index: 1,
                path_id: 8,
            })
        );
    }

    #[test]
    fn enforces_collection_wide_catalog_budgets_before_amplification() {
        let collection = synthetic_collection(8);
        let too_few_candidates = Live2dCatalogLimits {
            maximum_mono_behaviours: 2,
            ..Live2dCatalogLimits::default()
        };
        assert!(build_live2d_catalog(&collection, too_few_candidates).is_err());

        let too_few_components = Live2dCatalogLimits {
            maximum_live2d_components: 1,
            ..Live2dCatalogLimits::default()
        };
        assert!(build_live2d_catalog(&collection, too_few_components).is_err());

        let no_model_reads = Live2dCatalogLimits {
            maximum_model_type_tree_reads: 0,
            ..Live2dCatalogLimits::default()
        };
        assert!(build_live2d_catalog(&collection, no_model_reads).is_err());

        let short_script_names = Live2dCatalogLimits {
            maximum_total_script_name_bytes: "CubismModel".len() - 1,
            ..Live2dCatalogLimits::default()
        };
        assert!(build_live2d_catalog(&collection, short_script_names).is_err());

        let short_object_index = Live2dCatalogLimits {
            maximum_indexed_objects: 4,
            ..Live2dCatalogLimits::default()
        };
        assert!(build_live2d_catalog(&collection, short_object_index).is_err());

        let short_index_bytes = Live2dCatalogLimits {
            maximum_reference_index_bytes: 1,
            ..Live2dCatalogLimits::default()
        };
        assert!(build_live2d_catalog(&collection, short_index_bytes).is_err());
    }

    fn synthetic_collection(model_moc_path_id: i64) -> AssetCollection {
        let objects = vec![
            (7, 0, mono_behaviour(10, model_moc_path_id)),
            (8, 0, mono_behaviour(9, 0)),
            (11, 0, mono_behaviour(9, 0)),
            (9, 1, mono_script("CubismMoc")),
            (10, 1, mono_script("CubismModel")),
        ];
        let file = SerializedFile::open(Region::from_bytes(synthetic_v22(&objects, &[]))).unwrap();
        AssetCollection::from_loaded_parts(
            vec![LoadedSerializedFile {
                path: "live2d.assets".to_owned(),
                file,
            }],
            Vec::new(),
        )
    }

    fn mono_behaviour(script_path_id: i64, moc_path_id: i64) -> Vec<u8> {
        mono_behaviour_with_references(0, script_path_id, 0, moc_path_id)
    }

    fn mono_behaviour_with_references(
        script_file_id: i32,
        script_path_id: i64,
        moc_file_id: i32,
        moc_path_id: i64,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        push_pptr(&mut output, 0, 0);
        output.push(1);
        align(&mut output, 4);
        push_pptr(&mut output, script_file_id, script_path_id);
        push_aligned_string(&mut output, "Component");
        push_pptr(&mut output, moc_file_id, moc_path_id);
        output
    }

    fn mono_script(class_name: &str) -> Vec<u8> {
        let mut output = Vec::new();
        push_aligned_string(&mut output, "Cubism script");
        output.extend_from_slice(&0_i32.to_le_bytes());
        output.extend_from_slice(&[0x55; 16]);
        push_aligned_string(&mut output, class_name);
        push_aligned_string(&mut output, "Live2D.Cubism.Core");
        push_aligned_string(&mut output, "Live2D.Cubism.dll");
        output
    }

    fn synthetic_v22(objects: &[(i64, i32, Vec<u8>)], externals: &[&str]) -> Vec<u8> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(b"2022.3.62f1\0");
        metadata.extend_from_slice(&13_i32.to_le_bytes());
        metadata.push(1);
        metadata.extend_from_slice(&2_i32.to_le_bytes());
        push_type(&mut metadata, MONO_BEHAVIOUR_CLASS_ID, true);
        push_type(&mut metadata, MONO_SCRIPT_CLASS_ID, false);

        metadata.extend_from_slice(&i32::try_from(objects.len()).unwrap().to_le_bytes());
        let mut data = Vec::new();
        for (path_id, type_index, object) in objects {
            align(&mut data, 4);
            push_object_info(
                &mut metadata,
                *path_id,
                data.len(),
                object.len(),
                *type_index,
            );
            data.extend_from_slice(object);
        }
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.extend_from_slice(&i32::try_from(externals.len()).unwrap().to_le_bytes());
        for external in externals {
            metadata.push(0);
            metadata.extend_from_slice(&[0_u8; 16]);
            metadata.extend_from_slice(&0_i32.to_le_bytes());
            metadata.extend_from_slice(external.as_bytes());
            metadata.push(0);
        }
        metadata.extend_from_slice(&0_i32.to_le_bytes());
        metadata.push(0);

        let metadata_size = u32::try_from(metadata.len()).unwrap();
        let metadata_end = 48_u64 + u64::from(metadata_size);
        let data_offset = metadata_end.div_ceil(16) * 16;
        let file_size = data_offset + u64::try_from(data.len()).unwrap();
        let mut output = vec![0_u8; 48];
        output[8..12].copy_from_slice(&22_u32.to_be_bytes());
        output[16] = 0;
        output[20..24].copy_from_slice(&metadata_size.to_be_bytes());
        output[24..32].copy_from_slice(&i64::try_from(file_size).unwrap().to_be_bytes());
        output[32..40].copy_from_slice(&i64::try_from(data_offset).unwrap().to_be_bytes());
        output.extend_from_slice(&metadata);
        output.resize(usize::try_from(data_offset).unwrap(), 0);
        output.extend_from_slice(&data);
        output
    }

    fn push_type(output: &mut Vec<u8>, class_id: i32, behaviour: bool) {
        output.extend_from_slice(&class_id.to_le_bytes());
        output.push(0);
        output.extend_from_slice(&(-1_i16).to_le_bytes());
        if behaviour {
            output.extend_from_slice(&[0x11; 16]);
        }
        output.extend_from_slice(&[0x22; 16]);
        if behaviour {
            push_blob_tree(output, &mono_behaviour_tree());
        } else {
            output.extend_from_slice(&0_i32.to_le_bytes());
            output.extend_from_slice(&0_i32.to_le_bytes());
        }
        output.extend_from_slice(&0_i32.to_le_bytes());
    }

    fn push_object_info(
        output: &mut Vec<u8>,
        path_id: i64,
        byte_start: usize,
        byte_size: usize,
        type_index: i32,
    ) {
        align_with_base(output, 48, 4);
        output.extend_from_slice(&path_id.to_le_bytes());
        output.extend_from_slice(&i64::try_from(byte_start).unwrap().to_le_bytes());
        output.extend_from_slice(&u32::try_from(byte_size).unwrap().to_le_bytes());
        output.extend_from_slice(&type_index.to_le_bytes());
    }

    #[derive(Clone, Copy)]
    struct TestNode {
        type_name: &'static str,
        field_name: &'static str,
        level: u8,
        align: bool,
    }

    fn mono_behaviour_tree() -> Vec<TestNode> {
        vec![
            node("MonoBehaviour", "Base", 0, false),
            node("PPtr<GameObject>", "m_GameObject", 1, false),
            node("int", "m_FileID", 2, false),
            node("SInt64", "m_PathID", 2, false),
            node("UInt8", "m_Enabled", 1, true),
            node("PPtr<MonoScript>", "m_Script", 1, false),
            node("int", "m_FileID", 2, false),
            node("SInt64", "m_PathID", 2, false),
            node("string", "m_Name", 1, false),
            node("Array", "Array", 2, true),
            node("int", "size", 3, false),
            node("char", "data", 3, false),
            node("PPtr<MonoBehaviour>", "_moc", 1, false),
            node("int", "m_FileID", 2, false),
            node("SInt64", "m_PathID", 2, false),
        ]
    }

    const fn node(
        type_name: &'static str,
        field_name: &'static str,
        level: u8,
        align: bool,
    ) -> TestNode {
        TestNode {
            type_name,
            field_name,
            level,
            align,
        }
    }

    fn push_blob_tree(output: &mut Vec<u8>, nodes: &[TestNode]) {
        let mut strings = Vec::new();
        let mut offsets = Vec::new();
        for node in nodes {
            let type_offset = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(node.type_name.as_bytes());
            strings.push(0);
            let name_offset = u32::try_from(strings.len()).unwrap();
            strings.extend_from_slice(node.field_name.as_bytes());
            strings.push(0);
            offsets.push((type_offset, name_offset));
        }
        output.extend_from_slice(&i32::try_from(nodes.len()).unwrap().to_le_bytes());
        output.extend_from_slice(&i32::try_from(strings.len()).unwrap().to_le_bytes());
        for (index, (node, (type_offset, name_offset))) in nodes.iter().zip(offsets).enumerate() {
            output.extend_from_slice(&1_u16.to_le_bytes());
            output.push(node.level);
            output.push(0);
            output.extend_from_slice(&type_offset.to_le_bytes());
            output.extend_from_slice(&name_offset.to_le_bytes());
            output.extend_from_slice(&(-1_i32).to_le_bytes());
            output.extend_from_slice(&i32::try_from(index).unwrap().to_le_bytes());
            output.extend_from_slice(&(if node.align { 0x4000_i32 } else { 0 }).to_le_bytes());
            output.extend_from_slice(&0_u64.to_le_bytes());
        }
        output.extend_from_slice(&strings);
    }

    fn push_pptr(output: &mut Vec<u8>, file_id: i32, path_id: i64) {
        output.extend_from_slice(&file_id.to_le_bytes());
        output.extend_from_slice(&path_id.to_le_bytes());
    }

    fn push_aligned_string(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(&i32::try_from(value.len()).unwrap().to_le_bytes());
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
