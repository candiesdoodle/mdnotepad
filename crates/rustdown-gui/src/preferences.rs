use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::app_identity::{APP_ID, LEGACY_APP_IDS};

const SETTINGS_FILE: &str = "settings.toml";

/// Minimum heading count before nav is auto-shown in Preview/SideBySide.
pub const AUTO_NAV_MIN_HEADINGS: usize = 5;

/// User preferences persisted between sessions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct UserPreferences {
    pub nav_visible: bool,
    pub heading_color_mode: bool,
    pub side_by_side_scroll_sync: bool,
    pub zoom_factor: f32,
    /// The mode last chosen (View menu, status bar, Ctrl+Enter).
    pub mode: String,
    /// Settings → Start in: the mode to start in and open Markdown files
    /// in, or `"last"` (also when empty or unknown) for [`Self::mode`].
    pub start_mode: String,
    pub auto_save: bool,
    /// View → Theme: `"light"`, `"dark"` or `"system"` (see `ui_style::ThemeChoice`).
    pub theme: String,
    /// View → Word wrap.
    pub word_wrap: bool,
    /// View → Spell check.
    pub spell_check: bool,
    /// View → Status bar.
    pub show_status_bar: bool,
    /// View → Formatting toolbar.
    pub show_format_toolbar: bool,
    /// Settings → Close window with last tab: closing the last tab closes
    /// the window (Notepad), or leaves an empty Untitled tab.
    pub close_window_with_last_tab: bool,
    /// File → Recent files, newest first.
    pub recent_files: Vec<String>,
    /// The window's top-left corner, in logical pixels (not while
    /// maximized).
    pub window_position: Option<[f32; 2]>,
    /// The window's inner size, in logical pixels (not while maximized).
    pub window_size: Option<[f32; 2]>,
    /// Whether the window was maximized.
    pub window_maximized: bool,
    /// Explorer panel settings (`[explorer]` table).
    #[serde(default)]
    pub explorer: crate::explorer::ExplorerPreferences,
    /// Before 1.0: View → Markdown syntax (the editor showed the source).
    /// Only read from old files, never written: [`Self::from_toml`] turns
    /// Edit with it on into the Raw mode.
    #[serde(default, skip_serializing)]
    pub markdown_syntax: bool,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            nav_visible: false,
            heading_color_mode: true,
            side_by_side_scroll_sync: true,
            zoom_factor: 1.0,
            mode: String::new(),
            start_mode: String::new(),
            auto_save: true,
            theme: crate::ui_style::ThemeChoice::default().as_str().to_owned(),
            word_wrap: true,
            spell_check: true,
            show_status_bar: true,
            show_format_toolbar: true,
            close_window_with_last_tab: false,
            recent_files: Vec::new(),
            window_position: None,
            window_size: None,
            window_maximized: false,
            explorer: crate::explorer::ExplorerPreferences::default(),
            markdown_syntax: false,
        }
    }
}

impl UserPreferences {
    /// Load preferences from the standard config path, falling back to
    /// defaults on any error (missing file, parse error, etc.).
    #[must_use]
    pub fn load() -> Self {
        if crate::app_identity::portable_dir().is_none()
            && let Some(dir) = dirs::config_dir()
        {
            let _ = migrate_legacy_settings(&dir, APP_ID, LEGACY_APP_IDS);
        }
        let Some(path) = config_file_path() else {
            return Self::default();
        };
        let Ok(contents) = fs::read_to_string(&path) else {
            return Self::default();
        };
        Self::from_toml(&contents)
    }

    /// Preferences from the settings file's `contents` (defaults on a parse
    /// error), with old settings brought up to date: Edit with the old
    /// Markdown syntax switch on (or no mode saved yet) starts in Raw.
    #[must_use]
    pub fn from_toml(contents: &str) -> Self {
        let mut prefs: Self = toml::from_str(contents).unwrap_or_default();
        if std::mem::take(&mut prefs.markdown_syntax) && matches!(prefs.mode.as_str(), "edit" | "")
        {
            "raw".clone_into(&mut prefs.mode);
        }
        prefs
    }

    /// Persist preferences to the standard config path.
    /// Errors are silently ignored — preferences are best-effort.
    pub fn save(&self) {
        let Some(path) = config_file_path() else {
            return;
        };
        let Ok(contents) = toml::to_string_pretty(self) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, contents);
    }
}

/// Returns the path to `settings.toml` inside the platform config directory,
/// in a folder named after [`APP_ID`]:
///
/// - Linux: `~/.config/<app id>/settings.toml`
/// - Windows: `{FOLDERID_RoamingAppData}\<app id>\settings.toml`
///
/// In portable mode, `settings.toml` in the portable folder.
pub(crate) fn config_file_path() -> Option<PathBuf> {
    if let Some(dir) = crate::app_identity::portable_dir() {
        return Some(dir.join(SETTINGS_FILE));
    }
    dirs::config_dir().map(|dir| settings_path(&dir, APP_ID))
}

/// The settings file of the app with id `app_id` in `config_dir`.
fn settings_path(config_dir: &Path, app_id: &str) -> PathBuf {
    config_dir.join(app_id).join(SETTINGS_FILE)
}

/// After a rename: when `app_id` has no settings file in `config_dir` yet,
/// copy the one of the first of `legacy_ids` that has one (the old file
/// stays).  Returns the file copied.
fn migrate_legacy_settings(
    config_dir: &Path,
    app_id: &str,
    legacy_ids: &[&str],
) -> Option<PathBuf> {
    let target = settings_path(config_dir, app_id);
    if target.exists() {
        return None;
    }
    let legacy = legacy_ids
        .iter()
        .filter(|id| **id != app_id)
        .map(|id| settings_path(config_dir, id))
        .find(|path| path.is_file())?;
    fs::create_dir_all(target.parent()?).ok()?;
    fs::copy(&legacy, &target).ok()?;
    Some(legacy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preferences_are_sensible() {
        let prefs = UserPreferences::default();
        assert!(!prefs.nav_visible);
        assert!(prefs.heading_color_mode);
        assert!(prefs.side_by_side_scroll_sync);
        assert!(prefs.auto_save);
    }

    #[test]
    fn round_trip_toml() {
        let prefs = UserPreferences {
            nav_visible: true,
            heading_color_mode: false,
            side_by_side_scroll_sync: false,
            zoom_factor: 1.5,
            mode: "preview".to_owned(),
            auto_save: false,
            ..UserPreferences::default()
        };
        let serialized = toml::to_string_pretty(&prefs).unwrap_or_default();
        assert!(!serialized.is_empty(), "serialize should produce output");
        let deserialized: UserPreferences = toml::from_str(&serialized).unwrap_or_default();
        assert!(deserialized.nav_visible);
        assert!(!deserialized.heading_color_mode);
        assert!(!deserialized.side_by_side_scroll_sync);
        assert!(!deserialized.auto_save);
    }

    #[test]
    fn unknown_fields_ignored() {
        let toml_str = "\
            nav_visible = true\n\
            heading_color_mode = true\n\
            unknown_future_field = 42\n";
        let prefs: UserPreferences = toml::from_str(toml_str).unwrap_or_default();
        assert!(prefs.nav_visible);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let toml_str = "nav_visible = true\n";
        let prefs: UserPreferences = toml::from_str(toml_str).unwrap_or_default();
        assert!(prefs.nav_visible);
        assert!(prefs.heading_color_mode); // default
        assert!(prefs.side_by_side_scroll_sync);
    }

    #[test]
    fn empty_toml_uses_defaults() {
        let prefs: UserPreferences = toml::from_str("").unwrap_or_default();
        assert!(!prefs.nav_visible);
        assert!(prefs.heading_color_mode);
        assert!(prefs.side_by_side_scroll_sync);
    }

    #[test]
    fn new_view_settings_round_trip() {
        let prefs = UserPreferences {
            theme: "dark".to_owned(),
            word_wrap: false,
            spell_check: false,
            show_status_bar: false,
            show_format_toolbar: false,
            ..UserPreferences::default()
        };
        let serialized = toml::to_string_pretty(&prefs).unwrap_or_default();
        let loaded: UserPreferences = toml::from_str(&serialized).unwrap_or_default();
        assert_eq!(loaded.theme, "dark");
        assert_eq!(
            crate::ui_style::ThemeChoice::from_str_lossy(&loaded.theme),
            Some(crate::ui_style::ThemeChoice::Dark)
        );
        assert!(!loaded.word_wrap);
        assert!(!loaded.spell_check);
        assert!(!loaded.show_status_bar);
        assert!(!loaded.show_format_toolbar);
    }

    #[test]
    fn old_settings_file_gets_notepad_defaults() {
        // A settings file written before the theme / view settings existed.
        let old = "nav_visible = true\nheading_color_mode = false\nside_by_side_scroll_sync = true\nzoom_factor = 1.25\nmode = \"edit\"\nauto_save = true\n";
        let prefs: UserPreferences = toml::from_str(old).unwrap_or_default();
        assert!(prefs.nav_visible, "old fields still load");
        assert!(!prefs.heading_color_mode);
        assert_eq!(prefs.theme, "light", "light theme by default");
        assert!(prefs.word_wrap);
        assert!(prefs.spell_check, "spell check on by default");
        assert!(prefs.show_status_bar);
        assert!(prefs.show_format_toolbar);
    }

    #[test]
    fn explorer_table_round_trips_and_old_text_contrast_is_ignored() {
        let prefs = UserPreferences {
            close_window_with_last_tab: true,
            explorer: crate::explorer::ExplorerPreferences {
                show_all_files: true,
                ..crate::explorer::ExplorerPreferences::default()
            },
            ..UserPreferences::default()
        };
        let serialized = toml::to_string_pretty(&prefs).unwrap_or_default();
        let loaded: UserPreferences = toml::from_str(&serialized).unwrap_or_default();
        assert!(loaded.explorer.show_all_files);
        assert!(loaded.close_window_with_last_tab);
        assert!(!serialized.contains("text_contrast"), "{serialized}");

        // Files from when Text contrast was a setting still load.
        let old = "nav_visible = true\ntext_contrast = 85\ntheme = \"dark\"\n";
        let loaded = UserPreferences::from_toml(old);
        assert!(loaded.nav_visible);
        assert_eq!(loaded.theme, "dark");
        // Off unless chosen: closing the last tab keeps the window.
        assert!(!loaded.close_window_with_last_tab);
    }

    #[test]
    fn old_text_size_setting_is_ignored() {
        // The text size is fixed now; files from when it was a setting
        // still load (with their other settings) and lose it on save.
        let old = "nav_visible = true\ntext_size = 18\nzoom_factor = 1.25\n";
        let loaded = UserPreferences::from_toml(old);
        assert!(loaded.nav_visible);
        assert!((loaded.zoom_factor - 1.25).abs() < f32::EPSILON);
        let saved = toml::to_string_pretty(&loaded).unwrap_or_default();
        assert!(!saved.contains("text_size"), "{saved}");
    }

    #[test]
    fn old_markdown_syntax_setting_maps_edit_to_raw_and_is_not_written() {
        for (old, mode) in [
            ("mode = \"edit\"\nmarkdown_syntax = true\n", "raw"),
            ("markdown_syntax = true\n", "raw"),
            ("mode = \"edit\"\nmarkdown_syntax = false\n", "edit"),
            ("mode = \"preview\"\nmarkdown_syntax = true\n", "preview"),
            (
                "mode = \"sidebyside\"\nmarkdown_syntax = true\n",
                "sidebyside",
            ),
            ("mode = \"raw\"\n", "raw"),
        ] {
            let prefs = UserPreferences::from_toml(old);
            assert_eq!(prefs.mode, mode, "{old}");
            assert!(!prefs.markdown_syntax, "{old}");
            let saved = toml::to_string_pretty(&prefs).unwrap_or_default();
            assert!(!saved.contains("markdown_syntax"), "{saved}");
        }
    }

    #[test]
    fn legacy_settings_are_copied_once_when_the_app_has_none() {
        let dir = std::env::temp_dir().join(format!("rustdown-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let write = |id: &str, text: &str| {
            let path = settings_path(&dir, id);
            let _ = fs::create_dir_all(path.parent().unwrap_or(&dir));
            assert!(fs::write(path, text).is_ok());
        };
        let read = |id: &str| fs::read_to_string(settings_path(&dir, id)).ok();

        // Nothing to copy: no folder is created.
        assert_eq!(migrate_legacy_settings(&dir, "newapp", &["oldapp"]), None);
        assert!(!dir.join("newapp").exists());

        // The first legacy id with a settings file is copied; the old one stays.
        write("older", "theme = \"light\"\n");
        write("oldapp", "theme = \"dark\"\n");
        assert_eq!(
            migrate_legacy_settings(&dir, "newapp", &["missing", "oldapp", "older"]),
            Some(settings_path(&dir, "oldapp"))
        );
        assert_eq!(read("newapp").as_deref(), Some("theme = \"dark\"\n"));
        assert_eq!(read("oldapp").as_deref(), Some("theme = \"dark\"\n"));
        let prefs = UserPreferences::from_toml(&read("newapp").unwrap_or_default());
        assert_eq!(prefs.theme, "dark");

        // Once the app has settings they are never replaced.
        write("oldapp", "theme = \"system\"\n");
        assert_eq!(migrate_legacy_settings(&dir, "newapp", &["oldapp"]), None);
        assert_eq!(read("newapp").as_deref(), Some("theme = \"dark\"\n"));

        // The app's own id is never a legacy one.
        write("same", "x = 1\n");
        assert_eq!(migrate_legacy_settings(&dir, "same", &["same"]), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_file_path_returns_some() {
        if dirs::config_dir().is_some() {
            let path = config_file_path();
            assert!(path.is_some());
            if let Some(p) = path {
                let s = p.to_string_lossy();
                assert!(
                    p.ends_with(Path::new(APP_ID).join("settings.toml")),
                    "unexpected config path: {s}"
                );
            }
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join("rustdown-test-prefs");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("settings.toml");

        let prefs = UserPreferences {
            nav_visible: true,
            heading_color_mode: false,
            side_by_side_scroll_sync: false,
            zoom_factor: 1.5,
            mode: "preview".to_owned(),
            auto_save: false,
            ..UserPreferences::default()
        };
        if let Ok(contents) = toml::to_string_pretty(&prefs) {
            let _ = fs::write(&path, &contents);
        }

        if let Ok(raw) = fs::read_to_string(&path) {
            let loaded: UserPreferences = toml::from_str(&raw).unwrap_or_default();
            assert!(loaded.nav_visible);
            assert!(!loaded.heading_color_mode);
            assert!(!loaded.side_by_side_scroll_sync);
            assert!(!loaded.auto_save);
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
