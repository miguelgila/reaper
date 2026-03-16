# Quick Start

This guide assumes you have a Reaper-enabled cluster (see [Installation](installation.md)).

## Run a Command on the Host

Create a Pod with `runtimeClassName: reaper-v2`:

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

> **Note:** The `image` field is required by Kubernetes but ignored by Reaper. Use a small image like `busybox`.

## Interactive Shell

```bash
kubectl run -it debug --rm --image=busybox --restart=Never \
  --overrides='{"spec":{"runtimeClassName":"reaper-v2"}}' \
  -- /bin/bash
```

## Exec into Running Containers

```bash
kubectl exec -it my-pod -- /bin/sh
```

## Using Volumes

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

## ReaperPod CRD

A simplified, Reaper-native way to run workloads:

```yaml
apiVersion: reaper.io/v1alpha1
kind: ReaperPod
metadata:
  name: hello
spec:
  command: ["/bin/sh", "-c", "echo Hello from $(hostname)"]
```

```bash
kubectl apply -f hello.yaml
kubectl get reaperpods
kubectl describe reaperpod hello
```

See [ReaperPod CRD Reference](../reference/crds.md) for the full spec.
