//! Windows 11 style dialogs, after `WinUI`'s `ContentDialog`: a title, the
//! message with room around it, and a band along the bottom with the
//! buttons side by side, the main one in the accent colour.  The window
//! behind is dimmed and does not react until the dialog is answered.

use eframe::egui::{self, Color32, CornerRadius};

/// Width of a dialog.
const WIDTH: f32 = 456.0;
/// Space around the title and message, and around the buttons.
const PADDING: i8 = 24;
/// Size of the title (Windows 11's Subtitle).
const TITLE_SIZE: f32 = 20.0;
/// Space between the title and the message.
const TITLE_GAP: f32 = 12.0;
/// Height of a button.
const BUTTON_HEIGHT: f32 = 32.0;
/// Space between buttons.
const BUTTON_GAP: f32 = 8.0;
/// Corner radius of the dialog.
const CORNER: u8 = 8;
/// Size of the close button beside the title.
const CLOSE_SIZE: f32 = 32.0;

/// What a dialog button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// The main answer, in the accent colour.
    Primary,
    /// Any other answer.
    Secondary,
    /// An answer that throws work away: marked with a warning icon.
    Destructive,
}

/// More buttons than this are stacked, one per row.
const MAX_BUTTONS_IN_A_ROW: usize = 3;

/// Show the dialog `id`: `title`, then `body`, then `buttons` (label and
/// role) side by side, equally wide.  Returns the index of the button
/// chosen this frame.  With `escape`, the dialog has a close button beside
/// the title: it and Escape choose `escape` (which may be past the
/// buttons, for an answer that has no button of its own).
pub(crate) fn show(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    body: impl FnOnce(&mut egui::Ui),
    buttons: &[(&str, Role)],
    escape: Option<usize>,
) -> Option<usize> {
    show_wide(ctx, id, WIDTH, title, body, buttons, escape)
}

/// [`show`], `width` wide (long texts).  Up to three buttons sit side by
/// side; more are stacked, each the full width.
pub(crate) fn show_wide(
    ctx: &egui::Context,
    id: &str,
    width: f32,
    title: &str,
    body: impl FnOnce(&mut egui::Ui),
    buttons: &[(&str, Role)],
    escape: Option<usize>,
) -> Option<usize> {
    let mut dismissed = ctx.input(|input| input.key_pressed(egui::Key::Escape));
    let visuals = ctx.style().visuals.clone();
    let frame = egui::Frame::new()
        .fill(visuals.window_fill)
        .stroke(visuals.window_stroke)
        .corner_radius(CORNER)
        .shadow(visuals.popup_shadow);
    let content_width = 2.0f32.mul_add(-f32::from(PADDING), width);
    let mut chosen = None;
    egui::Modal::new(egui::Id::new(id))
        .frame(frame)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::new()
                .inner_margin(egui::Margin::same(PADDING))
                .show(ui, |ui| {
                    ui.set_width(content_width);
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = BUTTON_GAP;
                        let close = if escape.is_some() {
                            CLOSE_SIZE + BUTTON_GAP
                        } else {
                            0.0
                        };
                        let title_width = content_width - close;
                        ui.allocate_ui(egui::vec2(title_width, 0.0), |ui| {
                            ui.set_width(title_width);
                            ui.add(egui::Label::new(title_text(ui, title)).wrap());
                        });
                        if escape.is_some() && close_button(ui).clicked() {
                            dismissed = true;
                        }
                    });
                    ui.add_space(TITLE_GAP);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    body(ui);
                });
            if buttons.is_empty() {
                return;
            }
            let bottom = CornerRadius {
                nw: 0,
                ne: 0,
                sw: CORNER,
                se: CORNER,
            };
            egui::Frame::new()
                .fill(visuals.panel_fill)
                .corner_radius(bottom)
                .inner_margin(egui::Margin::same(PADDING))
                .show(ui, |ui| {
                    ui.set_width(content_width);
                    if buttons.len() > MAX_BUTTONS_IN_A_ROW {
                        ui.spacing_mut().item_spacing.y = BUTTON_GAP;
                        for (index, &(label, role)) in buttons.iter().enumerate() {
                            if button(ui, label, role, content_width).clicked() {
                                chosen = Some(index);
                            }
                        }
                        return;
                    }
                    let count = buttons.len() as f32;
                    let width = BUTTON_GAP.mul_add(-(count - 1.0), content_width) / count;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = BUTTON_GAP;
                        for (index, &(label, role)) in buttons.iter().enumerate() {
                            if button(ui, label, role, width).clicked() {
                                chosen = Some(index);
                            }
                        }
                    });
                });
        });
    chosen.or_else(|| escape.filter(|_| dismissed))
}

/// The close button beside the title: the icon font's Cancel cross, else ✕.
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let size = egui::vec2(CLOSE_SIZE, CLOSE_SIZE);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let response = response.on_hover_text("Close (Esc)");
    let visuals = ui.visuals();
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 4, visuals.widgets.hovered.weak_bg_fill);
    }
    let color = visuals.text_color();
    let painter = ui.painter();
    if !crate::icons::paint(
        painter,
        crate::icons::Icon::Cancel,
        rect.center(),
        12.0,
        color,
    ) {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{2715}",
            egui::FontId::proportional(14.0),
            color,
        );
    }
    response
}

/// The title: Windows 11's Subtitle, semibold (the bold font when the app's
/// fonts have it).
fn title_text(ui: &egui::Ui, title: &str) -> egui::RichText {
    let text = egui::RichText::new(title).size(TITLE_SIZE).strong();
    if crate::ui_style::has_bold_family(ui) {
        text.family(crate::ui_style::bold_family())
    } else {
        text
    }
}

/// A dialog button `width` wide: the primary one filled with the accent
/// colour (dark text on the dark theme's light accent), a destructive one
/// with a warning icon in the error colour, the others plain.
fn button(ui: &mut egui::Ui, label: &str, role: Role, width: f32) -> egui::Response {
    let visuals = ui.visuals();
    let button = match role {
        Role::Primary => {
            let ink = if visuals.dark_mode {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            egui::Button::new(egui::RichText::new(label).color(ink))
                .fill(crate::ui_style::accent_fill(visuals))
        }
        Role::Secondary => egui::Button::new(label),
        Role::Destructive => egui::Button::new(warning_label(ui, label)),
    };
    ui.add_sized([width, BUTTON_HEIGHT], button.corner_radius(4))
}

/// `label` after a warning icon in the error colour: the icon font's
/// Warning glyph, else the ⚠ character.
fn warning_label(ui: &egui::Ui, label: &str) -> egui::text::LayoutJob {
    let visuals = ui.visuals();
    let body = egui::TextStyle::Button.resolve(ui.style());
    let icon_font = egui::FontId::new(body.size, crate::ui_style::icon_family());
    let has_icon = ui.fonts(|fonts| fonts.definitions().families.contains_key(&icon_font.family));
    let (icon, font) = if has_icon {
        (WARNING_GLYPH, icon_font)
    } else {
        ('\u{26A0}', body.clone())
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &icon.to_string(),
        0.0,
        egui::TextFormat::simple(font, visuals.error_fg_color),
    );
    job.append(
        label,
        8.0,
        egui::TextFormat::simple(body, visuals.text_color()),
    );
    job
}

/// Segoe Fluent Icons / MDL2 Assets "Warning".
const WARNING_GLYPH: char = '\u{E7BA}';
