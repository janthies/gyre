# helix-picker

A Helix-style picker for VS Code: fuzzy file picker, workspace-wide search with preview, buffer, symbol and diagnostics pickers, opened from a Space leader like in Helix. Not published to the Marketplace; installed from this repo.

## Plan

| Step | Content | Status |
|---|---|---|
| 0 | Spike: can the picker open as a centered modal overlay, and how fast does it open? | in progress (`spike/`) |
| 1 | Rust helper using Helix's own libraries (nucleo for fuzzy matching, ignore and grep-searcher from ripgrep for walking and searching), file picker and global search with preview, Helix picker keys, opening files | |
| 2 | Buffer, symbol and diagnostics pickers, Space leader in VSCodeVim normal mode plus a chord elsewhere, key hint panel | |
| 3 | Window mode in the style of Helix's `Ctrl-w` | |

## Step 0: spike

`spike/` is a throwaway extension that opens the same picker two ways:

- **modal**: as a custom editor in VS Code's modal editor overlay (the one Settings uses). The public API cannot target that group, so the spike calls the internal `_workbench.openWith` with the modal group id `-4`.
- **tab**: as a webview panel in the active editor group, public API only.

Run it in an Extension Development Host window:

    code --extensionDevelopmentPath=$HOME/Documents/vscode-setup/helix-picker/spike ~/thesis

In that window, run "Helix Picker Spike: Open as modal overlay" and "Helix Picker Spike: Open as tab" from the command palette, each twice (the first open is cold). The picker's status line shows how long it took to become ready; every open is also logged to `/tmp/helix-picker-spike.log`.

Keys inside the picker: Up/Down or Ctrl-n/Ctrl-p, PageUp/PageDown, Enter opens the file, Ctrl-v opens it in a split, Esc or Ctrl-c closes. The fuzzy matching in the spike is a simple stand-in for nucleo.
