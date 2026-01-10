#!/usr/bin/env bash
set -euo pipefail

# This script configures minikube (containerd) to use reaper-runtime.
# Requirements: minikube, kubectl.

RUNTIME_BIN="reaper-runtime"
REAPER_ROOT="/run/reaper"

echo "Starting minikube with containerd..."
minikube start --container-runtime=containerd --driver=docker

echo "Building runtime binary..."
cargo build --release --bin "$RUNTIME_BIN"

BIN_PATH="$(pwd)/target/release/$RUNTIME_BIN"

echo "Copy runtime binary into minikube node..."
minikube cp "$BIN_PATH" "/usr/local/bin/$RUNTIME_BIN"

echo "Patching containerd config to register 'reaper' runtime..."
minikube ssh -- "sudo sed -i '/\[plugins\."io.containerd.grpc.v1.cri"\.containerd\]/a \
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper]\n  runtime_type = "io.containerd.runc.v2"\n  [plugins."io.containerd.grpc.v1.cri".containerd.runtimes.reaper.options]\n    BinaryName = \"/usr/local/bin/reaper-runtime\"' /etc/containerd/config.toml"

echo "Restarting containerd inside minikube..."
minikube ssh -- "sudo systemctl restart containerd || sudo pkill -HUP containerd"

echo "Creating RuntimeClass 'reaper'..."
kubectl apply -f k8s/runtimeclass.yaml

echo "Minikube runtime setup complete."
