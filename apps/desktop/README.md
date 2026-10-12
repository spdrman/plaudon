# Desktop application

`src/` is the React and TypeScript user interface; `src-tauri/` is the Tauri
command layer. Owned by the desktop shell work.

This file exists so the directory is part of the committed tree that
`tests/integration/tests/workspace_layout.rs` checks: git does not track
empty directories, so a layout assertion that reads the filesystem would
otherwise pass only in a working tree that happens to have them.
