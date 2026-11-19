# AGENTS

This repo is **MdNotePad** (`MdNotePad.exe`), a native (non-webview) Notepad-style Markdown editor in Rust (no wasm/web build), based on rustdown by teh-hippo. It targets **Windows only**; Linux is used only as a dev/test host.

## Project structure
- `crates/rustdown-gui`: eframe/egui native GUI app (package `rustdown-app`, binary `MdNotePad`)
  - `src/main.rs` — `RustdownApp` (tabs + app-wide settings), `update()` frame order
  - `src/app_identity.rs` — the app's name (`MdNotePad`) and id (`mdnotepad`) in one place: title, About, `--version`, settings / crash-log folders, pipe name, registry plan, file names; `LEGACY_APP_IDS` for settings and registry migration after a rename. A rename also touches `[[bin]] name`, `windows/*.rc` and the workflows' exe paths
  - `src/about.rs` — About box (name, version, copyright, credit) and the embedded LICENSE / THIRD-PARTY-NOTICES.md window
  - `src/tab.rs` — `Tab`: all per-document state and document actions (load/save/conflicts)
  - `src/commands.rs` — `AppCommand`, key bindings, `RustdownApp::execute`
  - `src/title_bar.rs` — custom title bar of the frameless window (icon, tabs, drag region, caption buttons) and the resize edges
  - `src/menu.rs` / `src/tab_strip.rs` — menu row (File · Edit · View, formatting controls, settings gear) and the tabs in the title bar; every popup (menus, submenu, formatting dropdowns, settings, explorer context menus) uses `menu::popup_style` and draws its items with `menu::menu_row` (Windows 11 menu geometry: 30-high rows, lead column for marks / icons, shortcut column, painted submenu arrow)
  - `src/status_bar.rs` — Notepad-style status bar (Ln/Col, characters, mode, zoom, line endings)
  - `src/edit_ops.rs` — Edit-menu commands on the editor (undo, clipboard, find next, time/date) and text-position helpers
  - `src/goto_line.rs` — Go to line dialog
  - `src/ui_style.rs` — fonts (Segoe UI Variable / Segoe UI / Consolas and the icon font on Windows), text sizes, Notepad light/dark themes, chrome colours, text contrast
  - `src/icons.rs` — chrome glyphs from Segoe Fluent Icons / Segoe MDL2 Assets (callers paint line art when neither is installed)
  - `src/app_actions.rs` — tab management, close prompts, mode, zoom, preferences
  - `src/app_panels.rs` — shortcuts, find bar, editor, preview, dialogs
  - `src/format_toolbar.rs` — formatting commands, shortcuts and the formatting controls of the menu row
  - `src/explorer.rs` — explorer panel
  - `src/md/` — markdown parsing and rendering (egui widgets)
  - `src/preferences.rs` — user settings persistence (`%APPDATA%\mdnotepad\settings.toml`; copied from a legacy id's folder on first start)
  - `src/bundled/` — embedded demo and verification markdown files
  - `src/nav/panel.rs` / `src/nav/outline.rs` — outline panel and heading extraction
  - `src/live_format.rs` — the editor's default formatted view: Markdown laid out like the preview, syntax marks hidden (near-zero font size) except on the caret's lines, bullets / checkboxes / quote bars / rules / code backgrounds / table grids painted as overlays, table cells lined up into columns with measured leading space (Edit mode); `src/editor.rs` builds and caches the editor galley for it or for the raw view (Raw and Split modes, and Edit for files over 512 KB: one monospace font, one size, one colour)
  - `src/format.rs` — `.editorconfig`-aware formatter
  - `src/line_ending.rs` — per-document line-ending style (detected on load, CRLF for new docs on Windows); line breaks added while editing are converted to it
  - `src/disk/{io,sync,watcher}.rs` — file I/O and live reload (per tab); a file changed elsewhere while edited asks Save as / Ignore / Reload (no merging)
  - `src/table_edit.rs` — table editing in the editor: right-click Insert / Delete row and column, Tab / Shift+Tab between cells, Enter for a new row; each change rewrites the table lined up, as one undoable edit
  - `src/mermaid.rs` — Mermaid diagrams in Preview: headless Edge draws each ```mermaid block with the bundled library (`assets/mermaid/mermaid.min.js`), the screenshot is cropped and cached as a PNG in `%TEMP%\mdnotepad-mermaid`; the Edit view shows the picture too while the caret is outside the block (`live_format::hide_diagrams` gives its first code row the picture's height, `editor.rs` places it)
  - `src/preview_dll/` — `mdnotepad_preview.dll` (the package's cdylib): File Explorer's preview pane for .md files; `handler.rs` is a hand-written COM preview handler showing the document as RTF (`rtf.rs`, from `md/parse.rs`) in a RichEdit control; registered by Register .md file type when the DLL is next to the exe
  - `src/print.rs` — File → Print… (the document as a web page, printed from the default browser) and Save as PDF… (Microsoft Edge prints the page to a PDF in the background)
  - `src/spell.rs` — View → Spell check: prose ranges, the checker thread, wavy underlines and the editor's right-click menu (suggestions, Add to dictionary, Ignore, Cut/Copy/Paste); `src/win_spell.rs` — Windows' spell checker (`ISpellChecker`) through hand-written COM calls
  - `src/windows_integration.rs` — Windows commands, settings-menu items, single-instance startup glue, DWM styling of the frameless window
  - `src/win_file_assoc.rs` — `.md` association as a pure registry plan (HKCU) + `winreg` apply layer; registering removes legacy ids' entries first
  - `src/win_single_instance.rs` — single instance: protocol, election and server (generic, tested) + named-pipe transport
  - `src/win_ffi.rs` — the Win32 FFI wrappers (Windows only)
  - `src/crash_report.rs` — panic hook / window-creation failure: writes `%LOCALAPPDATA%\mdnotepad\crash.log` and shows an error box
  - `src/test_harness.rs` — headless UI driver for tests (synthetic pointer input, viewport commands, painted text)
  - `build.rs` + `windows/mdnotepad.rc` — exe icon (`windows/mdnotepad.ico`) and version info (Windows targets only); the window takes the same icon (`win_ffi::set_window_icons_from_exe`)
- `installer/` — `mdnotepad.iss` (per-user Inno Setup installer: exe + preview DLL, ticks for the file types and the preview pane, registered through `--register-file-types` / `--register-preview`), `portable.txt` (the portable zip's marker: settings, session and crash log go in `data\` next to the exe, see `app_identity::portable_dir`) and `package.ps1` (builds both, with checksums; run by CI and releases)
- `docs/showcase.md` — the document of the README screenshots (`docs/screenshots/`, made by `screenshots.yml`); `docs/screenshots/demo.gif` is cut from `demo.yml`'s recording (manual run), `social-preview.png` is the repo's GitHub social preview
- `bucket/mdnotepad.json` — Scoop manifest (portable zip); bump its version and hash with each release. `package-managers.yml` (manual) tests it and validates the winget manifest on the winget-pkgs fork's submission branch
- App icon: a blue (#0078D4) notepad outline. Sources in `assets/icon/`: hand-tuned, pixel-aligned `16.svg`, `24.svg`, `32.svg` and `large.svg` (48 px and up; also `assets/icon.svg`). `tools/build-icon.sh` (rsvg-convert + ImageMagick) renders `assets/icon/{16,24,32}.png` (the title bar picks the one closest to 16 px × the display scale), `assets/icon.png` (256 px) and the multi-size `windows/mdnotepad.ico`; never downscale the 256 px render for the small sizes


## Dev commands
Builds, clippy and tests run on GitHub Actions (Windows) for every pushed branch; don't build on the dev machine.
The CI job runs:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --profile ci -p rustdown-app --locked   # exe artifact
cargo test --workspace --locked
```

## Quickstart
```bash
cargo run -p rustdown-app
```

## Conventions
- Prefer simple, explicit code.
- No `unsafe` (enforced). Exceptions: tiny Win32 FFI declarations (`AttachConsole` in `attach_parent_console()`, `GetLocalTime` in `edit_ops::now_local()`), the Win32 wrappers in `win_ffi.rs` (pipe, shell, window, DWM and message-box calls) and the spell checker's COM calls in `win_spell.rs`, each a thin function with `// SAFETY:` comments and a function-scoped `#[allow(unsafe_code)]`; and the preview DLL's COM handler (`src/preview_dll/handler.rs`), which is all FFI and allows `unsafe` for the module.
- Keep allocations/cloning minimal; favor borrowing.
- Avoid `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`dbg!` in non-test code (enforced via workspace clippy lints).
- CI runs with `--locked`; keep `Cargo.lock` committed and up to date.
- User preferences are persisted via `preferences.rs` using `dirs`+`toml`+`serde`. Extend `UserPreferences` for new settings.
