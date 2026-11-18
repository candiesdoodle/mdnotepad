#![forbid(unsafe_code)]
//! Configurable styles for Markdown preview rendering.

/// Heading font sizes (H1-H6) relative to the body text: linear from 1.5
/// down to the body size, so every level is a step apart.  Headings are
/// also bold (preview and formatted editor), so an H6 still stands out.
pub const HEADING_FONT_SCALES: [f32; 6] = [1.5, 1.4, 1.3, 1.2, 1.1, 1.0];

/// Dracula heading palette for dark themes, ordered from the primary
/// accent (H1) down to warmer secondary accents (H6).
pub const DARK_HEADING_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(0xBD, 0x93, 0xF9), // purple
    egui::Color32::from_rgb(0xFF, 0x79, 0xC6), // pink
    egui::Color32::from_rgb(0x8B, 0xE9, 0xFD), // cyan
    egui::Color32::from_rgb(0x50, 0xFA, 0x7B), // green
    egui::Color32::from_rgb(0xF1, 0xFA, 0x8C), // yellow
    egui::Color32::from_rgb(0xFF, 0xB8, 0x6C), // orange
];

/// Light-theme companions to the Dracula heading palette.
pub const LIGHT_HEADING_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(0x6A, 0x1B, 0x9A),
    egui::Color32::from_rgb(0xAD, 0x14, 0x57),
    egui::Color32::from_rgb(0x00, 0x5F, 0x9A),
    egui::Color32::from_rgb(0x2E, 0x7D, 0x32),
    egui::Color32::from_rgb(0x8C, 0x6D, 0x00),
    egui::Color32::from_rgb(0x9C, 0x3D, 0x00),
];

/// Paper theme: ink headings, the top two in a warm sienna.
pub const PAPER_HEADING_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(0x8A, 0x3B, 0x12),
    egui::Color32::from_rgb(0x7A, 0x45, 0x22),
    egui::Color32::from_rgb(0x3B, 0x33, 0x2B),
    egui::Color32::from_rgb(0x45, 0x3C, 0x33),
    egui::Color32::from_rgb(0x4F, 0x46, 0x3C),
    egui::Color32::from_rgb(0x59, 0x50, 0x46),
];

/// Neon theme: bright neon heading colours (they glow in the preview).
pub const NEON_HEADING_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(0xFF, 0x4F, 0xD8), // pink
    egui::Color32::from_rgb(0x4C, 0xE0, 0xFF), // cyan
    egui::Color32::from_rgb(0x9D, 0xFF, 0x6A), // lime
    egui::Color32::from_rgb(0xFF, 0xE6, 0x6B), // yellow
    egui::Color32::from_rgb(0xFF, 0x9F, 0x5A), // orange
    egui::Color32::from_rgb(0xB9, 0x8C, 0xFF), // violet
];

/// The heading palette of the current theme (its flavour, then light or
/// dark).
fn heading_palette(visuals: &egui::Visuals) -> &'static [egui::Color32; 6] {
    match crate::ui_style::flavor() {
        crate::ui_style::Flavor::Paper if !visuals.dark_mode => &PAPER_HEADING_COLORS,
        crate::ui_style::Flavor::Neon if visuals.dark_mode => &NEON_HEADING_COLORS,
        _ if visuals.dark_mode => &DARK_HEADING_COLORS,
        _ => &LIGHT_HEADING_COLORS,
    }
}

/// Colour of a level-`level` heading (1-6, clamped) in the outline and the
/// formatted editor: the theme's palette colour with heading colours on,
/// the hyperlink colour without.
pub fn heading_color(visuals: &egui::Visuals, level: usize, color_mode: bool) -> egui::Color32 {
    if !color_mode {
        return visuals.hyperlink_color;
    }
    let palette = heading_palette(visuals);
    palette[level.saturating_sub(1).min(palette.len() - 1)]
}

/// Per-heading-level style: font scale relative to body and colour.
#[derive(Clone, Copy, Debug)]
pub struct HeadingStyle {
    /// Multiplier applied to body font size.
    pub font_scale: f32,
    /// Text colour for this heading level.
    pub color: egui::Color32,
}

/// Full style configuration for the Markdown renderer.
#[derive(Clone, Debug)]
pub struct MarkdownStyle {
    /// Heading styles for levels H1-H6 (index 0 = H1).
    pub headings: [HeadingStyle; 6],
    /// Body text colour (falls back to `visuals.text_color()` if `None`).
    pub body_color: Option<egui::Color32>,
    /// Code background tint.
    pub code_bg: Option<egui::Color32>,
    /// Blockquote left-border colour.
    pub blockquote_bar: Option<egui::Color32>,
    /// Link colour.
    pub link_color: Option<egui::Color32>,
    /// Horizontal rule colour.
    pub hr_color: Option<egui::Color32>,
    /// Base URI for resolving relative image paths (e.g. `"file:///path/to/dir/"`).
    pub image_base_uri: String,
    /// Lay the document out in a centred column of a comfortable reading
    /// width (see `RenderMetrics::reading_width`) when the view is wider.
    pub centered_column: bool,
}

impl MarkdownStyle {
    /// Create a default style derived from egui visuals (no heading colours).
    #[must_use]
    pub fn from_visuals(visuals: &egui::Visuals) -> Self {
        let link = visuals.hyperlink_color;
        let headings = std::array::from_fn(|i| HeadingStyle {
            font_scale: HEADING_FONT_SCALES[i],
            color: link,
        });
        Self {
            headings,
            body_color: None,
            code_bg: Some(visuals.faint_bg_color),
            // The accent colour, a little muted.
            blockquote_bar: Some(link.gamma_multiply(0.7)),
            link_color: Some(link),
            hr_color: Some(visuals.widgets.noninteractive.bg_stroke.color),
            image_base_uri: String::new(),
            centered_column: false,
        }
    }

    /// Create a style with coloured headings, auto-selecting palette by theme.
    #[must_use]
    pub fn colored(visuals: &egui::Visuals) -> Self {
        let mut s = Self::from_visuals(visuals);
        s.set_heading_colors(*heading_palette(visuals));
        s
    }

    /// Set heading colours from an external palette.
    pub fn set_heading_colors(&mut self, colors: [egui::Color32; 6]) {
        for (h, c) in self.headings.iter_mut().zip(colors) {
            h.color = c;
        }
    }

    /// Set heading font scales.
    #[allow(dead_code)]
    pub fn set_heading_scales(&mut self, scales: [f32; 6]) {
        for (h, s) in self.headings.iter_mut().zip(scales) {
            h.font_scale = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_color_levels_themes_and_clamping() {
        let dark = egui::Visuals::dark();
        let light = egui::Visuals::light();
        assert_ne!(
            heading_color(&dark, 1, true),
            heading_color(&light, 1, true)
        );
        // Clamping: level 0 → level 1, level 7 → level 6.
        assert_eq!(heading_color(&dark, 0, true), heading_color(&dark, 1, true));
        assert_eq!(heading_color(&dark, 7, true), heading_color(&dark, 6, true));
        // Without heading colours: the hyperlink colour.
        assert_eq!(heading_color(&dark, 2, false), dark.hyperlink_color);
        // All six levels unique.
        let colors: Vec<_> = (1..=6).map(|l| heading_color(&dark, l, true)).collect();
        for i in 0..colors.len() {
            for j in (i + 1)..colors.len() {
                assert_ne!(colors[i], colors[j], "levels {} and {}", i + 1, j + 1);
            }
        }
    }

    #[test]
    fn colored_dark_uses_dark_palette() {
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        assert_eq!(style.headings[0].color, DARK_HEADING_COLORS[0]);
    }

    #[test]
    fn colored_light_uses_light_palette() {
        let style = MarkdownStyle::colored(&egui::Visuals::light());
        assert_eq!(style.headings[0].color, LIGHT_HEADING_COLORS[0]);
    }

    #[test]
    fn default_scales_match_constant() {
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        for (i, h) in style.headings.iter().enumerate() {
            assert!(
                (h.font_scale - HEADING_FONT_SCALES[i]).abs() < f32::EPSILON,
                "heading {i} scale mismatch"
            );
        }
    }

    /// Two colours are "visually distinct" when at least one RGB channel
    /// differs by ≥ 10 units.
    fn colors_distinct(a: egui::Color32, b: egui::Color32) -> bool {
        let dr = (i16::from(a.r()) - i16::from(b.r())).unsigned_abs();
        let dg = (i16::from(a.g()) - i16::from(b.g())).unsigned_abs();
        let db = (i16::from(a.b()) - i16::from(b.b())).unsigned_abs();
        dr >= 10 || dg >= 10 || db >= 10
    }

    fn assert_palette_pairwise_distinct(palette: &[egui::Color32; 6], label: &str) {
        for (i, a) in palette.iter().enumerate() {
            for (j, b) in palette.iter().enumerate().skip(i + 1) {
                assert!(
                    colors_distinct(*a, *b),
                    "{label} headings H{} and H{} are too similar",
                    i + 1,
                    j + 1,
                );
            }
        }
    }

    #[test]
    fn dark_heading_colors_are_pairwise_distinct() {
        assert_palette_pairwise_distinct(&DARK_HEADING_COLORS, "dark");
    }

    #[test]
    fn light_heading_colors_are_pairwise_distinct() {
        assert_palette_pairwise_distinct(&LIGHT_HEADING_COLORS, "light");
    }

    #[test]
    fn font_scales_step_down_linearly_from_h1_to_body_size() {
        assert!((HEADING_FONT_SCALES[0] - 1.5).abs() < f32::EPSILON, "H1");
        assert!((HEADING_FONT_SCALES[5] - 1.0).abs() < f32::EPSILON, "H6");
        for i in 1..HEADING_FONT_SCALES.len() {
            let step = HEADING_FONT_SCALES[i - 1] - HEADING_FONT_SCALES[i];
            assert!(
                (step - 0.1).abs() < 1e-6,
                "H{} ({}) should be 0.1 below H{} ({})",
                i + 1,
                HEADING_FONT_SCALES[i],
                i,
                HEADING_FONT_SCALES[i - 1],
            );
        }
    }

    #[test]
    fn body_color_differs_from_heading_colors_dark() {
        let style = MarkdownStyle::colored(&egui::Visuals::dark());
        let body = style
            .body_color
            .unwrap_or_else(|| egui::Visuals::dark().text_color());
        for (i, h) in style.headings.iter().enumerate() {
            assert!(
                colors_distinct(body, h.color),
                "dark body colour matches heading H{}",
                i + 1,
            );
        }
    }

    #[test]
    fn body_color_differs_from_heading_colors_light() {
        let style = MarkdownStyle::colored(&egui::Visuals::light());
        let body = style
            .body_color
            .unwrap_or_else(|| egui::Visuals::light().text_color());
        for (i, h) in style.headings.iter().enumerate() {
            assert!(
                colors_distinct(body, h.color),
                "light body colour matches heading H{}",
                i + 1,
            );
        }
    }

    #[test]
    fn hr_link_code_bg_are_set() {
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let style = MarkdownStyle::colored(&visuals);
            assert!(style.hr_color.is_some(), "hr_color should be set");
            assert!(style.link_color.is_some(), "link_color should be set");
            assert!(style.code_bg.is_some(), "code_bg should be set");

            let (hr, link, code_bg) = (
                style.hr_color.unwrap_or_default(),
                style.link_color.unwrap_or_default(),
                style.code_bg.unwrap_or_default(),
            );
            assert_ne!(hr.a(), 0, "hr_color should not be transparent");
            assert_ne!(link.a(), 0, "link_color should not be transparent");
            // code_bg may use additive blending (alpha=0 with non-zero RGB),
            // so just verify it is not fully black-transparent.
            assert!(
                code_bg.r() > 0 || code_bg.g() > 0 || code_bg.b() > 0 || code_bg.a() > 0,
                "code_bg should not be fully invisible"
            );

            let body = visuals.text_color();
            assert!(
                colors_distinct(link, body),
                "link colour should be visually distinct from body text"
            );
        }
    }

    #[test]
    fn from_visuals_works_for_both_modes() {
        let dark = MarkdownStyle::from_visuals(&egui::Visuals::dark());
        let light = MarkdownStyle::from_visuals(&egui::Visuals::light());
        assert_eq!(dark.headings.len(), 6);
        assert_eq!(light.headings.len(), 6);
        assert!(dark.code_bg.is_some());
        assert!(light.code_bg.is_some());
    }

    #[test]
    fn all_heading_scales_at_least_body_size() {
        for (i, &scale) in HEADING_FONT_SCALES.iter().enumerate() {
            assert!(
                scale >= 1.0 - f32::EPSILON,
                "H{} scale {} is smaller than body text",
                i + 1,
                scale
            );
        }
    }
}
