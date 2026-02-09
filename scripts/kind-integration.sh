#!/usr/bin/env bash
# DEPRECATED: This script has been replaced by run-integration-tests.sh
# It provides structured test output, GitHub Actions integration, and bug fixes.
#
# Usage:
#   ./scripts/run-integration-tests.sh                  # Full run
#   ./scripts/run-integration-tests.sh --skip-cargo     # K8s-only
#   ./scripts/run-integration-tests.sh --no-cleanup     # Keep cluster
#
# This wrapper will be removed in a future release.
set -euo pipefail
echo "WARNING: kind-integration.sh is deprecated. Use run-integration-tests.sh instead." >&2
exec "$(dirname "$0")/run-integration-tests.sh" "$@"
