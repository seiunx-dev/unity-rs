# unity-rs

Native Rust replacement for AssetStudio's .NET Unity asset parsing and export
stack. The public surfaces are the `assetstudio-core` Rust crate, the
`assetstudio-rs` Python package, the native `assetstudio` CLI, and the optional
`assetstudio-rs-node` Node-API package. The retired custom C ABI/context crate is
not retained in this repository. Rust, Python, and Node expose owned high-level
`Studio` values directly rather than numeric context handles.

Nothing here depends on .NET at runtime. The managed implementation, in the
separate [`Team-Haruki/AssetStudio`](https://github.com/Team-Haruki/AssetStudio)
repository, is kept as a compatibility oracle: the differential gate builds it
and compares complete manifests, so a Rust reader is only called compatible
once it matches. Point `ASSETSTUDIO_REPO` at a checkout of that repository, or
keep it as a sibling directory of this one.

```shell
cargo test -p assetstudio-core --test dotnet_oracle -- --ignored
```

The migration target is the parsing/export core, reusable Rust API, Python
package, native CLI, and an optional direct Node.js binding. The WinForms GUI
and further C ABI parity work are intentionally out of scope and are not
retirement gates for this workspace.

The maintained Chinese progress report, verification evidence, completion
criteria, and prioritized gap list live in
[`REWRITE_STATUS.md`](REWRITE_STATUS.md). The direct Core-to-binding mappings
and their automated audits are recorded in
[`docs/python-api-audit.md`](docs/python-api-audit.md) and
[`docs/node-api-audit.md`](docs/node-api-audit.md).

## Current compatibility

| Area | Status |
| --- | --- |
| Source-bound, concurrent bounded regions and independent cursors | Implemented |
| Checked little/big-endian primitive and string reading | Implemented; fixed-length and nul-terminated UTF-8 grow through fallible bounded allocation, while malformed sequences retain the managed replacement-character behavior without an unchecked lossy-copy amplification |
| Unity version parsing and compatibility ordering | Implemented |
| AssetStudio file-type detection, including embedded UnityFS offsets | Implemented |
| SerializedFile v5-v22 metadata, endian, object table, references, and bounded payload regions | Implemented; the effective Unity version follows the managed precedence, so a caller-supplied version wins, an enclosing bundle's revision applies only below format 7, and a file at or above format 7 keeps the version it declares. Where a file's own version is stripped and no override was supplied, this reader falls back to the bundle revision instead of refusing to load as the managed reader does. Formats 1-3 remain explicit `Unsupported` until a real file/TypeTree oracle exists; the managed format enum has no value 4, which is rejected by the same boundary rather than guessed |
| Common UnityWeb/UnityRaw/UnityArchive/UnityFS signature and header dispatch | Implemented |
| UnityWebData/TuanjieWebData directory and payload access | Implemented; bounded directory paths and their separately retained portable file names both use fallible exact-size allocation |
| UnityFS v6-v8 block directory, inline/tail metadata, and padding | Implemented; bounded entry paths and their separately retained portable file names both use fallible exact-size allocation |
| UnityCN-encrypted UnityFS detection and decryption | Implemented. Detection is flag-driven rather than a speculative parse, so an encrypted blocks-info table is named as such instead of surfacing as invalid compressed data. Decryption needs a caller-supplied 16-byte key on `BundleOpenOptions`/`AssetLoadOptions`, or `unity_cn_key=` from Python; without one these bundles are still refused, and no key material ships here. The AES-128 used for key verification and table derivation is implemented in this crate and checked against the FIPS-197 vectors |
| UnityFS None, LZ4/LZ4HC, LZMA, and Zstd blocks-info/data decoding | Implemented |
| gzip, Brotli, and safe Stored/Deflate ZIP traversal | Implemented |
| TypeTree decoding to ordered values and bounded JSON | Implemented |
| Recursive container/resource discovery with traversal budgets | Implemented; root and nested paths have independent per-path and cumulative UTF-8 byte limits exposed by Rust/Python and Node `OpenOptions`. Filesystem discovery charges root/child encoded path bytes, input directories and directory entries before retaining queue/list allocations; single-file companion-resource and selected-split scans use the same budgets, and every retained `PathBuf` grows through fallible reservation. External streamed-resource lookup uses bounded, allocation-free full-path and portable-name indices, preserves first-discovery matching, and safely falls back after low-level callers mutate the public resource table. Stem-named `.resS`/`.resource` companion checks compare extensions ASCII-case-insensitively in place rather than allocating one lowercase string per directory entry |
| AssetBundle/ResourceManager/PreloadData container metadata and cross-file PPtr resolution | Implemented; inherited/effective bundle names, bundle dependencies and the ordered preload/container/reference tables are directly readable from Rust, Python and Node under caller-supplied entry and string budgets |
| Numeric `.split0`...`.splitN` asset/resource reconstruction with lazy random access | Implemented |
| TextAsset bytes, managed-compatible TypeTree dump text, TypeTree JSON, and raw object export | Implemented; dump text reproduces .NET's default float and double rendering, including the switch to scientific notation outside the fixed-notation band, checked against 849 values generated on .NET 10. It targets `InvariantCulture`, which is what the managed exporter runs under, rather than reproducing localized separators and infinity symbols. `TypeValue` keeps a serialized `float` at its source width, so TypeTree JSON writes `0.1` rather than the double expansion of the widened value |
| Font, MovieTexture, legacy and Unity 5+ AudioClip, and VideoClip binary export | Implemented; AudioClip Auto emits WAV for verified pre-2.6 PCM16, existing RIFF/WAVE, FSB5 PCM8/16/24/32/float (including bounded float-to-PCM16 conversion), pure-Rust FMOD/Xbox IMA-ADPCM, Nintendo DSP/GC-ADPCM, Sony VAG/PS-ADPCM and HEVAG, FMOD FADPCM, MPEG Layer II/III including FSB's interleaved multistream layout up to 16 channels (a six-channel real-signal fixture is checked sample-by-sample against pinned vgmstream), standard 48 kHz FSB Opus from mono through 7.1 with the family-1 multistream mapping and 312-frame encoder delay (a six-channel real-signal fixture verifies packet framing and WAVE channel order against pinned vgmstream; CELT-mode packets match libopus, while SILK and hybrid packets retain the bounded upstream `ruopus` defect recorded in `docs/upstream-defects.md`), and pure-Rust FSB Vorbis from mono through 7.1 backed by a checked 161-entry setup-header dictionary; real fixtures for every 3-8 channel layout verify each Vorbis-to-WAVE speaker permutation sample-by-sample against pinned vgmstream. Remaining platform codecs stay raw until a verified pure-Rust decoder is available. Reader-owned extensions and legacy `.resS` lookup names use exact fallible allocation, and the latter remains inside the caller's string budget after the suffix is appended |
| Texture2D parsing plus bounded JPEG/PNG/BMP/TGA/lossless-WebP or raw-RGBA export for integer/packed/half/float/YUY2, BC1-BC7 (DXT1 punch-through blocks follow the s3tc specification and decode index 3 as transparent black, a deliberate divergence from the managed decoder's opaque black), classic/Unity Crunch DXT1/DXT5 plus ETC_RGB4Crunched/ETC2_RGBA8Crunched, PVRTC1 RGB/RGBA 2/4bpp, ATC, ETC/EAC, ASTC LDR/HDR, Xbox 360 word order, and Nintendo Switch GOB deswizzling | Implemented; PNG is the CLI default, JPEG quality is configurable from 1 through 100 and discards alpha. Ordinary multi-image/non-`Tex2D` payloads expose the first surface at mip zero, matching the managed converter and covered by a live differential; later multi-surface mips plus multi-surface Crunch/Switch framing remain explicit unsupported cases. Crunch export is bounded to resident mip zero, and Switch mip zero is read source-bound from both single-mip and mip-chain payloads, padded or cropped; lower Switch mip levels, stripped mips, and formats absent from the managed GOB table remain explicit unsupported cases. Every listed format decodes identically to the managed decoder except the DXT family, HDR included; DXT1 and DXT5 carry the deliberate s3tc divergence described above and DXT3 has no managed counterpart to compare against. Reaching that took vendoring the ASTC, BC6H and ATC decoders with three upstream defects corrected -- two rounding steps and one wrapped subtraction that moved ATC channels by up to 200 of 255 -- all recorded in `docs/upstream-defects.md` |
| Unity 2019+ Texture2DArray parsing, ordered raw-RGBA layer bundles, and direct per-layer Rust/Python RGBA8 access | Implemented for the Texture2D decoder formats; arrays remain linear on Switch, matching the managed synthetic-layer path (which has no platform blob) |
| Sprite metadata plus JPEG/PNG/BMP/TGA/lossless-WebP or raw-RGBA export, including legacy/modern tight-mesh masking, local/cross-file SpriteAtlas and color/alpha Texture2D references, collection-level `packedSprites` atlas backfill/master-over-variant replacement, ImageSharp-compatible downscaling/alpha-mask resampling, and packed flip/rotation transforms | Implemented for validated legacy and modern triangle meshes; Rust/Python/Node can directly read rect/pivot/border, composite atlas key/tags/PPtr, resident texture/settings/UV metadata, secondary textures and normalized tight-mesh triangles under caller budgets. Malformed mesh geometry deliberately falls back to the rectangular crop during rendering |
| SpriteAtlas class 687078895 metadata and render-data-key lookup | Implemented for sample-verified 2017.1-2023 and Unity 6000.0-6000.3 layouts, wired into explicit Sprite atlas references, and directly readable from Rust/Python/Node with the original composite GUID bytes, ordered render data, secondary textures and caller budgets |
| `SerializeReference` managed-references registry | Implemented; the registry Unity writes after an object body is read through the serialized file's own reference types, matched by class, namespace and assembly. The three identity strings are borrowed from the already-budgeted materialized value and resolved immediately, rather than cloned into an uncharged temporary; an undeclared-type diagnostic reports bounded byte lengths instead of copying asset-controlled names again. A null entry stores nothing and an entry naming an undeclared type is declined rather than skipped, since its length is only knowable from a layout that is not present. 93 registry-bearing objects in a Unity 6000.3 corpus match UnityPy byte for byte, including one with 712,288 bytes stored behind a single `rid`; the managed reader does not implement the registry, so there is no oracle row for it |
| MonoBehaviour embedded-TypeTree JSON, MonoScript metadata, and non-executing external full-object schema fallback | Implemented in Rust, the CLI (`--mono-schema`), Node and Python. Core JSON documents have explicit byte/entry/per-entry-node/total-node/per-string/decoded-string-total limits and use fallible copies; the CLI streams each file under the same cumulative budget instead of reading it without a bound, and also caps repeated documents. `tools/monoschema` generates the schema document from a game's managed assemblies as a separate trusted step, scopes every entry to the requested Unity version by default, and `tools/mono_schema_diff.py` checks a generated document against builds that still carry Unity's own type trees. See `docs/mono-schema.md` |
| Common resident or externally streamed standard Unity 2017.3-2023/6000.0-6000.3 and non-virtual Tuanjie 2022.3.x Mesh to bounded OBJ | Implemented with source-bound resource offsets, bounded Tuanjie SharedCluster rev1/rev2/rev3 consumption, collection-wide resolution, and decoded packed `CompressedMesh` geometry. Triangle-list, triangle-strip and quad submesh topologies are all expanded to triangles, following the managed reader's degenerate skipping, odd-position winding flip and pre-Unity-4 strip rule; lines and points remain an explicit unsupported case. Position, normal and UV0 channels decode from every floating-point vertex format the managed reader accepts - Float32, Float16, and the normalized 8- and 16-bit formats - so meshes built with Unity's Vertex Compression setting are covered; integer formats remain an explicit unsupported case for those channels. Unity 6000.2's `MeshLodInfo` tail is walked so a 6000.2+ mesh reads; its LOD levels are not exported, since this reader exports the mesh rather than its LOD chain. Tuanjie virtual-geometry cluster decoding remains an explicit gap |
| Material shader references, keyword/tag lists, and ordered saved texture/integer/float/color properties | Implemented for Unity 4.1+ managed-reader layouts; newer unparsed tail fields remain explicit |
| BuildSettings scene/level paths and PlayerSettings company/product metadata | Implemented with version-gated, endian-aware, bounded readers and high-level Rust/Python access |
| GameObject/Transform/MeshFilter/Animator stable prefixes, collection-wide bounded hierarchy assembly with managed `TryGet`/last-wins semantics, cross-file typed PPtr resolution, and Unity/Tuanjie MeshRenderer/SkinnedMeshRenderer model references | Implemented for standard Unity 2017.3-6000.3 and Tuanjie 2022.3.x renderer prefixes, including Tuanjie virtual-geometry/shading-rate/GRD gates; the bounded model IR and deterministic ASCII FBX 7.4 cover general transform hierarchies, non-virtual resident/external-stream triangle meshes, submeshes/material slots, UV0/normals, non-identity local TRS, direct/hash-recovered skin clusters, static blend shapes, and standard streamed/dense/constant animation samples. The same scene is available in FBX 7.4's binary encoding from the CLI, Node and Python. Virtual-geometry cluster decoding remains an explicit gap. Material textures are resolved, decoded and written beside the FBX as connected `Texture`/`Video` pairs, shared across a batch export by one name allocator; a reference that is not a `Texture2D` is reported and skipped |
| AnimationClip curves/muscle data, Legacy Animation/AnimatorOverrideController references, full AnimatorController constants/TOS/clip references, and collection-wide Animator/controller/clip binding graph | AnimationClip and AnimatorController are implemented for standard Unity 2017.3-2023/6000.0-6000.2 and Tuanjie 2022.3.x, including Tuanjie pre-61 little-endian `m_AnimData`, relocated legacy/61t1 curves, `StreamingInfo`, and the 48t3/55t1/55t4/61t1 ACL field gates. The stable legacy `Animation` GameObject/default/ordered-clip references and `AnimatorOverrideController` base-controller/ordered-substitution table are directly readable from Rust, Python and Node with source and allocation budgets; the scene/FBX graph continues to consume the same Core parsers. Rust, Python and Node can source-bound inspect official ACL 2.x outer tracks and safely inject a caller-supplied decoder into bounded FBX/Cubism projection; request-declared frame/curve/value budgets are rejected before the decoder is called, and size/tag/version/type/rate/FNV-1a plus returned shape/binding/time/value validation happen before consumers see samples. A bundled pure-Rust Tuanjie ACL decompressor remains an explicit gap |
| Avatar skeleton/human constants, legacy handles/colliders, TOS paths, and Unity 2019.1.0b1+ HumanDescription | Implemented as a complete, source-bound parser for standard Unity 2017.3-2023/6000.0-6000.2 and Tuanjie 2022.3.x, with high-level Rust/Python metadata access; the managed reader consumes the stable prefix through TOS, while strict Rust fixtures validate the HumanDescription tail |
| Verified `CubismMoc` MonoBehaviour discovery, bounded Cubism component catalog, source-bound MOC3 metadata, exact `.moc3` CLI output, and `CubismModel._moc`/`CubismRenderer._mainTexture` packages | Implemented, including cross-file typed PPtrs, deterministic collision-safe names, mip-zero PNG, expression `exp3.json`, pose `pose3.json`, display-info `cdi3.json`, physics `physics3.json`, fade-motion `motion3.json`, Animator-bound `AnimationClip` fallback motions, explicit or inferred EyeBlink/LipSync parameter groups sourced from the MOC3's own identifier tables, and trusted non-executing external schema providers in Rust/Python/Node. Collection-wide loose motion/expression fallback scans the role table lazily, validates cumulative item limits before fallible allocation, and retains derived names through fallible exact copies rather than duplicating the whole role index. Fade-motion's managed `part` fallback searches the original identifier through an allocation-free ASCII-insensitive byte window instead of materializing a full lowercase copy. Node can combine schemas with its Promise-backed ACL decoder, so a stripped Tuanjie model and its compressed motion are recovered in one bounded call rather than through two incompatible partial APIs |
| Legacy UnityWeb/UnityRaw v1-v6 payloads | Implemented |
| Shader text conversion from direct scripts, 5.3-5.4 subprograms, and 5.5-Unity 6000 serialized/chunked programs | Implemented for the managed layouts and known GPU record versions; SPIR-V records produce a stable diagnostic instead of native disassembly |
| High-level Rust `Studio` API for path or source-region loading, file/object/external-resource enumeration, source-bound resource streaming and bounded reads, Shader text, Mesh OBJ, Texture2D/Texture2DArray decoding, complete SpriteAtlas metadata, verified direct AudioClip WAV/raw access, MonoScript identity plus AnimationClip/AnimatorController/Avatar metadata, scene assembly, collection and selected-GameObject FBX, bounded Live2D materialization, and atomic export | Implemented; directly materialized FBX, OBJ/MTL, Mesh OBJ and TypeTree outputs use bounded writers with fallible allocation. Archive entry sanitization treats slash and backslash as separators in one pass, preserving traversal/drive/UNC rejection without first allocating a normalized copy of the complete path. Whole-model OBJ groups borrow each Renderer material-slot slice instead of cloning it, pre-count and fallibly reserve their group table, and deduplicate MTL records by stable material key without allocating one temporary formatted name per slot. Binary FBX scene projection checks geometry, skin, morph, animation and connection cardinalities before fallible growth; every input-derived model/geometry/material/texture/skin/morph/animation string property is cumulatively charged against the caller's output limit and exactly fallibly allocated before the encoder sees the node tree. Numeric arrays then stream into zlib without a second full raw byte buffer, every encoder scratch buffer enforces the same output cap before growth, and `FbxBinaryWriteLimits` independently caps output, nodes, properties, non-null depth and per-array elements before recursive encoding. The independent verifier has corresponding input/node/property/non-null-depth/array/cumulative-work limits plus exact declared-size, single-zlib-member inflation with no trailing compressed bytes and complete footer validation. The public binary property surface covers every standard 7.4 scalar and array code, including raw and deflated `b` boolean arrays with non-zero-byte `true` semantics. A zero depth budget accepts an empty root list but no record, rather than charging the list terminator as content |
| Python 3.9+ abi3 package backed directly by the Rust `Studio` API | Implemented first slice; path, single-buffer, or bounded multi-file in-memory loading, lazy/paged file/object/resource enumeration, bounded resource/raw/TextAsset/Shader/Mesh OBJ/TypeTree/MonoScript/externally-schemaed MonoBehaviour/Texture2D/Texture2DArray/Sprite pixels and complete Sprite/SpriteAtlas metadata/AudioClip/Font/MovieTexture/VideoClip/Material/settings/legacy Animation/AnimatorOverrideController/AssetBundle/ResourceManager/PreloadData/Cubism expression/pose/display/physics/fade-motion/AnimationClip-motion reads, scene/model bindings, `SplitObjects`/Animator candidate planning and selected-model FBX, verified Live2D package bytes, safe recursive extraction, and atomic export are available. Model OBJ and FBX-with-textures calls expose the same selectable JPEG/PNG/BMP/TGA/lossless-WebP/raw-RGBA texture formats as Core instead of fixing the Python surface to PNG, and `ModelTextureLimits` independently bounds the texture count, cumulative encoded bytes, and each texture's payload/output/decoder workspace. Core I/O, malformed-data, and unsupported errors remain distinct as Python's standard `OSError` subclasses, `ValueError` and `NotImplementedError`; bounded Rust byte results and the potentially large scene/candidate/image/report conversions use fallible Python-side allocation and report `MemoryError` instead of panicking. Export/image/audio option names are trimmed and matched ASCII-case-insensitively without allocating a lowercase copy; an invalid value longer than 64 UTF-8 bytes is represented by its byte count instead of being duplicated into the exception. The installed wheel is checked bidirectionally against its `.pyi`; a source audit classifies all 106 high-level Core methods (102 Python mappings and four explicit Rust ownership/borrow entries) and requires all 65 public Python methods plus three properties to appear in the strict Python 3.9 mypy consumer, including the runtime-exported ACL/Oodle decoder aliases |
| Node.js Node-API binding | Implemented optional Beta surface through napi-rs: path or owned-buffer loading, bounded metadata pages, the main binary/text/image/material/settings/animation readers, synchronous and worker-backed embedded-TypeTree or externally-schemaed MonoBehaviour JSON, selectable `AudioClip` `auto`/`raw`/verified decoder-free WAV output, direct legacy Animation/AnimatorOverrideController, complete standard/Tuanjie AnimationClip and Avatar metadata, AssetBundle/ResourceManager/PreloadData and complete Sprite/SpriteAtlas metadata, scene and selected-model FBX, atomic export/extract, complete Live2D package materialization, and direct expression/physics/fade-motion/pose/display-info/AnimationClip motion reads. `readLive2DPackagesWithSchemas` restores stripped managed layouts synchronously; `readLive2DPackagesWithAclDecoder` runs on a worker and optionally accepts the same schemas, with separate per-file/aggregate budgets and package diagnostics. `exportWithOptions` exposes Core's complete mode, filename/image/audio/JSON/overwrite and resource-budget policy while the old compact export call remains compatible. Its mode/filename/image/audio option names use the same allocation-free trimmed ASCII matching as Python; defaults do not allocate a temporary string, and oversized invalid values are summarized by UTF-8 byte count. Model OBJ and FBX-with-textures calls accept the same optional JPEG/PNG/BMP/TGA/lossless-WebP/raw-RGBA texture-format names as Core and Python while retaining the old positional calls; an optional trailing `ModelTextureLimits` object bounds count, cumulative encoded bytes and each texture's payload/output/decoder workspace. The existing `scene(maximumGameObjects?)` call remains compatible, while `sceneWithLimits` exposes all six Core hierarchy budgets. Standard Unity clips are synchronous; Tuanjie ACL clips accept a caller decoder on Promise-returning workers for whole-scene ASCII/binary FBX, selected-GameObject FBX, Cubism motion and complete packages, with every returned shape and budget validated in Core. Large Rust-owned input, schema, list, callback and writer-output conversions use fallible allocation rather than unchecked `to_vec`/`collect` growth; `fromBuffers`, schema collections, nested schema nodes and Cubism target lists remain JavaScript arrays until their counts pass the binding/Core reader budgets, and their strings are length-checked before fallible UTF-8 copies. ACL callback arrays likewise remain opaque until their declared lengths and total work pass the Core limits, then copy directly into fallibly reserved output without an intermediate `Vec<f64>`. It calls `assetstudio-core` directly and does not use the historical C ABI. Generated declarations are compiled as a strict TypeScript consumer during `npm test`, including the ACL and Oodle callback signatures |
| Native CLI inspect/info/list/scene, bounded export, and recursive safe extract commands with stable exit codes | Implemented; candidate tables, collision-safe output names and read-only summaries grow through fallible allocation, while summary output is deterministically sorted and escaped without a second materialized string. Recursive `inspect` charges directory/file paths against Core's default single and cumulative load-path limits before retaining queue/list entries, and uses the same input-directory, directory-entry and regular-file ceilings as normal collection loading. Non-UTF-8 filesystem paths are charged at their replacement-expanded UTF-8 length; failed-file names are replacement-decoded and escaped directly into the output writer, while successful root and recursive gzip/ZIP labels are composed through streaming `Display` adapters rather than materialized as lossy or concatenated temporary strings. The process entry retains at most 65,536 arguments, 1 MiB per encoded argument and 64 MiB cumulatively through `try_reserve`; every read-only, modern-write, and legacy input/output path then preserves the platform encoding in one exact fallible `PathBuf` copy, each two-path command reserves its complete positional table before retaining either path, and repeatable export class filters grow fallibly without changing their stable order or duplicate semantics. Load-option filtering and schema-path tables grow fallibly, legacy mode matching does not allocate a lowercase copy, and diagnostics summarize any argument longer than 64 encoded bytes instead of echoing it |
| UnityArchive payloads | Recognized and safely rejected; no sample-verified public format |
| Oodle bundle blocks through an injected safe decoder interface | Implemented in Rust; Python loading and extraction accept an exact-size callable adapter, while proprietary libraries remain user-supplied and are neither linked nor redistributed |
| Remaining platform-native codecs | Next milestone |
| Legacy headless CLI WorkMode entry names | Implemented for Extract, Export, ExportRaw, Dump, Info, Live2D (`l2d`/`live2d`), SplitObjects and Animator; explicit output paths and safe no-clobber publication replace implicit `ASExport`/`ASExtract` side effects |
| Remaining asset formats | Planned from sample-backed corpus hits |
| WinForms GUI | Intentionally out of scope |

## Build and test

Everything the CI workflow runs is also runnable here, on any platform:

```shell
# The whole gate: formatting, Clippy, rustdoc, the packaged crate, the
# workspace tests, release CLI/Node artifacts, the Python wheel/sdist (including
# an abi3 forward-compatibility install when Python 3.14 is present), and the
# differentials.
# Groups whose tool is missing are reported as skipped rather than failed.
python3 tools/local_ci.py --interpreter /path/to/venv/bin/python
python3 tools/local_ci.py --list          # what it would run
python3 tools/local_ci.py quality rust    # only these groups
python3 tools/local_ci.py --fail-on-skip quality rust security  # close-out evidence
python3 tools/local_ci.py cross           # Linux workspace + Windows Core/CLI/Python compile
python3 tools/local_ci.py linux           # release CLI/Python/Node on amd64/arm64
```

Or step by step:

```shell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo install cargo-audit --version 0.22.2 --locked --no-default-features
cargo audit --file Cargo.lock --deny unsound --deny yanked
# Optional private real-game corpus; see corpus/README.md.
ASSETSTUDIO_CORPUS_MANIFEST=/private/corpus/manifest.json \
  cargo test -p assetstudio-core --test real_corpus --locked -- --ignored
cargo run -p assetstudio-cli -- inspect <file-or-directory>
cargo run -p assetstudio-cli -- list <input> --unity-version 2022.3.62f1
cargo run -p assetstudio-cli -- info <file-or-directory>
cargo run -p assetstudio-cli -- list <file-or-directory>
cargo run -p assetstudio-cli -- scene <file-or-directory>
cargo run -p assetstudio-cli -- fbx <file-or-directory> <output.fbx>
cargo run -p assetstudio-cli -- obj <file-or-directory> <output.obj>
cargo run -p assetstudio-cli -- split-objects <file-or-directory> <output-directory>
cargo run -p assetstudio-cli -- animator <file-or-directory> <output-directory>
cargo run -p assetstudio-cli -- live2d <file-or-directory> <output-directory>
cargo run -p assetstudio-cli -- live2d-package <file-or-directory> <output-directory>
cargo run -p assetstudio-cli -- export <file-or-directory> <output-directory>
cargo run -p assetstudio-cli -- export <input> <output> --mode dump-text
cargo run -p assetstudio-cli -- export <input> <output> --image-format <jpg|jpeg|png|bmp|tga|webp|raw-rgba>
cargo run -p assetstudio-cli -- extract <file-or-directory> <output-directory>
cd crates/assetstudio-python
maturin develop --locked --offline
python tests/python_api.py
cd ../assetstudio-node
npm install
npm run build:debug
npm test
```

Rust applications can use the high-level API without any ABI boundary:

```rust
use assetstudio_core::studio::Studio;

let studio = Studio::open("game_Data")?;
for object in studio.objects() {
    println!("{} {} {}", object.file_index(), object.path_id(), object.class_id());
}
let payload = studio.object(0, 7).unwrap().read_raw(512 * 1024 * 1024)?;
let dump = studio.object(0, 7).unwrap().read_type_tree_dump(256 * 1024 * 1024)?;
# Ok::<(), assetstudio_core::Error>(())
```

Low-level callers can inspect `AssetCollection::serialized_files()` and
`AssetCollection::resources()` as stable discovery-order slices. The indexed
tables are intentionally not publicly mutable: consume a collection through
`into_parts()`, freely update the unindexed owned tables, reconstruct it with
`from_parts()`, and explicitly rebuild indexes or resolve metadata instead of
silently invalidating PathID/resource first-match semantics. This path moves
loaded regions rather than cloning their contents.

The Python package wraps that same API directly through PyO3:

```python
from assetstudio import AssetStudio, ExportLimits, ExtractionLimits, SceneLimits, extract

studio = AssetStudio("game_Data")
for obj in studio.iter_objects():
    print(obj.file_index, obj.path_id, obj.class_id, obj.name)
page = studio.object_page(0, offset=0, limit=4096)
extraction = extract(
    "bundle.ab",
    "unpacked",
    limits=ExtractionLimits(maximum_output_bytes=4 * 1024 * 1024 * 1024),
)
payload = studio.read_raw(0, 7, maximum_bytes=512 * 1024 * 1024)
dump = studio.read_type_tree_dump(0, 7, maximum_bytes=256 * 1024 * 1024)
scene = studio.scene(limits=SceneLimits(maximum_game_objects=100_000))
image = studio.read_texture(0, 28)
layers = studio.read_texture_array(0, 187)
sprite = studio.read_sprite(0, 213)
audio = studio.read_audio_clip(0, 83, format="auto")
shader = studio.read_shader(0, 48)
mesh_obj = studio.read_mesh_obj(0, 43)
build = studio.read_build_settings(0, 141)
player = studio.read_player_settings(0, 129)
expression = studio.read_cubism_expression(0, 114)
physics = studio.read_cubism_physics(0, 115)
motion = studio.read_cubism_fade_motion(0, 116)
packages = studio.read_live2d_packages(maximum_total_bytes=2 * 1024 * 1024 * 1024)
report = studio.export(
    "exported",
    image_format="png",
    limits=ExportLimits(maximum_total_output_bytes=4 * 1024 * 1024 * 1024),
)
```

For stripped `MonoBehaviour` data, the Python package also accepts a complete
`MonoBehaviourSchema` produced by a trusted offline schema tool. Multiple
schemas can be collected in `MonoBehaviourSchemas` and passed to Live2D
package materialization. The assembly name is used only for matching; the
runtime never loads or executes the DLL. Reads through a schema return the
JSON alongside the tree it came from, `"embedded"` or `"schema"`, because a
value read through a supplied schema is only as good as that schema.

The CLI takes the same schemas as a document: `--mono-schema <path>`,
repeatable, on any command that opens a collection. `docs/mono-schema.md`
describes the document, the generator that writes one, and how a generated
schema is checked against builds that still carry Unity's own type trees.

A game directory routinely mixes readable assets with encrypted, truncated or
not-yet-supported containers. By default Core refuses the whole load over any
one of them, which matches the historical Rust behaviour but reports nothing
where the managed tool reports almost everything. `AssetLoadOptions` therefore
carries a `failure_policy`: under `LoadFailurePolicy::SkipInput` the inputs that
did parse are kept and each skipped one is recorded in
`AssetCollection::diagnostics`. Python exposes this as
`skip_unreadable_inputs=True`. The CLI always skips, names every skipped input
on stdout, and exits with the partial-failure status; a load where nothing at
all parsed is still a hard failure rather than an empty success.

Every command that opens a collection accepts `--unity-version <VERSION>`,
which parses the input against that version instead of the one it declares. It
is required for files whose version was stripped at build time, and it outranks
both the declared version and any enclosing bundle revision, matching the
managed reader's `CustomUnityVersion`. `list` reports the effective version
whenever it differs from the declared one. Oodle-compressed bundles still need
a caller-supplied decoder and are therefore reachable from the Rust and Python
APIs but not from the CLI, which does not load external native libraries.

The read-only `inspect`, `info`, `list`, and `scene` commands never create a
default output directory. `scene` streams the bounded collection-wide
`GameObject` hierarchy, Transform TRS, and model component bindings using
stable file-index/path-ID keys. Directory inspection continues across bad
inputs and reports a partial-failure exit status. Usage errors, runtime
failures, and partial batch failures have distinct stable exit codes; the
legacy `<input> -m info` spelling is accepted without reproducing the .NET
CLI's output-directory side effect.

The `fbx` command writes deterministic FBX 7.4 for general
Transform hierarchies, ordinary and skinned renderer bindings, resident
triangle meshes, submeshes/material slots, UV0/normals, non-identity local
TRS, direct or bone-name-hash-recovered bones, bind poses, four-weight skin clusters, static blend-shape
targets/channels, and selected explicit legacy animation curves (including
delta-time/packed quaternions) plus standard streamed, dense, and constant
Transform or blend-shape samples. It mirrors
Unity X, converts mirrored
quaternions to Euler degrees, reverses triangle winding, writes through an
atomic no-clobber temporary file, and accepts
`--maximum-output-bytes <N>` with a 16 MiB default and 512 MiB hard ceiling.
For one `fbx` or `obj` invocation this is a cumulative limit over every file
newly published by that invocation: the FBX or OBJ document, the OBJ companion
MTL, and newly written textures. Existing/shared textures skipped by the
no-clobber publisher are not charged again.
The `obj` command writes the same model as Wavefront OBJ. Because OBJ has no
hierarchy, node transforms are baked into world space and vertex indices
accumulate across the file; a companion `.mtl` under the same stem carries the
material colours and `map_Kd`/`map_Bump`/`map_Ks` lines pointing at the sibling
textures. Its face references name only the channels the mesh actually has,
unlike the single-mesh `.obj` the `export` command writes, which reproduces the
managed writer's unconditional `v/vt/vn` exactly. The managed repository holds
three writers for this format and they disagree with each other; this matches
the headless one -- the path behind the managed library's own object payloads,
and so the one being replaced -- byte for byte, which the differential checks
by comparing the exported documents rather than only the geometry behind them.
Against the other two, the exporters behind the GUI and the CLI, two
differences remain, both documented on `write_mesh_obj`: line endings are CRLF
throughout, where those writers' `g` lines follow the platform and their other
lines do not; and `NaN` is replaced per numeric value rather than by a
document-wide text substitution that also rewrites a mesh named `NaN`.

`--binary` writes FBX 7.4's binary encoding instead of its text one, with the
same scene content; some importers accept only that form, and it is smaller and
faster to parse. `obj` rejects the flag rather than ignoring it, since OBJ has a
single encoding. Material textures are resolved
through their `PPtr`s, decoded once per texture object, and written beside the
FBX as `Texture`/`Video` pairs connected to `DiffuseColor`, `NormalMap`,
`SpecularColor` or `Bump`, following the managed reader's `_MainTex`/`_BumpMap`/
`Specular`/`Normal` property mapping; the UV offset and scale come from the
material's own `TexEnv`. Use `--no-textures` for geometry alone and
`--texture-format <FORMAT>` to pick something other than PNG. Texture names come
from the asset and are reduced to a single path component, so a hostile name
cannot escape the output directory, and an existing file is never overwritten.
One texture set is published transactionally: if a later texture fails
validation, writing, syncing, or no-clobber publication, every texture newly
published by that call is removed while pre-existing skipped files are kept.
The CLI publishes every FBX, including each `split-objects` or `animator`
candidate, only after its texture batch succeeds. For OBJ it
publishes textures first, the companion MTL second, and the OBJ last; a late
collision or publication failure rolls back the earlier files created by that
same command, so a newly written OBJ cannot be left referring to an old MTL.
Creating the no-clobber hard link is the commit point for FBX and single-MOC
outputs: failure to unlink the temporary name is deferred to `Drop` cleanup and
does not misreport an already visible destination as a failed publication.
For a complete Live2D package, the synced directory rename is the commit point;
removing and syncing the per-destination publication lock is likewise
best-effort cleanup with a `Drop` retry.
The 16 GiB cumulative batch ceiling charges each FBX plus the exact size of
texture files newly published for that candidate; shared or pre-existing files
that were skipped are not charged again, and exhaustion is checked before the
FBX commit point.
A reference that resolves to a class other than `Texture2D`, or that fails to
decode, is reported and skipped rather than failing the model. The
`split-objects` and `animator` commands publish one atomic, no-clobber FBX per
managed-compatible candidate; legacy `-m splitObjects` and `-m animator`
spellings route to those workflows when an explicit output directory is given.

Legacy `-m dump` and explicit `--mode dump-text` emit the original
tab-indented, CRLF TypeTree text contract with a `.txt` extension.
`typetree-json` remains a separate JSON mode.

The `live2d` command exports exact `.moc3` bytes only from `MonoBehaviour`
objects whose resolved `MonoScript` is `CubismMoc`. It uses stable ordering,
sanitized collision-safe names, cumulative output limits, and atomic
no-clobber writes. Its cumulative byte limit charges only successfully
published MOCs: a collision or write failure is reported but does not consume
capacity needed by a later model. Both Live2D commands validate the complete output-root path
before creating it and reject symbolic-link or non-directory components, so a
selected output directory cannot redirect a package or MOC elsewhere. FBX,
OBJ, and both Live2D commands share the same linear directory builder: it
normalizes a relative path once, then checks or creates each component using
one growing path buffer rather than retaining a complete path copy for every
missing parent. This also catches a symbolic link followed by an already
existing real directory. The
separate `live2d-package` command emits each verified
model into a sibling directory containing its exact MOC, deterministic
`model3.json`, mip-zero texture PNGs, and schema-verified expression
`exp3.json`, pose, display-info, physics, fade-motion, and Animator-bound
`AnimationClip` fallback motion JSON files. The manifest includes deterministic
EyeBlink and LipSync parameter groups, preferring explicit Cubism marker
components and falling back to the managed parameter-name heuristics. It builds and syncs the complete
package in a temporary sibling directory before publishing by rename. A
per-destination `create_new` lock makes concurrent runs of this tool
no-clobber; the final absence check is best effort against unrelated external
filesystem racers because portable Rust has no directory `rename_noreplace`.

Core also exposes a bounded package IR and deterministic `model3.json` writer.
It consumes embedded or trusted externally supplied TypeTrees for `_moc`,
`_mainTexture`, expression, fade-motion, physics, pose, and display-info fields,
plus native `AnimationClip` bindings;
requires resolved targets to have the
expected Cubism role; and reports missing or malformed schemas explicitly. It
does not guess managed object tails when neither an embedded tree nor an
external schema is available.

The native CLI also provides end-to-end export and extraction slices. Its `auto` mode
exports TextAsset bytes, raw Font/MovieTexture/VideoClip payloads, verified direct
AudioClip WAV or otherwise preserved raw audio,
managed-layout Shader text through Unity 6000 with known GPU record versions, embedded-TypeTree MonoBehaviour JSON,
JPEG, PNG (default), BMP, TGA, lossless WebP, or raw-RGBA Texture2D/Sprite images, ordered raw-RGBA
Texture2DArray layer bundles, and common
resident Mesh OBJ for the implemented paths. Generic objects use
TypeTree JSON where a usable tree exists and otherwise fall back to bounded raw
data; MonoBehaviour reads through `--mono-schema` documents where one matches
and otherwise reports a missing-schema error instead of guessing script
fields. `extract` recursively unwraps supported bundles, web
containers, ZIP, gzip, and Brotli while enforcing enclosed paths, no-follow
symbolic-link rules, atomic no-clobber writes, and cumulative limits. Directory extraction
charges every retained filesystem path before allocating its queue/list entry; a 32,767-byte
single-path ceiling and 64 MiB cumulative path/diagnostic-label ceiling are configurable through
Core and Python `ExtractionLimits`. Root memory-region labels and recursive `parent::child` labels
share that budget and are checked before allocation. Loading and extraction share one internal
filesystem-string converter: names that are not valid UTF-8 no longer materialize a complete
`to_string_lossy` result before their limits apply. Unix invalid sequences and Windows unpaired
UTF-16 surrogates are streamed with the platform's replacement semantics; the Loader and extractor
charge the expanded label bytes before one exact allocation, while extraction's portable 240-byte
component ceiling is enforced character by character. Extraction success, skip and failure reports
also reserve fallibly. Python export/extraction report paths and Node export report paths use the
same private two-pass filesystem-string algorithm: they count replacement-expanded UTF-8 first,
reserve with the binding's own fallible allocator, and then copy once, rather than allocating a complete
`to_string_lossy` temporary before the binding can report allocation failure. CLI argument and
MonoBehaviour-schema path diagnostics stream the same replacement characters directly into their
formatter. Both possible atomic-publication outcomes are reserved before writing, so a
published file cannot become unreportable because a later vector growth fails. Portable
case-insensitive output claims use a fallibly reserved hash index that is never iterated for naming;
Unicode lowercase expansion is length-checked before key allocation and retained keys share the
cumulative path budget. Asset-controlled relative output paths are assembled through checked,
fallible builders: container directories, parent-collision prefixes, wrapper-decoded names,
`~N` collision suffixes, absolute output candidates, and temporary publication names all reserve
before copying. The `_unpacked`, `.decoded`, and collision suffixes count toward the same 240-byte
portable component ceiling, while the complete relative path is rejected before allocation when it
would exceed `maximum_path_bytes`. Caller-supplied output roots likewise use fallible lexical
normalization and a linear nearest-existing-ancestor walk; a deep missing suffix is copied once
rather than rebuilt at every parent. Exported source
groups and object files share a fallible 240-byte portable component budget that includes
their numeric prefix, extension and worst-case collision suffix. Archive path components
use the same ceiling incrementally, so an oversized untrusted name is rejected before a
matching allocation is attempted. Export collision claims are scoped to one source group and store
only a fallibly lowercased bounded file-name key, never a copy of the full absolute output root for
every object. Output-root normalization, group/file paths, secure-directory walks, temporary names,
and replacement-backup paths all reserve fallibly. The successful `ExportRecord` and its report
slot are prepared before atomic publication, so a later report allocation cannot turn a file that
is already on disk into an unreportable error. Across export, extraction, and model-texture writes,
the hard-link or replacement rename is the publication commit point; failure to remove the temporary
hard-link or previous-file backup is retried by Drop and does not reclassify a committed destination
as a failed operation. Model-texture sets also revalidate every public caller-supplied file name as
one portable relative component before creating a temporary file, so manually pushed `../`, absolute,
multi-component, overlong, control/reserved-character or Windows device names cannot escape or poison
the selected directory. Asset-derived texture names, collision suffixes, binding properties, skip
diagnostics, lookup tables and written-path reports all grow through checked fallible allocation; the
complete report capacity is reserved before the first texture can be published. Texture,
Sprite, Mesh, AnimationClip and Live2D paths now pass differential tests
against the managed implementation, covering the block-compressed formats
apart from the DXT family, both Crunch dialects, Switch deswizzling padded and
cropped, and a whole Live2D package compared against the real managed
extractor. DXT1 and DXT5 are held out deliberately -- their colour palettes
diverge by design, which the texture row above states -- and DXT3 has no
managed decoder to compare with. The existing .NET
stack remains authoritative where this rewrite has no comparison: Shader GPU
record revisions from 5.5 on, blocked by a managed initializer defect recorded
in the status document, and breadth across real games.

The retired custom C ABI/context source has been removed rather than merely
excluded from the Cargo workspace. Rust, Python, and Node integrations use the
high-level Core API directly, so the rewrite has no C ABI release, context
handle lifecycle, or parity gate.

The Rust workflow tests the workspace on Linux, Windows, and macOS and builds
optimized `cp39-abi3` Python wheels for x86-64 and ARM64 variants of all three
platforms. Each wheel is installed and exercised on its build interpreter and
again on CPython 3.14. Tagged and manually dispatched builds also build and
smoke-test native CLI binaries and Node package tarballs for that same six-way
matrix, then publish them with the verified Python source distribution and the
Rust Core crate as workflow artifacts.

Every published artifact carries the project's [`LICENSE`](LICENSE), the
audited [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md), and the complete
locked dependency texts in
[`THIRD_PARTY_LICENSES.txt`](THIRD_PARTY_LICENSES.txt). The latter is generated
deterministically from the non-development Cargo graph and checked against the
copies shipped by the Core crate, CLI archive, Python wheel/sdist, and Node
package; `tools/generate_dependency_licenses.py --check` rejects a missing,
stale, or newly unlicensed dependency.

A second differential gate runs against
[`UnityPy`](https://github.com/K0lb3/UnityPy), an independent implementation
that installs from PyPI and needs no .NET, comparing object order, path IDs,
class IDs, byte sizes, names and raw payload hashes
(`crates/assetstudio-python/tests/unitypy_oracle.py`). It deliberately omits
decoded-pixel and mesh rows: UnityPy decodes through the same upstream
`texture2ddecoder` this workspace links and transliterates AssetStudio for mesh
and shader work, so agreement there would not be independent evidence. Where
UnityPy cannot resolve a name -- its name lookup goes through a bundled
TypeTree database that does not cover every class and version -- the run
reports the comparison as skipped rather than treating it as an agreed empty
string.

The managed-oracle CI job checks out the pinned managed parser and compares it with
Rust over v13 big-endian/32-bit-PathID and v22
little-endian/64-bit-PathID fixtures. The manifest locks object order, signed
and non-monotonic PathIDs, class IDs, names, byte sizes, raw object hashes,
TypeTree dump text, settings fields, Material shader/property values, resident
Mesh vertex/normal/UV/index bits, resolved Sprite crop pixels, and source-bound
legacy Shader output bytes, Texture2D, AudioClip, and VideoClip ranges from a
shared `.resS` file, plus Unity 6000.2 AnimatorController tail/reference
alignment. A
declared-size truncation fixture
also locks the existing directory-load rejection behavior. This is a baseline,
not yet a substitute for a versioned real-game corpus. The opt-in
[`corpus/`](corpus/README.md) runner now compares private real assets with
checked managed-oracle snapshots without committing proprietary input bytes.

## Migration rules

- Preserve observable object ordering, PathID behavior, payload formats, and
  error families in the Rust and Python APIs before retiring the .NET
  implementation.
- Treat all lengths, offsets, counts, and alignments as untrusted input.
- Keep parsers bound to `Region` values instead of shared mutable file cursors;
  large entries use streaming copies while convenience materialization is capped.
- Add a focused parser test and, where practical, a C#-versus-Rust fixture before
  marking a format branch compatible.
- Keep platform-native texture, FBX, FMOD, and Oodle adapters behind narrow
  interfaces so they can be isolated and replaced independently. Core's Oodle
  trait is safe; loading or executing an external native decoder belongs in a
  separately audited adapter/helper process.
