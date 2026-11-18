//! Headless driver for UI tests: runs frames of the window chrome (title
//! bar, menu row, resize edges) with synthetic pointer input, and reads
//! back the viewport commands sent and the text painted.

use eframe::egui::{self, Event, PointerButton, Pos2, Rect, ViewportCommand, pos2};

use crate::RustdownApp;

/// The window, in points.
pub(crate) const SCREEN: Rect = Rect::from_min_max(Pos2::ZERO, pos2(1024.0, 768.0));

pub(crate) struct Harness {
    pub(crate) ctx: egui::Context,
    /// Reported to the app as the window's maximised state.
    pub(crate) maximized: bool,
    time: f64,
}

impl Harness {
    pub(crate) fn new() -> Self {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        crate::ui_style::configure_style(&ctx);
        egui_extras::install_image_loaders(&ctx);
        Self {
            ctx,
            maximized: false,
            time: 0.0,
        }
    }

    /// One frame of the chrome over an empty document area.
    pub(crate) fn frame(&mut self, app: &mut RustdownApp, events: Vec<Event>) -> egui::FullOutput {
        let mut input = egui::RawInput {
            screen_rect: Some(SCREEN),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .maximized = Some(self.maximized);
        self.time += 1.0 / 60.0;
        self.ctx.run(input, |ctx| {
            app.handle_close_request(ctx);
            app.show_title_bar(ctx);
            app.show_menu_bar(ctx);
            egui::CentralPanel::default().show(ctx, |_| {});
            crate::title_bar::show_resize_edges(ctx);
            app.flush_window_close(ctx);
        })
    }

    /// Let `seconds` pass (e.g. so the next clicks are not a double click).
    pub(crate) fn wait(&mut self, seconds: f64) {
        self.time += seconds;
    }

    /// Two idle frames, so widget rects and popups settle.
    pub(crate) fn settle(&mut self, app: &mut RustdownApp) -> egui::FullOutput {
        let _ = self.frame(app, Vec::new());
        self.frame(app, Vec::new())
    }

    pub(crate) fn hover(&mut self, app: &mut RustdownApp, pos: Pos2) -> egui::FullOutput {
        self.frame(app, vec![Event::PointerMoved(pos)])
    }

    pub(crate) fn button(
        &mut self,
        app: &mut RustdownApp,
        pos: Pos2,
        button: PointerButton,
        pressed: bool,
    ) -> egui::FullOutput {
        self.frame(
            app,
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
    }

    /// Hover, press and release `button` at `pos`; the viewport commands
    /// sent during the three frames.
    pub(crate) fn click_with(
        &mut self,
        app: &mut RustdownApp,
        pos: Pos2,
        button: PointerButton,
    ) -> Vec<ViewportCommand> {
        let mut sent = commands(&self.hover(app, pos));
        sent.extend(commands(&self.button(app, pos, button, true)));
        sent.extend(commands(&self.button(app, pos, button, false)));
        sent
    }

    pub(crate) fn click(&mut self, app: &mut RustdownApp, pos: Pos2) -> Vec<ViewportCommand> {
        self.click_with(app, pos, PointerButton::Primary)
    }

    /// Press at `from`, move by `delta` with the button held, release.
    pub(crate) fn drag(
        &mut self,
        app: &mut RustdownApp,
        from: Pos2,
        delta: egui::Vec2,
    ) -> Vec<ViewportCommand> {
        let mut sent = commands(&self.hover(app, from));
        sent.extend(commands(&self.button(
            app,
            from,
            PointerButton::Primary,
            true,
        )));
        for step in 1..=4 {
            let pos = from + delta * (step as f32 / 4.0);
            sent.extend(commands(&self.hover(app, pos)));
        }
        let to = from + delta;
        sent.extend(commands(&self.button(
            app,
            to,
            PointerButton::Primary,
            false,
        )));
        sent
    }

    /// Settle, then click the centre of the first painted text `label`.
    /// Panics (fails the test) when no such text is on screen.
    pub(crate) fn click_text(
        &mut self,
        app: &mut RustdownApp,
        label: &str,
    ) -> Vec<ViewportCommand> {
        let output = self.settle(app);
        let rect = find_text(&output, label);
        assert!(rect.is_some(), "{label:?} is not on screen");
        let center = rect.map_or(Pos2::ZERO, |rect| rect.center());
        self.click(app, center)
    }
}

/// Viewport commands of the root window in `output`.
pub(crate) fn commands(output: &egui::FullOutput) -> Vec<ViewportCommand> {
    output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| viewport.commands.clone())
        .unwrap_or_default()
}

/// Bounds of the first painted text that is exactly `text`.
pub(crate) fn find_text(output: &egui::FullOutput, text: &str) -> Option<Rect> {
    fn walk(shape: &egui::Shape, text: &str) -> Option<Rect> {
        match shape {
            egui::Shape::Text(shape) if shape.galley.text() == text => {
                Some(shape.visual_bounding_rect())
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| walk(shape, text)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|clipped| walk(&clipped.shape, text))
}
