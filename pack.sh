#!/bin/bash
set -euo pipefail

# Resolve script directory regardless of CWD
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Create dist directory if missing
mkdir -p dist

echo "=== Building Workflow extension ==="

# Tri-platform: build for the NATIVE platform by default, or a cross target via
# `PACK_TARGET=<rust-triple> ./pack.sh` (e.g. x86_64-pc-windows-msvc — needs the
# matching toolchain/linker installed). Windows binaries carry .exe INSIDE the
# staging tree too, matching the manifest runtime.exec written below.
PACK_TARGET="${PACK_TARGET:-}"
TARGET_FLAG=""
TARGET_DIR="target/release"
if [ -n "$PACK_TARGET" ]; then
  TARGET_FLAG="--target $PACK_TARGET"
  TARGET_DIR="target/$PACK_TARGET/release"
fi
case "${PACK_TARGET:-$(uname -s)}" in
  *windows*|*Windows*|MINGW*|MSYS*|CYGWIN*) BIN_EXT=".exe" ;;
  *) BIN_EXT="" ;;
esac

# Build the Rust daemon + the MCP server (both shipped in the zip's bin/)
echo "Building office-daemon + workflow-mcp (release${PACK_TARGET:+, $PACK_TARGET})..."
cargo build --release -p office-daemon -p workflow-mcp $TARGET_FLAG

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
binary_src="$TARGET_DIR/office-daemon$BIN_EXT"
mcp_binary_src="$TARGET_DIR/workflow-mcp$BIN_EXT"

# Create temp staging directory
stage_dir=$(mktemp -d)
trap "rm -rf '$stage_dir'" EXIT

# Create bin directory in staging
mkdir -p "$stage_dir/bin"

# Copy and modify manifest.json (runtime.exec carries .exe on a windows target)
if [ "$JSON_TOOL" = "jq" ]; then
  jq ".runtime.exec = \"bin/office-daemon$BIN_EXT\"" "$manifest_src" > "$stage_dir/manifest.json"
else
  python3 -c "
import json
with open('$manifest_src', 'r') as f:
  data = json.load(f)
data['runtime']['exec'] = 'bin/office-daemon$BIN_EXT'
with open('$stage_dir/manifest.json', 'w') as f:
  json.dump(data, f, indent=2)
"
fi

# Copy release binaries (the daemon + the MCP server, side by side in bin/)
cp "$binary_src" "$stage_dir/bin/office-daemon$BIN_EXT"
cp "$mcp_binary_src" "$stage_dir/bin/workflow-mcp$BIN_EXT"

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

  # Verify both binaries made it into the zip before declaring success.
  if ! unzip -l "$zip_path" | grep -q "bin/office-daemon"; then
    echo "Error: bin/office-daemon missing from $zip_path"
    exit 1
  fi
  if ! unzip -l "$zip_path" | grep -q "bin/workflow-mcp"; then
    echo "Error: bin/workflow-mcp missing from $zip_path"
    exit 1
  fi

  echo ""
  echo "=== Summary ==="
  echo "Distributable package created:"
  echo "  $zip_path ($size)"
  echo "Zip contents (bin/):"
  unzip -l "$zip_path" | grep "bin/" | sed 's/^/  /'
fi
