#!/bin/bash
set -euo pipefail

# Resolve script directory regardless of CWD
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Check for the zip file
ZIP_FILE="$SCRIPT_DIR/dist/workflow.zip"
if [ ! -f "$ZIP_FILE" ]; then
  echo "Error: $ZIP_FILE not found. Run ./pack.sh first."
  exit 1
fi

# Extension ID and installation directory. koma's dev sideload AND the manual fallback both
# install to ~/.koma/extensions/<id>/, so the MCP command path is the same either way.
EXT_ID="aula.workflow"
INSTALL_DIR="$HOME/.koma/extensions/$EXT_ID"
CONFIG_FILE="$HOME/.koma/config.json"

echo "=== Installing Workflow extension ==="

# Upsert the workflow-mcp stdio server into `.mcp_servers`. Shared by BOTH install paths:
# `koma ext install --dev` registers the extension + grants its `requires` but does NOT register
# an MCP server, and the manual path doesn't either. This registers the "workflow" stdio server so
# koma spawns workflow-mcp and advertises its mcp__workflow__workflow_* tools. `mcp_servers` is an
# ARRAY of McpServerEntry (koma src-agent/src/model/app_config.rs): uuid/name/enabled/transport/
# command/args/env/url, transport serde wire value "stdio". Upsert by name "workflow", preserving
# an existing entry's uuid. The caller is responsible for backing up CONFIG_FILE once beforehand.
register_mcp_server() {
  if [ ! -f "$CONFIG_FILE" ]; then
    echo "Warning: $CONFIG_FILE not found; skipping MCP server registration (initialize koma first)."
    return 0
  fi
  if ! command -v jq &> /dev/null; then
    echo "Warning: jq not found; add the 'workflow' stdio server ($INSTALL_DIR/bin/workflow-mcp) to ~/.koma/config.json manually."
    return 0
  fi

  local MCP_CMD="$INSTALL_DIR/bin/workflow-mcp"
  local MCP_UUID
  MCP_UUID=$(jq -r '(.mcp_servers // []) | map(select(.name == "workflow")) | (.[0].uuid // empty)' "$CONFIG_FILE")
  if [ -z "$MCP_UUID" ]; then
    if command -v uuidgen &> /dev/null; then
      MCP_UUID=$(uuidgen)
    elif command -v python3 &> /dev/null; then
      MCP_UUID=$(python3 -c "import uuid; print(uuid.uuid4())")
    fi
  fi

  local MCP_ENTRY
  if [ -n "$MCP_UUID" ]; then
    MCP_ENTRY=$(jq -cn --arg uuid "$MCP_UUID" --arg cmd "$MCP_CMD" '{
      uuid: $uuid,
      name: "workflow",
      enabled: true,
      transport: "stdio",
      command: $cmd,
      args: [],
      env: [],
      url: ""
    }')
  else
    # No uuid tool available: omit uuid and let koma mint one (serde default new_uuid).
    MCP_ENTRY=$(jq -cn --arg cmd "$MCP_CMD" '{
      name: "workflow",
      enabled: true,
      transport: "stdio",
      command: $cmd,
      args: [],
      env: [],
      url: ""
    }')
  fi

  jq --argjson entry "$MCP_ENTRY" \
    '.mcp_servers = ((.mcp_servers // []) | map(select(.name != $entry.name))) + [$entry]' \
    "$CONFIG_FILE" > "$CONFIG_FILE.tmp"
  mv "$CONFIG_FILE.tmp" "$CONFIG_FILE"
  echo "MCP server 'workflow' registered ($MCP_CMD)."
}

# ---------------------------------------------------------------------------
# Preferred path: koma's official dev sideload verb.
#
# `koma ext install --dev <zip>` verifies+unpacks the unsigned zip into
# ~/.koma/extensions/<id>/ (same dir as INSTALL_DIR), AUTO-GRANTS every `requires`, sets tier
# "dev" + enabled true, and replaces any existing entry with this id in place — so we SKIP the
# manual unzip + installed_extensions jq entirely. We still register the MCP server ourselves
# (the --dev verb does not do that).
# ---------------------------------------------------------------------------
if command -v koma &> /dev/null; then
  echo "Using 'koma ext install --dev' (auto-grants requires, tier dev, replaces $EXT_ID in place)."
  koma ext install --dev "$ZIP_FILE"

  if [ -f "$CONFIG_FILE" ] && command -v jq &> /dev/null; then
    cp "$CONFIG_FILE" "$CONFIG_FILE.bak"
  fi
  register_mcp_server

  echo ""
  echo "=== Installation Complete ==="
  echo "Installed via 'koma ext install --dev' to: $INSTALL_DIR"
  echo ""
  echo "Reminder: already-running koma sessions must be RESTARTED to pick up this build."
  echo "Then open koma and look for the 'Workflow' panel tab."
  echo ""
  exit 0
fi

# ---------------------------------------------------------------------------
# Fallback: full manual install (koma not on PATH). Unzip into the extensions dir and edit the
# config registry (installed_extensions + mcp_servers) by hand.
# ---------------------------------------------------------------------------
echo "koma not on PATH — falling back to manual install."
echo "Installing to: $INSTALL_DIR"

# Create the installation directory
mkdir -p "$INSTALL_DIR"

# Remove any existing installation
if [ -d "$INSTALL_DIR" ]; then
  rm -rf "$INSTALL_DIR"
  mkdir -p "$INSTALL_DIR"
fi

# Extract the zip file
unzip -q "$ZIP_FILE" -d "$INSTALL_DIR"

echo "Extension files extracted."

# Verify the installation
if [ ! -f "$INSTALL_DIR/manifest.json" ] || [ ! -f "$INSTALL_DIR/bin/office-daemon" ] || [ ! -f "$INSTALL_DIR/bin/workflow-mcp" ] || [ ! -f "$INSTALL_DIR/ui/index.html" ]; then
  echo "Error: Installation verification failed. Missing required files."
  exit 1
fi

echo "Installation verified."

# Update the config file if it exists
if [ -f "$CONFIG_FILE" ]; then
  echo "Updating koma configuration..."

  # Check if jq is available
  if command -v jq &> /dev/null; then
    # Create a single pristine backup covering both the registry + MCP upserts below.
    cp "$CONFIG_FILE" "$CONFIG_FILE.bak"

    # `installed_extensions` is an ARRAY of InstalledExtension records (see koma
    # src-agent/src/model/app_config.rs), NOT a map keyed by id. Build the full
    # record from the manifest and upsert it: drop any existing entry with this
    # id, then append.
    ENTRY=$(jq -c '{
      id: .id,
      version: .version,
      tier: (.tier // "free"),
      granted: (.requires // []),
      enabled: true,
      kind: (.kind // "daemon"),
      exec: (.runtime.exec // "bin/office-daemon")
    }' "$INSTALL_DIR/manifest.json")

    jq --argjson entry "$ENTRY" \
      '.installed_extensions = ((.installed_extensions // []) | map(select(.id != $entry.id))) + [$entry]' \
      "$CONFIG_FILE" > "$CONFIG_FILE.tmp"
    mv "$CONFIG_FILE.tmp" "$CONFIG_FILE"

    # Register the MCP server too (the manual path, like the --dev verb, otherwise wouldn't).
    register_mcp_server

    echo "Configuration updated (extension registry + MCP server)."
  else
    echo "Warning: jq not found. Please manually add the following to ~/.koma/config.json:"
    echo "  \"installed_extensions\": [ { \"id\": \"$EXT_ID\", \"enabled\": true, ... } ]"
    echo "  and a \"workflow\" stdio server under \"mcp_servers\"."
  fi
else
  echo "Warning: $CONFIG_FILE not found. koma may need to be initialized first."
fi

echo ""
echo "=== Installation Complete ==="
echo "The Workflow extension has been installed to: $INSTALL_DIR"
echo ""
echo "Next steps:"
echo "1. Restart koma — already-running sessions must be restarted to pick up this build."
echo "2. Open koma and look for the 'Workflow' panel tab"
echo "3. Create a new project to get started"
echo ""
