# unity-rs for Python

`assetstudio-rs` is the compatibility distribution name of the native Python
package for `unity-rs`.
It binds directly to the safe, bounded `assetstudio-core::studio::Studio` API
through PyO3; it does not load or call the legacy C ABI.

The initial API supports:

- Unity asset, bundle, web-container, split-file, and directory loading;
- direct bounded in-memory asset, bundle, and web-container loading with
  `AssetStudio.from_bytes(...)`;
- deterministic lazy or bounded-paged serialized-file and object enumeration;
- deterministic lazy or bounded-paged external-resource enumeration plus
  bounded reads by stable index or portable path;
- safe recursive bundle/web/ZIP/gzip/Brotli extraction with cumulative limits,
  no-follow paths, and atomic publication;
- optional user-supplied Oodle block decoding for loading and extraction,
  exposed as an exact-size Python callable without linking or redistributing a
  proprietary Oodle library;
- bounded collection-wide `GameObject` hierarchy, managed-compatible
  `SplitObjects`/Animator candidate planning, and model bindings;
- bounded ASCII FBX 7.4 generation, including direct or bone-name-hash-recovered skin clusters, static blend shapes, explicit/packed legacy curves, and standard streamed/dense/constant Transform or blend-shape samples;
- bounded in-memory Live2D MOC/model3/mip-zero PNG packages with
  schema-verified expression, pose, display-info, physics, and fade-motion
  files, Animator-bound `AnimationClip` fallback motions, and explicit or
  inferred EyeBlink/LipSync parameter groups;
- bounded raw, `TextAsset`, managed-compatible `Shader` text and resident/external-stream `Mesh` OBJ for standard Unity and non-virtual Tuanjie 2022.3.x meshes, embedded-TypeTree JSON/managed-compatible text
  dumps, and externally-schemaed
  stripped `MonoBehaviour` reads without loading or executing managed DLLs;
- direct, bounded `MonoScript` assembly, namespace, class, execution-order,
  and editor-script metadata for selecting trusted external schemas;
- bounded `BuildSettings` scene/level paths and `PlayerSettings`
  company/product metadata reads;
- bounded Unity and Tuanjie `AnimationClip` curve, muscle, ACL, and external
  streaming metadata, including the 2022.3.48t3/55t1/55t4/61t1 field gates,
  plus source-bound ACL 2.x outer-track inspection with hash and output-budget
  validation, bounded retrieval of the exact compressed blob plus decoder map,
  and a safe Python callable decoder boundary used by decoded-track metadata,
  full or selected-model FBX, direct Cubism projection, and Live2D package
  fallback motions (a bundled pure-Rust Tuanjie ACL decompressor remains an
  explicit gap);
- complete bounded Unity/Tuanjie `AnimatorController` and `Avatar` parsing,
  exposed as stable TOS, clip-reference, skeleton, and HumanDescription metadata;
- bounded embedded-TypeTree `CubismExpressionData` projection, including the
  generated `exp3.json` bytes;
- bounded embedded-TypeTree Cubism pose/display-info/physics/fade-motion
  projections plus `AnimationClip` binding projection and generated JSON bytes;
- bounded `Texture2D` mip, ordered `Texture2DArray` layer, and display-order
  `Sprite` decoding to RGBA8 bytes,
  including legacy/modern tight-mesh masks and resolved `SpriteAtlas` data,
  including collection-level atlas backfill and variant replacement;
- source-bound `AudioClip` reads with verified WAV output for pre-2.6 PCM16,
  existing RIFF/WAVE payloads, FSB5 PCM8/16/24/32/float, and pure-Rust
  FMOD/Xbox IMA-ADPCM, Nintendo DSP/GC-ADPCM, Sony VAG/PS-ADPCM and HEVAG,
  FMOD FADPCM, MPEG Layer II/III including every 3-16 channel FSB multistream
  count checked against an independent decoder with FMOD frame padding, and
  sample-verified standard 48 kHz FSB Opus from mono through 7.1 with every
  3-8 channel family-1 multistream mapping checked against an independent
  decoder and a 312-frame encoder delay, plus pure-Rust FSB
  Vorbis from mono through 7.1 with every 3-8 channel Vorbis-to-WAVE speaker
  permutation checked against an independent decoder and the 161-entry FMOD
  setup-header table; remaining platform codecs are preserved raw until a
  verified pure-Rust decoder is available;
- direct embedded `Font`, legacy `MovieTexture`, and inline or externally
  streamed `VideoClip` payload reads, distinct from serialized wrapper bytes; and
- structured, order-preserving `Material` shader references, keywords, tags,
  texture environments, integer/float/color properties, and duplicate entries;
- bounded, atomic export to JPEG/PNG/BMP/TGA/lossless WebP/raw RGBA, text, TypeTree
  dump/JSON, OBJ, and other formats implemented by the Rust Core.

```python
from pathlib import Path

from assetstudio import (
    AssetStudio,
    CubismMotionTargets,
    ExportLimits,
    ExtractionLimits,
    MonoBehaviourSchema,
    MonoBehaviourSchemas,
    extract,
)

studio = AssetStudio(
    "game_Data",
    maximum_input_files=100_000,
    maximum_input_directories=100_000,
    maximum_directory_entries=200_000,
    maximum_path_bytes=1_048_576,
    maximum_total_path_bytes=67_108_864,
    maximum_diagnostic_bytes=256 * 1024 * 1024,
    skip_unreadable_inputs=True,
)

memory_studio = AssetStudio.from_bytes(downloaded_bundle, name="download.bundle")
memory_collection = AssetStudio.from_memory_files(
    [
        ("sharedassets0.assets", downloaded_assets),
        ("sharedassets0.resource", downloaded_resource),
    ]
)

# Oodle remains opt-in. The callback receives one compressed block and the
# exact expected output length; returning any other type or length is rejected.
def decode_oodle(block: bytes, expected_size: int) -> bytes:
    return my_licensed_oodle_wrapper.decompress(block, expected_size)

oodle_studio = AssetStudio("oodle.bundle", oodle_decoder=decode_oodle)

for obj in studio.iter_objects():
    print(obj.file_index, obj.path_id, obj.class_id, obj.name)

for resource in studio.iter_resources():
    print(resource.index, resource.path, resource.byte_size)
resource_bytes = studio.read_resource_by_path("sharedassets0.resource")
header = studio.read_resource_range(0, offset=0, length=4096)

# Tolerant loads retain only bounded skipped-input metadata and expose it by
# page instead of copying the whole diagnostic table at once.
print(studio.load_diagnostic_count)
for diagnostic in studio.load_diagnostic_page(offset=0, limit=4096):
    print(diagnostic.path, diagnostic.message)

# Page one serialized file without copying the rest of its object table.
page = studio.object_page(0, offset=0, limit=4096)

extraction = extract(
    "bundle.ab",
    "unpacked",
    limits=ExtractionLimits(
        maximum_output_bytes=4 * 1024 * 1024 * 1024,
        maximum_total_path_bytes=64 * 1024 * 1024,
        maximum_metadata_bytes=256 * 1024 * 1024,
    ),
)
# The same callback can be supplied to recursive extraction.
oodle_extraction = extract(
    "oodle.bundle",
    "oodle-unpacked",
    oodle_decoder=decode_oodle,
)
for failure in extraction.failures:
    print(failure.source, failure.error)

# maximum_total_path_bytes bounds traversal and recursive labels;
# maximum_metadata_bytes separately bounds the retained success, skip, and
# failure report strings returned to Python.

# Supply a complete Unity object tree produced by a trusted offline schema
# tool. The assembly name is an identity only; no DLL is opened or executed.
schema = MonoBehaviourSchema(
    "Assembly-CSharp.dll",
    "Stats",
    [
        ("MonoBehaviour", "Base", 0, False),
        ("PPtr<GameObject>", "m_GameObject", 1, False),
        ("int", "m_FileID", 2, False),
        ("SInt64", "m_PathID", 2, False),
        ("UInt8", "m_Enabled", 1, True),
        ("PPtr<MonoScript>", "m_Script", 1, False),
        ("int", "m_FileID", 2, False),
        ("SInt64", "m_PathID", 2, False),
        ("string", "m_Name", 1, False),
        ("Array", "Array", 2, True),
        ("int", "size", 3, False),
        ("char", "data", 3, False),
        ("SInt32", "score", 1, False),
    ],
    namespace="Game",
)
stats = studio.read_mono_behaviour_json(0, 114, schema)

# Discover the managed identity used to select that schema. This reads Unity
# metadata only; the named assembly is never opened or executed.
script = studio.read_mono_script(0, 115)
print(script.assembly_name, script.namespace, script.class_name)

# Package planning accepts many independently generated schemas and still
# never opens the managed assemblies named by those schemas.
schemas = MonoBehaviourSchemas([schema])
stats = studio.read_mono_behaviour_json_with_schemas(0, 114, schemas)

image = studio.read_texture(0, 1234, maximum_bytes=256 * 1024 * 1024)
assert len(image.rgba) == image.width * image.height * 4
layers = studio.read_texture_array(0, 187, maximum_bytes=256 * 1024 * 1024)
assert all(len(layer.rgba) == layer.width * layer.height * 4 for layer in layers)
sprite = studio.read_sprite(0, 213, maximum_bytes=256 * 1024 * 1024)
assert len(sprite.rgba) == sprite.width * sprite.height * 4

audio = studio.read_audio_clip(0, 83, format="auto")
Path("audio" + audio.extension).write_bytes(audio.data)

font = studio.read_font(0, 128)
Path("font" + font.extension).write_bytes(font.data)
movie = studio.read_movie_texture(0, 152)
Path("movie" + movie.extension).write_bytes(movie.data)
video = studio.read_video_clip(0, 329)
Path("video" + video.extension).write_bytes(video.data)

material = studio.read_material(0, 21)
print(material.shader, material.texture_environments, material.colors)

shader = studio.read_shader(0, 48, maximum_bytes=256 * 1024 * 1024)
Path("shader.shader").write_bytes(shader)

mesh_obj = studio.read_mesh_obj(0, 43, maximum_bytes=256 * 1024 * 1024)
Path("mesh.obj").write_bytes(mesh_obj)

clip = studio.read_animation_clip(0, 74)
print(clip.name, clip.muscle_present, clip.acl_present, clip.streaming_path)
controller = studio.read_animator_controller(0, 91)
avatar = studio.read_avatar(0, 90)
print(controller.animation_clips, avatar.paths)

for candidate in studio.split_object_fbx_candidates():
    fbx = studio.read_game_object_fbx(
        candidate.file_index,
        candidate.path_id,
        include_animations=False,
    )
    Path(candidate.name + ".fbx").write_bytes(fbx)

dump = studio.read_type_tree_dump(0, 1234)
assert "\r\n" in dump

build = studio.read_build_settings(0, 141)
print(build.scenes or build.levels)
player = studio.read_player_settings(0, 129)
print(player.company_name, player.product_name)
expression = studio.read_cubism_expression(0, 114)
print(expression.source_name, expression.parameters[0].blend)
physics = studio.read_cubism_physics(0, 115)
motion = studio.read_cubism_fade_motion(0, 116)
clip_motion = studio.read_cubism_clip_motion(
    0,
    117,
    targets=CubismMotionTargets(
        parameters=["ParamAngleX"],
        parts=["PartBody"],
    ),
)

live2d = studio.read_live2d_packages(
    schemas=schemas,
    maximum_total_bytes=2 * 1024 * 1024 * 1024,
)
for package in live2d.packages:
    print(package.directory_name, package.moc_file_name)

report = studio.export(
    "exported",
    image_format="png",
    limits=ExportLimits(
        maximum_objects=100_000,
        maximum_total_output_bytes=4 * 1024 * 1024 * 1024,
        maximum_metadata_bytes=256 * 1024 * 1024,
    ),
)
for failure in report.failures:
    print(failure)

# JPEG is lossy, discards alpha like the managed exporter, and accepts quality
# values from 1 through 100. PNG remains the default.
jpeg_report = studio.export("jpeg-export", image_format="jpeg", jpeg_quality=90)

# The original tab-indented CRLF TypeTree text is distinct from JSON.
dump_report = studio.export("dumped", mode="dump_text")
```

All materializing operations accept explicit limits or use conservative
defaults. Bulk export additionally limits the object count and cumulative
published bytes. `iter_files()` and `iter_objects()` are lazy and keep their
originating `AssetStudio` alive; `files()` and `objects()` remain convenience
lists with a one-million-entry safety ceiling. CPU-heavy parsing and export
release Python's GIL. The pure-Rust validation, node conversion, and registry
construction performed by `MonoBehaviourSchema` do too. Its Python-list length
and cumulative UTF-8 budget are checked before copying the input; only this
bounded conversion of the caller's Python strings and tuples remains under the
GIL. Export never follows symbolic-link destinations and publishes files
atomically; by default it does not overwrite existing files.

Caller-controlled lists are preflighted before conversion into owned Rust
vectors. This includes in-memory file tables, schema collections, Cubism target
names, and ACL decoder output; file counts, UTF-8/byte totals, and ACL
frame/curve/value limits are checked before their elements are copied. Core
also rejects request-declared ACL work beyond those limits before invoking the
Python callback at all.

This package is beta while the separately maintained .NET implementation remains
an optional format oracle. GUI support and further C ABI parity are not package goals.

## Local development

With Rust 1.88+, Python 3.9+, and Maturin installed:

```shell
cd crates/assetstudio-python
python -m venv .venv
source .venv/bin/activate
maturin develop --locked
python tests/python_api.py
```

## Release packages

The `cp39-abi3` wheel is built as an optimized release artifact and is usable
with CPython 3.9 and newer. CI builds native wheels for x86-64 and ARM64 Linux,
Windows, and macOS targets, installs every wheel into its build interpreter,
then installs the same wheel into CPython 3.14 and reruns the complete API
fixture. The Linux x86-64 job also builds the wheel through the generated
source distribution so missing workspace or Python-package files fail before
publication.

To reproduce the dependency-locked release wheel locally:

```shell
cd crates/assetstudio-python
maturin build --release --locked --compatibility pypi --out dist
python -m pip install --force-reinstall --no-deps dist/*.whl
python -I tests/python_api.py
```

Build and compile-check the source distribution separately:

```shell
maturin build --release --sdist --compatibility pypi --out sdist-dist
python tests/sdist_contents.py sdist-dist
```

Maturin deliberately prunes the workspace inside the source distribution to
Core + Python. Its internal source-build step therefore cannot use `--locked`:
Cargo first removes CLI/Node-only entries copied from the full workspace lock.
The wheel published by CI is still built from the checked-in lock file, while
the second command proves that the source distribution is complete and builds
successfully in its pruned workspace.
