//! Explorer side panel (folder / file tree) on the left.
//!
//! - `explorer_fs.rs`: pure helpers (sorting, filters, listing, names, paths).
//! - `explorer_tree.rs`: the lazy tree model, background listing, watcher.
//! - `explorer_ui.rs`: drawing (header, rows, icons, context menus).
//!
//! This module holds the commands, the app-level state and the glue with
//! tabs, menus and preferences.

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use serde::{Deserialize, Serialize};

use crate::{
    RustdownApp,
    commands::AppCommand,
    explorer_fs::{
        check_text_file, create_untitled_markdown, display_path, normalize_root,
        reveal_in_file_explorer,
    },
    explorer_tree::ExplorerTree,
    explorer_ui, is_markdown_path,
    tab::TabId,
};

const PANEL_DEFAULT_WIDTH: f32 = 240.0;
const PANEL_MIN_WIDTH: f32 = 160.0;
const PANEL_MAX_WIDTH: f32 = 600.0;

/// Folder tree commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExplorerCommand {
    /// File → Open folder… (folder picker).
    OpenFolderDialog,
    /// File → Close folder.
    CloseFolder,
    /// View → Explorer.
    TogglePanel,
    /// Show every file, not just Markdown.
    ToggleShowAllFiles,
    /// Re-read every shown folder.
    Refresh,
    CollapseAll,
    /// Expand or collapse a folder row.
    ToggleExpanded(PathBuf),
    /// Open a file in a tab (or focus its tab).
    OpenFile(PathBuf),
    /// Create `Untitled.md` (`Untitled 2.md`, …) in a folder and open it.
    NewMarkdownFile(PathBuf),
    CopyPath(PathBuf),
    RevealInFileExplorer(PathBuf),
    /// Re-read one folder.
    RefreshFolder(PathBuf),
}

/// Key bindings for [`ExplorerCommand`]; merged into `commands::shortcut_table`.
pub(crate) fn explorer_shortcuts() -> Vec<(KeyboardShortcut, AppCommand)> {
    vec![(
        KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::E),
        AppCommand::Explorer(ExplorerCommand::TogglePanel),
    )]
}

/// Explorer settings persisted in `settings.toml` (`[explorer]` table).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExplorerPreferences {
    /// The panel is shown (View → Explorer).
    pub visible: bool,
    /// The open folder; restored at startup if it still exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    /// Show every file, not just Markdown.
    pub show_all_files: bool,
}

/// App-level explorer state (the panel's visibility is
/// `RustdownApp::show_explorer`).
#[derive(Default)]
pub(crate) struct ExplorerState {
    /// The open folder's tree; `None` when no folder is open.
    pub(crate) tree: Option<ExplorerTree>,
    pub(crate) show_all_files: bool,
    /// Active tab (id and path) the tree last revealed; the tree re-syncs
    /// when either changes.
    synced: Option<(TabId, Option<PathBuf>)>,
}

impl RustdownApp {
    /// Run an [`ExplorerCommand`].
    pub(crate) fn execute_explorer(&mut self, cmd: ExplorerCommand, ctx: &egui::Context) {
        match cmd {
            ExplorerCommand::OpenFolderDialog => {
                let mut dialog = rfd::FileDialog::new();
                if let Some(start) = self.folder_dialog_start() {
                    dialog = dialog.set_directory(start);
                }
                if let Some(dir) = dialog.pick_folder() {
                    self.open_folder(&dir);
                    self.save_preferences();
                }
            }
            ExplorerCommand::CloseFolder => {
                self.close_folder();
                self.save_preferences();
            }
            ExplorerCommand::TogglePanel => {
                self.show_explorer = !self.show_explorer;
                self.explorer.synced = None;
                self.save_preferences();
            }
            ExplorerCommand::ToggleShowAllFiles => {
                self.explorer.show_all_files = !self.explorer.show_all_files;
                self.save_preferences();
            }
            ExplorerCommand::Refresh => {
                if let Some(tree) = self.explorer.tree.as_mut() {
                    tree.refresh_all();
                }
            }
            ExplorerCommand::CollapseAll => {
                if let Some(tree) = self.explorer.tree.as_mut() {
                    tree.collapse_all();
                }
            }
            ExplorerCommand::ToggleExpanded(dir) => {
                if let Some(tree) = self.explorer.tree.as_mut() {
                    tree.toggle_expanded(&dir);
                }
            }
            ExplorerCommand::OpenFile(path) => self.open_from_explorer(path),
            ExplorerCommand::NewMarkdownFile(dir) => self.new_markdown_file(&dir),
            ExplorerCommand::CopyPath(path) => ctx.copy_text(display_path(&path)),
            ExplorerCommand::RevealInFileExplorer(path) => {
                if let Err(err) = reveal_in_file_explorer(&path) {
                    self.tab_mut().error = Some(format!("Couldn't open File Explorer: {err}"));
                }
            }
            ExplorerCommand::RefreshFolder(dir) => {
                if let Some(tree) = self.explorer.tree.as_mut() {
                    tree.refresh_dir(&dir);
                }
            }
        }
    }

    /// Open a launch / forwarded path: a folder becomes the explorer root,
    /// anything else opens in a tab (or focuses the tab that has it).
    pub(crate) fn open_path_or_folder(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.open_folder(&path);
        } else {
            self.open_path(path);
        }
    }

    /// Show `dir` as the explorer root (and show the panel).
    pub(crate) fn open_folder(&mut self, dir: &Path) {
        let root = normalize_root(dir);
        if !root.is_dir() {
            self.tab_mut().error = Some(format!(
                "Open folder failed: {} is not a folder",
                display_path(&root)
            ));
            return;
        }
        self.explorer.tree = Some(ExplorerTree::new(root));
        self.explorer.synced = None;
        self.show_explorer = true;
    }

    pub(crate) fn close_folder(&mut self) {
        self.explorer.tree = None;
        self.explorer.synced = None;
    }

    /// Startup: apply saved explorer settings.  A folder given on the
    /// command line wins over the saved root and shows the panel; the saved
    /// root is reopened only if it still exists.
    pub(crate) fn restore_explorer(&mut self, prefs: &ExplorerPreferences, folder: Option<&Path>) {
        self.explorer.show_all_files = prefs.show_all_files;
        self.show_explorer = prefs.visible;
        if let Some(folder) = folder {
            self.open_folder(folder);
        } else if let Some(root) = prefs.root.as_ref().filter(|root| root.is_dir()) {
            self.open_folder(root);
            self.show_explorer = prefs.visible;
        }
    }

    /// Current explorer settings, for `save_preferences`.
    pub(crate) fn explorer_preferences(&self) -> ExplorerPreferences {
        ExplorerPreferences {
            visible: self.show_explorer,
            root: self
                .explorer
                .tree
                .as_ref()
                .map(|tree| tree.root().to_path_buf()),
            show_all_files: self.explorer.show_all_files,
        }
    }

    /// Where the folder picker starts: the open folder, else the active
    /// file's folder.
    fn folder_dialog_start(&self) -> Option<PathBuf> {
        self.explorer
            .tree
            .as_ref()
            .map(|tree| tree.root().to_path_buf())
            .or_else(|| {
                self.tab()
                    .doc
                    .path
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
            })
    }

    /// Open a file clicked in the tree.  Markdown always opens; other files
    /// only if they look like text and are under the size limit, otherwise
    /// the reason goes to the active tab's error banner.  A row whose file
    /// was deleted since the folder was read refreshes the folder instead
    /// of opening an empty document.
    pub(crate) fn open_from_explorer(&mut self, path: PathBuf) {
        if !path.is_file() {
            self.tab_mut().error = Some(format!(
                "{} no longer exists",
                path.file_name()
                    .map_or_else(|| display_path(&path), |n| n.to_string_lossy().into_owned())
            ));
            if let (Some(tree), Some(parent)) = (self.explorer.tree.as_mut(), path.parent()) {
                tree.refresh_dir(parent);
            }
            return;
        }
        if !is_markdown_path(&path)
            && let Err(message) = check_text_file(&path)
        {
            self.tab_mut().error = Some(message);
            return;
        }
        self.open_path(path);
    }

    fn new_markdown_file(&mut self, dir: &Path) {
        match create_untitled_markdown(dir) {
            Ok(path) => {
                if let Some(tree) = self.explorer.tree.as_mut() {
                    tree.expand(dir);
                    tree.refresh_dir(dir);
                }
                self.open_path(path);
            }
            Err(err) => {
                self.tab_mut().error = Some(format!(
                    "Couldn't create a file in {}: {err}",
                    display_path(dir)
                ));
            }
        }
    }

    /// Highlight the active tab's file in the tree (expanding its folders
    /// and scrolling to it) whenever the active tab or its path changes.
    fn sync_explorer_with_active_tab(&mut self) {
        let tab = &self.tabs[self.active];
        if let Some((id, path)) = &self.explorer.synced
            && *id == tab.id
            && path.as_deref() == tab.doc.path.as_deref()
        {
            return;
        }
        let path = tab.doc.path.clone();
        self.explorer.synced = Some((tab.id, path.clone()));
        if let Some(tree) = self.explorer.tree.as_mut() {
            let target = path.as_deref().and_then(|path| tree.tree_path_for(path));
            tree.reveal(target);
        }
    }

    /// Apply background listings / watcher events; runs every frame, also
    /// while the panel is hidden, so the cache stays fresh.
    fn poll_explorer(&mut self, ctx: &egui::Context) {
        let Some(tree) = self.explorer.tree.as_mut() else {
            return;
        };
        // Folders can be renamed or deleted while the app is in the
        // background; Windows does not report that for the watched root.
        let regained_focus = ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::WindowFocused(true)))
        });
        if regained_focus {
            tree.check_root();
        }
        if let Some(wake) = tree.poll(Some(ctx), Instant::now()) {
            ctx.request_repaint_after(wake);
        }
    }

    /// File menu: "Open folder…" and "Close folder".
    pub(crate) fn explorer_file_menu_items(&mut self, ui: &mut egui::Ui) {
        self.menu_item(
            ui,
            "Open folder…",
            AppCommand::Explorer(ExplorerCommand::OpenFolderDialog),
        );
        let has_folder = self.explorer.tree.is_some();
        ui.add_enabled_ui(has_folder, |ui| {
            self.menu_item(
                ui,
                "Close folder",
                AppCommand::Explorer(ExplorerCommand::CloseFolder),
            );
        });
    }

    /// View menu: the panel toggle and the "show all files" filter.
    pub(crate) fn explorer_view_menu_items(&mut self, ui: &mut egui::Ui) {
        self.menu_toggle(
            ui,
            "Explorer",
            self.show_explorer,
            AppCommand::Explorer(ExplorerCommand::TogglePanel),
        );
        self.menu_toggle(
            ui,
            "Show all files in Explorer",
            self.explorer.show_all_files,
            AppCommand::Explorer(ExplorerCommand::ToggleShowAllFiles),
        );
    }

    /// The left side panel, shown while `show_explorer` is set.
    pub(crate) fn show_explorer_panel(&mut self, ctx: &egui::Context) {
        self.poll_explorer(ctx);
        if !self.show_explorer {
            return;
        }
        self.sync_explorer_with_active_tab();
        let mut commands = Vec::new();
        egui::SidePanel::left("explorer")
            .resizable(true)
            .default_width(PANEL_DEFAULT_WIDTH)
            .width_range(PANEL_MIN_WIDTH..=PANEL_MAX_WIDTH)
            .frame(explorer_ui::panel_frame(&ctx.style()))
            .show(ctx, |ui| {
                explorer_ui::panel_contents(ui, &mut self.explorer, &mut commands);
            });
        for cmd in commands {
            self.execute_explorer(cmd, ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, time::Duration};

    use super::*;
    use crate::{explorer_fs::test_temp_dir, explorer_tree::RowKind};

    fn run(app: &mut RustdownApp, cmd: ExplorerCommand) {
        let ctx = egui::Context::default();
        app.execute(AppCommand::Explorer(cmd), &ctx);
    }

    fn wait_for_tree(app: &mut RustdownApp) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let show_all = app.explorer.show_all_files;
        if let Some(tree) = app.explorer.tree.as_mut() {
            loop {
                let _ = tree.poll(None, Instant::now());
                tree.prepare_rows(show_all);
                if !tree.is_loading() || Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn row_names(app: &RustdownApp) -> Vec<String> {
        app.explorer
            .tree
            .as_ref()
            .map(|tree| tree.rows().iter().map(|row| row.name.clone()).collect())
            .unwrap_or_default()
    }

    fn sample_dir(name: &str) -> PathBuf {
        let dir = test_temp_dir(name);
        let _ = fs::create_dir_all(dir.join("docs"));
        let _ = fs::write(dir.join("a.md"), "# A\n");
        let _ = fs::write(dir.join("docs").join("b.md"), "# B\n");
        let _ = fs::write(dir.join("notes.txt"), "plain\n");
        let _ = fs::write(dir.join("image.png"), [0x89, b'P', b'N', b'G', 0, 0]);
        dir
    }

    #[test]
    fn shortcut_toggles_the_panel() {
        let cmd = AppCommand::Explorer(ExplorerCommand::TogglePanel);
        let shortcut = cmd.shortcut();
        assert_eq!(
            shortcut,
            Some(KeyboardShortcut::new(
                Modifiers::COMMAND.plus(Modifiers::SHIFT),
                Key::E
            ))
        );
        let mut app = RustdownApp::default();
        assert!(!app.show_explorer);
        run(&mut app, ExplorerCommand::TogglePanel);
        assert!(app.show_explorer);
        run(&mut app, ExplorerCommand::TogglePanel);
        assert!(!app.show_explorer);
    }

    #[test]
    fn open_and_close_folder() {
        let dir = sample_dir("app-open");
        let mut app = RustdownApp::default();
        app.open_folder(&dir);
        assert!(app.show_explorer, "opening a folder shows the panel");
        assert_eq!(
            app.explorer
                .tree
                .as_ref()
                .map(|tree| tree.root().to_path_buf()),
            Some(dir.clone())
        );
        wait_for_tree(&mut app);
        assert_eq!(row_names(&app), ["docs", "a.md"]);
        assert_eq!(app.explorer_preferences().root, Some(dir.clone()));

        run(&mut app, ExplorerCommand::ToggleShowAllFiles);
        wait_for_tree(&mut app);
        assert_eq!(row_names(&app), ["docs", "a.md", "image.png", "notes.txt"]);

        run(&mut app, ExplorerCommand::CloseFolder);
        assert!(app.explorer.tree.is_none());
        assert_eq!(app.explorer_preferences().root, None);

        // Not a folder: error banner, nothing opened.
        app.open_folder(&dir.join("a.md"));
        assert!(app.explorer.tree.is_none());
        assert!(
            app.tab()
                .error
                .as_deref()
                .is_some_and(|e| e.contains("not a folder"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clicking_files_opens_tabs_with_text_checks() {
        let dir = sample_dir("app-files");
        let mut app = RustdownApp::default();
        app.open_folder(&dir);

        run(&mut app, ExplorerCommand::OpenFile(dir.join("a.md")));
        assert_eq!(app.tabs.len(), 1, "replaces the untouched Untitled tab");
        assert_eq!(app.tab().doc.text.as_str(), "# A\n");

        run(&mut app, ExplorerCommand::OpenFile(dir.join("notes.txt")));
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tab().doc.text.as_str(), "plain\n");

        // Opening an already-open file focuses its tab.
        run(&mut app, ExplorerCommand::OpenFile(dir.join("a.md")));
        assert_eq!((app.tabs.len(), app.active), (2, 0));

        // Binary file: refused with a message on the active tab's banner.
        run(&mut app, ExplorerCommand::OpenFile(dir.join("image.png")));
        assert_eq!(app.tabs.len(), 2);
        assert!(
            app.tab()
                .error
                .as_deref()
                .is_some_and(|e| e.contains("image.png")),
            "{:?}",
            app.tab().error
        );

        // A file deleted since the listing: no empty document is created.
        run(&mut app, ExplorerCommand::OpenFile(dir.join("deleted.md")));
        assert_eq!(app.tabs.len(), 2);
        assert!(
            app.tab()
                .error
                .as_deref()
                .is_some_and(|e| e.contains("deleted.md no longer exists"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_markdown_file_creates_unique_names_and_opens_them() {
        let dir = sample_dir("app-new");
        let mut app = RustdownApp::default();
        app.open_folder(&dir);
        wait_for_tree(&mut app);
        let docs = dir.join("docs");
        run(&mut app, ExplorerCommand::NewMarkdownFile(docs.clone()));
        run(&mut app, ExplorerCommand::NewMarkdownFile(docs.clone()));
        assert!(docs.join("Untitled.md").is_file());
        assert!(docs.join("Untitled 2.md").is_file());
        assert_eq!(
            app.tab().doc.path.as_deref(),
            Some(docs.join("Untitled 2.md").as_path())
        );
        assert!(
            app.explorer
                .tree
                .as_ref()
                .is_some_and(|tree| tree.is_expanded(&docs))
        );
        wait_for_tree(&mut app);
        let names = row_names(&app);
        assert!(names.contains(&"Untitled 2.md".to_owned()), "{names:?}");

        run(
            &mut app,
            ExplorerCommand::NewMarkdownFile(dir.join("missing")),
        );
        assert!(
            app.tab()
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Couldn't create"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn active_tab_is_highlighted_and_revealed() {
        let dir = sample_dir("app-reveal");
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        let mut app = RustdownApp::default();
        app.open_folder(&dir);
        let nested = dir.join("docs").join("b.md");
        app.open_path(nested.clone());

        let frame = |app: &mut RustdownApp| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                app.show_explorer_panel(ctx);
                egui::CentralPanel::default().show(ctx, |_| {});
            });
        };
        frame(&mut app);
        wait_for_tree(&mut app);
        frame(&mut app);
        let tree = app.explorer.tree.as_ref();
        assert!(tree.is_some_and(|tree| tree.is_expanded(&dir.join("docs"))));
        assert_eq!(
            tree.and_then(ExplorerTree::selected),
            Some(nested.as_path())
        );
        let rows: Vec<(String, RowKind)> = tree
            .map(|tree| {
                tree.rows()
                    .iter()
                    .map(|r| (r.name.clone(), r.kind.clone()))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            rows[0],
            ("docs".to_owned(), RowKind::Folder { expanded: true })
        );
        assert_eq!(rows[1].0, "b.md");

        // Switching to a tab outside the root clears the highlight.
        app.new_tab();
        app.tab_mut().doc.text = Arc::new("x".to_owned());
        frame(&mut app);
        assert_eq!(
            app.explorer.tree.as_ref().and_then(ExplorerTree::selected),
            None
        );

        // The panel also renders with no folder and with a missing root.
        run(&mut app, ExplorerCommand::CloseFolder);
        frame(&mut app);
        app.open_folder(&dir);
        let _ = fs::remove_dir_all(&dir);
        if let Some(tree) = app.explorer.tree.as_mut() {
            tree.check_root();
        }
        assert!(
            app.explorer
                .tree
                .as_ref()
                .is_some_and(ExplorerTree::root_missing)
        );
        frame(&mut app);
    }

    #[test]
    fn preferences_restore_root_only_if_it_exists() {
        let dir = sample_dir("app-prefs");
        let prefs = ExplorerPreferences {
            visible: false,
            root: Some(dir.clone()),
            show_all_files: true,
        };
        let mut app = RustdownApp::default();
        app.restore_explorer(&prefs, None);
        assert!(app.explorer.tree.is_some());
        assert!(!app.show_explorer, "a hidden panel stays hidden");
        assert!(app.explorer.show_all_files);
        assert_eq!(app.explorer_preferences(), prefs);

        let gone = ExplorerPreferences {
            visible: true,
            root: Some(dir.join("gone")),
            show_all_files: false,
        };
        let mut app = RustdownApp::default();
        app.restore_explorer(&gone, None);
        assert!(app.explorer.tree.is_none());
        assert!(app.show_explorer);

        // A folder from the command line wins and shows the panel.
        let mut app = RustdownApp::default();
        app.restore_explorer(&gone, Some(&dir.join("docs")));
        assert_eq!(
            app.explorer
                .tree
                .as_ref()
                .map(|tree| tree.root().to_path_buf()),
            Some(dir.join("docs"))
        );
        assert!(app.show_explorer);

        let toml = toml::to_string(&prefs).unwrap_or_default();
        let back: ExplorerPreferences = toml::from_str(&toml).unwrap_or_default();
        assert_eq!(back, prefs);
        let empty: ExplorerPreferences = toml::from_str("").unwrap_or_default();
        assert_eq!(empty, ExplorerPreferences::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_argument_opens_the_explorer_in_edit_mode() {
        let dir = sample_dir("app-cli");
        let options = crate::cli::parse_launch_options([dir.as_os_str().to_owned()]);
        assert_eq!(
            options.mode,
            crate::Mode::Edit,
            "a folder is not a document"
        );
        let app = RustdownApp::from_launch_options(options);
        assert_eq!(
            app.explorer
                .tree
                .as_ref()
                .map(|tree| tree.root().to_path_buf()),
            Some(dir.clone())
        );
        assert!(app.show_explorer);
        assert!(app.tab().doc.path.is_none(), "no tab for the folder");
        assert!(app.tab().error.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_path_and_reveal_do_not_touch_tabs() {
        let dir = sample_dir("app-copy");
        let mut app = RustdownApp::default();
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.execute(
                AppCommand::Explorer(ExplorerCommand::CopyPath(dir.join("a.md"))),
                ctx,
            );
        });
        run(
            &mut app,
            ExplorerCommand::RevealInFileExplorer(dir.join("a.md")),
        );
        assert!(app.tab().error.is_none());
        assert_eq!(app.tabs.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_paths_route_folders_to_explorer_and_files_to_tabs() {
        let dir = sample_dir("app-route");
        let mut app = RustdownApp::default();
        app.open_path_or_folder(dir.clone());
        assert!(app.show_explorer, "a folder opens the explorer");
        assert_eq!(app.explorer_preferences().root, Some(dir.clone()));
        assert_eq!(app.tabs.len(), 1, "a folder does not open a tab");

        app.open_path_or_folder(dir.join("a.md"));
        assert!(app.tab().doc.path.as_deref() == Some(dir.join("a.md").as_path()));
        let _ = fs::remove_dir_all(&dir);
    }
}
