#!/bin/sh
# install-node.sh — Init container script for the reaper-node DaemonSet.
# Copies Reaper binaries to the host, creates config, and optionally
# configures containerd.
#
# Environment variables (set by the Helm chart):
#   INSTALL_PATH          — Host binary path (default: /usr/local/bin)
#   CONFIGURE_CONTAINERD  — "true" to patch containerd config and restart
#   REAPER_DNS_MODE       — DNS mode for reaper.conf (default: kubernetes)
#   REAPER_RUNTIME_LOG    — Runtime log path (default: /run/reaper/runtime.log)

set -e

INSTALL_PATH="${INSTALL_PATH:-/usr/local/bin}"
CONFIGURE_CONTAINERD="${CONFIGURE_CONTAINERD:-false}"
REAPER_DNS_MODE="${REAPER_DNS_MODE:-kubernetes}"
REAPER_RUNTIME_LOG="${REAPER_RUNTIME_LOG:-/run/reaper/runtime.log}"

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
  x86_64)  BINARCH="amd64" ;;
  aarch64) BINARCH="arm64" ;;
  *)
    echo "ERROR: unsupported architecture: $ARCH"
    exit 1
    ;;
esac

echo "==> Installing Reaper binaries ($BINARCH) to /host${INSTALL_PATH}"

# Copy binaries to host
cp "/binaries/${BINARCH}/containerd-shim-reaper-v2" "/host${INSTALL_PATH}/containerd-shim-reaper-v2"
cp "/binaries/${BINARCH}/reaper-runtime" "/host${INSTALL_PATH}/reaper-runtime"
chmod 755 "/host${INSTALL_PATH}/containerd-shim-reaper-v2"
chmod 755 "/host${INSTALL_PATH}/reaper-runtime"

echo " OK  Binaries installed."

# Create Reaper directories
mkdir -p /host/run/reaper
mkdir -p /host/etc/reaper

# Write configuration file
cat > /host/etc/reaper/reaper.conf <<EOF
# Reaper runtime configuration (managed by Helm)
REAPER_DNS_MODE=${REAPER_DNS_MODE}
REAPER_RUNTIME_LOG=${REAPER_RUNTIME_LOG}
EOF

echo " OK  Configuration written to /etc/reaper/reaper.conf"

# Optionally configure containerd
if [ "$CONFIGURE_CONTAINERD" = "true" ]; then
  CONTAINERD_CONFIG="/host/etc/containerd/config.toml"

  if [ ! -f "$CONTAINERD_CONFIG" ]; then
    echo "WARN: containerd config not found at $CONTAINERD_CONFIG, creating minimal config"
    mkdir -p /host/etc/containerd
    cat > "$CONTAINERD_CONFIG" <<'TOML'
version = 2
[plugins]
  [plugins."io.containerd.grpc.v1.cri"]
    [plugins."io.containerd.grpc.v1.cri".containerd]
      [plugins."io.containerd.grpc.v1.cri".containerd.runtimes]
TOML
  fi

  if grep -q 'runtimes.reaper-v2' "$CONTAINERD_CONFIG" 2>/dev/null; then
    echo " OK  Reaper runtime already configured in containerd."
  else
    echo "==> Adding Reaper runtime to containerd config"
    cat >> "$CONTAINERD_CONFIG" <<'TOML'

        [plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper-v2]
          runtime_type = "io.containerd.reaper.v2"
          sandbox_mode = "podsandbox"
          pod_annotations = ["reaper.runtime/*"]
TOML

    echo " OK  Containerd config updated."

    # Restart containerd via nsenter into host PID namespace
    echo "==> Restarting containerd"
    nsenter --target 1 --mount --uts --ipc --pid -- systemctl restart containerd
    echo " OK  Containerd restarted."
  fi
fi

echo "==> Reaper node installation complete."
