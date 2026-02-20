#!/usr/bin/env bash
# release-utils.sh — GitHub release resolution helpers.
# Sourced by setup-playground.sh and test scripts; do not execute directly.

GITHUB_REPO="${GITHUB_REPO:-miguelgila/reaper}"

# resolve_latest_release — Print the tag name of the latest GitHub release.
#
# Tries `gh` CLI first (handles auth and rate limits), then falls back to
# the unauthenticated GitHub REST API via `curl` (60 requests/hour per IP).
#
# Outputs the tag (e.g., "v0.2.4") on stdout.
# Returns non-zero and prints an error on stderr if resolution fails.
resolve_latest_release() {
  local latest=""

  # Path 1: gh CLI (if available)
  if command -v gh >/dev/null 2>&1; then
    latest=$(gh release view --repo "$GITHUB_REPO" --json tagName -q '.tagName' 2>/dev/null) || true
  fi

  # Path 2: curl fallback (unauthenticated GitHub API)
  if [[ -z "${latest:-}" ]]; then
    latest=$(curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" 2>/dev/null \
      | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/') || true
  fi

  if [[ -z "${latest:-}" ]]; then
    echo "ERROR: Could not determine latest release for $GITHUB_REPO" >&2
    return 1
  fi

  echo "$latest"
}
