//! File → Print… and File → Save as PDF…: the document as a web page, from
//! the same HTML as Edit → Copy as formatted text, with print styles.
//!
//! - Print opens the page in the default browser, which shows its print
//!   dialog as it loads (a printer, or "Save as PDF").
//! - Save as PDF asks for a file name, then Microsoft Edge (on every
//!   Windows 10 and 11 PC) prints the page to it in the background, without
//!   a window; a note says when it is done.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::mpsc,
};

use eframe::egui;

use crate::{RustdownApp, app_identity::APP_ID};

/// Print styles: the preview's look on paper.
const PAGE_STYLE: &str = "\
@page { margin: 18mm; }
body { font-family: 'Segoe UI', system-ui, sans-serif; font-size: 11pt; line-height: 1.5;
  color: #1b1b1b; margin: 0; }
h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin: 1.2em 0 0.5em; break-after: avoid; }
p, ul, ol, blockquote, pre, table { margin: 0 0 0.8em; }
code { font-family: Consolas, 'Cascadia Mono', monospace; font-size: 0.92em;
  background: #f3f3f3; padding: 0 3px; border-radius: 3px; }
pre { background: #f5f5f5; padding: 10px 12px; border-radius: 4px; white-space: pre-wrap;
  break-inside: avoid; }
pre code { background: none; padding: 0; }
blockquote { border-left: 3px solid #c8c8c8; margin-left: 0; padding-left: 12px; color: #555; }
table { border-collapse: collapse; break-inside: avoid; }
th { background: #f3f3f3; }
img { max-width: 100%; }
a { color: #0b5cad; }
hr { border: none; border-top: 1px solid #c8c8c8; }
";

/// `markdown` as a complete web page titled `title`.  `folder` (the
/// document's) is the base of relative links and images; `print_on_load`
/// opens the browser's print dialog once everything has loaded.
#[must_use]
pub(crate) fn html_page(
    markdown: &str,
    title: &str,
    folder: Option<&Path>,
    print_on_load: bool,
) -> String {
    let mut page = String::from("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n");
    let _ = writeln!(page, "<title>{}</title>", escape(title));
    if let Some(folder) = folder {
        let _ = writeln!(page, "<base href=\"{}\">", escape(&file_url(folder)));
    }
    let _ = writeln!(page, "<style>\n{PAGE_STYLE}</style>");
    let body = crate::md::to_html(markdown);
    // Mermaid diagrams, drawn by the browser before printing.
    let diagrams = body.contains("<pre class=\"mermaid\">");
    if diagrams {
        let _ = writeln!(
            page,
            "<script src=\"{}mermaid.min.js\"></script>\n\
             <script>mermaid.initialize({{ startOnLoad: false, securityLevel: 'strict' }});</script>",
            file_url(&work_folder())
        );
    }
    if print_on_load || diagrams {
        let draw = if diagrams { "await mermaid.run();" } else { "" };
        let print = if print_on_load {
            "setTimeout(() => print(), 200);"
        } else {
            ""
        };
        let _ = writeln!(
            page,
            "<script>addEventListener('load', async () => {{ {draw} {print} }});</script>"
        );
    }
    page.push_str("</head>\n<body>\n");
    page.push_str(&body);
    page.push_str("</body>\n</html>\n");
    page
}

/// `text` with the characters that mean something in HTML escaped.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// The `file:` URL of folder `folder` (ending in `/`).
#[must_use]
pub(crate) fn file_url(folder: &Path) -> String {
    let path = folder.to_string_lossy().replace('\\', "/");
    let mut url = if path.starts_with("//") {
        // A network share: file://server/share/...
        String::from("file:")
    } else if path.starts_with('/') {
        String::from("file://")
    } else {
        String::from("file:///")
    };
    for c in path.chars() {
        match c {
            ' ' => url.push_str("%20"),
            '#' => url.push_str("%23"),
            '%' => url.push_str("%25"),
            '?' => url.push_str("%3F"),
            _ => url.push(c),
        }
    }
    if !url.ends_with('/') {
        url.push('/');
    }
    url
}

/// The folder for the pages handed to the browser or to Edge.
fn work_folder() -> PathBuf {
    std::env::temp_dir().join(format!("{APP_ID}-print"))
}

/// Write `page` to `name` in the work folder (emptied of earlier pages).
fn write_page(page: &str, name: &str) -> std::io::Result<PathBuf> {
    let folder = work_folder();
    std::fs::create_dir_all(&folder)?;
    if let Ok(entries) = std::fs::read_dir(&folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "html") {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    if page.contains("mermaid.min.js") {
        let script = folder.join("mermaid.min.js");
        let have = std::fs::metadata(&script).map_or(0, |meta| meta.len());
        if have != crate::mermaid::MERMAID_JS.len() as u64 {
            std::fs::write(script, crate::mermaid::MERMAID_JS)?;
        }
    }
    let path = folder.join(name);
    std::fs::write(&path, page)?;
    Ok(path)
}

/// A file name made of `title` (the tab's) without characters Windows
/// does not allow, and its Markdown extension.
fn file_stem(title: &str) -> String {
    let stem = Path::new(title).file_stem().map_or_else(
        || title.to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let clean: String = stem
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "Untitled".to_owned()
    } else {
        clean.to_owned()
    }
}

/// Microsoft Edge's program file: from its App Paths registration, else
/// where it installs.
#[cfg(windows)]
fn edge_path() -> Option<PathBuf> {
    use winreg::{
        RegKey,
        enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
    };
    const APP_PATH: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\msedge.exe";
    let registered = [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
        .into_iter()
        .filter_map(|root| RegKey::predef(root).open_subkey(APP_PATH).ok())
        .filter_map(|key| key.get_value::<String, _>("").ok())
        .map(PathBuf::from);
    let installed = ["ProgramFiles(x86)", "ProgramFiles"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join(r"Microsoft\Edge\Application\msedge.exe"));
    registered.chain(installed).find(|path| path.is_file())
}

/// The `file:` URL of the file `path`.
#[must_use]
pub(crate) fn file_url_of(path: &Path) -> String {
    let folder = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    let name = path.file_name().map_or_else(String::new, |name| {
        name.to_string_lossy().replace(' ', "%20")
    });
    format!("{}{name}", file_url(&folder))
}

/// Run Microsoft Edge without a window on the page at `url` with `args`
/// (which say the headless mode), and wait for it.  `folder` holds its
/// profile (`edge-profile`) and its output (`edge.log`), and is its
/// working folder.
#[cfg(windows)]
pub(crate) fn run_edge(args: &[&str], url: &str, folder: &Path) -> Result<(), String> {
    use std::{
        os::windows::process::CommandExt,
        process::Stdio,
        time::{Duration, Instant},
    };
    /// `CREATE_NO_WINDOW`: no console window flashes up.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const TIMEOUT: Duration = Duration::from_secs(90);

    let edge = edge_path().ok_or_else(|| "Microsoft Edge wasn't found".to_owned())?;
    let log = std::fs::File::create(folder.join("edge.log")).ok();
    let (out, err) = match log
        .as_ref()
        .and_then(|log| Some((log.try_clone().ok()?, log.try_clone().ok()?)))
    {
        Some((out, err)) => (Stdio::from(out), Stdio::from(err)),
        None => (Stdio::null(), Stdio::null()),
    };
    let mut child = std::process::Command::new(edge)
        .args([
            "--disable-gpu",
            "--disable-extensions",
            "--no-first-run",
            "--no-default-browser-check",
        ])
        .arg(format!(
            "--user-data-dir={}",
            folder.join("edge-profile").display()
        ))
        .args(args)
        .arg(url)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|err| format!("couldn't start Microsoft Edge: {err}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                return Err("Microsoft Edge took too long".to_owned());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(err) => return Err(format!("Microsoft Edge failed: {err}")),
        }
    }
}

/// The first of `paths` that Edge wrote, once it is complete: Edge may
/// hand the work to another of its processes and exit before the file is
/// written, so this waits for it a while.
pub(crate) fn wait_for_file(paths: &[PathBuf]) -> Option<PathBuf> {
    use std::time::{Duration, Instant};
    const WAIT: Duration = Duration::from_secs(20);
    let started = Instant::now();
    let mut last_size = None;
    loop {
        let found = paths
            .iter()
            .find_map(|path| Some((path, std::fs::metadata(path).ok()?.len())))
            .filter(|&(_, size)| size > 0);
        if let Some((path, size)) = found {
            // Done when its size stopped changing.
            if last_size == Some(size) {
                return Some(path.clone());
            }
            last_size = Some(size);
        }
        if started.elapsed() > WAIT {
            return found.map(|(path, _)| path.clone());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The last line Edge wrote to its log in `folder` (see [`run_edge`]).
#[must_use]
pub(crate) fn edge_said(folder: &Path) -> String {
    let log = std::fs::read_to_string(folder.join("edge.log")).unwrap_or_default();
    let line = log
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("nothing");
    line.chars().take(200).collect()
}

/// Print `page` (a file) to the PDF `target` with Edge, without a window.
#[cfg(windows)]
fn edge_print_to_pdf(page: &Path, target: &Path) -> Result<(), String> {
    if edge_path().is_none() {
        return Err(
            "Save as PDF needs Microsoft Edge, which wasn't found. Use Print… and \
                    choose “Save as PDF” instead."
                .to_owned(),
        );
    }
    let folder = work_folder();
    let output = folder.join("export.pdf");
    let _ = std::fs::remove_file(&output);
    let print_to = format!("--print-to-pdf={}", output.display());
    let args: [&str; 5] = [
        "--headless",
        "--no-pdf-header-footer",
        "--print-to-pdf-no-header",
        // Time for Mermaid diagrams to be drawn before printing.
        "--virtual-time-budget=10000",
        &print_to,
    ];
    run_edge(&args, &file_url_of(page), &folder)?;
    if wait_for_file(std::slice::from_ref(&output)).is_none() {
        return Err(format!(
            "Microsoft Edge didn't make the PDF (it said: {}).",
            edge_said(&folder)
        ));
    }
    std::fs::copy(&output, target).map_err(|err| format!("Couldn't save the PDF: {err}"))?;
    let _ = std::fs::remove_file(&output);
    Ok(())
}

#[cfg(not(windows))]
fn edge_print_to_pdf(_page: &Path, _target: &Path) -> Result<(), String> {
    Err("Save as PDF works on Windows only.".to_owned())
}

impl RustdownApp {
    /// The active document as a page: its text, title and folder.
    fn page_of_active_tab(&self, print_on_load: bool) -> (String, String) {
        let tab = self.tab();
        let title = tab.title().into_owned();
        let folder = tab.doc.path.as_deref().and_then(Path::parent);
        let page = html_page(tab.doc.text.as_str(), &title, folder, print_on_load);
        (page, file_stem(&title))
    }

    /// File → Print….
    pub(crate) fn print_document(&mut self, ctx: &egui::Context) {
        let (page, stem) = self.page_of_active_tab(true);
        let opened = write_page(&page, &format!("{stem}.html")).and_then(|path| {
            #[cfg(windows)]
            {
                crate::win_ffi::shell_open(&path.to_string_lossy())
            }
            #[cfg(not(windows))]
            {
                ctx.open_url(egui::OpenUrl::new_tab(format!(
                    "{}{stem}.html",
                    file_url(path.parent().unwrap_or(&path))
                )));
                Ok(())
            }
        });
        match opened {
            Ok(()) => self.notify(ctx, "Opening the print dialog in your browser…"),
            Err(err) => self.tab_mut().error = Some(format!("Couldn't print: {err}")),
        }
    }

    /// File → Save as PDF….
    pub(crate) fn export_pdf(&mut self, ctx: &egui::Context) {
        if self.pdf_export.is_some() {
            self.notify(ctx, "A PDF is still being made.");
            return;
        }
        let (page, stem) = self.page_of_active_tab(false);
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}.pdf"));
        if let Some(folder) = self.tab().doc.path.as_deref().and_then(Path::parent) {
            dialog = dialog.set_directory(folder);
        }
        let Some(target) = dialog.save_file() else {
            return;
        };
        let page_path = match write_page(&page, &format!("{stem}.html")) {
            Ok(path) => path,
            Err(err) => {
                self.tab_mut().error = Some(format!("Couldn't make the PDF: {err}"));
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = edge_print_to_pdf(&page_path, &target).map(|()| target);
            let _ = tx.send(result);
            repaint.request_repaint();
        });
        self.pdf_export = Some(rx);
        self.notify(ctx, "Making the PDF…");
    }

    /// Say when a PDF being made is done (each frame).
    pub(crate) fn poll_pdf_export(&mut self, ctx: &egui::Context) {
        let Some(result) = self.pdf_export.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        self.pdf_export = None;
        match result {
            Ok(path) => {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                self.notify(ctx, &format!("Saved {name}."));
            }
            Err(err) => self.tab_mut().error = Some(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_has_the_title_folder_styles_and_document() {
        let page = html_page(
            "# Notes\n\n![x](img/a.png)",
            "a <b> & c.md",
            Some(Path::new("/home/me/my docs")),
            true,
        );
        for expected in [
            "<title>a &lt;b&gt; &amp; c.md</title>",
            "<base href=\"file:///home/me/my%20docs/\">",
            "@page",
            "print()",
            "<h1 style=\"font-size:2em;font-weight:bold\">Notes</h1>",
            "src=\"img/a.png\"",
        ] {
            assert!(
                page.contains(expected),
                "{expected:?} missing from:\n{page}"
            );
        }
        let plain = html_page("text", "t", None, false);
        assert!(!plain.contains("print()") && !plain.contains("<base"));
        let diagram = html_page("```mermaid\ngraph TD\n  A --> B\n```\n", "t", None, false);
        for expected in [
            "mermaid.min.js\"></script>",
            "await mermaid.run();",
            "<pre class=\"mermaid\">graph TD\n  A --&gt; B</pre>",
        ] {
            assert!(
                diagram.contains(expected),
                "{expected:?} missing from:\n{diagram}"
            );
        }
    }

    #[test]
    fn folders_become_file_urls() {
        assert_eq!(
            file_url(Path::new(r"C:\Users\Me\Notes #1")),
            "file:///C:/Users/Me/Notes%20%231/"
        );
        assert_eq!(
            file_url(Path::new(r"\\server\share\docs")),
            "file://server/share/docs/"
        );
        assert_eq!(file_url(Path::new("/tmp/x/")), "file:///tmp/x/");
    }

    #[test]
    fn file_stems_drop_extensions_and_bad_characters() {
        assert_eq!(file_stem("notes.md"), "notes");
        assert_eq!(file_stem("a?b*.md"), "a_b_");
        assert_eq!(file_stem("Untitled"), "Untitled");
        assert_eq!(file_stem("  "), "Untitled");
    }
}
