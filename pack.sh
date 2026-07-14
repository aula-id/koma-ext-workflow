#!/bin/bash
set -euo pipefail

# Resolve script directory regardless of CWD
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Create dist directory if missing
mkdir -p dist

echo "=== Building Workflow extension ==="

# Build the Rust daemon
echo "Building office-daemon (release)..."
cargo build --release -p office-daemon

# Build the UI
echo "Building UI..."
cd ui
source ~/.nvm/nvm.sh >/dev/null 2>&1
nvm use 24 >/dev/null 2>&1 || true
npm run build >/dev/null 2>&1
cd ..

echo ""
echo "=== Packaging Workflow extension ==="

# Determine which tool to use for JSON editing
if command -v jq &> /dev/null; then
  JSON_TOOL="jq"
else
  JSON_TOOL="python3"
fi

manifest_src="manifest.json"
binary_src="target/release/office-daemon"

# Create temp staging directory
stage_dir=$(mktemp -d)
trap "rm -rf '$stage_dir'" EXIT

# Create bin directory in staging
mkdir -p "$stage_dir/bin"

# Copy and modify manifest.json
if [ "$JSON_TOOL" = "jq" ]; then
  jq ".runtime.exec = \"bin/office-daemon\"" "$manifest_src" > "$stage_dir/manifest.json"
else
  python3 -c "
import json
with open('$manifest_src', 'r') as f:
  data = json.load(f)
data['runtime']['exec'] = 'bin/office-daemon'
with open('$stage_dir/manifest.json', 'w') as f:
  json.dump(data, f, indent=2)
"
fi

# Copy release binary
cp "$binary_src" "$stage_dir/bin/office-daemon"

# Copy the UI dist folder
cp -r "ui/dist" "$stage_dir/ui"

# Create zip from the staging directory contents
(cd "$stage_dir" && zip -q -r "$SCRIPT_DIR/dist/workflow.zip" manifest.json bin/ ui/)

# Clean up temp directory
rm -rf "$stage_dir"

echo "Packaged: dist/workflow.zip"
zip_path="dist/workflow.zip"
if [ -f "$zip_path" ]; then
  size=$(du -h "$zip_path" | cut -f1)
  echo ""
  echo "=== Summary ==="
  echo "Distributable package created:"
  echo "  $zip_path ($size)"
fi
