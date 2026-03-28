#!/usr/bin/env bash
# setup.sh — Create a 2-node Kind cluster for the volumes demo.
#
# Topology:
#   control-plane  (no workloads)
#   worker         role=demo   ← all volume demos run here
#
# Creates:
#   - ConfigMap 'nginx-config' with a custom nginx server block
#   - Secret 'app-credentials' with sample credentials
#   - Host directory /opt/reaper-demo/html on the demo worker (for hostPath)
#
# Usage:
#   ./examples/04-volumes/setup.sh              # Create cluster
#   ./examples/04-volumes/setup.sh --cleanup    # Delete cluster
#
# Prerequisites:
#   - Docker running
#   - kind installed (https://kind.sigs.k8s.io/)
#   - helm installed (https://helm.sh/)
#   - Run from the repository root

set -euo pipefail

CLUSTER_NAME="reaper-volumes-demo"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KIND_CONFIG="/tmp/reaper-volumes-kind-config.yaml"
LOG_FILE="/tmp/reaper-volumes-setup.log"

# ---------------------------------------------------------------------------
# Colors (respects NO_COLOR)
# ---------------------------------------------------------------------------
if [[ -n "${NO_COLOR:-}" ]]; then
  B="" G="" Y="" C="" D="" R=""
elif [[ -t 1 ]]; then
  B=$'\033[1m'       # bold
  G=$'\033[1;32m'    # green
  Y=$'\033[1;33m'    # yellow
  C=$'\033[1;36m'    # cyan
  D=$'\033[0;37m'    # dim
  R=$'\033[0m'       # reset
else
  B="" G="" Y="" C="" D="" R=""
fi

info()  { echo "${C}==> ${R}${B}$*${R}"; }
ok()    { echo " ${G}OK${R}  $*"; }
warn()  { echo " ${Y}!!${R}  $*"; }
fail()  { echo " ${Y}ERR${R} $*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Help
# ---------------------------------------------------------------------------
if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  echo "Usage: $0 [OPTIONS]"
  echo ""
  echo "Create a 2-node Kind cluster for the volumes demo."
  echo ""
  echo "Options:"
  echo "  --cleanup    Delete the Kind cluster"
  echo "  -h, --help   Show this help message"
  exit 0
fi

# ---------------------------------------------------------------------------
# Cleanup mode
# ---------------------------------------------------------------------------
if [[ "${1:-}" == "--cleanup" ]]; then
  info "Deleting Kind cluster '$CLUSTER_NAME'..."
  kubectl delete configmap nginx-config --ignore-not-found 2>/dev/null || true
  kubectl delete secret app-credentials --ignore-not-found 2>/dev/null || true
  kind delete cluster --name "$CLUSTER_NAME" 2>/dev/null && ok "Cluster deleted." || warn "Cluster not found."
  exit 0
fi

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------
info "Preflight checks"

command -v docker >/dev/null 2>&1 || fail "docker not found. Install Docker first."
docker info >/dev/null 2>&1       || fail "Docker daemon not running."
command -v kind >/dev/null 2>&1   || fail "kind not found. Install from https://kind.sigs.k8s.io/"
command -v kubectl >/dev/null 2>&1 || fail "kubectl not found."
command -v helm >/dev/null 2>&1   || fail "helm not found. Install from https://helm.sh/"

ok "All prerequisites found."

# ---------------------------------------------------------------------------
# Create Kind config for 2 nodes (1 control-plane + 1 worker)
# ---------------------------------------------------------------------------
info "Writing Kind cluster config"

cat > "$KIND_CONFIG" <<'EOF'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
  - role: worker
EOF

ok "Config written to $KIND_CONFIG"

# ---------------------------------------------------------------------------
# Create cluster and install Reaper via Helm
# ---------------------------------------------------------------------------
info "Setting up cluster '$CLUSTER_NAME' with Reaper (Helm)..."

"$REPO_ROOT/scripts/setup-playground.sh" \
  --cluster-name "$CLUSTER_NAME" \
  --kind-config "$KIND_CONFIG" \
  --quiet \
  || fail "Cluster setup failed. See /tmp/reaper-playground-setup.log"

ok "Cluster created and Reaper installed via Helm."

# Set KUBECONFIG for subsequent kubectl commands
export KUBECONFIG="/tmp/reaper-${CLUSTER_NAME}-kubeconfig"
kind get kubeconfig --name "$CLUSTER_NAME" > "$KUBECONFIG"

# ---------------------------------------------------------------------------
# Apply node labels
# ---------------------------------------------------------------------------
info "Applying node labels"

WORKERS=($(kubectl get nodes --no-headers -o custom-columns=NAME:.metadata.name | grep worker | sort))

if [[ ${#WORKERS[@]} -lt 1 ]]; then
  fail "Expected at least 1 worker node, found ${#WORKERS[@]}"
fi

kubectl label node "${WORKERS[0]}" role=demo --overwrite >> "$LOG_FILE" 2>&1
ok "${WORKERS[0]} labeled role=demo"

DEMO_NODE="${WORKERS[0]}"

# ---------------------------------------------------------------------------
# Create ConfigMap with custom nginx config
# ---------------------------------------------------------------------------
info "Creating ConfigMap 'nginx-config'"

kubectl delete configmap nginx-config --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create configmap nginx-config --from-literal=demo.conf='server {
    listen 8080;
    server_name _;

    location / {
        default_type text/plain;
        return 200 "Welcome to Reaper Volumes Demo!\nHostname: $hostname\nServed by nginx via ConfigMap volume mount.\n";
    }

    location /health {
        default_type text/plain;
        return 200 "ok\n";
    }
}' >> "$LOG_FILE" 2>&1

ok "ConfigMap nginx-config created"

# ---------------------------------------------------------------------------
# Create Secret with sample credentials
# ---------------------------------------------------------------------------
info "Creating Secret 'app-credentials'"

kubectl delete secret app-credentials --ignore-not-found >> "$LOG_FILE" 2>&1
kubectl create secret generic app-credentials \
  --from-literal=username=demo-user \
  --from-literal=password=s3cret-p4ssw0rd \
  --from-literal=api-key=rpr-ak-7f3d9e2b1a4c6d8f >> "$LOG_FILE" 2>&1

ok "Secret app-credentials created"

# ---------------------------------------------------------------------------
# Create host directory for hostPath demo
# ---------------------------------------------------------------------------
info "Creating host directory on $DEMO_NODE for hostPath demo"

docker exec "$DEMO_NODE" mkdir -p /opt/reaper-demo/html
docker exec "$DEMO_NODE" sh -c 'cat > /opt/reaper-demo/html/index.html <<INNEREOF
<!DOCTYPE html>
<html>
<head><title>Reaper hostPath Demo</title></head>
<body>
<h1>Hello from the host filesystem!</h1>
<p>This file lives on the node at /opt/reaper-demo/html/index.html</p>
<p>It was mounted into the pod via a hostPath volume.</p>
</body>
</html>
INNEREOF'

ok "Host directory /opt/reaper-demo/html created on $DEMO_NODE"

# ---------------------------------------------------------------------------
# Verify RuntimeClass
# ---------------------------------------------------------------------------
info "Verifying RuntimeClass"

for i in $(seq 1 15); do
  if kubectl get runtimeclass reaper-v2 &>/dev/null; then
    ok "RuntimeClass reaper-v2 available."
    break
  fi
  sleep 1
done

kubectl get runtimeclass reaper-v2 &>/dev/null || fail "RuntimeClass reaper-v2 not found"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "${C}========================================${R}"
echo "${B}Cluster ready: $CLUSTER_NAME${R}"
echo "${C}========================================${R}"
echo ""

echo "${B}Reaper: installed via Helm${R}"

echo "${B}Nodes:${R}"
kubectl get nodes -o custom-columns=\
'NAME:.metadata.name,STATUS:.status.conditions[-1].type,ROLE:.metadata.labels.role,IP:.status.addresses[0].address' \
  --no-headers 2>/dev/null | while IFS= read -r line; do
  echo "  $line"
done

echo ""
echo "${B}Resources:${R}"
echo "  ConfigMap: nginx-config  (custom nginx server block on port 8080)"
echo "  Secret:    app-credentials  (username, password, api-key)"
echo "  hostPath:  /opt/reaper-demo/html on $DEMO_NODE"

echo ""
echo "${B}Connect:${R}"
echo "  export KUBECONFIG=/tmp/reaper-${CLUSTER_NAME}-kubeconfig"

echo ""
echo "${C}────────────────────────────────────────${R}"
echo ""
echo "${Y}NOTE:${R} Reaper workloads share a single overlay namespace per node."
echo "      Delete each demo pod before running the next to avoid port"
echo "      conflicts and config bleed between pods."
echo ""
echo "Run the demos (one at a time):"
echo ""
echo "  ${B}# 1. ConfigMap — nginx with custom config${R}"
echo "  kubectl apply -f examples/04-volumes/configmap-nginx.yaml"
echo "  kubectl logs configmap-nginx -f"
echo "  kubectl delete pod configmap-nginx"
echo ""
echo "  ${B}# 2. Secret — read-only credentials${R}"
echo "  kubectl apply -f examples/04-volumes/secret-env.yaml"
echo "  kubectl logs secret-reader -f"
echo "  kubectl delete pod secret-reader"
echo ""
echo "  ${B}# 3. hostPath — serve files from host${R}"
echo "  kubectl apply -f examples/04-volumes/hostpath-logs.yaml"
echo "  kubectl logs hostpath-reader -f"
echo "  kubectl delete pod hostpath-reader"
echo ""
echo "  ${B}# 4. emptyDir — ephemeral workspace${R}"
echo "  kubectl apply -f examples/04-volumes/emptydir-workspace.yaml"
echo "  kubectl logs emptydir-worker -f"
echo "  kubectl delete pod emptydir-worker"
echo ""
echo "  ${B}# Clean up everything${R}"
echo "  kubectl delete -f examples/04-volumes/"
echo "  ./examples/04-volumes/setup.sh --cleanup"
echo ""
echo "Log file: $LOG_FILE"
