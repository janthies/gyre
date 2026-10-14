# helix-picker

A Helix-style picker for VS Code: fuzzy file picker and workspace-wide search with a syntax-highlighted preview, in a centered overlay above the editor. Not published to the Marketplace; built and installed from this repo.

## Install

    ./install.sh

Needs `cargo`, `npm` and the `code` command. The script builds the Rust core, compiles the extension, packages both into `helix-picker.vsix` and installs it. Afterwards run "Developer: Reload Window" in open VS Code windows. Run it again after `git pull` to update.

## Use

| Command | Opens |
|---|---|
| Helix Picker: Files | fuzzy file picker over the workspace folders |
| Helix Picker: Global search | regex search across the workspace (smart case) |
| Helix Picker: Resume last picker | the last picker again, with its query and selection |

Without a workspace folder, both search the folder of the active file. Like Helix, hidden files are skipped, `.gitignore`/`.ignore` files are honored and symbolic links are followed.

Keys inside the picker follow Helix:

| Key | Action |
|---|---|
| Up, Down, Ctrl-p, Ctrl-n, Shift-Tab, Tab | previous / next result |
| PageUp, PageDown, Ctrl-u, Ctrl-d | page up / down |
| Home, End | first / last result |
| Enter | open in the editor group the picker was opened from |
| Ctrl-v, Ctrl-s | open in a new vertical / horizontal split |
| Ctrl-t | toggle the preview |
| Ctrl-w | delete the word before the cursor |
| Esc, Ctrl-c | close |

## Settings

- `helixPicker.presentation`: `modal` (default) opens the centered overlay, `tab` opens a tab in the active editor group. The first button in the overlay's header also moves an open picker into a tab.
- `helixPicker.previewTheme`: Shiki theme of the preview. `auto` uses the theme with the same name as the VS Code color theme if Shiki has one (for example Gruvbox Light Hard), otherwise GitHub light or dark.

The overlay is VS Code's modal editor (the one Settings opens in). By default it takes 80% of the window, capped at 1400×900 pixels; drag its edges or use its maximize button, and VS Code remembers the size for all modal editors.

## How it works

- `core/` is a Rust program built on the libraries Helix uses for its pickers: nucleo for fuzzy matching, and ignore and grep-searcher from ripgrep for walking and searching the workspace. It runs once per VS Code window and talks JSON lines over stdin/stdout. It only ever sends the slice of results the picker shows.
- `extension/` is the VS Code side in TypeScript. The picker view is a webview; the preview is highlighted with Shiki, which uses the same TextMate grammars and themes as VS Code.
- The overlay is opened through VS Code's internal `_workbench.openWith` command with the modal editor group id, because the public API cannot address that group. If that ever stops working, the picker falls back to a tab.

## Plan

| Step | Content | Status |
|---|---|---|
| 0 | Spike: modal overlay vs. tab, opening speed | done (removed; see git history) |
| 1 | Rust core, file picker and global search with highlighted preview, Helix picker keys, install script | done |
| 2 | Buffer, symbol and diagnostics pickers, Space leader, key hint panel, Helix keymap | |
| 3 | Window mode in the style of Helix's `Ctrl-w` | |
