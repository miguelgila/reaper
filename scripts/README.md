# Scripts

This directory contains helper scripts for testing, development, and Kubernetes integration.

> **Tip:** Most workflows are available via `make` targets in the project root. Run `make help` to see all options. These scripts are called by the Makefile — you rarely need to invoke them directly.

## Main Entry Point

**[run-integration-tests.sh](run-integration-tests.sh)** — Full integration test harness

The primary script for running integration tests. Orchestrates:
- Rust unit tests
- Kind cluster creation
- Binary builds and deployment
- Kubernetes integration tests
- Results reporting

**Usage:**
```bash
./scripts/run-integration-tests.sh                # Full run
./scripts/run-integration-tests.sh --skip-cargo   # K8s tests only (skip unit tests)
./scripts/run-integration-tests.sh --no-cleanup   # Keep cluster for debugging
./scripts/run-integration-tests.sh --verbose      # Print debug output to stdout
```

See [TESTING.md](../TESTING.md) for full documentation.

## Helper Scripts

**[configure-containerd.sh](configure-containerd.sh)** — Configure containerd for Reaper runtime

Used internally by `run-integration-tests.sh` to configure a containerd instance with the Reaper shim v2 runtime.

Can also be used manually:
```bash
./scripts/configure-containerd.sh <context> <node-id>
```
- `<context>`: `kind` or `minikube`
- `<node-id>`: Docker container ID

**[docker-coverage.sh](docker-coverage.sh)** — Code coverage in Docker

Generates code coverage using `cargo-tarpaulin` in a Linux container, matching CI configuration exactly (`tarpaulin.toml`). Uses a Docker volume to cache the cargo registry for faster repeat runs.

```bash
make coverage                 # Preferred
./scripts/docker-coverage.sh  # Direct invocation
```

**[install-hooks.sh](install-hooks.sh)** — Install git hooks

Sets up pre-commit hooks for the repository (formatting via `cargo fmt`).

```bash
./scripts/install-hooks.sh
```

## Configuration Files

**[minimal-containerd-config.toml](minimal-containerd-config.toml)** — Containerd configuration template

Defines the Reaper runtime class and shim plugin configuration. Used by `configure-containerd.sh` and `run-integration-tests.sh`.

## Documentation

For complete testing and development guidance, see [TESTING.md](../TESTING.md).
