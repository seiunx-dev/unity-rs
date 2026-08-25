# Node API audit

Last verified: 2026-08-25.

`assetstudio-rs-node` is an optional direct napi-rs binding over
`assetstudio-core`. It does not load the removed custom C ABI or a .NET
assembly. This document records how the stable high-level Rust surface is
represented in JavaScript/TypeScript and which ownership differences are
intentional.

## Core-to-Node mapping

| Rust `Studio` capability | Node surface |
| --- | --- |
| Path, one-region and named multi-region loading with options | constructor, `openWith`/`openAsync`, `fromBuffer`/`fromBufferAsync`, `fromBuffers` |
| File, object and resource counts and stable paging | properties, `filePage`, `objectPage`, `resourcePage` |
| Source-bound resources and object payloads | bounded `readResource*`, `readRaw`, `readText` and specialized readers |
| Scene hierarchy and model planning | `sceneWithLimits`, SplitObjects/Animator candidates and selected-GameObject FBX |
| Textures, sprites, audio/video/font, mesh, shader, material and project settings | corresponding bounded `read*` methods |
| Animation, controller, avatar, ACL adapter and Live2D | metadata/document/package readers and Promise decoder hooks |
| Atomic collection export and safe recursive extraction | `exportWithOptions` and static `extract` |

The final optional `OpenOptions` argument on `openAsync`, `fromBuffer`,
`fromBufferAsync`, and `fromBuffers` is significant: a worker-backed load, an
in-memory UnityCN archive, or a file with a stripped Unity version needs the
same key/version/failure-policy/path-budget combination as the synchronous
filesystem entry. New arguments follow the pre-existing parameters, so older
positional calls remain valid.

The following Rust methods intentionally do not have identically shaped Node
methods:

- borrowed `StudioFile`, `StudioObject` and `StudioResource` views become owned
  napi object metadata plus index/key-based bounded reads, so no Rust lifetime
  crosses the addon boundary;
- `write_*(&mut impl Write)` methods become bounded `Buffer` results or atomic
  path-based exports;
- `from_collection`, `collection` and `into_collection` accept, borrow or move
  the low-level Rust `AssetCollection`; they remain Rust ownership escape
  hatches rather than exposing that internal type to JavaScript;
- `object_by_index` returns a borrowed Rust `StudioObject`. Node exposes
  `objectIndex` for diagnostics and paging, while reads use the stable
  `(fileIndex, pathId)` key and retain collection first-match semantics;
- low-level provider traits become explicit JavaScript callbacks or inert
  schema descriptions. Work that needs a callback runs on a libuv worker so
  the event loop can execute the callback without deadlocking.

## Enforced evidence

- `tools/check_node_api_surface.py` parses the four high-level Core impl blocks,
  the `#[napi] impl AssetStudio` block, mapped napi object fields, generated
  `index.d.ts`, and the strict TypeScript consumer. All 107 public Core methods
  must map to a real symbol in both Rust and TypeScript or one of the four
  Rust-only ownership entries above.
- The Rust class and generated declaration must expose exactly the same current
  85 methods and 4 properties. A stale checked-in declaration, an addon method
  missing from the declaration, or a declaration with no Rust export fails
  `quality` before a platform-specific addon is loaded.
- Every public `AssetStudio` member is called by `tests/types.ts`; pinned `tsc`
  checks the real argument and result shapes. Comments are removed before the
  source-level coverage scan and cannot impersonate a caller.
- `tools/test_node_api_surface.py` has reverse tests for a new unclassified
  Core method, missing Rust and TypeScript mapping targets, Rust/declaration
  drift, an unconsumed method, a commented-out call, and missing object fields.
- `tests/node_api.cjs` runs the built addon. The options regressions prove
  `maximumPathBytes` reaches synchronous and worker-backed path/one-buffer Core
  loads, while `maximumInputFiles` rejects a multi-buffer collection before
  napi walks its elements.
- Promise APIs that accept external MonoBehaviour schemas count-check and copy
  the JavaScript-owned values before queueing, then validate Unity-version
  identity and build the random-keyed Core registry on the worker. Behavioral
  tests distinguish this from the synchronous API: an invalid version throws
  immediately from the synchronous call, while both the MonoBehaviour JSON and
  Live2D/ACL asynchronous calls return normally and reject their Promises before
  parsing or invoking the decoder callback.
- `readTextureAsync` and `readTextureArrayAsync` decode and convert bottom-up
  Unity pixels to the top-down JavaScript row order entirely in `Task::compute`.
  Their public task-output types encode that invariant. The array task also
  fallibly reserves the final layer table and converts every layer into its
  Node-facing `RgbaImage`/`Buffer` on the worker, so `resolve` only moves the
  already-final Vec. A positive/negative source audit rejects changing the
  task output or adding allocation, loops, or image projection back to
  `into_nodes`. Rust tests cover one image and multiple layers; installed-addon
  tests compare synchronous and Promise output for real Texture2D and
  Texture2DArray fixtures pixel by pixel.
- `readLive2DPackagesWithAclDecoder` materializes its packages on a worker and
  now also flattens every MOC, manifest, texture, JSON document and diagnostic
  into the final fallibly allocated `Live2DPackageSet` there. Its `resolve`
  method only returns that already-final table. A positive/negative source
  audit rejects changing the task output back to the Core set or moving the
  O(packages + files + diagnostics) projection onto the event loop.
- `tests/installed_package.cjs` loads the packed tarball from a temporary
  consumer, parses the installed `index.d.ts`, and compares its static methods,
  instance methods, and getters bidirectionally with the installed native
  `AssetStudio` class. The installed surface is therefore also locked at 85
  methods and 4 properties; a reverse check renames one declaration method and
  proves that an otherwise count-preserving drift is rejected. A source-tree
  addon cannot hide a missing platform binary, declaration, or runtime member
  from the published package.

The complete host gate and the native Linux amd64/arm64 addon-and-tarball gates
were rerun after this installed-surface check was added; every requested step
passed with zero skipped groups.

Remaining unsupported Unity/Tuanjie layouts are Core compatibility gaps, not
Node-binding gaps. They remain explicit napi errors until sample-backed Core
support exists. Node stays an optional delivery surface; Rust and Python remain
the formal primary targets.
