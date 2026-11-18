//! The editor's clipboard extras.
//!
//! - Edit → Copy as formatted text (Ctrl+Shift+C): the selection, or the
//!   whole document, as HTML (for email and Word) together with the
//!   Markdown as plain text.
//! - Pasting an image (Edit → Paste image, or Ctrl+V when the clipboard
//!   holds an image and no text): a dialog shows it and offers to save
//!   it; the user picks the file (suggested: the folder of the file being
//!   edited, else the folder open in the folder tree, else Documents, and
//!   a name), then a link to it is typed at
//!   the caret (undoable like typing): relative when the image is in the
//!   document's folder or below, else a `file:///` URL.

use std::{path::Path, sync::Arc};

use eframe::egui;

use crate::RustdownApp;

/// The pixels of a 24- or 32-bit device-independent bitmap (`dib`: a
/// `BITMAPINFOHEADER`, colour masks for `BI_BITFIELDS`, then the rows) as
/// top-down RGBA, with the width and height.  `None` for other formats.
pub(crate) fn dib_to_rgba(dib: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?))
    };
    let header_size = u32_at(0)? as usize;
    let width = i32::from_le_bytes(dib.get(4..8)?.try_into().ok()?);
    let height = i32::from_le_bytes(dib.get(8..12)?.try_into().ok()?);
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32_at(16)?;
    let palette_entries = u32_at(32)? as usize;
    let pixel_bytes = match (bits, compression) {
        (24, BI_RGB) => 3,
        (32, BI_RGB | BI_BITFIELDS) => 4,
        _ => return None,
    };
    if width <= 0 || height == 0 {
        return None;
    }
    let masks = if compression == BI_BITFIELDS && header_size == 40 {
        12
    } else {
        0
    };
    let start = header_size + masks + palette_entries * 4;
    let width_px = width.unsigned_abs() as usize;
    let height_px = height.unsigned_abs() as usize;
    // Rows are padded to whole 32-bit words.
    let stride = (width_px * pixel_bytes).div_ceil(4) * 4;
    let rows = dib.get(start..start + stride * height_px)?;
    let mut rgba = Vec::with_capacity(width_px * height_px * 4);
    for y in 0..height_px {
        // Bottom-up unless the height is negative.
        let row = if height > 0 { height_px - 1 - y } else { y };
        let row = &rows[row * stride..row * stride + width_px * pixel_bytes];
        for pixel in row.chunks_exact(pixel_bytes) {
            let alpha = if pixel_bytes == 4 { pixel[3] } else { 255 };
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], alpha]);
        }
    }
    // Most 32-bit clipboard bitmaps leave alpha at 0: then it is opaque.
    if pixel_bytes == 4 && rgba.chunks_exact(4).all(|pixel| pixel[3] == 0) {
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    Some((
        u32::try_from(width_px).ok()?,
        u32::try_from(height_px).ok()?,
        rgba,
    ))
}

/// `rgba` (`width` × `height` pixels) as the bytes of a PNG file.
pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// A pasted image waiting for the user to save it (see
/// [`RustdownApp::show_image_paste_dialog`]).
pub(crate) struct PendingImage {
    /// The image as a PNG file.
    png: Arc<[u8]>,
    /// Its name in egui's image cache, for the thumbnail.
    uri: String,
}

/// The suggested file name for an image pasted into the document `doc`
/// (`None`: Untitled): `<document name or "image">-<stamp>.png`, then
/// `-2`, `-3`… while taken in `folder`.
fn suggested_name(folder: Option<&Path>, doc: Option<&Path>, stamp: &str) -> String {
    let stem = doc.and_then(Path::file_stem).map_or_else(
        || "image".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    (1..1000)
        .map(|n| {
            if n == 1 {
                format!("{stem}-{stamp}.png")
            } else {
                format!("{stem}-{stamp}-{n}.png")
            }
        })
        .find(|name| folder.is_none_or(|folder| !folder.join(name).exists()))
        .unwrap_or_else(|| format!("{stem}-{stamp}.png"))
}

/// How the document `doc` (`None`: Untitled) refers to the image saved at
/// `image`: relative, with `/`, when it is in the document's folder or
/// below; otherwise a `file:///` URL.
pub(crate) fn image_reference(doc: Option<&Path>, image: &Path) -> String {
    if let Some(folder) = doc.and_then(Path::parent)
        && let Ok(relative) = image.strip_prefix(folder)
    {
        return relative.to_string_lossy().replace('\\', "/");
    }
    let absolute = image.to_string_lossy().replace('\\', "/");
    if absolute.starts_with('/') {
        format!("file://{absolute}")
    } else {
        format!("file:///{absolute}")
    }
}

/// The Markdown showing the image at `reference` (see
/// [`image_reference`]); one with spaces or brackets goes in angle
/// brackets.
pub(crate) fn image_link(name: &str) -> String {
    if name.contains([' ', '(', ')']) {
        format!("![](<{name}>)")
    } else {
        format!("![]({name})")
    }
}

impl RustdownApp {
    /// Edit → Copy as formatted text.
    pub(crate) fn copy_formatted(&mut self, ctx: &egui::Context) {
        let text = self.tab().doc.text.clone();
        let selected = if self.mode.uses_editor() {
            self.editor_selection(ctx)
        } else {
            0..0
        };
        let markdown = if selected.is_empty() {
            text.as_str()
        } else {
            let start = crate::edit_ops::char_to_byte(&text, selected.start);
            let end = crate::edit_ops::char_to_byte(&text, selected.end);
            text.get(start..end).unwrap_or(text.as_str())
        };
        let html = crate::md::clipboard_html(&crate::md::to_html(markdown));
        #[cfg(windows)]
        match crate::win_ffi::set_clipboard_html_and_text(&html, markdown) {
            Ok(()) => self.notify(ctx, "Copied as formatted text."),
            Err(err) => self.tab_mut().error = Some(format!("Couldn't copy: {err}")),
        }
        #[cfg(not(windows))]
        {
            let _ = html;
            ctx.copy_text(markdown.to_owned());
        }
    }

    /// Take the clipboard's image for the paste dialog (see the module
    /// docs).  `asked`: the menu item, which says when there is none;
    /// Ctrl+V with no image says nothing.
    pub(crate) fn paste_image(&mut self, ctx: &egui::Context, asked: bool) {
        if !self.mode.uses_editor() {
            return;
        }
        #[cfg(windows)]
        {
            let Some(image) = crate::win_ffi::clipboard_image() else {
                if asked {
                    self.notify(ctx, "There is no image on the clipboard.");
                }
                return;
            };
            let png = match image {
                crate::win_ffi::ClipboardImage::Png(bytes) => Some(bytes),
                crate::win_ffi::ClipboardImage::Dib(dib) => dib_to_rgba(&dib)
                    .and_then(|(width, height, rgba)| encode_png(width, height, &rgba)),
            };
            let Some(png) = png else {
                self.notify(
                    ctx,
                    "The image on the clipboard is in a format MdNotePad can't read.",
                );
                return;
            };
            let uri = format!(
                "bytes://pasted-image-{}.png",
                ctx.input(|input| input.time).to_bits()
            );
            self.pending_image = Some(PendingImage {
                png: png.into(),
                uri,
            });
            ctx.request_repaint();
        }
        #[cfg(not(windows))]
        {
            let _ = (ctx, asked);
        }
    }

    /// The paste dialog: the image's thumbnail, Save image… and Cancel.
    pub(crate) fn show_image_paste_dialog(&mut self, ctx: &egui::Context) {
        use crate::dialog::Role;

        let Some(pending) = &self.pending_image else {
            return;
        };
        let (png, uri) = (pending.png.clone(), pending.uri.clone());
        let answer = crate::dialog::show(
            ctx,
            "paste_image",
            "Image pasted",
            |ui| {
                ui.label("Save the image to insert a link to it in the document.");
                ui.add(
                    egui::Image::from_bytes(uri.clone(), png.clone())
                        .max_size(egui::vec2(408.0, 240.0))
                        .corner_radius(4.0),
                );
            },
            &[("Save image…", Role::Primary), ("Cancel", Role::Secondary)],
            Some(1),
        );
        let Some(answer) = answer else {
            return;
        };
        self.pending_image = None;
        ctx.forget_image(&uri);
        if answer == 0 {
            self.save_pasted_image(ctx, &png);
        }
    }

    /// Ask where to save the pasted image (`png`), save it there and type
    /// the link to it.
    fn save_pasted_image(&mut self, ctx: &egui::Context, png: &[u8]) {
        let doc = self.tab().doc.path.clone();
        // The folder of the file being edited; for an Untitled document
        // (or a folder that can't be reached) the folder open in the folder
        // tree; else Documents.
        let tree = self
            .explorer
            .tree
            .as_ref()
            .map(|tree| tree.root().to_path_buf());
        let folder = doc
            .as_deref()
            .and_then(Path::parent)
            .filter(|folder| folder.is_dir())
            .map(Path::to_path_buf)
            .or_else(|| tree.filter(|folder| folder.is_dir()))
            .or_else(dirs::document_dir);
        let now = crate::edit_ops::now_local();
        let stamp = format!(
            "{:04}{:02}{:02}-{:02}{:02}",
            now.year, now.month, now.day, now.hour, now.minute
        );
        let name = suggested_name(folder.as_deref(), doc.as_deref(), &stamp);
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PNG image", &["png"])
            .set_file_name(name);
        if let Some(folder) = &folder {
            dialog = dialog.set_directory(folder);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("png");
        }
        if let Err(err) = std::fs::write(&path, png) {
            self.tab_mut().error = Some(format!("Couldn't save the image: {err}"));
            return;
        }
        let reference = image_reference(doc.as_deref(), &path);
        self.queue_editor_event(ctx, egui::Event::Text(image_link(&reference)));
        let name = path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        self.notify(ctx, &format!("Image saved as {name}."));
    }

    /// Ctrl+V with an image (and no text) on the clipboard: egui then sees
    /// no paste at all, so the keys themselves are watched (Windows).
    pub(crate) fn watch_image_paste(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            let down = crate::win_ffi::ctrl_v_down();
            let pressed = down && !self.ctrl_v_down;
            self.ctrl_v_down = down;
            if !pressed || !self.mode.uses_editor() || self.dialog_open() {
                return;
            }
            let editor = self.tab().editor_id();
            let focused = ctx.memory(|memory| memory.has_focus(editor));
            let text_pasted = ctx.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Paste(_)))
            });
            if focused && !text_pasted {
                self.paste_image(ctx, false);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = ctx;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bitmap of `width` × `height` with `bits` per pixel, rows in the
    /// given order, every pixel `bgra`.
    fn dib(width: i32, height: i32, bits: u16, bgra: [u8; 4]) -> Vec<u8> {
        let mut data = vec![0_u8; 40];
        data[0..4].copy_from_slice(&40_u32.to_le_bytes());
        data[4..8].copy_from_slice(&width.to_le_bytes());
        data[8..12].copy_from_slice(&height.to_le_bytes());
        data[12..14].copy_from_slice(&1_u16.to_le_bytes());
        data[14..16].copy_from_slice(&bits.to_le_bytes());
        let pixel_bytes = usize::from(bits / 8);
        let width = width.unsigned_abs() as usize;
        let stride = (width * pixel_bytes).div_ceil(4) * 4;
        for _ in 0..height.unsigned_abs() {
            let mut row = Vec::new();
            for _ in 0..width {
                row.extend_from_slice(&bgra[..pixel_bytes]);
            }
            row.resize(stride, 0);
            data.extend_from_slice(&row);
        }
        data
    }

    #[test]
    fn bitmaps_become_rgba() {
        // 24-bit, 3 wide: rows padded from 9 to 12 bytes.
        let (width, height, rgba) =
            dib_to_rgba(&dib(3, 2, 24, [10, 20, 30, 0])).unwrap_or_default();
        assert_eq!((width, height), (3, 2));
        assert_eq!(rgba.len(), 3 * 2 * 4);
        assert_eq!(&rgba[..4], &[30, 20, 10, 255]);
        // 32-bit with alpha 0 everywhere: opaque.
        let (_, _, rgba) = dib_to_rgba(&dib(2, -2, 32, [1, 2, 3, 0])).unwrap_or_default();
        assert_eq!(&rgba[..4], &[3, 2, 1, 255]);
        // 32-bit with alpha: kept.
        let (_, _, rgba) = dib_to_rgba(&dib(1, 1, 32, [1, 2, 3, 128])).unwrap_or_default();
        assert_eq!(rgba, [3, 2, 1, 128]);
        // Too short, or a palette format: not read.
        assert!(dib_to_rgba(&dib(4, 4, 24, [0; 4])[..60]).is_none());
        assert!(dib_to_rgba(&dib(4, 4, 8, [0; 4])).is_none());
    }

    #[test]
    fn rows_come_out_top_down() {
        // Bottom-up 1 × 2: the first stored row is the bottom one.
        let mut data = dib(1, 2, 32, [0; 4]);
        data[40..44].copy_from_slice(&[0, 0, 255, 255]); // bottom: red
        data[44..48].copy_from_slice(&[255, 0, 0, 255]); // top: blue
        let (_, _, rgba) = dib_to_rgba(&data).unwrap_or_default();
        assert_eq!(rgba, [0, 0, 255, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn png_bytes_and_links() {
        let png = encode_png(1, 1, &[255, 0, 0, 255]).unwrap_or_default();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        assert_eq!(
            image_link("notes-20261112-1530.png"),
            "![](notes-20261112-1530.png)"
        );
        assert_eq!(image_link("my notes.png"), "![](<my notes.png>)");
    }

    #[test]
    fn pasted_image_names_follow_the_document_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("mdnotepad-paste-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let doc = dir.join("notes.md");
        let name = |doc: Option<&Path>| suggested_name(Some(dir.as_path()), doc, "20261112-1530");
        assert_eq!(name(Some(doc.as_path())), "notes-20261112-1530.png");
        assert_eq!(name(None), "image-20261112-1530.png");
        let _ = std::fs::write(dir.join("notes-20261112-1530.png"), b"x");
        assert_eq!(name(Some(doc.as_path())), "notes-20261112-1530-2.png");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn links_are_relative_beside_the_document_and_urls_elsewhere() {
        let dir = std::env::temp_dir().join("docs");
        let doc_path = dir.join("notes.md");
        let doc = Some(doc_path.as_path());
        assert_eq!(image_reference(doc, &dir.join("shot.png")), "shot.png");
        assert_eq!(
            image_reference(doc, &dir.join("img").join("a b.png")),
            "img/a b.png"
        );
        let elsewhere = std::env::temp_dir().join("other").join("x.png");
        for reference in [
            image_reference(doc, &elsewhere),
            image_reference(None, &elsewhere),
        ] {
            assert!(reference.starts_with("file:///"), "{reference}");
            assert!(reference.ends_with("/other/x.png") && !reference.contains('\\'));
        }
        assert_eq!(image_link("img/a b.png"), "![](<img/a b.png>)");
    }
}
