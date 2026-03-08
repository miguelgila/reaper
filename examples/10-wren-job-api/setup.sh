#!/usr/bin/env bash
# 10-wren-job-api: Demonstrates the Reaper agent's HTTP job execution API.
set -euo pipefail

CLUSTER_NAME="reaper-job-api-demo"
KUBECONFIG_PATH="/tmp/reaper-${CLUSTER_NAME}-kubeconfig"
LOG_FILE="/tmp/reaper-${CLUSTER_NAME}-setup.log"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# ---------------------------------------------------------------------------
# Colors (respect NO_COLOR)
# ---------------------------------------------------------------------------
if [[ -t 1 && "${NO_COLOR:-}" == "" ]]; then
  C_GREEN="\033[0;32m"; C_YELLOW="\033[0;33m"; C_CYAN="\033[0;36m"
  C_RED="\033[0;31m"; C_BOLD="\033[1m"; C_RESET="\033[0m"
else
  C_GREEN=""; C_YELLOW=""; C_CYAN=""; C_RED=""; C_BOLD=""; C_RESET=""
fi

info()  { printf "${C_CYAN}[INFO]${C_RESET}  %s\n" "$*"; }
ok()    { printf "${C_GREEN}[OK]${C_RESET}    %s\n" "$*"; }
warn()  { printf "${C_YELLOW}[WARN]${C_RESET}  %s\n" "$*"; }
fail()  { printf "${C_RED}[FAIL]${C_RESET}  %s\n" "$*"; exit 1; }

# ---------------------------------------------------------------------------
# Cleanup mode
# ---------------------------------------------------------------------------
if [[ "${1:-}" == "--cleanup" ]]; then
  info "Deleting Kind cluster '${CLUSTER_NAME}'..."
  kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null || true
  rm -f "$KUBECONFIG_PATH"
  ok "Cleanup complete."
  exit 0
fi

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------
for cmd in docker kind kubectl curl jq; do
  command -v "$cmd" >/dev/null || fail "Required tool '$cmd' not found"
done

# ---------------------------------------------------------------------------
# Create Kind cluster
# ---------------------------------------------------------------------------
if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
  info "Cluster '${CLUSTER_NAME}' already exists, reusing."
else
  info "Creating Kind cluster '${CLUSTER_NAME}'..."
  kind create cluster --name "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1
  ok "Cluster created."
fi

kind get kubeconfig --name "$CLUSTER_NAME" > "$KUBECONFIG_PATH"
export KUBECONFIG="$KUBECONFIG_PATH"

# ---------------------------------------------------------------------------
# Install Reaper + Agent
# ---------------------------------------------------------------------------
info "Setting up Reaper playground..."
"$PROJECT_ROOT/scripts/setup-playground.sh" --cluster-name "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1
ok "Reaper runtime installed."

info "Building and loading reaper-agent image..."
"$PROJECT_ROOT/scripts/build-agent-image.sh" --cluster-name "$CLUSTER_NAME" >> "$LOG_FILE" 2>&1
ok "Agent image loaded."

info "Deploying reaper-agent DaemonSet..."
kubectl apply -f "$PROJECT_ROOT/deploy/kubernetes/reaper-agent.yaml" >> "$LOG_FILE" 2>&1
kubectl rollout status daemonset/reaper-agent -n reaper-system --timeout=120s >> "$LOG_FILE" 2>&1
ok "Agent running."

# ---------------------------------------------------------------------------
# Port-forward to the agent
# ---------------------------------------------------------------------------
AGENT_POD=$(kubectl get pods -n reaper-system -l app.kubernetes.io/name=reaper-agent \
  -o jsonpath='{.items[0].metadata.name}')
LOCAL_PORT=9100
kubectl port-forward -n reaper-system "$AGENT_POD" ${LOCAL_PORT}:9100 >> "$LOG_FILE" 2>&1 &
PF_PID=$!
trap "kill $PF_PID 2>/dev/null || true" EXIT

# Wait for port-forward
for i in $(seq 1 15); do
  curl -sf http://localhost:${LOCAL_PORT}/healthz > /dev/null 2>&1 && break
  sleep 1
done
curl -sf http://localhost:${LOCAL_PORT}/healthz > /dev/null 2>&1 || fail "Port-forward not ready"
ok "Port-forward to agent ready (localhost:${LOCAL_PORT})"

echo ""
printf "${C_BOLD}=== Reaper Job API Demo ===${C_RESET}\n"
echo ""

# ---------------------------------------------------------------------------
# 1. Submit a simple job
# ---------------------------------------------------------------------------
info "1. Submitting a simple job..."
RESP=$(curl -s -X POST http://localhost:${LOCAL_PORT}/api/v1/jobs \
  -H "Content-Type: application/json" \
  -d '{"script":"echo hello from reaper && hostname && date","environment":{}}')
JOB_ID=$(echo "$RESP" | jq -r '.job_id')
echo "   Response: $RESP"
ok "Job submitted: $JOB_ID"
echo ""

# ---------------------------------------------------------------------------
# 2. Poll job status
# ---------------------------------------------------------------------------
info "2. Polling job status..."
sleep 2
STATUS=$(curl -s http://localhost:${LOCAL_PORT}/api/v1/jobs/${JOB_ID})
echo "   Status: $STATUS"
ok "Job status retrieved."
echo ""

# ---------------------------------------------------------------------------
# 3. Submit a job with environment variables
# ---------------------------------------------------------------------------
info "3. Submitting job with environment variables..."
RESP=$(curl -s -X POST http://localhost:${LOCAL_PORT}/api/v1/jobs \
  -H "Content-Type: application/json" \
  -d '{"script":"echo GREETING=$GREETING SCRATCH=$SCRATCH","environment":{"GREETING":"hello-wren","SCRATCH":"/scratch/project"}}')
JOB_ID2=$(echo "$RESP" | jq -r '.job_id')
sleep 2
STATUS=$(curl -s http://localhost:${LOCAL_PORT}/api/v1/jobs/${JOB_ID2})
echo "   Status: $STATUS"
ok "Environment variables passed to job."
echo ""

# ---------------------------------------------------------------------------
# 4. Submit a job with working directory
# ---------------------------------------------------------------------------
info "4. Submitting job with working directory /tmp..."
RESP=$(curl -s -X POST http://localhost:${LOCAL_PORT}/api/v1/jobs \
  -H "Content-Type: application/json" \
  -d '{"script":"pwd","environment":{},"working_dir":"/tmp"}')
JOB_ID3=$(echo "$RESP" | jq -r '.job_id')
sleep 2
STATUS=$(curl -s http://localhost:${LOCAL_PORT}/api/v1/jobs/${JOB_ID3})
echo "   Status: $STATUS"
ok "Working directory set."
echo ""

# ---------------------------------------------------------------------------
# 5. Submit a job with hostfile
# ---------------------------------------------------------------------------
info "5. Submitting job with MPI hostfile..."
RESP=$(curl -s -X POST http://localhost:${LOCAL_PORT}/api/v1/jobs \
  -H "Content-Type: application/json" \
  -d '{"script":"cat /tmp/demo-hostfile && echo ---hostfile-ok","environment":{},"hostfile":"node-0 slots=4\nnode-1 slots=4","hostfile_path":"/tmp/demo-hostfile"}')
JOB_ID4=$(echo "$RESP" | jq -r '.job_id')
sleep 2
STATUS=$(curl -s http://localhost:${LOCAL_PORT}/api/v1/jobs/${JOB_ID4})
echo "   Status: $STATUS"
ok "Hostfile written and read by job."
echo ""

# ---------------------------------------------------------------------------
# 6. Terminate a running job
# ---------------------------------------------------------------------------
info "6. Submitting long-running job, then terminating..."
RESP=$(curl -s -X POST http://localhost:${LOCAL_PORT}/api/v1/jobs \
  -H "Content-Type: application/json" \
  -d '{"script":"sleep 300","environment":{}}')
JOB_ID5=$(echo "$RESP" | jq -r '.job_id')
sleep 1
echo "   Terminating job $JOB_ID5..."
DELETE_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE \
  http://localhost:${LOCAL_PORT}/api/v1/jobs/${JOB_ID5})
echo "   DELETE response: HTTP $DELETE_CODE"
sleep 1
STATUS=$(curl -s http://localhost:${LOCAL_PORT}/api/v1/jobs/${JOB_ID5})
echo "   Final status: $STATUS"
ok "Job terminated."
echo ""

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
printf "${C_BOLD}=== Demo Complete ===${C_RESET}\n"
echo ""
echo "The Reaper agent's job API supports:"
echo "  - POST   /api/v1/jobs      Submit a job (script, env vars, working dir, hostfile)"
echo "  - GET    /api/v1/jobs/{id}  Poll status (running, succeeded, failed)"
echo "  - DELETE /api/v1/jobs/{id}  Terminate a running job"
echo ""
echo "Cleanup: $0 --cleanup"
