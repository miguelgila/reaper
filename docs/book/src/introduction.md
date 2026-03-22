# Reaper

**Reaper is a lightweight Kubernetes container-less runtime that executes commands directly on cluster nodes without traditional container isolation.**

Think of it as a way to run host-native processes through Kubernetes' orchestration layer — standard Kubernetes API (Pods, kubectl logs, kubectl exec) with full host access.

## What Reaper Provides

- Standard Kubernetes API (Pods, kubectl logs, kubectl exec)
- Process lifecycle management (start, stop, restart)
- Shared overlay filesystem for workload isolation from host changes
- Kubernetes volumes (ConfigMap, Secret, hostPath, emptyDir)
- Sensitive host file filtering (SSH keys, passwords, SSL keys)
- Interactive sessions (PTY support)
- UID/GID switching with `securityContext`
- Per-pod configuration via Kubernetes annotations
- **Custom Resource Definitions**: [ReaperPod](reference/crds.md#reaperpod) (simplified workloads), [ReaperOverlay](reference/crds.md#reaperoverlay) (overlay lifecycle management), [ReaperDaemonJob](reference/crds.md#reaperdaemonjob) (run jobs on every node with dependency ordering)
- **[Helm chart](reference/helm-chart.md)** for one-command installation and configuration

## What Reaper Does NOT Provide

- Container isolation (namespaces, cgroups)
- Resource limits (CPU, memory)
- Network isolation (uses host networking)
- Container image pulling

## Use Cases

- **HPC workloads**: Slurm worker daemons that need direct CPU/GPU access
- **Cluster maintenance**: Ansible playbooks and system configuration tasks
- **Privileged system utilities**: Direct hardware access, device management
- **Node monitoring**: Host-level metric exporters (node_exporter, etc.)
- **Legacy applications**: Programs that require host-level access
- **Development and debugging**: Interactive host access via kubectl

## Disclaimer

Reaper is an experimental, personal project built to explore what's possible with AI-assisted development. It is under continuous development with no stability guarantees. Use entirely at your own risk.

## Source Code

The source code is available at [github.com/miguelgila/reaper](https://github.com/miguelgila/reaper).
