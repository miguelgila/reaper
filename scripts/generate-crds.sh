#!/usr/bin/env bash
# Generate CRD YAML from Rust types and save to deploy/kubernetes/crds/.
# Usage: ./scripts/generate-crds.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
CRD_DIR="$PROJECT_DIR/deploy/kubernetes/crds"

mkdir -p "$CRD_DIR"

echo "Generating CRD definitions..."

# Generate JSON from Rust types, convert to YAML
cargo run --features controller --bin reaper-controller -- --generate-crds 2>/dev/null \
    | python3 -c "
import sys, json, yaml
data = json.load(sys.stdin)
# Clean up empty arrays from kube-rs derive
if 'spec' in data:
    names = data['spec'].get('names', {})
    for key in ['categories', 'shortNames']:
        if key in names and names[key] == []:
            del names[key]
yaml.dump(data, sys.stdout, default_flow_style=False)
" > "$CRD_DIR/reaperpods.reaper.io.yaml"

echo "Written: $CRD_DIR/reaperpods.reaper.io.yaml"
