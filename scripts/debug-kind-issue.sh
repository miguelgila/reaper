#!/usr/bin/env bash
set -euo pipefail

CLUSTER_NAME="${1:-reaper-ci}"

echo "🔍 Debugging Kind cluster: $CLUSTER_NAME"
echo "=========================================="

# Get the node ID
NODE_ID=$(docker ps --filter "name=${CLUSTER_NAME}-control-plane" --format '{{.ID}}')
if [ -z "$NODE_ID" ]; then
  echo "❌ Could not find kind node with name ${CLUSTER_NAME}-control-plane"
  exit 1
fi

echo "Node ID: $NODE_ID"
echo ""

# Check containerd config syntax
echo "✅ Checking containerd config syntax..."
docker exec "$NODE_ID" bash -c "
  echo 'Checking if config exists...'
  if [ ! -f /etc/containerd/config.toml ]; then
    echo '❌ /etc/containerd/config.toml does not exist'
    exit 1
  fi

  echo 'Config file size:' \$(wc -c < /etc/containerd/config.toml) 'bytes'
  echo ''
  echo 'Reaper-v2 section in config:'
  grep -A 2 'reaper-v2' /etc/containerd/config.toml || echo '❌ No reaper-v2 found'
  echo ''
  echo 'Runc section in config (should come after reaper-v2):'
  grep -A 1 '\[plugins.*runtimes.runc\]' /etc/containerd/config.toml || echo '⚠️  Pattern not found'
" || exit 1

echo ""

# Check containerd daemon status
echo "✅ Checking containerd daemon status..."
docker exec "$NODE_ID" bash -c "
  if systemctl is-active --quiet containerd; then
    echo 'containerd is running ✅'
    systemctl status containerd | head -5
  else
    echo 'containerd is NOT running ❌'
    systemctl status containerd || true
  fi
" || exit 1

echo ""

# Check if shim binary exists and is executable
echo "✅ Checking shim binary..."
docker exec "$NODE_ID" bash -c "
  SHIM=/usr/local/bin/containerd-shim-reaper-v2
  if [ ! -f \$SHIM ]; then
    echo '❌ Shim binary not found at' \$SHIM
    exit 1
  fi

  echo 'Shim binary exists: ✅'
  echo 'Shim info:'
  file \$SHIM
  ls -lh \$SHIM

  echo ''
  echo 'Testing if shim is executable by trying to run it...'
  \$SHIM --version 2>&1 || echo '(shim returned non-zero, may be normal)'
" || exit 1

echo ""

# Check for zombie processes
echo "✅ Checking for zombie processes..."
docker exec "$NODE_ID" bash -c "
  ZOMBIES=\$(ps aux | grep -c 'defunct' || true)
  if [ \$ZOMBIES -gt 1 ]; then
    echo '⚠️  Found' \$ZOMBIES 'zombie processes:'
    ps aux | grep defunct
  else
    echo 'No zombie processes found ✅'
  fi
" || exit 1

echo ""

# Check containerd logs
echo "✅ Checking containerd logs (last 50 lines)..."
docker exec "$NODE_ID" bash -c "
  if [ -f /var/log/containerd.log ]; then
    echo '=== /var/log/containerd.log ==='
    tail -50 /var/log/containerd.log
  else
    echo '⚠️  /var/log/containerd.log not found'
  fi
" || exit 1

echo ""

# Check journalctl for containerd errors
echo "✅ Checking journalctl for containerd errors..."
docker exec "$NODE_ID" bash -c "
  echo '=== Recent containerd entries (last 30 lines) ==='
  journalctl -u containerd -n 30 --no-pager || echo 'journalctl not available'
" || exit 1

echo ""

# Check kubelet logs
echo "✅ Checking kubelet logs for runtime errors..."
docker exec "$NODE_ID" bash -c "
  echo '=== Kubelet logs with containerd mentions (last 20 lines) ==='
  journalctl -u kubelet -n 50 --no-pager 2>/dev/null | grep -i 'containerd\|runtime\|reaper' | tail -20 || echo 'No relevant entries found'
" || exit 1

echo ""

# Check API server status
echo "✅ Checking API server status..."
docker exec "$NODE_ID" bash -c "
  if docker ps | grep -q 'kube-apiserver'; then
    echo 'API server container is running ✅'
    docker logs kube-apiserver 2>&1 | tail -20
  else
    echo '⚠️  API server container not running'
  fi
" || exit 1

echo ""
echo "=========================================="
echo "Diagnostic report complete!"
echo ""
echo "💡 Next steps:"
echo "1. Look for errors mentioning 'reaper-v2' or 'containerd-shim-reaper-v2'"
echo "2. Check if the containerd config reaper-v2 section is properly formatted"
echo "3. If shim tests fail, check if the binary works on this architecture"
echo ""
