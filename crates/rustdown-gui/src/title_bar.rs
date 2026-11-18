//! Custom title bar of the frameless window, Windows 11 Notepad style: the
//! app icon, the tabs (`tab_strip.rs`), an empty drag region and the
//! minimise / maximise / close buttons.  Plus the invisible resize edges
//! around the window.
//!
//! The window has no OS decorations (`with_decorations(false)`), so this
//! module does what the OS frame did: dragging the empty title bar or the
//! icon moves the window (`StartDrag` runs the system move loop, so
//! snapping to screen edges still works), double-clicking it maximises or
//! restores, and the edges resize (`BeginResize`).  The window title is
//! still sent for the taskbar and Alt+Tab (`update_viewport_title`).
//! Rounded corners and the drop shadow come from DWM
//! (`windows_integration::style_frameless_window`).

use eframe::egui::{
    self, Color32, CursorIcon, Id, Order, Painter, PointerButton, Pos2, Rect, ResizeDirection,
    Sense, Stroke, StrokeKind, ViewportCommand, emath::GuiRounding as _, pos2, vec2,
};

use crate::{
    AppCommand, RustdownApp,
    icons::{self, Icon},
    tab_strip::TAB_HEIGHT,
    ui_style,
};

/// Height of the title bar: the tab row plus a gap above it (Notepad's,
/// measured: 42).
pub(crate) const TITLE_BAR_HEIGHT: f32 = 42.0;
/// Windows 11 caption buttons are 46 px wide (at 100 % scaling).
const CAPTION_BUTTON_WIDTH: f32 = 46.0;
/// Space left of the tabs for the app icon.
pub(crate) const ICON_SLOT_WIDTH: f32 = 40.0;
/// Horizontal centre of the app icon (Notepad's, measured).
const ICON_CENTER_X: f32 = 22.5;
const ICON_SIZE: f32 = 16.0;
/// Empty title bar kept between the tabs and the caption buttons, so the
/// window can always be dragged.
const MIN_DRAG_WIDTH: f32 = 48.0;
/// Half the size of the caption glyphs (10 px squares).
const GLYPH_HALF: f32 = 5.0;
/// Windows 11 close-button hover red, and its pressed shade.
const CLOSE_HOVER_RED: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
const CLOSE_PRESSED_RED: Color32 = Color32::from_rgb(0xC8, 0x3C, 0x30);
/// Width of the invisible resize border along each window edge.
const RESIZE_EDGE: f32 = 5.0;
/// Along an edge, this close to a corner resizes diagonally.
const RESIZE_CORNER: f32 = 12.0;

/// A caption button at the right end of the title bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CaptionButton {
    Minimize,
    Maximize,
    Close,
}

impl CaptionButton {
    /// In layout order from the window's right edge.
    pub(crate) const RIGHT_TO_LEFT: [Self; 3] = [Self::Close, Self::Maximize, Self::Minimize];

    /// The command a click runs.  Close is File → Exit, so unsaved tabs are
    /// prompted for before the window closes.
    pub(crate) const fn command(self) -> AppCommand {
        match self {
            Self::Minimize => AppCommand::MinimizeWindow,
            Self::Maximize => AppCommand::ToggleMaximizeWindow,
            Self::Close => AppCommand::Exit,
        }
    }

    const fn tooltip(self, maximized: bool) -> &'static str {
        match self {
            Self::Minimize => "Minimize",
            Self::Maximize if maximized => "Restore down",
            Self::Maximize => "Maximize",
            Self::Close => "Close",
        }
    }

    const fn index_from_right(self) -> f32 {
        match self {
            Self::Close => 0.0,
            Self::Maximize => 1.0,
            Self::Minimize => 2.0,
        }
    }

    /// Hit area of the button in the title bar `bar` (full bar height).
    pub(crate) fn rect(self, bar: Rect) -> Rect {
        let right = self
            .index_from_right()
            .mul_add(-CAPTION_BUTTON_WIDTH, bar.right());
        Rect::from_min_max(
            pos2(right - CAPTION_BUTTON_WIDTH, bar.top()),
            pos2(right, bar.bottom()),
        )
    }
}

fn window_maximized(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.viewport().maximized.unwrap_or(false))
}

impl RustdownApp {
    /// Row 1: app icon, tabs, drag region and caption buttons.
    pub(crate) fn show_title_bar(&mut self, ctx: &egui::Context) {
        let maximized = window_maximized(ctx);
        // Caption glyphs fade while the window is inactive, as in Windows.
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        let fill = ui_style::title_bar_background(&ctx.style().visuals, focused);
        let mut cmds = Vec::new();
        egui::TopBottomPanel::top("title_bar")
            .frame(egui::Frame::new().fill(fill))
            .exact_height(TITLE_BAR_HEIGHT)
            .show_separator_line(false)
            .show(ctx, |ui| {
                let bar = ui.max_rect();
                // The whole bar is the drag region.  It is created first, so
                // the tabs and buttons on top of it win the pointer; they
                // sense drags too, so only empty space (and the icon) moves
                // the window.
                let drag = ui.interact(bar, ui.id().with("drag_region"), Sense::click_and_drag());
                if drag.double_clicked_by(PointerButton::Primary) {
                    cmds.push(AppCommand::ToggleMaximizeWindow);
                } else if drag.drag_started_by(PointerButton::Primary) {
                    ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                }

                paint_app_icon(ui, bar);
                let caption_left = CaptionButton::Minimize.rect(bar).left();
                let tabs_rect = Rect::from_min_max(
                    pos2(bar.left() + ICON_SLOT_WIDTH, bar.bottom() - TAB_HEIGHT),
                    pos2(
                        (caption_left - MIN_DRAG_WIDTH).max(bar.left() + ICON_SLOT_WIDTH),
                        bar.bottom(),
                    ),
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(tabs_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| self.tab_strip_ui(ui, &mut cmds),
                );
                for button in CaptionButton::RIGHT_TO_LEFT {
                    if caption_button(ui, bar, button, maximized, focused).clicked() {
                        cmds.push(button.command());
                    }
                }
            });
        for cmd in cmds {
            self.execute(cmd, ctx);
        }
    }
}

/// The app icon at the left of the title bar, centred on the tab row.  Not
/// interactive: it is part of the drag region.
fn paint_app_icon(ui: &egui::Ui, bar: Rect) {
    let center = pos2(bar.left() + ICON_CENTER_X, bar.bottom() - TAB_HEIGHT / 2.0);
    let pixels_per_point = ui.pixels_per_point();
    let rect = Rect::from_center_size(center, vec2(ICON_SIZE, ICON_SIZE))
        .round_to_pixels(pixels_per_point);
    egui::Image::new(title_icon(title_icon_px(pixels_per_point)))
        .texture_options(
            egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
        )
        .paint_at(ui, rect);
}

/// Sizes of the hand-tuned title-bar icon renders (`assets/icon/*.png`,
/// made by `tools/build-icon.sh`).
const TITLE_ICON_PX: [u32; 3] = [16, 24, 32];

/// The render closest to the icon's size in physical pixels (16 points ×
/// pixels per point), so it is drawn 1:1 at 100 %, 150 % and 200 %; on a
/// tie the larger one (scaled down rather than up).
fn title_icon_px(pixels_per_point: f32) -> u32 {
    let target = ICON_SIZE * pixels_per_point;
    TITLE_ICON_PX
        .into_iter()
        .rev()
        .min_by(|a, b| {
            let distance = |px: u32| (px as f32 - target).abs();
            distance(*a).total_cmp(&distance(*b))
        })
        .unwrap_or(TITLE_ICON_PX[0])
}

/// The title-bar icon render `px` pixels wide (one of [`TITLE_ICON_PX`]).
const fn title_icon(px: u32) -> egui::ImageSource<'static> {
    match px {
        16 => egui::include_image!("../../../assets/icon/16.png"),
        24 => egui::include_image!("../../../assets/icon/24.png"),
        _ => egui::include_image!("../../../assets/icon/32.png"),
    }
}

/// One caption button: hover fill (red for Close) and its glyph.
fn caption_button(
    ui: &egui::Ui,
    bar: Rect,
    button: CaptionButton,
    maximized: bool,
    focused: bool,
) -> egui::Response {
    let rect = button.rect(bar);
    // Senses drags so a press that wanders off does not move the window.
    let response = ui.interact(
        rect,
        Id::new(("caption_button", button)),
        Sense::click_and_drag(),
    );
    let visuals = ui.visuals();
    let hot = response.hovered() || response.is_pointer_button_down_on();
    let pressed = response.is_pointer_button_down_on();
    let (fill, glyph) = match button {
        CaptionButton::Close if hot => (
            if pressed {
                CLOSE_PRESSED_RED
            } else {
                CLOSE_HOVER_RED
            },
            Color32::WHITE,
        ),
        _ => {
            let fill = if hot {
                ui_style::subtle_fill(visuals, pressed)
            } else {
                Color32::TRANSPARENT
            };
            let glyph = if focused || hot {
                visuals.text_color()
            } else {
                visuals.weak_text_color()
            };
            (fill, glyph)
        }
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, fill);
    paint_caption_glyph(painter, rect.center(), button, maximized, glyph);
    response.on_hover_text(button.tooltip(maximized))
}

/// Windows 11 caption glyphs from the icon font; without it, the same 10 px
/// line art with 1 px strokes on pixel centres.
fn paint_caption_glyph(
    painter: &Painter,
    center: Pos2,
    button: CaptionButton,
    maximized: bool,
    color: Color32,
) {
    let icon = match button {
        CaptionButton::Minimize => Icon::ChromeMinimize,
        CaptionButton::Maximize if maximized => Icon::ChromeRestore,
        CaptionButton::Maximize => Icon::ChromeMaximize,
        CaptionButton::Close => Icon::ChromeClose,
    };
    if icons::paint(painter, icon, center, icons::CAPTION_SIZE, color) {
        return;
    }
    let c = center.round_to_pixel_center(painter.pixels_per_point());
    let stroke = Stroke::new(1.0, color);
    let h = GLYPH_HALF;
    match button {
        CaptionButton::Minimize => {
            painter.hline((c.x - h)..=(c.x + h), c.y, stroke);
        }
        CaptionButton::Maximize if maximized => {
            // Restore: a front square with a second one peeking out behind.
            let front =
                Rect::from_min_max(pos2(c.x - h, c.y - h + 2.0), pos2(c.x + h - 2.0, c.y + h));
            painter.rect_stroke(front, 1.0, stroke, StrokeKind::Middle);
            let back = vec![
                pos2(c.x - h + 2.0, c.y - h + 2.0),
                pos2(c.x - h + 2.0, c.y - h),
                pos2(c.x + h, c.y - h),
                pos2(c.x + h, c.y + h - 2.0),
                pos2(c.x + h - 2.0, c.y + h - 2.0),
            ];
            painter.add(egui::Shape::line(back, stroke));
        }
        CaptionButton::Maximize => {
            let square = Rect::from_center_size(c, vec2(2.0 * h, 2.0 * h));
            painter.rect_stroke(square, 1.0, stroke, StrokeKind::Middle);
        }
        CaptionButton::Close => paint_cross(painter, c, h, color),
    }
}

/// An `×` of half-size `half` centred on `center` (close glyphs).
pub(crate) fn paint_cross(painter: &Painter, center: Pos2, half: f32, color: Color32) {
    let stroke = Stroke::new(1.0, color);
    let (a, b) = (vec2(half, half), vec2(half, -half));
    painter.line_segment([center - a, center + a], stroke);
    painter.line_segment([center - b, center + b], stroke);
}

/// A `+` of half-size `half` centred on `center` (new tab).
pub(crate) fn paint_plus(painter: &Painter, center: Pos2, half: f32, color: Color32) {
    let c = center.round_to_pixel_center(painter.pixels_per_point());
    let stroke = Stroke::new(1.0, color);
    painter.hline((c.x - half)..=(c.x + half), c.y, stroke);
    painter.vline(c.x, (c.y - half)..=(c.y + half), stroke);
}

/// Which way the window resizes from `pos`, if it is on the resize border
/// of a window covering `window`.  Near a corner, the edges resize
/// diagonally.
pub(crate) fn resize_direction(window: Rect, pos: Pos2) -> Option<ResizeDirection> {
    if !window.contains(pos) {
        return None;
    }
    let on_edge = |distance: f32| distance < RESIZE_EDGE;
    let near = |distance: f32| distance < RESIZE_CORNER;
    let (left, right) = (pos.x - window.left(), window.right() - pos.x);
    let (top, bottom) = (pos.y - window.top(), window.bottom() - pos.y);
    let horizontal_edge = on_edge(top) || on_edge(bottom);
    let vertical_edge = on_edge(left) || on_edge(right);
    let west = on_edge(left) || (horizontal_edge && near(left));
    let east = on_edge(right) || (horizontal_edge && near(right));
    let north = on_edge(top) || (vertical_edge && near(top));
    let south = on_edge(bottom) || (vertical_edge && near(bottom));
    match (north, south, west, east) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (true, _, _, true) => Some(ResizeDirection::NorthEast),
        (_, true, true, _) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, ..) => Some(ResizeDirection::North),
        (_, true, ..) => Some(ResizeDirection::South),
        (_, _, true, _) => Some(ResizeDirection::West),
        (_, _, _, true) => Some(ResizeDirection::East),
        _ => None,
    }
}

const fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

/// Invisible resize borders along the window edges: the frameless window
/// has none of its own.  Each is a foreground area, so it takes the
/// pointer before the panels under it.  Off while maximised or fullscreen.
pub(crate) fn show_resize_edges(ctx: &egui::Context) {
    let (maximized, fullscreen) = ctx.input(|input| {
        let viewport = input.viewport();
        (
            viewport.maximized.unwrap_or(false),
            viewport.fullscreen.unwrap_or(false),
        )
    });
    if maximized || fullscreen {
        return;
    }
    let window = ctx.viewport_rect();
    let edges = [
        (
            "north",
            Rect::from_min_max(window.min, pos2(window.right(), window.top() + RESIZE_EDGE)),
        ),
        (
            "south",
            Rect::from_min_max(
                pos2(window.left(), window.bottom() - RESIZE_EDGE),
                window.max,
            ),
        ),
        (
            "west",
            Rect::from_min_max(
                window.min,
                pos2(window.left() + RESIZE_EDGE, window.bottom()),
            ),
        ),
        (
            "east",
            Rect::from_min_max(pos2(window.right() - RESIZE_EDGE, window.top()), window.max),
        ),
    ];
    for (name, rect) in edges {
        egui::Area::new(Id::new(("resize_edge", name)))
            .order(Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            .movable(false)
            .show(ctx, |ui| {
                // Drag-only: the drag starts on press, before any movement.
                let (_, response) = ui.allocate_exact_size(rect.size(), Sense::drag());
                let direction = response
                    .hover_pos()
                    .or_else(|| response.interact_pointer_pos())
                    .and_then(|pos| resize_direction(window, pos));
                if let Some(direction) = direction {
                    ui.ctx().set_cursor_icon(resize_cursor(direction));
                    if response.drag_started_by(PointerButton::Primary) {
                        ui.ctx()
                            .send_viewport_cmd(ViewportCommand::BeginResize(direction));
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CloseChoice,
        test_harness::{Harness, SCREEN, commands},
    };

    #[test]
    fn title_icon_is_the_render_closest_to_its_pixel_size() {
        for (pixels_per_point, px) in [
            (1.0, 16),
            (1.25, 24), // 20 px: between 16 and 24, the larger
            (1.5, 24),
            (1.75, 32), // 28 px: between 24 and 32, the larger
            (2.0, 32),
            (3.0, 32),
        ] {
            assert_eq!(title_icon_px(pixels_per_point), px, "{pixels_per_point}");
        }
        // Each render is a PNG of its size (IHDR width and height).
        for px in TITLE_ICON_PX {
            let egui::ImageSource::Bytes { bytes, .. } = title_icon(px) else {
                unreachable!("embedded bytes")
            };
            let bytes = bytes.as_ref();
            assert!(bytes.starts_with(b"\x89PNG"), "{px}");
            let size = |at: usize| {
                u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
            };
            assert_eq!((size(16), size(20)), (px, px));
        }
    }

    /// The title bar as laid out in the test window.
    fn bar() -> Rect {
        Rect::from_min_size(SCREEN.min, vec2(SCREEN.width(), TITLE_BAR_HEIGHT))
    }

    fn center(button: CaptionButton) -> Pos2 {
        button.rect(bar()).center()
    }

    /// A point of empty title bar between the tabs and the caption buttons.
    fn empty_space() -> Pos2 {
        pos2(center(CaptionButton::Minimize).x - 120.0, 20.0)
    }

    fn icon() -> Pos2 {
        pos2(ICON_SLOT_WIDTH / 2.0, TITLE_BAR_HEIGHT - TAB_HEIGHT / 2.0)
    }

    /// A point on the first tab's title.
    fn first_tab() -> Pos2 {
        pos2(ICON_SLOT_WIDTH + 30.0, TITLE_BAR_HEIGHT - TAB_HEIGHT / 2.0)
    }

    fn maximized_cmds(sent: &[ViewportCommand]) -> Vec<bool> {
        sent.iter()
            .filter_map(|cmd| match cmd {
                ViewportCommand::Maximized(on) => Some(*on),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn caption_buttons_run_window_commands() {
        assert_eq!(
            CaptionButton::Minimize.command(),
            AppCommand::MinimizeWindow
        );
        assert_eq!(
            CaptionButton::Maximize.command(),
            AppCommand::ToggleMaximizeWindow
        );
        // Close is File → Exit: it goes through the unsaved-changes prompt.
        assert_eq!(CaptionButton::Close.command(), AppCommand::Exit);

        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        let _ = ui.settle(&mut app);

        let sent = ui.click(&mut app, center(CaptionButton::Minimize));
        assert!(sent.contains(&ViewportCommand::Minimized(true)), "{sent:?}");

        let sent = ui.click(&mut app, center(CaptionButton::Maximize));
        assert_eq!(maximized_cmds(&sent), [true]);
        ui.maximized = true;
        let sent = ui.click(&mut app, center(CaptionButton::Maximize));
        assert_eq!(maximized_cmds(&sent), [false], "restores when maximised");
        ui.maximized = false;

        // Nothing unsaved: Close closes the window.
        let sent = ui.click(&mut app, center(CaptionButton::Close));
        assert!(sent.contains(&ViewportCommand::Close), "{sent:?}");
        assert!(app.close_prompt.is_none());
    }

    #[test]
    fn close_button_prompts_for_unsaved_tabs_first() {
        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        app.tab_mut().doc.dirty = true;
        let _ = ui.settle(&mut app);

        let sent = ui.click(&mut app, center(CaptionButton::Close));
        assert!(!sent.contains(&ViewportCommand::Close), "{sent:?}");
        assert!(app.close_prompt.as_ref().is_some_and(|p| p.closing_window));

        // Answering the prompt lets the close through.
        app.resolve_close_prompt(CloseChoice::DontSave);
        let sent = commands(&ui.frame(&mut app, Vec::new()));
        assert!(sent.contains(&ViewportCommand::Close), "{sent:?}");
    }

    #[test]
    fn double_click_on_empty_title_bar_or_icon_toggles_maximize() {
        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        let _ = ui.settle(&mut app);

        for pos in [empty_space(), icon()] {
            ui.wait(1.0);
            let mut sent = ui.click(&mut app, pos);
            assert!(
                maximized_cmds(&sent).is_empty(),
                "a single click does nothing"
            );
            sent.extend(ui.click(&mut app, pos));
            assert_eq!(maximized_cmds(&sent), [true], "{pos:?}");
        }

        ui.maximized = true;
        ui.wait(1.0);
        let mut sent = ui.click(&mut app, empty_space());
        sent.extend(ui.click(&mut app, empty_space()));
        assert_eq!(maximized_cmds(&sent), [false]);

        // Not on a tab.
        ui.maximized = false;
        ui.wait(1.0);
        let mut sent = ui.click(&mut app, first_tab());
        sent.extend(ui.click(&mut app, first_tab()));
        assert!(maximized_cmds(&sent).is_empty(), "{sent:?}");
    }

    #[test]
    fn dragging_the_title_bar_moves_the_window_but_not_from_tabs_or_buttons() {
        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        app.new_tab();
        let _ = ui.settle(&mut app);

        for from in [empty_space(), icon()] {
            let sent = ui.drag(&mut app, from, vec2(30.0, 10.0));
            assert!(sent.contains(&ViewportCommand::StartDrag), "{from:?}");
        }
        let maximize = center(CaptionButton::Maximize);
        for from in [first_tab(), maximize] {
            let sent = ui.drag(&mut app, from, vec2(30.0, 10.0));
            assert!(!sent.contains(&ViewportCommand::StartDrag), "{from:?}");
            assert!(maximized_cmds(&sent).is_empty(), "dragged off: no click");
        }
    }

    #[test]
    fn tabs_in_the_title_bar_stay_clickable_and_closable() {
        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        app.new_tab();
        assert_eq!(app.active, 1);
        let _ = ui.settle(&mut app);

        let _ = ui.click(&mut app, first_tab());
        assert_eq!(app.active, 0, "click activates");

        let _ = ui.click_with(&mut app, first_tab(), PointerButton::Middle);
        assert_eq!(app.tabs.len(), 1, "middle-click closes");

        // The `+` button follows the last tab.
        let _ = ui.settle(&mut app);
        let tab_right = ICON_SLOT_WIDTH + crate::tab_strip::TAB_DEFAULT_WIDTH;
        let mut added = false;
        for x in (ICON_SLOT_WIDTH as i32..tab_right as i32 + 60).step_by(8) {
            let _ = ui.click(&mut app, pos2(x as f32, first_tab().y));
            if app.tabs.len() == 2 {
                added = true;
                break;
            }
        }
        assert!(added, "the + button adds a tab");
    }

    #[test]
    fn resize_direction_covers_edges_and_corners() {
        let w = SCREEN;
        let (r, b) = (w.right() - 1.0, w.bottom() - 1.0);
        for (pos, expected) in [
            (pos2(500.0, 1.0), Some(ResizeDirection::North)),
            (pos2(500.0, b), Some(ResizeDirection::South)),
            (pos2(1.0, 400.0), Some(ResizeDirection::West)),
            (pos2(r, 400.0), Some(ResizeDirection::East)),
            (pos2(1.0, 1.0), Some(ResizeDirection::NorthWest)),
            // Along an edge, near a corner: diagonal.
            (pos2(10.0, 1.0), Some(ResizeDirection::NorthWest)),
            (pos2(r, 10.0), Some(ResizeDirection::NorthEast)),
            (pos2(r - 10.0, b), Some(ResizeDirection::SouthEast)),
            (pos2(1.0, b - 10.0), Some(ResizeDirection::SouthWest)),
            (pos2(10.0, 10.0), None),
            (pos2(500.0, 400.0), None),
            (pos2(-5.0, 400.0), None),
        ] {
            assert_eq!(resize_direction(w, pos), expected, "{pos:?}");
        }
    }

    #[test]
    fn window_edges_begin_resize_except_when_maximized() {
        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        let _ = ui.settle(&mut app);
        let (r, b) = (SCREEN.right() - 2.0, SCREEN.bottom() - 2.0);
        for (pos, direction) in [
            (pos2(r, 400.0), ResizeDirection::East),
            (pos2(500.0, b), ResizeDirection::South),
            (pos2(2.0, 400.0), ResizeDirection::West),
            (pos2(500.0, 2.0), ResizeDirection::North),
            (pos2(r, b), ResizeDirection::SouthEast),
            (pos2(2.0, 2.0), ResizeDirection::NorthWest),
        ] {
            let output = ui.hover(&mut app, pos);
            assert_eq!(
                output.platform_output.cursor_icon,
                resize_cursor(direction),
                "{pos:?}"
            );
            let sent = ui.click(&mut app, pos);
            assert!(
                sent.contains(&ViewportCommand::BeginResize(direction)),
                "{pos:?}: {sent:?}"
            );
        }

        ui.maximized = true;
        let _ = ui.settle(&mut app);
        let sent = ui.click(&mut app, pos2(r, 400.0));
        assert!(
            !sent
                .iter()
                .any(|cmd| matches!(cmd, ViewportCommand::BeginResize(_))),
            "{sent:?}"
        );
    }

    #[test]
    fn title_bar_draws_in_light_and_dark_while_inactive_or_maximized() {
        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        app.tab_mut().doc.dirty = true;
        for (dark, maximized) in [(false, false), (true, true)] {
            ui.ctx.set_theme(if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            ui.maximized = maximized;
            let output = ui.settle(&mut app);
            assert!(!output.shapes.is_empty());
            let sent = ui.click(&mut app, center(CaptionButton::Maximize));
            assert_eq!(maximized_cmds(&sent), [!maximized]);
        }
    }
}
