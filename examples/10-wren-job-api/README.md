# 10 — Wren Job Execution API (DEPRECATED)

> **DEPRECATED:** The HTTP job execution API (`POST/GET/DELETE /api/v1/jobs`) is
> deprecated. The recommended way to run Wren-managed workloads is via the
> **ReaperPod CRD** (see [example 09](../09-reaperpod/)). The CRD approach
> delegates job lifecycle to Kubernetes, providing native volumes, logs, exec,
> and event handling without reinventing what Kubernetes already provides.
>
> This example is kept for reference only. The `reaper-agent` binary remains
> available behind the `--features agent` flag but is no longer the recommended
> integration path.

Demonstrates the Reaper agent's HTTP job execution API, which the Wren
controller uses to run jobs on bare-metal nodes.

## What It Shows

- **POST /api/v1/jobs** — submit a shell script for execution
- **GET /api/v1/jobs/{id}** — poll job status (running/succeeded/failed)
- **DELETE /api/v1/jobs/{id}** — terminate a running job
- **User identity** — jobs run as a specific UID/GID (privilege dropping)
- **Hostfile** — MPI hostfile written to disk before job starts
- **Working directory** — jobs can specify a working directory
- **Environment variables** — custom env vars passed to the job

## Architecture

```
  Wren Controller                   Reaper Agent (DaemonSet)
  ┌──────────────┐                  ┌─────────────────────┐
  │              │  POST /api/v1/   │                     │
  │ build_job_   │  jobs            │  submit_job_handler │
  │ request()    ├─────────────────►│       │             │
  │              │                  │  ┌────▼────┐        │
  │              │  GET /api/v1/    │  │Executor │        │
  │ poll status  │  jobs/{id}       │  │ nsenter │        │
  │              ├─────────────────►│  │ setuid  │        │
  │              │                  │  │ /bin/sh │        │
  │              │  DELETE          │  └─────────┘        │
  │ cancel job   ├─────────────────►│                     │
  └──────────────┘                  └─────────────────────┘
```

## Prerequisites

- Reaper installed with the agent DaemonSet (Helm chart or manual)
- Agent image built with `--features agent`

## Running the Demo

The `setup.sh` script creates a Kind cluster, installs Reaper with the agent,
and runs through each API operation interactively.

```bash
# Full demo (creates cluster, installs Reaper, runs API examples)
./setup.sh

# Cleanup
./setup.sh --cleanup
```

### Manual Testing

If you already have a cluster with the agent running:

```bash
# Port-forward to the agent pod
AGENT_POD=$(kubectl get pods -n reaper-system -l app.kubernetes.io/name=reaper-agent \
  -o jsonpath='{.items[0].metadata.name}')
kubectl port-forward -n reaper-system "$AGENT_POD" 9100:9100 &

# Submit a job
curl -s -X POST http://localhost:9100/api/v1/jobs \
  -H "Content-Type: application/json" \
  -d '{"script":"echo hello from reaper && hostname","environment":{}}' | jq .

# Check status (replace JOB_ID with the returned job_id)
curl -s http://localhost:9100/api/v1/jobs/JOB_ID | jq .

# Submit with user identity (runs as uid 1000)
curl -s -X POST http://localhost:9100/api/v1/jobs \
  -H "Content-Type: application/json" \
  -d '{"script":"id && whoami","environment":{},"uid":1000,"gid":1000,"username":"demo"}' | jq .

# Submit with hostfile
curl -s -X POST http://localhost:9100/api/v1/jobs \
  -H "Content-Type: application/json" \
  -d '{"script":"cat /tmp/hostfile","environment":{},"hostfile":"node-0 slots=4\nnode-1 slots=4","hostfile_path":"/tmp/hostfile"}' | jq .

# Terminate a running job
curl -s -X DELETE http://localhost:9100/api/v1/jobs/JOB_ID | jq .
```
