# Ansible Playbooks for Reaper Runtime

This directory contains Ansible playbooks for deploying and managing Reaper runtime on Kubernetes cluster nodes.

## Overview

Ansible provides a production-ready, idempotent approach to installing Reaper across multiple nodes:

- **External orchestration**: No circular dependencies with containerd
- **Idempotent**: Safe to re-run without side effects
- **Rollback support**: Built-in rollback playbook
- **Rolling updates**: Deploy to nodes one at a time to minimize impact
- **Cloud-agnostic**: Works with any cluster where nodes are SSH-accessible

## Prerequisites

### Local Machine

- Ansible 2.9+ installed (`pip install ansible` or `brew install ansible`)
- SSH key-based authentication configured for all target nodes
- Reaper binaries built: `cargo build --release`
- `kubectl` access to create RuntimeClass (post-installation)

### Target Nodes

- SSH access with sudo privileges
- containerd installed and running
- Python 3 installed (for Ansible)
- `/usr/local/bin` in PATH

## Quick Start

1. **Create inventory file**:
   ```bash
   cp ansible/inventory.ini.example ansible/inventory.ini
   # Edit inventory.ini with your node details
   ```

2. **Test connectivity**:
   ```bash
   ansible -i ansible/inventory.ini k8s_nodes -m ping
   ```

3. **Run installation playbook**:
   ```bash
   ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml
   ```

4. **Create RuntimeClass**:
   ```bash
   kubectl apply -f kubernetes/runtimeclass.yaml
   ```

5. **Verify**:
   ```bash
   kubectl apply -f kubernetes/runtimeclass.yaml  # deploys test pod
   kubectl logs reaper-example
   ```

## Playbooks

### install-reaper.yml

Main installation playbook that:
- Detects node architecture
- Copies binaries to `/usr/local/bin/`
- Backs up existing containerd configuration
- Merges Reaper runtime configuration
- Restarts containerd service
- Verifies installation

**Usage:**
```bash
# Standard installation
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml

# Dry run (check mode)
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml --check

# Verbose output
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml -v

# Target specific nodes
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml --limit node1,node2
```

### rollback-reaper.yml

Rollback playbook that:
- Removes Reaper binaries
- Restores containerd configuration from backup
- Restarts containerd service
- Cleans up overlay filesystem directories

**Usage:**
```bash
# Interactive rollback (prompts for confirmation)
ansible-playbook -i ansible/inventory.ini ansible/rollback-reaper.yml

# Automatic rollback (no prompts)
ansible-playbook -i ansible/inventory.ini ansible/rollback-reaper.yml -e 'ansible_check_mode=false'

# Rollback specific nodes
ansible-playbook -i ansible/inventory.ini ansible/rollback-reaper.yml --limit node3
```

## Inventory Configuration

The inventory file (`inventory.ini`) defines your cluster nodes and SSH connection details.

### Basic Example

```ini
[k8s_nodes]
node1 ansible_host=192.168.1.10 ansible_user=ubuntu
node2 ansible_host=192.168.1.11 ansible_user=ubuntu
node3 ansible_host=192.168.1.12 ansible_user=ubuntu

[k8s_nodes:vars]
ansible_ssh_private_key_file=~/.ssh/id_rsa
ansible_become=yes
ansible_python_interpreter=/usr/bin/python3
```

### Cloud Provider Examples

**GKE (Google Kubernetes Engine)**:
```ini
[k8s_nodes]
gke-node-1 ansible_host=gke-node-1.c.project-id.internal ansible_user=admin

[k8s_nodes:vars]
ansible_ssh_common_args='-o ProxyCommand="gcloud compute ssh %h --tunnel-through-iap"'
```

**EKS (Amazon Elastic Kubernetes Service)** with SSM:
```ini
[k8s_nodes]
eks-node-1 ansible_host=i-0123456789abcdef0 ansible_user=ec2-user

[k8s_nodes:vars]
ansible_connection=aws_ssm
ansible_aws_ssm_region=us-east-1
```

**AKS (Azure Kubernetes Service)** with bastion:
```ini
[k8s_nodes]
aks-node-1 ansible_host=10.0.1.10 ansible_user=azureuser

[k8s_nodes:vars]
ansible_ssh_common_args='-o ProxyCommand="ssh -W %h:%p bastion.azure.example.com"'
```

## Advanced Usage

### Rolling Updates

Deploy to nodes one at a time to minimize impact:

```bash
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml --forks=1
```

### Parallel Deployment

Deploy to multiple nodes in parallel (default is 5):

```bash
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml --forks=10
```

### Custom Binary Path

If binaries are in a different location:

```bash
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml \
  -e "local_binary_dir=/path/to/binaries"
```

### Custom Overlay Location

```bash
ansible-playbook -i ansible/inventory.ini ansible/install-reaper.yml \
  -e "overlay_base=/custom/overlay/path"
```

## Troubleshooting

### SSH Connection Issues

Test SSH connectivity:
```bash
ansible -i ansible/inventory.ini k8s_nodes -m ping
```

Debug SSH issues:
```bash
ansible -i ansible/inventory.ini k8s_nodes -m ping -vvv
```

### Sudo/Privilege Issues

Test sudo access:
```bash
ansible -i ansible/inventory.ini k8s_nodes -m shell -a "whoami" --become
```

### Containerd Not Running

Check containerd status on all nodes:
```bash
ansible -i ansible/inventory.ini k8s_nodes -m systemd -a "name=containerd state=started" --become
```

### Binary Verification Failed

Ensure binaries are built and executable:
```bash
ls -la target/release/containerd-shim-reaper-v2
ls -la target/release/reaper-runtime
cargo build --release --bin containerd-shim-reaper-v2 --bin reaper-runtime
```

### Configuration Validation Failed

Manually check containerd config on a node:
```bash
ansible -i ansible/inventory.ini k8s_nodes -m shell -a "containerd config dump | grep reaper" --become
```

## Files

- `install-reaper.yml` - Main installation playbook
- `rollback-reaper.yml` - Rollback playbook
- `inventory.ini.example` - Example inventory template
- `README.md` - This file

## See Also

- [Main README](../README.md) - Project overview
- [Kubernetes README](../kubernetes/README.md) - Kubernetes integration guide
- [Installation Plan](../docs/plan-install-script.md) - Detailed implementation plan
- [Testing Guide](../TESTING.md) - Testing procedures
