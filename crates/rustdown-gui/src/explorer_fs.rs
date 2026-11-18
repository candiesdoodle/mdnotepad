//! Explorer: pure file-system helpers (no UI).
//!
//! Sorting, filtering, directory listing, unique file names, path display
//! and the "can this file be opened as text" check.  Kept free of egui so
//! everything here is unit-testable with temp dirs.

use std::{
    borrow::Cow,
    cmp::Ordering,
    fs, io,
    path::{Component, Path, PathBuf},
};

use crate::is_markdown_path;

/// Directories never shown (heavy build / dependency folders).  Dot entries
/// (`.git`, `.vscode`, …) are skipped separately.
const HEAVY_DIRS: &[&str] = &["node_modules", "target"];

/// Largest non-Markdown file the explorer opens.
pub(crate) const MAX_TEXT_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// How much of a non-Markdown file is sniffed to decide whether it is text.
const SNIFF_BYTES: usize = 8 * 1024;

/// One file or folder in a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// File name as shown (lossy UTF-8).
    pub(crate) name: String,
    /// Full path: the listed directory joined with the entry's file name.
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
    /// `.md` / `.markdown` file (always `false` for folders).
    pub(crate) is_markdown: bool,
}

/// A directory's entries, or a user-facing error message.
pub(crate) type Listing = Result<Vec<Entry>, String>;

/// Case-insensitive "natural" order: digit runs compare by value, so
/// `file2` sorts before `file10`.  Names that differ only in case (or in
/// leading zeros) fall back to ordinal order so sorting is deterministic.
#[must_use]
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    natural_cmp_folded(a, b).then_with(|| a.cmp(b))
}

fn natural_cmp_folded(mut a: &str, mut b: &str) -> Ordering {
    loop {
        let (Some(ca), Some(cb)) = (a.chars().next(), b.chars().next()) else {
            return a.is_empty().cmp(&b.is_empty()).reverse();
        };
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let (digits_a, rest_a) = split_digits(a);
            let (digits_b, rest_b) = split_digits(b);
            let ord = cmp_digit_runs(digits_a, digits_b);
            if ord != Ordering::Equal {
                return ord;
            }
            a = rest_a;
            b = rest_b;
        } else {
            let ord = ca.to_lowercase().cmp(cb.to_lowercase());
            if ord != Ordering::Equal {
                return ord;
            }
            a = &a[ca.len_utf8()..];
            b = &b[cb.len_utf8()..];
        }
    }
}

fn split_digits(s: &str) -> (&str, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s.split_at(end)
}

/// Compare two ASCII digit runs by numeric value (any length, no overflow).
fn cmp_digit_runs(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Folders first, then files; each group in [`natural_cmp`] order.
pub(crate) fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
}

/// Dot entries (`.git`, `.obsidian`, …) are never shown.
#[must_use]
pub(crate) fn is_dot_name(name: &str) -> bool {
    name.starts_with('.')
}

/// Heavy folders (`node_modules`, `target`) are never shown.
#[must_use]
pub(crate) fn is_heavy_dir_name(name: &str) -> bool {
    HEAVY_DIRS
        .iter()
        .any(|heavy| name.eq_ignore_ascii_case(heavy))
}

/// Whether `rel` (a path relative to the explorer root) can appear in the
/// tree: no component is a dot entry and no folder on the way is a heavy
/// folder.  Used to ignore watcher noise (e.g. `.git/`, `target/`) and to
/// avoid revealing files the tree never shows.
#[must_use]
pub(crate) fn is_visible_relative_path(rel: &Path) -> bool {
    let mut components = rel.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return false;
        };
        let name = name.to_string_lossy();
        let is_last = components.peek().is_none();
        if is_dot_name(&name) || (!is_last && is_heavy_dir_name(&name)) {
            return false;
        }
    }
    true
}

/// Read one directory: skips dot entries, heavy folders and (on Windows)
/// Hidden / System entries; sorts folders first, natural order.
pub(crate) fn read_listing(dir: &Path) -> Listing {
    let read = fs::read_dir(dir).map_err(|err| listing_error_message(&err))?;
    let mut entries = Vec::new();
    for item in read {
        // A single unreadable entry should not hide the whole folder.
        let Ok(item) = item else {
            continue;
        };
        let file_name = item.file_name();
        let name = file_name.to_string_lossy();
        if is_dot_name(&name) || is_hidden_or_system(&item) {
            continue;
        }
        let path = item.path();
        let is_dir = match item.file_type() {
            // Follow symlinks / junctions so linked folders can be expanded.
            Ok(kind) if kind.is_symlink() => path.is_dir(),
            Ok(kind) => kind.is_dir(),
            Err(_) => continue,
        };
        if is_dir && is_heavy_dir_name(&name) {
            continue;
        }
        let is_markdown = !is_dir && is_markdown_path(&path);
        entries.push(Entry {
            name: name.into_owned(),
            path,
            is_dir,
            is_markdown,
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

/// Short, user-facing text for a directory read error.
#[must_use]
pub(crate) fn listing_error_message(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::PermissionDenied => "Access denied".to_owned(),
        io::ErrorKind::NotFound => "Folder not found".to_owned(),
        _ => format!("Can't read folder: {err}"),
    }
}

#[cfg(windows)]
fn is_hidden_or_system(entry: &fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    // `DirEntry::metadata` comes from the directory enumeration on Windows,
    // so this costs no extra system call.
    entry.metadata().is_ok_and(|metadata| {
        metadata.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
    })
}

#[cfg(not(windows))]
const fn is_hidden_or_system(_entry: &fs::DirEntry) -> bool {
    false
}

/// `Untitled.md`, `Untitled 2.md`, `Untitled 3.md`, …
#[must_use]
pub(crate) fn untitled_file_name(n: usize) -> String {
    if n <= 1 {
        "Untitled.md".to_owned()
    } else {
        format!("Untitled {n}.md")
    }
}

/// Create an empty, uniquely named `Untitled.md` (`Untitled 2.md`, …) in
/// `dir` and return its path.  Uses create-new semantics, so an existing
/// file is never overwritten (also on case-insensitive file systems).
pub(crate) fn create_untitled_markdown(dir: &Path) -> io::Result<PathBuf> {
    const MAX_ATTEMPTS: usize = 10_000;
    for n in 1..=MAX_ATTEMPTS {
        let path = dir.join(untitled_file_name(n));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other("too many Untitled files in this folder"))
}

/// Strip a Windows verbatim prefix for display: `\\?\C:\x` → `C:\x`,
/// `\\?\UNC\server\share` → `\\server\share`.  Other verbatim forms
/// (volume GUIDs) are kept, since they have no friendlier spelling.
#[must_use]
pub(crate) fn strip_verbatim_prefix(path: &str) -> Cow<'_, str> {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return Cow::Owned(format!(r"\\{rest}"));
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        let mut chars = rest.chars();
        if chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':') {
            return Cow::Borrowed(rest);
        }
    }
    Cow::Borrowed(path)
}

/// A path as the user would type it (never with a `\\?\` prefix).
#[must_use]
pub(crate) fn display_path(path: &Path) -> String {
    strip_verbatim_prefix(&path.to_string_lossy()).into_owned()
}

/// The form of a folder the explorer keeps as its root: absolute, without
/// a verbatim prefix and without resolving links or mapped drives (so the
/// tree shows the path the user chose).
#[must_use]
pub(crate) fn normalize_root(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match absolute.to_str().map(strip_verbatim_prefix) {
        Some(Cow::Owned(stripped)) => PathBuf::from(stripped),
        Some(Cow::Borrowed(stripped)) if stripped.len() != absolute.as_os_str().len() => {
            PathBuf::from(stripped)
        }
        _ => absolute,
    }
}

/// Folder name for headers; drive roots (`C:\`) have none, so show the path.
#[must_use]
pub(crate) fn folder_display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || display_path(path),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Heuristic "is this text": no NUL bytes and valid UTF-8 (a multi-byte
/// character cut off at the end of the sniffed prefix is fine).
#[must_use]
pub(crate) fn looks_like_text(bytes: &[u8]) -> bool {
    if memchr::memchr(0, bytes).is_some() {
        return false;
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        Err(err) => err.error_len().is_none(),
    }
}

/// Whether a non-Markdown file may be opened from the explorer: smaller
/// than [`MAX_TEXT_FILE_BYTES`] and looks like UTF-8 text.  The error is a
/// user-facing message for the tab's error banner.
pub(crate) fn check_text_file(path: &Path) -> Result<(), String> {
    use std::io::Read as _;

    let name = path
        .file_name()
        .map_or_else(|| display_path(path), |n| n.to_string_lossy().into_owned());
    let fail = |err: io::Error| format!("Can't open {name}: {err}");
    let metadata = fs::metadata(path).map_err(fail)?;
    if metadata.len() >= MAX_TEXT_FILE_BYTES {
        return Err(format!(
            "Can't open {name}: files over {} MB are not opened as text",
            MAX_TEXT_FILE_BYTES / (1024 * 1024)
        ));
    }
    let mut buf = Vec::with_capacity(SNIFF_BYTES);
    fs::File::open(path)
        .and_then(|file| file.take(SNIFF_BYTES as u64).read_to_end(&mut buf))
        .map_err(fail)?;
    if looks_like_text(&buf) {
        Ok(())
    } else {
        Err(format!(
            "Can't open {name}: it doesn't look like a text file"
        ))
    }
}

/// Show `path` selected in a File Explorer window.
#[cfg(windows)]
pub(crate) fn reveal_in_file_explorer(path: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt as _;
    // explorer.exe parses its own command line: `/select,"C:\a b\c.md"`
    // works for paths with spaces or commas, a fully quoted argument doesn't.
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", display_path(path)))
        .spawn()
        .map(drop)
}

/// No-op on the Linux dev host (the product is Windows-only).
#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)] // same signature as the Windows version
pub(crate) const fn reveal_in_file_explorer(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// A fresh, empty temp dir for explorer tests.
#[cfg(test)]
pub(crate) fn test_temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustdown-explorer-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        test_temp_dir(name)
    }

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut names: Vec<String> = names.iter().map(|&n| n.to_owned()).collect();
        names.sort_by(|a, b| natural_cmp(a, b));
        names
    }

    #[test]
    fn natural_order_compares_numbers_by_value_and_ignores_case() {
        assert_eq!(
            sorted(&["file10.md", "file2.md", "File1.md", "file1.md", "a.md"]),
            ["a.md", "File1.md", "file1.md", "file2.md", "file10.md"]
        );
        assert_eq!(
            sorted(&["Zeta", "alpha", "Beta"]),
            ["alpha", "Beta", "Zeta"]
        );
        assert_eq!(
            sorted(&["v1.10", "v1.9", "v1.2"]),
            ["v1.2", "v1.9", "v1.10"]
        );
        // Huge numbers do not overflow; leading zeros are a tie-breaker only.
        assert_eq!(
            natural_cmp("n99999999999999999999999", "n100000000000000000000000"),
            Ordering::Less
        );
        assert_eq!(
            natural_cmp("x007", "x7").reverse(),
            natural_cmp("x7", "x007")
        );
        assert_ne!(natural_cmp("x007", "x7"), Ordering::Equal);
        assert_eq!(natural_cmp("x2y", "x02z"), Ordering::Less);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("ab", "a"), Ordering::Greater);
        assert_eq!(
            natural_cmp("Ärger", "ärger").reverse(),
            natural_cmp("ärger", "Ärger")
        );
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);
    }

    #[test]
    fn entries_sort_folders_first() {
        let entry = |name: &str, is_dir| Entry {
            name: name.to_owned(),
            path: PathBuf::from(name),
            is_dir,
            is_markdown: !is_dir,
        };
        let mut entries = vec![
            entry("b.md", false),
            entry("z-dir", true),
            entry("a.md", false),
            entry("A-dir", true),
            entry("notes10", true),
            entry("notes9", true),
        ];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["A-dir", "notes9", "notes10", "z-dir", "a.md", "b.md"]
        );
    }

    #[test]
    fn skip_rules() {
        assert!(is_dot_name(".git") && is_dot_name(".obsidian") && !is_dot_name("a.md"));
        assert!(is_heavy_dir_name("node_modules") && is_heavy_dir_name("Target"));
        assert!(!is_heavy_dir_name("targets"));
        assert!(is_visible_relative_path(Path::new("docs/a.md")));
        assert!(is_visible_relative_path(Path::new("")));
        assert!(!is_visible_relative_path(Path::new(".git/HEAD")));
        assert!(!is_visible_relative_path(Path::new("docs/.hidden.md")));
        assert!(!is_visible_relative_path(Path::new(
            "web/node_modules/x/readme.md"
        )));
        // A heavy name as the last component may be a file (or the folder
        // appearing / disappearing in its parent).
        assert!(is_visible_relative_path(Path::new("web/target")));
        assert!(!is_visible_relative_path(Path::new("../outside.md")));
    }

    #[test]
    fn read_listing_filters_and_sorts() {
        let dir = temp_dir("listing");
        for name in [
            "b10.md",
            "b2.md",
            "Notes.MARKDOWN",
            "image.png",
            ".hidden.md",
            "target",
        ] {
            let _ = fs::write(dir.join(name), "x");
        }
        for name in ["sub", ".git", "node_modules", "Archive"] {
            let _ = fs::create_dir_all(dir.join(name).join("inner"));
        }
        let _ = fs::create_dir_all(dir.join("build").join("target"));

        let listing = read_listing(&dir).unwrap_or_default();
        let names: Vec<&str> = listing.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Archive",
                "build",
                "sub",
                "b2.md",
                "b10.md",
                "image.png",
                "Notes.MARKDOWN",
                "target"
            ]
        );
        let flags: Vec<(bool, bool)> = listing.iter().map(|e| (e.is_dir, e.is_markdown)).collect();
        assert_eq!(flags[0], (true, false));
        assert_eq!(flags[3], (false, true));
        assert_eq!(flags[5], (false, false));
        assert_eq!(flags[6], (false, true));
        assert!(listing.iter().all(|e| e.path == dir.join(&e.name)));

        // A heavy folder name is skipped in nested folders too.
        let nested = read_listing(&dir.join("build")).unwrap_or_else(|_| vec![]);
        assert!(nested.is_empty());

        let missing = read_listing(&dir.join("missing"));
        assert_eq!(missing, Err("Folder not found".to_owned()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folder_is_an_error_not_a_panic() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("unreadable");
        let locked = dir.join("locked");
        let _ = fs::create_dir_all(&locked);
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o000));
        let listing = read_listing(&locked);
        // Root can read anything; only assert when the permission applies.
        if fs::read_dir(&locked).is_err() {
            assert_eq!(listing, Err("Access denied".to_owned()));
        }
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn untitled_names_are_unique() {
        assert_eq!(untitled_file_name(0), "Untitled.md");
        assert_eq!(untitled_file_name(1), "Untitled.md");
        assert_eq!(untitled_file_name(2), "Untitled 2.md");
        assert_eq!(untitled_file_name(12), "Untitled 12.md");

        let dir = temp_dir("untitled");
        let first = create_untitled_markdown(&dir).ok();
        assert_eq!(first, Some(dir.join("Untitled.md")));
        let _ = fs::write(dir.join("Untitled 3.md"), "keep");
        let second = create_untitled_markdown(&dir).ok();
        assert_eq!(second, Some(dir.join("Untitled 2.md")));
        let fourth = create_untitled_markdown(&dir).ok();
        assert_eq!(fourth, Some(dir.join("Untitled 4.md")));
        // Existing files are never overwritten.
        assert_eq!(
            fs::read_to_string(dir.join("Untitled 3.md"))
                .ok()
                .as_deref(),
            Some("keep")
        );
        assert!(create_untitled_markdown(&dir.join("missing")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verbatim_prefixes_are_never_displayed() {
        assert_eq!(
            strip_verbatim_prefix(r"\\?\C:\notes\a.md"),
            r"C:\notes\a.md"
        );
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\server\share\a.md"),
            r"\\server\share\a.md"
        );
        assert_eq!(
            strip_verbatim_prefix(r"\\?\Volume{1234}\a.md"),
            r"\\?\Volume{1234}\a.md"
        );
        assert_eq!(strip_verbatim_prefix(r"C:\plain"), r"C:\plain");
        assert_eq!(strip_verbatim_prefix("/home/u/a.md"), "/home/u/a.md");
        assert_eq!(display_path(Path::new("/tmp/x.md")), "/tmp/x.md");
        assert_eq!(folder_display_name(Path::new("/tmp/notes")), "notes");
        assert_eq!(folder_display_name(Path::new("/")), "/");
    }

    #[test]
    fn normalize_root_is_absolute() {
        let root = normalize_root(Path::new("some/relative"));
        assert!(root.is_absolute());
        assert!(root.ends_with("some/relative"));
        // An already-absolute root is kept as is (on Windows `/abs/dir` is
        // only drive-relative, so use a full path there).
        let absolute = if cfg!(windows) {
            r"C:\abs\dir"
        } else {
            "/abs/dir"
        };
        assert_eq!(normalize_root(Path::new(absolute)), PathBuf::from(absolute));
    }

    #[test]
    fn text_sniffing() {
        assert!(looks_like_text(b"plain text\nline 2"));
        assert!(looks_like_text("ünïcödé".as_bytes()));
        assert!(looks_like_text(b""));
        // A multi-byte character cut off at the end of the sniffed prefix.
        assert!(looks_like_text(&"é".as_bytes()[..1]));
        assert!(!looks_like_text(b"PNG\0\0binary"));
        assert!(!looks_like_text(&[0xff, 0xfe, b'a', b'b']));
    }

    #[test]
    fn check_text_file_rejects_binary_and_large_files() {
        let dir = temp_dir("textcheck");
        let text = dir.join("notes.txt");
        let binary = dir.join("image.png");
        let large = dir.join("big.log");
        let _ = fs::write(&text, "hello");
        let _ = fs::write(&binary, [0x89, b'P', b'N', b'G', 0, 0, 0]);
        if let Ok(file) = fs::File::create(&large) {
            let _ = file.set_len(MAX_TEXT_FILE_BYTES);
        }
        assert_eq!(check_text_file(&text), Ok(()));
        let err = check_text_file(&binary).err().unwrap_or_default();
        assert!(
            err.contains("image.png") && err.contains("text file"),
            "{err}"
        );
        let err = check_text_file(&large).err().unwrap_or_default();
        assert!(err.contains("10 MB"), "{err}");
        assert!(check_text_file(&dir.join("missing.txt")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reveal_is_a_no_op_off_windows() {
        #[cfg(not(windows))]
        assert!(reveal_in_file_explorer(Path::new("/tmp")).is_ok());
    }
}
