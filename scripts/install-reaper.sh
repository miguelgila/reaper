#!/usr/bin/env bash
# install-reaper.sh - Deploy Reaper runtime using Ansible
#
# This is a unified installation script that uses Ansible for both Kind
# and production clusters. It provides a single, consistent deployment method.
#
# Usage:
#   ./scripts/install-reaper.sh --kind <cluster-name>
#   ./scripts/install-reaper.sh --kind <cluster-name> --release v0.2.0
#   ./scripts/install-reaper.sh --inventory <inventory-file>
#   ./scripts/install-reaper.sh --inventory <inventory-file> --release v0.2.0
#
# For Kind clusters, this script auto-generates an inventory and uses
# Ansible's Docker connection plugin. For production, it uses your provided
# inventory with SSH.
#
# The --release flag downloads pre-built binaries from GitHub Releases
# instead of requiring locally built binaries.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ANSIBLE_DIR="$PROJECT_ROOT/deploy/ansible"

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
GITHUB_REPO="miguelgila/reaper"

# ---------------------------------------------------------------------------
# State
# ---------------------------------------------------------------------------
MODE=""  # kind, inventory
KIND_CLUSTER_NAME=""
INVENTORY_FILE=""
RELEASE_VERSION=""
VERBOSE=false
DRY_RUN=false
SKIP_RUNTIMECLASS=false

# ---------------------------------------------------------------------------
# Color setup
# ---------------------------------------------------------------------------
setup_colors() {
  if [[ -n "${NO_COLOR:-}" ]] || [[ ! -t 1 ]]; then
    CLR_INFO="" CLR_SUCCESS="" CLR_ERROR="" CLR_WARN="" CLR_RESET=""
  else
    CLR_INFO=$'\033[1;36m'
    CLR_SUCCESS=$'\033[1;32m'
    CLR_ERROR=$'\033[1;31m'
    CLR_WARN=$'\033[1;33m'
    CLR_RESET=$'\033[0m'
  fi
}
setup_colors

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
log_info() { echo "${CLR_INFO}[INFO]${CLR_RESET} $*"; }
log_success() { echo "${CLR_SUCCESS}[SUCCESS]${CLR_RESET} $*"; }
log_error() { echo "${CLR_ERROR}[ERROR]${CLR_RESET} $*" >&2; }
log_warn() { echo "${CLR_WARN}[WARN]${CLR_RESET} $*"; }

# ---------------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------------
usage() {
  cat <<EOF
Usage: $0 [OPTIONS]

Deploy Reaper runtime to Kubernetes clusters using Ansible.

Modes:
  --kind <name>          Deploy to Kind cluster (auto-generates inventory)
  --inventory <file>     Deploy using existing inventory file

Options:
  --release <version>    Download pre-built binaries from GitHub Releases (e.g., v0.2.0)
  --verbose              Enable verbose Ansible output (-vv)
  --dry-run              Ansible check mode (no changes)
  --skip-runtimeclass    Skip RuntimeClass creation
  -h, --help             Show this help message

Examples:
  # Deploy to Kind cluster (local binaries)
  $0 --kind my-cluster

  # Deploy to Kind cluster (pre-built release)
  $0 --kind my-cluster --release v0.2.0

  # Deploy to production cluster (pre-built release)
  $0 --inventory deploy/ansible/inventory.ini --release v0.2.0

  # Dry run on Kind
  $0 --kind test --dry-run

Environment Variables:
  NO_COLOR               Disable colored output
  REAPER_BINARY_DIR      Override binary directory for Ansible installer

EOF
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
parse_args() {
  while [[ $# -gt 0 ]]; do
    case $1 in
      --kind)
        MODE="kind"
        KIND_CLUSTER_NAME="${2:-}"
        if [[ -z "$KIND_CLUSTER_NAME" ]]; then
          log_error "--kind requires a cluster name"
          exit 1
        fi
        shift 2
        ;;
      --inventory)
        MODE="inventory"
        INVENTORY_FILE="${2:-}"
        if [[ -z "$INVENTORY_FILE" ]]; then
          log_error "--inventory requires a file path"
          exit 1
        fi
        shift 2
        ;;
      --release)
        RELEASE_VERSION="${2:-}"
        if [[ -z "$RELEASE_VERSION" ]]; then
          log_error "--release requires a version (e.g., v0.2.0)"
          exit 1
        fi
        shift 2
        ;;
      --verbose)
        VERBOSE=true
        shift
        ;;
      --dry-run)
        DRY_RUN=true
        shift
        ;;
      --skip-runtimeclass)
        SKIP_RUNTIMECLASS=true
        shift
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        log_error "Unknown option: $1"
        usage
        exit 1
        ;;
    esac
  done

  if [[ -z "$MODE" ]]; then
    log_error "Must specify --kind or --inventory"
    usage
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# Pre-flight checks
# ---------------------------------------------------------------------------
check_ansible() {
  if ! command -v ansible-playbook &>/dev/null; then
    log_error "ansible-playbook not found. Install: pip install ansible"
    exit 1
  fi
}

check_kind_cluster() {
  if ! command -v kind &>/dev/null; then
    log_error "kind not found. Install from https://kind.sigs.k8s.io/"
    exit 1
  fi

  if ! kind get clusters 2>/dev/null | grep -q "^${KIND_CLUSTER_NAME}$"; then
    log_error "Kind cluster '$KIND_CLUSTER_NAME' not found"
    log_info "Available clusters:"
    kind get clusters 2>/dev/null || echo "  (none)"
    exit 1
  fi
}

check_binaries() {
  # If using release mode, binaries will be downloaded later
  if [[ -n "$RELEASE_VERSION" ]]; then
    return 0
  fi

  # Allow override via REAPER_BINARY_DIR for CI/integration tests
  local binary_dir="${REAPER_BINARY_DIR:-$PROJECT_ROOT/target/release}"
  if [[ ! -f "$binary_dir/containerd-shim-reaper-v2" ]]; then
    log_error "Binaries not found at $binary_dir"
    log_error "Run: cargo build --release"
    log_error "Or use --release <version> to download pre-built binaries"
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# Download release binaries
# ---------------------------------------------------------------------------
download_release() {
  local version="$1"
  local target="$2"  # e.g., x86_64-unknown-linux-musl

  local download_dir
  download_dir=$(mktemp -d)

  local tarball_name="reaper-${version#v}-${target}.tar.gz"
  local url="https://github.com/${GITHUB_REPO}/releases/download/${version}/${tarball_name}"
  local checksum_url="https://github.com/${GITHUB_REPO}/releases/download/${version}/checksums-sha256.txt"

  log_info "Downloading $tarball_name from GitHub Releases..."

  if ! curl -fsSL -o "$download_dir/$tarball_name" "$url"; then
    log_error "Failed to download: $url"
    log_error "Check that version '$version' exists at https://github.com/${GITHUB_REPO}/releases"
    rm -rf "$download_dir"
    exit 1
  fi

  log_info "Verifying checksum..."
  if curl -fsSL -o "$download_dir/checksums-sha256.txt" "$checksum_url" 2>/dev/null; then
    (cd "$download_dir" && sha256sum -c <(grep "$tarball_name" checksums-sha256.txt))
    if [[ $? -ne 0 ]]; then
      log_error "Checksum verification failed!"
      rm -rf "$download_dir"
      exit 1
    fi
    log_success "Checksum verified"
  else
    log_warn "Checksums file not available, skipping verification"
  fi

  log_info "Extracting binaries..."
  tar xzf "$download_dir/$tarball_name" -C "$download_dir"

  # Find the extracted directory (reaper-VERSION-TARGET/)
  local extracted_dir="$download_dir/reaper-${version#v}-${target}"
  if [[ ! -d "$extracted_dir" ]]; then
    log_error "Unexpected tarball layout — expected directory: reaper-${version#v}-${target}"
    rm -rf "$download_dir"
    exit 1
  fi

  # Set REAPER_BINARY_DIR so Ansible picks up the downloaded binaries
  export REAPER_BINARY_DIR="$extracted_dir"
  log_success "Binaries downloaded to $extracted_dir"

  # Return the temp dir path so the caller can clean it up
  echo "$download_dir"
}

# ---------------------------------------------------------------------------
# Detect target triple for Kind nodes
# ---------------------------------------------------------------------------
detect_kind_target() {
  local cluster_name="$1"
  local node_id
  node_id=$(docker ps --filter "name=${cluster_name}-control-plane" --format '{{.ID}}')
  if [[ -z "$node_id" ]]; then
    log_error "Cannot find control-plane container for cluster '$cluster_name'"
    exit 1
  fi

  local arch
  arch=$(docker exec "$node_id" uname -m 2>&1)
  case "$arch" in
    aarch64) echo "aarch64-unknown-linux-musl" ;;
    x86_64)  echo "x86_64-unknown-linux-musl" ;;
    *)
      log_error "Unsupported architecture: $arch"
      exit 1
      ;;
  esac
}

check_inventory_file() {
  if [[ ! -f "$INVENTORY_FILE" ]]; then
    log_error "Inventory file not found: $INVENTORY_FILE"
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# Kind deployment
# ---------------------------------------------------------------------------
deploy_to_kind() {
  log_info "Deploying to Kind cluster: $KIND_CLUSTER_NAME"

  # If release mode, download binaries for the right architecture
  local release_temp_dir=""
  if [[ -n "$RELEASE_VERSION" ]]; then
    local target
    target=$(detect_kind_target "$KIND_CLUSTER_NAME")
    log_info "Detected Kind node target: $target"
    release_temp_dir=$(download_release "$RELEASE_VERSION" "$target")
  fi

  # Generate inventory
  local temp_inventory
  temp_inventory=$(mktemp)
  trap "rm -f '$temp_inventory'; [[ -n '$release_temp_dir' ]] && rm -rf '$release_temp_dir'" EXIT

  log_info "Generating Kind inventory..."
  if ! "$SCRIPT_DIR/generate-kind-inventory.sh" "$KIND_CLUSTER_NAME" "$temp_inventory"; then
    log_error "Failed to generate Kind inventory"
    exit 1
  fi

  # Run Ansible playbook
  run_ansible_playbook "$temp_inventory"
}

# ---------------------------------------------------------------------------
# Inventory-based deployment
# ---------------------------------------------------------------------------
deploy_with_inventory() {
  log_info "Deploying using inventory: $INVENTORY_FILE"

  # If release mode, download binaries (default to x86_64 for production)
  local release_temp_dir=""
  if [[ -n "$RELEASE_VERSION" ]]; then
    local target="${REAPER_TARGET:-x86_64-unknown-linux-musl}"
    log_info "Using target: $target (set REAPER_TARGET to override)"
    release_temp_dir=$(download_release "$RELEASE_VERSION" "$target")
    trap "rm -rf '$release_temp_dir'" EXIT
  fi

  run_ansible_playbook "$INVENTORY_FILE"
}

# ---------------------------------------------------------------------------
# Run Ansible playbook
# ---------------------------------------------------------------------------
run_ansible_playbook() {
  local inventory=$1

  local ansible_args=()
  ansible_args+=("-i" "$inventory")

  # Use our ansible.cfg for cross-version compatibility
  export ANSIBLE_CONFIG="$ANSIBLE_DIR/ansible.cfg"

  # Override any environment variables that might use removed plugins
  export ANSIBLE_STDOUT_CALLBACK=default
  export ANSIBLE_LOAD_CALLBACK_PLUGINS=false

  if $VERBOSE; then
    ansible_args+=("-vv")
  fi

  if $DRY_RUN; then
    ansible_args+=("--check")
    log_warn "Running in check mode (dry-run)"
  fi

  log_info "Running Ansible playbook..."

  # Pass binary directory to Ansible (override default if REAPER_BINARY_DIR is set)
  local binary_dir="${REAPER_BINARY_DIR:-$PROJECT_ROOT/target/release}"
  ansible_args+=("-e" "local_binary_dir=$binary_dir")

  if ! ansible-playbook "${ansible_args[@]}" "$ANSIBLE_DIR/install-reaper.yml"; then
    log_error "Ansible playbook failed"
    exit 1
  fi

  log_success "Ansible playbook completed"

  # Create RuntimeClass
  if ! $SKIP_RUNTIMECLASS && ! $DRY_RUN; then
    create_runtimeclass
  fi
}

# ---------------------------------------------------------------------------
# Create RuntimeClass
# ---------------------------------------------------------------------------
create_runtimeclass() {
  if ! command -v kubectl &>/dev/null; then
    log_warn "kubectl not found, skipping RuntimeClass creation"
    return 0
  fi

  log_info "Creating RuntimeClass..."
  if kubectl apply -f "$PROJECT_ROOT/deploy/kubernetes/runtimeclass.yaml"; then
    log_success "RuntimeClass created"
  else
    log_warn "RuntimeClass creation failed (may already exist)"
  fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
main() {
  parse_args "$@"

  log_info "Reaper Ansible Installer"
  log_info "Mode: $MODE"

  # Pre-flight checks
  check_ansible
  check_binaries

  case "$MODE" in
    kind)
      check_kind_cluster
      deploy_to_kind
      ;;
    inventory)
      check_inventory_file
      deploy_with_inventory
      ;;
    *)
      log_error "Unknown mode: $MODE"
      exit 1
      ;;
  esac

  log_success "Installation complete!"
  log_info ""
  log_info "Next steps:"
  log_info "  1. Verify: kubectl get runtimeclass reaper-v2"
  log_info "  2. Test: kubectl apply -f deploy/kubernetes/runtimeclass.yaml"
  log_info "  3. Check logs: kubectl logs reaper-example"
}

main "$@"
