//! Command-line argument parsing, version output, and platform workarounds.

use std::{ffi::OsString, path::PathBuf};

use super::{DIAGNOSTICS_DEFAULT_ITERATIONS, DIAGNOSTICS_DEFAULT_RUNS, Mode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOptions {
    pub mode: Mode,
    /// `true` when the user explicitly chose a mode via CLI flag (`-e`,
    /// `-r`, `-s`, `-p`).
    pub mode_explicit: bool,
    pub path: Option<PathBuf>,
    pub print_version: bool,
    pub diagnostics: DiagnosticsMode,
    pub diagnostics_iterations: usize,
    pub diagnostics_runs: usize,
    /// Positional paths after the first; opened in tabs too.
    pub extra_paths: Vec<PathBuf>,
    /// `--new-window`: open a window of our own instead of handing the
    /// paths to an already running window (Windows single instance).
    pub new_window: bool,
    /// `--register-file-types` / `--unregister-file-types`: change the
    /// Windows `.md` file association, print the result, and exit.
    pub file_types: Option<FileTypesAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileTypesAction {
    Register,
    Unregister,
    /// `--register-preview` / `--unregister-preview`: only the preview
    /// pane's handler (the installer).
    RegisterPreview,
    UnregisterPreview,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagnosticsMode {
    #[default]
    Off,
    OpenPipeline,
    #[cfg(debug_assertions)]
    NavPipeline,
}

/// Parse a `--long-name=VALUE` or `--short-name=VALUE` positive integer argument.
fn parse_kv_usize(arg: &OsString, long: &str, short: &str) -> Option<usize> {
    arg.to_str()
        .and_then(|s| s.strip_prefix(short).or_else(|| s.strip_prefix(long)))
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
}

#[must_use]
pub fn parse_launch_options<I, S>(args: I) -> LaunchOptions
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut mode = None;
    let mut path = None;
    let mut extra_paths = Vec::new();
    let mut new_window = false;
    let mut file_types = None;
    let mut print_version = false;
    let mut diagnostics = DiagnosticsMode::Off;
    let mut diagnostics_iterations = DIAGNOSTICS_DEFAULT_ITERATIONS;
    let mut diagnostics_runs = DIAGNOSTICS_DEFAULT_RUNS;
    let mut parse_flags = true;

    for arg in args {
        let arg = arg.into();
        if arg == "-v" || arg == "--version" {
            print_version = true;
            continue;
        }
        if parse_flags {
            if arg == "--" {
                parse_flags = false;
                continue;
            }
            let flag_mode = match arg.to_str() {
                Some("-e") => Some(Mode::Edit),
                Some("-r") => Some(Mode::Raw),
                Some("-s") => Some(Mode::SideBySide),
                Some("-p") => Some(Mode::Preview),
                _ => None,
            };
            if flag_mode.is_some() {
                mode = flag_mode;
                continue;
            }
            if arg == "--new-window" {
                new_window = true;
                continue;
            }
            if arg == "--register-file-types" {
                file_types = Some(FileTypesAction::Register);
                continue;
            }
            if arg == "--unregister-file-types" {
                file_types = Some(FileTypesAction::Unregister);
                continue;
            }
            if arg == "--register-preview" {
                file_types = Some(FileTypesAction::RegisterPreview);
                continue;
            }
            if arg == "--unregister-preview" {
                file_types = Some(FileTypesAction::UnregisterPreview);
                continue;
            }
            if arg == "--diagnostics-open" || arg == "--diag-open" {
                diagnostics = DiagnosticsMode::OpenPipeline;
                continue;
            }
            #[cfg(debug_assertions)]
            if arg == "--diagnostics-nav" || arg == "--diag-nav" {
                diagnostics = DiagnosticsMode::NavPipeline;
                continue;
            }
            if let Some(v) = parse_kv_usize(&arg, "--diagnostics-iterations=", "--diag-iterations=")
            {
                diagnostics_iterations = v;
                continue;
            }
            if let Some(v) = parse_kv_usize(&arg, "--diagnostics-runs=", "--diag-runs=") {
                diagnostics_runs = v;
                continue;
            }
            if arg.to_str().is_some_and(|value| value.starts_with('-')) {
                continue;
            }
        }

        if path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            extra_paths.push(PathBuf::from(arg));
        }
    }

    let mode_explicit = mode.is_some();
    let mode = mode.unwrap_or_else(|| {
        // A folder argument opens the Explorer, not a document to preview;
        // a plain text file opens as plain text.
        match path.as_deref() {
            Some(path) if crate::is_text_path(path) => Mode::Raw,
            Some(path) if !path.is_dir() => Mode::Preview,
            _ => Mode::Edit,
        }
    });

    LaunchOptions {
        mode,
        mode_explicit,
        path,
        print_version,
        diagnostics,
        diagnostics_iterations,
        diagnostics_runs,
        extra_paths,
        new_window,
        file_types,
    }
}

/// Attach to the parent process's console so that `println!` output is
/// visible when the GUI-subsystem binary is invoked from `PowerShell` or cmd.
#[cfg(windows)]
pub fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    // SAFETY: `AttachConsole` is a well-documented Win32 API.  Calling it
    // with ATTACH_PARENT_PROCESS is harmless even if there is no parent
    // console — it simply returns FALSE.
    #[allow(unsafe_code)]
    {
        unsafe extern "system" {
            safe fn AttachConsole(process_id: u32) -> i32;
        }
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> LaunchOptions {
        parse_launch_options(args.iter().copied())
    }

    #[test]
    fn windows_integration_flags() {
        let options = parse(&[]);
        assert!(!options.new_window);
        assert_eq!(options.file_types, None);
        assert!(options.extra_paths.is_empty());

        let options = parse(&["--new-window", "a.md"]);
        assert!(options.new_window);
        assert_eq!(options.path, Some(PathBuf::from("a.md")));

        let options = parse(&["--register-file-types"]);
        assert_eq!(options.file_types, Some(FileTypesAction::Register));
        assert_eq!(options.path, None);
        let options = parse(&["--unregister-file-types"]);
        assert_eq!(options.file_types, Some(FileTypesAction::Unregister));
        let options = parse(&["--register-preview"]);
        assert_eq!(options.file_types, Some(FileTypesAction::RegisterPreview));
        let options = parse(&["--unregister-preview"]);
        assert_eq!(options.file_types, Some(FileTypesAction::UnregisterPreview));

        // After `--` they are file names.
        let options = parse(&["--", "--new-window"]);
        assert!(!options.new_window);
        assert_eq!(options.path, Some(PathBuf::from("--new-window")));
    }

    #[test]
    fn every_positional_path_is_kept() {
        let options = parse(&["a.md", "-p", "b.md", "c.md"]);
        assert_eq!(options.path, Some(PathBuf::from("a.md")));
        assert_eq!(
            options.extra_paths,
            vec![PathBuf::from("b.md"), PathBuf::from("c.md")]
        );
        assert_eq!(options.mode, Mode::Preview);
    }
}
