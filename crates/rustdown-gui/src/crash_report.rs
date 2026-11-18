//! Crash reporting for a GUI app without a console: on a panic, or when the
//! window cannot be created, write `%LOCALAPPDATA%\<app id>\crash.log` and
//! (on Windows) show an error box, so a failure is never silent.  The
//! release profile aborts on panic, so the hook is the only chance to report.

use crate::app_identity::{APP_NAME, VERSION};
use std::{
    backtrace::Backtrace,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Install the panic hook (keeps the default hook's stderr output).
pub(crate) fn install() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map_or_else(String::new, |loc| {
            format!(" at {}:{}", loc.file(), loc.line())
        });
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned());
        let backtrace = Backtrace::force_capture().to_string();
        report(&format!("panicked{location}: {payload}"), &backtrace);
        default_hook(info);
    }));
}

/// Write the crash log and tell the user where it is.
pub(crate) fn report(message: &str, details: &str) {
    let text = format_report(VERSION, unix_seconds(), message, details);
    let written = log_path().and_then(|path| {
        let dir = path.parent()?;
        fs::create_dir_all(dir).ok()?;
        fs::write(&path, &text).ok()?;
        Some(path)
    });
    let where_ = written.map_or_else(
        || "The crash log could not be written.".to_owned(),
        |path| format!("Details were saved to:\n{}", path.display()),
    );
    eprintln!("{text}");
    #[cfg(windows)]
    crate::win_ffi::error_box(
        &format!("{APP_NAME} has stopped"),
        &format!("{APP_NAME} ran into a problem and has to close.\n\n{message}\n\n{where_}"),
    );
    #[cfg(not(windows))]
    let _ = where_;
}

fn log_path() -> Option<PathBuf> {
    Some(crate::app_identity::local_data_dir()?.join("crash.log"))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn format_report(version: &str, when_unix: u64, message: &str, details: &str) -> String {
    format!(
        "{APP_NAME} {version} crash report\ntime (unix seconds): {when_unix}\nos: {} {}\n\n{message}\n\n{details}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_text_has_version_message_and_details() {
        let text = format_report("1.2.3", 42, "panicked at x.rs:1: boom", "frame 0");
        assert!(text.starts_with(&format!("{APP_NAME} 1.2.3 crash report\n")));
        assert!(text.contains("time (unix seconds): 42"));
        assert!(text.contains("panicked at x.rs:1: boom"));
        assert!(text.contains("frame 0"));
    }

    #[test]
    fn log_lives_under_local_app_data() {
        if let Some(path) = log_path() {
            assert!(
                path.ends_with(std::path::Path::new(crate::app_identity::APP_ID).join("crash.log"))
            );
        }
    }
}
