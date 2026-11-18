//! Mermaid diagrams in Preview.  A ```` ```mermaid ```` code block is drawn
//! by Microsoft Edge in the background (headless, with the Mermaid library
//! built into the app, so it works offline), cut to the diagram and kept
//! as a PNG in a cache folder; Preview shows the picture once it is ready
//! and the code until then.  Pictures are kept between runs, so a diagram
//! is drawn once per text and theme.  Print and Save as PDF let the
//! browser draw the diagrams itself (`print`).

use std::{
    collections::HashMap,
    fmt::Write as _,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

use eframe::egui;

use crate::app_identity::APP_ID;

/// The Mermaid library (MIT License, see THIRD-PARTY-NOTICES.md).
pub(crate) static MERMAID_JS: &str = include_str!("../../../assets/mermaid/mermaid.min.js");
/// The library's version: part of each picture's cache key.
const MERMAID_VERSION: &str = "11.4.1";
/// Pixels per point of the pictures (sharp on high-DPI screens).
pub(crate) const SCALE: f32 = 2.0;
/// The page Edge draws on, in CSS pixels: wider diagrams are scaled down
/// to fit, taller ones are cut.
const PAGE_SIZE: (u32, u32) = (1200, 2400);
/// Transparent margin kept around a diagram, in pixels.
const MARGIN: usize = 4;

/// A diagram's picture.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Diagram {
    /// Being drawn.
    Pending,
    /// Drawn: the picture's URI (for `egui::Image`) and its size in points.
    Ready { uri: String, size: egui::Vec2 },
    /// Not drawn: why.
    Failed(String),
}

/// A diagram to draw.
struct Job {
    key: u64,
    code: String,
    dark: bool,
    png: PathBuf,
}

#[derive(Default)]
struct Shared {
    diagrams: HashMap<u64, Diagram>,
    jobs: Option<mpsc::Sender<Job>>,
}

/// Counts diagrams finished, so the editor knows to lay out again.
static FINISHED: AtomicU64 = AtomicU64::new(0);

/// How many diagrams have finished drawing (changes when one does).
#[must_use]
pub(crate) fn version() -> u64 {
    FINISHED.load(Ordering::Acquire)
}

fn shared() -> MutexGuard<'static, Shared> {
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    SHARED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The cache key of `code` drawn in the dark or light theme.
fn key(code: &str, dark: bool) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (code.trim(), dark, MERMAID_VERSION).hash(&mut hasher);
    hasher.finish()
}

/// Where the pictures (and the pages they are drawn from) are kept.
fn cache_folder() -> PathBuf {
    std::env::temp_dir().join(format!("{APP_ID}-mermaid"))
}

/// The `file://` URI egui's loaders read `path` from (as for images in
/// documents, see `default_image_uri_scheme`).
fn file_uri(path: &Path) -> String {
    let mut path = path.to_string_lossy().replace('\\', "/");
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    format!("file://{path}")
}

/// Width and height of the PNG file `path`, from its header.
fn png_size(path: &Path) -> Option<(u32, u32)> {
    let mut header = [0u8; 24];
    std::io::Read::read_exact(&mut std::fs::File::open(path).ok()?, &mut header).ok()?;
    if &header[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    Some((width, height))
}

/// The picture at `png`, if it is there.
fn ready(png: &Path) -> Option<Diagram> {
    let (width, height) = png_size(png)?;
    Some(Diagram::Ready {
        uri: file_uri(png),
        size: egui::vec2(width as f32 / SCALE, height as f32 / SCALE),
    })
}

/// The diagram of `code` in the dark or light theme: drawn, being drawn,
/// or failed.  A new one starts being drawn; `ctx` repaints when it is.
pub(crate) fn diagram(ctx: &egui::Context, code: &str, dark: bool) -> Diagram {
    let key = key(code, dark);
    let mut shared = shared();
    if let Some(diagram) = shared.diagrams.get(&key) {
        return diagram.clone();
    }
    let png = cache_folder().join(format!("{key:016x}.png"));
    let diagram = if let Some(ready) = ready(&png) {
        ready
    } else {
        if shared.jobs.is_none() {
            shared.jobs = start_worker(ctx);
        }
        let job = Job {
            key,
            code: code.to_owned(),
            dark,
            png,
        };
        match &shared.jobs {
            Some(jobs) if jobs.send(job).is_ok() => Diagram::Pending,
            _ => Diagram::Failed("couldn't start drawing".to_owned()),
        }
    };
    shared.diagrams.insert(key, diagram.clone());
    diagram
}

/// The thread that draws diagrams, one at a time.
fn start_worker(ctx: &egui::Context) -> Option<mpsc::Sender<Job>> {
    let (jobs, queue) = mpsc::channel::<Job>();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("mermaid".to_owned())
        .spawn(move || {
            for job in queue {
                let diagram = match draw(&job) {
                    Ok(()) => ready(&job.png)
                        .unwrap_or_else(|| Diagram::Failed("couldn't read the picture".to_owned())),
                    Err(err) => Diagram::Failed(err),
                };
                shared().diagrams.insert(job.key, diagram);
                FINISHED.fetch_add(1, Ordering::AcqRel);
                ctx.request_repaint();
            }
        })
        .ok()?;
    Some(jobs)
}

/// `text` as a JavaScript string literal (safe inside a `<script>`).
fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if c < ' ' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The page Edge draws `code` on: the diagram on a transparent page, or
/// nothing when Mermaid can't read it.
fn page(code: &str, dark: bool) -> String {
    let theme = if dark { "dark" } else { "default" };
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n\
         <style>html, body {{ margin: 0; background: transparent; }}</style>\n\
         <script src=\"mermaid.min.js\"></script>\n</head>\n<body>\n<div id=\"diagram\"></div>\n\
         <script>\n\
         mermaid.initialize({{ startOnLoad: false, theme: \"{theme}\", securityLevel: \"strict\" }});\n\
         mermaid.render(\"svg\", {code})\n\
         .then(({{ svg }}) => {{ document.getElementById(\"diagram\").innerHTML = svg; }})\n\
         .catch(() => {{ document.getElementById(\"diagram\").innerHTML = \"\"; }});\n\
         </script>\n</body>\n</html>\n",
        code = js_string(code)
    )
}

/// `png` (a screenshot with a transparent background) cut to what is
/// drawn on it, with a small margin: the new PNG.
pub(crate) fn crop(png_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|err| err.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "picture too large".to_owned())?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|err| err.to_string())?;
    if info.color_type != png::ColorType::Rgba {
        return Err("unexpected picture".to_owned());
    }
    let (width, height, stride) = (info.width as usize, info.height as usize, info.line_size);
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    for y in 0..height {
        let row = &buffer[y * stride..y * stride + width * 4];
        for (x, pixel) in row.chunks_exact(4).enumerate() {
            if pixel[3] != 0 {
                left = left.min(x);
                right = right.max(x + 1);
                top = top.min(y);
                bottom = bottom.max(y + 1);
            }
        }
    }
    if right <= left || bottom <= top {
        return Err("Mermaid couldn't read it".to_owned());
    }
    let (left, top) = (left.saturating_sub(MARGIN), top.saturating_sub(MARGIN));
    let (right, bottom) = ((right + MARGIN).min(width), (bottom + MARGIN).min(height));
    let mut rgba = Vec::with_capacity((right - left) * (bottom - top) * 4);
    for y in top..bottom {
        rgba.extend_from_slice(&buffer[y * stride + left * 4..y * stride + right * 4]);
    }
    crate::clipboard::encode_png((right - left) as u32, (bottom - top) as u32, &rgba)
        .ok_or_else(|| "couldn't save the picture".to_owned())
}

/// Draw `job` with Edge: its picture is written to `job.png`.
#[cfg(windows)]
fn draw(job: &Job) -> Result<(), String> {
    let folder = cache_folder();
    std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
    let script = folder.join("mermaid.min.js");
    if !std::fs::metadata(&script).is_ok_and(|meta| meta.len() == MERMAID_JS.len() as u64) {
        std::fs::write(&script, MERMAID_JS).map_err(|err| err.to_string())?;
    }
    let page_path = folder.join("diagram.html");
    std::fs::write(&page_path, page(&job.code, job.dark)).map_err(|err| err.to_string())?;
    let shot = folder.join("diagram.png");
    // Where Edge puts a screenshot when it ignores the name it was given.
    let fallback = folder.join("screenshot.png");
    let screenshot = format!("--screenshot={}", shot.display());
    let window = format!("--window-size={},{}", PAGE_SIZE.0, PAGE_SIZE.1);
    let scale = format!("--force-device-scale-factor={SCALE}");
    let url = crate::print::file_url_of(&page_path);
    // Edge's headless modes: its default, then the new one by name.
    for headless in ["--headless", "--headless=new"] {
        let _ = std::fs::remove_file(&shot);
        let _ = std::fs::remove_file(&fallback);
        let args: [&str; 7] = [
            headless,
            &screenshot,
            &window,
            &scale,
            "--default-background-color=00000000",
            "--hide-scrollbars",
            "--virtual-time-budget=10000",
        ];
        crate::print::run_edge(&args, &url, &folder)?;
        let found = crate::print::wait_for_file(&[shot.clone(), fallback.clone()]);
        if let Some(bytes) = found.and_then(|path| std::fs::read(path).ok()) {
            let png = crop(&bytes)?;
            return std::fs::write(&job.png, png).map_err(|err| err.to_string());
        }
    }
    Err(format!(
        "Microsoft Edge didn't draw it (it said: {})",
        crate::print::edge_said(&folder)
    ))
}

#[cfg(not(windows))]
fn draw(_job: &Job) -> Result<(), String> {
    Err("diagrams are drawn on Windows only".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagrams_go_into_the_page_safely() {
        assert_eq!(
            js_string("a\"b\\c\n</script>"),
            r#""a\"b\\c\n\u003c/script>""#
        );
        let page = page("graph TD\n  A --> B", true);
        assert!(page.contains("theme: \"dark\""));
        assert!(page.contains(r#"mermaid.render("svg", "graph TD\n  A --> B")"#));
        assert!(!page.contains("A --> B</"));
    }

    #[test]
    fn screenshots_are_cut_to_the_diagram() {
        // 10×8 transparent, with an opaque 2×3 block at (5, 2).
        let (width, height) = (10u32, 8u32);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for y in 2..5 {
            for x in 5..7 {
                rgba[((y * width + x) * 4 + 3) as usize] = 255;
            }
        }
        let png = crate::clipboard::encode_png(width, height, &rgba).unwrap_or_default();
        let cut = crop(&png).unwrap_or_default();
        let decoder = png::Decoder::new(std::io::Cursor::new(&cut));
        let info = decoder.read_info().map(|reader| {
            let info = reader.info();
            (info.width, info.height)
        });
        // The block, plus the margin where the picture has room for it.
        assert_eq!(info.ok(), Some((2 + 4 + 3, 3 + 2 + 3)));
        let empty = crate::clipboard::encode_png(2, 2, &[0; 16]).unwrap_or_default();
        assert!(
            crop(&empty).is_err(),
            "nothing drawn: Mermaid couldn't read it"
        );

        // A picture's size comes from its header.
        let path =
            std::env::temp_dir().join(format!("mdnotepad-png-size-{}.png", std::process::id()));
        assert!(std::fs::write(&path, &cut).is_ok());
        assert_eq!(png_size(&path), Some((9, 8)));
        let Some(Diagram::Ready { size, .. }) = ready(&path) else {
            unreachable!("a picture");
        };
        assert_eq!(size, egui::vec2(4.5, 4.0));
        let _ = std::fs::remove_file(&path);
    }
}
