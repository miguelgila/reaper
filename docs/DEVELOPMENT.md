# Development Guide

This document contains information for developers working on the Reaper project.

## Table of Contents

- [Development Setup](#development-setup)
- [Building](#building)
- [Testing](#testing)
- [Code Quality](#code-quality)
- [Git Hooks](#git-hooks)
- [Docker (Optional)](#docker-optional)
- [VS Code Setup](#vs-code-setup)
- [CI/CD](#cicd)
- [Coverage](#coverage)
- [Contributing](#contributing)

## Development Setup

### Prerequisites

- **Rust toolchain** (we pin `stable` via `rust-toolchain.toml`)
- **Docker** (optional, for Linux-specific testing on macOS)
- **Ansible** (for deploying to clusters)

### Clone and Build

```bash
git clone https://github.com/miguelgila/reaper
cd reaper
cargo build
```

The repository includes `rust-toolchain.toml` which automatically pins the Rust toolchain version and enables `rustfmt` and `clippy` components.

## Building

### Local Build (Debug)

```bash
cargo build
```

Binaries are output to `target/debug/`.

### Release Build

```bash
cargo build --release
```

Binaries are output to `target/release/`.

### Static Musl Build (for Kubernetes deployment)

For deployment to Kubernetes clusters, we build static musl binaries:

```bash
# Install musl target (one-time setup)
rustup target add x86_64-unknown-linux-musl

# Build static binary
docker run --rm \
  -v "$(pwd)":/work \
  -w /work \
  messense/rust-musl-cross:x86_64-musl \
  cargo build --release --target x86_64-unknown-linux-musl
```

This produces binaries at `target/x86_64-unknown-linux-musl/release/` that work in containerized environments (like Kind nodes).

For aarch64:
```bash
rustup target add aarch64-unknown-linux-musl

docker run --rm \
  -v "$(pwd)":/work \
  -w /work \
  messense/rust-musl-cross:aarch64-musl \
  cargo build --release --target aarch64-unknown-linux-musl
```

## Testing

See [TESTING.md](TESTING.md) for comprehensive testing documentation.

### Quick Reference

```bash
# Unit tests (fast, recommended for local development)
cargo test

# Full integration tests (Kubernetes + unit tests)
./scripts/run-integration-tests.sh

# Integration tests (K8s only, skip cargo tests)
./scripts/run-integration-tests.sh --skip-cargo

# Coverage report (requires Docker)
./scripts/docker-coverage.sh
```

### Test Modules

- `tests/integration_basic_binary.rs` - Basic runtime functionality (create/start/state/delete)
- `tests/integration_user_management.rs` - User/group ID handling, umask
- `tests/integration_shim.rs` - Shim-specific tests
- `tests/integration_io.rs` - FIFO stdout/stderr redirection
- `tests/integration_exec.rs` - Exec into running containers
- `tests/integration_overlay.rs` - Overlay filesystem tests

Run a specific test suite:

```bash
cargo test --test integration_basic_binary
```

## Code Quality

### Formatting

Format all code before committing:

```bash
cargo fmt --all
```

Check formatting without making changes:

```bash
cargo fmt --all -- --check
```

### Linting

Run clippy to catch common mistakes and improve code quality:

```bash
# Quick check
cargo clippy --all-targets --all-features

# Match CI exactly (treats warnings as errors)
cargo clippy -- -D warnings
```

CI runs clippy with `-D warnings`, so any warning is a hard failure. The pre-push hook runs this automatically if you've installed hooks via `./scripts/install-hooks.sh`.

### Linux Cross-Check (macOS only)

The overlay module (`src/bin/reaper-runtime/overlay.rs`) is gated by `#[cfg(target_os = "linux")]` and doesn't compile on macOS. To catch compilation errors in Linux-only code:

```bash
# One-time setup
rustup target add x86_64-unknown-linux-gnu

# Check compilation for Linux target
cargo clippy --target x86_64-unknown-linux-gnu --all-targets --all-features
```

## Git Hooks

We provide git hooks in `.githooks/` to catch issues before they reach CI.

### Enable Hooks

```bash
./scripts/install-hooks.sh
```

This sets `core.hooksPath` to `.githooks/` and marks the hooks executable. Since the hooks are checked into the repo, every contributor gets the same setup.

### Available Hooks

| Hook | Runs | Purpose |
|------|------|---------|
| `pre-commit` | `cargo fmt --all` | Auto-formats code and stages changes before each commit |
| `pre-push` | `cargo clippy -- -D warnings` | Catches lint issues before pushing (matches CI) |

The pre-push hook mirrors the exact clippy invocation used in CI, so pushes that pass locally will pass the CI clippy check too.

### Customization

- **pre-commit**: To fail on unformatted code instead of auto-fixing, change `cargo fmt --all` to `cargo fmt --all -- --check` and remove the re-staging logic.
- **pre-push**: To skip clippy for a one-off push, use `git push --no-verify`.

## Docker (Optional)

Docker is **not required** for local development on macOS. Prefer `cargo test` locally for speed.

Use Docker when you need:
- Code coverage via `cargo-tarpaulin` (Linux-first tool)
- CI failure reproduction specific to Linux
- Static musl binary builds for Kubernetes

### Run Coverage in Docker

```bash
./scripts/docker-coverage.sh
```

This runs `cargo-tarpaulin` in a Linux container with appropriate capabilities.

## VS Code Setup

### Recommended Extensions

- **rust-analyzer** — Main Rust language support
- **CodeLLDB** (vadimcn.vscode-lldb) — Debug adapter for Rust
- **Test Explorer UI** — Unified test UI

Configure rust-analyzer to run clippy on save and enable CodeLens for inline run/debug buttons.

## CI/CD

GitHub Actions workflows run on pushes and pull requests to `main`:

### Tests Workflow (`test.yml`)

- Builds and caches dependencies
- Runs `cargo test` with coverage via `cargo-tarpaulin`
- Uploads coverage to Codecov
- Runs `cargo clippy -- -D warnings`
- Checks formatting with `cargo fmt --all -- --check`

### Build and Audit Workflow (`build.yml`)

- Runs `cargo audit` to scan dependencies for known vulnerabilities

### Integration Workflow (`integration.yml`)

- Builds static musl binaries (architecture-aware)
- Creates a Kind cluster
- Deploys Reaper via Ansible
- Runs the full Kubernetes integration test suite

## Coverage

### Local Coverage (Linux)

If running on Linux, you can use tarpaulin directly:

```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Xml --timeout 600
```

### Coverage via Docker (macOS/Windows)

Run the included Docker script:

```bash
./scripts/docker-coverage.sh
```

Configuration lives in `tarpaulin.toml`. Functions requiring root + Linux namespaces (tested by kind-integration) are excluded via `#[cfg(not(tarpaulin_include))]` so coverage reflects what unit tests can actually reach.

## Contributing

### Before Opening a PR

1. **Format code:**
   ```bash
   cargo fmt --all
   ```

2. **Run linting:**
   ```bash
   cargo clippy --all-targets --all-features
   ```

3. **Run tests:**
   ```bash
   cargo test
   ```

4. **Optional: Run integration tests:**
   ```bash
   ./scripts/run-integration-tests.sh
   ```

5. **Install git hooks** (auto-formats on commit, runs clippy before push):
   ```bash
   ./scripts/install-hooks.sh
   ```

### Development Workflow

For fast feedback during development:

```bash
# Quick iteration cycle
cargo test              # Unit tests (seconds)
cargo clippy            # Linting

# Before pushing
cargo fmt --all         # Format code
cargo test              # All unit tests
./scripts/run-integration-tests.sh  # Full validation
```

### Integration Test Iteration

If you're iterating on overlay or shim logic:

```bash
# First run (build cluster, binaries, tests)
./scripts/run-integration-tests.sh --no-cleanup

# Make code changes...

# Rebuild and test (skip cargo, reuse cluster)
cargo build --release --bin containerd-shim-reaper-v2 --bin reaper-runtime
./scripts/run-integration-tests.sh --skip-cargo --no-cleanup

# Repeat until satisfied...

# Final cleanup run
./scripts/run-integration-tests.sh --skip-cargo
```

## Project Structure

```
reaper/
├── src/
│   ├── bin/
│   │   ├── containerd-shim-reaper-v2/  # Shim binary
│   │   │   └── main.rs                 # Shim implementation
│   │   └── reaper-runtime/             # Runtime binary
│   │       ├── main.rs                 # OCI runtime CLI
│   │       ├── state.rs                # State persistence
│   │       └── overlay.rs              # Overlay filesystem (Linux)
├── tests/                              # Integration tests
├── scripts/                            # Installation and testing scripts
├── deploy/
│   ├── ansible/                        # Ansible playbooks for deployment
│   └── kubernetes/                     # Kubernetes manifests
├── docs/                               # Documentation
└── .githooks/                          # Git hooks (pre-commit, pre-push)
```

## Common Tasks

### Add a New Binary

1. Create directory under `src/bin/<binary-name>/`
2. Add `main.rs` in that directory
3. Add entry to `Cargo.toml`:
   ```toml
   [[bin]]
   name = "binary-name"
   path = "src/bin/binary-name/main.rs"
   ```

### Add a New Test Suite

1. Create `tests/integration_<name>.rs`
2. Use `#[test]` or `#[tokio::test]` for async tests
3. Run with `cargo test --test integration_<name>`

### Update Dependencies

```bash
# Check for outdated dependencies
cargo outdated

# Update to latest compatible versions
cargo update

# Update Cargo.lock and check tests still pass
cargo test
```

### Debug a Test

Use VS Code's debug launch configurations or run with logging:

```bash
RUST_LOG=debug cargo test <test-name> -- --nocapture
```

## Troubleshooting

### Clippy Errors on macOS for Linux-only Code

Run clippy with Linux target:
```bash
cargo clippy --target x86_64-unknown-linux-gnu --all-targets
```

### Tests Fail with "Permission Denied"

Some tests require root for namespace operations. Run:
```bash
sudo cargo test
```

Or use integration tests which run in Kind (isolated environment):
```bash
./scripts/run-integration-tests.sh
```

### Docker Build Fails

Ensure Docker is running:
```bash
docker ps
```

If Docker daemon is not accessible, start Docker Desktop or the Docker daemon.

### Integration Tests Timeout

Increase timeout or check cluster resources:
```bash
kubectl get nodes
kubectl describe pod <pod-name>
```

## Additional Resources

- [Rust Book](https://doc.rust-lang.org/book/)
- [OCI Runtime Specification](https://github.com/opencontainers/runtime-spec)
- [Containerd Shim v2](https://github.com/containerd/containerd/tree/main/runtime/v2)
- [Kubernetes RuntimeClass](https://kubernetes.io/docs/concepts/containers/runtime-class/)
