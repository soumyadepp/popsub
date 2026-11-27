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

The other workspace crates have `publish = false`: they are the server's internals, shipped inside the binary and the image.

The `crates`, `npm` and `pypi` jobs run in the `release` environment, so they wait for a person to approve them. Every publish step skips a version that is already there, so a job that failed can be rerun after fixing what broke it.

The last job, `verify`, asks each registry for the new version (the two crates, npm, PyPI, the Go module proxy, the image, and the three release downloads) and fails the run if any is still missing after 15 minutes. `scripts/verify-release.sh X.Y.Z` runs the same check from your machine.

## The first release (0.1.0)

Done for this repository on 2026-11-20. The steps stay here for reference, and for anyone setting the pipeline up again, such as on a fork or a new registry. For a release now, see "Every later release" below.

Do this once, in this order. Allow about an hour. You need accounts on crates.io (signed in with GitHub, with a verified email address under https://crates.io/settings/profile, or it refuses to publish), npmjs.com with two-factor authentication on (https://www.npmjs.com/settings/~/tfa, set to cover writes, or it refuses with E403), and pypi.org.

### 1. Merge the release pipeline

The tag has to point at a commit on `main` that contains `.github/workflows/release.yml`.

```bash
git checkout main && git pull
scripts/check-version.sh 0.1.0
```

It prints nothing and exits 0 when every version agrees. Check that the latest CI run on `main` is green.

### 2. Create the `release` environment on GitHub

Do this before pushing the tag. If the environment doesn't exist, GitHub creates it on the fly with no reviewer, and the publish jobs run without waiting.

1. Open https://github.com/soumyadepp/popsub/settings/environments and click **New environment**.
2. Name it `release` and click **Configure environment**.
3. Tick **Required reviewers**, add yourself, and leave **Prevent self-review** unticked.
4. Under **Deployment branches and tags**, choose **Selected branches and tags**, click **Add deployment branch or tag rule**, pick **Tag**, and enter `v*`. Only release tags can then publish.
5. Click **Save protection rules**.

### 3. Add the pending publisher on PyPI

1. Open https://pypi.org/manage/account/publishing/ signed in as yourself. You don't need a PyPI organization.
2. Under **Add a new pending publisher**, choose **GitHub** and fill in:

   | Field | Value |
   |---|---|
   | PyPI Project Name | `popsub` |
   | Owner | `soumyadepp` |
   | Repository name | `popsub` |
   | Workflow name | `release.yml` |
   | Environment name | `release` |

3. Click **Add**.

A pending publisher doesn't reserve the name, so don't leave days between this step and the tag.

### 4. Push the tag

```bash
git tag v0.1.0
git push origin v0.1.0
```

Open https://github.com/soumyadepp/popsub/actions and click the **Release** run. `check`, `binaries`, `image`, `image-manifest` and `github-release` run straight away. `crates`, `npm` and `pypi` show **Waiting**. Don't approve them yet.

If `check` fails, nothing has been published. Delete the tag (`git push origin :refs/tags/v0.1.0` and `git tag -d v0.1.0`), fix it on a branch, merge, and start this step again.

### 5. Publish the two crates by hand

crates.io sets up trusted publishing on a crate that already exists, so 0.1.0 comes from your machine.

1. Open https://crates.io/settings/tokens and click **New Token**. Name it `popsub-0.1.0`, set it to expire tomorrow, tick the scopes **publish-new** and **publish-update**, and limit it to the crates `popsub_protocol` and `popsub_sdk`. Copy the token.
2. From a clean checkout of the tag:

   ```bash
   git checkout v0.1.0
   cargo login
   cargo publish -p popsub_protocol
   cargo publish -p popsub_sdk
   ```

   `cargo login` asks you to paste the token and stores it in `~/.cargo/credentials.toml`. Never put it in the repository, its secrets or a chat.
3. Check https://crates.io/crates/popsub_protocol and https://crates.io/crates/popsub_sdk show 0.1.0.

### 6. Publish the npm package by hand

npm also sets up trusted publishing on a package that already exists.

```bash
cd clients/typescript
npm login
npm ci
npm publish --access public
cd ../..
```

`npm login` opens a browser to sign in, and `npm publish` asks for a two-factor code. npm may hold the first version as a staged publish: the package then shows only a `0.0.0-stage` placeholder until you confirm the version on npmjs.com with your second factor. Check https://www.npmjs.com/package/popsub shows 0.1.0 before going on. Don't use a token with "bypass 2FA" to get around this.

### 7. Turn on trusted publishing for crates.io and npm

So that every later release publishes from the workflow with no token:

1. **crates.io**, for each of `popsub_protocol` and `popsub_sdk`: open the crate's page, then **Settings**, then **Trusted Publishing**, and add a GitHub publisher with repository owner `soumyadepp`, repository `popsub`, workflow `release.yml` and environment `release`.
2. **npm**: open https://www.npmjs.com/package/popsub, then **Settings**, then **Trusted Publisher**, choose **GitHub Actions**, and enter `soumyadepp`, `popsub`, `release.yml` and `release`. On the same page, under **Publishing access**, you can then require two-factor authentication and disallow tokens.
3. Remove the crates.io token: run `cargo logout`, and revoke `popsub-0.1.0` on https://crates.io/settings/tokens.

### 8. Approve the publish jobs

Back on the Release run, click **Review deployments**, tick `release`, and click **Approve and deploy**.

- `crates` and `npm` find 0.1.0 already published and skip it.
- `pypi` publishes and creates the project.
- `verify` then checks every registry. It retries for up to 15 minutes, since registries and the Go module proxy take a few minutes to show a new version. Don't run `scripts/verify-release.sh` before the tag exists: the Go module proxy caches a "not found" for up to about half an hour, and `verify` reports the Go module missing until it expires.

### 9. Make the image public

GitHub may create the container package as private. Open https://github.com/users/soumyadepp/packages/container/package/popsub, then **Package settings**, then **Change visibility**, and choose **Public**. If `verify` failed only on the image, rerun it from the run's page with **Re-run failed jobs**.

### 10. Check it as a user would

In an empty folder:

```bash
cargo add popsub_sdk
npm install popsub
pip install popsub
go get github.com/soumyadepp/popsub/clients/go@v0.1.0
docker pull ghcr.io/soumyadepp/popsub:0.1.0
```

## Every later release

1. On a branch, set the new version in `crates/popsub/Cargo.toml`, `crates/popsub_protocol/Cargo.toml`, `crates/popsub_sdk/Cargo.toml` (both its own version and its `popsub_protocol` dependency), `clients/typescript/package.json` and `package-lock.json`, and `clients/python/pyproject.toml`. Add a `## [X.Y.Z]` section to `CHANGELOG.md`.
2. Run `scripts/check-version.sh X.Y.Z`. It prints every file that disagrees.
3. Run `make upgrade-fixture VERSION=X.Y.Z` and commit the new folder under `crates/popsub/tests/fixtures/upgrade/`. It is the data this release writes, so the upgrade test checks every later release can still open it.
4. Merge the pull request, then tag the merge commit on `main`:

   ```bash
   git checkout main && git pull
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

5. On the Release run, click **Review deployments** and approve `release`. Every registry publishes from the workflow, and `verify` confirms it.

## When something goes wrong

- **A job failed after the publish jobs ran.** Fix the cause and click **Re-run failed jobs**. Publish steps skip versions that are already there.
- **`crates` or `npm` fails with an authentication error.** Check the trusted publisher's owner, repository, workflow and environment match exactly. npm drops a trusted publisher that isn't used for a publish within a couple of days of being added, and 0.1.0 skips npm, so the 0.1.1 release may need it added again.
- **`pypi` fails with "invalid-publisher".** The pending publisher's fields don't match, or someone else registered the name first.
- **A bad version went out.** A version can't be replaced on crates.io, npm or PyPI. Publish the next patch version. You can yank a crate (`cargo yank --version X.Y.Z popsub_sdk`) and deprecate an npm version (`npm deprecate popsub@X.Y.Z "use X.Y.Z+1"`), which stops new installs without breaking existing lock files.
