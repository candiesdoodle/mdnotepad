//! Chrome icons from the Windows icon font, as Notepad draws them: Segoe
//! Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10 (loaded into
//! their own font family by `ui_style.rs`).  Without either font, as on the
//! Linux dev host, [`paint`] returns `false` and the caller paints its
//! line-art fallback.

use eframe::egui::{self, Color32, FontId, Painter, Pos2, emath::GuiRounding as _};

use crate::ui_style;

/// Glyph size of the caption buttons (Windows 11 title bars).
pub(crate) const CAPTION_SIZE: f32 = 10.0;
/// Glyph size of the tab close and new-tab buttons (`WinUI` tab view).
pub(crate) const TAB_BUTTON_SIZE: f32 = 12.0;
/// Glyph size of dropdown chevrons and submenu arrows.
pub(crate) const CHEVRON_SIZE: f32 = 10.0;
/// Glyph size of the menu-row buttons: Notepad's toolbar glyphs measure
/// 14 (its bold glyph is 12 tall, 0.87 em).
pub(crate) const TOOLBAR_SIZE: f32 = 14.0;

/// An icon of the window chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    ChromeMinimize,
    ChromeMaximize,
    ChromeRestore,
    ChromeClose,
    /// Tab close button.
    Cancel,
    /// New tab; the Insert dropdown.
    Add,
    ChevronDown,
    /// Submenu arrow.
    ChevronRight,
    Bold,
    Italic,
    Strikethrough,
    /// Inline code.
    Code,
    Link,
    BulletedList,
    Settings,
}

impl Icon {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 15] = [
        Self::ChromeMinimize,
        Self::ChromeMaximize,
        Self::ChromeRestore,
        Self::ChromeClose,
        Self::Cancel,
        Self::Add,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::Bold,
        Self::Italic,
        Self::Strikethrough,
        Self::Code,
        Self::Link,
        Self::BulletedList,
        Self::Settings,
    ];

    /// Code point in both icon fonts, from Microsoft's tables ("Segoe
    /// Fluent Icons font" and "Segoe MDL2 Assets icons" on Microsoft
    /// Learn), where each has the same name as the variant (Settings is
    /// "Setting" in the MDL2 table).
    pub(crate) const fn codepoint(self) -> char {
        match self {
            Self::ChromeMinimize => '\u{E921}',
            Self::ChromeMaximize => '\u{E922}',
            Self::ChromeRestore => '\u{E923}',
            Self::ChromeClose => '\u{E8BB}',
            Self::Cancel => '\u{E711}',
            Self::Add => '\u{E710}',
            Self::ChevronDown => '\u{E70D}',
            Self::ChevronRight => '\u{E76C}',
            Self::Bold => '\u{E8DD}',
            Self::Italic => '\u{E8DB}',
            Self::Strikethrough => '\u{EDE0}',
            Self::Code => '\u{E943}',
            Self::Link => '\u{E71B}',
            Self::BulletedList => '\u{E8FD}',
            Self::Settings => '\u{E713}',
        }
    }
}

/// Paint `icon` from the icon font at `size`, its ink centred on `center`.
/// `false` (nothing painted) when the icon font or the glyph is missing.
pub(crate) fn paint(
    painter: &Painter,
    icon: Icon,
    center: Pos2,
    size: f32,
    color: Color32,
) -> bool {
    paint_char(painter, icon.codepoint(), center, size, color)
}

/// [`paint`] for any character of the icon family.
fn paint_char(painter: &Painter, chr: char, center: Pos2, size: f32, color: Color32) -> bool {
    let font_id = FontId::new(size, ui_style::icon_family());
    let available = painter.ctx().fonts_mut(|fonts| {
        fonts.definitions().families.contains_key(&font_id.family) && fonts.has_glyph(&font_id, chr)
    });
    if !available {
        return false;
    }
    let galley = painter.layout_no_wrap(chr.to_string(), font_id, color);
    // Centre the glyph's ink, not its line box: the icon fonts' vertical
    // metrics differ, and the glyphs do not all fill their em square.
    let ink = galley.rows.first().and_then(|row| {
        row.glyphs.first().map(|glyph| {
            egui::Rect::from_min_size(
                row.pos + glyph.pos.to_vec2() + glyph.uv_rect.offset,
                glyph.uv_rect.size,
            )
        })
    });
    let ink_center = ink.map_or_else(|| galley.rect.center(), |ink| ink.center());
    let pos = (center - ink_center.to_vec2()).round_to_pixels(painter.pixels_per_point());
    painter.galley(pos, galley, color);
    true
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn codepoints_are_distinct_private_use_characters() {
        for (index, icon) in Icon::ALL.iter().enumerate() {
            let cp = u32::from(icon.codepoint());
            assert!((0xE000..=0xF8FF).contains(&cp), "{icon:?}");
            for other in &Icon::ALL[index + 1..] {
                assert_ne!(icon.codepoint(), other.codepoint(), "{icon:?} {other:?}");
            }
        }
    }

    /// Run one frame painting `chr` at `center`; whether it was painted and
    /// the painted shapes.
    fn paint_in(ctx: &egui::Context, chr: char, center: Pos2) -> (bool, Vec<egui::Shape>) {
        let mut painted = false;
        // Fonts are installed at the start of a frame.
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                painted = paint_char(ui.painter(), chr, center, TOOLBAR_SIZE, Color32::BLACK);
            });
        });
        let shapes = output.shapes.into_iter().map(|clipped| clipped.shape);
        (painted, shapes.collect())
    }

    #[test]
    fn without_an_icon_font_nothing_is_painted() {
        // The app's fonts on a host without Windows icon fonts.
        let ctx = egui::Context::default();
        ctx.set_fonts(ui_style::font_definitions(None, None));
        for icon in Icon::ALL {
            let (painted, _) = paint_in(&ctx, icon.codepoint(), Pos2::new(50.0, 50.0));
            assert!(!painted, "{icon:?}");
        }
        // A bare context has no icon family either.
        let (painted, _) = paint_in(&egui::Context::default(), 'B', Pos2::new(5.0, 5.0));
        assert!(!painted);
    }

    /// On Windows, the installed icon font has every chrome glyph.
    #[cfg(windows)]
    #[test]
    fn the_windows_icon_font_has_every_chrome_glyph() {
        let ctx = egui::Context::default();
        assert!(ui_style::configure_fonts(&ctx).is_ok());
        for icon in Icon::ALL {
            let (painted, _) = paint_in(&ctx, icon.codepoint(), Pos2::new(50.0, 50.0));
            assert!(painted, "{icon:?}");
        }
    }

    #[test]
    fn icon_font_glyphs_are_painted_ink_centred_and_missing_ones_are_not() {
        // A stand-in "icon font" (a text font) in the icon family.
        let mut fonts = egui::FontDefinitions::default();
        let data = include_bytes!("../../../assets/fonts/SourceSans3-Regular.ttf");
        fonts.font_data.insert(
            "fake-icons".to_owned(),
            Arc::new(egui::FontData::from_static(data)),
        );
        fonts
            .families
            .insert(ui_style::icon_family(), vec!["fake-icons".to_owned()]);
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);

        let center = Pos2::new(40.0, 30.0);
        let (painted, shapes) = paint_in(&ctx, 'H', center);
        assert!(painted);
        let text = shapes.iter().find_map(|shape| match shape {
            egui::Shape::Text(text) => Some(text.visual_bounding_rect()),
            _ => None,
        });
        // The ink of the H (its painted mesh) is centred within a pixel.
        let ink = shapes.iter().find_map(|shape| match shape {
            egui::Shape::Text(text) => {
                let row = text.galley.rows.first()?;
                let glyph = row.glyphs.first()?;
                Some(egui::Rect::from_min_size(
                    text.pos + row.pos.to_vec2() + glyph.pos.to_vec2() + glyph.uv_rect.offset,
                    glyph.uv_rect.size,
                ))
            }
            _ => None,
        });
        assert!(text.is_some(), "{shapes:?}");
        let ink = ink.unwrap_or(egui::Rect::NOTHING);
        assert!((ink.center() - center).length() <= 1.0, "{ink:?}");
        assert!(ink.height() > TOOLBAR_SIZE * 0.5, "a real glyph: {ink:?}");

        // A code point the font lacks is left to the caller.
        let (painted, shapes) = paint_in(&ctx, Icon::Bold.codepoint(), center);
        assert!(!painted);
        assert!(
            !shapes.iter().any(|s| matches!(s, egui::Shape::Text(_))),
            "{shapes:?}"
        );
    }
}
