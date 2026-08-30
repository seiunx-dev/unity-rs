# UnityPy compatibility facade

`unity_rs.compat.unitypy` is a read-focused compatibility facade pinned to the
public behavior of UnityPy 1.25.3. It intentionally does not install a top-level
`UnityPy` package: a real UnityPy installation remains importable for
differential testing and applications can opt in explicitly:

```python
from unity_rs.compat import unitypy as UnityPy

environment = UnityPy.load("game_Data")
```

The target is source-compatible asset inspection and extraction, not a claim
that every UnityPy implementation detail or generated class is reproduced.

## Implemented read contract

- `load` / `Environment` and the legacy `AssetsManager` alias;
- `UnityPy.config.FALLBACK_UNITY_VERSION` for files whose serialized and
  enclosing-bundle versions are both missing, including UnityPy-compatible
  error and warning categories; an explicit `unity_version=` still wins;
- path, directory, bytes-like, bounded binary-stream and multiple-source input;
- read-only fsspec-style `fs=` file and recursive-directory input through
  `isfile`, `isdir`, `walk`, and `open`, without a mandatory fsspec dependency;
- `Environment.files`, `.assets`, `.objects`, `.container`, single-file `.file`,
  CAB registration, and case-insensitive already-loaded lookup through
  `get_cab` / `find_file`;
- `SerializedFile.objects` keyed by PathID, `.files` legacy alias, external
  paths, platform/version metadata and a read-only container view;
- paged, bounded `SerializedFile.types` / `.ref_types` records with hashes,
  dependency metadata, reference-type identities, lazy flat `.nodes`, and a
  linked `.node` root compatible with UnityPy traversal;
- lazy `ObjectReader` metadata, `get`, `get_raw_data`, `peek_name`,
  `parse_as_dict`, `parse_as_object` / `read`, and UnityPy's
  `read_typetree(nodes=None, wrap=False, check_read=True)` call shape;
- `TypeTreeNode.traverse`, `dump_structure`, `to_dict`, `to_dict_list`, and
  `ObjectReader.dump_typetree_structure` for supplied or embedded trees;
- lazy `serialized_type.nodes` metadata for objects with embedded TypeTrees;
- local and external `PPtr` resolution, null handling, equality and legacy read
  aliases;
- direct embedded-TypeTree conversion with UnityPy value shapes: `char` is an
  integer, `TypelessData` is `bytes`, and maps are ordered lists of tuples;
- caller-supplied UnityPy `TypeTreeNode` roots and flat lists of UnityPy nodes
  or node dictionaries, including reads of files without embedded trees;
- common `TextAsset`, `Texture2D`, `Sprite`, `AudioClip`, `Mesh`, `Shader` and
  `Font` conveniences. Pillow is imported only when `.image` is requested.

The specialized export-facing objects keep UnityPy's usual call shapes:

| Unity object | Compatibility surface | Native work performed by `unity-rs` |
| --- | --- | --- |
| `Texture2D`, `Sprite` | `.image` | Bounded RGBA decode, exposed as a Pillow image on demand |
| `AudioClip` | `.samples` | Bounded embedded or streamed payload resolution |
| `Mesh` | `.export()` | Bounded OBJ materialization |
| `Shader` | `.export()` | Bounded shader-text materialization |
| `Font` | `.m_FontData` | Bounded embedded or streamed font payload resolution |

For example:

```python
from pathlib import Path
from unity_rs.compat import unitypy as UnityPy

environment = UnityPy.load("model.bundle")
for reader in environment.objects:
    if reader.type is UnityPy.ClassIDType.Mesh:
        mesh = reader.read()
        Path(mesh.m_Name + ".obj").write_text(mesh.export(), encoding="utf-8")
```

Compatibility properties that materialize Python lists, dictionaries or
TypeTree values have explicit caller-adjustable limits. Native parsing remains
strict about complete known layouts; `check_read=False` is rejected rather
than weakening validation.

## Explicit boundaries

- UnityPy's bundled TPK database is not redistributed. Applications may pass a
  complete tree obtained from their own trusted provider to `read_typetree` or
  `parse_as_dict`; malformed, partial, oversized, or non-consuming trees are
  rejected. Automatic external TypeTree-provider registration remains future
  work.
- `fs=` is a read-only loading adapter. `find_file` resolves only serialized
  files admitted during `Environment` construction; it does not mutate the
  native collection or lazily discover a new dependency after load. Other
  mutating filesystem methods and writing through the filesystem are not part
  of this phase. Virtual file enumeration and callback results remain subject
  to the same file-count and byte budgets as local inputs.
- The loader currently exposes discovered serialized files rather than exact
  UnityPy `BundleFile` / `WebFile` provenance objects for nested containers.
- A collection mixing valid and versionless serialized files cannot apply the
  global fallback without corrupting the valid files through the native global
  override. The facade rejects that ambiguous case and asks the caller to load
  the roots separately or use an intentional `unity_version=` override.
  Versionless non-seekable streams likewise need an explicit override because
  fallback detection requires a bounded first read before reopening.
- `set_raw_data`, `patch`, `save_typetree`, parsed-object `save`, serialized
  file `save`, environment `save`, and bundle repacking all raise
  `NotImplementedError`. Implementing them requires a bounded serialized-file
  writer and container repacker; they are not emulated with partial output.
- The package supports Python 3.9+, matching `unity-rs`, even though UnityPy
  itself also supports Python 3.8.

## Verification

The regular Python wheel and sdist tests exercise the object graph, local and
external PPtrs, duplicate container paths, missing-tree failures, byte limits,
legacy aliases and structured TypeTree shapes. The optional `unitypy` local-CI
group additionally compares the facade and native API with an independently
installed UnityPy across the repository's synthetic serialized-file corpus:

```shell
python3 tools/local_ci.py --fail-on-skip unitypy
```
