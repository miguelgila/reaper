# Quick Start

This guide assumes you have a Reaper-enabled cluster (see [Installation](installation.md)).

## Run a Command on the Host

The simplest way to run a command on the host is with a ReaperPod:

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: my-task
spec:
  command: ["/bin/sh", "-c", "echo Hello from $(hostname) && uname -a"]
```

```bash
kubectl apply -f my-task.yaml
kubectl logs my-task
kubectl get reaperpods
```

## With Volumes

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: config-reader
spec:
  command: ["/bin/sh", "-c", "cat /config/settings.yaml"]
  volumes:
    - name: config
      mountPath: /config
      configMap: "my-config"
      readOnly: true
```

## With Node Selector

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: compute-task
spec:
  command: ["/bin/sh", "-c", "echo Running on $(hostname)"]
  nodeSelector:
    workload-type: compute
```

See [ReaperPod CRD Reference](../reference/crds.md) for the full spec.

---

## Using Raw Pods

For use cases that need the full Kubernetes Pod API — interactive sessions, DaemonSets, Deployments, exec, etc. — you can use standard Pods with `runtimeClassName: reaper-v2`.

> **Note:** The `image` field is required by Kubernetes but ignored by Reaper. Use a small image like `busybox`.

### Run a Command

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
      image: busybox
      command: ["/bin/sh", "-c"]
      args: ["echo Hello from host && uname -a"]
```

```bash
kubectl apply -f my-task.yaml
kubectl logs my-task        # See output
kubectl get pod my-task     # Status: Completed
```

### Interactive Shell

```bash
kubectl run -it debug --rm --image=busybox --restart=Never \
  --overrides='{"spec":{"runtimeClassName":"reaper-v2"}}' \
  -- /bin/bash
```

### Exec into Running Containers

```bash
kubectl exec -it my-pod -- /bin/sh
```

### Volumes

ConfigMaps, Secrets, hostPath, emptyDir, and projected volumes all work:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: my-task
spec:
  runtimeClassName: reaper-v2
  restartPolicy: Never
  volumes:
    - name: config
      configMap:
        name: my-config
  containers:
    - name: task
      image: busybox
      command: ["/bin/sh", "-c", "cat /config/settings.yaml"]
      volumeMounts:
        - name: config
          mountPath: /config
          readOnly: true
```

See [Pod Compatibility](../configuration/compatibility.md) for the full list of supported and ignored fields.
