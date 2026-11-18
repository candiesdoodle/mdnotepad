use std::sync::Arc;

use eframe::egui;

use crate::{
    document::{EditorGalleyCache, EditorView, formatted_analysis},
    live_format, ui_style,
};

/// What the editor's galley depends on besides the text.
#[derive(Clone, Copy, Debug)]
pub struct LayoutParams {
    /// Edit sequence number of the text.
    pub seq: u64,
    pub view: EditorView,
    pub heading_color_mode: bool,
    /// Build the row ↔ byte table (outline panel).
    pub nav_visible: bool,
    pub word_wrap: bool,
}

/// The editor's galley for `text`, from `cache` when nothing it depends on
/// changed.  A wrap-width or display-scale change reuses the cached
/// sections; a new text, view or text size (zoom)
/// builds them again (the formatted view from the analysis in `analysis`,
/// which survives caret moves).
pub fn editor_galley(
    ui: &egui::Ui,
    text: &str,
    wrap_width: f32,
    params: &LayoutParams,
    cache: &mut Option<EditorGalleyCache>,
    analysis: &mut Option<(u64, live_format::Analysis)>,
) -> Arc<egui::Galley> {
    // Word wrap off: one row per line; the editor grows sideways and the
    // scroll area scrolls horizontally.
    let wrap_width = if params.word_wrap {
        wrap_width
    } else {
        f32::INFINITY
    };
    let wrap_width_bits = wrap_width.to_bits();
    let text_sizes_bits = text_sizes_key(ui.style());
    let pixels_per_point_bits = ui.ctx().pixels_per_point().to_bits();
    let diagrams_version = crate::mermaid::version();
    let same_content = |cache: &EditorGalleyCache| {
        cache.content_seq == params.seq
            && cache.content_color_mode == params.heading_color_mode
            && cache.view == params.view
            && cache.text_sizes_bits == text_sizes_bits
            && cache.diagrams_version == diagrams_version
    };

    if let Some(cache) = cache.as_ref()
        && same_content(cache)
        && cache.wrap_width_bits == wrap_width_bits
        && cache.pixels_per_point_bits == pixels_per_point_bits
    {
        return cache.galley.clone();
    }

    // Diagram pictures shown in the formatted view, sized to the width.
    let pictures = match params.view {
        EditorView::Formatted(reveal) => {
            let analysis = formatted_analysis(analysis, params.seq, text);
            diagram_pictures(ui, text, analysis, reveal, wrap_width)
        }
        EditorView::Raw => Vec::new(),
    };

    // Partial hit (wrap / display scale changed): reuse the cached sections
    // and rebuild the job from the text, instead of cloning a full job
    // (not with pictures, whose rows follow the width).
    let reused = cache
        .as_ref()
        .filter(|cache| same_content(cache) && pictures.is_empty() && cache.pictures.is_empty())
        .map(|cache| (cache.layout_sections.clone(), cache.format_style.clone()));
    let fresh = reused.is_none();
    let (mut sections, format_style) = reused.unwrap_or_else(|| match params.view {
        EditorView::Raw => (raw_sections(ui.style(), text), None),
        EditorView::Formatted(reveal) => {
            let style = format_style(ui, params.heading_color_mode);
            let analysis = formatted_analysis(analysis, params.seq, text);
            let job = live_format::layout_job(text, analysis, reveal, &style, |piece, font| {
                ui.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(piece.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
                        .size()
                        .x
                })
            });
            (job.sections, Some(style))
        }
    });
    if fresh && !pictures.is_empty() {
        let heights: Vec<(usize, f32)> = pictures
            .iter()
            .map(|picture| (picture.index, picture.size.y))
            .collect();
        let analysis = formatted_analysis(analysis, params.seq, text);
        live_format::hide_diagrams(&mut sections, analysis, &heights);
    }
    let job = egui::text::LayoutJob {
        text: text.to_owned(),
        sections: sections.clone(),
        wrap: egui::text::TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        ..Default::default()
    };
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let row_byte_offsets = if params.nav_visible {
        build_row_byte_offsets(&galley, text)
    } else {
        Vec::new()
    };
    let placed = if pictures.is_empty() {
        Vec::new()
    } else {
        let analysis = formatted_analysis(analysis, params.seq, text);
        pictures
            .into_iter()
            .filter_map(|picture| {
                let top = live_format::diagram_top(&galley, analysis, picture.index)?;
                let rect =
                    egui::Rect::from_min_size(egui::pos2(live_format::CODE_PAD, top), picture.size);
                Some((rect, picture.uri))
            })
            .collect()
    };
    let overlays = match (params.view, &format_style) {
        (EditorView::Formatted(reveal), Some(style)) => {
            let analysis = formatted_analysis(analysis, params.seq, text);
            live_format::place_overlays(&galley, analysis, reveal, style)
        }
        _ => Vec::new(),
    };
    *cache = Some(EditorGalleyCache {
        content_seq: params.seq,
        content_color_mode: params.heading_color_mode,
        view: params.view,
        wrap_width_bits,
        text_sizes_bits,
        pixels_per_point_bits,
        layout_sections: sections,
        galley: galley.clone(),
        row_byte_offsets,
        format_style,
        overlays,
        diagrams_version,
        pictures: placed,
    });
    galley
}

/// A diagram shown as its picture in the formatted view.
struct Picture {
    /// Index into the analysis' diagrams.
    index: usize,
    uri: String,
    /// Shown size, in points (at most the editor's width).
    size: egui::Vec2,
}

/// The pictures of the diagrams the caret is not in that are drawn already
/// (the others show their code), sized to fit `wrap_width`.
fn diagram_pictures(
    ui: &egui::Ui,
    text: &str,
    analysis: &live_format::Analysis,
    reveal: live_format::Reveal,
    wrap_width: f32,
) -> Vec<Picture> {
    let dark = ui.visuals().dark_mode;
    let room = 2.0f32.mul_add(-live_format::CODE_PAD, wrap_width).max(40.0);
    analysis
        .diagrams()
        .iter()
        .enumerate()
        .filter(|(_, diagram)| !diagram.revealed(reveal))
        .filter_map(|(index, diagram)| {
            let code = text.get(diagram.code.clone())?;
            let crate::mermaid::Diagram::Ready { uri, size } =
                crate::mermaid::diagram(ui.ctx(), code, dark)
            else {
                return None;
            };
            let scale = (room / size.x).min(1.0);
            Some(Picture {
                index,
                uri,
                size: (size * scale).round(),
            })
        })
        .collect()
}

/// The raw view's sections: the whole text in one font (monospace at the
/// Monospace text style's size: the document's code size, zoomed) and one
/// colour, as a plain-text editor shows a Markdown file.
#[must_use]
pub fn raw_sections(style: &egui::Style, text: &str) -> Vec<egui::text::LayoutSection> {
    vec![egui::text::LayoutSection {
        leading_space: 0.0,
        byte_range: 0..text.len(),
        format: egui::TextFormat::simple(
            egui::TextStyle::Monospace.resolve(style),
            style.visuals.text_color(),
        ),
    }]
}

/// The text sizes the editor's sections are built for (Body, Monospace,
/// Small: the document's, zoomed).
fn text_sizes_key(style: &egui::Style) -> [u32; 3] {
    [
        egui::TextStyle::Body,
        egui::TextStyle::Monospace,
        egui::TextStyle::Small,
    ]
    .map(|text_style| text_style.resolve(style).size.to_bits())
}

/// The formatted view's style for the current fonts and theme.
fn format_style(ui: &egui::Ui, heading_color_mode: bool) -> live_format::FormatStyle {
    live_format::FormatStyle::new(
        ui.style(),
        ui.visuals(),
        heading_color_mode,
        ui_style::has_bold_family(ui).then(ui_style::bold_family),
        ui_style::has_heading_family(ui).then(ui_style::heading_family),
        |font| ui.fonts_mut(|fonts| fonts.row_height(font)),
    )
}

/// Build a `(row_y, row_start_byte)` table from galley rows.
/// Computed once per galley rebuild; enables O(log n) scroll ↔ byte lookups.
/// Uses a single-pass O(n) char scan instead of O(n²) repeated `.nth()`.
#[allow(clippy::cast_possible_truncation)] // byte offsets are < 4GB for any realistic markdown file
pub fn build_row_byte_offsets(galley: &egui::Galley, text: &str) -> Vec<(f32, u32)> {
    let mut result = Vec::with_capacity(galley.rows.len());
    let mut char_iter = text.char_indices().peekable();
    let mut chars_consumed = 0usize;
    let mut target_char = 0usize;

    for row in &galley.rows {
        while chars_consumed < target_char {
            if char_iter.next().is_none() {
                break;
            }
            chars_consumed += 1;
        }
        let byte_offset = char_iter.peek().map_or(text.len(), |&(i, _)| i);
        result.push((row.rect().min.y, byte_offset as u32));

        target_char += row.glyphs.len();
        if row.ends_with_newline {
            target_char += 1;
        }
    }
    result
}

/// Map a byte offset to the Y coordinate of the row containing it.
/// O(log n) binary search.
#[inline]
#[allow(clippy::cast_possible_truncation)] // byte offsets are < 4GB
pub fn row_byte_offset_to_y(rows: &[(f32, u32)], byte_offset: usize) -> f32 {
    if rows.is_empty() {
        return 0.0;
    }
    let offset = byte_offset as u32;
    let idx = rows
        .partition_point(|(_, b)| *b <= offset)
        .saturating_sub(1);
    rows[idx].0
}

/// Map a Y coordinate to the byte offset at the start of the row at that
/// position.  O(log n) binary search.
#[inline]
pub fn row_y_to_byte_offset(rows: &[(f32, u32)], y: f32) -> usize {
    if rows.is_empty() || !y.is_finite() {
        return 0;
    }
    let idx = rows
        .partition_point(|(row_y, _)| *row_y <= y)
        .saturating_sub(1);
    rows[idx].1 as usize
}

/// Convert a character index to a byte offset in `text`.
#[cfg(test)]
pub fn char_index_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(i, _)| i)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn raw_sections_are_one_font_one_size_one_colour() {
        let ctx = egui::Context::default();
        crate::ui_style::configure_style(&ctx);
        let style = ctx.style();
        let text = "# Title\n\nSome **bold** and `code`.\n\n```\nfenced\n```\n";
        let sections = raw_sections(&style, text);
        assert_eq!(sections.len(), 1);
        let section = &sections[0];
        assert_eq!(section.byte_range, 0..text.len());
        assert_eq!(
            section.format.font_id,
            egui::TextStyle::Monospace.resolve(&style)
        );
        assert_eq!(section.format.font_id.family, egui::FontFamily::Monospace);
        assert_eq!(section.format.color, style.visuals.text_color());
        assert_eq!(section.format.background, egui::Color32::TRANSPARENT);
        assert!(
            raw_sections(&style, "")
                .iter()
                .all(|s| s.byte_range == (0..0))
        );
    }

    #[test]
    fn char_index_to_byte_handles_ascii_and_multibyte() {
        assert_eq!(char_index_to_byte("hello", 0), 0);
        assert_eq!(char_index_to_byte("hello", 3), 3);
        assert_eq!(char_index_to_byte("hello", 5), 5);
        // Beyond end clamps to text.len()
        assert_eq!(char_index_to_byte("hello", 100), 5);
        // Multi-byte: 'é' is 2 bytes in UTF-8
        assert_eq!(char_index_to_byte("h\u{00e9}llo", 0), 0);
        assert_eq!(char_index_to_byte("h\u{00e9}llo", 1), 1);
        assert_eq!(char_index_to_byte("h\u{00e9}llo", 2), 3);
    }

    #[test]
    fn row_byte_offset_to_y_binary_search() {
        let rows = vec![(0.0, 0u32), (20.0, 50), (40.0, 100), (60.0, 150)];
        assert_eq!(row_byte_offset_to_y(&rows, 0), 0.0);
        assert_eq!(row_byte_offset_to_y(&rows, 75), 20.0);
        assert_eq!(row_byte_offset_to_y(&rows, 100), 40.0);
        assert_eq!(row_byte_offset_to_y(&rows, 200), 60.0);
        assert_eq!(row_byte_offset_to_y(&[], 0), 0.0);
        assert_eq!(row_byte_offset_to_y(&[], 42), 0.0);
    }

    #[test]
    fn row_y_to_byte_offset_binary_search() {
        let rows = vec![(0.0, 0u32), (20.0, 50), (40.0, 100), (60.0, 150)];
        assert_eq!(row_y_to_byte_offset(&rows, 0.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 10.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 25.0), 50);
        assert_eq!(row_y_to_byte_offset(&rows, 40.0), 100);
        assert_eq!(row_y_to_byte_offset(&rows, 99.0), 150);
        assert_eq!(row_y_to_byte_offset(&[], 0.0), 0);
        assert_eq!(row_y_to_byte_offset(&[], 42.0), 0);
    }

    #[test]
    fn row_byte_offset_to_y_boundaries() {
        let rows = vec![(0.0, 0u32), (15.0, 30), (30.0, 60)];
        // Exact match on first row.
        assert_eq!(row_byte_offset_to_y(&rows, 0), 0.0);
        // Exact match on last row.
        assert_eq!(row_byte_offset_to_y(&rows, 60), 30.0);
        // Beyond last row byte offset clamps to last row y.
        assert_eq!(row_byte_offset_to_y(&rows, 999), 30.0);
        // Between first and second row maps to first row.
        assert_eq!(row_byte_offset_to_y(&rows, 15), 0.0);
    }

    // ── Single-row (long line / no newlines) ────────────────────────

    #[test]
    fn row_byte_offset_to_y_single_row() {
        let rows = vec![(5.0, 0u32)];
        assert_eq!(row_byte_offset_to_y(&rows, 0), 5.0);
        assert_eq!(row_byte_offset_to_y(&rows, 500), 5.0);
        assert_eq!(row_byte_offset_to_y(&rows, usize::MAX), 5.0);
    }

    #[test]
    fn row_y_to_byte_offset_single_row() {
        let rows = vec![(5.0, 0u32)];
        assert_eq!(row_y_to_byte_offset(&rows, 0.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 5.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 999.0), 0);
    }

    // ── Multi-byte UTF-8 aligned offsets ────────────────────────────

    #[test]
    fn row_byte_offset_to_y_multibyte_offsets() {
        // Simulates rows where boundaries fall on multi-byte char edges.
        // "café\n" is 6 bytes: c(1) a(1) f(1) é(2) \n(1)
        let rows = vec![(0.0, 0u32), (20.0, 6), (40.0, 12)];
        assert_eq!(row_byte_offset_to_y(&rows, 0), 0.0);
        assert_eq!(row_byte_offset_to_y(&rows, 3), 0.0); // inside first row
        assert_eq!(row_byte_offset_to_y(&rows, 6), 20.0); // exact second row start
        assert_eq!(row_byte_offset_to_y(&rows, 9), 20.0); // inside second row
        assert_eq!(row_byte_offset_to_y(&rows, 12), 40.0);
    }

    #[test]
    fn row_y_to_byte_offset_multibyte_offsets() {
        let rows = vec![(0.0, 0u32), (20.0, 6), (40.0, 12)];
        assert_eq!(row_y_to_byte_offset(&rows, 0.0), 0);
        assert_eq!(row_y_to_byte_offset(&rows, 20.0), 6);
        assert_eq!(row_y_to_byte_offset(&rows, 40.0), 12);
        assert_eq!(row_y_to_byte_offset(&rows, 30.0), 6); // between rows 2 and 3
    }

    // ── char_index_to_byte extended ─────────────────────────────────

    #[test]
    fn char_index_to_byte_edge_cases() {
        // Empty string
        assert_eq!(char_index_to_byte("", 0), 0);
        assert_eq!(char_index_to_byte("", 5), 0);
        // CJK + emoji: '日' is 3 bytes, '🦀' is 4 bytes
        let text = "日🦀x";
        assert_eq!(char_index_to_byte(text, 0), 0);
        assert_eq!(char_index_to_byte(text, 1), 3);
        assert_eq!(char_index_to_byte(text, 2), 7);
        assert_eq!(char_index_to_byte(text, 3), 8);
    }

    // ── Chaos tests ──────────────────────────────────────────────────

    #[test]
    fn row_y_to_byte_offset_nan_inf() {
        let rows = vec![(0.0, 0u32), (20.0, 50), (40.0, 100)];
        // NaN and Inf should not panic — return 0
        assert_eq!(row_y_to_byte_offset(&rows, f32::NAN), 0);
        assert_eq!(row_y_to_byte_offset(&rows, f32::INFINITY), 0);
        assert_eq!(row_y_to_byte_offset(&rows, f32::NEG_INFINITY), 0);
    }

    #[test]
    fn row_byte_offset_to_y_extreme_values() {
        let rows = vec![(0.0, 0u32), (20.0, 50)];
        // usize::MAX should not panic — clamps to last row
        assert_eq!(row_byte_offset_to_y(&rows, usize::MAX), 20.0);
    }

    #[test]
    fn row_y_to_byte_offset_negative() {
        let rows = vec![(0.0, 0u32), (20.0, 50)];
        // Negative y should return first row's byte offset
        assert_eq!(row_y_to_byte_offset(&rows, -100.0), 0);
    }
}
