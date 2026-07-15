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

# CI (GitHub Actions release matrix) sets PACK_PLATFORM to a store-convention
# label (e.g. "linux-x64", "darwin-arm64") so each target uploads its own zip
# instead of clobbering a shared dist/workflow.zip.
PACK_PLATFORM="${PACK_PLATFORM:-}"
if [ -n "$PACK_PLATFORM" ]; then
  zip_name="workflow-$PACK_PLATFORM.zip"
else
  zip_name="workflow.zip"
fi
zip_path="dist/$zip_name"

# Build the Rust daemon + the MCP server (both shipped in the zip's bin/)
echo "Building office-daemon + workflow-mcp (release${PACK_TARGET:+, $PACK_TARGET})..."
cargo build --release -p office-daemon -p workflow-mcp $TARGET_FLAG

# Build the UI
echo "Building UI..."
cd ui
# nvm is a local dev convenience; on CI runners node is already provisioned
# on PATH (actions/setup-node) and ~/.nvm may not exist at all.
if [ -f ~/.nvm/nvm.sh ]; then
  source ~/.nvm/nvm.sh >/dev/null 2>&1
  nvm use 24 >/dev/null 2>&1 || true
fi
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
# ALWAYS build a fresh archive: `zip -r` against an existing file UPDATES it in
# place, and a stale/corrupt leftover zip (e.g. from an older pack run) makes the
# result unpredictable across machines.
rm -f "$SCRIPT_DIR/$zip_path"
if command -v zip &> /dev/null; then
  (cd "$stage_dir" && zip -q -r "$SCRIPT_DIR/$zip_path" manifest.json bin/ ui/)
else
  # git-bash on the Windows runner ships no `zip` binary — fall back to
  # python3's zipfile module. Preserve the executable bit via external_attr
  # so office-daemon.exe / workflow-mcp.exe stay runnable after unzip.
  echo "'zip' not found, falling back to python3 zipfile module..."
  python3 - "$stage_dir" "$SCRIPT_DIR/$zip_path" <<'PYEOF'
import os
import sys
import zipfile

stage_dir, out_path = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out_path, "w", zipfile.ZIP_DEFLATED) as zf:
    for root, dirs, files in os.walk(stage_dir):
        dirs.sort()
        for name in sorted(files):
            full = os.path.join(root, name)
            arcname = os.path.relpath(full, stage_dir)
            zi = zipfile.ZipInfo(arcname)
            zi.compress_type = zipfile.ZIP_DEFLATED
            st = os.stat(full)
            zi.external_attr = (st.st_mode & 0xFFFF) << 16
            with open(full, "rb") as fh:
                zf.writestr(zi, fh.read())
PYEOF
fi

# Clean up temp directory
rm -rf "$stage_dir"

echo "Packaged: $zip_path"
if [ -f "$zip_path" ]; then
  size=$(du -h "$zip_path" | cut -f1)

  # unzip may be absent alongside zip (same git-bash gap on Windows) — fall
  # back to python3's zipfile listing for verification.
  if command -v unzip &> /dev/null; then
    list_zip() { unzip -l "$1"; }
  else
    list_zip() { python3 -m zipfile -l "$1"; }
  fi

  # Verify both binaries made it into the zip before declaring success. List the zip ONCE into a
  # variable and grep the string — NOT `list_zip | grep -q`: under `set -o pipefail`, `grep -q`
  # exits on the first match and closes the pipe, so the lister (unzip / python -m zipfile) gets
  # SIGPIPE (exit 141) while still writing the now sprite-heavy listing, and the pipeline falsely
  # reports the binary "missing". A captured string has no pipe to break.
  zip_listing="$(list_zip "$zip_path")"
  if ! grep -q "bin/office-daemon$BIN_EXT" <<< "$zip_listing"; then
    echo "Error: bin/office-daemon$BIN_EXT missing from $zip_path"
    echo "--- actual zip contents ---"
    echo "$zip_listing"
    exit 1
  fi
  if ! grep -q "bin/workflow-mcp$BIN_EXT" <<< "$zip_listing"; then
    echo "Error: bin/workflow-mcp$BIN_EXT missing from $zip_path"
    echo "--- actual zip contents ---"
    echo "$zip_listing"
    exit 1
  fi

  echo ""
  echo "=== Summary ==="
  echo "Distributable package created:"
  echo "  $zip_path ($size)"
  echo "Zip contents (bin/):"
  list_zip "$zip_path" | grep "bin/" | sed 's/^/  /'
fi
