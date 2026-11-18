//! View → Focus mode (F11): full screen with nothing but the document, its
//! text in a centred column.  F11 or Escape leaves it.

use eframe::egui;

use crate::RustdownApp;

/// How long the "how to leave" hint shows, in seconds.
const HINT_SECONDS: f64 = 2.5;
/// The hint fades out over its last this many seconds.
const HINT_FADE_SECONDS: f64 = 0.5;
/// Width of the text column, in base text sizes (as the preview's
/// reading column).
pub(crate) const COLUMN_WIDTH_EM: f32 = 52.0;

impl RustdownApp {
    /// Enter or leave focus mode (and full screen).
    pub(crate) fn toggle_focus_mode(&mut self, ctx: &egui::Context) {
        self.focus_mode = !self.focus_mode;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.focus_mode));
        self.focus_entered_at = self.focus_mode.then(|| ctx.input(|input| input.time));
    }

    /// The text column in focus mode: at most [`COLUMN_WIDTH_EM`] wide,
    /// centred in `full`.
    pub(crate) fn focus_column(full: egui::Rect) -> egui::Rect {
        let width = full
            .width()
            .min(crate::ui_style::TEXT_SIZE * COLUMN_WIDTH_EM);
        let left = full.center().x - width / 2.0;
        egui::Rect::from_x_y_ranges(left..=left + width, full.y_range())
    }

    /// For a moment after entering focus mode: how to leave it.
    pub(crate) fn show_focus_hint(&mut self, ctx: &egui::Context) {
        let Some(entered) = self.focus_entered_at else {
            return;
        };
        let age = ctx.input(|input| input.time) - entered;
        if !self.focus_mode || age > HINT_SECONDS {
            self.focus_entered_at = None;
            return;
        }
        ctx.request_repaint();
        let opacity = ((HINT_SECONDS - age) / HINT_FADE_SECONDS).min(1.0) as f32;
        egui::Area::new(egui::Id::new("focus_mode_hint"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 16.0))
            .interactable(false)
            .show(ctx, |ui| {
                ui.multiply_opacity(opacity);
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label("Focus mode: press F11 or Esc to leave");
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_column_is_centred_and_never_wider_than_the_window() {
        let wide = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(2000.0, 900.0));
        let column = RustdownApp::focus_column(wide);
        assert!((column.center().x - 1000.0).abs() < 1e-3);
        let expected = crate::ui_style::TEXT_SIZE * COLUMN_WIDTH_EM;
        assert!((column.width() - expected).abs() < 1e-3);
        let narrow = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 900.0));
        assert_eq!(RustdownApp::focus_column(narrow), narrow);
    }
}
