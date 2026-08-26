# GitHub Copilot instructions for unity-rs

Read and follow the repository-root `AGENTS.md` before making changes. It is
the canonical instruction source; this file is a Copilot-oriented operational
summary and does not override it.

## Project boundaries

- `unity-rs` is a headless Unity asset parser and exporter implemented in Rust.
- The workspace has exactly four production packages:
  `unity-rs-core`, `unity-rs-cli`, `unity-rs-python`, and `unity-rs-node`.
- Keep Core free of .NET runtime dependencies and handwritten unsafe Rust.
- Do not add a GUI, custom exported C ABI, public context handles, proprietary
  decoders, game keys, or managed runtime dependencies.
- Keep first-party naming in the `unity-rs` family. Use `AssetStudio` only for
  credited upstream projects, the optional managed oracle, or exact upstream
  behavior references.

## Architecture and implementation

- Put parsing, version gates, limits, cross-file resolution, decoding, and
  export semantics in `unity-rs-core`.
- Keep CLI, PyO3, and napi-rs layers thin. Do not implement a format separately
  in each binding.
- Treat every Unity file, path, archive entry, callback result, and schema as
  untrusted input.
- Use checked arithmetic/conversions and caller-configurable individual plus
  cumulative budgets for sizes, counts, strings, allocation, decompression,
  traversal, metadata, and output.
- Use fallible reservation before growing input-derived collections or strings.
- Prefer immutable source-backed `Region` data and bounded streaming writers.
- Never guess an unverified layout from a nearby Unity version. Require sample
  evidence, exact version gates, boundary tests, and an independent oracle when
  practical.
- Preserve distinct error families. Do not turn malformed input into
  `Unsupported`, or an unverified layout into `InvalidData`.
- Preserve atomic output publication, traversal/symlink protection,
  deterministic naming, and no-clobber behavior.
- Do not weaken hostile-input checks or tests just to make one fixture pass.

## Binding contracts

- A public Core change must be classified in both API audits:
  `tools/check_python_api_surface.py` and
  `tools/check_node_api_surface.py`.
- Keep Python runtime exports, `__all__`, and
  `crates/unity-rs-python/python/unity_rs/__init__.pyi` synchronized. Use
  Python 3.9-compatible typing syntax.
- Keep Node Rust exports, generated `index.js`, generated `index.d.ts`, runtime
  tests, package tests, and the strict TypeScript consumer synchronized.
- Regenerate Node declarations with the pinned napi-rs tooling; do not update
  only the Rust class or only the generated declarations.
- Asset-sized Node work belongs in worker `compute`, not event-loop `resolve`.
- Validate callback result shapes and budgets before copying them into Rust.
- Return detached owned binding data; never expose Rust borrows or raw pointers.

## Documentation and compatibility language

- In user-facing documentation and compatibility matrices, label formats,
  versions, platforms, or codecs without representative evidence as
  **Not tested**.
- Do not use **Unsupported** as a maturity/status label for something that is
  merely unverified.
- Keep `Unsupported` when naming the actual Rust error variant, runtime error
  family, `ExportReport.unsupported` field, or a tested rejection contract.
- Distinguish **Not tested**, **Not implemented**, **Intentionally not
  supported**, and **Invalid data**.
- Keep `README.md` concise; record detailed evidence and chronology in
  `REWRITE_STATUS.md`.
- Preserve upstream credits and legal/provenance notices.

## Generated, vendored, and legal files

- Do not edit generated `THIRD_PARTY_LICENSES.txt` files directly. Use
  `python3 tools/generate_dependency_licenses.py` and verify with `--check`.
- Commit relevant `Cargo.lock` and `package-lock.json` updates.
- Preserve licenses and notices for vendored code and fixtures.
- Never commit `target/`, wheels, sdists, `.node` binaries, virtual
  environments, caches, or `corpus/private/` data.
- Avoid unrelated changes to vendored decoders. Keep documented local fixes and
  panic containment intact.

## Verification

Use the required Rust order:

```shell
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

For the main host closeout across Core, bindings, typing, packaging, and the
managed oracle, run:

```shell
python3 tools/local_ci.py --fail-on-skip quality rust python node typing oracle
```

Also run the narrow tests for the changed module first. For output formats, use
the corresponding independent validator in `tools/`; a project-local
writer/reader round trip is not independent evidence. Report optional corpus or
oracle checks as skipped unless they actually ran. Output, security,
cross-compilation, Linux-container, and release changes also require their
corresponding groups from `python3 tools/local_ci.py --list`.

Before committing, run `git diff --check`, inspect the complete diff, and make
sure no build artifacts or unrelated user changes are included.

## Commits and workflows

- Commit subjects use `[Feat]`, `[Fix]`, `[Chore]`, or `[Docs]`, followed by an
  imperative description starting with a capital letter, no trailing period,
  and roughly 70 characters or fewer.
- Copilot attribution, when applicable, must be the standard trailer in the
  commit body after a blank line:
  `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`.
- Never add a free-form `Agent:` line.
- Never rewrite history, amend others' commits, force-push, publish, release,
  tag, or merge without explicit authorization.
- Follow the standardized `CI`, `Release`, and `Docker` workflow rules in
  `AGENTS.md`, including action versions, permissions, triggers, concurrency,
  and canonical filenames. Preserve intentionally package-specific workflows.
