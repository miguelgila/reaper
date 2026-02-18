# Releasing Reaper

## How Releases Work

Reaper uses an automated release pipeline. Every PR merge to `main` triggers a patch release automatically. Major and minor releases are triggered manually.

```
PR merges to main                    Manual trigger (Actions UI)
        │                                     │
   auto-release.yml                  manual-release.yml
   (bump patch)                      (bump major/minor/patch)
        │                                     │
        └──────── both push v* tag ───────────┘
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
5. Pushes — the tag push triggers the existing [release workflow](../.github/workflows/release.yml)

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

Both auto and manual releases trigger the [Release](../.github/workflows/release.yml) workflow via tag push, which:

1. Validates that the tag version matches `Cargo.toml`
2. Builds static musl binaries for x86_64 and aarch64
3. Verifies the version string is embedded in the binaries
4. Packages tarballs with LICENSE and README
5. Generates SHA-256 checksums
6. Signs the checksums with [cosign](https://docs.sigstore.dev/cosign/overview/) (keyless, via GitHub OIDC)
7. Creates a GitHub Release with auto-generated release notes

Monitor at: `https://github.com/miguelgila/reaper/actions/workflows/release.yml`

## Setup

### `RELEASE_TOKEN` (one-time)

The auto and manual release workflows require a Personal Access Token (PAT) stored as a repository secret named `RELEASE_TOKEN`. This is needed because pushes made with the default `GITHUB_TOKEN` do not trigger downstream workflows (the tag push must trigger `release.yml`).

1. Go to **Settings** → **Developer settings** → **Personal access tokens** → **Fine-grained tokens**
2. Create a token with:
   - **Repository access**: Only this repository
   - **Permissions**: Contents (read and write)
3. Go to the repository → **Settings** → **Secrets and variables** → **Actions**
4. Create a secret named `RELEASE_TOKEN` with the token value

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
- Verify the `RELEASE_TOKEN` secret is configured

### Release workflow didn't trigger after version bump

- The `RELEASE_TOKEN` PAT may have expired — regenerate it
- Pushes with the default `GITHUB_TOKEN` don't trigger downstream workflows; ensure auto/manual release uses `RELEASE_TOKEN`

### Version bump commit triggered CI workflows

- The commit message should contain `[skip ci]` — check the auto/manual release workflow for the commit step
- Note: `[skip ci]` is respected by GitHub Actions natively

### Concurrent version bumps

- Both auto and manual release share the `version-bump` concurrency group
- Only one version bump can run at a time; the second will queue (not cancel)

## Version Scheme

Reaper uses [Semantic Versioning](https://semver.org/):

- **0.x.y** — pre-1.0 development phase (breaking changes may occur in minor bumps)
- **1.0.0** — first stable release
- After 1.0: MAJOR for breaking changes, MINOR for features, PATCH for fixes
