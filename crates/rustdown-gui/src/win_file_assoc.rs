//! Per-user `.md` / `.markdown` / `.txt` file association (HKCU, no admin
//! rights).
//!
//! [`register_plan`] and [`unregister_plan`] describe every registry change
//! as plain data, unit-tested on any OS; [`register`] and [`unregister`]
//! apply them on Windows through the safe `winreg` crate.
//!
//! Windows 10/11 do not let apps make themselves the default: the user's
//! choice (`UserChoice`) is hash-protected.  Registering makes the app a
//! candidate in "Open with" and in Settings → Default apps, where the user
//! confirms the default once ([`open_default_apps_settings`]).

// Only the Windows build applies the plans; they are compiled everywhere so
// the tests run on the Linux dev host.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;

use crate::app_identity::{APP_ID, APP_NAME, EXE_NAME, LEGACY_APP_IDS, app_id};

/// Whether this build can change file associations at all.
pub(crate) const SUPPORTED: bool = cfg!(windows);

/// Settings → Default apps, opened at the app's page (Windows 11; Windows
/// 10 ignores the parameter and shows the Default apps list).  The value
/// is the app's name under `RegisteredApplications`: its id.
pub(crate) const DEFAULT_APPS_URI: &str =
    concat!("ms-settings:defaultapps?registeredAppUser=", app_id!());

const APP_DESCRIPTION: &str = "Notepad-style Markdown editor";
const DOCUMENT_TYPE_NAME: &str = "Markdown Document";
const EXTENSIONS: [&str; 2] = [".md", ".markdown"];
/// Plain text files: their own `ProgID` (type name "Text Document").
const TEXT_TYPE_NAME: &str = "Text Document";
const TEXT_EXTENSIONS: [&str; 1] = [".txt"];
const CLASSES: &str = r"Software\Classes";
const REGISTERED_APPLICATIONS: &str = r"Software\RegisteredApplications";
#[cfg(not(windows))]
const UNSUPPORTED: &str = "File associations are only available on Windows.";

/// File name of the preview handler for File Explorer's preview pane, next
/// to the exe (`src/preview_dll`).
pub(crate) const PREVIEW_DLL: &str = "mdnotepad_preview.dll";
/// Its class id (`CLSID_PREVIEW` in `src/preview_dll/handler.rs`).
const PREVIEW_CLSID: &str = "{5F3B6C2E-8D4A-4B7F-9E21-7C6A1D93B0E4}";
/// The shell's slot for a type's preview handler (`IPreviewHandler`).
const PREVIEW_SLOT: &str = "{8895B1C6-B41F-4C1C-A562-0D564250836F}";
/// `prevhost.exe`, which runs 64-bit preview handlers.
const PREVHOST_APP_ID: &str = "{6D2B5079-2F0B-48DD-AB7F-97CEC514D30B}";
const PREVIEW_NAME: &str = "MdNotePad Markdown preview";
const PREVIEW_HANDLERS: &str = r"Software\Microsoft\Windows\CurrentVersion\PreviewHandlers";

/// Registry value data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RegData {
    /// `REG_SZ`.
    Sz(String),
    /// `REG_NONE` with no data (the documented form for `OpenWithProgids`).
    Empty,
}

/// One registry change; keys are relative to `HKEY_CURRENT_USER`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RegOp {
    /// Create `key` (and its parents) and set value `name` (`""` is the
    /// key's default value).
    Set {
        key: String,
        name: String,
        data: RegData,
    },
    /// Delete `key` and everything below it; a missing key is fine.
    DeleteTree { key: String },
    /// Delete value `name` of `key`; a missing key or value is fine.
    DeleteValue { key: String, name: String },
    /// Delete `key` only if it has no values and no subkeys left.
    DeleteKeyIfEmpty { key: String },
    /// Delete `key` and everything below it if its value `name` is `data`
    /// (ignoring case): an entry that is still ours.
    DeleteTreeIfValue {
        key: String,
        name: String,
        data: String,
    },
}

impl RegOp {
    pub(crate) fn key(&self) -> &str {
        match self {
            Self::Set { key, .. }
            | Self::DeleteTree { key }
            | Self::DeleteValue { key, .. }
            | Self::DeleteKeyIfEmpty { key }
            | Self::DeleteTreeIfValue { key, .. } => key,
        }
    }
}

fn set(key: impl Into<String>, name: &str, data: &str) -> RegOp {
    RegOp::Set {
        key: key.into(),
        name: name.to_owned(),
        data: RegData::Sz(data.to_owned()),
    }
}

/// File name of the exe (the `Applications\<name>` key).  Splits on both
/// separators so tests with Windows paths behave the same on Linux.
fn exe_file_name(exe: &str) -> &str {
    exe.rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(EXE_NAME)
}

/// The registry names of the app with id `id` (the current app or one of
/// `LEGACY_APP_IDS`).
struct Names {
    /// `ProgID`, e.g. `<id>.md`.
    prog_id: String,
    /// `ProgID` of plain text files, `<id>.txt`.
    text_prog_id: String,
    /// `Software\<id>`.
    app_key: String,
    /// `Software\<id>\Capabilities`.
    capabilities: String,
    /// The value name under `RegisteredApplications`.
    registered_name: String,
}

impl Names {
    fn new(id: &str) -> Self {
        let app_key = format!(r"Software\{id}");
        Self {
            prog_id: format!("{id}.md"),
            text_prog_id: format!("{id}.txt"),
            capabilities: format!(r"{app_key}\Capabilities"),
            app_key,
            registered_name: id.to_owned(),
        }
    }

    fn prog_id_key(&self) -> String {
        format!(r"{CLASSES}\{}", self.prog_id)
    }

    fn text_prog_id_key(&self) -> String {
        format!(r"{CLASSES}\{}", self.text_prog_id)
    }

    /// Every extension with the `ProgID` that opens it.
    fn extensions(&self) -> impl Iterator<Item = (&'static str, &str)> {
        EXTENSIONS
            .into_iter()
            .map(|ext| (ext, self.prog_id.as_str()))
            .chain(
                TEXT_EXTENSIONS
                    .into_iter()
                    .map(|ext| (ext, self.text_prog_id.as_str())),
            )
    }
}

fn applications_key(exe: &str) -> String {
    format!(r"{CLASSES}\Applications\{}", exe_file_name(exe))
}

/// Every write that registers `exe` as a handler.  Only sets values, so
/// running it again is harmless and points everything at the current exe.
pub(crate) fn register_plan(exe: &Path) -> Result<Vec<RegOp>, String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| format!("the exe path is not valid Unicode: {}", exe.display()))?;
    Ok(register_ops(&Names::new(APP_ID), APP_NAME, exe))
}

/// [`register_plan`] for the app named `names` / `display_name`.
fn register_ops(names: &Names, display_name: &str, exe: &str) -> Vec<RegOp> {
    let command = format!("\"{exe}\" \"%1\"");
    let icon = format!("{exe},0");
    let prog_id = names.prog_id_key();
    let app = applications_key(exe);
    let capabilities = names.capabilities.as_str();

    // ProgIDs: how the app opens a Markdown file and a text file.
    let mut ops = Vec::new();
    for (class, type_name) in [
        (prog_id, DOCUMENT_TYPE_NAME),
        (names.text_prog_id_key(), TEXT_TYPE_NAME),
    ] {
        ops.push(set(&class, "", type_name));
        ops.push(set(format!(r"{class}\DefaultIcon"), "", &icon));
        ops.push(set(format!(r"{class}\shell\open\command"), "", &command));
    }
    // Offer it for each extension ("Open with").
    for (ext, prog_id) in names.extensions() {
        ops.push(RegOp::Set {
            key: format!(r"{CLASSES}\{ext}\OpenWithProgids"),
            name: prog_id.to_owned(),
            data: RegData::Empty,
        });
    }
    // The application itself ("Open with" → "Choose another app").
    ops.push(set(&app, "FriendlyAppName", display_name));
    ops.push(set(format!(r"{app}\DefaultIcon"), "", &icon));
    ops.push(set(format!(r"{app}\shell\open\command"), "", &command));
    for (ext, _) in names.extensions() {
        ops.push(set(format!(r"{app}\SupportedTypes"), ext, ""));
    }
    // Capabilities + RegisteredApplications: listed in Settings → Default apps.
    ops.push(set(capabilities, "ApplicationName", display_name));
    ops.push(set(capabilities, "ApplicationDescription", APP_DESCRIPTION));
    ops.push(set(capabilities, "ApplicationIcon", &icon));
    for (ext, prog_id) in names.extensions() {
        ops.push(set(
            format!(r"{capabilities}\FileAssociations"),
            ext,
            prog_id,
        ));
    }
    ops.push(set(
        REGISTERED_APPLICATIONS,
        &names.registered_name,
        capabilities,
    ));
    ops
}

/// The writes that register the preview handler `dll` (a path) for
/// `.md` / `.markdown` in File Explorer's preview pane.
pub(crate) fn preview_plan(dll: &str) -> Vec<RegOp> {
    let class = format!(r"{CLASSES}\CLSID\{PREVIEW_CLSID}");
    let server = format!(r"{class}\InprocServer32");
    let mut ops = vec![
        set(&class, "", PREVIEW_NAME),
        set(&class, "DisplayName", PREVIEW_NAME),
        set(&class, "AppID", PREVHOST_APP_ID),
        set(&server, "", dll),
        set(&server, "ThreadingModel", "Apartment"),
    ];
    for ext in EXTENSIONS {
        ops.push(set(
            format!(r"{CLASSES}\{ext}\shellex\{PREVIEW_SLOT}"),
            "",
            PREVIEW_CLSID,
        ));
    }
    ops.push(set(PREVIEW_HANDLERS, PREVIEW_CLSID, PREVIEW_NAME));
    ops
}

/// Undo [`preview_plan`]; a type's preview slot only while it still names
/// this handler.
pub(crate) fn preview_unregister_plan() -> Vec<RegOp> {
    let mut ops = vec![
        RegOp::DeleteTree {
            key: format!(r"{CLASSES}\CLSID\{PREVIEW_CLSID}"),
        },
        RegOp::DeleteValue {
            key: PREVIEW_HANDLERS.to_owned(),
            name: PREVIEW_CLSID.to_owned(),
        },
    ];
    for ext in EXTENSIONS {
        let ext_key = format!(r"{CLASSES}\{ext}");
        let shellex = format!(r"{ext_key}\shellex");
        ops.push(RegOp::DeleteTreeIfValue {
            key: format!(r"{shellex}\{PREVIEW_SLOT}"),
            name: String::new(),
            data: PREVIEW_CLSID.to_owned(),
        });
        ops.push(RegOp::DeleteKeyIfEmpty { key: shellex });
        ops.push(RegOp::DeleteKeyIfEmpty { key: ext_key });
    }
    ops
}

/// Undo [`register_plan`]: removes only what it wrote, and the extension
/// keys only if nothing else is left in them.
pub(crate) fn unregister_plan(exe: &Path) -> Vec<RegOp> {
    unregister_ops(&Names::new(APP_ID), &exe.to_string_lossy())
}

/// [`unregister_plan`] for the app named `names` whose exe is `exe`.
fn unregister_ops(names: &Names, exe: &str) -> Vec<RegOp> {
    let mut ops = vec![
        RegOp::DeleteTree {
            key: names.prog_id_key(),
        },
        RegOp::DeleteTree {
            key: names.text_prog_id_key(),
        },
    ];
    for (ext, prog_id) in names.extensions() {
        let ext_key = format!(r"{CLASSES}\{ext}");
        let open_with = format!(r"{ext_key}\OpenWithProgids");
        ops.push(RegOp::DeleteValue {
            key: open_with.clone(),
            name: prog_id.to_owned(),
        });
        ops.push(RegOp::DeleteKeyIfEmpty { key: open_with });
        ops.push(RegOp::DeleteKeyIfEmpty { key: ext_key });
    }
    ops.push(RegOp::DeleteTree {
        key: applications_key(exe),
    });
    ops.push(RegOp::DeleteTree {
        key: names.capabilities.clone(),
    });
    ops.push(RegOp::DeleteKeyIfEmpty {
        key: names.app_key.clone(),
    });
    ops.push(RegOp::DeleteValue {
        key: REGISTERED_APPLICATIONS.to_owned(),
        name: names.registered_name.clone(),
    });
    ops
}

/// The default app for one of the file types the app registers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileTypeDefault {
    pub(crate) extension: &'static str,
    /// The default app's name, `None` when there is none.
    pub(crate) app: Option<String>,
    /// Whether the default app is this one (its exe).
    pub(crate) ours: bool,
}

/// For each file type the app registers, its default app now, asked from
/// Windows the way Explorer resolves a double-click (so it is what really
/// happens).  Windows lets apps read the user's choice, not change it.
#[cfg(windows)]
pub(crate) fn defaults() -> Vec<FileTypeDefault> {
    use crate::win_ffi::{HandlerInfo, file_type_handler};
    let this_exe = std::env::current_exe().ok().and_then(|exe| {
        exe.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    Names::new(APP_ID)
        .extensions()
        .map(|(ext, _)| {
            let handler = file_type_handler(ext, HandlerInfo::Executable);
            let ours = handler
                .as_deref()
                .zip(this_exe.as_deref())
                .is_some_and(|(handler, this)| exe_file_name(handler).eq_ignore_ascii_case(this));
            FileTypeDefault {
                extension: ext,
                app: file_type_handler(ext, HandlerInfo::FriendlyName),
                ours,
            }
        })
        .collect()
}

/// Off Windows: nothing to report.
#[cfg(not(windows))]
pub(crate) const fn defaults() -> Vec<FileTypeDefault> {
    Vec::new()
}

/// Remove the registrations of the app's earlier ids ([`LEGACY_APP_IDS`],
/// whose exe was `<id>.exe`), so Windows does not list the app twice.
/// Leaves the current id alone.
pub(crate) fn legacy_cleanup_plan() -> Vec<RegOp> {
    legacy_cleanup_ops(LEGACY_APP_IDS)
}

fn legacy_cleanup_ops(legacy_ids: &[&str]) -> Vec<RegOp> {
    legacy_ids
        .iter()
        .filter(|id| !id.eq_ignore_ascii_case(APP_ID))
        .flat_map(|id| unregister_ops(&Names::new(id), &format!("{id}.exe")))
        .collect()
}

/// What Register does: the legacy registrations go first, then
/// [`register_plan`] writes the current one.
pub(crate) fn register_with_cleanup_plan(exe: &Path) -> Result<Vec<RegOp>, String> {
    let mut ops = legacy_cleanup_plan();
    ops.extend(register_plan(exe)?);
    Ok(ops)
}

/// What [`register`] registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Registration {
    /// The file types (Settings menu: and the preview pane's handler when
    /// its DLL is next to the exe).
    Everything,
    /// Only the file types (the installer's "Open .md files with" tick).
    FileTypes,
    /// Only the preview pane's handler (the installer's preview tick).
    Preview,
}

/// Register the running exe (removing legacy registrations first) and/or
/// its preview handler, and notify the shell.
#[cfg(windows)]
pub(crate) fn register(what: Registration) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|err| format!("cannot locate {EXE_NAME}: {err}"))?;
    let mut ops = if what == Registration::Preview {
        Vec::new()
    } else {
        register_with_cleanup_plan(&exe)?
    };
    if what != Registration::FileTypes {
        let dll = exe.with_file_name(PREVIEW_DLL);
        match dll.to_str().filter(|_| dll.is_file()) {
            Some(dll) => ops.extend(preview_plan(dll)),
            None if what == Registration::Preview => {
                return Err(format!("{PREVIEW_DLL} is not next to {EXE_NAME}"));
            }
            None => {}
        }
    }
    let result = registry::apply(&ops);
    crate::win_ffi::notify_associations_changed();
    result
}

/// Remove the registration (and any legacy ones) and the preview handler's,
/// or only the preview handler's, and notify the shell.
#[cfg(windows)]
pub(crate) fn unregister(only_preview: bool) -> Result<(), String> {
    let mut ops = Vec::new();
    if !only_preview {
        let exe = std::env::current_exe().unwrap_or_default();
        ops.extend(unregister_plan(&exe));
    }
    ops.extend(preview_unregister_plan());
    if !only_preview {
        ops.extend(legacy_cleanup_plan());
    }
    let result = registry::apply(&ops);
    crate::win_ffi::notify_associations_changed();
    result
}

/// Open Settings → Default apps at the app's page; `false` if it failed.
#[cfg(windows)]
pub(crate) fn open_default_apps_settings() -> bool {
    crate::win_ffi::shell_open(DEFAULT_APPS_URI).is_ok()
}

#[cfg(not(windows))]
pub(crate) fn register(_what: Registration) -> Result<(), String> {
    Err(UNSUPPORTED.to_owned())
}

#[cfg(not(windows))]
pub(crate) fn unregister(_only_preview: bool) -> Result<(), String> {
    Err(UNSUPPORTED.to_owned())
}

#[cfg(not(windows))]
pub(crate) const fn open_default_apps_settings() -> bool {
    false
}

/// Applies [`RegOp`]s to `HKEY_CURRENT_USER`.
#[cfg(windows)]
mod registry {
    use std::{borrow::Cow, io};

    use winreg::{
        RegKey, RegValue,
        enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, RegType},
    };

    use super::{RegData, RegOp};

    /// Apply every op (continuing past failures); the error lists each
    /// failed key.
    pub(super) fn apply(ops: &[RegOp]) -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let errors: Vec<String> = ops
            .iter()
            .filter_map(|op| {
                apply_one(&hkcu, op)
                    .err()
                    .map(|err| format!(r"HKCU\{}: {err}", op.key()))
            })
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }

    fn apply_one(hkcu: &RegKey, op: &RegOp) -> io::Result<()> {
        match op {
            RegOp::Set { key, name, data } => {
                let (key, _) = hkcu.create_subkey(key)?;
                match data {
                    RegData::Sz(text) => key.set_value(name, text),
                    RegData::Empty => key.set_raw_value(
                        name,
                        &RegValue {
                            bytes: Cow::Borrowed(&[]),
                            vtype: RegType::REG_NONE,
                        },
                    ),
                }
            }
            RegOp::DeleteTree { key } => ignore_missing(hkcu.delete_subkey_all(key)),
            RegOp::DeleteValue { key, name } => ignore_missing(
                hkcu.open_subkey_with_flags(key, KEY_SET_VALUE)
                    .and_then(|key| key.delete_value(name)),
            ),
            RegOp::DeleteTreeIfValue { key, name, data } => {
                let value: io::Result<String> = hkcu
                    .open_subkey_with_flags(key, KEY_READ)
                    .and_then(|opened| opened.get_value(name));
                match value {
                    Ok(found) if found.eq_ignore_ascii_case(data) => {
                        ignore_missing(hkcu.delete_subkey_all(key))
                    }
                    Ok(_) => Ok(()),
                    Err(err) => ignore_missing(Err(err)),
                }
            }
            RegOp::DeleteKeyIfEmpty { key } => {
                let info = match hkcu.open_subkey_with_flags(key, KEY_READ) {
                    Ok(opened) => opened.query_info()?,
                    Err(err) => return ignore_missing(Err(err)),
                };
                if info.sub_keys == 0 && info.values == 0 {
                    ignore_missing(hkcu.delete_subkey(key))
                } else {
                    Ok(())
                }
            }
        }
    }

    fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
        match result {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const EXE: &str = r"C:\Program Files\App\app.exe";

    fn plan(exe: &str) -> Vec<RegOp> {
        let plan = register_plan(&PathBuf::from(exe));
        assert!(plan.is_ok(), "{plan:?}");
        plan.unwrap_or_default()
    }

    fn sz(key: &str, name: &str, data: &str) -> RegOp {
        set(key, name, data)
    }

    fn empty(key: &str, name: &str) -> RegOp {
        RegOp::Set {
            key: key.to_owned(),
            name: name.to_owned(),
            data: RegData::Empty,
        }
    }

    #[test]
    fn register_plan_writes_exactly_these_values() {
        // Every name comes from the app identity: ProgID `<id>.md`,
        // `Software\<id>\Capabilities`, the RegisteredApplications value
        // `<id>`, the display name as FriendlyAppName / ApplicationName.
        let (id, name) = (APP_ID, APP_NAME);
        let command = r#""C:\Program Files\App\app.exe" "%1""#;
        let icon = r"C:\Program Files\App\app.exe,0";
        let app = r"Software\Classes\Applications\app.exe";
        let prog_id = format!("{id}.md");
        let class = format!(r"Software\Classes\{prog_id}");
        let text_prog_id = format!("{id}.txt");
        let text_class = format!(r"Software\Classes\{text_prog_id}");
        let capabilities = format!(r"Software\{id}\Capabilities");
        let associations = format!(r"{capabilities}\FileAssociations");
        let expected = vec![
            sz(&class, "", "Markdown Document"),
            sz(&format!(r"{class}\DefaultIcon"), "", icon),
            sz(&format!(r"{class}\shell\open\command"), "", command),
            sz(&text_class, "", "Text Document"),
            sz(&format!(r"{text_class}\DefaultIcon"), "", icon),
            sz(&format!(r"{text_class}\shell\open\command"), "", command),
            empty(r"Software\Classes\.md\OpenWithProgids", &prog_id),
            empty(r"Software\Classes\.markdown\OpenWithProgids", &prog_id),
            empty(r"Software\Classes\.txt\OpenWithProgids", &text_prog_id),
            sz(app, "FriendlyAppName", name),
            sz(&format!(r"{app}\DefaultIcon"), "", icon),
            sz(&format!(r"{app}\shell\open\command"), "", command),
            sz(&format!(r"{app}\SupportedTypes"), ".md", ""),
            sz(&format!(r"{app}\SupportedTypes"), ".markdown", ""),
            sz(&format!(r"{app}\SupportedTypes"), ".txt", ""),
            sz(&capabilities, "ApplicationName", name),
            sz(
                &capabilities,
                "ApplicationDescription",
                "Notepad-style Markdown editor",
            ),
            sz(&capabilities, "ApplicationIcon", icon),
            sz(&associations, ".md", &prog_id),
            sz(&associations, ".markdown", &prog_id),
            sz(&associations, ".txt", &text_prog_id),
            sz(r"Software\RegisteredApplications", id, &capabilities),
        ];
        assert_eq!(plan(EXE), expected);
    }

    #[test]
    fn registered_app_name_matches_the_settings_uri() {
        let name = DEFAULT_APPS_URI.rsplit('=').next().unwrap_or_default();
        assert!(plan(EXE).iter().any(|op| matches!(
            op,
            RegOp::Set { key, name: value, .. }
                if key == REGISTERED_APPLICATIONS && value == name
        )));
    }

    #[test]
    fn reregistering_a_moved_exe_only_changes_paths() {
        let old = plan(EXE);
        let new = plan(r"D:\Tools\app.exe");
        assert_eq!(old.len(), new.len());
        for (old, new) in old.iter().zip(&new) {
            let (
                RegOp::Set {
                    key: old_key,
                    name: old_name,
                    data: old_data,
                },
                RegOp::Set {
                    key: new_key,
                    name: new_name,
                    data: new_data,
                },
            ) = (old, new)
            else {
                unreachable!("register only sets values: {old:?} {new:?}")
            };
            assert_eq!((old_key, old_name), (new_key, new_name));
            if let RegData::Sz(new_text) = new_data
                && new_text.contains(r"D:\Tools")
            {
                assert_ne!(old_data, new_data);
            } else {
                assert_eq!(old_data, new_data);
            }
        }
    }

    #[test]
    fn applications_key_uses_the_exe_file_name() {
        let ops = plan(r"C:\Users\me\Downloads\app-x64.exe");
        assert!(
            ops.iter()
                .any(|op| op.key() == r"Software\Classes\Applications\app-x64.exe")
        );
        assert_eq!(exe_file_name("app.exe"), "app.exe");
        assert_eq!(exe_file_name(r"C:\dir\"), EXE_NAME);
    }

    #[test]
    fn unregister_removes_everything_register_wrote() {
        let undo = unregister_plan(Path::new(EXE));
        for op in plan(EXE) {
            let RegOp::Set { key, name, .. } = &op else {
                unreachable!()
            };
            let removed = undo.iter().any(|undo| match undo {
                RegOp::DeleteTree { key: tree } => {
                    key == tree || key.starts_with(&format!(r"{tree}\"))
                }
                RegOp::DeleteValue {
                    key: undo_key,
                    name: undo_name,
                } => undo_key == key && undo_name == name,
                _ => false,
            });
            assert!(removed, "not undone: {op:?}");
        }
    }

    #[test]
    fn unregister_never_deletes_shared_keys() {
        let app_key = Names::new(APP_ID).app_key;
        let shared = [
            CLASSES,
            r"Software\Classes\.md",
            r"Software\Classes\.markdown",
            r"Software\Classes\.md\OpenWithProgids",
            r"Software\Classes\.markdown\OpenWithProgids",
            r"Software\Classes\.txt",
            r"Software\Classes\.txt\OpenWithProgids",
            r"Software\Classes\Applications",
            app_key.as_str(),
            REGISTERED_APPLICATIONS,
            "Software",
        ];
        for op in unregister_plan(Path::new(EXE)) {
            if let RegOp::DeleteTree { key } = &op {
                assert!(!shared.contains(&key.as_str()), "deletes shared {key}");
            }
            assert!(
                !matches!(op, RegOp::Set { .. }),
                "unregister writes: {op:?}"
            );
        }
    }

    /// Whether `undo` removes the value `name` of `key`.
    fn undoes(undo: &[RegOp], key: &str, name: &str) -> bool {
        undo.iter().any(|undo| match undo {
            RegOp::DeleteTree { key: tree } => key == tree || key.starts_with(&format!(r"{tree}\")),
            RegOp::DeleteValue {
                key: undo_key,
                name: undo_name,
            } => undo_key == key && undo_name == name,
            _ => false,
        })
    }

    #[test]
    fn legacy_cleanup_removes_an_old_registration_and_nothing_of_the_current_one() {
        // What an old version named "oldapp" (oldapp.exe) wrote.
        let old_exe = r"C:\Program Files\oldapp\oldapp.exe";
        let old = register_ops(&Names::new("oldapp"), "OldApp", old_exe);
        let cleanup = legacy_cleanup_ops(&["oldapp"]);
        for op in &old {
            let RegOp::Set { key, name, .. } = op else {
                unreachable!()
            };
            assert!(undoes(&cleanup, key, name), "left behind: {op:?}");
        }
        // Old ProgID, OpenWithProgids values, Applications\oldapp.exe,
        // Capabilities and the RegisteredApplications value are removed.
        for expected in [
            RegOp::DeleteTree {
                key: r"Software\Classes\oldapp.md".to_owned(),
            },
            RegOp::DeleteValue {
                key: r"Software\Classes\.md\OpenWithProgids".to_owned(),
                name: "oldapp.md".to_owned(),
            },
            RegOp::DeleteTree {
                key: r"Software\Classes\Applications\oldapp.exe".to_owned(),
            },
            RegOp::DeleteTree {
                key: r"Software\oldapp\Capabilities".to_owned(),
            },
            RegOp::DeleteValue {
                key: REGISTERED_APPLICATIONS.to_owned(),
                name: "oldapp".to_owned(),
            },
        ] {
            assert!(cleanup.contains(&expected), "{expected:?}");
        }
        // Nothing the current registration writes is touched.
        for op in plan(EXE) {
            let RegOp::Set { key, name, .. } = &op else {
                unreachable!()
            };
            assert!(!undoes(&cleanup, key, name), "removes current {op:?}");
        }
        // Never writes; the current id is never a legacy one.
        assert!(cleanup.iter().all(|op| !matches!(op, RegOp::Set { .. })));
        assert!(legacy_cleanup_ops(&[APP_ID]).is_empty());
    }

    #[test]
    fn register_removes_the_legacy_ids_first() {
        let ops = register_with_cleanup_plan(Path::new(EXE)).unwrap_or_default();
        let cleanup = legacy_cleanup_plan();
        assert_eq!(ops[..cleanup.len()], cleanup[..]);
        assert_eq!(ops[cleanup.len()..], plan(EXE)[..]);
        for id in LEGACY_APP_IDS.iter().filter(|id| **id != APP_ID) {
            let prog_id = RegOp::DeleteTree {
                key: format!(r"Software\Classes\{id}.md"),
            };
            assert!(cleanup.contains(&prog_id), "{id}");
        }
    }

    #[test]
    fn keys_are_relative_and_well_formed() {
        let all = plan(EXE)
            .into_iter()
            .chain(unregister_plan(Path::new(EXE)))
            .chain(legacy_cleanup_ops(&["oldapp"]));
        for op in all {
            let key = op.key();
            assert!(key.starts_with(r"Software\"), "{key}");
            assert!(!key.ends_with('\\') && !key.contains(r"\\"), "{key}");
            assert!(!key.contains('/'), "{key}");
        }
    }

    #[test]
    fn the_preview_handler_is_registered_and_removed_only_while_ours() {
        let dll = r"C:\Apps\mdnotepad_preview.dll";
        let ops = preview_plan(dll);
        let class = format!(r"{CLASSES}\CLSID\{PREVIEW_CLSID}");
        assert!(ops.contains(&set(format!(r"{class}\InprocServer32"), "", dll)));
        assert!(ops.contains(&set(&class, "AppID", PREVHOST_APP_ID)));
        for ext in EXTENSIONS {
            let slot = format!(r"{CLASSES}\{ext}\shellex\{PREVIEW_SLOT}");
            assert!(ops.contains(&set(&slot, "", PREVIEW_CLSID)), "{ext}");
            assert!(
                preview_unregister_plan().contains(&RegOp::DeleteTreeIfValue {
                    key: slot,
                    name: String::new(),
                    data: PREVIEW_CLSID.to_owned(),
                })
            );
        }
        assert!(ops.iter().all(|op| op.key().starts_with(r"Software\")));
    }
}
