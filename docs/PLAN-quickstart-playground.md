# Plan: Quick-Start Playground Kind Cluster

**Goal**: Create a one-command playground for users to get a working Reaper-enabled Kind cluster with 3 nodes, add a short README section, and refactor the integration test script to eliminate duplicated setup logic.

---

## Phase 1: Create `scripts/setup-playground.sh`

**New file**: `scripts/setup-playground.sh`

A standalone script that creates a 3-node Kind cluster (1 control-plane + 2 workers) with Reaper pre-installed and ready for manual workload testing.

**Based on**: `examples/01-scheduling/setup.sh` pattern (cleanest existing reference), with setup logic extracted from `scripts/lib/test-phases.sh` `phase_setup()`.

**Features**:
- `--cleanup` flag to delete the cluster
- `--cluster-name <name>` to override default (`reaper-playground`)
- `--kind-config <path>` to supply a custom Kind config (for integration test reuse)
- `--skip-build` to skip binary compilation (if binaries already exist)
- Supports `CI` env var for CI-mode binary directory handling (needed by integration tests)
- Supports `REAPER_BINARY_DIR` env var override (needed by integration tests)

**Steps the script performs**:
1. Preflight checks: docker, kind, kubectl, ansible-playbook
2. Generate a 3-node Kind config (inline) with containerd reaper-v2 runtime patch — OR use `--kind-config` if provided
3. Create Kind cluster (or reuse existing)
4. Detect node architecture from control-plane container
5. Cross-compile static musl binaries via Docker (unless `--skip-build`)
6. Copy binaries to `target/release/` (local mode) or set `REAPER_BINARY_DIR` (CI mode)
7. Install Reaper via `./scripts/install-reaper.sh --kind <name>`
8. Wait for nodes Ready + verify RuntimeClass
9. Run a smoke test pod (`/bin/echo "Hello from Reaper!"`) and show its logs
10. Print summary: nodes, RuntimeClass, example commands to try, cleanup command

**Generated Kind config** (when no `--kind-config` provided):
```yaml
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
  - role: worker
  - role: worker
containerdConfigPatches:
  - |
    [plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
      runtime_type = "io.containerd.reaper.v2"
      sandbox_mode = "podsandbox"
```

Note: The containerd config patch MUST be in the Kind config even though Ansible also patches it — the Kind config patch registers the runtime at cluster creation time so containerd knows about it before Ansible runs. Without it, the initial containerd config wouldn't reference `reaper-v2` at all.

**Smoke test pod** (applied and cleaned up automatically):
```yaml
apiVersion: v1
kind: Pod
metadata:
  name: reaper-smoke-test
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: test
      image: busybox
      command: ["/bin/echo", "Hello from Reaper playground!"]
```

---

## Phase 2: Add README.md Playground Section

**Where**: New subsection inside "Quick Start", inserted BEFORE "0. Build" (new first thing users see in Quick Start).

**Content** (short — 10-15 lines):
```markdown
### Playground (try it locally)

Spin up a 3-node Kind cluster with Reaper pre-installed:

\```bash
# Prerequisites: Docker, kind, kubectl, ansible (pip install ansible)
./scripts/setup-playground.sh
\```

This builds Reaper, creates a Kind cluster with 1 control-plane + 2 worker nodes,
installs the runtime on all nodes, and runs a smoke test. Once ready, try:

\```bash
kubectl run hello --rm -it --image=busybox --restart=Never \
  --overrides='{"spec":{"runtimeClassName":"reaper-v2"}}' \
  -- /bin/sh -c "echo Hello from \$(hostname) && uname -a"
\```

To tear down: `./scripts/setup-playground.sh --cleanup`
```

---

## Phase 3: Refactor Integration Test Script

**Goal**: Make `run-integration-tests.sh` use `setup-playground.sh` for its cluster setup instead of duplicating the logic in `phase_setup()`.

**Current duplication**: `phase_setup()` in `scripts/lib/test-phases.sh` (lines 27-121) does the exact same thing as the playground script: create Kind cluster, build binaries, install via Ansible.

**Changes to `scripts/lib/test-phases.sh`**:

Replace the body of `phase_setup()` with a call to the playground script:

```bash
phase_setup() {
  log_status ""
  log_status "${CLR_PHASE}Phase 2: Infrastructure setup${CLR_RESET}"
  log_status "========================================"
  ci_group_start "Phase 2: Infrastructure setup"

  local setup_args=(
    --cluster-name "$CLUSTER_NAME"
    --kind-config "scripts/kind-config.yaml"  # 1-node config for CI
  )

  if $VERBOSE; then
    setup_args+=(--verbose)
  fi

  ./scripts/setup-playground.sh "${setup_args[@]}" 2>&1 | tee -a "$LOG_FILE" || {
    log_error "Cluster setup failed"
    tail -100 "$LOG_FILE" >&2
    exit 1
  }

  # Capture NODE_ID for diagnostics (used by cleanup and test functions)
  NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')

  log_status "Infrastructure setup complete."
  ci_group_end
}
```

**What stays in `test-phases.sh`**: `phase_readiness()` (the test-specific parts: ServiceAccount wait, stale pod cleanup) and `phase_summary()` remain unchanged. The readiness wait for nodes and RuntimeClass moves into the playground script (it's needed for playground users too).

**Changes to `phase_readiness()`**: Remove the node-wait and RuntimeClass-verify steps (now handled by playground script). Keep only:
- Wait for default ServiceAccount
- Clean stale test pods
- The 30-second stability buffer (it exists for CI test reliability, not playground)

**Changes to `scripts/lib/test-common.sh`**: Remove the `kind` install logic from `phase_setup` (it was in test-phases.sh). The playground script handles its own preflight. If kind is not installed, the playground script fails with a clear message.

**No changes to `run-integration-tests.sh`** itself — it keeps calling `phase_setup`, `phase_readiness`, etc. The refactoring is internal to `phase_setup`.

---

## Phase 4 (Future/Optional): Refactor Example Setup Scripts

Not in scope for this PR, but worth noting: all four example `setup.sh` scripts duplicate the same build+install pattern. They could be refactored to call `setup-playground.sh --cluster-name <name> --kind-config <config>` and then just apply their example-specific resources. This would eliminate ~80 lines of duplicated logic per example.

---

## File Changes Summary

| File | Action | Description |
|------|--------|-------------|
| `scripts/setup-playground.sh` | **Create** | New standalone playground setup script |
| `README.md` | **Edit** | Add "Playground" subsection to Quick Start |
| `scripts/lib/test-phases.sh` | **Edit** | Refactor `phase_setup()` to call playground script; trim `phase_readiness()` |
| `docs/TODO.md` | **Edit** | Mark line 16 as done |

---

## Testing the Changes

1. Run `./scripts/setup-playground.sh` — verify 3-node cluster comes up, smoke test passes
2. Run `./scripts/setup-playground.sh --cleanup` — verify cluster deleted
3. Run `./scripts/run-integration-tests.sh` — verify all integration tests still pass (refactored path)
4. Run `./scripts/run-integration-tests.sh --skip-cargo --no-cleanup` — verify iterative workflow still works

---

## Decisions (Resolved)

1. **Verbose output**: Yes — playground script defaults to verbose. Add `--quiet` for integration test use.
2. **Kind pre-installed**: Yes — require kind as a prerequisite, document it. Drop auto-install from integration path. CI workflow installs kind separately.

---

## Progress Tracker

- [x] Phase 1: Create `scripts/setup-playground.sh` → commit
- [ ] Phase 2: Add README.md playground section → commit
- [ ] Phase 3: Refactor integration test script → commit
- [ ] Phase 4: Mark TODO.md line 16 as done → (in final commit)
- [ ] Verify: Run integration tests end-to-end
