# Versioning, Packaging & Release Plan

Tracking issue: TODO.md line 12 — "Add versioning, packaging and release model and processes"

## Current State

- Version hardcoded as `0.1.0` in `Cargo.toml`
- `reaper-runtime --version` works via clap (shows `reaper-runtime 0.1.0`)
- `containerd-shim-reaper-v2` has no version reporting at all
- CI builds static musl binaries (x86_64 + aarch64) but only as test artifacts
- No git tags, no GitHub releases, no pre-built downloadable binaries
- Installation requires building from source or copying binaries manually

## Design Decisions

### Versioning Scheme: Semantic Versioning (SemVer)

Use `MAJOR.MINOR.PATCH` following SemVer 2.0:
- **0.x.y** — pre-1.0 development (current phase, breaking changes expected)
- **1.0.0** — first stable release (when core features are production-ready)

The Cargo.toml `version` field is the single source of truth.

### Version String Format

Binaries report version as:

```
reaper-runtime 0.2.0 (abc1234 2026-02-18)
containerd-shim-reaper-v2 0.2.0 (abc1234 2026-02-18)
```

Components: `<name> <semver> (<git-short-hash> <build-date>)`

For dev builds (dirty tree): `0.2.0 (abc1234-dirty 2026-02-18)`

### Binary Portability

Static musl binaries are already fully portable — they have no dynamic library
dependencies. A binary built for `x86_64-unknown-linux-musl` runs on any x86_64
Linux kernel (3.2+). Same for `aarch64-unknown-linux-musl`. No special packaging
(deb, rpm) is needed for the initial release model.

### Release Artifacts

Each release produces:
- `reaper-<version>-x86_64-unknown-linux-musl.tar.gz` — both binaries for x86_64
- `reaper-<version>-aarch64-unknown-linux-musl.tar.gz` — both binaries for aarch64
- `checksums-sha256.txt` — SHA-256 checksums for all artifacts

Tarball contents:
```
reaper-0.2.0-x86_64-unknown-linux-musl/
├── containerd-shim-reaper-v2
├── reaper-runtime
├── LICENSE
└── README.md
```

## Implementation Plan

### Phase 1: Version Embedding in Binaries
Status: [x] complete

**1.1 Create build.rs for compile-time metadata**
- [x] Add `build.rs` at project root
- [x] Inject `GIT_HASH` (short commit hash + dirty suffix), `BUILD_DATE` (YYYY-MM-DD)
- [x] Use `cargo:rustc-env` to set env vars available at compile time
- [x] Fall back gracefully when not in a git repo (outputs "unknown")

**1.2 Add version reporting to both binaries**
- [x] `reaper-runtime`: enhanced clap `version` to include git hash and build date
- [x] `containerd-shim-reaper-v2`: added `--version` flag handling before shim loop

**1.3 Remove dummy `reaper` binary target**
- [x] Removed `[[bin]] name = "reaper"` from Cargo.toml
- [x] Deleted `src/main.rs`

### Phase 2: Release GitHub Actions Workflow
Status: [x] complete

**2.1 Create release workflow (`.github/workflows/release.yml`)**
- [x] Trigger: push of tag matching `v*`
- [x] Build matrix: x86_64 + aarch64 using `messense/rust-musl-cross`
- [x] Package binaries into tarballs with LICENSE and README
- [x] Generate SHA-256 checksums
- [x] Create GitHub Release with auto-generated release notes
- [x] Upload tarballs and checksums as release assets

**2.2 Add tag validation**
- [x] Verify git tag version matches `Cargo.toml` version (fail build if mismatch)
- [x] Verify version string is embedded in x86_64 binary

### Phase 3: Installation from Pre-built Artifacts
Status: [x] complete

**3.1 Update install script to support downloading releases**
- [x] Added `--release <version>` flag to `scripts/install-reaper.sh`
- [x] Downloads tarball from GitHub Releases for the node's arch
- [x] Verifies SHA-256 checksum after download
- [x] Extracts and passes to Ansible via `REAPER_BINARY_DIR`
- [x] Existing `--kind` flow preserved for local development

**3.2 Ansible playbook unchanged**
- [x] No changes needed — the install script sets `REAPER_BINARY_DIR` which already
      flows through to Ansible's `local_binary_dir` variable

### Phase 4: Release Process Documentation
Status: [x] complete

**4.1 Document the release process**
- [x] Wrote `docs/RELEASING.md` with step-by-step instructions
- [x] Covers: version bump, tag creation, push, verification
- [x] Includes rollback instructions

**4.2 Update TODO.md**
- [ ] Mark line 12 as complete (pending merge to main)

## Release Workflow

Releases are fully automated. Every PR merge to `main` triggers a patch release unless the PR has the `skip-release` label. Major and minor bumps are triggered manually via the GitHub Actions UI.

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

See [RELEASING.md](RELEASING.md) for full details, setup instructions, and troubleshooting.

## What We're NOT Doing (and Why)

- **No deb/rpm packages** — static musl binaries don't need them; tarballs suffice
- **No container image** — Reaper is a host-level runtime, not a containerized service
- **No changelog generation tool** — GitHub auto-generated release notes are sufficient for now
- **No cargo-release or release-plz** — our own auto-release and manual-release workflows are simpler and purpose-built
