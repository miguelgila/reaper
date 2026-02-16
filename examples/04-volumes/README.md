# 04-volumes — Kubernetes Volume Mounts

Demonstrates Reaper's support for four Kubernetes volume types: **ConfigMap**, **Secret**, **hostPath**, and **emptyDir**. Each demo runs as a Pod on a labeled worker node and showcases how Reaper handles volume mounts inside its shared overlay namespace.

A key highlight: software like nginx is installed *inside* the pod commands via `apt-get`, proving that Reaper's overlay namespace allows package installation without modifying the host.

## Setup

From the repository root:

```bash
./examples/04-volumes/setup.sh
```

This creates a 2-node Kind cluster (1 control-plane + 1 worker) with:
- Reaper installed on all nodes
- The worker labeled `role=demo`
- **ConfigMap** `nginx-config` — custom nginx server block (listen 8080)
- **Secret** `app-credentials` — sample username, password, and API key
- **Host directory** `/opt/reaper-demo/html` on the demo worker with a custom HTML file

## Demos

> **Important:** Reaper workloads share a single overlay namespace per node. Delete each demo pod before running the next to avoid port conflicts and config bleed between pods (e.g., nginx configs from one pod being visible to another).


### 1. ConfigMap — nginx with custom config

Mounts a ConfigMap as an nginx config file, installs nginx in the overlay, and serves a custom welcome page.

```bash
kubectl apply -f examples/04-volumes/configmap-nginx.yaml
kubectl logs configmap-nginx -f
```

Once nginx is running, verify from another terminal:

```bash
kubectl exec configmap-nginx -- curl -s http://localhost:8080
kubectl exec configmap-nginx -- curl -s http://localhost:8080/health
```

**What it demonstrates:**
- ConfigMap mounted as a single file via `subPath`
- Real application (nginx) configured entirely via volume mount
- Package installation inside overlay (host filesystem untouched)

### 2. Secret — read-only credentials

Mounts a Secret at `/etc/secrets/` (read-only) and reads the credential files.

```bash
kubectl apply -f examples/04-volumes/secret-env.yaml
kubectl logs secret-reader -f
```

**What it demonstrates:**
- Secret files mounted at a custom path
- Read-only enforcement (write attempts are rejected)
- Sensitive data delivered securely via Kubernetes Secrets

### 3. hostPath — serve files from the host

Reads a custom HTML file from the host's `/opt/reaper-demo/html` directory, then installs nginx in the overlay to serve it.

```bash
kubectl apply -f examples/04-volumes/hostpath-logs.yaml
kubectl logs hostpath-reader -f
```

Once nginx is running, verify from another terminal:

```bash
kubectl exec hostpath-reader -- curl -s http://localhost:8081
```

**What it demonstrates:**
- hostPath volume mounting host directories into the pod
- Read-only access to host-provided files
- Combining host content with overlay-installed software

### 4. emptyDir — ephemeral scratch workspace

Uses an emptyDir volume at `/workspace` to generate data files, process them, and produce a summary report.

```bash
kubectl apply -f examples/04-volumes/emptydir-workspace.yaml
kubectl logs emptydir-worker -f
```

**What it demonstrates:**
- emptyDir providing writable scratch storage
- Multi-step data processing workflow
- Ephemeral storage that disappears when the pod is deleted

## How It Works

Reaper supports Kubernetes volumes through the standard OCI runtime contract:

1. **Kubelet** prepares volume content as host directories (downloads ConfigMap/Secret data, creates emptyDir, resolves hostPath)
2. **Containerd** writes bind-mount directives to the OCI `config.json` `mounts` array
3. **Reaper runtime** reads the mounts array and performs bind mounts inside the shared overlay namespace

Because all mounts happen inside the overlay namespace, the host filesystem remains protected. Volume content appears at the expected paths, and workloads interact with volumes exactly as they would in a traditional container runtime.

Read-only mounts (like Secrets) are enforced via `MS_RDONLY` remount flags.

## Cleanup

Delete the pods:

```bash
kubectl delete -f examples/04-volumes/
```

Delete the cluster:

```bash
./examples/04-volumes/setup.sh --cleanup
```
