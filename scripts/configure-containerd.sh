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

        docker exec "$node_id" bash -c "
            # Read the current kind-generated containerd config
            # (preserving all existing settings and customizations)
            cp /etc/containerd/config.toml /tmp/containerd-config-new.toml

            # Remove any existing reaper-v2 sections to avoid TOML duplicate table errors
            # Use a more precise pattern: delete reaper-v2 line and next 2 lines (runtime_type and sandbox_mode)
            # This is safer than range deletions which can remove too much
            if grep -q '\[.*reaper-v2\]' /tmp/containerd-config-new.toml; then
                sed -i '/\[.*reaper-v2\]/,+2d' /tmp/containerd-config-new.toml
            fi

            # Add reaper-v2 runtime before runc section
            # NOTE: NO options section - it triggers a cgroup path bug in containerd-shim
            # Try modern plugin path first (v2 config format), fall back to v1 if needed
            if grep -q 'runtimes.runc\]' /tmp/containerd-config-new.toml; then
                # v2 format with io.containerd.grpc.v1.cri path
                sed -i \"/\\[plugins.\\\"io.containerd.grpc.v1.cri\\\".containerd.runtimes.runc\\]/i\\[plugins.\\\"io.containerd.grpc.v1.cri\\\".containerd.runtimes.reaper-v2]\\n  runtime_type = \\\"io.containerd.reaper.v2\\\"\\n  sandbox_mode = \\\"podsandbox\\\"\\n\" /tmp/containerd-config-new.toml
            elif grep -q \"plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.runc\" /tmp/containerd-config-new.toml; then
                # v3 format with io.containerd.cri.v1.runtime path
                sed -i \"/\\[plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.runc\\]/i\\[plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.reaper-v2]\\nruntime_type = 'io.containerd.reaper.v2'\\nsandbox_mode = 'podsandbox'\\n\" /tmp/containerd-config-new.toml
            else
                echo \"Error: Could not find runc runtime section in config\" >&2
                echo \"Config contents:\" >&2
                grep -E '\\[plugins' /tmp/containerd-config-new.toml >&2
                exit 1
            fi

            # Replace config
            mv /tmp/containerd-config-new.toml /etc/containerd/config.toml

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
