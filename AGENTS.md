# Agent guidelines

These rules apply to the entire repository.

## Canonical instructions

- This file is the canonical repository-wide instruction source for coding
  agents and human contributors.
- `.github/copilot-instructions.md` adapts these rules for GitHub Copilot.
- `CLAUDE.md` imports this file instead of duplicating it.
- A more deeply nested `AGENTS.md`, if one is added later, may refine rules for
  that subtree but must not weaken repository-wide safety, testing, licensing,
  or delivery constraints.

## Project mission and scope

`unity-rs` is a headless Unity asset reader, inspector, extractor, and
exporter. The supported delivery surfaces are:

| Surface | Package or identifier |
| --- | --- |
| Rust library | `unity-rs-core` / `unity_rs_core` |
| Native CLI | `unity-rs-cli` / `unity-rs` |
| Python | distribution `unity-rs`, import `unity_rs`, class `UnityRs` |
| Node.js | package `unity-rs-node`, class `UnityRs` |

The repository intentionally does not ship a GUI, managed runtime, custom C
ABI, public context handles, proprietary decoder binaries, or embedded game
keys. Do not reintroduce those surfaces. Optional Oodle, ACL, UnityCN, and
managed-schema capabilities must remain behind explicit caller-supplied data or
adapters.

Use `unity-rs` naming for first-party code. `AssetStudio` may appear only when
referring to the credited upstream projects, the managed differential oracle,
or an exact upstream behavior contract.

## Repository map

- `crates/unity-rs-core`: safe Rust parsing, resolution, decoding, and export.
- `crates/unity-rs-cli`: native command-line frontend.
- `crates/unity-rs-python`: PyO3 `abi3-py39` binding and Python typing surface.
- `crates/unity-rs-node`: napi-rs binding and TypeScript declarations.
- `oracle`: checked managed differential harness; never a runtime dependency.
- `corpus`: opt-in real-asset acceptance harness. Private corpus data stays
  under ignored `corpus/private/` and must never be committed.
- `tools`: API, package, output, license, CI, and differential audits.
- `README.md`: concise user-facing capabilities and setup.
- `REWRITE_STATUS.md`: detailed evidence, history, current gaps, and next work.
- `THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt`: redistribution and
  dependency records.

## Change principles

- Treat Unity files, archive names, paths, callbacks, and schemas as untrusted
  input.
- Prefer a narrow, verifiable implementation over a speculative compatibility
  claim. Do not infer an undocumented layout from an adjacent Unity version.
- Preserve existing public behavior unless the task explicitly authorizes a
  breaking change. Keep Rust, CLI, Python, and Node behavior aligned.
- Make the smallest coherent change. Do not opportunistically rewrite nearby
  code, generated files, vendored sources, or user-owned worktree changes.
- Never weaken a limit, validation, test, or error distinction merely to make a
  sample pass. Fix the cause or document the missing evidence.
- Do not add silent fallbacks that turn corrupted or unknown input into
  plausible-looking output.
- Do not rewrite Git history, amend another contributor's commit, move tags,
  publish packages, create releases, merge pull requests, or force-push unless
  the user explicitly requests that exact operation and scope.

## Rust and parser requirements

- The minimum supported Rust version is 1.88, pinned by
  `rust-toolchain.toml`; the workspace uses edition 2024.
- Keep the workspace dependency graph locked. Use `--locked` in documented and
  CI build/test commands.
- Core, CLI, and Python inherit the workspace `unsafe_code = "forbid"` policy.
  The Node crate overrides that lint only for audited napi-rs registration
  glue; do not add handwritten unsafe code there.
- Use checked arithmetic and checked conversions for input-derived offsets,
  sizes, counts, alignment, strides, and capacities.
- Bound individual and cumulative input, allocation, decompression, traversal,
  string, metadata, and output work. Avoid unbounded `read_to_end`, eager
  directory collection, and infallible growth from attacker-controlled counts.
- Prefer immutable source-backed `Region` values and bounded streaming writers
  to whole-file copies or shared mutable cursors.
- Use fallible reservation before growing large `Vec`, `String`, map, set, or
  result-table allocations derived from input.
- Validate a complete known layout. If a tail or version gate is not verified,
  reject it rather than partially parsing it as success.
- Keep error families meaningful: malformed bytes are `InvalidData`, missing
  verified support is `Unsupported`, resource limits are limit errors, and I/O
  failures remain I/O failures. Do not collapse them into one generic error.
- Exports and extraction must keep traversal protection, symlink rejection,
  bounded names, same-directory temporary files, atomic publication, and
  no-clobber semantics unless overwrite was explicitly requested.
- Production code must not use `todo!`, `unimplemented!`, or input-reachable
  panics as compatibility handling.

## Format work and evidence

- New format/version support needs a representative fixture, boundary and
  malformed-input tests, and an independent source of truth where practical.
- Prefer real sample-backed version gates. Synthetic fixtures must encode the
  actual layout, endianness, absolute alignment, PathID width, and resource
  offsets rather than mirror only the reader's assumptions.
- The managed oracle is optional test infrastructure. Runtime crates must not
  depend on .NET or the upstream checkout.
- A comparison is only independent when the other implementation does not
  share the same translated decoder or assumptions. Record intentional oracle
  differences explicitly.
- Preserve fixture provenance and licensing. Do not commit proprietary game
  files, keys, Autodesk/FMOD/Oodle binaries, or material without redistribution
  permission.
- Fuzz/malformed tests must assert stable errors and absence of panics; they
  must not merely prove that a function returned.

## Public API and binding parity

- `unity-rs-core` owns parsing, limits, resolution, and format semantics. CLI,
  Python, and Node should be thin adapters rather than independent parsers.
- A public Core API change must classify its Python and Node disposition in
  `tools/check_python_api_surface.py` and `tools/check_node_api_surface.py`.
- Python runtime exports, `__all__`, and
  `crates/unity-rs-python/python/unity_rs/__init__.pyi` must agree. Keep Python
  3.9-compatible typing syntax and run the strict consumer tests.
- Node Rust exports, generated `index.js`/`index.d.ts`, package contents, and
  the strict TypeScript consumer must agree. Work proportional to asset input
  belongs in napi-rs worker `compute`, not event-loop `resolve`.
- Boundary copies from Python/Node must validate lengths first and use fallible
  allocation. Caller callbacks must be shape-, length-, and budget-checked
  before their results enter Core.
- Preserve detached/owned binding results; do not expose Rust borrows, raw
  pointers, numeric context handles, or a replacement custom C ABI.

## Generated and synchronized files

- Regenerate Node `index.js` and `index.d.ts` with the pinned napi-rs build
  tooling after changing exported Node symbols. Do not hand-edit only one side.
- Treat the Python `.pyi` file as a checked public contract and update it with
  the PyO3 surface.
- Commit `Cargo.lock` and `package-lock.json` changes that accompany dependency
  updates.
- `THIRD_PARTY_LICENSES.txt` and package copies are generated/synchronized by
  `tools/generate_dependency_licenses.py`. Run it after dependency or legal
  input changes; do not manually patch generated license bundles.
- Preserve upstream licenses and notices when modifying vendored code. Keep
  local modifications documented in `docs/upstream-defects.md` when relevant.
- Never commit build outputs such as `target/`, wheels, sdists, `.node` files,
  virtual environments, caches, or private corpus files.

## Documentation terminology

- Use **Not tested** for a format, engine version, platform, or codec that lacks
  representative samples or independent verification. This is the standard
  user-facing compatibility-matrix status.
- Do not use **Unsupported** as a documentation maturity/status label merely
  because support has not been verified.
- Use `Unsupported` only when referring to the exact Rust error variant,
  runtime error family, or an observed rejection contract. Put the identifier
  in code formatting and explain separately that the capability is **Not
  tested** when that is the underlying evidence state.
- Distinguish **Not tested** from **Not implemented**, **Intentionally not
  supported**, and **Invalid data**. Do not imply that untested input works.
- Keep `README.md` concise and user-facing. Put detailed chronology, evidence,
  limits, and remaining work in `REWRITE_STATUS.md`, and keep its last-updated
  date accurate when materially changing it.
- Preserve upstream credits to
  `https://github.com/aelurum/AssetStudioMod` and
  `https://github.com/Perfare/AssetStudio`.

## Verification

Run tests in proportion to the change. The canonical Rust order is:

```shell
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

For the main host closeout across Core, bindings, typing, packaging, and the
managed oracle, run the repository orchestrator with no skipped groups:

```shell
python3 tools/local_ci.py --fail-on-skip quality rust python node typing oracle
```

Additional guidance:

- Core/parser changes: run the narrow module tests first, then workspace
  Clippy and tests.
- CLI changes: run `cargo test -p unity-rs-cli --all-targets --locked` plus the
  relevant process-level integration test.
- Python changes: run the `python` and `typing` local-CI groups; verify both the
  wheel and sdist installed surfaces.
- Node changes: from `crates/unity-rs-node`, run `npm ci`,
  `npm run build:debug`, `npm test`, and package-content tests as applicable.
- Dependency/legal changes: run
  `python3 tools/generate_dependency_licenses.py --check` and package audits.
- Output-format changes: run the corresponding independent validator in
  `tools/`, not only a writer/reader round trip implemented by this project.
- Corpus tests are opt-in and may require private data. Report them as skipped
  unless the required corpus was actually present; never claim they passed.
- Output, security, cross-compilation, Linux-container, or release changes must
  also run their corresponding groups from `python3 tools/local_ci.py --list`;
  the main host command above does not replace those specialized gates.
- Before committing, run `git diff --check` and confirm the worktree contains
  no unrelated or generated artifacts.

## Git commits

All commit subjects must follow:

```text
[Type] Short description starting with capital letter
```

Allowed types:

| Type | Usage |
| --- | --- |
| `[Feat]` | New feature or capability |
| `[Fix]` | Bug fix |
| `[Chore]` | Maintenance, refactoring, dependency or build changes |
| `[Docs]` | Documentation-only changes |

Rules:

- Description starts with a capital letter.
- Use imperative mood: `Add ...`, not `Added ...`.
- No trailing period.
- Keep the subject at or below roughly 70 characters.
- **Agent attribution uses the standard Git `Co-authored-by:` trailer in the
  commit body, not a free-form `Agent:` line.** This makes GitHub render the
  co-author avatar on the commit page. The trailer must be on its own line,
  separated from the subject by a blank line, in the form
  `Co-authored-by: <Display Name> <email>`. Suggested values per agent:
  - Claude (any 4.x):
    `Co-authored-by: Claude Opus 5 <noreply@anthropic.com>` (substitute the
    actual model, for example `Claude Sonnet 4.6` or `Claude Haiku 4.5`).
  - Codex: `Co-authored-by: Codex <noreply@openai.com>`
  - Copilot:
    `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`

Examples from this repository's history:

```text
[Feat] Add configurable asset export types
[Fix] Nuverse parse issue
[Chore] Update dependencies
[Feat] Replace git2 with git CLI and add commit signing (#16)
```

## GitHub Actions workflows

Use the standardized workflow layout in `.github/workflows`:

- `ci.yml` runs on `main` pushes, pull requests targeting `main`, and manual
  dispatch.
- Rust CI order: `cargo fmt --all -- --check`,
  `cargo check --locked --all-targets`,
  `cargo clippy --locked --all-targets -- -D warnings`, then
  `cargo test --locked`.
- `release.yml` is the standard release build entrypoint. It runs on `v*` tags
  and manual dispatch, builds release artifacts, uploads them with
  `actions/upload-artifact`, and publishes GitHub Release assets on tag pushes.
- `docker.yml` is the standard Docker entrypoint. It runs on `main` pushes,
  `v*` tags, pull requests that touch Docker/build inputs, and manual dispatch.
  Pull requests build only; non-pull-request runs push GHCR images with
  lowercase image names and Docker metadata tags.

Workflow maintenance rules:

- Keep workflow filenames and top-level names aligned: `CI`, `Release`,
  `Docker`, and optional package-specific names.
- Use `actions/checkout@v6`, `actions/setup-go@v6`,
  `actions/upload-artifact@v7`, `actions/download-artifact@v8`,
  `softprops/action-gh-release@v3`, and current Docker actions
  (`setup-buildx@v4`, `login@v4`, `metadata@v6`, `build-push@v7`).
- Keep `permissions` minimal: `contents: read` for CI/Docker build-only work,
  `contents: write` for release publishing, and `packages: write` only when
  pushing container images.
- Use workflow `concurrency` keyed by workflow name and ref, with release jobs
  using `release-${{ github.ref_name }}` and `cancel-in-progress: false`.
- Do not reintroduce legacy workflow names such as `rust-ci.yml`, `build.yml`,
  `release-build.yml`, `docker-build.yml`, or `docker-release.yml` unless a
  package-specific workflow already exists and is intentionally preserved.
- Workflow moves must be atomic with their structural audits. In particular,
  update `tools/check_ci_matrix.py`, `tools/test_ci_matrix.py`, local-CI
  orchestration, artifact paths, and documentation whenever release jobs move
  between workflow files. Do not add a placeholder `docker.yml` when the
  repository has no Docker build input.
