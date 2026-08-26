# Releasing unity-rs

Release versions are shared by the Rust workspace, Python distribution, and
Node package. Update every synchronized version and generated binding file in
the same pull request before creating a tag.

## Python trusted publisher

The `unity-rs` PyPI project uses GitHub Actions Trusted Publishing. The PyPI
publisher must have these exact values:

| Field | Value |
| --- | --- |
| PyPI project | `unity-rs` |
| Owner | `seiunx-dev` |
| Repository | `unity-rs` |
| Workflow | `ci.yml` |
| Environment | `pypi` |

For the first release, configure these values as a pending publisher before
pushing the tag. No long-lived PyPI API token belongs in GitHub secrets.

## Release sequence

1. Merge the version bump and all release changes to `main`.
2. Run `Release Crates` with the same version and wait for the Rust packages to
   reach the crates.io index.
3. Create and push `v<version>` from the exact tested `main` commit.
4. The tag-triggered `CI` run builds and tests six Python wheels plus one source
   distribution. After every release gate passes, the `publish-python` job
   validates the tag, downloads those same-run artifacts, and publishes them
   to PyPI through OIDC.

The PyPI job does not run for pull requests, branch pushes, or manual CI runs.
