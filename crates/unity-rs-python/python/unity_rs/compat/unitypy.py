"""Read-focused compatibility facade for UnityPy 1.25.x.

The facade deliberately lives below :mod:`unity_rs` so installing the main
wheel never shadows a real ``UnityPy`` installation.  It mirrors UnityPy's
object graph while keeping parsing, resource resolution, and limits in the
native ``unity-rs`` implementation.
"""

from __future__ import annotations

import ntpath
import os
import re
import warnings
from collections.abc import Iterator, Mapping
from enum import IntEnum
from importlib import import_module
from pathlib import Path
from typing import (
    Any,
    BinaryIO,
    Callable,
    Dict,
    List,
    Optional,
    Sequence,
    Tuple,
    Type,
    Union,
    cast,
)

from .. import FileInfo, ObjectInfo, RgbaImage, UnityRs

UNITYPY_COMPAT_VERSION = "1.25.3"
__version__ = UNITYPY_COMPAT_VERSION
_DEFAULT_MAXIMUM_FILE_BYTES = 536_870_912
_DEFAULT_MAXIMUM_TOTAL_BYTES = 4_294_967_296
_DEFAULT_MAXIMUM_COMPAT_OBJECTS = 1_000_000
_DEFAULT_MAXIMUM_TYPE_TREE_NODES = 1_000_000
_OBJECT_PAGE_SIZE = 4_096


class TypeTreeError(ValueError):
    """The object has no verified TypeTree or its tree does not match."""


class UnityVersionFallbackError(ValueError):
    """A stripped Unity version requires an explicit caller override."""


class UnityVersionFallbackWarning(UserWarning):
    """Compatibility warning for an explicitly configured fallback version."""


class _UnityPyConfig:
    """Mutable subset of :mod:`UnityPy.config` used by read migrations."""

    def __init__(self) -> None:
        self.FALLBACK_UNITY_VERSION: Optional[str] = None

    def _validated_fallback_version(self) -> str:
        fallback = self.FALLBACK_UNITY_VERSION
        if not isinstance(fallback, str):
            raise UnityVersionFallbackError(
                "No valid Unity version found, and the fallback version is not correctly configured. "
                "Please explicitly set the value of UnityPy.config.FALLBACK_UNITY_VERSION."
            )
        return fallback

    @staticmethod
    def _warn_fallback_version(fallback: str) -> None:
        warnings.warn(
            "No valid Unity version found, defaulting to UnityPy.config.FALLBACK_UNITY_VERSION ({})".format(
                fallback
            ),
            category=UnityVersionFallbackWarning,
            stacklevel=3,
        )

    def get_fallback_version(self) -> str:
        fallback = self._validated_fallback_version()
        self._warn_fallback_version(fallback)
        return fallback


config = _UnityPyConfig()


class ClassIDType(IntEnum):
    """Common Unity class IDs used by UnityPy-style consumer code.

    Unknown numeric IDs remain representable and retain their original value.
    """

    UnknownType = -1
    GameObject = 1
    Component = 2
    Transform = 4
    Material = 21
    MeshRenderer = 23
    Texture2D = 28
    MeshFilter = 33
    Mesh = 43
    Shader = 48
    TextAsset = 49
    AnimationClip = 74
    AudioClip = 83
    Avatar = 90
    AnimatorController = 91
    Animator = 95
    Animation = 111
    MonoBehaviour = 114
    MonoScript = 115
    Font = 128
    PlayerSettings = 129
    SkinnedMeshRenderer = 137
    BuildSettings = 141
    AssetBundle = 142
    ResourceManager = 147
    PreloadData = 150
    MovieTexture = 152
    Texture2DArray = 187
    Sprite = 213
    AnimatorOverrideController = 221
    RectTransform = 224
    VideoClip = 329
    SpriteAtlas = 687_078_895

    @classmethod
    def _missing_(cls, value: object) -> Optional[ClassIDType]:
        if not isinstance(value, int):
            return None
        member = int.__new__(cls, value)
        member._name_ = "UnknownType_{}".format(value)
        member._value_ = value
        cls._value2member_map_[value] = member
        return member


class ExternalFile:
    """Minimal UnityPy-compatible external serialized-file record."""

    def __init__(self, path: str) -> None:
        self.path = path
        self.name = os.path.basename(path.replace("\\", "/"))


class Environment:
    """UnityPy-shaped owner for one native, bounded asset collection."""

    def __init__(
        self,
        *sources: object,
        fs: Optional[object] = None,
        path: Optional[os.PathLike[str]] = None,
        unity_version: Optional[str] = None,
        maximum_files: int = 100_000,
        maximum_file_bytes: int = _DEFAULT_MAXIMUM_FILE_BYTES,
        maximum_total_bytes: int = _DEFAULT_MAXIMUM_TOTAL_BYTES,
        maximum_compat_objects: int = _DEFAULT_MAXIMUM_COMPAT_OBJECTS,
        maximum_compat_types: int = 1_000_000,
        maximum_type_dependencies: int = 1_000_000,
        maximum_type_tree_values: int = 1_000_000,
        maximum_type_tree_array_elements: int = 1_000_000,
        maximum_type_tree_materialized_bytes: int = _DEFAULT_MAXIMUM_FILE_BYTES,
        oodle_decoder: Optional[Callable[[bytes, int], bytes]] = None,
        skip_unreadable_inputs: bool = False,
        unity_cn_key: Optional[Union[bytes, str]] = None,
        strict_unity_versions: bool = False,
    ) -> None:
        if path is not None:
            if sources:
                raise TypeError("path= cannot be combined with positional sources")
            sources = (path,)
        if not sources:
            raise TypeError("UnityPy.load requires at least one source")
        if maximum_compat_objects < 0:
            raise ValueError("maximum_compat_objects must be non-negative")
        for name, value in (
            ("maximum_compat_types", maximum_compat_types),
            ("maximum_type_dependencies", maximum_type_dependencies),
            ("maximum_type_tree_values", maximum_type_tree_values),
            ("maximum_type_tree_array_elements", maximum_type_tree_array_elements),
            (
                "maximum_type_tree_materialized_bytes",
                maximum_type_tree_materialized_bytes,
            ),
        ):
            if value < 0:
                raise ValueError("{} must be non-negative".format(name))

        self.maximum_compat_objects = maximum_compat_objects
        self.maximum_compat_types = maximum_compat_types
        self.maximum_type_dependencies = maximum_type_dependencies
        self.maximum_object_bytes = maximum_file_bytes
        self.maximum_type_tree_values = maximum_type_tree_values
        self.maximum_type_tree_array_elements = maximum_type_tree_array_elements
        self.maximum_type_tree_materialized_bytes = maximum_type_tree_materialized_bytes
        self.fs = fs
        self.path = _environment_base_path(sources, fs)
        stream_positions = (
            _capture_stream_positions(sources) if unity_version is None else {}
        )
        fallback_applied_during_open = False
        try:
            self._native = self._open_native(
                sources,
                fs=fs,
                unity_version=unity_version,
                maximum_files=maximum_files,
                maximum_file_bytes=maximum_file_bytes,
                maximum_total_bytes=maximum_total_bytes,
                oodle_decoder=oodle_decoder,
                skip_unreadable_inputs=skip_unreadable_inputs,
                unity_cn_key=unity_cn_key,
                strict_unity_versions=strict_unity_versions,
            )
        except NotImplementedError as error:
            if unity_version is not None or not _is_missing_unity_version_error(error):
                raise
            fallback_version = config._validated_fallback_version()
            _rewind_stream_sources(sources, stream_positions)
            fallback_native = self._open_native(
                sources,
                fs=fs,
                unity_version=fallback_version,
                maximum_files=maximum_files,
                maximum_file_bytes=maximum_file_bytes,
                maximum_total_bytes=maximum_total_bytes,
                oodle_decoder=oodle_decoder,
                skip_unreadable_inputs=skip_unreadable_inputs,
                unity_cn_key=unity_cn_key,
                strict_unity_versions=strict_unity_versions,
            )
            file_infos = fallback_native.files()
            missing_versions = [
                info for info in file_infos if _needs_unity_version_fallback(info.unity_version)
            ]
            if len(missing_versions) != len(file_infos):
                raise UnityVersionFallbackError(
                    "the loaded collection mixes valid and missing Unity versions; "
                    "UnityPy.config.FALLBACK_UNITY_VERSION cannot be applied without "
                    "overriding valid files, so load the inputs separately or pass an "
                    "explicit unity_version= override"
                ) from error
            config._warn_fallback_version(fallback_version)
            self._native = fallback_native
            fallback_applied_during_open = True
        if unity_version is None and not fallback_applied_during_open:
            file_infos = self._native.files()
            missing_versions = [
                info
                for info in file_infos
                if _needs_unity_version_fallback(info.effective_unity_version)
            ]
            if missing_versions:
                if len(missing_versions) != len(file_infos):
                    raise UnityVersionFallbackError(
                        "the loaded collection mixes valid and missing Unity versions; "
                        "UnityPy.config.FALLBACK_UNITY_VERSION cannot be applied without "
                        "overriding valid files, so load the inputs separately or pass an "
                        "explicit unity_version= override"
                    )
                fallback_version = config._validated_fallback_version()
                _rewind_stream_sources(sources, stream_positions)
                config._warn_fallback_version(fallback_version)
                self._native = self._open_native(
                    sources,
                    fs=fs,
                    unity_version=fallback_version,
                    maximum_files=maximum_files,
                    maximum_file_bytes=maximum_file_bytes,
                    maximum_total_bytes=maximum_total_bytes,
                    oodle_decoder=oodle_decoder,
                    skip_unreadable_inputs=skip_unreadable_inputs,
                    unity_cn_key=unity_cn_key,
                    strict_unity_versions=strict_unity_versions,
                )
        self._readers: Dict[Tuple[int, int], ObjectReader] = {}
        self.assets = [SerializedFile(self, info) for info in self._native.files()]
        self.files = {asset.path: asset for asset in self.assets}
        self.cabs: Dict[str, SerializedFile] = {}
        for asset in self.assets:
            self.register_cab(asset.path, asset)
        if len(self.assets) == 1:
            self.file = self.assets[0]
        self.container = ContainerHelper(self._container_entries())

    @staticmethod
    def _open_native(
        sources: Sequence[object],
        *,
        fs: Optional[object],
        unity_version: Optional[str],
        maximum_files: int,
        maximum_file_bytes: int,
        maximum_total_bytes: int,
        oodle_decoder: Optional[Callable[[bytes, int], bytes]],
        skip_unreadable_inputs: bool,
        unity_cn_key: Optional[Union[bytes, str]],
        strict_unity_versions: bool,
    ) -> UnityRs:
        if fs is not None:
            fs_memory_files = _read_fs_sources(
                fs,
                sources,
                maximum_files=maximum_files,
                maximum_file_bytes=maximum_file_bytes,
                maximum_total_bytes=maximum_total_bytes,
            )
            return UnityRs.from_memory_files(
                fs_memory_files,
                unity_version=unity_version,
                maximum_files=maximum_files,
                maximum_file_bytes=maximum_file_bytes,
                maximum_total_bytes=maximum_total_bytes,
                oodle_decoder=oodle_decoder,
                skip_unreadable_inputs=skip_unreadable_inputs,
                unity_cn_key=unity_cn_key,
                strict_unity_versions=strict_unity_versions,
            )
        if len(sources) == 1 and isinstance(sources[0], (str, os.PathLike)):
            source_path = Path(sources[0])
            if source_path.is_file():
                source_size = source_path.stat().st_size
                if source_size > maximum_file_bytes:
                    raise ValueError(
                        "input {} is {} bytes, exceeding maximum_file_bytes {}".format(
                            source_path, source_size, maximum_file_bytes
                        )
                    )
                if source_size > maximum_total_bytes:
                    raise ValueError(
                        "input {} is {} bytes, exceeding maximum_total_bytes {}".format(
                            source_path, source_size, maximum_total_bytes
                        )
                    )
            return UnityRs(
                source_path,
                unity_version=unity_version,
                maximum_input_files=maximum_files,
                maximum_expanded_bytes=maximum_total_bytes,
                maximum_single_entry_bytes=maximum_file_bytes,
                oodle_decoder=oodle_decoder,
                skip_unreadable_inputs=skip_unreadable_inputs,
                unity_cn_key=unity_cn_key,
                strict_unity_versions=strict_unity_versions,
            )

        if len(sources) > maximum_files:
            raise ValueError(
                "input count {} exceeds maximum_files {}".format(
                    len(sources), maximum_files
                )
            )
        memory_files: List[Tuple[str, bytes]] = []
        total = 0
        for index, source in enumerate(sources):
            name, data = _read_source(source, index, maximum_file_bytes)
            total += len(data)
            if total > maximum_total_bytes:
                raise ValueError(
                    "input bytes exceed maximum_total_bytes {}".format(
                        maximum_total_bytes
                    )
                )
            memory_files.append((name, data))
        return UnityRs.from_memory_files(
            memory_files,
            unity_version=unity_version,
            maximum_files=maximum_files,
            maximum_file_bytes=maximum_file_bytes,
            maximum_total_bytes=maximum_total_bytes,
            oodle_decoder=oodle_decoder,
            skip_unreadable_inputs=skip_unreadable_inputs,
            unity_cn_key=unity_cn_key,
            strict_unity_versions=strict_unity_versions,
        )

    @property
    def objects(self) -> List[ObjectReader]:
        self._check_object_materialization(self._native.object_count)
        output: List[ObjectReader] = []
        for asset in self.assets:
            output.extend(asset.objects.values())
        return output

    def get(self, key: str, default: object = None) -> object:
        return getattr(self, key, default)

    def get_cab(self, name: str) -> Optional[SerializedFile]:
        return self.cabs.get(_simplify_name(name))

    def register_cab(self, name: str, item: SerializedFile) -> None:
        if not isinstance(item, SerializedFile):
            raise TypeError("item must be a SerializedFile")
        if item.environment is not self:
            raise ValueError("cannot register a SerializedFile from another Environment")
        self.cabs[_simplify_name(name)] = item

    def find_file(
        self, name: str, is_dependency: bool = True
    ) -> Optional[SerializedFile]:
        del is_dependency
        item = self.get_cab(name)
        if item is not None:
            return item

        normalized_name = name.replace("\\", "/").lower()
        for path, asset in self.files.items():
            normalized_path = path.replace("\\", "/").lower()
            if normalized_path == normalized_name or normalized_path.endswith(
                "/" + normalized_name
            ):
                return asset
        raise FileNotFoundError(
            "File {} not found in {}".format(name, self.path or "loaded inputs")
        )

    def _check_object_materialization(self, count: int) -> None:
        if count > self.maximum_compat_objects:
            raise MemoryError(
                "UnityPy compatibility would materialize {} ObjectReader values, exceeding maximum_compat_objects {}; use the native paged API instead".format(
                    count, self.maximum_compat_objects
                )
            )

    def _reader(self, assets_file: SerializedFile, info: ObjectInfo) -> ObjectReader:
        key = (info.file_index, info.path_id)
        reader = self._readers.get(key)
        if reader is None:
            reader = ObjectReader(self, assets_file, info)
            self._readers[key] = reader
        return reader

    def _container_entries(self) -> List[Tuple[str, PPtr]]:
        entries: List[Tuple[str, PPtr]] = []
        for asset in self.assets:
            for key, pointer in asset.container.items():
                self._check_object_materialization(len(entries) + 1)
                entries.append((key, pointer))
        return entries

    def save(self, pack: str = "none", out_path: str = "output") -> None:
        del pack, out_path
        raise NotImplementedError(
            "editing and container repacking are not implemented by the read-focused unity-rs compatibility facade"
        )


load = Environment
AssetsManager = Environment


class SerializedFile:
    """UnityPy-style view of one parsed serialized file."""

    def __init__(self, environment: Environment, info: FileInfo) -> None:
        self.environment = environment
        self._info = info
        self.path = info.path
        self.name = os.path.basename(info.path.replace("\\", "/"))
        self.unity_version = info.effective_unity_version
        self.target_platform = info.target_platform
        self.version = info.format_version
        self.endian = "<" if info.endianness == 0 else ">"
        self.enable_type_tree = info.type_tree_enabled
        self.externals = [ExternalFile(path) for path in info.external_paths]
        self._objects: Optional[Dict[int, ObjectReader]] = None
        self._types: Optional[List[SerializedType]] = None
        self._ref_types: Optional[List[SerializedType]] = None
        self.container = ContainerHelper(self._container_entries())

    @property
    def objects(self) -> Dict[int, ObjectReader]:
        if self._objects is None:
            self.environment._check_object_materialization(self._info.object_count)
            objects: Dict[int, ObjectReader] = {}
            for info in self._iter_object_infos():
                objects[info.path_id] = self.environment._reader(self, info)
            self._objects = objects
        return self._objects

    @property
    def files(self) -> Dict[int, ObjectReader]:
        return self.objects

    @property
    def types(self) -> List[SerializedType]:
        if self._types is None:
            self._types = self._load_serialized_types(reference_types=False)
        return self._types

    @property
    def ref_types(self) -> Optional[List[SerializedType]]:
        if self.version < 20:
            return None
        if self._ref_types is None:
            self._ref_types = self._load_serialized_types(reference_types=True)
        return self._ref_types

    def _load_serialized_types(self, reference_types: bool) -> List[SerializedType]:
        output: List[SerializedType] = []
        dependencies = 0
        while True:
            remaining_types = self.environment.maximum_compat_types - len(output)
            limit = min(_OBJECT_PAGE_SIZE, remaining_types + 1)
            remaining_dependencies = (
                self.environment.maximum_type_dependencies - dependencies
            )
            rows = self.environment._native.serialized_type_page(
                self._info.index,
                reference_types=reference_types,
                offset=len(output),
                limit=limit,
                maximum_dependencies=remaining_dependencies,
                maximum_string_bytes=self.environment.maximum_type_tree_materialized_bytes,
            )
            if not rows:
                return output
            if len(output) + len(rows) > self.environment.maximum_compat_types:
                raise MemoryError(
                    "UnityPy compatibility would materialize more than maximum_compat_types {} serialized types".format(
                        self.environment.maximum_compat_types
                    )
                )
            for row in rows:
                dependencies += len(row[6])
                output.append(SerializedType(self, row, reference_types))

    def _iter_object_infos(self) -> Iterator[ObjectInfo]:
        offset = 0
        while offset < self._info.object_count:
            page = self.environment._native.object_page(
                self._info.index,
                offset=offset,
                limit=min(_OBJECT_PAGE_SIZE, self._info.object_count - offset),
            )
            if not page:
                raise RuntimeError("native object table ended before its declared count")
            for info in page:
                yield info
            offset += len(page)

    def _container_entries(self) -> List[Tuple[str, PPtr]]:
        for info in self._iter_object_infos():
            if info.class_id != int(ClassIDType.AssetBundle):
                continue
            bundle = self.environment._native.read_asset_bundle(
                info.file_index,
                info.path_id,
                maximum_entries=self.environment.maximum_compat_objects,
                maximum_string_bytes=self.environment.maximum_type_tree_materialized_bytes,
                maximum_total_string_bytes=self.environment.maximum_type_tree_materialized_bytes,
            )
            entries: List[Tuple[str, PPtr]] = []
            for key, _preload_index, _preload_size, asset in bundle.container:
                self.environment._check_object_materialization(len(entries) + 1)
                file_id, path_id = asset
                entries.append((key, PPtr(self, file_id, path_id)))
            return entries
        return []

    def save(self, packer: str = "none") -> bytes:
        del packer
        raise NotImplementedError(
            "serialized-file writing is not implemented by the read-focused unity-rs compatibility facade"
        )


AssetsFile = SerializedFile


class TypeTreeNode:
    """UnityPy-style view of one serialized TypeTree node."""

    def __init__(
        self,
        m_Level: int,
        m_Type: str,
        m_Name: str,
        m_ByteSize: int,
        m_Version: int,
        m_Children: Optional[List[TypeTreeNode]] = None,
        m_TypeFlags: Optional[int] = None,
        m_VariableCount: Optional[int] = None,
        m_Index: Optional[int] = None,
        m_MetaFlag: Optional[int] = None,
        m_RefTypeHash: Optional[int] = None,
    ) -> None:
        self.m_Level = m_Level
        self.m_Type = m_Type
        self.m_Name = m_Name
        self.m_ByteSize = m_ByteSize
        self.m_Version = m_Version
        self.m_Children = [] if m_Children is None else m_Children
        self.m_TypeFlags = m_TypeFlags
        self.m_VariableCount = m_VariableCount
        self.m_Index = m_Index
        self.m_MetaFlag = m_MetaFlag
        self.m_RefTypeHash = m_RefTypeHash
        self._clean_name = _clean_type_tree_name(m_Name)

    def traverse(
        self, maximum_nodes: int = _DEFAULT_MAXIMUM_TYPE_TREE_NODES
    ) -> Iterator[TypeTreeNode]:
        if maximum_nodes < 0:
            raise ValueError("maximum_nodes must be non-negative")
        stack = [self]
        seen: set[int] = set()
        while stack:
            node = stack.pop()
            identity = id(node)
            if identity in seen:
                raise ValueError("TypeTree contains a cycle or shared node")
            if len(seen) >= maximum_nodes:
                raise MemoryError(
                    "TypeTree exceeds maximum_nodes {}".format(maximum_nodes)
                )
            seen.add(identity)
            yield node
            stack.extend(reversed(node.m_Children))

    def dump_structure(
        self,
        indent: str = "  ",
        maximum_nodes: int = _DEFAULT_MAXIMUM_TYPE_TREE_NODES,
        maximum_bytes: int = _DEFAULT_MAXIMUM_FILE_BYTES,
    ) -> str:
        if maximum_nodes < 0:
            raise ValueError("maximum_nodes must be non-negative")
        if maximum_bytes < 0:
            raise ValueError("maximum_bytes must be non-negative")
        lines: List[str] = []
        stack: List[Tuple[TypeTreeNode, str]] = [(self, indent)]
        seen: set[int] = set()
        materialized_bytes = 0
        while stack:
            node, node_indent = stack.pop()
            identity = id(node)
            if identity in seen:
                raise ValueError("TypeTree contains a cycle or shared node")
            if len(seen) >= maximum_nodes:
                raise MemoryError(
                    "TypeTree exceeds maximum_nodes {}".format(maximum_nodes)
                )
            seen.add(identity)
            line = (
                "{}{} {} // ByteSize{{{:X}}}, Index{{{}}}, Version{{{}}}, TypeFlags{{{}}}, MetaFlag{{{}}}".format(
                    node_indent,
                    node.m_Type,
                    node.m_Name,
                    node.m_ByteSize,
                    node.m_Index,
                    node.m_Version,
                    node.m_TypeFlags,
                    node.m_MetaFlag,
                )
            )
            materialized_bytes += len(line.encode("utf-8"))
            if lines:
                materialized_bytes += 1
            if materialized_bytes > maximum_bytes:
                raise MemoryError(
                    "TypeTree structure exceeds maximum_bytes {}".format(
                        maximum_bytes
                    )
                )
            lines.append(line)
            child_indent = node_indent + "  "
            stack.extend(
                (child, child_indent) for child in reversed(node.m_Children)
            )
        return "\n".join(lines)

    def to_dict(self) -> Dict[str, Any]:
        return {
            key: value
            for key, value in (
                ("m_Level", self.m_Level),
                ("m_Type", self.m_Type),
                ("m_Name", self.m_Name),
                ("m_ByteSize", self.m_ByteSize),
                ("m_Version", self.m_Version),
                ("m_Children", self.m_Children),
                ("m_TypeFlags", self.m_TypeFlags),
                ("m_VariableCount", self.m_VariableCount),
                ("m_Index", self.m_Index),
                ("m_MetaFlag", self.m_MetaFlag),
                ("m_RefTypeHash", self.m_RefTypeHash),
            )
            if value is not None
        }

    def to_dict_list(
        self, maximum_nodes: int = _DEFAULT_MAXIMUM_TYPE_TREE_NODES
    ) -> List[Dict[str, Any]]:
        return [node.to_dict() for node in self.traverse(maximum_nodes)]

    def __repr__(self) -> str:
        return "TypeTreeNode(m_Level={}, m_Type={!r}, m_Name={!r}, m_MetaFlag={})".format(
            self.m_Level, self.m_Type, self.m_Name, self.m_MetaFlag
        )


class SerializedType:
    """Lazy UnityPy-compatible serialized type-table record."""

    index: int
    script_type_index: int

    def __init__(
        self,
        assets_file: SerializedFile,
        row: Tuple[
            int,
            int,
            bool,
            int,
            Optional[List[int]],
            Optional[List[int]],
            List[int],
            Optional[str],
            Optional[str],
            Optional[str],
        ],
        reference_type: bool = False,
    ) -> None:
        self.assets_file = assets_file
        self.index = row[0]
        self.class_id = row[1]
        self.is_stripped_type = row[2]
        self.script_type_index = row[3]
        self.script_id = None if row[4] is None else bytes(row[4])
        self.old_type_hash = None if row[5] is None else bytes(row[5])
        self.type_dependencies = (
            tuple(row[6])
            if assets_file.enable_type_tree
            and assets_file.version >= 21
            and not reference_type
            else None
        )
        self.m_ClassName = row[7]
        self.m_NameSpace = row[8]
        self.m_AssemblyName = row[9]
        self._reference_type = reference_type
        self._object_reader: Optional[ObjectReader] = None
        self._nodes_loaded = False
        self._nodes: Optional[List[TypeTreeNode]] = None

    @classmethod
    def from_object(cls, object_reader: ObjectReader) -> SerializedType:
        output = cls.__new__(cls)
        output.assets_file = object_reader.assets_file
        output.index = (
            -1
            if object_reader.serialized_type_index is None
            else object_reader.serialized_type_index
        )
        output.class_id = object_reader.class_id
        output.is_stripped_type = bool(object_reader.stripped)
        output.script_type_index = (
            -1
            if object_reader.script_type_index is None
            else object_reader.script_type_index
        )
        output.script_id = None
        output.old_type_hash = None
        output.type_dependencies = None
        output.m_ClassName = None
        output.m_NameSpace = None
        output.m_AssemblyName = None
        output._reference_type = False
        output._object_reader = object_reader
        output._nodes_loaded = False
        output._nodes = None
        return output

    @property
    def nodes(self) -> Optional[List[TypeTreeNode]]:
        if not self._nodes_loaded:
            try:
                if self._object_reader is not None:
                    rows = self._object_reader.environment._native.type_tree_nodes(
                        self._object_reader._info.file_index,
                        self._object_reader.path_id,
                    )
                else:
                    rows = self.assets_file.environment._native.serialized_type_tree_nodes(
                        self.assets_file._info.index,
                        self.index,
                        reference_type=self._reference_type,
                    )
            except NotImplementedError:
                self._nodes = None
            else:
                self._nodes = [_type_tree_node_from_row(row) for row in rows]
                _link_type_tree_nodes(self._nodes)
            self._nodes_loaded = True
        return self._nodes

    @property
    def node(self) -> Optional[TypeTreeNode]:
        nodes = self.nodes
        return None if not nodes else nodes[0]


def _link_type_tree_nodes(nodes: List[TypeTreeNode]) -> None:
    stack: List[TypeTreeNode] = []
    for node in nodes:
        node.m_Children.clear()
        while stack and stack[-1].m_Level >= node.m_Level:
            stack.pop()
        if stack:
            stack[-1].m_Children.append(node)
        stack.append(node)


class ObjectReader:
    """Lazy UnityPy-compatible handle for one serialized object."""

    def __init__(
        self, environment: Environment, assets_file: SerializedFile, info: ObjectInfo
    ) -> None:
        self.environment = environment
        self.assets_file = assets_file
        self._info = info
        self.path_id = info.path_id
        self.type_id = info.type_id
        self.class_id = info.class_id
        self.type = ClassIDType(info.class_id)
        self.byte_start = info.byte_start
        self.byte_size = info.byte_size
        self.serialized_type_index = info.serialized_type_index
        self.destroyed = info.destroyed
        self.stripped = info.stripped
        self.script_type_index = info.script_type_index
        self._serialized_type: Optional[SerializedType] = None
        self.container = info.container
        self.platform = assets_file.target_platform

    def get_raw_data(self, maximum_bytes: int = _DEFAULT_MAXIMUM_FILE_BYTES) -> bytes:
        return self.environment._native.read_raw(
            self._info.file_index,
            self.path_id,
            maximum_bytes=maximum_bytes,
        )

    @property
    def serialized_type(self) -> SerializedType:
        if self._serialized_type is None:
            index = self.serialized_type_index
            if index is not None and index < len(self.assets_file.types):
                self._serialized_type = self.assets_file.types[index]
            else:
                self._serialized_type = SerializedType.from_object(self)
        return self._serialized_type

    def peek_name(self) -> str:
        return self._info.name or ""

    def get(self, key: str, default: object = None) -> object:
        return getattr(self, key, default)

    def parse_as_dict(
        self,
        nodes: Optional[object] = None,
        check_read: bool = True,
    ) -> Dict[str, Any]:
        if not check_read:
            raise NotImplementedError(
                "check_read=False is incompatible with unity-rs complete-layout validation"
            )
        try:
            if nodes is None:
                value = self.environment._native.read_type_tree(
                    self._info.file_index,
                    self.path_id,
                    maximum_object_bytes=self.environment.maximum_object_bytes,
                    maximum_values=self.environment.maximum_type_tree_values,
                    maximum_array_elements=self.environment.maximum_type_tree_array_elements,
                    maximum_materialized_bytes=self.environment.maximum_type_tree_materialized_bytes,
                )
            else:
                rows = _caller_type_tree_rows(
                    nodes,
                    maximum_nodes=self.environment.maximum_type_tree_values,
                    maximum_string_bytes=self.environment.maximum_type_tree_materialized_bytes,
                )
                value = self.environment._native.read_type_tree_with_nodes(
                    self._info.file_index,
                    self.path_id,
                    rows,
                    maximum_object_bytes=self.environment.maximum_object_bytes,
                    maximum_values=self.environment.maximum_type_tree_values,
                    maximum_array_elements=self.environment.maximum_type_tree_array_elements,
                    maximum_materialized_bytes=self.environment.maximum_type_tree_materialized_bytes,
                )
        except (NotImplementedError, ValueError) as error:
            raise TypeTreeError(str(error)) from error
        if not isinstance(value, dict):
            raise TypeTreeError("the TypeTree root is not an object")
        return value

    def parse_as_object(
        self,
        nodes: Optional[object] = None,
        check_read: bool = True,
    ) -> Object:
        if not check_read:
            raise NotImplementedError(
                "check_read=False is incompatible with unity-rs complete-layout validation"
            )
        if nodes is None and self.class_id in _SPECIALIZED_OBJECTS:
            return _SPECIALIZED_OBJECTS[self.class_id](self)
        data = self.parse_as_dict(nodes=nodes, check_read=check_read)
        return _object_from_mapping(self, self.type.name, data)

    def read(self, check_read: bool = True) -> Object:
        return self.parse_as_object(check_read=check_read)

    def read_typetree(
        self,
        nodes: Optional[object] = None,
        wrap: bool = False,
        check_read: bool = True,
    ) -> Union[Dict[str, Any], Object]:
        if wrap:
            return self.parse_as_object(nodes=nodes, check_read=check_read)
        return self.parse_as_dict(nodes=nodes, check_read=check_read)

    def dump_typetree_structure(
        self,
        nodes: Optional[object] = None,
        indent: str = "  ",
    ) -> str:
        if nodes is None:
            root = self.serialized_type.node
            if root is None:
                raise TypeTreeError("the object has no type tree")
        else:
            rows = _caller_type_tree_rows(
                nodes,
                maximum_nodes=self.environment.maximum_type_tree_values,
                maximum_string_bytes=self.environment.maximum_type_tree_materialized_bytes,
            )
            normalized_nodes = [_type_tree_node_from_row(row) for row in rows]
            _link_type_tree_nodes(normalized_nodes)
            root = normalized_nodes[0]
        return root.dump_structure(
            indent=indent,
            maximum_nodes=self.environment.maximum_type_tree_values,
            maximum_bytes=self.environment.maximum_type_tree_materialized_bytes,
        )

    def __repr__(self) -> str:
        return "<{} {}>".format(self.__class__.__name__, self.type.name)

    def set_raw_data(self, data: bytes) -> None:
        del data
        raise NotImplementedError(
            "object editing is not implemented by the read-focused unity-rs compatibility facade"
        )

    def patch(self, data: object) -> bytes:
        del data
        raise NotImplementedError(
            "TypeTree patching is not implemented by the read-focused unity-rs compatibility facade"
        )

    def save_typetree(self, data: object) -> bytes:
        return self.patch(data)


_MISSING = object()


def _simplify_name(name: str) -> str:
    if not isinstance(name, str):
        raise TypeError("name must be a string")
    return ntpath.basename(name).lower()


def _clean_type_tree_name(name: str) -> str:
    if not name:
        return name
    if name.startswith("(int&)"):
        name = name[6:]
    if name.endswith("?"):
        name = name[:-1]
    name = re.sub(r"[ .:\-\[\]]", "_", name)
    if name in ("pass", "from"):
        name += "_"
    if name[0].isdigit():
        name = "x" + name
    return name


def _type_tree_node_from_row(
    row: Tuple[str, str, int, int, int, int, int, int, int]
) -> TypeTreeNode:
    return TypeTreeNode(
        m_Level=row[7],
        m_Type=row[0],
        m_Name=row[1],
        m_ByteSize=row[2],
        m_Version=row[5],
        m_TypeFlags=row[4],
        m_Index=row[3],
        m_MetaFlag=row[6],
        m_RefTypeHash=row[8],
    )


def _caller_type_tree_rows(
    nodes: object,
    *,
    maximum_nodes: int,
    maximum_string_bytes: int,
) -> List[Tuple[str, str, int, int, int, int, int, int, int]]:
    """Flatten UnityPy nodes without trusting their size or field types."""

    if isinstance(nodes, list):
        iterator = iter(nodes)
    else:
        traverse = getattr(nodes, "traverse", None)
        if not callable(traverse):
            raise TypeError("nodes must be a list[dict or TypeTreeNode] or TypeTreeNode")
        iterator = iter(traverse())

    rows: List[Tuple[str, str, int, int, int, int, int, int, int]] = []
    string_bytes = 0
    for index, node in enumerate(iterator):
        if index >= maximum_nodes:
            raise MemoryError(
                "caller-supplied TypeTree exceeds maximum_type_tree_values {}".format(
                    maximum_nodes
                )
            )
        type_name = _caller_node_value(node, "m_Type")
        field_name = _caller_node_value(node, "m_Name")
        if not isinstance(type_name, str) or not isinstance(field_name, str):
            raise TypeError(
                "caller-supplied TypeTree node {} names must be strings".format(index)
            )
        string_bytes += len(type_name.encode("utf-8")) + len(
            field_name.encode("utf-8")
        )
        if string_bytes > maximum_string_bytes:
            raise MemoryError(
                "caller-supplied TypeTree strings exceed maximum_type_tree_materialized_bytes {}".format(
                    maximum_string_bytes
                )
            )
        rows.append(
            (
                type_name,
                field_name,
                _caller_node_int(node, "m_ByteSize", index, 0),
                _caller_node_int(node, "m_Index", index, index),
                _caller_node_int(node, "m_TypeFlags", index, 0),
                _caller_node_int(node, "m_Version", index, 0),
                _caller_node_int(node, "m_MetaFlag", index, 0),
                _caller_node_int(node, "m_Level", index),
                _caller_node_int(node, "m_RefTypeHash", index, 0),
            )
        )
    if not rows:
        raise ValueError("caller-supplied TypeTree must contain a root node")
    return rows


def _caller_node_value(
    node: object, name: str, default: object = _MISSING
) -> object:
    if isinstance(node, Mapping):
        value = node.get(name, default)
    else:
        value = getattr(node, name, default)
    if value is _MISSING:
        raise ValueError("caller-supplied TypeTree node is missing {}".format(name))
    return value


def _caller_node_int(
    node: object,
    name: str,
    index: int,
    default: object = _MISSING,
) -> int:
    value = _caller_node_value(node, name, default)
    if value is None and default is not _MISSING:
        value = default
    if not isinstance(value, int):
        raise TypeError(
            "caller-supplied TypeTree node {} field {} must be an integer".format(
                index, name
            )
        )
    return value


class PPtr:
    """Unity object reference with local and external-file resolution."""

    def __init__(self, assets_file: SerializedFile, file_id: int, path_id: int) -> None:
        self.assets_file = assets_file
        self.m_FileID = file_id
        self.m_PathID = path_id

    @property
    def file_id(self) -> int:
        return self.m_FileID

    @property
    def path_id(self) -> int:
        return self.m_PathID

    @property
    def type(self) -> ClassIDType:
        reader = self.deref()
        if reader is None:
            raise ValueError("PPtr can't resolve a null object reference")
        return reader.type

    def deref(self) -> Optional[ObjectReader]:
        resolved = self.assets_file.environment._native.resolve_pptr(
            self.assets_file._info.index,
            self.m_FileID,
            self.m_PathID,
        )
        if resolved is None:
            return None
        file_index, object_index, path_id = resolved
        del object_index
        try:
            target = self.assets_file.environment.assets[file_index]
        except IndexError as error:
            raise FileNotFoundError(
                "resolved PPtr file index {} is unavailable".format(file_index)
            ) from error
        return target.objects[path_id]

    def deref_parse_as_object(self) -> Optional[Object]:
        reader = self.deref()
        return None if reader is None else reader.parse_as_object()

    def deref_parse_as_dict(self) -> Optional[Dict[str, Any]]:
        reader = self.deref()
        return None if reader is None else reader.parse_as_dict()

    def parse_as_object(self) -> Optional[Object]:
        return self.deref_parse_as_object()

    def parse_as_dict(self) -> Optional[Dict[str, Any]]:
        return self.deref_parse_as_dict()

    def read(self) -> Optional[Object]:
        return self.deref_parse_as_object()

    def read_typetree(self) -> Optional[Dict[str, Any]]:
        return self.deref_parse_as_dict()

    def __bool__(self) -> bool:
        return self.m_FileID >= 0 and self.m_PathID != 0

    def __hash__(self) -> int:
        return hash((self.m_FileID, self.m_PathID))

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, PPtr):
            return NotImplemented
        return (self.m_FileID, self.m_PathID) == (
            other.m_FileID,
            other.m_PathID,
        )


class ContainerHelper:
    """Read-only multidict used by UnityPy's ``container`` properties."""

    def __init__(self, entries: Sequence[Tuple[str, PPtr]]) -> None:
        self._entries = list(entries)
        self._collapsed = dict(entries)

    def __getitem__(self, key: str) -> PPtr:
        return self._collapsed[key]

    def __iter__(self) -> Iterator[str]:
        for key, _ in self._entries:
            yield key

    def __len__(self) -> int:
        return len(self._entries)

    def items(self) -> Iterator[Tuple[str, PPtr]]:
        return iter(self._entries)

    def keys(self) -> Iterator[str]:
        return (key for key, _ in self._entries)

    def values(self) -> Iterator[PPtr]:
        return (value for _, value in self._entries)

    def __setitem__(self, key: str, value: PPtr) -> None:
        del key, value
        raise NotImplementedError("UnityPy containers are read-only")

    def __delitem__(self, key: str) -> None:
        del key
        raise NotImplementedError("UnityPy containers are read-only")


class Object:
    """Base for parsed UnityPy-compatible objects."""

    def __init__(self, object_reader: ObjectReader) -> None:
        self.object_reader = object_reader
        self.assets_file = object_reader.assets_file

    def save(self) -> None:
        raise NotImplementedError(
            "object editing is not implemented by the read-focused unity-rs compatibility facade"
        )


class UnknownObject(Object):
    pass


class TextAsset(Object):
    def __init__(self, object_reader: ObjectReader) -> None:
        super().__init__(object_reader)
        self.m_Name = object_reader.peek_name()
        script = object_reader.environment._native.read_text(
            object_reader._info.file_index,
            object_reader.path_id,
        )
        self.m_Script = script.decode("utf-8", errors="surrogateescape")


class _ImageObject(Object):
    def __init__(self, object_reader: ObjectReader) -> None:
        super().__init__(object_reader)
        self.m_Name = object_reader.peek_name()
        self._rgba_image: Optional[RgbaImage] = None

    def _decode(self) -> RgbaImage:
        raise NotImplementedError

    @property
    def image(self) -> Any:
        try:
            image_module = import_module("PIL.Image")
        except ImportError as error:
            raise ImportError(
                "Pillow is required for UnityPy-compatible .image; install Pillow separately"
            ) from error
        image = self._decode()
        return image_module.frombytes(
            "RGBA", (image.width, image.height), image.rgba
        )


class Texture2D(_ImageObject):
    def _decode(self) -> RgbaImage:
        if self._rgba_image is None:
            self._rgba_image = self.object_reader.environment._native.read_texture(
                self.object_reader._info.file_index,
                self.object_reader.path_id,
            )
        return self._rgba_image

    @property
    def m_Width(self) -> int:
        return self._decode().width

    @property
    def m_Height(self) -> int:
        return self._decode().height


class Sprite(_ImageObject):
    def _decode(self) -> RgbaImage:
        if self._rgba_image is None:
            self._rgba_image = self.object_reader.environment._native.read_sprite(
                self.object_reader._info.file_index,
                self.object_reader.path_id,
            )
        return self._rgba_image


class AudioClip(Object):
    def __init__(self, object_reader: ObjectReader) -> None:
        super().__init__(object_reader)
        self._audio = object_reader.environment._native.read_audio_clip(
            object_reader._info.file_index,
            object_reader.path_id,
        )
        self.m_Name = self._audio.name

    @property
    def extension(self) -> str:
        return self._audio.extension

    @property
    def samples(self) -> Dict[str, bytes]:
        name = self.m_Name
        if not name.lower().endswith(self.extension.lower()):
            name += self.extension
        return {name: self._audio.data}


class Mesh(Object):
    def __init__(self, object_reader: ObjectReader) -> None:
        super().__init__(object_reader)
        self.m_Name = object_reader.peek_name()

    def export(self) -> str:
        data = self.object_reader.environment._native.read_mesh_obj(
            self.object_reader._info.file_index,
            self.object_reader.path_id,
        )
        return data.decode("utf-8")


class Shader(Object):
    def __init__(self, object_reader: ObjectReader) -> None:
        super().__init__(object_reader)
        self.m_Name = object_reader.peek_name()

    def export(self) -> str:
        data = self.object_reader.environment._native.read_shader(
            self.object_reader._info.file_index,
            self.object_reader.path_id,
        )
        return data.decode("utf-8", errors="surrogateescape")


class Font(Object):
    def __init__(self, object_reader: ObjectReader) -> None:
        super().__init__(object_reader)
        asset = object_reader.environment._native.read_font(
            object_reader._info.file_index,
            object_reader.path_id,
        )
        self.m_Name = asset.name
        self.m_FontData = asset.data


_SPECIALIZED_OBJECTS: Dict[int, Type[Object]] = {
    int(ClassIDType.TextAsset): TextAsset,
    int(ClassIDType.Texture2D): Texture2D,
    int(ClassIDType.Sprite): Sprite,
    int(ClassIDType.AudioClip): AudioClip,
    int(ClassIDType.Mesh): Mesh,
    int(ClassIDType.Shader): Shader,
    int(ClassIDType.Font): Font,
}
_DYNAMIC_OBJECT_TYPES: Dict[str, Type[Object]] = {}


def _object_from_mapping(
    reader: ObjectReader, class_name: str, data: Dict[str, Any]
) -> Object:
    object_type = _DYNAMIC_OBJECT_TYPES.get(class_name)
    if object_type is None:
        object_type = cast(Type[Object], type(class_name, (UnknownObject,), {}))
        _DYNAMIC_OBJECT_TYPES[class_name] = object_type
    output = object_type(reader)
    for key, value in data.items():
        setattr(output, key, _objectify(reader.assets_file, value))
    return output


def _objectify(assets_file: SerializedFile, value: Any) -> Any:
    if isinstance(value, dict):
        if "m_FileID" in value and "m_PathID" in value:
            file_id = value["m_FileID"]
            path_id = value["m_PathID"]
            if isinstance(file_id, int) and isinstance(path_id, int):
                return PPtr(assets_file, file_id, path_id)
        record = _DynamicRecord()
        for key, child in value.items():
            setattr(record, key, _objectify(assets_file, child))
        return record
    if isinstance(value, list):
        return [_objectify(assets_file, child) for child in value]
    if isinstance(value, tuple):
        return tuple(_objectify(assets_file, child) for child in value)
    return value


class _DynamicRecord:
    pass


def _needs_unity_version_fallback(version: str) -> bool:
    return not version or version == "0.0.0"


def _is_missing_unity_version_error(error: NotImplementedError) -> bool:
    return "requires a Unity version" in str(error)


def _capture_stream_positions(sources: Sequence[object]) -> Dict[int, int]:
    positions: Dict[int, int] = {}
    for index, source in enumerate(sources):
        if isinstance(source, (bytes, bytearray, memoryview, str, os.PathLike)):
            continue
        if not hasattr(source, "read"):
            continue
        tell = getattr(source, "tell", None)
        seek = getattr(source, "seek", None)
        if not callable(tell) or not callable(seek):
            continue
        try:
            position = tell()
        except (OSError, ValueError):
            continue
        if isinstance(position, int) and position >= 0:
            positions[index] = position
    return positions


def _rewind_stream_sources(
    sources: Sequence[object], positions: Dict[int, int]
) -> None:
    for index, source in enumerate(sources):
        if isinstance(source, (bytes, bytearray, memoryview, str, os.PathLike)):
            continue
        if not hasattr(source, "read"):
            continue
        position = positions.get(index)
        seek = getattr(source, "seek", None)
        if position is None or not callable(seek):
            raise UnityVersionFallbackError(
                "UnityPy.config.FALLBACK_UNITY_VERSION requires a second bounded "
                "read for a versionless binary stream; pass unity_version= explicitly "
                "or provide a seekable stream"
            )
        try:
            seek(position)
        except (OSError, ValueError) as error:
            raise UnityVersionFallbackError(
                "could not rewind a versionless binary stream for "
                "UnityPy.config.FALLBACK_UNITY_VERSION"
            ) from error


def _read_source(source: object, index: int, maximum_bytes: int) -> Tuple[str, bytes]:
    if maximum_bytes < 0:
        raise ValueError("maximum_file_bytes must be non-negative")
    if isinstance(source, bytes):
        data = source
        name = "memory-{}.assets".format(index)
    elif isinstance(source, (bytearray, memoryview)):
        data = bytes(source)
        name = "memory-{}.assets".format(index)
    elif isinstance(source, (str, os.PathLike)):
        path = Path(source)
        if path.is_dir():
            raise NotImplementedError(
                "directories can only be passed as the sole UnityPy source"
            )
        name = path.name
        with path.open("rb") as path_stream:
            data = _read_stream_bounded(path_stream, maximum_bytes)
    elif hasattr(source, "read"):
        raw_name = getattr(source, "name", "memory-{}.assets".format(index))
        name = os.path.basename(os.fspath(raw_name))
        data = _read_stream_bounded(cast(BinaryIO, source), maximum_bytes)
    else:
        raise TypeError(
            "UnityPy source must be a path, bytes-like object, or bounded binary stream"
        )
    if len(data) > maximum_bytes:
        raise ValueError(
            "input {} is {} bytes, exceeding maximum_file_bytes {}".format(
                name, len(data), maximum_bytes
            )
        )
    return name, data


def _environment_base_path(sources: Sequence[object], fs: Optional[object]) -> str:
    if len(sources) != 1 or not isinstance(sources[0], (str, os.PathLike)):
        return "" if fs is not None else os.getcwd()
    source = os.fspath(sources[0])
    if not isinstance(source, str):
        raise TypeError("UnityPy paths must resolve to strings")
    if fs is not None:
        isdir = getattr(fs, "isdir", None)
        if callable(isdir) and bool(isdir(source)):
            return source
        separator = getattr(fs, "sep", "/")
        if isinstance(separator, str) and separator and separator in source:
            return source.rsplit(separator, 1)[0] or separator
        return "."
    path = Path(source)
    return os.fspath(path if path.is_dir() else path.parent)


def _read_fs_sources(
    fs: object,
    sources: Sequence[object],
    *,
    maximum_files: int,
    maximum_file_bytes: int,
    maximum_total_bytes: int,
) -> List[Tuple[str, bytes]]:
    if maximum_files < 0:
        raise ValueError("maximum_files must be non-negative")
    for name, value in (
        ("maximum_file_bytes", maximum_file_bytes),
        ("maximum_total_bytes", maximum_total_bytes),
    ):
        if value < 0:
            raise ValueError("{} must be non-negative".format(name))
    isfile = getattr(fs, "isfile", None)
    isdir = getattr(fs, "isdir", None)
    open_file = getattr(fs, "open", None)
    if not callable(isfile) or not callable(isdir) or not callable(open_file):
        raise TypeError("fs must provide callable isfile(), isdir(), and open() methods")

    paths: List[str] = []
    for source in sources:
        if not isinstance(source, (str, os.PathLike)):
            raise TypeError("fs= sources must be string or path-like paths")
        path = os.fspath(source)
        if not isinstance(path, str):
            raise TypeError("fs= paths must resolve to strings")
        if bool(isfile(path)):
            _append_fs_path(paths, path, maximum_files)
        elif bool(isdir(path)):
            _append_fs_directory(fs, path, paths, maximum_files)
        else:
            raise FileNotFoundError("virtual filesystem path was not found: {}".format(path))

    output: List[Tuple[str, bytes]] = []
    total = 0
    for path in paths:
        stream = open_file(path, "rb")
        try:
            if not hasattr(stream, "read"):
                raise TypeError("fs.open() must return a binary stream")
            data = _read_stream_bounded(cast(BinaryIO, stream), maximum_file_bytes)
        finally:
            close = getattr(stream, "close", None)
            if callable(close):
                close()
        total += len(data)
        if total > maximum_total_bytes:
            raise ValueError(
                "virtual filesystem inputs exceed maximum_total_bytes {}".format(
                    maximum_total_bytes
                )
            )
        output.append((path, data))
    return output


def _append_fs_path(paths: List[str], path: str, maximum_files: int) -> None:
    if len(paths) >= maximum_files:
        raise ValueError(
            "virtual filesystem input count exceeds maximum_files {}".format(
                maximum_files
            )
        )
    paths.append(path)


def _append_fs_directory(
    fs: object,
    path: str,
    paths: List[str],
    maximum_files: int,
) -> None:
    walk = getattr(fs, "walk", None)
    if not callable(walk):
        raise TypeError("fs must provide walk() for directory sources")
    separator = getattr(fs, "sep", "/")
    if not isinstance(separator, str) or not separator:
        raise TypeError("fs.sep must be a non-empty string")
    for entry in walk(path):
        if not isinstance(entry, (tuple, list)) or len(entry) != 3:
            raise TypeError("fs.walk() must yield (root, directories, files) triples")
        root, _directories, files = entry
        if not isinstance(root, (str, os.PathLike)):
            raise TypeError("fs.walk() roots must be string or path-like values")
        try:
            iterator = iter(files)
        except TypeError as error:
            raise TypeError("fs.walk() files must be iterable") from error
        raw_root = os.fspath(root)
        if not isinstance(raw_root, str):
            raise TypeError("fs.walk() roots must resolve to strings")
        root_name = raw_root.rstrip(separator)
        for file_name in iterator:
            if not isinstance(file_name, (str, os.PathLike)):
                raise TypeError("fs.walk() file names must be string or path-like values")
            leaf = os.fspath(file_name)
            if not isinstance(leaf, str):
                raise TypeError("fs.walk() file names must resolve to strings")
            full_path = separator.join(part for part in (root_name, leaf) if part)
            _append_fs_path(paths, full_path, maximum_files)


def _read_stream_bounded(stream: BinaryIO, maximum_bytes: int) -> bytes:
    requested = maximum_bytes + 1
    data = stream.read(requested)
    if not isinstance(data, bytes):
        raise TypeError("binary stream read() must return bytes")
    if len(data) > maximum_bytes:
        raise ValueError(
            "binary stream exceeds maximum_file_bytes {}".format(maximum_bytes)
        )
    return data


def set_assetbundle_decrypt_key(key: bytes) -> None:
    del key
    raise NotImplementedError(
        "global decryption keys are not stored; pass unity_cn_key explicitly to load()"
    )


__all__ = [
    "__version__",
    "AssetsFile",
    "AssetsManager",
    "AudioClip",
    "ClassIDType",
    "ContainerHelper",
    "Environment",
    "ExternalFile",
    "Font",
    "Mesh",
    "Object",
    "ObjectReader",
    "PPtr",
    "SerializedFile",
    "SerializedType",
    "Shader",
    "Sprite",
    "TextAsset",
    "Texture2D",
    "TypeTreeError",
    "TypeTreeNode",
    "UNITYPY_COMPAT_VERSION",
    "UnityVersionFallbackError",
    "UnityVersionFallbackWarning",
    "UnknownObject",
    "config",
    "load",
    "set_assetbundle_decrypt_key",
]
