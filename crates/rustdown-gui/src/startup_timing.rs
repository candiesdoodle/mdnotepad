//! Cold-start timings: how long a launch takes from the process being
//! created to its first frame on screen, step by step.  Each launch that
//! opens a window appends one line to `%LOCALAPPDATA%\<app id>\startup.log`
//! (the last [`KEPT_LINES`] lines are kept), and one more when it closes:
//! how long closing took.

use std::{
    fmt::Write as _,
    fs,
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui;

use crate::app_identity::{APP_ID, VERSION};

/// Launches kept in the log.
const KEPT_LINES: usize = 50;

/// The steps of one launch, timed from the start of `main`.
pub(crate) struct StartupTiming {
    /// When `main` started.
    main_start: Instant,
    /// How old the process was when `main` started: the time Windows spent
    /// loading the exe and its libraries.
    before_main: Option<Duration>,
    /// Whether the launch opens a file (a double-clicked `.md`).
    opens_file: bool,
    /// Named steps, with the time since `main` started.
    marks: Vec<(&'static str, Duration)>,
    /// The first frame has started.
    first_frame_started: bool,
}

impl StartupTiming {
    /// Start timing; call first thing in `main`.
    #[must_use]
    pub(crate) fn start() -> Self {
        Self {
            main_start: Instant::now(),
            before_main: process_age(),
            opens_file: false,
            marks: Vec::new(),
            first_frame_started: false,
        }
    }

    /// Note whether this launch opens a file.
    pub(crate) const fn set_opens_file(&mut self, opens_file: bool) {
        self.opens_file = opens_file;
    }

    /// Record that `step` has just finished.
    pub(crate) fn mark(&mut self, step: &'static str) {
        self.marks.push((step, self.main_start.elapsed()));
    }

    /// Call at the start of every frame.  Marks the start of the first
    /// frame and, at the start of the second, that the first one is on
    /// screen (eframe shows the window once it has drawn it); then writes
    /// the log and returns `true`, after which the timing can be dropped.
    pub(crate) fn frame(&mut self, ctx: &egui::Context) -> bool {
        if ctx.cumulative_frame_nr() == 0 {
            if !self.first_frame_started {
                self.first_frame_started = true;
                self.mark("first frame starts");
            }
            // The second frame tells when the first one was presented.
            ctx.request_repaint();
            return false;
        }
        self.mark("first frame shown");
        let line = self.log_line(unix_seconds());
        // Off the UI thread: the log is only for reading later.
        let _ = std::thread::Builder::new()
            .name("startup-log".to_owned())
            .spawn(move || append_to_log(&line));
        true
    }

    /// One log line: the time, the version, then each step's time since
    /// the process was created, in milliseconds.
    fn log_line(&self, when_unix: u64) -> String {
        let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
        let before_main = self.before_main.unwrap_or_default();
        let mut line = format!("{when_unix} v{VERSION}");
        if self.opens_file {
            line.push_str(" opening a file");
        }
        let main_starts = self
            .before_main
            .map_or_else(|| "?".to_owned(), |age| format!("{:.0}", ms(age)));
        let _ = write!(line, " | main starts {main_starts}");
        for (step, at) in &self.marks {
            let _ = write!(line, " | {step} {:.0}", ms(before_main + *at));
        }
        line.push_str(" (ms after the process was created)");
        line
    }
}

/// When the window started closing (the first close only).
static CLOSE_STARTED: OnceLock<Instant> = OnceLock::new();

/// Note that the window starts closing now (no unsaved changes are left
/// to ask about).
pub(crate) fn close_started() {
    let _ = CLOSE_STARTED.set(Instant::now());
}

/// Log how long closing took, from [`close_started`] to now (the window
/// is gone and the app dropped); call at the end of `main`.  Written
/// right away: the process is about to exit.
pub(crate) fn log_close() {
    if let Some(started) = CLOSE_STARTED.get() {
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        append_to_log(&format!(
            "{} v{VERSION} | closed in {ms:.0} (ms from close to the end of the app)",
            unix_seconds()
        ));
    }
}

/// How old the process is (Windows only).
#[cfg(windows)]
fn process_age() -> Option<Duration> {
    crate::win_ffi::process_age()
}

#[cfg(not(windows))]
const fn process_age() -> Option<Duration> {
    None
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn log_path() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join(APP_ID).join("startup.log"))
}

fn append_to_log(line: &str) {
    let Some(path) = log_path() else {
        return;
    };
    let old = fs::read_to_string(&path).unwrap_or_default();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(&path, with_line(&old, line, KEPT_LINES));
}

/// `old` log text plus `line`, keeping only the last `kept` lines.
fn with_line(old: &str, line: &str, kept: usize) -> String {
    let mut lines: Vec<&str> = old.lines().filter(|l| !l.is_empty()).collect();
    lines.push(line);
    let first = lines.len().saturating_sub(kept);
    let mut text = lines[first..].join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line_lists_steps_from_process_creation() {
        let timing = StartupTiming {
            main_start: Instant::now(),
            before_main: Some(Duration::from_millis(40)),
            opens_file: true,
            marks: vec![
                ("window created", Duration::from_millis(100)),
                ("first frame shown", Duration::from_millis(250)),
            ],
            first_frame_started: true,
        };
        assert_eq!(
            timing.log_line(7),
            format!(
                "7 v{VERSION} opening a file | main starts 40 | window created 140 \
                 | first frame shown 290 (ms after the process was created)"
            )
        );
    }

    #[test]
    fn log_keeps_the_last_lines() {
        assert_eq!(with_line("", "a", 3), "a\n");
        assert_eq!(with_line("a\nb\nc\n", "d", 3), "b\nc\nd\n");
        assert_eq!(with_line("a\n\nb\n", "c", 5), "a\nb\nc\n");
    }

    #[test]
    fn log_lives_under_local_app_data() {
        if let Some(path) = log_path() {
            assert!(path.ends_with(std::path::Path::new(APP_ID).join("startup.log")));
        }
    }
}
