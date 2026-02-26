# Example 08 — Mix Container Runtime Engines

This example demonstrates **mixed runtime engines** in the same Kubernetes cluster: a standard containerized OpenLDAP server (default containerd/runc runtime) alongside Reaper workloads that configure every worker node to authenticate against it via SSSD.

This is the first example showing Reaper workloads consuming a service from a traditional container — a realistic pattern for hybrid clusters where some services run in containers and others need direct host access.

## What This Demonstrates

| Concept | How |
|---------|-----|
| Mixed runtimes | OpenLDAP runs with default containerd/runc; SSSD configuration runs via Reaper |
| Service consumption | Reaper workloads reach a ClusterIP service via kube-proxy iptables |
| Host-level system changes | SSSD packages, config files, and daemon installed on each node via overlay |
| Dependency ordering | Init containers poll for Ansible availability and LDAP connectivity |
| Kubernetes DNS | Reaper pods resolve services via CoreDNS (`REAPER_DNS_MODE=kubernetes` in `/etc/reaper/reaper.conf`) |

## Topology

```
                    ┌─────────────────────┐
                    │   control-plane      │
                    │   (no workloads)     │
                    └─────────────────────┘
                              │
          ┌───────────────────┼───────────────────┐
          ▼                   ▼                   ▼
┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐
│    worker-0     │ │    worker-1     │ │    worker-2     │
│   role=login    │ │  role=compute   │ │  role=compute   │
│                 │ │                 │ │                 │
│ ┌─────────────┐ │ │                 │ │                 │
│ │  OpenLDAP   │ │ │                 │ │                 │
│ │  (default   │ │ │                 │ │                 │
│ │   runtime)  │ │ │                 │ │                 │
│ └─────────────┘ │ │                 │ │                 │
│                 │ │                 │ │                 │
│ ┌─────────────┐ │ │ ┌─────────────┐ │ │ ┌─────────────┐ │
│ │ base-deps   │ │ │ │ base-deps   │ │ │ │ base-deps   │ │
│ │  (reaper)   │ │ │ │  (reaper)   │ │ │ │  (reaper)   │ │
│ └─────────────┘ │ │ └─────────────┘ │ │ └─────────────┘ │
│ ┌─────────────┐ │ │ ┌─────────────┐ │ │ ┌─────────────┐ │
│ │ base-config │ │ │ │ base-config │ │ │ │ base-config │ │
│ │  (reaper)   │ │ │ │  (reaper)   │ │ │ │  (reaper)   │ │
│ └─────────────┘ │ │ └─────────────┘ │ │ └─────────────┘ │
└─────────────────┘ └─────────────────┘ └─────────────────┘
```

## Workloads

| Name | Runtime | Target Nodes | Purpose |
|------|---------|--------------|---------|
| `openldap` | Default (runc) | role=login | LDAP directory with 5 posixAccount users |
| `base-deps` | Reaper | All workers | Install Ansible into shared overlay |
| `base-config` | Reaper | All workers | Configure SSSD via Ansible playbook |

## Dependency Ordering

All three workloads deploy simultaneously with `kubectl apply -f`. Init containers handle ordering automatically:

```
openldap Deployment + Service start (default runtime, login node)
  └── OpenLDAP boots → loads LDIF → users ready

base-deps DaemonSet starts (Reaper, all 3 workers)
  └── apt-get install ansible → sleep infinity

base-config DaemonSet starts (Reaper, all 3 workers)
  ├── Init 1: wait-for-ansible → polls until ansible-playbook found (up to 300s)
  ├── Init 2: wait-for-ldap → resolves openldap DNS + polls TCP (up to 300s)
  └── Main: ansible-playbook → install SSSD → configure → start → verify → sleep
```

## Prerequisites

- Docker running
- [kind](https://kind.sigs.k8s.io/) installed
- kubectl installed
- Ansible installed (`pip install ansible`)
- Run from the repository root

## Setup

```bash
# Create cluster with latest release
./examples/08-mix-container-runtime-engines/setup.sh

# Or specify a version
./examples/08-mix-container-runtime-engines/setup.sh v0.2.5
```

## Running the Demo

```bash
# Deploy everything at once
kubectl apply -f examples/08-mix-container-runtime-engines/

# Wait for rollouts
kubectl rollout status deployment/openldap --timeout=120s
kubectl rollout status daemonset/base-deps --timeout=300s
kubectl rollout status daemonset/base-config --timeout=300s
```

## Verification

```bash
# 1. Check all pods are running
kubectl get pods -o wide

# 2. Verify OpenLDAP has users
kubectl exec deploy/openldap -c openldap -- ldapsearch -x -H ldap://localhost \
  -D "cn=admin,dc=reaper,dc=local" -w adminpassword \
  -b "dc=reaper,dc=local" "(objectClass=posixAccount)" uid uidNumber gidNumber

# 3. Check SSSD configuration logs
kubectl logs -l app=base-config --all-containers --prefix

# 4. Verify user resolution from any worker node
# (sleep gives SSSD's NSS responder time to connect on first lookup)
kubectl run ldap-test --image=busybox --restart=Never --rm -it \
  --overrides='{"spec":{"runtimeClassName":"reaper-v2"}}' \
  -- sh -c 'sleep 1 && getent passwd user1'
```

Expected output for `getent passwd user1`:
```
user1:*:10001:5000:User One:/home/user1:/bin/bash
```

## Cleanup

```bash
# Delete workloads
kubectl delete -f examples/08-mix-container-runtime-engines/

# Delete cluster and ConfigMaps
./examples/08-mix-container-runtime-engines/setup.sh --cleanup
```
