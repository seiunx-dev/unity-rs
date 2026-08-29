"""Read-focused compatibility facade for UnityPy 1.25.x.

The facade deliberately lives below :mod:`unity_rs` so installing the main
wheel never shadows a real ``UnityPy`` installation.  It mirrors UnityPy's
object graph while keeping parsing, resource resolution, and limits in the
native ``unity-rs`` implementation.
"""

from __future__ import annotations

import os
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
_OBJECT_PAGE_SIZE = 4_096


class TypeTreeError(ValueError):
    """The object has no verified TypeTree or its tree does not match."""


class UnityVersionFallbackError(ValueError):
    """A stripped Unity version requires an explicit caller override."""


class UnityVersionFallbackWarning(UserWarning):
    """Compatibility warning for an explicitly configured fallback version."""


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
        self.maximum_object_bytes = maximum_file_bytes
        self.maximum_type_tree_values = maximum_type_tree_values
        self.maximum_type_tree_array_elements = maximum_type_tree_array_elements
        self.maximum_type_tree_materialized_bytes = maximum_type_tree_materialized_bytes
        self.fs = fs
        self.path = _environment_base_path(sources, fs)
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
        self._readers: Dict[Tuple[int, int], ObjectReader] = {}
        self.assets = [SerializedFile(self, info) for info in self._native.files()]
        self.files = {asset.path: asset for asset in self.assets}
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
            for info in asset._iter_object_infos():
                if info.container is not None:
                    self._check_object_materialization(len(entries) + 1)
                    entries.append(
                        (
                            info.container,
                            PPtr(asset, 0, info.path_id),
                        )
                    )
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
    def types(self) -> object:
        raise NotImplementedError(
            "serialized type-table records are not exposed by this compatibility phase"
        )

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
        entries: List[Tuple[str, PPtr]] = []
        for info in self._iter_object_infos():
            if info.container is not None:
                self.environment._check_object_materialization(len(entries) + 1)
                entries.append((info.container, PPtr(self, 0, info.path_id)))
        return entries

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
        type_name: str,
        field_name: str,
        byte_size: int,
        index: int,
        type_flags: int,
        version: int,
        meta_flags: int,
        level: int,
        reference_type_hash: int,
    ) -> None:
        self.m_Type = type_name
        self.m_Name = field_name
        self.m_ByteSize = byte_size
        self.m_Index = index
        self.m_TypeFlags = type_flags
        self.m_Version = version
        self.m_MetaFlag = meta_flags
        self.m_Level = level
        self.m_RefTypeHash = reference_type_hash


class SerializedType:
    """Lazy object type metadata attached to an :class:`ObjectReader`."""

    def __init__(self, object_reader: ObjectReader) -> None:
        self.class_id = object_reader.class_id
        self.is_stripped_type = bool(object_reader.stripped)
        self.script_type_index = object_reader.script_type_index
        self._object_reader = object_reader
        self._nodes_loaded = False
        self._nodes: Optional[List[TypeTreeNode]] = None

    @property
    def nodes(self) -> Optional[List[TypeTreeNode]]:
        if not self._nodes_loaded:
            try:
                rows = self._object_reader.environment._native.type_tree_nodes(
                    self._object_reader._info.file_index,
                    self._object_reader.path_id,
                )
            except NotImplementedError:
                self._nodes = None
            else:
                self._nodes = [TypeTreeNode(*row) for row in rows]
            self._nodes_loaded = True
        return self._nodes


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
        self.serialized_type = SerializedType(self)
        self.container = info.container
        self.platform = assets_file.target_platform

    def get_raw_data(self, maximum_bytes: int = _DEFAULT_MAXIMUM_FILE_BYTES) -> bytes:
        return self.environment._native.read_raw(
            self._info.file_index,
            self.path_id,
            maximum_bytes=maximum_bytes,
        )

    def peek_name(self) -> str:
        return self._info.name or ""

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
        check_read: bool = True,
    ) -> Dict[str, Any]:
        return self.parse_as_dict(nodes=nodes, check_read=check_read)

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
    "load",
    "set_assetbundle_decrypt_key",
]
