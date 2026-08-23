# assetstudio-rs-node

This package is a thin Node-API binding over `assetstudio-core`. It does not
define or route through AssetStudio's historical C ABI. Parsing, cross-file
resolution, limits, and format errors remain owned by the safe Rust core.

The first binding slice supports:

- path or in-memory loading with composable `OpenOptions`; the memory forms
  keep their old positional arguments and accept options last;
- bounded file, object, and resource metadata pages;
- bounded resource, raw-object, `TextAsset`, TypeTree JSON/dump, `Shader`, and
  `Mesh` reads;
- bounded legacy `Animation`, `AnimatorOverrideController`, `AssetBundle`,
  `ResourceManager`, `PreloadData`, and complete `Sprite`/`SpriteAtlas`
  reference/metadata reads;
- complete bounded `AnimationClip` curve-count, muscle, ACL, and external
  streaming metadata;
- complete bounded `Avatar` skeleton, ordered TOS path, and HumanDescription
  metadata;
- bounded `Texture2D`, `Texture2DArray`, and `Sprite` decoding to tightly
  packed RGBA8 bytes;
- selectable `AudioClip` `auto`/`raw`/`wav` materialization, using Core's
  verified decoder-free WAV paths while retaining `readAudio` as the raw-only
  compatibility entry point;
- complete `exportWithOptions` coverage for Core's mode, filename, image,
  audio, JSON, overwrite and per-payload/aggregate budget policy, while the
  compact `export(outputRoot, overwrite?)` call remains compatible;
- worker-backed caller ACL decoders for whole-scene ASCII/binary FBX and
  selected-`GameObject` FBX, with Core validating callback shape and budgets;
- direct bounded `MonoBehaviour` JSON through an embedded Unity TypeTree, plus
  the existing trusted-schema variant for stripped managed layouts; both have
  synchronous and worker-backed Promise forms and report whether the resulting
  document used an `embedded` or `schema` tree;
- complete Live2D package materialization with trusted external
  `MonoBehaviour` schemas, plus a Promise worker that can combine those
  schemas with a caller ACL decoder for Tuanjie motions; single-file and
  aggregate output ceilings are independent, and partial package failures
  remain visible through diagnostics;
- Promise-returning worker variants for path/buffer opening and the main
  raw/text/TypeTree/shader/mesh/image reads, backed by libuv's worker pool;
  `openAsync` and `fromBufferAsync` accept the same trailing `OpenOptions` as
  their synchronous counterparts.

Build and test locally with Node.js 20.17 or newer:

```shell
npm install
npm run build:debug
npm test
```

Both synchronous methods and `*Async` worker variants are available. Prefer
the Promise-returning variants for large or untrusted inputs so parsing and
decoding do not block the JavaScript event loop; the same Rust core limits
apply to both paths. Rust-owned copies at the Node boundary use fallible
allocation: in-memory inputs, schema nodes, metadata/candidate lists, model
texture reports, ACL/Oodle callback payloads and decoded arrays all return a
JavaScript error if their allocation cannot be reserved. Direct JSON and FBX
writers use a fallible bounded buffer, so an exhausted output budget or
allocator cannot leave an unchecked, growing `Vec` behind the binding.

`fromBuffers`, schema collections and each schema's nested `nodes` table stay
as JavaScript arrays until their counts pass the file/schema/node budgets.
Names are length-checked before a fallible UTF-8 copy, so napi-rs never gets a
chance to eagerly build an unbounded Rust `Vec` or `String` for these inputs.
The optional Cubism parameter/part target lists follow the same rule and are
charged to the motion reader's curve and string budgets before any element is
read.

Path, `openAsync`, `fromBuffer`, `fromBufferAsync`, and `fromBuffers` loading
accept `maximumPathBytes` and `maximumTotalPathBytes` through `OpenOptions`.
They bound both caller labels and fully qualified paths created while
recursively opening bundles, WebData and ZIP entries; gzip and Brotli wrappers
retain the same path and are not charged a second time. The defaults match the
in-memory input contract: 1 MiB per path and 64 MiB across one load. The
complete Core-to-Node disposition and the source/declaration/consumer checks
are recorded in
[`docs/node-api-audit.md`](../../docs/node-api-audit.md).

ACL callback results stay as opaque JavaScript values until the returned
object and all three array lengths have been checked against the request and
`AclDecodeLimits`. Only then are elements copied into fallibly reserved Rust
buffers; floating-point values are narrowed directly from JavaScript `number`
to `f32`, without first creating an unbounded intermediate `Vec<f64>`.
