//! Short notes that fade out by themselves (a pasted image was saved…):
//! a small box above the status bar, for a few seconds.

use eframe::egui;

use crate::RustdownApp;

/// How long a note shows, in seconds.
const SHOW_SECONDS: f64 = 4.0;
/// It fades out over its last this many seconds.
const FADE_SECONDS: f64 = 0.6;

impl RustdownApp {
    /// Show `text` as a note for a few seconds.
    pub(crate) fn notify(&mut self, ctx: &egui::Context, text: &str) {
        self.toast = Some((text.to_owned(), ctx.input(|input| input.time)));
        ctx.request_repaint();
    }

    /// The note, while it lasts.
    pub(crate) fn show_toast(&mut self, ctx: &egui::Context) {
        let Some((text, since)) = &self.toast else {
            return;
        };
        let age = ctx.input(|input| input.time) - since;
        if age > SHOW_SECONDS {
            self.toast = None;
            return;
        }
        ctx.request_repaint();
        let opacity = ((SHOW_SECONDS - age) / FADE_SECONDS).min(1.0) as f32;
        egui::Area::new(egui::Id::new("toast"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -48.0))
            .interactable(false)
            .show(ctx, |ui| {
                ui.multiply_opacity(opacity);
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.label(text.as_str());
                    });
            });
    }
}
