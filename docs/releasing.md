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
| Workflow | `release.yml` |
| Environment | `pypi` |

No long-lived PyPI API token belongs in GitHub secrets. crates.io publishing
uses the `CARGO_REGISTRY_TOKEN` repository secret from the `crates-io`
environment.

## Release sequence

1. Merge the version bump and all release changes to `main` and wait for its
   `CI OK` check.
2. Optionally run the `Release` workflow by hand on `main`: a dry run that
   builds and checks every artifact (six CLI archives, six Node packages, six
   abi3 wheels and the sdist) and publishes nothing.
3. Create and push the signed tag `v<version>` from that exact `main` commit.
4. The tag-triggered `Release` run refuses a tag that differs from the
   workspace `Cargo.toml` or the Node `package.json`, waits for `CI OK` on the
   tagged commit, rebuilds the artifacts, checks the Python distribution set
   (six wheels, one sdist, all with the tag's version), then publishes PyPI and
   crates.io (`unity-rs-core`, then `unity-rs-cli`; versions already on the
   index are skipped) and finally creates the GitHub Release with the CLI
   archives, the Node packages and `SHA256SUMS-<tag>.txt`.

Publishing jobs never run for pull requests, branch pushes, or manual runs.
`tools/check_ci_matrix.py` (run in CI) audits this shape.
