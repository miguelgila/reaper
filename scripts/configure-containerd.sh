#!/usr/bin/env bash
# Unified script to configure containerd with the Reaper shim v2 runtime
# Works for minikube, kind, and regular Kubernetes nodes
#
# Usage:
#   ./configure-containerd.sh                          # Local system
#   ./configure-containerd.sh minikube                 # Inside minikube
#   ./configure-containerd.sh kind <node-container>    # Inside kind node

set -euo pipefail

RUNTIME_TYPE="io.containerd.reaper.v2"
RUNTIME_NAME="reaper-v2"
SHIM_BINARY="containerd-shim-reaper-v2"
SHIM_PATH="/usr/local/bin/${SHIM_BINARY}"

configure_containerd() {
    local target="${1:-local}"
    local node_id="${2:-}"

    echo "📝 Configuring containerd for $target..."

    # The working configuration:
    # - Use default containerd config as base
    # - Add reaper-v2 runtime WITHOUT options section
    # - This avoids the cgroup path bug in containerd-shim library

    if [ "$target" = "minikube" ]; then
        minikube ssh -- "sudo bash -c '
            # Generate default config as base
            containerd config default > /tmp/containerd-config-new.toml

            # Add reaper-v2 runtime before runc section
            # Try both old and new plugin paths for compatibility
            # NOTE: NO options section - it triggers a cgroup path bug in containerd-shim
            if grep -q \"plugins.\\047io.containerd.cri.v1.runtime\\047.containerd.runtimes.runc\" /tmp/containerd-config-new.toml; then
                # New path (containerd 2.x)
                sed -i \"/\\[plugins.\\047io.containerd.cri.v1.runtime\\047.containerd.runtimes.runc\\]/i\\        [plugins.\\047io.containerd.cri.v1.runtime\\047.containerd.runtimes.reaper-v2]\\n          runtime_type = \\047io.containerd.reaper.v2\\047\\n          sandbox_mode = \\047podsandbox\\047\\n\" /tmp/containerd-config-new.toml
            else
                # Old path (containerd 1.x)
                sed -i \"/\\[plugins.\\\"io.containerd.grpc.v1.cri\\\".containerd.runtimes.runc\\]/i\\      [plugins.\\\"io.containerd.grpc.v1.cri\\\".containerd.runtimes.reaper-v2]\\n        runtime_type = \\\"io.containerd.reaper.v2\\\"\\n        sandbox_mode = \\\"podsandbox\\\"\\n\" /tmp/containerd-config-new.toml
            fi

            # Replace config
            mv /tmp/containerd-config-new.toml /etc/containerd/config.toml

            # Restart containerd
            systemctl restart containerd
        '"
        echo "✅ Containerd configured in minikube"

    elif [ "$target" = "kind" ]; then
        if [ -z "$node_id" ]; then
            echo "Error: kind requires node container ID" >&2
            exit 1
        fi

        # Get script directory to find minimal config
        SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

        docker exec "$node_id" bash -c "
            # Use minimal config template instead of full default
            # This avoids compatibility issues that cause control plane instability
            cat > /etc/containerd/config.toml <<'CONTAINERD_CONFIG'
version = 3
root = '/var/lib/containerd'
state = '/run/containerd'

[grpc]
address = '/run/containerd/containerd.sock'
uid = 0
gid = 0
max_recv_message_size = 16777216
max_send_message_size = 16777216

[debug]
level = 'info'

[plugins]
[plugins.'io.containerd.cri.v1.images']
snapshotter = 'overlayfs'

[plugins.'io.containerd.cri.v1.images'.registry]
config_path = '/etc/containerd/certs.d:/etc/docker/certs.d'

[plugins.'io.containerd.cri.v1.runtime']
enable_selinux = false
max_container_log_line_size = 16384
tolerate_missing_hugetlb_controller = true
disable_hugetlb_controller = true

[plugins.'io.containerd.cri.v1.runtime'.containerd]
default_runtime_name = 'runc'

[plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes]

[plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.reaper-v2]
runtime_type = 'io.containerd.reaper.v2'
sandbox_mode = 'podsandbox'

[plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.runc]
runtime_type = 'io.containerd.runc.v2'

[plugins.'io.containerd.cri.v1.runtime'.cni]
bin_dirs = ['/opt/cni/bin']
conf_dir = '/etc/cni/net.d'

[plugins.'io.containerd.grpc.v1.cri']
disable_tcp_service = true

[plugins.'io.containerd.snapshotter.v1.overlayfs']

[timeouts]
'io.containerd.timeout.shim.cleanup' = '5s'
'io.containerd.timeout.shim.load' = '5s'
'io.containerd.timeout.shim.shutdown' = '3s'
'io.containerd.timeout.task.state' = '2s'
CONTAINERD_CONFIG

            # Restart containerd
            pkill -HUP containerd || systemctl restart containerd
        "
        echo "✅ Containerd configured in kind node $node_id"

    else
        # Local system configuration
        echo "Generating containerd config..."
        sudo bash -c "
            containerd config default > /tmp/containerd-config-new.toml
            # NOTE: NO options section - it triggers a cgroup path bug in containerd-shim
            sed -i '/\\[plugins.\"io.containerd.grpc.v1.cri\".containerd.runtimes.runc\\]/i\\      [plugins.\"io.containerd.grpc.v1.cri\".containerd.runtimes.reaper-v2]\\n        runtime_type = \"io.containerd.reaper.v2\"\\n        sandbox_mode = \"podsandbox\"\\n' /tmp/containerd-config-new.toml
            mv /tmp/containerd-config-new.toml /etc/containerd/config.toml
            systemctl restart containerd
        "
        echo "✅ Containerd configured locally"
    fi
}

verify_config() {
    local target="${1:-local}"
    local node_id="${2:-}"

    echo "🔍 Verifying containerd configuration..."

    if [ "$target" = "minikube" ]; then
        minikube ssh -- "sudo grep -A 3 'reaper-v2' /etc/containerd/config.toml" || {
            echo "❌ Reaper runtime not found in config"
            return 1
        }
    elif [ "$target" = "kind" ]; then
        docker exec "$node_id" grep -A 3 'reaper-v2' /etc/containerd/config.toml || {
            echo "❌ Reaper runtime not found in config"
            return 1
        }
    else
        sudo grep -A 3 'reaper-v2' /etc/containerd/config.toml || {
            echo "❌ Reaper runtime not found in config"
            return 1
        }
    fi

    echo "✅ Containerd configuration verified"
}

# Main execution
case "${1:-local}" in
    minikube)
        configure_containerd minikube
        verify_config minikube
        ;;
    kind)
        configure_containerd kind "${2:-}"
        verify_config kind "${2:-}"
        ;;
    local|*)
        configure_containerd local
        verify_config local
        ;;
esac
