#!/usr/bin/env bash
set -euo pipefail

echo "Ensuring RuntimeClass exists..."
kubectl apply -f k8s/runtimeclass.yaml

echo "Deploying dummy pod using RuntimeClass 'reaper'..."
kubectl apply -f k8s/pod-reaper.yaml

echo "Waiting for pod to complete..."
kubectl wait --for=condition=Succeeded --timeout=120s pod/reaper-dummy || {
  echo "Pod did not succeed; showing logs:";
  kubectl logs pod/reaper-dummy || true;
  exit 1;
}

echo "Fetching pod logs:"
kubectl logs pod/reaper-dummy || true

echo "Test complete."
