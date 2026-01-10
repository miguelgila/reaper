#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo ".")"
cd "$REPO_ROOT"

git config core.hooksPath .githooks

echo "Configured git hooks path to .githooks."
echo "Make sure the hook file is executable: chmod +x .githooks/pre-commit"
