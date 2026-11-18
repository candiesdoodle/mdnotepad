//! Inline text rendering — `SpanFormat`, styled text, layout jobs.

use super::layout::body_font_size;
use crate::md::parse::{SpanStyle, StyledText};
use crate::md::style::MarkdownStyle;

/// Inline code relative to the text around it (see
/// `ui_style::MONOSPACE_SCALE`).
const CODE_SPAN_SCALE: f32 = crate::ui_style::MONOSPACE_SCALE;

/// Snap `pos` to a valid UTF-8 char boundary in `text`, rounding down.
/// If `pos` is already on a boundary (or at 0 / `text.len()`), returns it unchanged.
#[inline]
const fn snap_to_char_boundary(text: &str, pos: usize) -> usize {
    if pos >= text.len() {
        return text.len();
    }
    text.floor_char_boundary(pos)
}

#[inline]
fn effective_is_ascii(st: &StyledText) -> bool {
    st.text.is_empty()
        || if st.char_count > 0 {
            st.is_ascii
        } else {
            st.text.is_ascii()
        }
}

#[inline]
fn span_byte_range(
    text: &str,
    span: &crate::md::parse::Span,
    is_ascii: bool,
) -> Option<std::ops::Range<usize>> {
    let len = text.len();
    let start = (span.start as usize).min(len);
    let end = (span.end as usize).min(len);
    let range = if is_ascii {
        start..end
    } else {
        snap_to_char_boundary(text, start)..snap_to_char_boundary(text, end)
    };
    (range.start < range.end).then_some(range)
}

/// Inline formatting properties resolved from a composite `SpanStyle`.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct SpanFormat {
    font_family: egui::FontFamily,
    color: egui::Color32,
    background: egui::Color32,
    underline: bool,
    strikethrough: bool,
    italics: bool,
    strong: bool,
}

impl SpanFormat {
    #[inline]
    fn resolve(
        ss: SpanStyle,
        md_style: &MarkdownStyle,
        base_color: egui::Color32,
        ui: &egui::Ui,
        bold: Option<&egui::FontFamily>,
    ) -> Self {
        Self {
            font_family: span_font_family(ss.code(), ss.strong(), bold),
            color: if ss.has_link() {
                md_style
                    .link_color
                    .unwrap_or_else(|| ui.visuals().hyperlink_color)
            } else {
                base_color
            },
            background: if ss.code() {
                md_style
                    .code_bg
                    .unwrap_or_else(|| ui.visuals().faint_bg_color)
            } else {
                egui::Color32::TRANSPARENT
            },
            underline: ss.has_link(),
            strikethrough: ss.strikethrough(),
            italics: ss.emphasis(),
            strong: ss.strong(),
        }
    }
}

/// Monospace for code, the bold family (when the fonts have it) for strong
/// text, proportional otherwise.
fn span_font_family(code: bool, strong: bool, bold: Option<&egui::FontFamily>) -> egui::FontFamily {
    if code {
        egui::FontFamily::Monospace
    } else if strong && let Some(bold) = bold {
        bold.clone()
    } else {
        egui::FontFamily::Proportional
    }
}

/// The bold family when the context's fonts have it.
fn bold_family(ui: &egui::Ui) -> Option<egui::FontFamily> {
    crate::ui_style::has_bold_family(ui).then(crate::ui_style::bold_family)
}

/// The headings' family: the theme's heading font when it has one (the
/// Paper theme's serif), otherwise the bold family.
fn heading_family(ui: &egui::Ui) -> Option<egui::FontFamily> {
    if crate::ui_style::has_heading_family(ui) {
        Some(crate::ui_style::heading_family())
    } else {
        bold_family(ui)
    }
}

/// Whether headings glow (the Neon theme).
fn headings_glow(ui: &egui::Ui) -> bool {
    ui.visuals().dark_mode && crate::ui_style::flavor() == crate::ui_style::Flavor::Neon
}

/// Show `galley` with a soft glow of `color` behind it: copies of the text
/// around it in two faint rings.
fn label_with_glow(ui: &mut egui::Ui, galley: &std::sync::Arc<egui::Galley>, color: egui::Color32) {
    let behind = ui.painter().add(egui::Shape::Noop);
    let response = ui.label(galley.clone());
    let pos = response.rect.min;
    let mut glow = Vec::with_capacity(16);
    for (radius, alpha) in [(1.5_f32, 0.15_f32), (3.0, 0.06)] {
        for step in 0..8_u8 {
            let offset = radius * egui::Vec2::angled(f32::from(step) * std::f32::consts::FRAC_PI_4);
            glow.push(egui::Shape::galley_with_override_text_color(
                pos + offset,
                galley.clone(),
                color.gamma_multiply(alpha),
            ));
        }
    }
    ui.painter().set(behind, egui::Shape::Vec(glow));
}

pub(super) fn render_styled_text(ui: &mut egui::Ui, st: &StyledText, style: &MarkdownStyle) {
    render_styled_text_ex(ui, st, style, None, None);
}

pub(super) fn render_styled_text_ex(
    ui: &mut egui::Ui,
    st: &StyledText,
    style: &MarkdownStyle,
    font_size: Option<f32>,
    color_override: Option<egui::Color32>,
) {
    render_text(ui, st, style, font_size, color_override, false);
}

/// A heading's text: `size`, `color`, and bold throughout (when the fonts
/// have a bold family).
pub(super) fn render_heading_text(
    ui: &mut egui::Ui,
    st: &StyledText,
    style: &MarkdownStyle,
    size: f32,
    color: egui::Color32,
) {
    render_text(ui, st, style, Some(size), Some(color), true);
}

fn render_text(
    ui: &mut egui::Ui,
    st: &StyledText,
    style: &MarkdownStyle,
    font_size: Option<f32>,
    color_override: Option<egui::Color32>,
    all_bold: bool,
) {
    if st.text.is_empty() {
        return;
    }

    let has_links = st.has_links;

    // If there are links, render in a horizontal wrap so we can make links clickable.
    if has_links {
        render_text_with_links(ui, st, style, font_size, color_override, all_bold);
        return;
    }

    let body_size = body_font_size(ui);
    let size = font_size.unwrap_or(body_size);
    let base_color = color_override
        .or(style.body_color)
        .unwrap_or_else(|| ui.visuals().text_color());
    let wrap_width = ui.available_width();
    let regular = if all_bold {
        heading_family(ui).unwrap_or(egui::FontFamily::Proportional)
    } else {
        egui::FontFamily::Proportional
    };

    let mut job = if st.spans.is_empty() {
        egui::text::LayoutJob::simple(
            st.text.clone(),
            egui::FontId::new(size, regular.clone()),
            base_color,
            wrap_width,
        )
    } else {
        build_layout_job(st, &st.spans, style, base_color, size, wrap_width, ui)
    };
    if all_bold {
        for section in &mut job.sections {
            if section.format.font_id.family == egui::FontFamily::Proportional {
                section.format.font_id.family = regular.clone();
            }
        }
    }

    let galley = ui.fonts_mut(|f| f.layout_job(job));
    if all_bold && headings_glow(ui) {
        label_with_glow(ui, &galley, base_color);
    } else {
        ui.label(galley);
    }
}

/// Render text that contains links: non-link spans as labels, link spans as hyperlinks.
pub(super) fn render_text_with_links(
    ui: &mut egui::Ui,
    st: &StyledText,
    style: &MarkdownStyle,
    font_size: Option<f32>,
    color_override: Option<egui::Color32>,
    all_bold: bool,
) {
    let body_size = body_font_size(ui);
    let size = font_size.unwrap_or(body_size);
    let base_color = color_override
        .or(style.body_color)
        .unwrap_or_else(|| ui.visuals().text_color());
    let is_ascii = effective_is_ascii(st);
    let bold = if all_bold {
        heading_family(ui)
    } else {
        bold_family(ui)
    };

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for span in &st.spans {
            let Some(range) = span_byte_range(&st.text, span, is_ascii) else {
                continue;
            };
            let text = &st.text[range];
            let is_code = span.style.code();
            let span_size = if is_code {
                size * CODE_SPAN_SCALE
            } else {
                size
            };
            let font_family =
                span_font_family(is_code, span.style.strong() || all_bold, bold.as_ref());
            let mut rt = egui::RichText::new(text).font(egui::FontId::new(span_size, font_family));

            if span.style.emphasis() {
                rt = rt.italics();
            }
            if span.style.strikethrough() {
                rt = rt.strikethrough();
            }
            if is_code {
                rt = rt
                    .background_color(style.code_bg.unwrap_or_else(|| ui.visuals().faint_bg_color));
            }

            if let Some(url) = st.link_url(span.style.link_idx) {
                if span.style.strong() {
                    rt = rt.strong();
                }
                ui.hyperlink_to(rt, url.as_ref());
            } else {
                rt = rt.color(if span.style.strong() {
                    strengthen_color(base_color)
                } else {
                    base_color
                });
                ui.label(rt);
            }
        }
    });
}

/// Build a `LayoutJob` for non-link text spans.
pub(super) fn build_layout_job(
    st: &StyledText,
    spans: &[crate::md::parse::Span],
    style: &MarkdownStyle,
    base_color: egui::Color32,
    size: f32,
    wrap_width: f32,
    ui: &egui::Ui,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob {
        text: st.text.clone(),
        sections: Vec::with_capacity(spans.len()),
        wrap: egui::text::TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        ..Default::default()
    };
    let is_ascii = effective_is_ascii(st);
    let bold = bold_family(ui);

    for span in spans {
        let Some(range) = span_byte_range(&st.text, span, is_ascii) else {
            continue;
        };
        let sf = SpanFormat::resolve(span.style, style, base_color, ui, bold.as_ref());
        let span_size = if span.style.code() {
            size * CODE_SPAN_SCALE
        } else {
            size
        };
        let color = if sf.strong {
            strengthen_color(sf.color)
        } else {
            sf.color
        };
        let stroke = |active: bool| {
            if active {
                egui::Stroke::new(1.0, color)
            } else {
                egui::Stroke::NONE
            }
        };
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: range,
            format: egui::TextFormat {
                font_id: egui::FontId::new(span_size, sf.font_family),
                color,
                background: sf.background,
                italics: sf.italics,
                underline: stroke(sf.underline),
                strikethrough: stroke(sf.strikethrough),
                ..Default::default()
            },
        });
    }
    job
}

/// Approximate "bold" by increasing contrast of a colour.
/// egui has no bold font weight, so we visually distinguish strong text.
/// On dark backgrounds (bright text) the colour is brightened;
/// on light backgrounds (dark text) it is darkened.
#[inline]
pub(crate) fn strengthen_color(color: egui::Color32) -> egui::Color32 {
    // Work in unmultiplied space so luma is correct for semi-transparent
    // colours and the output never violates the premultiplied invariant.
    let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
    // Perceptual luminance (ITU-R BT.601).
    let luma = 0.114f32.mul_add(
        f32::from(blue),
        0.299f32.mul_add(f32::from(red), 0.587 * f32::from(green)),
    );
    if luma > 127.0 {
        // Bright text (dark background) → brighten toward white.
        let boost = |val: u8| {
            let delta = (u16::from(255_u8.saturating_sub(val))) / 3;
            val.saturating_add(delta.min(255) as u8)
        };
        egui::Color32::from_rgba_unmultiplied(boost(red), boost(green), boost(blue), alpha)
    } else {
        // Dark text (light background) → darken toward black.
        let darken = |val: u8| {
            let delta = u16::from(val) / 3;
            val.saturating_sub(delta.min(255) as u8)
        };
        egui::Color32::from_rgba_unmultiplied(darken(red), darken(green), darken(blue), alpha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_to_char_boundary_ascii() {
        let text = "hello";
        for i in 0..=text.len() {
            assert_eq!(snap_to_char_boundary(text, i), i);
        }
    }

    #[test]
    fn snap_to_char_boundary_multibyte() {
        // 'é' = 2 bytes, '日' = 3 bytes, '🦀' = 4 bytes
        let text = "aé日🦀z";
        // Valid boundaries: 0, 1, 3, 6, 10, 11
        assert_eq!(snap_to_char_boundary(text, 0), 0); // 'a'
        assert_eq!(snap_to_char_boundary(text, 1), 1); // start of 'é'
        assert_eq!(snap_to_char_boundary(text, 2), 1); // mid 'é' → snaps to 1
        assert_eq!(snap_to_char_boundary(text, 3), 3); // start of '日'
        assert_eq!(snap_to_char_boundary(text, 4), 3); // mid '日' → snaps to 3
        assert_eq!(snap_to_char_boundary(text, 5), 3); // mid '日' → snaps to 3
        assert_eq!(snap_to_char_boundary(text, 6), 6); // start of '🦀'
        assert_eq!(snap_to_char_boundary(text, 7), 6); // mid '🦀' → snaps to 6
        assert_eq!(snap_to_char_boundary(text, 8), 6); // mid '🦀' → snaps to 6
        assert_eq!(snap_to_char_boundary(text, 9), 6); // mid '🦀' → snaps to 6
        assert_eq!(snap_to_char_boundary(text, 10), 10); // start of 'z'
        assert_eq!(snap_to_char_boundary(text, 11), 11); // end of string
    }

    #[test]
    fn snap_to_char_boundary_beyond_length() {
        let text = "abc";
        assert_eq!(snap_to_char_boundary(text, 100), 3);
        assert_eq!(snap_to_char_boundary(text, usize::MAX), 3);
    }

    #[test]
    fn snap_to_char_boundary_empty_string() {
        assert_eq!(snap_to_char_boundary("", 0), 0);
        assert_eq!(snap_to_char_boundary("", 5), 0);
    }

    #[test]
    fn snap_to_char_boundary_only_multibyte() {
        // String of only 4-byte chars
        let text = "🦀🦀🦀";
        for pos in 0..text.len() {
            let snapped = snap_to_char_boundary(text, pos);
            assert!(
                text.is_char_boundary(snapped),
                "pos {pos} → {snapped} is not a boundary"
            );
            assert!(snapped <= pos, "snapped {snapped} > original {pos}");
        }
    }

    #[test]
    fn effective_is_ascii_handles_manual_styled_text() {
        let ascii = StyledText {
            text: "plain".to_owned(),
            ..StyledText::default()
        };
        assert!(effective_is_ascii(&ascii));

        let unicode = StyledText {
            text: "éx".to_owned(),
            ..StyledText::default()
        };
        assert!(!effective_is_ascii(&unicode));
    }

    #[test]
    fn span_byte_range_respects_ascii_fast_path_and_unicode_fallback() {
        let ascii = StyledText {
            text: "hello".to_owned(),
            ..StyledText::default()
        };
        let span = crate::md::parse::Span {
            start: 1,
            end: 4,
            style: SpanStyle::default(),
        };
        assert_eq!(
            span_byte_range(&ascii.text, &span, effective_is_ascii(&ascii)),
            Some(1..4)
        );

        let unicode = StyledText {
            text: "éx".to_owned(),
            ..StyledText::default()
        };
        let span = crate::md::parse::Span {
            start: 1,
            end: 3,
            style: SpanStyle::default(),
        };
        assert_eq!(
            span_byte_range(&unicode.text, &span, effective_is_ascii(&unicode)),
            Some(0..3)
        );
    }
}
