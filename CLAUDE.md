# Reaper Project - Claude Code Instructions

This file contains important project-specific context and instructions for Claude Code.

## CI/CD and Integration Testing

### Permission Issues in GitHub Actions

**Problem**: In GitHub Actions CI, the `target/` directory is often cached and owned by a different user than the current workflow step. This causes "Permission denied" errors when trying to copy binaries to `target/release/`.

**Solution**: The integration test scripts detect CI mode via the `CI` environment variable and use binaries directly from `target/<target-triple>/release/` without copying them. This is controlled by the `REAPER_BINARY_DIR` environment variable.

- **CI mode** (`CI=true`): Uses binaries from `target/<target-triple>/release/` directly
- **Local mode**: Copies binaries to `target/release/` for convenience

Key environment variables:
- `CI`: Set by GitHub Actions automatically. Enables CI-specific behavior.
- `REAPER_BINARY_DIR`: Override the binary directory location for Ansible installer.

Files involved:
- [scripts/run-integration-tests.sh](scripts/run-integration-tests.sh): Detects CI mode and sets `REAPER_BINARY_DIR`
- [scripts/install-reaper.sh](scripts/install-reaper.sh): Accepts `REAPER_BINARY_DIR` and passes it to Ansible
- [ansible/install-reaper.yml](ansible/install-reaper.yml): Uses `local_binary_dir` variable (set from `REAPER_BINARY_DIR`)

### Building Binaries for Integration Tests

The integration tests build static musl binaries using Docker to ensure compatibility with Kind nodes:

```bash
# Detects node architecture (x86_64 or aarch64)
docker run --rm \
  -v "$(pwd)":/work \
  -w /work \
  messense/rust-musl-cross:<arch>-musl \
  cargo build --release --target <target-triple>
```

This produces binaries at `target/<target-triple>/release/` that work in Kind's container environment.

## Architecture Notes

See [MEMORY.md](.claude/projects/-Users-miguelgi-Documents-CODE-Explorations-reaper/memory/MEMORY.md) for key architecture decisions and common pitfalls.

## Integration Test Structure

The integration test suite ([scripts/run-integration-tests.sh](scripts/run-integration-tests.sh)) has four phases:

1. **Phase 1**: Rust cargo tests (unit and integration tests)
2. **Phase 2**: Infrastructure setup (Kind cluster, build binaries, install Reaper via Ansible)
3. **Phase 3**: Kubernetes readiness checks (API server, RuntimeClass, ServiceAccount)
4. **Phase 4**: Integration tests (DNS, overlay, process cleanup, exec support, etc.)

All tests must pass for the suite to succeed.
