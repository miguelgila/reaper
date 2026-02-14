#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo ".")"
cd "$REPO_ROOT"

git config core.hooksPath .githooks
chmod +x .githooks/pre-commit .githooks/pre-push

echo "Configured git hooks path to .githooks."
echo "Hooks enabled:"
echo "  pre-commit  — runs cargo fmt and stages changes"
echo "  pre-push    — runs cargo clippy -- -D warnings (matches CI)"
