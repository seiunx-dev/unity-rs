# Agent guidelines

These rules apply to the entire repository.

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
