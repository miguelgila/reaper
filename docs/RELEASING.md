# Releasing Reaper

## How Releases Work

Reaper uses an automated release pipeline. Every PR merge to `main` triggers a patch release automatically. Major and minor releases are triggered manually.

```
PR merges to main                    Manual trigger (Actions UI)
        │                                     │
   auto-release.yml                  manual-release.yml
   (bump patch)                      (bump major/minor/patch)
        │                                     │
        └──── both push commit + tag ─────────┘
              then trigger via workflow_dispatch
                         │
                   release.yml
                   (build, sign, publish)
```

### Automatic Releases (patch)

Every PR merged to `main` triggers the [Auto Release](../.github/workflows/auto-release.yml) workflow, which:

1. Bumps the patch version in `Cargo.toml` (e.g., `0.1.0` → `0.1.1`)
2. Updates `Cargo.lock`
3. Commits with `chore(release): vX.Y.Z [skip ci]`
4. Creates an annotated tag `vX.Y.Z`
5. Pushes commit and tag, then triggers the [release workflow](../.github/workflows/release.yml) via `workflow_dispatch`

To **skip** the auto-release, add the `skip-release` label to your PR before merging.

Use `skip-release` for:
- Documentation-only changes
- CI/CD configuration changes
- Cosmetic changes (formatting, comments)
- Changes that shouldn't produce a new binary release

### Manual Releases (major/minor/patch)

For major or minor version bumps, use the [Manual Release](../.github/workflows/manual-release.yml) workflow:

1. Go to **Actions** → **Manual Release** → **Run workflow**
2. Select the bump type (major, minor, or patch)
3. Optionally enable **dry run** to preview changes without committing
4. Click **Run workflow**

The workflow performs the same steps as auto-release but supports all bump types:
- **major**: `0.1.0` → `1.0.0` (resets minor and patch)
- **minor**: `0.1.0` → `0.2.0` (resets patch)
- **patch**: `0.1.0` → `0.1.1`

### Release Pipeline (build, sign, publish)

Both auto and manual releases trigger the [Release](../.github/workflows/release.yml) workflow via `workflow_dispatch` (not tag push — see [Design Decisions](#design-decisions) below), which:

1. Validates that the tag version matches `Cargo.toml`
2. Builds static musl binaries for x86_64 and aarch64
3. Verifies the version string is embedded in the binaries
4. Packages tarballs with LICENSE and README
5. Generates SHA-256 checksums
6. Signs the checksums with [cosign](https://docs.sigstore.dev/cosign/overview/) (keyless, via GitHub OIDC)
7. Creates a GitHub Release with auto-generated release notes

Monitor at: `https://github.com/miguelgila/reaper/actions/workflows/release.yml`

## Setup

### GitHub App credentials (one-time)

The auto and manual release workflows use a [GitHub App](https://docs.github.com/en/apps) to push version-bump commits and tags, and to trigger `release.yml` via `workflow_dispatch`. This is needed because the default `GITHUB_TOKEN` cannot push to protected branches or trigger downstream workflows. A GitHub App token avoids the expiration headaches of classic PATs and scopes access to just this repository.

1. **Create a GitHub App** (if you haven't already):
   - Go to **Settings** → **Developer settings** → **GitHub Apps** → **New GitHub App**
   - Set **Homepage URL** to the repository URL
   - Uncheck **Webhook → Active**
   - Under **Repository permissions**, set:
     - **Contents** → **Read and write** (push commits and tags)
     - **Actions** → **Read and write** (trigger `release.yml` via `workflow_dispatch`)
   - Click **Create GitHub App** and note the **App ID**

2. **Generate a private key**:
   - On the app's settings page, scroll to **Private keys** → **Generate a private key**
   - A `.pem` file downloads — keep it safe

3. **Install the app on the repository**:
   - Go to the app's **Install App** tab → select your account → **Only select repositories** → choose `reaper`

4. **Add the app to the branch protection bypass list**:
   - Go to repo **Settings** → **Rules** → **Rulesets** → select the ruleset for `main`
   - Under **Bypass list** → **Add bypass** → search for the app name
   - Set to **Always** bypass
   - Save — this allows the app to push release commits directly to `main`

5. **Store credentials as repository secrets**:
   ```bash
   gh secret set APP_ID --body "<your app id>"
   gh secret set APP_PRIVATE_KEY < /path/to/downloaded-key.pem
   ```

The workflows use [`actions/create-github-app-token`](https://github.com/actions/create-github-app-token) to mint a short-lived token on each run — no manual rotation needed.

## Release Artifacts

Each release produces:

| Artifact | Description |
|----------|-------------|
| `reaper-X.Y.Z-x86_64-unknown-linux-musl.tar.gz` | Binaries for x86_64 Linux |
| `reaper-X.Y.Z-aarch64-unknown-linux-musl.tar.gz` | Binaries for aarch64 Linux |
| `checksums-sha256.txt` | SHA-256 checksums for all tarballs |
| `checksums-sha256.txt.sig` | Cosign signature for the checksums file |
| `checksums-sha256.txt.pem` | Signing certificate (GitHub OIDC identity) |

Each tarball contains:
- `containerd-shim-reaper-v2` — the containerd shim binary
- `reaper-runtime` — the OCI runtime binary
- `LICENSE`
- `README.md`

## Installing from a Release

```bash
# Kind cluster (auto-detects architecture)
./scripts/install-reaper.sh --kind my-cluster --release v0.2.0

# Production cluster (defaults to x86_64, use REAPER_TARGET for aarch64)
./scripts/install-reaper.sh --inventory my-inventory.ini --release v0.2.0

# Production cluster (aarch64)
REAPER_TARGET=aarch64-unknown-linux-musl \
  ./scripts/install-reaper.sh --inventory my-inventory.ini --release v0.2.0
```

## Verifying Installed Version

```bash
# On the node where Reaper is installed:
reaper-runtime --version
# reaper-runtime 0.2.0 (abc1234 2026-02-18)

containerd-shim-reaper-v2 --version
# containerd-shim-reaper-v2 0.2.0 (abc1234 2026-02-18)
```

## Verifying Release Signatures

Release artifacts are signed with [cosign](https://docs.sigstore.dev/cosign/overview/) using keyless signing via GitHub Actions OIDC. This proves the binaries were built by the official CI pipeline — no private keys to manage or leak.

### Automatic verification

The install script automatically verifies cosign signatures when `cosign` is available:

```bash
# cosign installed — signature verified automatically
./scripts/install-reaper.sh --kind my-cluster --release v0.2.0

# cosign not installed — skips with a warning, still verifies SHA-256 checksums
./scripts/install-reaper.sh --kind my-cluster --release v0.2.0
```

### Manual verification

```bash
# Download the release files
VERSION=v0.2.0
curl -fsSLO "https://github.com/miguelgila/reaper/releases/download/${VERSION}/checksums-sha256.txt"
curl -fsSLO "https://github.com/miguelgila/reaper/releases/download/${VERSION}/checksums-sha256.txt.sig"
curl -fsSLO "https://github.com/miguelgila/reaper/releases/download/${VERSION}/checksums-sha256.txt.pem"

# Verify the signature
cosign verify-blob \
  --certificate-identity-regexp '.*miguelgila/reaper.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --signature checksums-sha256.txt.sig \
  --certificate checksums-sha256.txt.pem \
  checksums-sha256.txt

# Then verify tarballs against the signed checksums
sha256sum -c checksums-sha256.txt
```

## Rollback

If a release has issues:

1. Delete the GitHub Release (via web UI or `gh release delete vX.Y.Z`)
2. Delete the tag: `git tag -d vX.Y.Z && git push origin :refs/tags/vX.Y.Z`
3. Fix the issue, then re-release

To roll back a deployed version, re-run the install script with the previous version:

```bash
./scripts/install-reaper.sh --kind my-cluster --release v0.1.0
```

## Troubleshooting

### Auto-release didn't trigger after PR merge

- Check if the PR had the `skip-release` label
- Check if the last commit on `main` was already a `chore(release):` commit (loop guard)
- Verify the `APP_ID` and `APP_PRIVATE_KEY` secrets are configured

### Release workflow didn't trigger after version bump

- Verify the GitHub App is installed on the repository with **Contents: read & write** and **Actions: read & write** permissions
- Check that `APP_ID` and `APP_PRIVATE_KEY` secrets are set correctly
- Check the auto/manual release run logs — the "Trigger release workflow" step should show success

### Version bump commit was rejected by branch protection

- The GitHub App must be in the branch protection **bypass list** (see [Setup](#github-app-credentials-one-time))
- Without the bypass, the tag push succeeds but the commit push is rejected, leaving an orphaned tag — delete it with `git push origin :refs/tags/vX.Y.Z` and retry after fixing

### Version bump commit triggered CI workflows

- The commit message should contain `[skip ci]` — check the auto/manual release workflow for the commit step
- Note: `[skip ci]` is respected by GitHub Actions natively

### Concurrent version bumps

- Both auto and manual release share the `version-bump` concurrency group
- Only one version bump can run at a time; the second will queue (not cancel)

## Design Decisions

### Why `workflow_dispatch` instead of tag-push events

The release workflow (`release.yml`) is triggered explicitly via `gh workflow run` rather than relying on `on: push: tags`. This is because:

1. **`GITHUB_TOKEN` doesn't trigger workflows.** GitHub suppresses workflow triggers from pushes made with `GITHUB_TOKEN` to prevent infinite loops.

2. **GitHub App tokens don't generate tag push events.** While App tokens _can_ trigger workflows for branch pushes, tag pushes via `git push origin <tag>` do not generate the `push` event that `on: push: tags` listens for. This is true even when the tag push succeeds — it lands on the remote but no event fires.

3. **`--follow-tags` merges events.** Using `git push --follow-tags` sends the branch and tag in a single operation. GitHub generates a single push event for the branch ref, not a separate one for the tag.

4. **Classic PATs work but have drawbacks.** A classic PAT with `repo` scope _does_ generate tag push events, but classic PATs expire (requiring manual rotation), grant broad access (all repos, not just this one), and are being deprecated in favor of fine-grained PATs and GitHub Apps.

The `workflow_dispatch` approach is explicit, reliable, and works with any token type. The `on: push: tags` trigger is kept as a fallback (e.g., for manual `git push` of a tag with a classic PAT).

### Why a GitHub App instead of a PAT

- **No expiration** — App tokens are minted per-run, so there's nothing to rotate
- **Scoped to one repo** — unlike classic PATs which grant access to all repos
- **Fine-grained permissions** — only Contents and Actions, not full `repo` scope
- **Audit trail** — actions appear as the app, not a personal account

## Version Scheme

Reaper uses [Semantic Versioning](https://semver.org/):

- **0.x.y** — pre-1.0 development phase (breaking changes may occur in minor bumps)
- **1.0.0** — first stable release
- After 1.0: MAJOR for breaking changes, MINOR for features, PATCH for fixes
