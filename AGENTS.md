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
  One sanctioned exception exists: for a standard-Unity version **above** a
  class's verified ceiling, the default runtime behavior is to attempt the
  newest known layout (with `strict_unity_versions` restoring rejection).
  Verified-range claims still move only with fixtures, and this exception
  never extends to versions below a floor, Tuanjie builds, stripped versions,
  or container/format gates.
- Preserve existing public behavior unless the task explicitly authorizes a
  breaking change. Keep Rust, CLI, Python, and Node behavior aligned.
- Make the smallest coherent change. Do not opportunistically rewrite nearby
  code, generated files, vendored sources, or user-owned worktree changes.
- Never weaken a limit, validation, test, or error distinction merely to make a
  sample pass. Fix the cause or document the missing evidence. The
  above-ceiling lenient path is policy, not a weakened validation: a failure
  there must surface as `Unsupported` carrying the inner diagnostic verbatim.
- Do not add silent fallbacks that turn corrupted or unknown input into
  plausible-looking output. Above-ceiling lenient parsing is not such a
  fallback: it either fully succeeds with the newest known layout or fails
  with an explicit `Unsupported` naming the attempt.
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
  reject it rather than partially parsing it as success. Strict rejection
  applies to below-floor versions, Tuanjie builds outside their verified
  range, stripped versions, and container/format gates. A standard-Unity
  version above a class's verified ceiling is parsed with the newest known
  layout by default (`strict_unity_versions` restores rejection); such a
  lenient parse either fully succeeds or fails — never partial success.
- Keep error families meaningful: malformed bytes are `InvalidData`, missing
  verified support is `Unsupported`, resource limits are limit errors, and I/O
  failures remain I/O failures. Do not collapse them into one generic error.
  One addition for the lenient path: an above-ceiling parse failure —
  including end-of-input and budget diagnostics reached through untrusted
  above-ceiling counts — is reported as `Unsupported` with the original
  message preserved, because the layout, not the bytes, is the suspect.
- Exports and extraction must keep traversal protection, symlink rejection,
  bounded names, same-directory temporary files, atomic publication, and
  no-clobber semantics unless overwrite was explicitly requested.
- Production code must not use `todo!`, `unimplemented!`, or input-reachable
  panics as compatibility handling.

## Format work and evidence

- New format/version support needs a representative fixture, boundary and
  malformed-input tests, and an independent source of truth where practical.
  Lenient above-ceiling parsing is not verification: widening a documented
  verified ceiling still requires bringing a fixture with it, as the
  sprite-atlas and animation-stack 6000.3 precedents did.
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
- A standard-Unity version above a class's verified ceiling remains
  **Not tested**. Document it as "attempted with the newest known layout by
  default; failures return `Unsupported`", never as supported.
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
  - Claude:
    `Co-authored-by: Claude Opus 5 <noreply@anthropic.com>` (substitute the
    actual model, for example `Claude Sonnet 4.6` or `Claude Haiku 4.5`).
  - Codex: `Co-authored-by: Codex <noreply@openai.com>`
  - Copilot:
    `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`

Examples from this repository's history:

```text
[Feat] Read one-byte type tree arrays as byte arrays (#37)
[Fix] Read typetree char at its declared byte size (#28)
[Chore] Prepare 0.6.0 release (#38)
[Docs] Codify the default-lenient version ceiling policy
```

## GitHub Actions workflows

CI reuses the shared templates in
[`seiunx-dev/ci-templates`](https://github.com/seiunx-dev/ci-templates) at `@v1`.
The files in `.github/workflows` are thin callers; jobs that no template covers are
written in the caller with a comment saying why.

- `ci.yml` (`CI`) runs on `main` pushes, pull requests targeting `main`, and manual
  dispatch:
  - `Rust` (`rust-ci`, toolchain from `rust-toolchain.toml` = MSRV 1.88): fmt, clippy
    `--workspace --all-targets -D warnings`, the workspace tests once under
    `cargo llvm-cov`, and, in the lint job, the structural audits and their self-tests
    (local-CI policy, Python/Node API surface, `check_ci_matrix.py`), warning-free
    rustdoc, `cargo package -p unity-rs-core` + `check_core_package.py`, the
    third-party license bundle and the delivery-scope audit.
  - `Rust (Windows)` / `Rust (macOS)` (`rust-ci`, `lint: false`): the workspace tests
    on the other two desktop OSes.
  - `cargo audit` (RustSec, `--deny unsound --deny yanked`), `Node-API` (debug addon,
    API/type tests and package checks on three OSes).
  - `Python wheels` (`maturin-wheels`): the abi3 wheel built in the manylinux_2_28
    container on Linux, installed into Python 3.14 and run through
    `tests/installed_wheel.py` and `tests/python_api.py`. Pull requests build
    linux-x64 and macos-arm64; `main` and dispatch runs build all six platforms.
  - `Python 3.9 floor and sdist`: the floor wheel with both suites, the strict mypy
    consumer, and the source distribution (contents check, a wheel rebuilt from it
    and both suites against that wheel).
  - `Differential oracles` (`scripts/ci/run-oracles.sh`: AssetStudio/.NET, vgmstream,
    UnityPy on one build), `Python tool coverage`, `Sonar` (skipped green on
    Dependabot/fork PRs), `Workflow lint` (actionlint).
- The aggregate job **`CI OK`** is the only required status check.
- `release.yml` (`Release`) replaces the tag-triggered half of the old `ci.yml` and
  the manual `release-crates.yml`. Bump the shared version (workspace
  `Cargo.toml`, `crates/unity-rs-node/package.json`, generated bindings) in a PR →
  merge and wait for `CI OK` on `main` → push the tag `v<version>`. `release-gate`
  refuses a tag that differs from `Cargo.toml` or `package.json` and waits for
  `CI OK` on the tagged commit; then, in one run:
  - `CLI binaries` (`rust-release`): `unity-rs-cli-<version>-<label>.tar.gz` / `.zip`
    for the six platforms, each staged by `tools/stage_cli_artifact.py` (binary,
    license, notices, third-party licenses at the archive root) and smoke-tested with
    `--help` before packaging;
  - `Python wheels` (`maturin-wheels`): six abi3 wheels with the same tests as CI,
    plus the sdist; `Python distribution set` checks there are six wheels and one
    sdist carrying the version;
  - `Node package`: release addon, tests, package checks and `npm pack`, published as
    `unity-rs-node-<version>-<label>.tgz`;
  - PyPI (trusted publishing, workflow `release.yml`, environment `pypi`), crates.io
    (`unity-rs-core` then `unity-rs-cli`, environment `crates-io`,
    `CARGO_REGISTRY_TOKEN`), and the GitHub Release with the CLI archives, the Node
    packages and `SHA256SUMS-<tag>.txt`.
  Manual dispatch is a dry run: it builds and checks every artifact and publishes
  nothing. See `docs/releasing.md`.

Workflow maintenance rules:

- Use the shared templates first. Add custom jobs or steps only when a template
  genuinely cannot meet the project's needs, keep them in the thin caller files, and
  add a comment explaining why.
- Template bugs and missing features are fixed upstream in `seiunx-dev/ci-templates`
  (new `v1.x.y` tag), not worked around here.
- Workflow moves must be atomic with their structural audits: update
  `tools/check_ci_matrix.py`, `tools/test_ci_matrix.py`, local-CI orchestration,
  artifact paths, and documentation whenever a job, target or artifact changes.
- Keep top-level `permissions: contents: read`; grant `contents: write` /
  `id-token: write` only on the job that needs it.
- Do not set `*.reportPaths` in `sonar-project.properties` and do not suppress
  `githubactions:S7637` there: the template's `sonar.yml` passes the report paths and
  ignores S7637 for the `@v1` references.
- Third-party actions in caller-side custom steps are pinned to a full commit SHA with a
  `# vX.Y.Z` comment; Dependabot (`github-actions`) updates them and the template refs.
- Do not add a placeholder `docker.yml`: the repository has no Docker build input.

## Release notes

GitHub Releases follow the org standard,
[RELEASE_NOTES.md](https://github.com/seiunx-dev/ci-templates/blob/main/RELEASE_NOTES.md),
and are written in English.

- Title every release with the tag only, for example `v0.6.0`.
- Publish a tag as a pre-release if and only if it has an `-alpha`, `-beta` or
  `-rc` suffix; every tag gets a release.
- Omit empty sections, and end every item with its PR number `(#123)` (the
  short commit SHA when there is no PR).
- After the `Release` workflow publishes a release with auto-generated notes,
  rewrite them to the standard with `gh release edit <tag> --notes-file <file>`.
