#!/usr/bin/env bash
set -euo pipefail

# This script configures minikube (containerd) to use reaper-runtime.
# Requirements: minikube, kubectl.

RUNTIME_BIN="reaper-runtime"
REAPER_ROOT="/run/reaper"

echo "Starting minikube with containerd..."
minikube start --container-runtime=containerd --driver=docker

echo "Detecting minikube node architecture..."
NODE_ARCH="$(minikube ssh -- uname -m | tr -d '\r')"
echo "Node arch: $NODE_ARCH"

echo "Building static (musl) Linux runtime binary inside Docker..."
# Build a statically linked musl binary to avoid glibc mismatch.
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

docker run --rm \
	-v "$(pwd)":/work \
	-w /work \
	"$MUSL_IMAGE" \
	cargo build --release --bin "$RUNTIME_BIN" --target "$TARGET_TRIPLE"

BIN_PATH="$(pwd)/target/$TARGET_TRIPLE/release/$RUNTIME_BIN"

echo "Copy runtime binary into minikube node..."
minikube cp "$BIN_PATH" "/usr/local/bin/$RUNTIME_BIN"

echo "Patching containerd config to register 'reaper' runtime..."
minikube ssh -- "bash -c 'if ! grep -q runtimes.reaper /etc/containerd/config.toml; then \
	echo -e "[plugins.\\\"io.containerd.grpc.v1.cri\\\".containerd.runtimes.reaper]\\n  runtime_type = \\\"io.containerd.runc.v2\\\"\\n  [plugins.\\\"io.containerd.grpc.v1.cri\\\".containerd.runtimes.reaper.options]\\n    BinaryName = \\\"/usr/local/bin/reaper-runtime\\\"\\n" | sudo tee -a /etc/containerd/config.toml >/dev/null; \
	echo Added reaper runtime to containerd config; \
else \
	echo reaper runtime already configured; \
fi'"

echo "Restarting containerd inside minikube..."
minikube ssh -- "sudo systemctl restart containerd || sudo pkill -HUP containerd"

echo "Creating RuntimeClass 'reaper'..."
kubectl apply -f k8s/runtimeclass.yaml

echo "Minikube runtime setup complete."
