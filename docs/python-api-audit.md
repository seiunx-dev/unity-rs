# Python API audit

Last verified: 2026-08-22.

`assetstudio-rs` is a direct PyO3 binding over `assetstudio-core`.  It does not
load the removed custom C ABI or a .NET assembly.  This document records how
the stable high-level Rust surface is represented in Python and which apparent
one-to-one differences are intentional wrappers rather than missing features.

## Core-to-Python mapping

| Rust `Studio` capability | Python surface |
| --- | --- |
| Path, one-region and named multi-region loading with options | `AssetStudio(...)`, `from_bytes`, `from_memory_files` |
| File, object and resource counts and stable iteration | properties, convenience lists, iterators and bounded pages |
| Source-bound resources and object payloads | bounded `read_resource*`, `read_raw`, `read_text` and specialized readers |
| Scene hierarchy and model planning | `scene`, SplitObjects/Animator candidates and selected-GameObject FBX |
| Textures, sprites, audio/video/font, mesh, shader, material and project settings | corresponding bounded `read_*` methods |
| Animation, controller, avatar, ACL adapter and Live2D | corresponding metadata/document/package readers and caller decoder hooks |
| Atomic collection export and safe recursive extraction | `AssetStudio.export` and module-level `extract` |

The following Rust methods intentionally do not have identically shaped Python
methods:

- borrowed `StudioFile`, `StudioObject` and `StudioResource` views become owned
  Python metadata plus index/key-based bounded reads, so no Rust lifetime crosses
  the extension boundary;
- `write_*(&mut impl Write)` methods become byte-returning `read_*` methods or
  atomic path-based export methods;
- `from_collection`, `collection` and `into_collection` accept, borrow or move
  the low-level Rust `AssetCollection`; they remain Rust ownership escape
  hatches rather than leaking that internal type across PyO3;
- `object_by_index` returns a borrowed Rust `StudioObject`. Python exposes the
  serialized `object_index` in `ObjectInfo` for stable diagnostics and paging,
  while object reads use the managed-compatible `(file_index, path_id)` key;
  duplicate path IDs therefore keep first-match collection semantics instead
  of pretending Python can preserve a Rust borrow;
- low-level provider traits remain explicit Python callables or schema registry
  objects rather than exposing Rust trait objects.

## Enforced evidence

- `tools/check_python_api_surface.py` parses both the Rust high-level source and
  the Python 3.9-compatible stub. All 107 public methods across `Studio`,
  `StudioFile`, `StudioResource` and `StudioObject` must map to a real Python
  symbol or one of the four Rust-only ownership/borrow entries above. It also
  requires every public `AssetStudio` method and property to be used by the
  strict mypy consumer. A newly published but unclassified Core method, a
  missing Python target, or a published but unconsumed Python method fails
  `quality`. `tools/test_python_api_surface.py` runs the current
  107-Core/4-Rust-only and 66-method/4-property pairs and proves all those
  failure directions instead of silently checking an empty surface.
- `tests/installed_wheel.py` compares the installed runtime and shipped `.pyi`
  in both directions and compares every literal default parameter.
- `tests/typecheck_api.py` is compiled by pinned strict mypy as an ordinary
  Python 3.9 caller, including ACL/Oodle callbacks and every public method.
- `tests/python_api.py` exercises the installed wheel with behavioral fixtures,
  budgets and error-family checks; it does not import the Rust source tree.
- Byte-returning adapters that must drive a Core writer use a bounded,
  fallibly-growing buffer. In particular, textured FBX and Cubism
  expression/physics/fade/clip JSON do not write directly to an infallibly
  growing `Vec`; exact limits succeed, one-byte-short limits become
  `ValueError`, and Python allocation failure remains `MemoryError`.
- CPU-heavy Core work runs through `Python::detach`. Schema construction is
  covered separately because it previously validated and converted as many as
  100,000 TypeTree nodes while holding the GIL: the installed-wheel API test
  first proves the one-million-node public limit is checked against the Python
  list before any invalid element is converted, then disables Python's periodic
  thread switch, arms a helper thread, proves it did not run before the Rust
  constructor, and requires it to run while `MonoBehaviourSchema` builds a
  valid 100,000-node registry. Per-node UTF-8 is charged before the fallible
  Rust copy, so rejection never first materializes an unbounded `Vec<String>`.
  The same installed-wheel test builds a 100,000-entry
  `MonoBehaviourSchemas` collection and requires the helper thread to run while
  its shared Core lookup index is constructed; Python object extraction remains
  under the GIL, but the pure-Rust hash/index work does not.
  Programmatic schema construction also uses the JSON document's version
  invariant: an invalid Unity version becomes `ValueError` and cannot leave a
  never-matching entry in the registry.
- Texture2D and Texture2DArray reads keep both decoder work and the complete
  O(pixel bytes) bottom-up-to-display row conversion inside one
  `Python::detach` closure. The attached path receives `DisplayRowPyImage(s)`,
  whose type invariant permits only ownership moves into `RgbaImage` wrappers;
  it does not scan the pixel buffer. `check_python_api_surface.py` validates
  both methods lexically and its negative tests move each conversion outside
  `py.detach` to prove the gate fails. Installed wheel and sdist tests continue
  to compare the resulting single-image and per-layer pixels exactly.
- Public list-shaped inputs no longer rely on PyO3's eager `Vec` extraction.
  `from_memory_files` charges the Python-list count before reading even one
  tuple, then charges names and byte payloads before each fallible copy;
  `MonoBehaviourSchemas` and `CubismMotionTargets` reserve fallibly and convert
  one entry at a time. A schema collection rejects more than 100,000 entries
  before converting any element, retains each reusable Core registry through
  `Arc`, and builds one shared random-keyed index instead of scanning the whole
  Python list for every stripped object. `unity_cn_key` accepts only the documented `bytes | str`
  and copies directly into the exact 16-byte array rather than accepting an
  arbitrary integer sequence. Injected ACL output carries the same
  `AclDecodeLimits` as Core and checks list lengths plus the frame×curve value
  budget before converting any float or index element. Core separately checks
  request-declared frames, curves, and scalar values before invoking the
  callback, so an asset cannot ask Python to allocate output that is already
  known to exceed the caller's limits.

Remaining unsupported Unity/Tuanjie layouts are Core compatibility gaps, not
Python-binding gaps.  They stay explicit `NotImplementedError` translations
until sample-backed Core support exists.
