//! Settings → About: the app's name and version, the copyright, the
//! upstream credit, and the license and third-party notices (embedded, in
//! a scrollable window).

use eframe::egui;

use crate::{
    RustdownApp,
    app_identity::{self, app_name},
};

/// Title of the About window and label of its settings-menu item.
pub(crate) const ABOUT_TITLE: &str = concat!("About ", app_name!());
const ABOUT_ID: &str = "about_window";
const LEGAL_TEXT_ID: &str = "legal_text_window";
/// Widest the license window gets; it stays inside narrow windows.
const LEGAL_TEXT_MAX_WIDTH: f32 = 620.0;

/// The texts the About box can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LegalText {
    License,
    ThirdPartyNotices,
}

impl LegalText {
    pub(crate) const ALL: [Self; 2] = [Self::License, Self::ThirdPartyNotices];

    /// Button label in the About box and title of its window.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::License => "License",
            Self::ThirdPartyNotices => "Third-party notices",
        }
    }

    pub(crate) const fn text(self) -> &'static str {
        match self {
            Self::License => app_identity::LICENSE_TEXT,
            Self::ThirdPartyNotices => app_identity::THIRD_PARTY_NOTICES,
        }
    }
}

impl RustdownApp {
    /// The About window and, over it, the license or notices it opened.
    /// Escape closes the text window first, then the About window.
    pub(crate) fn show_about_window(&mut self, ctx: &egui::Context) {
        use crate::dialog::Role;

        if !self.show_about {
            return;
        }
        let mut clicked = None;
        let (mut show_settings, mut reset) = (false, false);
        let settings = crate::preferences::config_file_path();
        let closed = crate::dialog::show(
            ctx,
            ABOUT_ID,
            ABOUT_TITLE,
            |ui| {
                ui.label(
                    egui::RichText::new(app_identity::APP_NAME)
                        .size(16.0)
                        .strong(),
                );
                ui.label(format!("Version {}", app_identity::VERSION));
                ui.label(app_identity::DESCRIPTION);
                ui.add_space(4.0);
                ui.label(app_identity::COPYRIGHT);
                ui.label(app_identity::BASED_ON);
                ui.horizontal(|ui| {
                    for text in LegalText::ALL {
                        if ui.button(text.title()).clicked() {
                            clicked = Some(text);
                        }
                    }
                });
                ui.add_space(8.0);
                ui.separator();
                ui.label("Settings are saved in:");
                let shown = settings.as_deref().map_or_else(
                    || "(no settings folder)".to_owned(),
                    |path| path.display().to_string(),
                );
                ui.label(egui::RichText::new(shown).monospace().small());
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            settings.is_some(),
                            egui::Button::new("Show in File Explorer"),
                        )
                        .clicked()
                    {
                        show_settings = true;
                    }
                    if ui.button("Reset settings…").clicked() {
                        reset = true;
                    }
                });
            },
            &[("OK", Role::Primary)],
            // Escape closes the text window first.
            self.legal_text.is_none().then_some(0),
        )
        .is_some();
        if let Some(text) = self.legal_text {
            let screen = ctx.viewport_rect();
            let width = (screen.width() - 80.0).clamp(200.0, LEGAL_TEXT_MAX_WIDTH);
            let height = (screen.height() - 260.0).max(120.0);
            let done = crate::dialog::show_wide(
                ctx,
                LEGAL_TEXT_ID,
                width,
                text.title(),
                |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(height)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(text.text()).monospace().small());
                        });
                },
                &[("Close", Role::Primary)],
                Some(0),
            );
            if done.is_some() {
                self.legal_text = None;
            }
        }
        if show_settings {
            self.show_settings_file();
        }
        if reset {
            self.confirm_reset = true;
        }
        if clicked.is_some() {
            self.legal_text = clicked;
        }
        if closed {
            self.show_about = false;
            self.legal_text = None;
        }
    }
}

impl RustdownApp {
    /// Show the settings file in File Explorer (saved first, so it exists).
    fn show_settings_file(&mut self) {
        self.save_preferences();
        let Some(path) = crate::preferences::config_file_path() else {
            return;
        };
        if let Err(err) = crate::explorer_fs::reveal_in_file_explorer(&path) {
            self.tab_mut().error = Some(format!("Couldn't open File Explorer: {err}"));
        }
    }

    /// The "Reset settings?" question from the About box, while asked.
    pub(crate) fn show_reset_settings_prompt(&mut self, ctx: &egui::Context) {
        if !self.confirm_reset {
            return;
        }
        let answer = crate::dialog::show(
            ctx,
            "reset_settings",
            "Reset settings?",
            |ui| {
                ui.label(
                    "Theme, zoom, panels and the other settings go back to their \
                     defaults. Your files and open tabs are not changed.",
                );
            },
            &[
                ("Reset", crate::dialog::Role::Primary),
                ("Cancel", crate::dialog::Role::Secondary),
            ],
            Some(1),
        )
        .map(|index| index == 0);
        if let Some(reset) = answer {
            self.confirm_reset = false;
            if reset {
                self.reset_settings(ctx);
            }
        }
    }

    /// Every setting back to its default (the open tabs stay), saved.
    /// The window keeps its place now and opens at the default size next
    /// time.
    pub(crate) fn reset_settings(&mut self, ctx: &egui::Context) {
        let defaults = crate::preferences::UserPreferences::default();
        self.nav_visible = defaults.nav_visible;
        self.heading_color_mode = defaults.heading_color_mode;
        self.side_by_side_scroll_sync = defaults.side_by_side_scroll_sync;
        self.auto_save = defaults.auto_save;
        self.theme =
            crate::ui_style::ThemeChoice::from_str_lossy(&defaults.theme).unwrap_or_default();
        self.word_wrap = defaults.word_wrap;
        self.status_bar_visible = defaults.show_status_bar;
        self.show_format_toolbar = defaults.show_format_toolbar;
        self.close_window_with_last_tab = defaults.close_window_with_last_tab;
        self.zoom = crate::clamped_zoom_factor(defaults.zoom_factor);
        self.window = crate::window_state::WindowState::default();
        self.explorer.show_all_files = defaults.explorer.show_all_files;
        self.apply_theme(ctx);
        self.save_preferences();
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{Event, Key, Modifiers, PointerButton, Pos2};

    use super::*;
    use crate::test_harness::find_text;

    struct Driver {
        ctx: egui::Context,
        time: f64,
    }

    impl Driver {
        fn new() -> Self {
            let ctx = egui::Context::default();
            assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
            crate::ui_style::configure_style(&ctx);
            Self { ctx, time: 0.0 }
        }

        fn frame(&mut self, app: &mut RustdownApp, events: Vec<Event>) -> egui::FullOutput {
            self.time += 0.1;
            let input = egui::RawInput {
                screen_rect: Some(crate::test_harness::SCREEN),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            self.ctx.run(input, |ctx| app.show_about_window(ctx))
        }

        fn settle(&mut self, app: &mut RustdownApp) -> egui::FullOutput {
            let _ = self.frame(app, Vec::new());
            self.frame(app, Vec::new())
        }

        fn click_text(&mut self, app: &mut RustdownApp, text: &str) {
            let output = self.settle(app);
            let rect = find_text(&output, text);
            assert!(rect.is_some(), "{text:?} is not on screen");
            let pos = rect.map_or(Pos2::ZERO, |rect| rect.center());
            let _ = self.frame(app, vec![Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                let button = Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                };
                let _ = self.frame(app, vec![button]);
            }
        }

        fn escape(&mut self, app: &mut RustdownApp) {
            let _ = self.frame(
                app,
                vec![Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
            );
        }
    }

    fn painted_texts(output: &egui::FullOutput) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn about_box_shows_name_version_copyright_and_credit() {
        let mut driver = Driver::new();
        let mut app = RustdownApp {
            show_about: true,
            ..RustdownApp::default()
        };
        let output = driver.settle(&mut app);
        let version = format!("Version {}", app_identity::VERSION);
        for text in [
            ABOUT_TITLE,
            app_identity::APP_NAME,
            version.as_str(),
            "© 2026 candiesdoodle",
            "Based on rustdown by teh-hippo (MIT License)",
            "License",
            "Third-party notices",
        ] {
            assert!(find_text(&output, text).is_some(), "{text:?}");
        }
        // No upstream link that would suggest teh-hippo maintains it.
        assert!(
            painted_texts(&output)
                .iter()
                .all(|text| !text.contains("github.com")),
            "{:?}",
            painted_texts(&output)
        );
    }

    #[test]
    fn license_and_notices_open_over_the_about_box_and_escape_closes_them_first() {
        let mut driver = Driver::new();
        let mut app = RustdownApp {
            show_about: true,
            ..RustdownApp::default()
        };
        for text in LegalText::ALL {
            driver.click_text(&mut app, text.title());
            assert_eq!(app.legal_text, Some(text));
            let output = driver.settle(&mut app);
            assert!(
                find_text(&output, text.text()).is_some(),
                "the whole {text:?} text is shown"
            );
            assert!(find_text(&output, ABOUT_TITLE).is_some(), "About stays");

            driver.escape(&mut app);
            assert_eq!(app.legal_text, None);
            assert!(app.show_about, "Escape closed the text window only");
        }
        driver.escape(&mut app);
        assert!(!app.show_about);

        // Opening About again starts without a text window.
        app.execute(crate::AppCommand::ShowAbout, &driver.ctx);
        driver.click_text(&mut app, LegalText::License.title());
        assert!(app.legal_text.is_some());
        app.execute(crate::AppCommand::ShowAbout, &driver.ctx);
        assert!(app.show_about && app.legal_text.is_none());
    }

    #[test]
    fn legal_texts_are_the_embedded_files() {
        assert!(LegalText::License.text().contains("MIT License"));
        assert!(
            LegalText::ThirdPartyNotices
                .text()
                .contains("Third-Party Notices")
        );
    }
}
