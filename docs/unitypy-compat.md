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
- path, directory, bytes-like, bounded binary-stream and multiple-source input;
- `Environment.files`, `.assets`, `.objects`, `.container`, and single-file
  `.file`;
- `SerializedFile.objects` keyed by PathID, `.files` legacy alias, external
  paths, platform/version metadata and a read-only container view;
- lazy `ObjectReader` metadata, `get_raw_data`, `peek_name`,
  `parse_as_dict` / `read_typetree`, and `parse_as_object` / `read`;
- lazy `serialized_type.nodes` metadata for objects with embedded TypeTrees;
- local and external `PPtr` resolution, null handling, equality and legacy read
  aliases;
- direct embedded-TypeTree conversion with UnityPy value shapes: `char` is an
  integer, `TypelessData` is `bytes`, and maps are ordered lists of tuples;
- common `TextAsset`, `Texture2D`, `Sprite`, `AudioClip`, `Mesh`, `Shader` and
  `Font` conveniences. Pillow is imported only when `.image` is requested.

Compatibility properties that materialize Python lists, dictionaries or
TypeTree values have explicit caller-adjustable limits. Native parsing remains
strict about complete known layouts; `check_read=False` is rejected rather
than weakening validation.

## Explicit boundaries

- Generic TypeTree reads require a tree embedded in the serialized file.
  UnityPy's bundled TPK database is not redistributed. Caller-supplied UnityPy
  node lists and a validated external TypeTree provider remain future work.
- `fs=` is not accepted yet. Passing it raises `NotImplementedError`.
- The loader currently exposes discovered serialized files rather than exact
  UnityPy `BundleFile` / `WebFile` provenance objects for nested containers.
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
