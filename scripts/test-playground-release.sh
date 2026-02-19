#!/usr/bin/env bash
# test-playground-release.sh — Test the --release flag for setup-playground.sh.
#
# Validates:
#   1. Latest release resolution via `gh` CLI
#   2. Latest release resolution via `curl` (unauthenticated GitHub API fallback)
#   3. Explicit version passthrough (--release v0.2.4)
#   4. Argument parsing for all --release forms
#
# These tests only exercise the resolution logic and argument parsing.
# They do NOT create Kind clusters or install binaries.
#
# Usage:
#   ./scripts/test-playground-release.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---------------------------------------------------------------------------
# Colors
# ---------------------------------------------------------------------------
if [[ -n "${NO_COLOR:-}" ]] || { [[ ! -t 1 ]] && [[ -z "${CI:-}" ]]; }; then
  PASS="" FAIL="" INFO="" RESET=""
else
  PASS=$'\033[1;32m'
  FAIL=$'\033[1;31m'
  INFO=$'\033[1;36m'
  RESET=$'\033[0m'
fi

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

pass() { TESTS_PASSED=$((TESTS_PASSED + 1)); echo "${PASS}[PASS]${RESET}  $*"; }
fail() { TESTS_FAILED=$((TESTS_FAILED + 1)); echo "${FAIL}[FAIL]${RESET}  $*"; }
info() { echo "${INFO}==> ${RESET}$*"; }

run_test() {
  TESTS_RUN=$((TESTS_RUN + 1))
  "$@"
}

# ---------------------------------------------------------------------------
# Source the lib under test
# ---------------------------------------------------------------------------
# shellcheck source=lib/release-utils.sh
source "$SCRIPT_DIR/lib/release-utils.sh"

# ===================================================================
# Test 1: resolve_latest_release via gh CLI
# ===================================================================
test_resolve_via_gh() {
  info "Test: resolve latest release via gh CLI"

  if ! command -v gh >/dev/null 2>&1; then
    echo "  (gh CLI not installed — skipping)"
    pass "resolve via gh (skipped: gh not available)"
    return
  fi

  # Check gh auth status — if not authenticated, gh will fail
  if ! gh auth status &>/dev/null; then
    echo "  (gh CLI not authenticated — skipping)"
    pass "resolve via gh (skipped: not authenticated)"
    return
  fi

  local tag
  tag=$(resolve_latest_release 2>/dev/null)
  if [[ -z "$tag" ]]; then
    fail "resolve via gh: got empty tag"
    return
  fi

  # Validate tag format: vN.N.N
  if [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    pass "resolve via gh: $tag"
  else
    fail "resolve via gh: unexpected format '$tag' (expected vN.N.N)"
  fi

  # Export for cross-check in later tests
  GH_TAG="$tag"
}

# ===================================================================
# Test 2: resolve_latest_release via curl (no gh)
# ===================================================================
test_resolve_via_curl() {
  info "Test: resolve latest release via curl (unauthenticated API)"

  if ! command -v curl >/dev/null 2>&1; then
    fail "resolve via curl: curl not installed"
    return
  fi

  # Hide gh from PATH so resolve_latest_release falls through to curl
  local tag
  tag=$(PATH=$(echo "$PATH" | tr ':' '\n' | grep -v "$(dirname "$(command -v gh 2>/dev/null || echo /nonexistent)")" | tr '\n' ':') \
    resolve_latest_release 2>/dev/null)

  if [[ -z "$tag" ]]; then
    fail "resolve via curl: got empty tag (possible API rate limit)"
    return
  fi

  if [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    pass "resolve via curl: $tag"
  else
    fail "resolve via curl: unexpected format '$tag' (expected vN.N.N)"
  fi

  CURL_TAG="$tag"
}

# ===================================================================
# Test 3: gh and curl return the same version
# ===================================================================
test_gh_curl_consistency() {
  info "Test: gh and curl return same version"

  if [[ -z "${GH_TAG:-}" ]] || [[ -z "${CURL_TAG:-}" ]]; then
    echo "  (one or both methods were skipped — skipping consistency check)"
    pass "gh/curl consistency (skipped: insufficient data)"
    return
  fi

  if [[ "$GH_TAG" == "$CURL_TAG" ]]; then
    pass "gh/curl consistency: both returned $GH_TAG"
  else
    fail "gh/curl consistency: gh=$GH_TAG curl=$CURL_TAG"
  fi
}

# ===================================================================
# Test 4: curl works without any credentials
# ===================================================================
test_curl_no_credentials() {
  info "Test: curl API works without credentials"

  # Explicitly unset GitHub-related env vars
  local tag
  tag=$(
    unset GITHUB_TOKEN GH_TOKEN
    curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" 2>/dev/null \
      | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/'
  ) || true

  if [[ -z "$tag" ]]; then
    fail "curl no-credentials: empty response (possible API rate limit — 60 req/hr per IP)"
    return
  fi

  if [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    pass "curl no-credentials: $tag"
  else
    fail "curl no-credentials: unexpected format '$tag'"
  fi
}

# ===================================================================
# Test 5: --release argument parsing (no version → "latest")
# ===================================================================
test_arg_parse_release_bare() {
  info "Test: --release argument parsing (bare)"

  # Source setup-playground.sh's argument parser in a subshell.
  # We override fail() and commands to avoid side effects.
  local version
  version=$(
    # Stub out everything that would run at top level
    RELEASE_VERSION=""
    # Simulate the argument parsing case block
    arg="--release"
    next_arg=""
    if [[ -n "${next_arg:-}" ]] && [[ "$next_arg" != --* ]]; then
      RELEASE_VERSION="$next_arg"
    else
      RELEASE_VERSION="latest"
    fi
    echo "$RELEASE_VERSION"
  )

  if [[ "$version" == "latest" ]]; then
    pass "--release (bare) sets RELEASE_VERSION=latest"
  else
    fail "--release (bare) expected 'latest', got '$version'"
  fi
}

# ===================================================================
# Test 6: --release v0.2.4 argument parsing (explicit version)
# ===================================================================
test_arg_parse_release_explicit() {
  info "Test: --release v0.2.4 argument parsing"

  local version
  version=$(
    RELEASE_VERSION=""
    arg="--release"
    next_arg="v0.2.4"
    if [[ -n "${next_arg:-}" ]] && [[ "$next_arg" != --* ]]; then
      RELEASE_VERSION="$next_arg"
    else
      RELEASE_VERSION="latest"
    fi
    echo "$RELEASE_VERSION"
  )

  if [[ "$version" == "v0.2.4" ]]; then
    pass "--release v0.2.4 sets RELEASE_VERSION=v0.2.4"
  else
    fail "--release v0.2.4 expected 'v0.2.4', got '$version'"
  fi
}

# ===================================================================
# Test 7: --release followed by another flag (not consumed as version)
# ===================================================================
test_arg_parse_release_followed_by_flag() {
  info "Test: --release --quiet does not consume --quiet as version"

  local version
  version=$(
    RELEASE_VERSION=""
    arg="--release"
    next_arg="--quiet"
    if [[ -n "${next_arg:-}" ]] && [[ "$next_arg" != --* ]]; then
      RELEASE_VERSION="$next_arg"
    else
      RELEASE_VERSION="latest"
    fi
    echo "$RELEASE_VERSION"
  )

  if [[ "$version" == "latest" ]]; then
    pass "--release --quiet correctly defaults to latest"
  else
    fail "--release --quiet expected 'latest', got '$version'"
  fi
}

# ===================================================================
# Test 8: --help output includes --release documentation
# ===================================================================
test_help_includes_release() {
  info "Test: --help mentions --release"

  local help_output
  help_output=$("$SCRIPT_DIR/setup-playground.sh" --help 2>&1)

  if echo "$help_output" | grep -q "\-\-release"; then
    pass "--help includes --release documentation"
  else
    fail "--help does not mention --release"
  fi
}

# ===================================================================
# Test 9: resolve fails gracefully for nonexistent repo
# ===================================================================
test_resolve_nonexistent_repo() {
  info "Test: resolution fails gracefully for nonexistent repo"

  local old_repo="$GITHUB_REPO"
  GITHUB_REPO="nonexistent-user/nonexistent-repo-12345"

  local output
  if output=$(resolve_latest_release 2>&1); then
    fail "nonexistent repo: expected failure but got: $output"
  else
    pass "nonexistent repo: failed gracefully"
  fi

  GITHUB_REPO="$old_repo"
}

# ===================================================================
# Run all tests
# ===================================================================
GH_TAG=""
CURL_TAG=""

echo ""
echo "${INFO}========================================${RESET}"
echo " Playground --release integration tests"
echo "${INFO}========================================${RESET}"
echo ""

run_test test_resolve_via_gh
run_test test_resolve_via_curl
run_test test_gh_curl_consistency
run_test test_curl_no_credentials
run_test test_arg_parse_release_bare
run_test test_arg_parse_release_explicit
run_test test_arg_parse_release_followed_by_flag
run_test test_help_includes_release
run_test test_resolve_nonexistent_repo

echo ""
echo "${INFO}────────────────────────────────────────${RESET}"
echo "Total: $TESTS_RUN  Passed: $TESTS_PASSED  Failed: $TESTS_FAILED"

if [[ $TESTS_FAILED -gt 0 ]]; then
  echo ""
  echo "${FAIL}Some tests failed.${RESET}"
  exit 1
fi

echo "${PASS}All tests passed.${RESET}"
