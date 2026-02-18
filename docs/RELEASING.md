# Releasing Reaper

This document describes how to create a new Reaper release.

## Prerequisites

- Push access to the `main` branch
- GitHub CLI (`gh`) installed (optional, for verifying releases)

## Release Process

### 1. Update the version

Edit `Cargo.toml` and update the `version` field:

```toml
[package]
version = "0.2.0"  # ← update this
```

### 2. Commit the version bump

```bash
git add Cargo.toml
git commit -m "chore: bump version to 0.2.0"
```

### 3. Create an annotated tag

```bash
git tag -a v0.2.0 -m "Release v0.2.0"
```

### 4. Push the commit and tag

```bash
git push origin main --tags
```

### 5. Verify the release

The [release workflow](../.github/workflows/release.yml) will automatically:

1. Validate that the tag version matches `Cargo.toml`
2. Build static musl binaries for x86_64 and aarch64
3. Verify the version string is embedded in the binaries
4. Package tarballs with LICENSE and README
5. Generate SHA-256 checksums
6. Sign the checksums file with [cosign](https://docs.sigstore.dev/cosign/overview/) (keyless, via GitHub OIDC)
7. Create a GitHub Release with auto-generated release notes

Monitor the workflow at: `https://github.com/miguelgila/reaper/actions/workflows/release.yml`

Once complete, verify the release:

```bash
# List releases
gh release list

# View release assets
gh release view v0.2.0
```

## Release Artifacts

Each release produces:

| Artifact | Description |
|----------|-------------|
| `reaper-0.2.0-x86_64-unknown-linux-musl.tar.gz` | Binaries for x86_64 Linux |
| `reaper-0.2.0-aarch64-unknown-linux-musl.tar.gz` | Binaries for aarch64 Linux |
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

1. Delete the GitHub Release (via web UI or `gh release delete v0.2.0`)
2. Delete the tag: `git tag -d v0.2.0 && git push origin :refs/tags/v0.2.0`
3. Fix the issue, then re-release

To roll back a deployed version, re-run the install script with the previous version:

```bash
./scripts/install-reaper.sh --kind my-cluster --release v0.1.0
```

## Version Scheme

Reaper uses [Semantic Versioning](https://semver.org/):

- **0.x.y** — pre-1.0 development phase (breaking changes may occur in minor bumps)
- **1.0.0** — first stable release
- After 1.0: MAJOR for breaking changes, MINOR for features, PATCH for fixes
