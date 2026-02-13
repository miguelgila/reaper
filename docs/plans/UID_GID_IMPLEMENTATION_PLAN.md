# UID/GID Implementation & Testing Plan

**Status:** Draft - Pending Review
**Created:** 2026-02-12
**Goal:** Enable and validate UID/GID switching functionality in Reaper runtime

## Current State Analysis

### What Exists
1. **OCI config parsing** - `OciUser` struct fully implemented in [src/bin/reaper-runtime/main.rs](../src/bin/reaper-runtime/main.rs):
   - `uid`, `gid`, `additional_gids`, `umask` fields
   - Proper deserialization with OCI spec compliance (handles `additionalGids` alias)

2. **Integration tests** - [tests/integration_user_management.rs](../../tests/integration_user_management.rs):
   - Basic config parsing validation
   - Tests for current user, no user field, umask, additional groups, root user
   - **All tests currently run with `REAPER_NO_OVERLAY=1`** (unit test mode)
   - **Tests verify config parsing only, not actual UID/GID switching**

3. **Documentation references**:
   - [.github/claude-instructions.md](.github/claude-instructions.md) mentions privilege dropping sequence: `setgroups → setgid → setuid`
   - Notes implementation should use `Command::pre_exec` hook
   - Confirms user switching is "currently disabled for debugging"

### What's Missing
1. **No actual `setuid()`/`setgid()`/`setgroups()` system calls** in the runtime
2. **No validation that processes run with requested UID/GID**
3. **No Kubernetes integration tests** with `securityContext.runAsUser` / `runAsGroup`
4. **No tests on actual Linux cluster** (all integration tests run in Kind with overlay disabled)

### Known Issues
- User switching is intentionally disabled (per comments in [tests/integration_user_management.rs:301-303](../../tests/integration_user_management.rs#L301-L303))
- Non-root users cannot switch to arbitrary UIDs/GIDs (permission error expected, see [line 282-287](../../tests/integration_user_management.rs#L282-L287))

---

## Implementation Plan

### Phase 1: Enable UID/GID Switching in Runtime ⏱️ 2-3 hours

#### 1.1 Add System Calls to `do_start()` - Non-PTY Mode
**File:** [src/bin/reaper-runtime/main.rs](../src/bin/reaper-runtime/main.rs)
**Location:** After [line 685](../src/bin/reaper-runtime/main.rs#L685) (before `cmd.spawn()` in non-terminal mode)

**Implementation:**
```rust
// Apply user/group configuration if present
if let Some(ref user) = user_config {
    let uid_val = user.uid;
    let gid_val = user.gid;
    let groups = user.additional_gids.clone();
    let umask_val = user.umask;

    unsafe {
        cmd.pre_exec(move || {
            // Set supplementary groups first
            if !groups.is_empty() {
                let gids: Vec<nix::libc::gid_t> = groups.iter().map(|&g| g).collect();
                if nix::libc::setgroups(gids.len(), gids.as_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }

            // Set GID before UID (drop privileges correctly)
            if nix::libc::setgid(gid_val) != 0 {
                return Err(std::io::Error::last_os_error());
            }

            // Set UID (this must be last)
            if nix::libc::setuid(uid_val) != 0 {
                return Err(std::io::Error::last_os_error());
            }

            // Apply umask if specified
            if let Some(mask) = umask_val {
                nix::libc::umask(mask as nix::libc::mode_t);
            }

            Ok(())
        });
    }
}
```

**Critical Notes:**
- Order matters: `setgroups()` → `setgid()` → `setuid()` (per privilege dropping best practices)
- Must use `unsafe` block (modifying process credentials)
- `pre_exec` runs in child after fork, before exec
- Errors cause spawn to fail (correct behavior)

#### 1.2 Add System Calls to `exec_with_pty()` - PTY Mode (Initial Container)
**File:** [src/bin/reaper-runtime/main.rs](../src/bin/reaper-runtime/main.rs)
**Location:** Inside `cmd.pre_exec()` at [line 437-460](../src/bin/reaper-runtime/main.rs#L437-L460), after `setsid()` and before `ioctl(TIOCSCTTY)`

**Challenge:** PTY mode currently doesn't capture user config. Need to clone it before fork.

**Implementation:**
1. Clone `user_config` before line 396 (before terminal mode branch):
   ```rust
   let user_cfg_for_pty = proc.user.clone();
   ```

2. Make `OciUser` derive `Clone`:
   ```rust
   #[derive(Debug, Deserialize, Clone)]
   struct OciUser { /* ... */ }
   ```

3. Inside `pre_exec` (after `setsid()`, before `TIOCSCTTY`):
   ```rust
   // Apply user/group configuration
   if let Some(ref user) = user_cfg_for_pty {
       // Same setgroups/setgid/setuid/umask sequence as above
   }
   ```

#### 1.3 Add System Calls to `exec_with_pty()` - Exec Support
**File:** [src/bin/reaper-runtime/main.rs](../src/bin/reaper-runtime/main.rs)
**Location:** `exec_with_pty()` function at [line 834](../src/bin/reaper-runtime/main.rs#L834)

**Challenge:** Exec process has its own user config from `ExecState`, not from container's OCI config.

**Implementation:**
1. Extend `ExecState` struct in [src/bin/reaper-runtime/state.rs](../src/bin/reaper-runtime/state.rs) to include `user` field:
   ```rust
   pub struct ExecState {
       // ... existing fields ...
       pub user: Option<OciUser>,
   }
   ```

2. Update shim to pass user config when creating exec state (separate task, may require shim changes)

3. Apply same privilege dropping sequence in `exec_with_pty` and `exec_without_pty`

#### 1.4 Add System Calls to `exec_without_pty()` - Exec Support
**File:** [src/bin/reaper-runtime/main.rs](../src/bin/reaper-runtime/main.rs)
**Location:** Before [line 1011](../src/bin/reaper-runtime/main.rs#L1011) (before `cmd.spawn()`)

**Implementation:** Same as 1.3, using `unsafe cmd.pre_exec()`

---

### Phase 2: Unit Tests (Cargo Test) ⏱️ 3-4 hours

**Goal:** Test UID/GID switching in isolation without Kubernetes

#### 2.1 Update Existing Tests
**File:** [tests/integration_user_management.rs](../../tests/integration_user_management.rs)

**Changes:**
1. **Remove `REAPER_NO_OVERLAY=1` where appropriate** - some tests should run with overlay
2. **Add validation** - instead of just checking that containers start, verify actual UID/GID

**Example - Enhanced `test_run_with_current_user()`:**
```rust
#[test]
fn test_run_with_current_user() {
    // ... existing setup ...

    // Capture stdout to verify uid/gid
    let stdout_fifo = bundle_path.join("stdout.fifo");
    let _ = std::process::Command::new("mkfifo")
        .arg(&stdout_fifo)
        .output();

    // Update config to write uid/gid to stdout
    let config = serde_json::json!({
        "process": {
            "args": ["/bin/sh", "-c", "id -u && id -g"],
            "cwd": "/tmp",
            "env": ["PATH=/usr/bin:/bin"],
            "user": {
                "uid": uid,
                "gid": gid
            }
        }
    });

    // ... create/start container with stdout FIFO ...

    // Read stdout and validate
    let output = read_from_fifo(&stdout_fifo, Duration::from_secs(2));
    let lines: Vec<&str> = output.trim().split('\n').collect();
    assert_eq!(lines.len(), 2, "Expected uid and gid output");
    assert_eq!(lines[0].parse::<u32>().unwrap(), uid);
    assert_eq!(lines[1].parse::<u32>().unwrap(), gid);
}
```

#### 2.2 Add Root User Privilege Drop Test
**Purpose:** Verify that running as root and requesting non-root user works

**Prerequisite:** Test must run as root (skip if not)

```rust
#[test]
fn test_privilege_drop_from_root() {
    // Skip if not running as root
    if unsafe { nix::libc::getuid() } != 0 {
        eprintln!("Skipping test_privilege_drop_from_root: not running as root");
        return;
    }

    // Create container with uid=1000, gid=1000
    // Verify process runs with those credentials
    // Verify process CANNOT write to root-only paths
}
```

#### 2.3 Add Non-Root Permission Denial Test
**Purpose:** Verify that non-root users get proper errors when switching to other users

```rust
#[test]
fn test_non_root_cannot_switch_user() {
    if unsafe { nix::libc::getuid() } == 0 {
        eprintln!("Skipping: running as root");
        return;
    }

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/true"],
            "user": { "uid": 0, "gid": 0 }
        }
    });

    // Start should fail with permission error
    let start_output = Command::new(reaper_bin)
        .arg("start")
        .arg("test-fail")
        .output()
        .expect("Failed to run start");

    assert!(!start_output.status.success());
    let stderr = String::from_utf8_lossy(&start_output.stderr);
    assert!(
        stderr.contains("Operation not permitted") || stderr.contains("Permission denied"),
        "Expected permission error, got: {}", stderr
    );
}
```

#### 2.4 Add Supplementary Groups Test
**Purpose:** Verify `additionalGids` works

```rust
#[test]
fn test_additional_groups_applied() {
    // Skip if not root (can't set arbitrary groups)
    if unsafe { nix::libc::getuid() } != 0 {
        return;
    }

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/sh", "-c", "groups"],
            "user": {
                "uid": 1000,
                "gid": 1000,
                "additionalGids": [10, 20, 30]
            }
        }
    });

    // Start container, capture stdout
    // Parse groups output and verify 10, 20, 30 are present
}
```

#### 2.5 Add Umask Validation Test
**Purpose:** Verify umask actually affects file creation permissions

```rust
#[test]
fn test_umask_affects_file_creation() {
    let test_file = bundle_path.join("umask_test");

    let config = serde_json::json!({
        "process": {
            "args": ["/bin/sh", "-c", format!("umask && touch {}", test_file.display())],
            "user": {
                "uid": current_uid,
                "gid": current_gid,
                "umask": 0o077  // Very restrictive: only owner can access
            }
        }
    });

    // Start container, let it create file
    // Check file permissions match umask
    let metadata = fs::metadata(&test_file).unwrap();
    let perms = metadata.permissions().mode();
    assert_eq!(perms & 0o777, 0o700, "File should be created with 0700 permissions (umask 077)");
}
```

---

### Phase 3: Kubernetes Integration Tests ⏱️ 4-5 hours

**Goal:** Validate UID/GID switching in real Kubernetes environment

#### 3.1 Add Security Context Test to `run-integration-tests.sh`
**File:** [scripts/run-integration-tests.sh](../../scripts/run-integration-tests.sh)

**New Test Case:** "Test UID/GID switching with securityContext"

```bash
# Test UID/GID switching
echo "==== Test: UID/GID switching with securityContext ===="
cat <<EOF | kubectl apply -f -
apiVersion: v1
kind: Pod
metadata:
  name: test-uid-gid
spec:
  runtimeClassName: reaper
  restartPolicy: Never
  securityContext:
    runAsUser: 1000
    runAsGroup: 1000
    fsGroup: 2000
  containers:
  - name: test
    image: alpine:3.18
    command: ["/bin/sh", "-c", "id -u && id -g && id -G && exit 0"]
EOF

kubectl wait --for=condition=Ready pod/test-uid-gid --timeout=30s || true
kubectl wait --for=jsonpath='{.status.phase}'=Succeeded pod/test-uid-gid --timeout=60s

# Validate output
LOGS=$(kubectl logs test-uid-gid)
echo "Container output: $LOGS"

# Parse output (expecting: "1000\n1000\n1000 2000")
UID=$(echo "$LOGS" | sed -n '1p')
GID=$(echo "$LOGS" | sed -n '2p')
GROUPS=$(echo "$LOGS" | sed -n '3p')

if [ "$UID" != "1000" ]; then
    echo "ERROR: Expected UID 1000, got $UID"
    exit 1
fi

if [ "$GID" != "1000" ]; then
    echo "ERROR: Expected GID 1000, got $GID"
    exit 1
fi

# Verify fsGroup (2000) is in supplementary groups
if ! echo "$GROUPS" | grep -q "2000"; then
    echo "ERROR: fsGroup 2000 not in supplementary groups: $GROUPS"
    exit 1
fi

echo "✓ UID/GID switching test passed"
kubectl delete pod test-uid-gid
```

**Note:** `fsGroup` handling may require shim changes to translate Kubernetes `securityContext.fsGroup` into OCI `additionalGids`.

#### 3.2 Add Root vs Non-Root Test
**Purpose:** Verify privilege dropping from root to non-root user

```bash
echo "==== Test: Privilege drop (root to user) ===="
cat <<EOF | kubectl apply -f -
apiVersion: v1
kind: Pod
metadata:
  name: test-privilege-drop
spec:
  runtimeClassName: reaper
  restartPolicy: Never
  securityContext:
    runAsUser: 65534  # nobody user
    runAsGroup: 65534
  containers:
  - name: test
    image: alpine:3.18
    command: ["/bin/sh", "-c", "id -u && id -un && test ! -w /etc/shadow && echo 'privilege-drop-ok'"]
EOF

kubectl wait --for=jsonpath='{.status.phase}'=Succeeded pod/test-privilege-drop --timeout=60s

LOGS=$(kubectl logs test-privilege-drop)
if ! echo "$LOGS" | grep -q "privilege-drop-ok"; then
    echo "ERROR: Privilege drop test failed"
    exit 1
fi

echo "✓ Privilege drop test passed"
kubectl delete pod test-privilege-drop
```

#### 3.3 Add Negative Test (Permission Denied)
**Purpose:** Verify that invalid UID/GID configurations fail gracefully

```bash
echo "==== Test: Invalid user configuration (should fail gracefully) ===="
# This test verifies error handling when non-root runtime tries to switch to root
# In Reaper, the runtime itself runs as root in the container context,
# so we test switching to a user that doesn't exist
cat <<EOF | kubectl apply -f -
apiVersion: v1
kind: Pod
metadata:
  name: test-invalid-user
spec:
  runtimeClassName: reaper
  restartPolicy: Never
  securityContext:
    runAsUser: 999999  # Non-existent UID
    runAsGroup: 999999
  containers:
  - name: test
    image: alpine:3.18
    command: ["/bin/true"]
EOF

# Pod should reach a terminal state (either Failed or Succeeded)
kubectl wait --for=condition=Ready=false pod/test-invalid-user --timeout=30s || true
kubectl wait --for=jsonpath='{.status.phase}'=Failed pod/test-invalid-user --timeout=60s || \
kubectl wait --for=jsonpath='{.status.phase}'=Succeeded pod/test-invalid-user --timeout=60s

# Check that container has a non-zero exit code or error message
STATUS=$(kubectl get pod test-invalid-user -o jsonpath='{.status.containerStatuses[0].state.terminated.exitCode}')
if [ "$STATUS" = "0" ]; then
    echo "Warning: Container succeeded despite invalid UID (this may be expected if UID validation is not enforced)"
else
    echo "✓ Invalid user configuration handled correctly (exit code: $STATUS)"
fi

kubectl delete pod test-invalid-user
```

---

### Phase 4: Documentation Updates ⏱️ 1 hour

#### 4.1 Update TESTING.md
**File:** [TESTING.md](../../TESTING.md)

**Additions:**
- Document new UID/GID tests in unit test section
- Add prerequisites (running as root for some tests)
- Document how to skip tests: `cargo test -- --skip test_privilege_drop_from_root`

#### 4.2 Update CLAUDE.md
**File:** [CLAUDE.md](../../CLAUDE.md)

**Changes:**
- Remove "user switching is currently disabled" note
- Add security note about privilege dropping order
- Document that UID/GID switching requires root privileges for cross-user switching

#### 4.3 Update SHIMV2_DESIGN.md
**File:** [docs/SHIMV2_DESIGN.md](SHIMV2_DESIGN.md)

**Additions:**
- Add section on user/group management
- Document privilege dropping sequence
- Note interaction with overlay filesystem (both use `pre_exec`)

#### 4.4 Update TODO.md
**File:** [docs/TODO.md](TODO.md)

**Change:**
```diff
-[ ] Ensure uid and gid changes are validated in the integration tests
+[x] Ensure uid and gid changes are validated in the integration tests
```

---

## Testing Strategy

### Test Matrix

| Test Type | Environment | Root Required | Overlay | Purpose |
|-----------|-------------|---------------|---------|---------|
| Unit (parsing) | macOS/Linux | No | Disabled | Config parsing validation |
| Unit (switching) | Linux | Yes | Enabled | Actual UID/GID switching |
| Integration (K8s) | Kind cluster | No* | Enabled | End-to-end with `securityContext` |

**Note:** Kubernetes integration tests run in Kind (Linux containers), so the runtime runs as root inside the container namespace.

### Test Execution Order
1. **Phase 2 (Unit)** - Fast, catches obvious bugs early
2. **Phase 3 (K8s)** - Slower, validates full integration
3. **Manual validation** - Deploy to real cluster (GKE/EKS) if available

### CI/CD Considerations
- Some unit tests require root → skip on CI unless running in Docker with `--privileged`
- Add conditional test execution:
  ```rust
  #[test]
  fn test_that_needs_root() {
      if unsafe { nix::libc::getuid() } != 0 {
          eprintln!("SKIP: requires root");
          return;
      }
      // ... test code ...
  }
  ```

---

## Security Considerations

### Privilege Dropping Order
**Critical:** Must follow this sequence to prevent privilege escalation:
1. `setgroups()` - Set supplementary groups
2. `setgid()` - Drop group privileges
3. `setuid()` - Drop user privileges (irreversible)

**Why:** Once `setuid()` is called, process loses ability to change groups. Must set groups while still privileged.

### Root in Container Context
- Reaper runtime runs as root inside Kubernetes pod (containerd runs as root)
- Safe to switch to any UID/GID
- No additional capabilities required beyond `CAP_SETUID` / `CAP_SETGID` (which root has)

### Overlay Filesystem Interaction
- Overlay namespace is created BEFORE user switching (at [line 374-388](../src/bin/reaper-runtime/main.rs#L374-L388))
- User switching happens in `pre_exec` (after fork, before exec)
- **Order is critical:** Overlay setup → Fork → Detach → Join overlay → Spawn (with UID/GID in pre_exec)

---

## Rollout Plan

### Step 1: Enable Feature (Phase 1)
- Implement UID/GID switching in runtime
- Default behavior: apply user config if present
- No feature flag needed (OCI spec compliance is mandatory)

### Step 2: Add Tests (Phase 2 & 3)
- Unit tests first (fast feedback)
- K8s integration tests second (slower but comprehensive)

### Step 3: Validate on Real Cluster
- Deploy to GKE or EKS (if available)
- Test with actual production-like security contexts
- Verify no regressions

### Step 4: Update Documentation (Phase 4)
- Remove "disabled" warnings
- Add security best practices
- Update architecture docs

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Breaking existing workloads | High | UID/GID switching only applies if `user` field is set in OCI config (backward compatible) |
| Permission errors in K8s | Medium | Clear error messages, fail fast (current behavior for spawn errors) |
| Overlay + UID/GID interaction | Medium | Test thoroughly, document execution order |
| CI tests require root | Low | Skip root-only tests on CI, document requirement |

---

## Success Criteria

### Must Have ✅
- [ ] UID/GID switching works for current user → current user (no-op validation)
- [ ] UID/GID switching works for root → non-root (privilege drop)
- [ ] Supplementary groups (`additionalGids`) applied correctly
- [ ] Umask setting works
- [ ] Kubernetes integration test passes with `runAsUser` / `runAsGroup`
- [ ] All existing tests still pass (no regressions)

### Nice to Have 🎯
- [ ] `fsGroup` support (requires shim changes)
- [ ] Real cluster testing (GKE/EKS)
- [ ] Performance benchmarks (UID/GID switching overhead)

---

## Effort Estimate

| Phase | Hours | Notes |
|-------|-------|-------|
| Phase 1: Implementation | 2-3 | Straightforward, mostly copy-paste pattern |
| Phase 2: Unit Tests | 3-4 | Includes FIFO handling, output validation |
| Phase 3: K8s Tests | 4-5 | Bash scripting, yaml manifests, validation logic |
| Phase 4: Documentation | 1 | Updating existing docs |
| **Total** | **10-13** | Plus contingency for debugging |

**Contingency:** Add 25% buffer for debugging edge cases (especially overlay + UID/GID interaction)

---

## Next Steps

1. **Review this plan** - Get feedback on approach and scope
2. **Phase 1 implementation** - Enable UID/GID switching in runtime
3. **Phase 2 testing** - Validate with unit tests
4. **Phase 3 integration** - End-to-end Kubernetes tests
5. **Phase 4 documentation** - Update all relevant docs
6. **Delete this plan file** - Once work is complete and merged

---

## References

- OCI Runtime Specification: https://github.com/opencontainers/runtime-spec/blob/main/config.md#user
- Kubernetes Security Context: https://kubernetes.io/docs/tasks/configure-pod-container/security-context/
- Linux Privilege Dropping: https://wiki.sei.cmu.edu/confluence/display/c/POS37-C.+Ensure+that+privilege+relinquishment+is+successful
