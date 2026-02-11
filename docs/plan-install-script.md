# Installation Script Implementation Plan

**Status**: In Progress
**Started**: 2026-02-11
**Last Updated**: 2026-02-11

## Progress Tracker

- [ ] Phase 1: Core Installation Script
  - [ ] Create `scripts/install-reaper.sh` with modular structure
  - [ ] Extract reusable functions from `run-integration-tests.sh`
- [ ] Phase 2: Deployment Methods
  - [ ] Implement direct deployment (kind/SSH)
  - [ ] Implement DaemonSet deployment
- [ ] Phase 3: Verification & Safety
  - [ ] Add verification suite
  - [ ] Add safety features (backup, rollback, dry-run)
- [ ] Phase 4: Integration with Test Suite
  - [ ] Refactor `run-integration-tests.sh` to use `install-reaper.sh`
  - [ ] Update documentation
- [ ] Phase 5: Production Features (Optional)
  - [ ] Add production enhancements

---

## Overview
Create a production-ready installation script (`scripts/install-reaper.sh`) that can deploy Reaper to any Kubernetes cluster, with the integration test environment using it as a validation mechanism.

## Current State Analysis

**Existing Components:**
- `configure-containerd.sh` - Configures containerd on nodes (supports kind, minikube, local)
- `run-integration-tests.sh` - Full test harness that manually orchestrates installation
- `kind-config.yaml` - Kind-specific containerd configuration
- `kubernetes/runtimeclass.yaml` - RuntimeClass and example pod definitions
- Two binaries: `containerd-shim-reaper-v2` and `reaper-runtime`

**Current Installation Steps (from run-integration-tests.sh):**
1. Build static musl binaries for target architecture
2. Detect node architecture (aarch64 or x86_64)
3. Deploy binaries to nodes at `/usr/local/bin/`
4. Create overlay directories (`/run/reaper/overlay/{upper,work}`)
5. Configure containerd with `configure-containerd.sh`
6. Create RuntimeClass resource
7. Wait for cluster readiness

## Proposed Solution

**New Script: `scripts/install-reaper.sh`**

A unified installation script supporting multiple deployment modes.

### Modes of Operation

1. **Kind cluster** (default for CI/testing)
   ```bash
   ./scripts/install-reaper.sh --kind <cluster-name>
   ```

2. **Real Kubernetes cluster** (production)
   ```bash
   ./scripts/install-reaper.sh --cluster --nodes node1,node2,node3
   ```

3. **Auto-detect** (inspects current kubectl context)
   ```bash
   ./scripts/install-reaper.sh --auto
   ```

4. **DaemonSet deployment** (Kubernetes-native, for production clusters)
   ```bash
   ./scripts/install-reaper.sh --daemonset
   ```

### Features

**Build & Distribution:**
- Auto-detect node architecture from cluster
- Build static musl binaries for detected architectures (x86_64, aarch64)
- Support cross-compilation using Docker
- Verify binary compatibility before deployment
- Option to use pre-built binaries (for CI caching)

**Deployment Methods:**
- **Direct deploy**: Copy binaries via SSH/docker exec to each node
- **DaemonSet**: Kubernetes-native installation using privileged DaemonSet (for production)
- **ConfigMap/Initcontainer**: Bundle binaries in ConfigMap, extract via init container

**Configuration:**
- Automatically configure containerd on all nodes
- Handle different containerd versions (1.x, 2.x)
- Create/update RuntimeClass
- Verify configuration post-deployment

**Validation:**
- Pre-flight checks (kubectl access, node connectivity, permissions)
- Post-install verification (shim binary accessible, containerd accepts runtime, test pod runs)
- Health check mode (`--verify-only`)

**Safety:**
- Dry-run mode (`--dry-run`) to preview actions
- Rollback capability (restore previous containerd config)
- Non-destructive by default (backup existing configs)

## Implementation Tasks

### Phase 1: Core Installation Script ⏳
1. **Create `scripts/install-reaper.sh`** with modular structure:
   - Argument parsing and mode detection
   - Pre-flight validation functions
   - Binary building and detection
   - Node discovery and architecture detection
   - Deployment orchestration
   - Post-install verification

2. **Extract reusable functions from `run-integration-tests.sh`**:
   - Binary building logic (lines 399-420)
   - Node architecture detection (lines 394-396)
   - Overlay directory creation (line 437)
   - RuntimeClass creation (lines 466-472)

### Phase 2: Deployment Methods
3. **Implement direct deployment** (kind/SSH):
   - Kind node deployment via `docker cp` and `docker exec`
   - SSH-based deployment for real clusters
   - Node connectivity pre-checks
   - Parallel deployment to multiple nodes

4. **Implement DaemonSet deployment**:
   - Create DaemonSet YAML template (`kubernetes/installer-daemonset.yaml`)
   - Bundle binaries as ConfigMap or use init container with HTTP download
   - Privileged DaemonSet to copy binaries to host `/usr/local/bin/`
   - Configure containerd from DaemonSet
   - Self-cleanup after installation

### Phase 3: Verification & Safety
5. **Add verification suite**:
   - Verify binaries are in PATH on all nodes
   - Verify containerd config includes reaper-v2
   - Run test pod to validate end-to-end
   - `--verify-only` mode to check existing installation

6. **Add safety features**:
   - Backup existing containerd config to `/etc/containerd/config.toml.backup`
   - `--rollback` command to restore previous state
   - `--dry-run` mode that shows what would be done
   - Idempotency (safe to run multiple times)

### Phase 4: Integration with Test Suite
7. **Refactor `run-integration-tests.sh`** to use `install-reaper.sh`:
   - Replace manual binary deployment (lines 399-444) with `./scripts/install-reaper.sh --kind "$CLUSTER_NAME"`
   - Remove duplicate logic
   - Keep test-specific orchestration (test execution, cleanup, reporting)
   - Maintain backward compatibility with existing flags

8. **Update documentation**:
   - Add installation guide to README.md
   - Update kubernetes/README.md with new installation methods
   - Update scripts/README.md with script descriptions
   - Update docs/TODO.md to mark task as complete

### Phase 5: Production Features (Optional)
9. **Add production enhancements**:
   - Support for Helm chart (as alternative to raw script)
   - Binary versioning and release management
   - GitHub Releases integration (download pre-built binaries)
   - Multi-cluster support (install to multiple clusters at once)
   - Uninstall command

## Script Structure

```bash
#!/usr/bin/env bash
# install-reaper.sh - Deploy Reaper runtime to any Kubernetes cluster

set -euo pipefail

# --- Configuration ---
REAPER_VERSION="${REAPER_VERSION:-latest}"
SHIM_BINARY="containerd-shim-reaper-v2"
RUNTIME_BINARY="reaper-runtime"
INSTALL_PATH="/usr/local/bin"
OVERLAY_BASE="/run/reaper/overlay"

# --- Modes ---
MODE="auto"  # auto, kind, cluster, daemonset
DRY_RUN=false
VERIFY_ONLY=false
ROLLBACK=false
VERBOSE=false

# --- Functions ---
parse_args() { ... }
preflight_checks() { ... }
detect_cluster_type() { ... }
discover_nodes() { ... }
detect_architectures() { ... }
build_binaries() { ... }
deploy_to_kind() { ... }
deploy_to_cluster() { ... }
deploy_via_daemonset() { ... }
configure_containerd_all_nodes() { ... }
create_runtimeclass() { ... }
verify_installation() { ... }
run_test_pod() { ... }
rollback_installation() { ... }

# --- Main ---
main() {
  parse_args "$@"
  preflight_checks

  if $ROLLBACK; then
    rollback_installation
    exit 0
  fi

  if $VERIFY_ONLY; then
    verify_installation
    exit 0
  fi

  case "$MODE" in
    auto)     detect_cluster_type; deploy ;;
    kind)     deploy_to_kind ;;
    cluster)  deploy_to_cluster ;;
    daemonset) deploy_via_daemonset ;;
  esac

  verify_installation
}

main "$@"
```

## Testing Strategy

**Unit-level validation:**
- Test script with `--dry-run` on various cluster types
- Verify idempotency (run twice, should succeed both times)
- Test rollback functionality
- Test verification mode

**Integration validation:**
- Refactor `run-integration-tests.sh` to use new script
- Ensure all existing integration tests pass
- Add new test: install → uninstall → reinstall cycle
- Test on both x86_64 and aarch64 Kind nodes

**Production validation:**
- Document manual testing procedure for real clusters
- Add to CI pipeline as separate workflow (optional)

## Files to Create/Modify

**New files:**
- `scripts/install-reaper.sh` - Main installation script
- `kubernetes/installer-daemonset.yaml` - DaemonSet template (Phase 2)
- `scripts/uninstall-reaper.sh` - Uninstallation script (Phase 5)

**Modified files:**
- `scripts/run-integration-tests.sh` - Refactor to use install-reaper.sh
- `docs/TODO.md` - Mark task #6 complete
- `README.md` - Add installation section
- `kubernetes/README.md` - Update with new installation methods
- `scripts/README.md` - Document new scripts

## Success Criteria

1. ✅ Single command installs Reaper to any K8s cluster
2. ✅ Works on Kind clusters (replaces manual steps in integration tests)
3. ✅ Works on real clusters (SSH-based deployment)
4. ✅ Supports DaemonSet deployment for production
5. ✅ Includes verification and rollback
6. ✅ Integration test suite uses the script (dogfooding)
7. ✅ Documentation updated with installation procedures
8. ✅ Safe to run multiple times (idempotent)

## Dependencies & Assumptions

**Required tools:**
- `kubectl` (cluster access)
- `docker` (for Kind and cross-compilation)
- `cargo` (for building binaries)
- `ssh` (for cluster mode, optional)
- `kind` (for Kind mode only)

**Assumptions:**
- User has cluster admin permissions
- Nodes are accessible (SSH for cluster mode, docker exec for Kind)
- Containerd is the container runtime (not Docker/cri-o)
- Static musl binaries work on all Linux nodes

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Breaking existing test suite | High | Incremental refactor, maintain backward compat |
| Different containerd versions | Medium | Test against 1.x and 2.x, version detection |
| Cross-architecture complexity | Medium | Use Docker for cross-compilation, verify binaries |
| Production cluster failures | High | Dry-run mode, rollback, extensive validation |
| SSH access issues | Low | Provide alternative DaemonSet method |

## Implementation Notes

*This section will be updated as implementation progresses with notes on decisions, challenges, and solutions.*

---

**Related Files:**
- Implementation: `scripts/install-reaper.sh`
- Test harness: `scripts/run-integration-tests.sh`
- Containerd config: `scripts/configure-containerd.sh`
- TODO tracking: `docs/TODO.md`
