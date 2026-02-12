#!/usr/bin/env bash
# install-reaper-ansible.sh - Deploy Reaper runtime using Ansible
#
# This is a unified installation script that uses Ansible for both Kind
# and production clusters. It provides a single, consistent deployment method.
#
# Usage:
#   ./scripts/install-reaper-ansible.sh --kind <cluster-name>
#   ./scripts/install-reaper-ansible.sh --inventory <inventory-file>
#
# For Kind clusters, this script auto-generates an inventory and uses
# Ansible's Docker connection plugin. For production, it uses your provided
# inventory with SSH.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ANSIBLE_DIR="$PROJECT_ROOT/ansible"

# ---------------------------------------------------------------------------
# State
# ---------------------------------------------------------------------------
MODE=""  # kind, inventory
KIND_CLUSTER_NAME=""
INVENTORY_FILE=""
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
  --verbose              Enable verbose Ansible output (-vv)
  --dry-run              Ansible check mode (no changes)
  --skip-runtimeclass    Skip RuntimeClass creation
  -h, --help             Show this help message

Examples:
  # Deploy to Kind cluster
  $0 --kind my-cluster

  # Deploy to production cluster
  $0 --inventory ansible/inventory.ini

  # Dry run on Kind
  $0 --kind test --dry-run

Environment Variables:
  NO_COLOR               Disable colored output

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
  # Allow override via REAPER_BINARY_DIR for CI/integration tests
  local binary_dir="${REAPER_BINARY_DIR:-$PROJECT_ROOT/target/release}"
  if [[ ! -f "$binary_dir/containerd-shim-reaper-v2" ]]; then
    log_error "Binaries not found at $binary_dir"
    log_error "Run: cargo build --release"
    exit 1
  fi
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

  # Generate inventory
  local temp_inventory
  temp_inventory=$(mktemp)
  trap "rm -f '$temp_inventory'" EXIT

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
  if kubectl apply -f "$PROJECT_ROOT/kubernetes/runtimeclass.yaml"; then
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
  log_info "  2. Test: kubectl apply -f kubernetes/runtimeclass.yaml"
  log_info "  3. Check logs: kubectl logs reaper-example"
}

main "$@"
