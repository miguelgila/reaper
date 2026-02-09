#!/usr/bin/env bash
# DEPRECATED: This script has been replaced by run-integration-tests.sh
# Minikube support is deprecated in favor of kind-based testing.
#
# Usage:
#   ./scripts/run-integration-tests.sh
#
# This wrapper will be removed in a future release.
set -euo pipefail
echo "WARNING: minikube-setup-runtime.sh is deprecated. Use run-integration-tests.sh instead." >&2
exec "$(dirname "$0")/run-integration-tests.sh" "$@"
