//! The fixed text size and the zoom, end to end: the size of the text
//! painted in each part of the window (menu row, tabs, explorer, status
//! bar, editor, preview), the zoom slider of the settings menu (zoom
//! scales the whole window), and the explorer's context menus (the shared
//! menu rows).

use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{self, Event, Key, Modifiers};

use crate::{AppCommand, Mode, RustdownApp, preferences::UserPreferences, ui_style};

const DOC: &str =
    "# Title\n\n###### Sixth\n\nBody paragraph with `code` in it.\n\n```\nfenced code\n```\n";
/// The preview lays the paragraph out without the backticks.
const PREVIEW_PARAGRAPH: &str = "Body paragraph with code in it.";

struct Window {
    ctx: egui::Context,
    time: f64,
    folder: PathBuf,
}

impl Window {
    /// A window with the app's fonts and style, and a folder holding
    /// `notes.md` for the explorer.
    fn new(name: &str) -> Self {
        let ctx = egui::Context::default();
        assert!(ui_style::configure_fonts(&ctx).is_ok());
        ui_style::configure_style(&ctx);
        let folder =
            std::env::temp_dir().join(format!("rustdown-text-size-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        let _ = fs::create_dir_all(&folder);
        let _ = fs::write(folder.join("notes.md"), "# Notes\n");
        Self {
            ctx,
            time: 0.0,
            folder,
        }
    }

    /// An app showing `DOC` in Split, with the folder in the explorer.
    fn app(&self, prefs: &UserPreferences) -> RustdownApp {
        let options = crate::cli::parse_launch_options(Vec::<String>::new());
        let mut app = RustdownApp::from_launch_options_and_preferences(options, prefs);
        app.mode = Mode::SideBySide;
        app.tab_mut().doc.text = Arc::new(DOC.to_owned());
        app.open_folder(&self.folder);
        // The folder is listed on a worker thread; wait for it (slower on
        // the Windows runners than on the dev host).
        let deadline = Instant::now() + Duration::from_secs(10);
        let show_all = app.explorer.show_all_files;
        if let Some(tree) = app.explorer.tree.as_mut() {
            loop {
                let _ = tree.poll(None, Instant::now());
                tree.prepare_rows(show_all);
                if !tree.is_loading() || Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        app
    }

    /// One frame of the whole window (as `update` draws it); a few, so
    /// panels and the preview's heights settle.
    fn frames(&mut self, app: &mut RustdownApp, events: Vec<Event>) -> egui::FullOutput {
        let mut output = self.frame(app, events);
        for _ in 0..2 {
            output = self.frame(app, Vec::new());
        }
        output
    }

    fn frame(&mut self, app: &mut RustdownApp, events: Vec<Event>) -> egui::FullOutput {
        self.frame_after(app, events, 1.0)
    }

    /// Click at `pos` (press and release close together, as a click), then
    /// let the window settle.
    fn click(&mut self, app: &mut RustdownApp, pos: egui::Pos2) -> egui::FullOutput {
        self.click_with(app, pos, egui::PointerButton::Primary)
    }

    fn click_with(
        &mut self,
        app: &mut RustdownApp,
        pos: egui::Pos2,
        button: egui::PointerButton,
    ) -> egui::FullOutput {
        let _ = self.frame_after(app, vec![Event::PointerMoved(pos)], 0.5);
        for pressed in [true, false] {
            let button = Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: Modifiers::NONE,
            };
            let _ = self.frame_after(app, vec![button], 0.05);
        }
        self.frames(app, Vec::new())
    }

    /// One frame, `seconds` after the previous one.
    fn frame_after(
        &mut self,
        app: &mut RustdownApp,
        events: Vec<Event>,
        seconds: f64,
    ) -> egui::FullOutput {
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::MouseWheel { modifiers, .. } => {
                    Some(*modifiers)
                }
                _ => None,
            })
            .unwrap_or_default();
        let input = egui::RawInput {
            screen_rect: Some(crate::test_harness::SCREEN),
            time: Some(self.time),
            modifiers,
            events,
            ..Default::default()
        };
        self.time += seconds;
        self.ctx.run(input, |ctx| {
            app.sync_zoom(ctx);
            app.handle_keyboard_shortcuts(ctx);
            app.show_title_bar(ctx);
            app.show_menu_bar(ctx);
            app.show_status_bar(ctx);
            app.show_explorer_panel(ctx);
            app.show_content_panels(ctx);
        })
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.folder);
    }
}

/// The galleys painted in `output`.
fn galleys(output: &egui::FullOutput) -> Vec<Arc<egui::Galley>> {
    fn walk(shape: &egui::Shape, out: &mut Vec<Arc<egui::Galley>>) {
        match shape {
            egui::Shape::Text(text) => out.push(text.galley.clone()),
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

/// Font of `needle` in the first painted galley whose whole text is
/// `text`.
fn font_in(output: &egui::FullOutput, text: &str, needle: &str) -> egui::FontId {
    let galley = galleys(output)
        .into_iter()
        .find(|galley| galley.text() == text);
    assert!(galley.is_some(), "{text:?} is not painted");
    let galley = galley.unwrap_or_else(|| unreachable!());
    let byte = text.find(needle).unwrap_or_else(|| unreachable!());
    galley
        .job
        .sections
        .iter()
        .find(|section| section.byte_range.contains(&byte))
        .map_or_else(
            || unreachable!("{needle:?} has no section"),
            |section| section.format.font_id.clone(),
        )
}

/// Bounds of every painted text that is exactly `text`, in paint order
/// (popups last).
fn text_rects(output: &egui::FullOutput, text: &str) -> Vec<egui::Rect> {
    let mut rects = Vec::new();
    for clipped in &output.shapes {
        let mut found = Vec::new();
        walk_text(&clipped.shape, text, &mut found);
        rects.extend(found);
    }
    rects
}

fn walk_text(shape: &egui::Shape, text: &str, out: &mut Vec<egui::Rect>) {
    match shape {
        egui::Shape::Text(shape) if shape.galley.text() == text => {
            out.push(shape.visual_bounding_rect());
        }
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk_text(shape, text, out)),
        _ => {}
    }
}

/// Size of the painted text `text` (a whole galley).
fn size_of(output: &egui::FullOutput, text: &str) -> f32 {
    font_in(output, text, text).size
}

/// The sizes of every part of the window.
#[derive(Debug, PartialEq)]
struct Painted {
    menu: f32,
    tree: f32,
    tab: f32,
    status: f32,
    preview_body: f32,
    preview_h1: f32,
    preview_h6: f32,
    preview_code: f32,
    editor_body: f32,
    editor_h1: f32,
    editor_code: f32,
}

fn painted(output: &egui::FullOutput) -> Painted {
    Painted {
        menu: size_of(output, "File"),
        tree: size_of(output, "notes.md"),
        tab: size_of(output, "Untitled"),
        status: size_of(output, "UTF-8"),
        preview_body: font_in(output, PREVIEW_PARAGRAPH, "Body").size,
        preview_h1: size_of(output, "Title"),
        preview_h6: size_of(output, "Sixth"),
        preview_code: size_of(output, "fenced code"),
        editor_body: font_in(output, DOC, "Body").size,
        editor_h1: font_in(output, DOC, "Title").size,
        editor_code: font_in(output, DOC, "fenced").size,
    }
}

/// What every part should be (in Split: raw editor | preview), in points:
/// the base sizes whatever the zoom, which scales the whole window
/// (egui's zoom factor).
fn expected() -> Painted {
    let base = ui_style::TextSizes::base();
    let [h1, .., h6] = crate::md::HEADING_FONT_SCALES;
    Painted {
        menu: base.body,
        tree: base.body,
        tab: base.small,
        status: base.small,
        preview_body: base.body,
        preview_h1: base.body * h1,
        preview_h6: base.body * h6,
        preview_code: base.body * ui_style::MONOSPACE_SCALE,
        // Split: the raw editor, one size (the code size) throughout.
        editor_body: base.monospace,
        editor_h1: base.monospace,
        editor_code: base.monospace,
    }
}

fn key(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn one_base_size_reaches_menus_tree_tabs_status_and_the_document() {
    let mut window = Window::new("base");
    let mut app = window.app(&UserPreferences::default());
    let output = window.frames(&mut app, Vec::new());
    let sizes = painted(&output);
    assert_eq!(sizes, expected());
    // Notepad's: Body 14 for the menus, the tree and the text; Caption 12
    // for the tabs and the status bar.
    assert_eq!(
        (sizes.menu, sizes.tree, sizes.preview_body),
        (14.0, 14.0, 14.0)
    );
    assert_eq!(sizes.editor_body, 14.0 * ui_style::MONOSPACE_SCALE);
    assert_eq!((sizes.tab, sizes.status), (12.0, 12.0));
    assert_eq!((sizes.preview_h1, sizes.preview_h6), (21.0, 14.0));
    // Headings are bold, so an H6 (body size) still stands out.
    let bold = ui_style::bold_family();
    assert_eq!(font_in(&output, "Sixth", "Sixth").family, bold);
    // The raw editor of Split: monospace throughout.
    assert_eq!(
        font_in(&output, DOC, "Sixth").family,
        egui::FontFamily::Monospace
    );
    assert_eq!(
        font_in(&output, PREVIEW_PARAGRAPH, "Body").family,
        egui::FontFamily::Proportional
    );
    // Code is monospace in both views.
    assert_eq!(
        font_in(&output, DOC, "fenced").family,
        egui::FontFamily::Monospace
    );
    assert_eq!(
        font_in(&output, PREVIEW_PARAGRAPH, "code").family,
        egui::FontFamily::Monospace
    );

    // Edit: the formatted editor, headings 1.5 × down to 1.0 × and bold,
    // body at the base size, code 1.02 ×.
    app.mode = Mode::Edit;
    let output = window.frames(&mut app, Vec::new());
    assert_eq!(font_in(&output, DOC, "Title").size, 21.0);
    assert_eq!(font_in(&output, DOC, "Title").family, bold);
    assert_eq!(font_in(&output, DOC, "Sixth").size, 14.0);
    assert_eq!(font_in(&output, DOC, "Sixth").family, bold);
    assert_eq!(font_in(&output, DOC, "Body").size, 14.0);
    assert_eq!(
        font_in(&output, DOC, "fenced").size,
        14.0 * ui_style::MONOSPACE_SCALE
    );
}

#[test]
fn old_text_size_setting_is_ignored() {
    let mut window = Window::new("old-setting");
    let prefs = UserPreferences::from_toml("text_size = 18\nzoom_factor = 1.5\n");
    let mut app = window.app(&prefs);
    let output = window.frames(&mut app, Vec::new());
    assert_eq!(painted(&output), expected(), "14 px");
    assert_eq!(window.ctx.zoom_factor(), 1.5, "zoomed 150 %");
}

#[test]
fn gear_menu_has_a_zoom_slider_and_no_text_settings() {
    let mut window = Window::new("gear-zoom");
    let mut app = window.app(&UserPreferences::default());
    let _ = window.frames(&mut app, Vec::new());
    let gear = crate::menu::settings_button_rect(&window.ctx);
    assert!(gear.is_some());
    let output = window.click(
        &mut app,
        gear.map_or(egui::Pos2::ZERO, |rect| rect.center()),
    );
    let find = |text| crate::test_harness::find_text(&output, text);
    assert!(find("Zoom").is_some());
    assert!(find("100 %").is_some(), "the slider's value");
    assert!(find("Text size").is_none(), "no text size setting");
    assert!(find("Text contrast").is_none(), "no text contrast setting");
}

#[test]
fn zoom_scales_the_whole_window() {
    let mut window = Window::new("zoom");
    let mut app = window.app(&UserPreferences::default());
    let before = window.frames(&mut app, Vec::new());
    assert_eq!(painted(&before), expected());
    assert_eq!(window.ctx.zoom_factor(), 1.0);

    // Ctrl+Plus twice: 120 %, for everything (sizes in points stay).
    for _ in 0..2 {
        let _ = window.frames(&mut app, vec![key(Key::Plus, Modifiers::COMMAND)]);
    }
    assert!((app.zoom - 1.2).abs() < 1e-6, "{}", app.zoom);
    let output = window.frames(&mut app, Vec::new());
    assert_eq!(painted(&output), expected());
    assert!((window.ctx.zoom_factor() - 1.2).abs() < 1e-6);
    assert!(
        crate::test_harness::find_text(&output, "120%").is_some(),
        "the status bar shows the zoom"
    );
    assert_eq!(app.preferences().zoom_factor, app.zoom, "saved");

    // Ctrl+Minus, then Ctrl+0 back to 100 %.
    let _ = window.frames(&mut app, vec![key(Key::Minus, Modifiers::COMMAND)]);
    assert!((app.zoom - 1.1).abs() < 1e-6, "{}", app.zoom);
    let _ = window.frames(&mut app, vec![key(Key::Num0, Modifiers::COMMAND)]);
    assert_eq!(app.zoom, 1.0);
    let _ = window.frames(&mut app, Vec::new());
    assert_eq!(window.ctx.zoom_factor(), 1.0);

    // Ctrl+wheel zooms too.
    let wheel = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 1.0),
        modifiers: Modifiers::COMMAND,
    };
    let _ = window.frames(&mut app, vec![wheel]);
    assert!(app.zoom > 1.0, "{}", app.zoom);
    let _ = window.frames(&mut app, Vec::new());
    assert!(window.ctx.zoom_factor() > 1.0);

    // The zoom range.
    for _ in 0..30 {
        app.execute(AppCommand::ZoomOut, &window.ctx);
    }
    assert_eq!(app.zoom, crate::MIN_ZOOM_FACTOR);
}

#[test]
fn persisted_zoom_restores_at_startup() {
    let mut window = Window::new("restore");
    let prefs = UserPreferences {
        zoom_factor: 1.5,
        ..UserPreferences::default()
    };
    let mut app = window.app(&prefs);
    assert_eq!(app.zoom, 1.5);
    let output = window.frames(&mut app, Vec::new());
    assert_eq!(painted(&output), expected());
    assert!(crate::test_harness::find_text(&output, "150%").is_some());
    assert_eq!(window.ctx.zoom_factor(), 1.5);
    // Saved as it was loaded.
    assert_eq!(app.preferences().zoom_factor, 1.5);

    // Out-of-range or broken values are clamped.
    for (stored, zoom) in [(10.0, crate::MAX_ZOOM_FACTOR), (f32::NAN, 1.0)] {
        let prefs = UserPreferences {
            zoom_factor: stored,
            ..UserPreferences::default()
        };
        assert_eq!(window.app(&prefs).zoom, zoom, "{stored}");
    }
}

#[test]
fn editor_lays_out_again_when_the_zoom_changes() {
    let mut window = Window::new("caches");
    let mut app = window.app(&UserPreferences::default());
    let _ = window.frames(&mut app, Vec::new());
    let editor_key = |app: &RustdownApp| {
        app.tab()
            .doc
            .editor_galley_cache
            .as_ref()
            .map(|cache| (cache.pixels_per_point_bits, Arc::as_ptr(&cache.galley)))
    };
    let editor = editor_key(&app);
    assert!(editor.is_some());

    // Nothing changed: kept.
    let _ = window.frames(&mut app, Vec::new());
    assert_eq!(editor_key(&app), editor);

    // Zoom: the galley is laid out again for the new pixel density.
    app.execute(AppCommand::ZoomIn, &window.ctx);
    let _ = window.frames(&mut app, Vec::new());
    let zoomed = editor_key(&app);
    assert_ne!(zoomed.map(|key| key.0), editor.map(|key| key.0));
}

/// The explorer's context menus are drawn like the other popups: items
/// 30 apart at the shared text column, after the lead column only in the
/// root's menu (its "Show all files" check item).
#[test]
fn explorer_context_menus_use_the_menu_rows() {
    use crate::menu::{MENU_LEAD_WIDTH, MENU_ROW_HEIGHT, MENU_ROW_PADDING_X, popup_style};

    let mut window = Window::new("ctx");
    let mut app = window.app(&UserPreferences::default());
    app.show_explorer = true;
    let output = window.frames(&mut app, Vec::new());
    let mut style = (*window.ctx.style()).clone();
    popup_style(&mut style);
    let margin = egui::Frame::menu(&style).total_margin().left;
    let last = |output: &egui::FullOutput, text: &str| {
        let rects = text_rects(output, text);
        assert!(!rects.is_empty(), "{text:?} is not painted");
        rects.last().copied().unwrap_or(egui::Rect::NOTHING)
    };
    let secondary = egui::PointerButton::Secondary;
    let folder = crate::explorer_ui::PANEL_TITLE.to_owned();
    for (target, items, lead) in [
        ("notes.md", ["Copy path", "Reveal in File Explorer"], 0.0),
        (
            folder.as_str(),
            ["Collapse all", "Show all files"],
            MENU_LEAD_WIDTH,
        ),
    ] {
        let at = last(&output, target).center();
        let menu = window.click_with(&mut app, at, secondary);
        let (first, second) = (last(&menu, items[0]), last(&menu, items[1]));
        // Glyph ink (both labels have ascenders, so their tops line up).
        let pitch = second.top() - first.top();
        assert!((pitch - MENU_ROW_HEIGHT).abs() <= 1.0, "{target}: {pitch}");
        assert!((second.left() - first.left()).abs() <= 1.5, "{target}");
        let layer = window.ctx.layer_id_at(first.center());
        let popup = layer.and_then(|layer| window.ctx.memory(|m| m.area_rect(layer.id)));
        let popup = popup.unwrap_or(egui::Rect::NOTHING);
        let column = popup.left() + margin + MENU_ROW_PADDING_X + lead;
        assert!(
            (first.left() - column).abs() <= 2.0,
            "{target}: {first:?} {popup:?}"
        );
        if lead > 0.0 {
            let shown = app.explorer.show_all_files;
            let _ = window.click(&mut app, second.center());
            assert_eq!(app.explorer.show_all_files, !shown, "the check item works");
        } else {
            let _ = window.frames(
                &mut app,
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
}
