# Reaper

[![Tests](https://github.com/miguelgi/reaper/actions/workflows/test.yml/badge.svg?branch=main)](https://github.com/miguelgi/reaper/actions/workflows/test.yml)
[![Build](https://github.com/miguelgi/reaper/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/miguelgi/reaper/actions/workflows/build.yml)
[![Coverage](https://codecov.io/gh/miguelgi/reaper/branch/main/graph/badge.svg)](https://codecov.io/gh/miguelgi/reaper)
[![Security](https://github.com/miguelgi/reaper/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/miguelgi/reaper/actions/workflows/build.yml)

A Rust project.

## Quick start

Build and run locally:

```bash
cargo build --release
cargo run
```

Run tests:

```bash
# Unit tests (fast, recommended locally)
cargo test

# Full integration tests (Kubernetes + unit tests)
./scripts/run-integration-tests.sh

# For complete testing guidance, see TESTING.md
```

## Development setup

Prerequisites:

- Rust toolchain (we pin `stable` via `rust-toolchain.toml`)
- Docker (optional, for Linux-like CI runs on macOS)

Clone and build:

```bash
git clone https://github.com/miguelgi/reaper
cd reaper
cargo build
```

### Toolchain

The repository includes `rust-toolchain.toml` to pin the toolchain and enable `rustfmt` and `clippy` components.

### Git hooks

We provide a repository git hooks directory and an install script to enable them locally. The pre-commit hook runs `cargo fmt --all` and stages formatting changes.

Enable hooks locally:

```bash
chmod +x .githooks/pre-commit
./scripts/install-hooks.sh
```

If you prefer the hook to fail instead of auto-staging, edit `.githooks/pre-commit` to use `cargo fmt --all -- --check` and exit non-zero on mismatch.

### Formatting & linting

Use the toolchain components:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features
```

CI runs formatting and clippy checks; push will fail if they don't pass.

## Docker (Optional: Linux development / CI parity)

A `Dockerfile` and coverage script are available for Linux-specific testing. You do not need Docker to develop on macOS; prefer `cargo test` locally for speed.

Use Docker when you need:
- Code coverage via `cargo-tarpaulin` (Linux-first tool)
- CI failure reproduction specific to Linux

Run coverage in Docker:

```bash
./scripts/docker-coverage.sh
```

For more testing details, see [TESTING.md](TESTING.md).

## Testing Guide

Complete information about unit tests, integration tests, and troubleshooting is documented in **[TESTING.md](TESTING.md)**.

Quick reference:
- **Unit tests**: `cargo test`
- **Integration tests** (Kubernetes): `./scripts/run-integration-tests.sh`
- **Integration tests** (K8s only): `./scripts/run-integration-tests.sh --skip-cargo`
- **Coverage**: `./scripts/docker-coverage.sh`

For usage, options, and troubleshooting, see [TESTING.md](TESTING.md).

## VS Code

Recommended extensions (workspace recommends them automatically):

- `rust-analyzer` — main Rust language support
- `CodeLLDB` (vadimcn.vscode-lldb) — debug adapter for Rust
- `Test Explorer UI` — unified test UI

Workspace settings enable CodeLens and configure rust-analyzer to run clippy on save; a `launch.json` is provided for debugging tests and binaries.

## CI

GitHub Actions are configured to run the following workflows:

- `Tests` — runs `cargo test`
- `Build` — builds across OS/rust matrix, checks formatting, runs clippy, and builds release
- `Coverage` — runs `cargo-tarpaulin` and uploads to Codecov

- `Security` — CI also runs `cargo-audit` to scan the dependency tree for known advisories and yanked crates.

## Runtime engine (containerd/Kubernetes)

This repository includes an initial runtime binary `reaper-runtime` that exposes an OCI-like CLI for running native binaries directly on the host.

### Quick start: Running binaries with reaper-runtime

**Create a bundle with config.json:**

```bash
mkdir -p /tmp/my-bundle
cat > /tmp/my-bundle/config.json <<'EOF'
{
  "process": {
    "args": ["/bin/sh", "-c", "echo Hello from reaper && sleep 2"],
    "cwd": "/tmp",
    "env": ["PATH=/usr/bin:/bin:/usr/local/bin"]
  }
}
EOF
```

**Create, start, and manage the container:**

```bash
# Create metadata
reaper-runtime create my-app --bundle /tmp/my-bundle

# Start the process (runs immediately)
reaper-runtime start my-app --bundle /tmp/my-bundle

# Check status
reaper-runtime state my-app

# Kill if needed
reaper-runtime kill my-app --signal 15

# Cleanup
reaper-runtime delete my-app
```

### Testing on Kubernetes

To test the Reaper runtime with Kubernetes (kind cluster):

```bash
./scripts/run-integration-tests.sh
```

This runs a complete integration test suite including DNS resolution, overlay filesystem sharing, host protection, and more. See [TESTING.md](TESTING.md) for full details.

### Testing core binary execution

Integration tests verify that the core functionality works: running host binaries with OCI-like syntax. Tests are located in `tests/integration_basic_binary.rs`:

```bash
cargo test --test integration_basic_binary
cargo test  # Run all tests
```

Tests cover:
1. **`test_run_echo_hello_world`** — Full lifecycle (create → start → state → delete) for `echo "hello world"`
2. **`test_run_shell_script`** — Multi-line shell command execution with output capture
3. **`test_invalid_bundle`** — Error handling for missing `config.json`

Additional test suites:
- **`integration_io`** — FIFO stdout/stderr redirection, fallback behavior, multiline output
- **`integration_user_management`** — uid/gid handling, additional groups, umask
- **`integration_shim`** — Shim binary existence, bundle creation, config parsing

All tests use isolated temporary directories to avoid state pollution.

### Process output (stdout/stderr)

Container stdout and stderr are captured via FIFOs provided by containerd:
- Output is automatically captured when running in Kubernetes and available via `kubectl logs <pod>`
- The runtime connects container processes to FIFOs (named pipes) provided by containerd in the CreateTask request
- No manual redirection is needed in production environments

For local testing or debugging:
- Run reaper-runtime directly without containerd (inherits parent's stdio)
- Or redirect at the shell level: `reaper-runtime start my-app --bundle /tmp/my-bundle > /tmp/my-app.out 2> /tmp/my-app.err`
- To debug the runtime itself (not container output), use environment variables:
  - `REAPER_RUNTIME_LOG=/var/log/reaper-runtime.log` — Runtime internals
  - `REAPER_SHIM_LOG=/var/log/reaper-shim.log` — Shim internals

### CLI Commands

Commands implemented:
- `create <id> [--bundle PATH]` — records container metadata from the bundle's `config.json`.
- `start <id> [--bundle PATH]` — spawns the process defined in `config.json.process.args` and persists the PID.
- `state <id>` — prints JSON with `id`, `status`, `pid`, `bundle`.
- `kill <id> [--signal N]` — sends a Unix signal to the process.
- `delete <id> [--force]` — removes runtime state.

Accepted global flags for compatibility (ignored): `--root`, `--log`, `--log-format`.

State directory: defaults to `/run/reaper` and can be overridden via `REAPER_RUNTIME_ROOT`.

### Kubernetes Integration (Experimental)

✅ **Current Status**: Full containerd shim v2 protocol implemented! Reaper now supports Kubernetes integration via direct command execution on host nodes. See `docs/SHIMV2_DESIGN.md` for implementation details and `kubernetes/` for configuration examples.

To test with Kubernetes:

```bash
# Recommended: Kind cluster integration (builds static musl binaries, deploys, and tests)
./scripts/run-integration-tests.sh

# Or build and install shim manually
cargo build --release --bin containerd-shim-reaper-v2 --bin reaper-runtime
sudo cp target/release/containerd-shim-reaper-v2 /usr/local/bin/
sudo cp target/release/reaper-runtime /usr/local/bin/

# Manual setup (see kubernetes/README.md)
kubectl apply -f kubernetes/runtimeclass.yaml
kubectl logs -f reaper-example
```

#### Configure containerd to use reaper-v2

Add a runtime entry in `/etc/containerd/config.toml`:

```toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
  runtime_type = "io.containerd.reaper.v2"
  sandbox_mode = "podsandbox"
```

Restart containerd:
```bash
sudo systemctl restart containerd
```

#### Kubernetes RuntimeClass example

```yaml
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: reaper-v2
handler: reaper-v2
```

### Implemented features

- ✅ **Overlay filesystem**: Shared mount namespace + overlayfs protects the host filesystem while allowing cross-deployment file sharing. Enabled by default. See `docs/OVERLAY_DESIGN.md`.
- ✅ **User/Group ID Management**: Parses `process.user.uid`, `process.user.gid`, `process.user.additionalGids`, `process.user.umask` from config.json (currently disabled for debugging — code exists in `do_start()`)
- ✅ **Containerd shim v2 protocol**: Full Task trait with create/start/delete/wait/kill/state/pids/exec/stats/resize_pty methods
- ✅ **Sandbox lifecycle**: Pause containers use blocking `wait()` with `kill()` signaling via `tokio::sync::Notify`
- ✅ **Direct command execution**: Commands run on host nodes (no container isolation by design)
- ✅ **RuntimeClass support**: Configure via `kubernetes/runtimeclass.yaml`
- ✅ **End-to-end testing**: Validated with kind cluster (`scripts/run-integration-tests.sh`)
- ✅ **Container I/O**: stdout/stderr captured via FIFOs for `kubectl logs` integration
- See `kubernetes/README.md` for complete setup and testing instructions

### Overlay filesystem

All Reaper workloads on a node share a single overlay filesystem. The host root is the read-only lower layer; writes go to a shared upper layer. This means:

- Workload A writes `/etc/config` → Workload B can read it
- The host's real `/etc/config` is never modified
- `/proc`, `/sys`, `/dev` remain real host mounts
- Overlay is ephemeral (clears on reboot)

Configuration:
```bash
# Custom overlay location (default: /run/reaper/overlay)
export REAPER_OVERLAY_BASE=/custom/path
```

Overlay is mandatory — workloads cannot run without isolation.

### Next steps

- Exec into running containers (requires daemon protocol)
- Resource monitoring (stats without cgroups)
- Performance optimization (reduce 500ms startup delay)
- Re-enable user/group switching after further validation


## Coverage

Local coverage (Linux) with tarpaulin:

```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Xml --timeout 600
```

On macOS run the included Docker coverage script instead.

## Contributing

- Run `cargo fmt` and `cargo clippy` before opening PRs.
- Install git hooks to auto-format on commit.

## License

MIT
