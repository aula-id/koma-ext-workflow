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

# Extension ID and installation directory
EXT_ID="aula.workflow"
INSTALL_DIR="$HOME/.koma/extensions/$EXT_ID"
CONFIG_FILE="$HOME/.koma/config.json"

echo "=== Installing Workflow extension ==="
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
    # Create a backup
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

    # Also register the MCP server (workflow-mcp) in `.mcp_servers` so koma's MCP client
    # spawns it and advertises its tools as mcp__workflow__workflow_*. `mcp_servers` is an
    # ARRAY of McpServerEntry (koma src-agent/src/model/app_config.rs):
    # uuid/name/enabled/transport/command/args/env/url, transport serde wire value "stdio".
    # Upsert by name "workflow", preserving an existing entry's uuid when present (mirrors
    # the installed_extensions upsert above).
    MCP_CMD="$INSTALL_DIR/bin/workflow-mcp"
    MCP_UUID=$(jq -r '(.mcp_servers // []) | map(select(.name == "workflow")) | (.[0].uuid // empty)' "$CONFIG_FILE")
    if [ -z "$MCP_UUID" ]; then
      if command -v uuidgen &> /dev/null; then
        MCP_UUID=$(uuidgen)
      elif command -v python3 &> /dev/null; then
        MCP_UUID=$(python3 -c "import uuid; print(uuid.uuid4())")
      fi
    fi

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

    echo "Configuration updated (extension registry + MCP server)."
  else
    echo "Warning: jq not found. Please manually add the following to ~/.koma/config.json:"
    echo "  \"installed_extensions\": {"
    echo "    \"$EXT_ID\": {\"enabled\": true}"
    echo "  }"
  fi
else
  echo "Warning: $CONFIG_FILE not found. koma may need to be initialized first."
fi

echo ""
echo "=== Installation Complete ==="
echo "The Workflow extension has been installed to: $INSTALL_DIR"
echo ""
echo "Next steps:"
echo "1. Restart koma (if running)"
echo "2. Open koma and look for the 'Workflow' panel tab"
echo "3. Create a new project to get started"
echo ""
