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
if [ ! -f "$INSTALL_DIR/manifest.json" ] || [ ! -f "$INSTALL_DIR/bin/office-daemon" ] || [ ! -f "$INSTALL_DIR/ui/index.html" ]; then
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

    # Add or update the installed_extensions entry for this extension
    jq ".installed_extensions[\"$EXT_ID\"] = {\"enabled\": true}" "$CONFIG_FILE" > "$CONFIG_FILE.tmp"
    mv "$CONFIG_FILE.tmp" "$CONFIG_FILE"

    echo "Configuration updated."
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
