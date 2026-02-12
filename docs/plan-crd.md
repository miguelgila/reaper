# Plan: Custom Resource Definition (CRD) for Reaper

## Executive Summary

**Recommendation: Do NOT create a CRD at this time. Continue using standard Pod resources with RuntimeClass.**

**Reasoning:**
- Current Pod approach works well for the intended use case (host-native process execution)
- CRD introduces significant complexity without proportional benefits
- Container-specific features (images, cgroups, volumes) are intentionally bypassed by design
- Standard Kubernetes tooling (kubectl, monitoring, RBAC) works out-of-the-box with Pods
- The "impedance mismatch" with container features is a documentation/education issue, not a technical limitation

## Current State Analysis

### What Reaper Does Today

Reaper is a **container runtime that deliberately runs processes directly on the host** without containerization:

1. **OCI Runtime Interface**: Implements the standard OCI runtime spec (create/start/kill/delete)
2. **Containerd Shim v2**: Standard shim protocol for lifecycle management
3. **RuntimeClass Integration**: Users specify `runtimeClassName: reaper-v2` in Pod manifests
4. **Pod → Process Mapping**: Each container in a Pod becomes a host process (no isolation)
5. **Host-Native Execution**: Processes run with full host access, no namespaces or cgroups

### What Container Features Don't Apply

The following Kubernetes/container features are **intentionally not implemented** in Reaper:

| Feature | Why Not Applicable |
|---------|-------------------|
| **Container Images** | No image pulling—Reaper expects binaries to exist on the host. The "image" field in Pod specs is ignored. |
| **Cgroup Limits** | No resource isolation—processes use host resources directly. CPU/memory limits in Pod specs are ignored. |
| **Network Namespaces** | Processes use host networking—no pod-level IP addresses. |
| **Volume Mounts** | Currently not implemented—would need custom implementation for `hostPath` volumes. |
| **PID Namespace** | Processes share host PID namespace—no PID isolation. |
| **IPC Namespace** | Processes share host IPC—no isolation. |
| **User Namespaces** | No UID remapping—processes run as specified UID on host. |

### What Currently Works

Despite the above limitations, these features **do work**:

✅ **Process Execution**: Spawn and monitor host processes
✅ **Exit Code Capture**: Accurate exit codes via fork-first architecture
✅ **Stdout/Stderr Logging**: `kubectl logs` works via FIFO redirection
✅ **Interactive Sessions**: `kubectl run -it` and `kubectl exec -it` with PTY support
✅ **Overlay Filesystem**: Shared writable layer for process isolation from host filesystem
✅ **Pod Lifecycle**: Proper state transitions (Pending → Running → Completed/Failed)
✅ **RuntimeClass Selection**: Standard Kubernetes mechanism to opt into Reaper
✅ **RBAC & Security Policies**: Standard Pod Security Standards and OPA policies apply

## Problem Statement (TODO Line 7)

> "Evaluate creating a CRD similar to pods/deployments/daemonsets to avoid code that is built for containers (images)"

**Analysis:** The "problem" is that Pod specs contain fields (like `image`, `resources.limits`) that don't apply to Reaper. This creates user confusion and potential errors when users specify values that are silently ignored.

**Current Workarounds:**
1. Documentation clearly states which Pod fields are ignored
2. RuntimeClass acts as an "opt-in" marker for host-native execution
3. OCI bundle's `config.json` contains the actual process spec (args, env, cwd)

## Option 1: Continue with Pod + RuntimeClass (RECOMMENDED)

### Pros

1. **Zero Additional Complexity**
   - No new controllers to write, test, or maintain
   - No CRD lifecycle management
   - No API versioning concerns (v1alpha1 → v1beta1 → v1)

2. **Standard Tooling Works**
   - `kubectl logs`, `kubectl exec`, `kubectl describe pod`
   - Monitoring tools (Prometheus, Grafana) understand Pods
   - Log aggregators (Fluentd, Loki) scrape Pod logs
   - RBAC policies target Pods (`pods.create`, `pods/log`, `pods/exec`)

3. **Integration with Existing Ecosystems**
   - CI/CD pipelines know how to deploy Pods
   - Helm charts, Kustomize, and GitOps tools handle Pods natively
   - Admission controllers (OPA, Kyverno) validate Pods
   - Pod Security Standards apply directly

4. **No Breaking Changes**
   - Existing users continue working without migration
   - No need to rewrite automation or documentation

5. **Low Maintenance Burden**
   - Kubernetes API server handles Pod CRUD operations
   - Scheduler, kubelet, and containerd handle the rest
   - Reaper only needs to implement the OCI runtime interface

### Cons

1. **Field Confusion**
   - Pod specs have fields like `image`, `resources.limits` that don't apply
   - Users might specify values that are silently ignored
   - **Mitigation:** Documentation, validation webhooks, error messages

2. **No Type Safety**
   - Can't enforce "Reaper-specific" fields at the API level
   - **Mitigation:** Validation webhooks can reject invalid Pod specs

3. **Mental Model Mismatch**
   - Users think "container" but get "process"
   - **Mitigation:** Clear documentation and naming (e.g., "ReaperTask Pod")

### Implementation Effort

**Time Estimate: 0 days** (already implemented)

- Reaper already works with Pods via RuntimeClass
- Documentation improvements: 1-2 days
- Optional validation webhook: 2-3 days

## Option 2: Create a Custom Resource Definition (CRD)

### Example: ReaperTask CRD

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperTask
metadata:
  name: my-task
  namespace: default
spec:
  restartPolicy: Never
  command: ["/usr/local/bin/my-binary"]
  args: ["--flag", "value"]
  env:
    - name: VAR1
      value: "value1"
  workingDir: "/var/app"
  user:
    uid: 1000
    gid: 1000
  terminal: false
  stdin: false
  stdout: true
  stderr: true
```

### Pros

1. **Semantic Clarity**
   - API expresses exactly what Reaper supports
   - No confusing `image` or `resources.limits` fields
   - Clear intent: "I want to run a host process, not a container"

2. **Type Safety**
   - OpenAPI schema enforces valid fields
   - Incorrect fields rejected at API level (no silent ignoring)

3. **Custom Status Fields**
   - Can add Reaper-specific status like `overlayNamespaceID`, `hostProcessPID`
   - Better observability into runtime state

4. **Future Extensibility**
   - Easier to add Reaper-specific features (e.g., `hostPathVolumes`, `overlayMode`)
   - Can version the API independently (`v1alpha1` → `v1beta1` → `v1`)

### Cons

1. **Massive Implementation Effort**
   - **Controller**: Must implement reconciliation loop (create/update/delete)
   - **Pod Translation**: Controller must generate internal Pod with RuntimeClass
   - **State Sync**: Must sync ReaperTask status with underlying Pod status
   - **Error Handling**: Must handle Pod failures and reflect in ReaperTask status
   - **Finalizers**: Must clean up resources on ReaperTask deletion
   - **Webhooks**: Need validation and defaulting webhooks
   - **Testing**: Unit tests, integration tests, e2e tests for controller logic
   - **Deployment**: Must deploy controller as a Deployment, with RBAC, ServiceAccount, Webhooks

2. **Loss of Standard Tooling**
   - `kubectl logs <reaper-task>` **won't work** (logs live on the underlying Pod)
   - `kubectl exec <reaper-task>` **won't work** (exec targets Pods, not CRDs)
   - Monitoring tools won't scrape ReaperTask resources (they look for Pods)
   - Log aggregators won't collect logs from ReaperTasks
   - RBAC policies need new rules (`reapertasks.create`, not `pods.create`)

3. **Integration Complexity**
   - CI/CD pipelines need updates to deploy ReaperTasks instead of Pods
   - Helm charts and Kustomize need ReaperTask support
   - Admission controllers need ReaperTask policies (duplicate effort)
   - Custom dashboards, alerts, and observability tooling required

4. **Maintenance Burden**
   - New component to test, version, release, and document
   - Controller bugs can break all ReaperTask instances
   - Kubernetes API changes require controller updates
   - Breaking changes require migration path (v1alpha1 → v1beta1)

5. **Hidden Complexity**
   - Controller still creates Pods under the hood
   - Now there are **two resources** for every workload (ReaperTask + Pod)
   - Debugging requires inspecting both resources
   - Confusion about "which is the source of truth?"

### Implementation Effort

**Time Estimate: 4-8 weeks (160-320 hours)**

Breakdown:
1. **CRD Definition**: 1-2 days
   - Define OpenAPI schema
   - Write validation rules
   - Test with kubectl apply

2. **Controller Implementation**: 2-3 weeks
   - Reconcile loop (create Pods from ReaperTasks)
   - Status sync (Pod status → ReaperTask status)
   - Finalizers (cleanup on deletion)
   - Leader election (multi-replica controller)
   - Error handling and retries

3. **Webhooks**: 1 week
   - Validation webhook (enforce schema)
   - Defaulting webhook (set defaults for optional fields)
   - TLS certificate management
   - Webhook server deployment

4. **Testing**: 1-2 weeks
   - Unit tests (controller logic)
   - Integration tests (controller + API server)
   - E2E tests (full lifecycle: create → run → delete)
   - Failure scenarios (Pod failures, controller restarts)

5. **Documentation**: 1 week
   - API reference
   - User guide
   - Migration guide (Pod → ReaperTask)
   - Troubleshooting guide

6. **CI/CD & Release**: 1 week
   - Dockerize controller
   - Helm chart for deployment
   - Release automation
   - Version upgrade testing

## Option 3: Hybrid Approach (Pod + Validation Webhook)

### Description

Keep using Pods, but add a **validation webhook** that:
- Rejects Pods with `runtimeClassName: reaper-v2` if they specify `image` (or warns)
- Warns if `resources.limits` is specified (not enforced)
- Provides helpful error messages

### Pros

- ✅ Maintains standard Pod tooling
- ✅ Improves user experience (early validation)
- ✅ Minimal implementation effort (3-5 days)

### Cons

- ❌ Still requires webhook deployment and TLS management
- ❌ Doesn't fully eliminate "field confusion"

### Implementation Effort

**Time Estimate: 3-5 days**

1. Write validation webhook (1 day)
2. Write tests (1 day)
3. Deploy webhook with cert-manager (1 day)
4. Documentation (1 day)

## Comparison Matrix

| Criteria | Pod + RuntimeClass | CRD (ReaperTask) | Pod + Webhook |
|----------|-------------------|------------------|---------------|
| **Implementation Time** | 0 days | 4-8 weeks | 3-5 days |
| **Maintenance Burden** | Low | High | Medium |
| **kubectl logs Works** | ✅ Yes | ❌ No (need proxy) | ✅ Yes |
| **kubectl exec Works** | ✅ Yes | ❌ No (need proxy) | ✅ Yes |
| **Standard Monitoring** | ✅ Yes | ❌ No (custom) | ✅ Yes |
| **Clear API Semantics** | ⚠️ Some confusion | ✅ Very clear | ✅ Better |
| **Type Safety** | ⚠️ Silently ignores fields | ✅ Full validation | ✅ Full validation |
| **RBAC Compatibility** | ✅ Standard Pod RBAC | ❌ New RBAC needed | ✅ Standard Pod RBAC |
| **CI/CD Integration** | ✅ Works out-of-box | ❌ Needs updates | ✅ Works out-of-box |
| **Future Extensibility** | ⚠️ Limited | ✅ High | ⚠️ Limited |

## Recommendation

**Choose Option 1: Continue with Pod + RuntimeClass**

Optionally add **Option 3 (Validation Webhook)** later if user confusion becomes a significant problem.

### Reasoning

1. **ROI is Negative for CRD**
   - 4-8 weeks of implementation + ongoing maintenance
   - Loss of standard tooling (`kubectl logs`, `kubectl exec`, monitoring)
   - Increased complexity (controller + CRD + Pod) vs. simpler (Pod only)
   - Minimal benefit: clearer API, but same functionality

2. **Current Pod Approach is Sufficient**
   - RuntimeClass provides clear "opt-in" semantics
   - Standard tooling works perfectly
   - RBAC, security policies, and observability "just work"
   - Documentation can address field confusion

3. **CRD Would Complicate, Not Simplify**
   - Users would need to learn a new API
   - Two resources per workload (ReaperTask + underlying Pod)
   - Debugging requires understanding the mapping
   - Migration pain for existing users

4. **Real Problem is Education, Not Technology**
   - The "problem" is that users might specify `image: busybox` and wonder why it's ignored
   - This is solved by **documentation** and **validation webhooks**, not by building a new API
   - Example: Document that "Reaper ignores `image` field; binaries must exist on host"

## Alternative Solutions (No CRD Required)

Instead of building a CRD, consider these lightweight approaches:

### 1. Enhanced Documentation

**File: `docs/POD_SPECIFICATION.md`**

```markdown
# Using Pods with Reaper Runtime

Reaper runs processes directly on the host, not in containers. When writing Pod specs:

❌ **Ignored Fields** (silently ignored by Reaper):
- `spec.containers[].image` (Reaper doesn't pull images)
- `spec.containers[].resources.limits` (no cgroup enforcement)
- `spec.containers[].volumeMounts` (not yet implemented)

✅ **Supported Fields**:
- `spec.containers[].command` (program path on host)
- `spec.containers[].args` (arguments to program)
- `spec.containers[].env` (environment variables)
- `spec.containers[].workingDir` (process working directory)
- `spec.runtimeClassName` (must be "reaper-v2")

📝 **Example:**
```yaml
apiVersion: v1
kind: Pod
metadata:
  name: my-task
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  containers:
    - name: task
      image: placeholder  # Ignored by Reaper
      command: ["/usr/local/bin/my-binary"]
      args: ["--flag", "value"]
      env:
        - name: VAR1
          value: "value1"
```

**Best Practice:** Set `image: placeholder` to make it clear the image is not used.
```

### 2. Validation Webhook (Optional)

Deploy a webhook that validates Pods with `runtimeClassName: reaper-v2`:

- ❌ **Reject** if `image` is not `placeholder` or empty (with helpful error message)
- ⚠️ **Warn** if `resources.limits` is specified
- ⚠️ **Warn** if `volumeMounts` is specified

Example error message:
```
Error: Reaper runtime does not use container images.
Set 'image: placeholder' or leave it empty.
The binary at '/usr/local/bin/my-binary' must exist on the host.
```

### 3. Improved Error Messages in Runtime

When Reaper encounters unsupported features in `config.json`:

```rust
if let Some(ref image) = oci_config.image {
    if image != "placeholder" && !image.is_empty() {
        tracing::warn!(
            "Container image '{}' is ignored by Reaper. \
             Ensure the binary exists on the host at the specified path.",
            image
        );
    }
}
```

### 4. Linter/Pre-flight Tool

Create a CLI tool users can run before deploying:

```bash
$ reaper-validate pod.yaml
⚠️  Warning: Field 'image: busybox' will be ignored
⚠️  Warning: Field 'resources.limits.memory' will be ignored
✅ Pod is valid for Reaper runtime
```

## Migration Path (If CRD Becomes Necessary)

If, in the future, Reaper's feature set diverges significantly from standard containers (e.g., adds host-specific features like persistent overlay namespaces, host volume mounts, etc.), then a CRD might make sense.

**Incremental Migration:**
1. **Phase 1**: Continue supporting Pods (current state)
2. **Phase 2**: Introduce ReaperTask CRD as **optional** (controller creates Pods under the hood)
3. **Phase 3**: Deprecate direct Pod usage (recommend ReaperTask instead)
4. **Phase 4**: (Optional) Remove Pod support if desired

**Estimated Timeline:**
- Phase 1 → Phase 2: 4-8 weeks (CRD + controller)
- Phase 2 → Phase 3: 6-12 months (deprecation period)
- Phase 3 → Phase 4: 6-12 months (removal, if needed)

## Conclusion

**Do NOT create a CRD now.** The current Pod + RuntimeClass approach is:
- ✅ Fully functional
- ✅ Standards-compliant
- ✅ Compatible with Kubernetes tooling
- ✅ Low maintenance burden

**If user confusion becomes a problem,** add a **validation webhook** (3-5 days of work) rather than a full CRD (4-8 weeks of work).

**Revisit this decision** if Reaper's feature set significantly diverges from standard containers, such that the Pod API becomes a poor fit.

---

**Difficulty Assessment:**
- **CRD Creation**: Moderate (3-5 days to define schema and deploy)
- **Controller Implementation**: High (3-4 weeks for robust reconciliation)
- **Integration & Testing**: High (2-3 weeks for full e2e testing)
- **Ongoing Maintenance**: High (new component to maintain, version, and debug)

**Total Difficulty: HIGH** (8-12 weeks initial, plus ongoing maintenance)

**Current Approach Difficulty: LOW** (already working, minimal maintenance)

**Verdict: The juice is not worth the squeeze.**
