#!/usr/bin/env bash
# install-reaper.sh - Deploy Reaper runtime to any Kubernetes cluster
#
# This script automates the installation of the Reaper containerd shim runtime
# on Kubernetes clusters, supporting multiple deployment modes.
#
# Usage:
#   ./scripts/install-reaper.sh --kind <cluster-name>     # Install to Kind cluster
#   ./scripts/install-reaper.sh --auto                    # Auto-detect cluster type
#   ./scripts/install-reaper.sh --verify-only             # Verify existing installation
#   ./scripts/install-reaper.sh --dry-run --kind test     # Preview actions
#
# Options:
#   --kind <name>        Install to Kind cluster with given name
#   --auto               Auto-detect cluster type from kubectl context
#   --dry-run            Show what would be done without making changes
#   --verify-only        Only verify existing installation
#   --verbose            Print detailed output
#   --skip-build         Skip building binaries (use existing)
#   --binaries-path      Path to pre-built binaries directory
#   -h, --help           Show this help message

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
REAPER_VERSION="${REAPER_VERSION:-latest}"
SHIM_BINARY="containerd-shim-reaper-v2"
RUNTIME_BINARY="reaper-runtime"
INSTALL_PATH="/usr/local/bin"
OVERLAY_BASE="/run/reaper/overlay"
RUNTIMECLASS_NAME="reaper-v2"
RUNTIME_TYPE="io.containerd.reaper.v2"

# Script directory (for finding configure-containerd.sh)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# State
# ---------------------------------------------------------------------------
MODE=""              # kind, auto
DRY_RUN=false
VERIFY_ONLY=false
VERBOSE=false
SKIP_BUILD=false
BINARIES_PATH=""
KIND_CLUSTER_NAME=""
NODE_IDS=()
NODE_ARCH=""

# ---------------------------------------------------------------------------
# Color setup
# ---------------------------------------------------------------------------
setup_colors() {
  if [[ -n "${NO_COLOR:-}" ]]; then
    CLR_INFO="" CLR_SUCCESS="" CLR_ERROR="" CLR_WARN="" CLR_RESET="" CLR_DIM=""
  elif [[ -n "${CI:-}" ]] || [[ -t 1 ]]; then
    CLR_INFO=$'\033[1;36m'      # bold cyan
    CLR_SUCCESS=$'\033[1;32m'   # bold green
    CLR_ERROR=$'\033[1;31m'     # bold red
    CLR_WARN=$'\033[1;33m'      # bold yellow
    CLR_DIM=$'\033[0;37m'       # dim white
    CLR_RESET=$'\033[0m'
  else
    CLR_INFO="" CLR_SUCCESS="" CLR_ERROR="" CLR_WARN="" CLR_RESET="" CLR_DIM=""
  fi
}
setup_colors

# ---------------------------------------------------------------------------
# Logging functions
# ---------------------------------------------------------------------------
log_info() {
  echo "${CLR_INFO}[INFO]${CLR_RESET} $*"
}

log_success() {
  echo "${CLR_SUCCESS}[SUCCESS]${CLR_RESET} $*"
}

log_error() {
  echo "${CLR_ERROR}[ERROR]${CLR_RESET} $*" >&2
}

log_warn() {
  echo "${CLR_WARN}[WARN]${CLR_RESET} $*"
}

log_verbose() {
  if $VERBOSE; then
    echo "${CLR_DIM}[VERBOSE]${CLR_RESET} $*"
  fi
}

log_dry_run() {
  if $DRY_RUN; then
    echo "${CLR_WARN}[DRY-RUN]${CLR_RESET} Would execute: $*"
  fi
}

# ---------------------------------------------------------------------------
# Usage and argument parsing
# ---------------------------------------------------------------------------
usage() {
  cat <<EOF
Usage: $0 [OPTIONS]

Deploy Reaper runtime to a Kubernetes cluster.

Modes:
  --kind <name>        Install to Kind cluster with given name
  --auto               Auto-detect cluster type from kubectl context

Options:
  --dry-run            Show what would be done without making changes
  --verify-only        Only verify existing installation
  --verbose            Print detailed output
  --skip-build         Skip building binaries (use existing in target/)
  --binaries-path DIR  Path to directory containing pre-built binaries
  -h, --help           Show this help message

Examples:
  # Install to Kind cluster named 'reaper-ci'
  $0 --kind reaper-ci

  # Verify existing installation
  $0 --verify-only

  # Preview installation without changes
  $0 --dry-run --kind test

  # Use pre-built binaries
  $0 --kind test --binaries-path ./my-binaries

Environment Variables:
  REAPER_VERSION       Version tag (default: latest)
  NO_COLOR             Disable colored output

EOF
}

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
      --auto)
        MODE="auto"
        shift
        ;;
      --dry-run)
        DRY_RUN=true
        shift
        ;;
      --verify-only)
        VERIFY_ONLY=true
        shift
        ;;
      --verbose)
        VERBOSE=true
        shift
        ;;
      --skip-build)
        SKIP_BUILD=true
        shift
        ;;
      --binaries-path)
        BINARIES_PATH="${2:-}"
        if [[ -z "$BINARIES_PATH" ]]; then
          log_error "--binaries-path requires a directory path"
          exit 1
        fi
        shift 2
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

  # Validate mode
  if [[ -z "$MODE" ]] && ! $VERIFY_ONLY; then
    log_error "Must specify a mode: --kind or --auto"
    usage
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# Pre-flight checks
# ---------------------------------------------------------------------------
check_command() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    log_error "Required command not found: $cmd"
    return 1
  fi
  log_verbose "Found command: $cmd"
  return 0
}

preflight_checks() {
  log_info "Running pre-flight checks..."

  local required_commands=("kubectl")

  if [[ "$MODE" == "kind" ]]; then
    required_commands+=("kind" "docker")
  fi

  if ! $SKIP_BUILD && [[ -z "$BINARIES_PATH" ]]; then
    required_commands+=("cargo" "docker")
  fi

  for cmd in "${required_commands[@]}"; do
    if ! check_command "$cmd"; then
      log_error "Pre-flight check failed: missing required command '$cmd'"
      exit 1
    fi
  done

  # Check kubectl connectivity
  if ! kubectl cluster-info >/dev/null 2>&1; then
    log_error "Cannot connect to Kubernetes cluster. Check kubectl configuration."
    exit 1
  fi
  log_verbose "kubectl can connect to cluster"

  log_success "Pre-flight checks passed"
}

# ---------------------------------------------------------------------------
# Cluster detection and node discovery
# ---------------------------------------------------------------------------
detect_cluster_type() {
  log_info "Auto-detecting cluster type..."

  local context
  context=$(kubectl config current-context 2>/dev/null || echo "")

  if [[ "$context" == kind-* ]]; then
    MODE="kind"
    KIND_CLUSTER_NAME="${context#kind-}"
    log_info "Detected Kind cluster: $KIND_CLUSTER_NAME"
  else
    log_error "Auto-detection not yet supported for non-Kind clusters"
    log_error "Current context: $context"
    exit 1
  fi
}

discover_kind_nodes() {
  log_info "Discovering Kind nodes for cluster '$KIND_CLUSTER_NAME'..."

  # Check if cluster exists
  if ! kind get clusters 2>/dev/null | grep -q "^${KIND_CLUSTER_NAME}\$"; then
    log_error "Kind cluster '$KIND_CLUSTER_NAME' does not exist"
    log_info "Available clusters:"
    kind get clusters 2>/dev/null || echo "  (none)"
    exit 1
  fi

  # Get node container IDs
  local control_plane_id
  control_plane_id=$(docker ps --filter "name=${KIND_CLUSTER_NAME}-control-plane" --format '{{.ID}}')

  if [[ -z "$control_plane_id" ]]; then
    log_error "Could not find control-plane container for cluster '$KIND_CLUSTER_NAME'"
    exit 1
  fi

  NODE_IDS=("$control_plane_id")
  log_success "Found ${#NODE_IDS[@]} node(s): ${NODE_IDS[*]}"

  # Detect architecture
  NODE_ARCH=$(docker exec "$control_plane_id" uname -m)
  log_info "Node architecture: $NODE_ARCH"
}

# ---------------------------------------------------------------------------
# Binary building
# ---------------------------------------------------------------------------
get_target_triple() {
  local arch="$1"
  case "$arch" in
    aarch64)
      echo "aarch64-unknown-linux-musl"
      ;;
    x86_64)
      echo "x86_64-unknown-linux-musl"
      ;;
    *)
      log_error "Unsupported architecture: $arch"
      exit 1
      ;;
  esac
}

get_musl_image() {
  local arch="$1"
  case "$arch" in
    aarch64)
      echo "messense/rust-musl-cross:aarch64-musl"
      ;;
    x86_64)
      echo "messense/rust-musl-cross:x86_64-musl"
      ;;
    *)
      log_error "Unsupported architecture: $arch"
      exit 1
      ;;
  esac
}

build_binaries() {
  local arch="$1"

  if $SKIP_BUILD; then
    log_info "Skipping binary build (--skip-build)"
    return 0
  fi

  if [[ -n "$BINARIES_PATH" ]]; then
    log_info "Using pre-built binaries from: $BINARIES_PATH"
    return 0
  fi

  log_info "Building static musl binaries for $arch..."

  local target_triple
  target_triple=$(get_target_triple "$arch")

  local musl_image
  musl_image=$(get_musl_image "$arch")

  if $DRY_RUN; then
    log_dry_run "docker run --rm -v \"$PROJECT_ROOT\":/work -w /work \"$musl_image\" cargo build --release --bin \"$SHIM_BINARY\" --bin \"$RUNTIME_BINARY\" --target \"$target_triple\""
    return 0
  fi

  log_verbose "Target triple: $target_triple"
  log_verbose "Docker image: $musl_image"

  docker run --rm \
    -v "$PROJECT_ROOT":/work \
    -w /work \
    "$musl_image" \
    cargo build --release --bin "$SHIM_BINARY" --bin "$RUNTIME_BINARY" --target "$target_triple"

  log_success "Binaries built successfully"
}

get_binary_path() {
  local binary_name="$1"
  local arch="$2"

  if [[ -n "$BINARIES_PATH" ]]; then
    echo "$BINARIES_PATH/$binary_name"
  else
    local target_triple
    target_triple=$(get_target_triple "$arch")
    echo "$PROJECT_ROOT/target/$target_triple/release/$binary_name"
  fi
}

verify_binaries() {
  local arch="$1"

  log_info "Verifying binaries exist..."

  local shim_path
  shim_path=$(get_binary_path "$SHIM_BINARY" "$arch")

  local runtime_path
  runtime_path=$(get_binary_path "$RUNTIME_BINARY" "$arch")

  if [[ ! -f "$shim_path" ]]; then
    log_error "Shim binary not found: $shim_path"
    exit 1
  fi

  if [[ ! -f "$runtime_path" ]]; then
    log_error "Runtime binary not found: $runtime_path"
    exit 1
  fi

  log_verbose "Shim binary: $shim_path"
  log_verbose "Runtime binary: $runtime_path"
  log_success "Binaries verified"
}

# ---------------------------------------------------------------------------
# Deployment to Kind
# ---------------------------------------------------------------------------
deploy_binaries_to_kind_node() {
  local node_id="$1"
  local arch="$2"

  log_info "Deploying binaries to Kind node $node_id..."

  local shim_path
  shim_path=$(get_binary_path "$SHIM_BINARY" "$arch")

  local runtime_path
  runtime_path=$(get_binary_path "$RUNTIME_BINARY" "$arch")

  if $DRY_RUN; then
    log_dry_run "docker cp \"$shim_path\" \"$node_id:$INSTALL_PATH/$SHIM_BINARY\""
    log_dry_run "docker exec \"$node_id\" chmod +x \"$INSTALL_PATH/$SHIM_BINARY\""
    log_dry_run "docker cp \"$runtime_path\" \"$node_id:$INSTALL_PATH/$RUNTIME_BINARY\""
    log_dry_run "docker exec \"$node_id\" chmod +x \"$INSTALL_PATH/$RUNTIME_BINARY\""
    return 0
  fi

  # Copy shim binary
  docker cp "$shim_path" "$node_id:$INSTALL_PATH/$SHIM_BINARY"
  docker exec "$node_id" chmod +x "$INSTALL_PATH/$SHIM_BINARY"

  # Copy runtime binary
  docker cp "$runtime_path" "$node_id:$INSTALL_PATH/$RUNTIME_BINARY"
  docker exec "$node_id" chmod +x "$INSTALL_PATH/$RUNTIME_BINARY"

  log_success "Binaries deployed to node $node_id"
}

create_overlay_directories() {
  local node_id="$1"

  log_info "Creating overlay directories on node $node_id..."

  if $DRY_RUN; then
    log_dry_run "docker exec \"$node_id\" mkdir -p \"$OVERLAY_BASE/upper\" \"$OVERLAY_BASE/work\""
    return 0
  fi

  docker exec "$node_id" mkdir -p "$OVERLAY_BASE/upper" "$OVERLAY_BASE/work"

  log_success "Overlay directories created"
}

configure_containerd_on_kind_node() {
  local node_id="$1"

  log_info "Configuring containerd on node $node_id..."

  if $DRY_RUN; then
    log_dry_run "$SCRIPT_DIR/configure-containerd.sh kind \"$node_id\""
    return 0
  fi

  # Run the configure-containerd script
  "$SCRIPT_DIR/configure-containerd.sh" kind "$node_id"

  log_success "Containerd configured"
}

deploy_to_kind() {
  log_info "Deploying to Kind cluster '$KIND_CLUSTER_NAME'..."

  discover_kind_nodes
  build_binaries "$NODE_ARCH"
  verify_binaries "$NODE_ARCH"

  for node_id in "${NODE_IDS[@]}"; do
    deploy_binaries_to_kind_node "$node_id" "$NODE_ARCH"
    create_overlay_directories "$node_id"
    configure_containerd_on_kind_node "$node_id"
  done

  create_runtimeclass

  log_success "Deployment to Kind cluster complete"
}

# ---------------------------------------------------------------------------
# RuntimeClass creation
# ---------------------------------------------------------------------------
create_runtimeclass() {
  log_info "Creating RuntimeClass '$RUNTIMECLASS_NAME'..."

  if $DRY_RUN; then
    log_dry_run "kubectl apply -f - <<EOF
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: $RUNTIMECLASS_NAME
handler: $RUNTIMECLASS_NAME
EOF"
    return 0
  fi

  cat <<EOF | kubectl apply -f - >/dev/null
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: $RUNTIMECLASS_NAME
handler: $RUNTIMECLASS_NAME
EOF

  log_success "RuntimeClass created"
}

# ---------------------------------------------------------------------------
# Verification
# ---------------------------------------------------------------------------
verify_binaries_on_kind_node() {
  local node_id="$1"

  log_verbose "Verifying binaries on node $node_id..."

  # Check shim binary
  if ! docker exec "$node_id" test -x "$INSTALL_PATH/$SHIM_BINARY"; then
    log_error "Shim binary not found or not executable on node $node_id: $INSTALL_PATH/$SHIM_BINARY"
    return 1
  fi

  # Check runtime binary
  if ! docker exec "$node_id" test -x "$INSTALL_PATH/$RUNTIME_BINARY"; then
    log_error "Runtime binary not found or not executable on node $node_id: $INSTALL_PATH/$RUNTIME_BINARY"
    return 1
  fi

  log_verbose "Binaries verified on node $node_id"
  return 0
}

verify_containerd_config_on_kind_node() {
  local node_id="$1"

  log_verbose "Verifying containerd config on node $node_id..."

  if ! docker exec "$node_id" grep -q "$RUNTIMECLASS_NAME" /etc/containerd/config.toml; then
    log_error "Containerd config does not contain $RUNTIMECLASS_NAME runtime on node $node_id"
    return 1
  fi

  log_verbose "Containerd config verified on node $node_id"
  return 0
}

verify_runtimeclass() {
  log_verbose "Verifying RuntimeClass..."

  if ! kubectl get runtimeclass "$RUNTIMECLASS_NAME" >/dev/null 2>&1; then
    log_error "RuntimeClass '$RUNTIMECLASS_NAME' not found"
    return 1
  fi

  log_verbose "RuntimeClass verified"
  return 0
}

verify_installation() {
  log_info "Verifying installation..."

  local failed=false

  # Verify based on mode
  if [[ "$MODE" == "kind" ]] || [[ -n "$KIND_CLUSTER_NAME" ]]; then
    # If not already discovered, discover nodes
    if [[ ${#NODE_IDS[@]} -eq 0 ]]; then
      discover_kind_nodes
    fi

    for node_id in "${NODE_IDS[@]}"; do
      if ! verify_binaries_on_kind_node "$node_id"; then
        failed=true
      fi
      if ! verify_containerd_config_on_kind_node "$node_id"; then
        failed=true
      fi
    done
  fi

  if ! verify_runtimeclass; then
    failed=true
  fi

  if $failed; then
    log_error "Installation verification failed"
    return 1
  fi

  log_success "Installation verification passed"
  return 0
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
main() {
  parse_args "$@"

  if $DRY_RUN; then
    log_warn "Running in DRY-RUN mode - no changes will be made"
  fi

  preflight_checks

  if $VERIFY_ONLY; then
    # For verify-only, try to detect mode if not specified
    if [[ -z "$MODE" ]]; then
      detect_cluster_type
    fi
    verify_installation
    exit $?
  fi

  # Auto-detect if needed
  if [[ "$MODE" == "auto" ]]; then
    detect_cluster_type
  fi

  # Deploy based on mode
  case "$MODE" in
    kind)
      deploy_to_kind
      ;;
    *)
      log_error "Unsupported mode: $MODE"
      exit 1
      ;;
  esac

  # Verify installation
  if ! $DRY_RUN; then
    verify_installation
  fi

  log_success "Installation complete!"
}

main "$@"
