# Pod Field Compatibility

Reaper implements the Kubernetes Pod API but **ignores or doesn't support certain container-specific fields** since it runs processes directly on the host without traditional container isolation.

## Field Reference

| Pod Field                                        | Behavior                                                                                                                                                               |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `spec.containers[].image`                        | **Ignored by Reaper** — Kubelet pulls the image before the runtime runs, so a valid image is required. Use a lightweight image like `busybox`. Reaper does not use it. |
| `spec.containers[].resources.limits`             | **Ignored** — No cgroup enforcement; processes use host resources.                                                                                                     |
| `spec.containers[].resources.requests`           | **Ignored** — Scheduling hints not used.                                                                                                                               |
| `spec.containers[].volumeMounts`                 | **Supported** — Bind mounts for ConfigMap, Secret, hostPath, emptyDir.                                                                                                 |
| `spec.containers[].securityContext.capabilities` | **Ignored** — Processes run with host-level capabilities.                                                                                                              |
| `spec.containers[].livenessProbe`                | **Ignored** — No health checking.                                                                                                                                      |
| `spec.containers[].readinessProbe`               | **Ignored** — No readiness checks.                                                                                                                                     |
| `spec.containers[].command`                      | **Supported** — Program path on host (must exist).                                                                                                                     |
| `spec.containers[].args`                         | **Supported** — Arguments to the command.                                                                                                                              |
| `spec.containers[].env`                          | **Supported** — Environment variables.                                                                                                                                 |
| `spec.containers[].workingDir`                   | **Supported** — Working directory for the process.                                                                                                                     |
| `spec.runtimeClassName`                          | **Required** — Must be set to `reaper-v2`.                                                                                                                             |

## Best Practice

Use a small, valid image like `busybox`. Kubelet pulls the image before handing off to the runtime, so the image must exist in a registry. Reaper itself ignores the image entirely — it runs the `command` directly on the host.

## Supported Features Summary

| Feature | Status |
|---------|--------|
| `command` / `args` | Supported |
| `env` / `envFrom` | Supported |
| `volumeMounts` (ConfigMap, Secret, hostPath, emptyDir) | Supported |
| `workingDir` | Supported |
| `securityContext.runAsUser` / `runAsGroup` | Supported |
| `restartPolicy` | Supported (by kubelet) |
| `runtimeClassName` | Required (`reaper-v2`) |
| Resource limits/requests | Ignored |
| Probes (liveness, readiness, startup) | Ignored |
| Capabilities | Ignored |
| Image pulling | Handled by kubelet, ignored by Reaper |
