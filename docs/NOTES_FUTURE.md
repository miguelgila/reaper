# Future Notes: Kubernetes Security & Root Pod Management

## Overview

This document covers how to implement fine-grained access control for running root containers in Kubernetes when using Reaper as a container runtime. It addresses the question: **How do we allow specific users to schedule root pods while preventing general users from doing so?**

## Security Model Summary

### How setuid(0) Protection Works

The Linux kernel enforces privilege requirements for `setuid()`:

1. **If reaper-runtime is NOT running as root:**
   - Calling `setuid(0)` will **fail with EPERM** (Operation not permitted)
   - Non-root users **cannot escalate to root** even if config.json says `uid: 0`

2. **If reaper-runtime IS running as root:**
   - `setuid(0)` succeeds (it's already root)
   - `setuid(1000)` also succeeds (root can drop to any UID)

**Key Point:** The kernel prevents unauthorized privilege escalation. The risk is legitimate root operations when the runtime itself runs privileged—which is standard for container runtimes and must be controlled at the orchestration level.

## Kubernetes Access Control Strategies

### Strategy 1: Namespace-Based Isolation (Recommended)

Use different Pod Security Standards per namespace with RBAC to control access.

#### Setup

```yaml
# Privileged namespace for admins
apiVersion: v1
kind: Namespace
metadata:
  name: admin-workloads
  labels:
    # Allow root containers here
    pod-security.kubernetes.io/enforce: privileged
    pod-security.kubernetes.io/audit: privileged
---
# Restricted namespace for regular users
apiVersion: v1
kind: Namespace
metadata:
  name: user-workloads
  labels:
    # Block root containers
    pod-security.kubernetes.io/enforce: restricted
    pod-security.kubernetes.io/audit: restricted
---
# RBAC: admins can create pods in admin-workloads
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: admin-pod-creator
  namespace: admin-workloads
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: edit  # Built-in role with pod create permissions
subjects:
- kind: User
  name: alice@example.com
  apiGroup: rbac.authorization.k8s.io
---
# Regular users can only create in user-workloads
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: user-pod-creator
  namespace: user-workloads
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: edit
subjects:
- kind: Group
  name: developers
  apiGroup: rbac.authorization.k8s.io
```

#### Example Usage

**Admin pod (allowed):**
```yaml
apiVersion: v1
kind: Pod
metadata:
  name: admin-task
  namespace: admin-workloads  # Only admins have access
spec:
  runtimeClassName: reaper
  securityContext:
    runAsUser: 0  # Allowed in this namespace
  containers:
  - name: task
    image: myimage
    command: ["/bin/privileged-operation"]
```

**User pod attempting root (blocked):**
```yaml
apiVersion: v1
kind: Pod
metadata:
  name: user-app
  namespace: user-workloads  # Enforces restricted PSS
spec:
  runtimeClassName: reaper
  securityContext:
    runAsUser: 0  # REJECTED by Pod Security Standards
  containers:
  - name: app
    image: myapp
```

### Strategy 2: OPA/Gatekeeper Policy with User Context

Enforce root restrictions based on the requesting user across all namespaces.

```yaml
apiVersion: templates.gatekeeper.sh/v1
kind: ConstraintTemplate
metadata:
  name: k8srestrictrootbyuser
spec:
  crd:
    spec:
      names:
        kind: K8sRestrictRootByUser
      validation:
        openAPIV3Schema:
          type: object
          properties:
            allowedUsers:
              type: array
              items:
                type: string
  targets:
    - target: admission.k8s.gatekeeper.sh
      rego: |
        package k8srestrictrootbyuser

        violation[{"msg": msg}] {
          # Get the username from the request
          username := input.review.userInfo.username

          # Check if pod tries to run as root
          container := input.review.object.spec.containers[_]
          is_root_user(container)

          # Check if user is in allowed list
          not username_allowed(username)

          msg := sprintf("User %v is not authorized to run containers as root", [username])
        }

        is_root_user(container) {
          container.securityContext.runAsUser == 0
        }

        is_root_user(container) {
          not container.securityContext.runAsNonRoot
          not container.securityContext.runAsUser
        }

        username_allowed(username) {
          allowed := input.parameters.allowedUsers[_]
          username == allowed
        }
---
apiVersion: constraints.gatekeeper.sh/v1beta1
kind: K8sRestrictRootByUser
metadata:
  name: restrict-root-except-admins
spec:
  match:
    kinds:
    - apiGroups: [""]
      kinds: ["Pod"]
  parameters:
    allowedUsers:
    - "alice@example.com"
    - "system:serviceaccount:kube-system:system-admin"
```

### Strategy 3: RuntimeClass-Based Authorization

Create separate RuntimeClasses with different security profiles.

```yaml
# Regular runtime class (non-root enforced)
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: reaper
handler: reaper
---
# Privileged runtime class (allows root)
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: reaper-privileged
handler: reaper
---
# OPA policy: only admins can use reaper-privileged
apiVersion: templates.gatekeeper.sh/v1
kind: ConstraintTemplate
metadata:
  name: k8srestrictruntimeclass
spec:
  crd:
    spec:
      names:
        kind: K8sRestrictRuntimeClass
      validation:
        openAPIV3Schema:
          type: object
          properties:
            restrictedClasses:
              type: array
              items:
                type: string
            allowedUsers:
              type: array
              items:
                type: string
  targets:
    - target: admission.k8s.gatekeeper.sh
      rego: |
        package k8srestrictruntimeclass

        violation[{"msg": msg}] {
          username := input.review.userInfo.username
          runtime_class := input.review.object.spec.runtimeClassName

          # Check if runtime class is restricted
          restricted := input.parameters.restrictedClasses[_]
          runtime_class == restricted

          # Check if user is allowed
          not user_allowed(username)

          msg := sprintf("User %v cannot use RuntimeClass %v", [username, runtime_class])
        }

        user_allowed(username) {
          allowed := input.parameters.allowedUsers[_]
          username == allowed
        }
---
apiVersion: constraints.gatekeeper.sh/v1beta1
kind: K8sRestrictRuntimeClass
metadata:
  name: restrict-privileged-runtime
spec:
  match:
    kinds:
    - apiGroups: [""]
      kinds: ["Pod"]
  parameters:
    restrictedClasses:
    - "reaper-privileged"
    allowedUsers:
    - "alice@example.com"
    - "system:serviceaccount:admin:deployer"
```

### Strategy 4: Pod Security Admission with Exemptions

Configure PSA exemptions for specific users (requires kube-apiserver configuration).

```yaml
# In kube-apiserver configuration or AdmissionConfiguration
apiVersion: apiserver.config.k8s.io/v1
kind: AdmissionConfiguration
plugins:
- name: PodSecurity
  configuration:
    apiVersion: pod-security.admission.config.k8s.io/v1
    kind: PodSecurityConfiguration
    defaults:
      enforce: "restricted"
      audit: "restricted"
      warn: "restricted"
    exemptions:
      usernames:
      - "alice@example.com"
      - "system:serviceaccount:kube-system:cluster-admin"
      runtimeClasses:
      - "reaper-privileged"  # Special RuntimeClass for root workloads
      namespaces:
      - "kube-system"
      - "admin-workloads"
```

### Strategy 5: RBAC + Custom Resource Approach

Define a special ClusterRole for root pod scheduling.

```yaml
# Define a ClusterRole for root pod scheduling
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: root-pod-scheduler
rules:
- apiGroups: [""]
  resources: ["pods"]
  verbs: ["create", "update"]
- apiGroups: ["policy"]
  resources: ["podsecuritypolicies"]
  verbs: ["use"]
  resourceNames: ["allow-root-containers"]
---
# Bind to specific users/service accounts
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: admin-can-schedule-root
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: root-pod-scheduler
subjects:
- kind: User
  name: alice@example.com
  apiGroup: rbac.authorization.k8s.io
- kind: ServiceAccount
  name: system-admin
  namespace: kube-system
```

## Recommended Implementation

**Best approach: Namespace + RBAC + OPA (Defense in Depth)**

1. **Two namespaces with different PSS profiles:**
   - `admin-workloads` (PSS: privileged) - allows root
   - `user-workloads` (PSS: restricted) - blocks root

2. **RBAC bindings:**
   - Admins can create pods in both namespaces
   - Regular users can only create in `user-workloads`

3. **OPA validation** (optional extra layer):
   - Validates `runAsUser` against username
   - Provides audit trail and fine-grained control

This provides:
- ✅ **Clear separation** via namespaces
- ✅ **Access control** via RBAC
- ✅ **Policy enforcement** via PSS
- ✅ **Audit trail** via OPA
- ✅ **Defense in depth** - multiple layers of security

## Safe Pod Manifest Example

Example of a properly configured pod with security context:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: safe-app
spec:
  runtimeClassName: reaper

  # Pod-level security context
  securityContext:
    runAsUser: 1000
    runAsGroup: 1000
    fsGroup: 1000
    runAsNonRoot: true  # Prevent any container from running as root

  containers:
  - name: app
    image: myapp:latest

    # Container-level security context (optional override)
    securityContext:
      runAsUser: 2000
      runAsGroup: 2000
      allowPrivilegeEscalation: false
      readOnlyRootFilesystem: true
      capabilities:
        drop:
        - ALL
```

## Runtime-Level Validation (Future Enhancement)

Consider adding validation directly in reaper-runtime for defense in depth:

```rust
// In src/bin/reaper-runtime/main.rs
if let Some(ref user) = oci_config.process.user {
    if user.uid == 0 && !std::env::var("REAPER_ALLOW_ROOT").is_ok() {
        return Err(anyhow::anyhow!(
            "Running as root (uid=0) is disabled. Set REAPER_ALLOW_ROOT=1 to override."
        ));
    }
}
```

This adds an extra safety layer at the runtime level, though Kubernetes policies should be the primary enforcement mechanism.

## Request Flow with Policies

1. **User submits pod with `uid: 0` in config.json**
2. **Kubernetes admission controller intercepts:**
   - PSS/PSP checks `securityContext.runAsUser`
   - OPA/Gatekeeper validates against username
   - RBAC verifies namespace access
   - If any check fails → **Rejected before scheduling**
3. **Only validated pods reach the node**
4. **Reaper runtime receives config.json with approved UID**
5. **Reaper calls `setuid()` in pre_exec** → process runs with specified UID

## Future Considerations

### User Namespaces

When implementing user namespaces in the future:
- Container `uid: 0` maps to host `uid: 100000` (not real root)
- Provides isolation without requiring complex policies
- Reduces risk of host compromise

### Runtime Environment Variable

Could add `REAPER_ALLOW_ROOT` environment variable to the runtime:
- Default: `false` (reject uid=0)
- Can be enabled per-node for privileged workloads
- Provides runtime-level enforcement independent of Kubernetes

## References

- [Kubernetes Pod Security Standards](https://kubernetes.io/docs/concepts/security/pod-security-standards/)
- [OPA Gatekeeper](https://open-policy-agent.github.io/gatekeeper/)
- [Kubernetes RBAC](https://kubernetes.io/docs/reference/access-authn-authz/rbac/)
- [RuntimeClass](https://kubernetes.io/docs/concepts/containers/runtime-class/)
- [Pod Security Admission](https://kubernetes.io/docs/concepts/security/pod-security-admission/)
