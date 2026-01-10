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

Run tests (recommended locally):

```bash
# Fast: run tests natively on your machine
cargo test

# If you need Linux parity or want to reproduce CI, use the Docker helper (optional)
chmod +x scripts/docker-test.sh
./scripts/docker-test.sh
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

## Docker (Linux development / CI parity)

We include a `Dockerfile` and helper scripts to run coverage and reproduce Linux CI locally. You do not need Docker to develop on macOS; prefer `cargo test` locally for speed. Use Docker when you need:

- `cargo-tarpaulin` (coverage) which is Linux-first, or
- to reproduce CI failures that only happen on Linux.

Run coverage in Docker:

```bash
chmod +x scripts/docker-coverage.sh
./scripts/docker-coverage.sh
```

If you still want to run the test container, the helper exists but is optional:

```bash
chmod +x scripts/docker-test.sh
./scripts/docker-test.sh
```

Note: `docker-coverage.sh` adds `--cap-add=SYS_PTRACE` and `--security-opt seccomp=unconfined` required by `cargo-tarpaulin`.

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

⚠️ **Current Status**: The runtime is registered in containerd but requires **shim v2 protocol implementation** for Kubernetes pods to work. The CLI runs successfully locally; full Kubernetes support is pending.

To test locally with Minikube:

```bash
# Setup containerd with reaper handler
chmod +x scripts/minikube-setup-runtime.sh
./scripts/minikube-setup-runtime.sh

# Deploy a test pod (currently fails; shim v2 protocol needed)
bash scripts/minikube-test.sh
```

#### Configure containerd to use reaper-runtime

Add a runtime entry in `/etc/containerd/config.toml`:

```toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper]
	runtime_type = "io.containerd.runc.v2"
	[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper.options]
		BinaryName = "reaper-runtime"
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
	name: reaper
handler: reaper
```

### Next steps to reach full Kubernetes compatibility
- **Implement containerd shim v2 protocol**: Handle socket-based task lifecycle (create, start, delete) and write required state files (init.pid, exit status).
- Implement full OCI `state` output matching `runc state`.
- Handle container lifecycle robustly (exit status, `stopped` state).
- Accept and ignore additional runc options (e.g., `--systemd-cgroup`).
- Add integration tests invoking the runtime via containerd's shim layer.


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
