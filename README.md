# helix-picker

A Helix-style picker for VS Code: fuzzy file picker, ranked workspace search and symbol picker, with a preview highlighted like Helix, in a centered overlay above the editor. Not on the Marketplace; the extension is a download on the Releases page.

## Install

Download `helix-picker.vsix` from the latest release and run

    code --install-extension helix-picker.vsix

then "Developer: Reload Window" in open VS Code windows. The package contains the Rust core for Linux x86-64; it needs glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36 or later). The preview is highlighted with the grammars and themes of an installed Helix (snap, package or `~/.config/helix/runtime`); without Helix it shows plain text.

To build it yourself instead, run `./install.sh`. It needs `cargo`, `npm` and the `code` command, builds the Rust core, compiles the extension, packages both into `helix-picker.vsix` and installs it.

## Use

| Command | Key | Opens |
|---|---|---|
| Helix Picker: Files | Ctrl-Alt-a | fuzzy file picker over the workspace folders |
| Helix Picker: Global search | Ctrl-Alt-e | keyword search across the workspace, ranked by relevance |
| Helix Picker: Symbols | Ctrl-Alt-s | symbols from the language servers: the active file's while the query is empty, the workspace's matching the query |
| Helix Picker: Resume last picker | Ctrl-Alt-q | the last picker again, with its query and selection |

To change the keys, open "Preferences: Open Keyboard Shortcuts" and search for `helixPicker`.

The overlay shows the three modes on top (Files, Text, Symbols); click one, press Alt-Left/Alt-Right or Alt-1, Alt-2, Alt-3 to switch, the query stays. Without a workspace folder, files and text search the folder of the active file. Like Helix, hidden files are skipped, `.gitignore`/`.ignore` files are honored and symbolic links are followed.

### Search

The query is a list of keywords; a file matches if every keyword occurs in its content or path. A keyword is matched literally and ignores case unless it contains an uppercase letter. `"a phrase"` matches the words together, `/regex/` is a regular expression. Results are grouped by file: a header with the file's name, folder and number of matching lines, then up to ten of its best lines.

Files are ranked the way Sourcegraph ranks keyword search (BM25F): repeated matches count less and less, matches in short files and rare keywords count more, and matches in the path or on a definition (`fn`, `#define`, a function head, a Markdown heading, ...) count five times. A file that defines a searched name ranks twice as high. Whole words count more than parts of words, matches in comments count half. Backups, `.patch`/`.diff` files, generated, vendored and test code rank lower; files open in VS Code and files next to the one you are working on rank higher. Within a file, lines with more of the keywords come first, then definitions.

### Symbols

Symbols come from the language servers through VS Code (clangd, rust-analyzer, ...), like Helix's symbol pickers: with an empty query the active file's symbols in file order (Helix's `Space s`), otherwise the workspace's symbols matching the query (`Space S`). The language server filters and orders them; exact and prefix name matches, functions and types, and files next to the active one move up, backups, tests and build output down. clangd loads its compile database and index only once a C file is open and finds nothing before that; if no C file is open, the picker opens the first file of the compile database in the background and asks again for a few seconds. Paths in linked folders are shown below the link (`RIOT/...`), not under their real location.

### Keys

Keys inside the picker follow Helix:

| Key | Action |
|---|---|
| Up, Down, Ctrl-p, Ctrl-n, Ctrl-k, Ctrl-j, Shift-Tab, Tab | previous / next result |
| PageUp, PageDown, Ctrl-u, Ctrl-d | page up / down |
| Home, End | first / last result |
| Enter | open in the editor group the picker was opened from |
| Ctrl-v, Ctrl-s | open in a new vertical / horizontal split |
| Ctrl-t | toggle the preview |
| Ctrl-w | delete the word before the cursor |
| Alt-Left, Alt-Right | previous / next mode (files, text, symbols) |
| Alt-1, Alt-2, Alt-3 | switch to files, text, symbols |
| Esc, Ctrl-c | close |

## Settings

- `helixPicker.presentation`: `modal` (default) opens the centered overlay, `tab` opens a tab in the active editor group. The first button in the overlay's header also moves an open picker into a tab.
- `helixPicker.fontSize`: font size of the picker in pixels (default 12).
- `helixPicker.previewTheme`: Helix theme for highlighting. `auto` uses the Helix theme named like the VS Code color theme (Gruvbox Light Hard becomes `gruvbox_light_hard`), otherwise Helix's GitHub light or dark theme.
- `helixPicker.saturation`: factor on the highlight colors' saturation (default 1.4). Theme colors that are already as saturated as the screen allows get a little lighter to make room.
- `helixPicker.boldKeywords`: keywords (`if`, `return`, `struct`, `#include`, `fn`, ...) in bold (default on).
- `helixPicker.helixRuntime`: Helix runtime directory. Empty searches `$HELIX_RUNTIME`, `~/.config/helix/runtime` and the usual install locations, including the snap.

The overlay is VS Code's modal editor (the one Settings opens in). By default it takes 80% of the window, capped at 1400×900 pixels; drag its edges or use its maximize button, and VS Code remembers the size for all modal editors.

## How it works

- `core/` is a Rust program built on the libraries Helix uses for its pickers: nucleo for fuzzy matching, and ignore and grep-searcher from ripgrep for walking and searching the workspace. It runs once per VS Code window and talks JSON lines over stdin/stdout. It only ever sends the slice of results the picker shows.
- Highlighting also happens in the core, with tree-sitter and the grammars, queries and themes of the installed Helix, so the preview and the result lines look exactly like Helix. Without a Helix installation the picker works, unhighlighted.
- `extension/` is the VS Code side in TypeScript. The picker view is a webview.
- The overlay is opened through VS Code's internal `_workbench.openWith` command with the modal editor group id, because the public API cannot address that group. If that ever stops working, the picker falls back to a tab.

## Plan

| Step | Content | Status |
|---|---|---|
| 0 | Spike: modal overlay vs. tab, opening speed | done (removed; see git history) |
| 1 | Rust core, file picker and global search with highlighted preview, Helix picker keys, install script | done |
| 2 | Buffer, symbol and diagnostics pickers, Space leader, key hint panel, Helix keymap | |
| 3 | Window mode in the style of Helix's `Ctrl-w` | |
