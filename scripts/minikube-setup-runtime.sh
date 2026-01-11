#!/usr/bin/env bash
set -euo pipefail

# This script configures minikube (containerd) to use containerd-shim-reaper-v2.
# Requirements: minikube, kubectl.

SHIM_BIN="containerd-shim-reaper-v2"
RUNTIME_BIN="reaper-runtime"
REAPER_ROOT="/run/reaper"

echo "Starting minikube with containerd..."
minikube start --container-runtime=containerd --driver=docker

echo "Detecting minikube node architecture..."
NODE_ARCH="$(minikube ssh -- uname -m | tr -d '\r')"
echo "Node arch: $NODE_ARCH"

echo "Building static (musl) Linux binaries inside Docker..."
# Build statically linked musl binaries to avoid glibc mismatch.
case "$NODE_ARCH" in
	aarch64)
		TARGET_TRIPLE="aarch64-unknown-linux-musl"
		MUSL_IMAGE="messense/rust-musl-cross:aarch64-musl"
		;;
	x86_64)
		TARGET_TRIPLE="x86_64-unknown-linux-musl"
		MUSL_IMAGE="messense/rust-musl-cross:x86_64-musl"
		;;
	*)
		echo "Unsupported node arch: $NODE_ARCH" >&2
		exit 1
		;;
esac

# Build both binaries in one docker run
docker run --rm \
	-v "$(pwd)":/work \
	-w /work \
	"$MUSL_IMAGE" \
	cargo build --release --bin "$SHIM_BIN" --bin "$RUNTIME_BIN" --target "$TARGET_TRIPLE"

SHIM_BIN_PATH="$(pwd)/target/$TARGET_TRIPLE/release/$SHIM_BIN"
RUNTIME_BIN_PATH="$(pwd)/target/$TARGET_TRIPLE/release/$RUNTIME_BIN"

echo "Copy shim binary into minikube node..."
minikube cp "$SHIM_BIN_PATH" "/usr/local/bin/$SHIM_BIN"
minikube ssh -- "sudo chmod +x /usr/local/bin/$SHIM_BIN"

echo "Copy runtime binary into minikube node..."
minikube cp "$RUNTIME_BIN_PATH" "/usr/local/bin/$RUNTIME_BIN"
minikube ssh -- "sudo chmod +x /usr/local/bin/$RUNTIME_BIN"

echo "Enabling shim and runtime debug logging..."
minikube ssh -- "sudo bash -c '
    mkdir -p /etc/systemd/system/containerd.service.d
    cat > /etc/systemd/system/containerd.service.d/reaper-shim-logging.conf <<EOF
[Service]
Environment=\"REAPER_SHIM_LOG=/var/log/reaper-shim.log\"
Environment=\"REAPER_RUNTIME_LOG=/var/log/reaper-runtime.log\"
EOF
    systemctl daemon-reload
'"

echo "Configuring containerd to use reaper-v2 shim runtime..."
./scripts/configure-containerd.sh minikube

echo "Verifying containerd config..."
minikube ssh -- "sudo cat /etc/containerd/config.toml | grep -A 5 'reaper-v2'"

echo "Shim logs will be written to /var/log/reaper-shim.log"
echo "Runtime logs will be written to /var/log/reaper-runtime.log"

echo "Creating RuntimeClass 'reaper-v2' and example pod..."
kubectl apply -f kubernetes/runtimeclass.yaml

echo "Minikube runtime setup complete."
echo "Both binaries deployed:"
echo "  - Shim: /usr/local/bin/$SHIM_BIN"
echo "  - Runtime: /usr/local/bin/$RUNTIME_BIN"
