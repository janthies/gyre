#!/usr/bin/env bash
# Builds the Rust core and the extension, packages both into gyre.vsix and installs it
# into VS Code. Needs cargo, npm and the `code` command.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

echo "== core"
cargo build --release --manifest-path core/Cargo.toml
mkdir -p extension/bin
cp core/target/release/hxp-core extension/bin/

# vsce needs a newer Node than many distributions ship; VS Code's own Electron runs as Node.
VSCODE_NODE="$(dirname "$(readlink -f "$(command -v code)")")/../code"

echo "== extension"
cd extension
npm install --no-audit --no-fund --loglevel=error
npm run compile
ELECTRON_RUN_AS_NODE=1 "$VSCODE_NODE" node_modules/@vscode/vsce/vsce package --allow-missing-repository --skip-license --out ../gyre.vsix
cd ..

echo "== install"
code --install-extension gyre.vsix --force
echo "Installed. Run 'Developer: Reload Window' in open VS Code windows to load the new version."
