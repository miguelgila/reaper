# Testing & Integration

This document consolidates all information about running tests, integration tests, and development workflows for the Reaper project.

## Quick Reference

All common tasks are available via `make`. Run `make help` for the full list.

| Task | Command |
|------|---------|
| **Full CI check** (recommended before push) | `make ci` |
| **Unit tests** | `make test` |
| **Clippy** (macOS) | `make clippy` |
| **Clippy** (Linux cross-check) | `make check-linux` |
| **Coverage** (Docker, CI-parity) | `make coverage` |
| **Integration tests** (full suite) | `make integration` |
| **Integration tests** (K8s only, skip cargo) | `make integration-quick` |

## Unit Tests

Run Rust tests natively on your machine:

```bash
cargo test
```

Tests run in a few seconds and provide immediate feedback. Use this for development iteration.

### Test Modules

- `integration_basic_binary` - Basic runtime functionality
- `integration_user_management` - User/group handling
- `integration_shim` - Shim-specific tests

Run a specific test:

```bash
cargo test --test integration_basic_binary
```

## Integration Tests (Kubernetes)

The main integration test suite runs against a kind (Kubernetes in Docker) cluster. It validates:

- ✓ DNS resolution in container
- ✓ Basic command execution (echo)
- ✓ Overlay filesystem sharing across pods
- ✓ Host filesystem protection (no leakage to host)
- ✓ Shim cleanup after pod deletion
- ✓ No defunct (zombie) processes
- ✓ `kubectl exec` support

### Full Suite (Recommended)

Runs cargo tests, builds binaries, creates a kind cluster, and runs all integration tests:

```bash
./scripts/run-integration-tests.sh
```

Options:
- `--skip-cargo` — Skip Rust unit tests (useful for rapid K8s-only reruns)
- `--no-cleanup` — Keep the kind cluster running after tests (for debugging)
- `--verbose` — Also print debug output to stdout (in addition to log file)
- `--help` — Show usage

### Examples

Rerun K8s tests against an existing cluster:

```bash
./scripts/run-integration-tests.sh --skip-cargo --no-cleanup
```

Then modify the overlay code and test again:

```bash
./scripts/run-integration-tests.sh --skip-cargo
```

Keep cluster for interactive debugging:

```bash
./scripts/run-integration-tests.sh --no-cleanup
```

Then interact with the cluster:

```bash
kubectl get pods
kubectl logs <pod-name>
kubectl describe pod <pod-name>
```

### Test Output & Logs

- **Console output**: Test results with pass/fail badges
- **Log file**: `/tmp/reaper-integration-logs/integration-test.log` (detailed diagnostics)
- **GitHub Actions**: Results posted to job summary when run in CI

### How It Works

The test harness orchestrates:

1. **Phase 1**: Rust cargo tests (`integration_*` tests)
2. **Phase 2**: Kubernetes infrastructure setup
   - Create or reuse kind cluster
   - Build static musl binaries (matches node architecture)
   - Deploy shim and runtime binaries to cluster node
   - Configure containerd with the Reaper runtime
3. **Phase 3**: Kubernetes readiness
   - Wait for API server and nodes
   - Create RuntimeClass
   - Wait for default ServiceAccount
4. **Phase 4**: Integration tests
   - DNS, echo, overlay, host protection, exec, zombie check
5. **Phase 5**: Summary & reporting

## Coverage

Generate code coverage report using Docker:

```bash
./scripts/docker-coverage.sh
```

This runs `cargo-tarpaulin` (Linux-first tool) in a container with appropriate capabilities.

## Containerd Configuration

Configure a containerd instance to use the Reaper shim runtime:

```bash
./scripts/configure-containerd.sh <context> <node-id>
```

- `<context>`: `kind` or `minikube` (determines config locations)
- `<node-id>`: Docker container ID (e.g., from `docker ps`)

This script is automatically run by `run-integration-tests.sh`.

## Development Workflow

### Before Pushing (CI-parity on macOS)

Run the full CI-equivalent check locally:

```bash
make ci
```

This runs, in order: `fmt` check, `clippy` (macOS), `clippy` (Linux cross-check), `cargo test`, and `coverage` (Docker + tarpaulin). If this passes, CI will pass.

### Quick Iteration

For fast feedback during development:

```bash
make test              # Unit tests only (seconds)
make clippy            # macOS clippy
make check-linux       # Catches #[cfg(linux)] compilation issues
```

### Linux Cross-Check

The overlay module (`overlay.rs`) is gated by `#[cfg(target_os = "linux")]` and doesn't compile on macOS. `make check-linux` cross-checks clippy against the `x86_64-unknown-linux-gnu` target to catch compilation errors in Linux-only code without leaving macOS.

Requires the target (one-time setup):
```bash
rustup target add x86_64-unknown-linux-gnu
```

### Coverage (CI-parity)

Coverage runs tarpaulin inside Docker to match CI exactly:

```bash
make coverage
```

Configuration lives in `tarpaulin.toml`. Functions requiring root + Linux namespaces (tested by kind-integration) are excluded via `#[cfg(not(tarpaulin_include))]` so coverage reflects what unit tests can actually reach.

### Integration Test Iteration

If you're iterating on overlay or shim logic:

```bash
# First run (build cluster, binaries, tests)
./scripts/run-integration-tests.sh --no-cleanup

# Make code changes...

# Rebuild and test (skip cargo, reuse cluster)
cargo build --release --bin containerd-shim-reaper-v2 --bin reaper-runtime --target x86_64-unknown-linux-musl
./scripts/run-integration-tests.sh --skip-cargo --no-cleanup

# Repeat until satisfied...

# Final cleanup run
./scripts/run-integration-tests.sh --skip-cargo
```

## Troubleshooting

### No kind cluster available

The test harness automatically creates one. If it fails, check:

- Docker is running: `docker ps`
- kind is installed: `kind --version`
- Sufficient disk space: `df -h`

### Pod stuck in Pending

Check containerd logs on the node:

```bash
docker exec <node-id> journalctl -u containerd -n 50 --no-pager
```

Check Kubelet logs:

```bash
docker exec <node-id> journalctl -u kubelet -n 50 --no-pager
```

### Test times out

Increase timeout in test function or check node resources:

```bash
docker exec <node-id> top -b -n 1
docker exec <node-id> df -h
```

### RuntimeClass not found

Wait a few seconds after applying the RuntimeClass, as it takes time to propagate.

## Directory Structure

```
reaper/
├── scripts/
│   ├── run-integration-tests.sh      [MAIN] Orchestrates all integration tests
│   ├── configure-containerd.sh       Helper to configure containerd
│   ├── install-hooks.sh              Setup git hooks (optional)
│   └── docker-coverage.sh            Run coverage in Docker
├── tests/
│   ├── integration_basic_binary.rs
│   ├── integration_user_management.rs
│   └── integration_shim.rs
├── kubernetes/                       [K8s cluster config examples]
├── examples/                         [Example pods and RuntimeClass]
└── TESTING.md                        [This file]
```

## CI Integration

The GitHub Actions workflow automatically runs:

```bash
./scripts/run-integration-tests.sh
```

Results are posted to the GitHub Actions job summary. If any test fails, the workflow reports the failure with diagnostics.

## Archived / Deprecated Scripts

The following scripts have been consolidated into `run-integration-tests.sh` and are no longer maintained:

- `kind-integration.sh` — Replaced by `run-integration-tests.sh` (more features, better test reporting)
- `minikube-setup-runtime.sh` — Minikube support deprecated
- `minikube-test.sh` — Minikube support deprecated
- `test-k8s-integration.sh` — Replaced by `run-integration-tests.sh`
- `docker-test.sh` — Optional helper; use `cargo test` for speed or `docker-coverage.sh` for coverage

## Next Steps

- Read the [Architecture](docs/SHIM_ARCHITECTURE.md) documentation for deeper understanding
- Check [Overlay Design](docs/OVERLAY_DESIGN.md) for filesystem isolation details
- See [SHIMV2 Design](docs/SHIMV2_DESIGN.md) for runtime internals
