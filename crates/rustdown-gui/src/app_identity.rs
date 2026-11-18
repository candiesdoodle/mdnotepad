//! The app's identity in one place.  Renaming the app is a change of
//! [`app_name!`] and [`app_id!`] here, plus the exe name (`[[bin]] name` in
//! `crates/rustdown-gui/Cargo.toml`), the strings of `windows/*.rc` and the
//! exe paths in `.github/workflows`.  Add the old id to [`LEGACY_APP_IDS`]
//! so settings and file associations carry over.

/// The display name: window and taskbar title, About, dialogs, the
/// registered app name.  A macro so `concat!` can build constant strings
/// from it.
macro_rules! app_name {
    () => {
        "MdNotePad"
    };
}

/// The lowercase id: settings and crash-log folders, the single-instance
/// pipe, the registry `ProgID` and keys, file names the app writes.
macro_rules! app_id {
    () => {
        "mdnotepad"
    };
}

pub(crate) use {app_id, app_name};

/// See [`app_name!`].
pub(crate) const APP_NAME: &str = app_name!();
/// See [`app_id!`].
pub(crate) const APP_ID: &str = app_id!();
/// The exe's file name on Windows (`[[bin]] name` in Cargo.toml).
pub(crate) const EXE_NAME: &str = concat!(app_name!(), ".exe");
/// Ids the app had before a rename (its exe was `<id>.exe`).  At startup
/// the settings file of the first one that has one is copied when the app
/// has none yet (`preferences`); registering file types removes their
/// registry entries first (`win_file_assoc::legacy_cleanup_plan`).
pub(crate) const LEGACY_APP_IDS: &[&str] = &["rustdown"];

/// The workspace version (`Cargo.toml`).
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const COPYRIGHT: &str = "© 2026 candiesdoodle";
pub(crate) const BASED_ON: &str = "Based on rustdown by teh-hippo (MIT License)";
pub(crate) const DESCRIPTION: &str = "A Notepad-style Markdown editor for Windows.";

/// The license and the third-party notices, shown from the About box.
pub(crate) const LICENSE_TEXT: &str = include_str!("../../../LICENSE");
pub(crate) const THIRD_PARTY_NOTICES: &str = include_str!("../../../THIRD-PARTY-NOTICES.md");

/// `--version` output, e.g. `rustdown 1.0.0`.
#[must_use]
pub(crate) fn version_line() -> String {
    format!("{APP_NAME} {VERSION}")
}

/// A file of this name next to the exe makes the app portable: settings,
/// the session and the crash log are kept in a `data` folder next to it
/// instead of the user's profile (the portable download has one).
pub(crate) const PORTABLE_MARKER: &str = "portable.txt";

/// In portable mode, the folder for settings, the session and the crash
/// log (see [`PORTABLE_MARKER`]).
pub(crate) fn portable_dir() -> Option<&'static std::path::Path> {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let folder = exe.parent()?;
        folder
            .join(PORTABLE_MARKER)
            .is_file()
            .then(|| folder.join("data"))
    })
    .as_deref()
}

/// The folder for the session and the crash log: the portable folder, or
/// `<local app data>\<app id>`.
pub(crate) fn local_data_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = portable_dir() {
        return Some(dir.to_path_buf());
    }
    Some(dirs::data_local_dir()?.join(APP_ID))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_consistent() {
        assert!(!APP_NAME.is_empty() && !APP_ID.is_empty());
        assert_eq!(APP_ID, APP_ID.to_lowercase(), "the id is lowercase");
        assert!(
            APP_ID
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "the id is safe in paths, pipe names and registry keys"
        );
        assert_eq!(EXE_NAME, format!("{APP_NAME}.exe"));
        assert_eq!(VERSION, "1.0.0");
        assert_eq!(version_line(), format!("{APP_NAME} 1.0.0"));
    }

    #[test]
    fn window_title_ends_with_the_app_name() {
        let ctx = eframe::egui::Context::default();
        let mut app = crate::RustdownApp::default();
        let output = ctx.run(eframe::egui::RawInput::default(), |ctx| {
            app.update_viewport_title(ctx);
        });
        let title = crate::test_harness::commands(&output)
            .into_iter()
            .find_map(|cmd| match cmd {
                eframe::egui::ViewportCommand::Title(title) => Some(title),
                _ => None,
            });
        assert_eq!(title, Some(format!("Untitled - {APP_NAME}")));
    }

    /// The exe is named after the app (`[[bin]] name`).
    #[test]
    fn binary_is_named_after_the_app() {
        let manifest = include_str!("../Cargo.toml");
        let bin = manifest.split("[[bin]]").nth(1).unwrap_or_default();
        assert!(
            bin.lines()
                .any(|line| line.trim() == format!("name = \"{APP_NAME}\"")),
            "{bin}"
        );
    }

    #[test]
    fn license_keeps_the_upstream_notice_and_adds_ours() {
        assert!(LICENSE_TEXT.starts_with("MIT License"));
        assert!(LICENSE_TEXT.contains("Copyright (c) 2026 teh-hippo"));
        assert!(LICENSE_TEXT.contains("Copyright (c) 2026 candiesdoodle"));
        assert!(LICENSE_TEXT.contains("Permission is hereby granted"));
        assert!(THIRD_PARTY_NOTICES.contains("SIL Open Font License"));
        assert!(BASED_ON.contains("rustdown") && BASED_ON.contains("teh-hippo"));
        assert!(COPYRIGHT.contains("2026 candiesdoodle"));
    }
}
