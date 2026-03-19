#!/usr/bin/env bash
# Generate CRD YAML from Rust types and save to deploy/kubernetes/crds/.
# Usage: ./scripts/generate-crds.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
K8S_CRD_DIR="$PROJECT_DIR/deploy/kubernetes/crds"
HELM_CRD_DIR="$PROJECT_DIR/deploy/helm/reaper/crds"

mkdir -p "$K8S_CRD_DIR"
mkdir -p "$HELM_CRD_DIR"

echo "Generating CRD definitions..."

# The controller outputs two JSON CRDs to stdout (separated by newline).
# ReaperPod CRD is first, ReaperOverlay CRD is second.
# The separator "---" goes to stderr to avoid mixing with JSON.

PYTHON_CLEAN='
import sys, json, yaml

def clean_crd(data):
    """Clean up empty arrays from kube-rs derive."""
    if "spec" in data:
        names = data["spec"].get("names", {})
        for key in ["categories", "shortNames"]:
            if key in names and names[key] == []:
                del names[key]
    return data

# Read all stdin, split on newlines to find JSON objects
raw = sys.stdin.read()
# Split by newlines and find JSON objects (lines starting with {)
jsons = []
buf = ""
depth = 0
for line in raw.split("\n"):
    stripped = line.strip()
    if not stripped:
        continue
    buf += line + "\n"
    depth += stripped.count("{") - stripped.count("}")
    if depth == 0 and buf.strip():
        try:
            jsons.append(json.loads(buf))
        except json.JSONDecodeError:
            pass
        buf = ""

for i, data in enumerate(jsons):
    data = clean_crd(data)
    kind = data.get("spec", {}).get("names", {}).get("kind", "Unknown")
    plural = data.get("spec", {}).get("names", {}).get("plural", "unknown")
    group = data.get("spec", {}).get("group", "unknown")
    filename = f"{plural}.{group}.yaml"
    print(f"CRD:{filename}", file=sys.stderr)
    with open(f"{sys.argv[1]}/{filename}", "w") as f:
        yaml.dump(data, f, default_flow_style=False)
'

cargo run --features controller --bin reaper-controller -- --generate-crds 2>/dev/null \
    | python3 -c "$PYTHON_CLEAN" "$K8S_CRD_DIR" 2>&1 | while read -r line; do
    if [[ "$line" == CRD:* ]]; then
        filename="${line#CRD:}"
        echo "Written: $K8S_CRD_DIR/$filename"
        # Copy to Helm chart crds/ directory
        cp "$K8S_CRD_DIR/$filename" "$HELM_CRD_DIR/$filename"
        echo "Copied:  $HELM_CRD_DIR/$filename"
    fi
done

echo "Done."
