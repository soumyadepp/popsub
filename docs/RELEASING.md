# Releasing PopSub

A release is one version number shared by the server, the Rust crates and every client library. Pushing a tag `vX.Y.Z` runs `.github/workflows/release.yml`, which checks and publishes everything. No registry token is stored anywhere: crates.io, npm and PyPI accept the workflow through trusted publishing (OpenID Connect), and the container image and GitHub release use the workflow's own token.

## What a release publishes

| Where | What | Job |
|---|---|---|
| GitHub Releases | `popsub` binaries for Linux x86_64 and aarch64 and macOS aarch64, each with a `.sha256`, and the notes from `CHANGELOG.md` | `binaries`, `github-release` |
| ghcr.io | `ghcr.io/soumyadepp/popsub`, for `linux/amd64` and `linux/arm64`, tagged `X.Y.Z`, `X.Y` and `latest` | `image`, `image-manifest` |
| crates.io | `popsub_protocol`, then `popsub_sdk` | `crates` |
| npm | `popsub` (the TypeScript client) | `npm` |
| PyPI | `popsub` (the Python client) | `pypi` |
| Go modules | the tag `clients/go/vX.Y.Z`, which is how Go finds a module in a subdirectory | `github-release` |

The last job, `verify`, asks each registry for the new version (the two crates, npm, PyPI, the Go module proxy, the image, and the three release downloads) and fails the run if any is still missing after 15 minutes. `scripts/verify-release.sh X.Y.Z` runs the same check from your machine.

The other workspace crates have `publish = false`: they are the server's internals, shipped inside the binary and the image.

Every publish step skips a version that is already there, so a job that failed can be rerun after fixing what broke it.

## Cutting a release

1. On a branch, set the new version in `crates/popsub/Cargo.toml`, `crates/popsub_protocol/Cargo.toml`, `crates/popsub_sdk/Cargo.toml` (both its own version and its `popsub_protocol` dependency), `clients/typescript/package.json` and `package-lock.json`, and `clients/python/pyproject.toml`. Add a `## [X.Y.Z]` section to `CHANGELOG.md`.
2. Run `scripts/check-version.sh X.Y.Z`. It prints every file that disagrees.
3. Merge the pull request.
4. Tag the merge commit on `main` and push the tag:

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

5. Approve the `release` environment's deployment when GitHub asks, then watch the workflow. The `check` job fails before anything is published if a version disagrees with the tag or CI fails.

A published version can't be replaced on crates.io, npm or PyPI. To fix a bad release, publish the next patch version. A crate can be yanked and an npm version deprecated, which stops new installs without breaking existing lock files.

## One-time setup

Done once by the repository owner, before the first release.

1. **GitHub environment.** In the repository's Settings, Environments, create `release` and add yourself as a required reviewer, so publishing waits for a person to approve it.
2. **PyPI.** On pypi.org, add a pending trusted publisher for the project `popsub`: owner `soumyadepp`, repository `popsub`, workflow `release.yml`, environment `release`. The first release then creates the project. A pending publisher doesn't reserve the name, so do this shortly before tagging.
3. **crates.io and npm.** Trusted publishing is set up on a package that already exists, so version 0.1.0 of each is published by hand once. Push the `v0.1.0` tag first; while the workflow's publish jobs wait for approval, publish from a clean checkout of that tag:

   ```bash
   cargo login
   cargo publish -p popsub_protocol
   cargo publish -p popsub_sdk
   cd clients/typescript && npm login && npm ci && npm publish --access public
   ```

   `cargo login` and `npm login` keep your credentials on your machine only; don't add them to the repository or its secrets. Then configure the trusted publisher on each package: on crates.io under each crate's settings (repository `soumyadepp/popsub`, workflow `release.yml`, environment `release`), and on npmjs.com under the `popsub` package's settings with the same values. Remove the token `cargo login` created once the trusted publishers work. Then approve the `release` environment: the `crates` and `npm` jobs see the versions already published and skip them, and the `pypi` job creates the project.
4. **Container image.** The first push creates the `popsub` package on ghcr.io. If it shows as private under your profile's Packages, change its visibility to public there.
