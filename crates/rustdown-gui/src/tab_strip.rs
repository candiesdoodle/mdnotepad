//! Notepad-style tabs in the title bar (see `title_bar.rs`): one tab per
//! document, then a `+` new-tab button.
//!
//! Styled like Windows 11 Notepad (a `WinUI` tab view): tabs are 240 wide
//! while they fit and shrink evenly when they do not; titles are Caption
//! size (12).  The active tab has the menu row's background, a thin
//! outline, rounded top corners and curved feet, so it joins the row below
//! like a folder tab; inactive tabs are flat, with thin dividers and a
//! subtle hover fill.  A dirty tab shows `•` in its close slot, which turns
//! into `×` on hover; a clean tab shows `×` while active or hovered.  Long
//! titles end in `…`.

use std::sync::Arc;

use eframe::egui::{
    self, CornerRadius, Galley, Pos2, Rect, Sense, Stroke, TextWrapMode, pos2, vec2,
};

use crate::{
    AppCommand, RustdownApp,
    icons::{self, Icon},
    tab::{Tab, TabId},
    title_bar, ui_style,
};

/// Height of a tab (the title bar's tab row).
pub(crate) const TAB_HEIGHT: f32 = 32.0;
/// Width of every tab while they fit: `WinUI`'s tab maximum, Notepad's
/// default.
pub(crate) const TAB_DEFAULT_WIDTH: f32 = 240.0;
/// Tabs shrink evenly down to this width; past it the row scrolls.
pub(crate) const TAB_MIN_WIDTH: f32 = 100.0;
/// Title inset from the tab's left edge: the outline plus `WinUI`'s padding.
const TITLE_INSET: f32 = 9.0;
/// The title sits this far below the tab's centre, as in Notepad.
const TITLE_DROP: f32 = 1.0;
/// Close button: `WinUI`'s 32 × 24, inset from the tab's right edge.
const CLOSE_SIZE: egui::Vec2 = vec2(32.0, 24.0);
const CLOSE_INSET: f32 = 5.0;
const TITLE_CLOSE_GAP: f32 = 4.0;
/// Space between tabs (the dividers of inactive tabs sit in it).
const TAB_GAP: f32 = 2.0;
/// How long tabs take to slide into a new place (a drag, a tab opened or
/// closed), in seconds.
const TAB_SLIDE_TIME: f32 = 0.15;
const TAB_CORNER: u8 = 8;
/// Radius of the curved feet at the active tab's bottom corners.
const FOOT: f32 = 6.0;
/// Space before the first tab and between the last tab and `+`: room for
/// the active tab's feet.
pub(crate) const TAB_ROW_PADDING: f32 = FOOT + 1.0;
pub(crate) const NEW_TAB_SIZE: egui::Vec2 = vec2(32.0, 28.0);
/// The `+` sits this far below the tab row's centre, as in Notepad.
const NEW_TAB_DROP: f32 = 1.5;
/// Widest title that fits in a tab of default width.
const MAX_TITLE_WIDTH: f32 =
    TAB_DEFAULT_WIDTH - TITLE_INSET - TITLE_CLOSE_GAP - CLOSE_SIZE.x - CLOSE_INSET;

/// What a tab's close slot shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseSlot {
    Empty,
    /// `•`: unsaved changes.
    Dot,
    /// `×`: the close button.
    Cross,
}

/// Notepad: hovering shows `×`; otherwise a dirty tab shows `•` and a clean
/// active tab `×`.
const fn close_slot(active: bool, dirty: bool, hovered: bool) -> CloseSlot {
    if hovered || (active && !dirty) {
        CloseSlot::Cross
    } else if dirty {
        CloseSlot::Dot
    } else {
        CloseSlot::Empty
    }
}

/// One laid-out tab.  Tabs are painted after all are laid out, the active
/// one last, so its feet overlap its neighbours.
#[allow(clippy::struct_excessive_bools)]
struct TabVisual {
    /// The tab's id, and where the pointer is while the tab is dragged.
    id: TabId,
    dragged_to: Option<f32>,
    /// Where in the tab the drag took hold of it (from its left edge).
    grab: f32,
    /// Its place in the row (clicks), and where it is painted: sliding
    /// towards that place, or under the pointer while dragged.
    rect: Rect,
    close_rect: Rect,
    paint: Rect,
    close_paint: Rect,
    galley: Arc<Galley>,
    active: bool,
    dirty: bool,
    hovered: bool,
    close_hovered: bool,
    close_pressed: bool,
}

impl RustdownApp {
    /// The tabs and the `+` button, left to right in `ui` (the title bar's
    /// tab row).  Clicks are queued in `cmds`.
    pub(crate) fn tab_strip_ui(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>) {
        let scroll_to_active = std::mem::take(&mut self.scroll_to_active_tab);
        let width = tab_width(ui.available_width(), self.tabs.len());
        egui::ScrollArea::horizontal()
            .id_salt("tab_strip_scroll")
            .auto_shrink([true, false])
            // No drag-to-scroll: dragging the title bar moves the window.
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: false,
                ..egui::scroll_area::ScrollSource::ALL
            })
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                let row = egui::Layout::left_to_right(egui::Align::Center);
                ui.with_layout(row, |ui| {
                    ui.spacing_mut().item_spacing.x = TAB_GAP;
                    ui.add_space(TAB_ROW_PADDING);
                    let mut tabs = Vec::with_capacity(self.tabs.len());
                    for (index, tab) in self.tabs.iter().enumerate() {
                        let active = index == self.active;
                        tabs.push(tab_button(
                            ui,
                            tab,
                            width,
                            active,
                            active && scroll_to_active,
                            cmds,
                        ));
                    }
                    place_tabs(ui, &mut tabs);
                    paint_tabs(ui, &tabs);
                    queue_tab_move(&tabs, cmds);
                    ui.add_space(TAB_ROW_PADDING - TAB_GAP);
                    new_tab_button(ui, cmds);
                });
            });
    }
}

/// Tab label: title, prefixed with `⚠` when a question about its changed file waits.
fn tab_label(tab: &Tab) -> String {
    let mut label = String::new();
    if tab.disk.awaits_answer() {
        label.push_str("⚠ ");
    }
    label.push_str(&tab.title());
    label
}

/// Width of each of `count` tabs in a tab row `row_width` wide: the default
/// width while they all fit next to the `+` button, else an even share of
/// the row, but never below the minimum (the row then scrolls).  Whole
/// points, so tab edges stay crisp.
pub(crate) fn tab_width(row_width: f32, count: usize) -> f32 {
    let count = count.max(1) as f32;
    let fixed = TAB_GAP.mul_add(count - 1.0, 2.0f32.mul_add(TAB_ROW_PADDING, NEW_TAB_SIZE.x));
    let share = ((row_width - fixed) / count).floor();
    share.clamp(TAB_MIN_WIDTH, TAB_DEFAULT_WIDTH)
}

/// Widest title that fits in a tab `width` wide.
fn max_title_width(width: f32) -> f32 {
    MAX_TITLE_WIDTH - (TAB_DEFAULT_WIDTH - width)
}

/// Lay out one tab `width` wide and queue its clicks.
fn tab_button(
    ui: &mut egui::Ui,
    tab: &Tab,
    width: f32,
    active: bool,
    scroll_into_view: bool,
    cmds: &mut Vec<AppCommand>,
) -> TabVisual {
    // Notepad's Caption size (the Small text style).
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = egui::WidgetText::from(egui::RichText::new(tab_label(tab)).font(font))
        .into_galley(
            ui,
            Some(TextWrapMode::Truncate),
            max_title_width(width),
            egui::TextStyle::Button,
        );
    // Senses drags too, so a drag that starts on a tab does not move the
    // window (the title bar behind it starts window drags) but the tab.
    // Its id follows the tab, not its place, so a drag survives the move.
    let (rect, _) = ui.allocate_exact_size(vec2(width, TAB_HEIGHT), Sense::hover());
    let response = ui.interact(
        rect,
        egui::Id::new(("tab", tab.id)),
        Sense::click_and_drag(),
    );
    let close_rect = Rect::from_center_size(
        pos2(
            rect.right() - CLOSE_INSET - CLOSE_SIZE.x / 2.0,
            rect.center().y,
        ),
        CLOSE_SIZE,
    );
    // Created after the tab, so it wins clicks on the overlap.
    let close = ui
        .interact(close_rect, response.id.with("close"), Sense::click())
        .on_hover_text("Close tab");
    let response = response.on_hover_text(tab.doc.path_label());

    if scroll_into_view {
        response.scroll_to_me(None);
    }
    if response.clicked() || response.drag_started_by(egui::PointerButton::Primary) {
        cmds.push(AppCommand::ActivateTab(tab.id));
    }
    let grab_id = response.id.with("grab");
    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(pointer) = response.interact_pointer_pos()
    {
        ui.data_mut(|data| data.insert_temp(grab_id, pointer.x - rect.left()));
    }
    let grab = ui
        .data(|data| data.get_temp::<f32>(grab_id))
        .unwrap_or(width / 2.0);
    let dragged_to = response
        .interact_pointer_pos()
        .filter(|_| response.dragged_by(egui::PointerButton::Primary))
        .map(|pos| pos.x);
    if close.clicked() || response.middle_clicked() || close.middle_clicked() {
        cmds.push(AppCommand::CloseTab(tab.id));
    }
    TabVisual {
        id: tab.id,
        dragged_to,
        grab,
        rect,
        close_rect,
        paint: rect,
        close_paint: close_rect,
        galley,
        active,
        dirty: tab.doc.dirty,
        hovered: response.hovered() || close.hovered(),
        close_hovered: close.hovered(),
        close_pressed: close.is_pointer_button_down_on(),
    }
}

/// Where each tab is painted: a dragged tab under the pointer (where it
/// was taken hold of), the others sliding to their places.  Places are
/// measured from the first one, so scrolling the row does not slide them.
fn place_tabs(ui: &egui::Ui, tabs: &mut [TabVisual]) {
    let (Some(first), Some(last)) = (tabs.first(), tabs.last()) else {
        return;
    };
    let (origin, end, width) = (first.rect.left(), last.rect.left(), first.rect.width());
    // A window resize changes every place at once: no sliding then.
    let width_id = egui::Id::new("tab_slide_width");
    let resized = ui
        .data(|data| data.get_temp::<f32>(width_id))
        .is_some_and(|before| (before - width).abs() > 0.5);
    ui.data_mut(|data| data.insert_temp(width_id, width));
    let slide = if ui_style::motion() && !resized {
        TAB_SLIDE_TIME
    } else {
        0.0
    };
    for tab in tabs.iter_mut() {
        let (target, time) = tab.dragged_to.map_or_else(
            || (tab.rect.left(), slide),
            |x| ((x - tab.grab).clamp(origin, end.max(origin)), 0.0),
        );
        let id = egui::Id::new(("tab_x", tab.id));
        let x = ui.ctx().animate_value_with_time(id, target - origin, time);
        let shift = vec2((origin + x).round() - tab.rect.left(), 0.0);
        tab.paint = tab.rect.translate(shift);
        tab.close_paint = tab.close_rect.translate(shift);
    }
}

/// Drag to reorder: once the middle of a dragged tab is over another one,
/// it takes that one's place.
fn queue_tab_move(tabs: &[TabVisual], cmds: &mut Vec<AppCommand>) {
    let Some((from, x)) = tabs.iter().enumerate().find_map(|(index, tab)| {
        tab.dragged_to
            .map(|x| (index, x - tab.grab + tab.rect.width() / 2.0))
    }) else {
        return;
    };
    let target = tabs
        .iter()
        .position(|tab| tab.rect.x_range().contains(x))
        .or_else(|| {
            // Past either end of the row.
            let first = tabs.first()?.rect.left();
            Some(if x < first { 0 } else { tabs.len() - 1 })
        });
    if let Some(to) = target
        && to != from
    {
        cmds.push(AppCommand::MoveTab {
            id: tabs[from].id,
            to,
        });
    }
}

/// Paint the inactive tabs and their dividers, then the active tab.
fn paint_tabs(ui: &egui::Ui, tabs: &[TabVisual]) {
    let divider = Stroke::new(1.0, ui_style::chrome_divider(ui.visuals()));
    for (index, tab) in tabs.iter().enumerate() {
        if !tab.active {
            paint_tab(ui, tab);
        }
        // Dividers only between two plain inactive tabs.
        let Some(next) = tabs.get(index + 1) else {
            continue;
        };
        if !(tab.active || next.active || tab.hovered || next.hovered) {
            let x = tab.paint.right() + TAB_GAP / 2.0;
            let (y, half) = (tab.paint.center().y, TAB_HEIGHT / 4.0);
            ui.painter().vline(x, (y - half)..=(y + half), divider);
        }
    }
    if let Some(active) = tabs.iter().find(|tab| tab.active) {
        paint_tab(ui, active);
    }
}

/// Outline of the active tab, inside its edge: up from the end of the left
/// foot, over the rounded top and down to the end of the right foot.
/// `inset` is half a pixel, so the 1 px line covers whole pixels.
fn active_tab_outline(rect: Rect, inset: f32) -> Vec<Pos2> {
    use std::f32::consts::{FRAC_PI_2, PI};
    const STEPS: usize = 8;
    let corner = f32::from(TAB_CORNER);
    let mut points = Vec::with_capacity(4 * (STEPS + 1));
    let mut arc = |center: Pos2, radius: f32, from: f32, to: f32| {
        for step in 0..=STEPS {
            let angle = (to - from).mul_add(step as f32 / STEPS as f32, from);
            points.push(center + radius * vec2(angle.cos(), angle.sin()));
        }
    };
    let foot_y = rect.bottom() - FOOT;
    // Concave feet are arcs around points outside the tab; the convex top
    // corners are arcs around points inside it.
    arc(
        pos2(rect.left() - FOOT, foot_y),
        FOOT + inset,
        FRAC_PI_2,
        0.0,
    );
    arc(
        pos2(rect.left() + corner, rect.top() + corner),
        corner - inset,
        PI,
        1.5 * PI,
    );
    arc(
        pos2(rect.right() - corner, rect.top() + corner),
        corner - inset,
        1.5 * PI,
        2.0 * PI,
    );
    arc(
        pos2(rect.right() + FOOT, foot_y),
        FOOT + inset,
        PI,
        FRAC_PI_2,
    );
    points
}

fn paint_tab(ui: &egui::Ui, tab: &TabVisual) {
    let rect = tab.paint;
    if !ui.is_rect_visible(rect.expand2(vec2(FOOT, 0.0))) {
        return;
    }
    let visuals = ui.visuals();
    let painter = ui.painter();
    if tab.active {
        let fill = ui_style::toolbar_background(visuals);
        let corner = CornerRadius {
            nw: TAB_CORNER,
            ne: TAB_CORNER,
            sw: 0,
            se: 0,
        };
        painter.rect_filled(rect, corner, fill);
        // Concave feet that join the tab to the menu row below.
        for (inner, outer) in [
            (rect.left(), rect.left() - FOOT),
            (rect.right(), rect.right() + FOOT),
        ] {
            let foot = Rect::from_min_max(
                pos2(inner.min(outer), rect.bottom() - FOOT),
                pos2(inner.max(outer), rect.bottom()),
            );
            let foot_painter = painter.with_clip_rect(foot.intersect(painter.clip_rect()));
            // The square minus the circle of radius FOOT around its outer
            // top corner: a ring from FOOT to 2 FOOT around that corner,
            // clipped to the square.  Nothing is painted where the title
            // bar shows, so the feet need not know its colour.
            foot_painter.circle_stroke(
                pos2(outer, rect.bottom() - FOOT),
                FOOT * 1.5,
                Stroke::new(FOOT, fill),
            );
        }
        let outline = active_tab_outline(rect, 0.5 / painter.pixels_per_point());
        let stroke = Stroke::new(1.0, ui_style::tab_outline(visuals));
        painter.add(egui::Shape::line(outline, stroke));
    } else if tab.hovered {
        let hover = Rect::from_min_max(rect.min + vec2(0.0, 2.0), rect.max - vec2(0.0, 4.0));
        painter.rect_filled(hover, 6.0, ui_style::subtle_fill(visuals, false));
    }

    let text_color = if tab.active || tab.hovered {
        visuals.text_color()
    } else {
        visuals.weak_text_color()
    };
    let text_pos = pos2(
        rect.left() + TITLE_INSET,
        rect.center().y - tab.galley.size().y / 2.0 + TITLE_DROP,
    );
    painter.galley(text_pos, tab.galley.clone(), text_color);

    let center = tab.close_paint.center();
    match close_slot(tab.active, tab.dirty, tab.hovered) {
        CloseSlot::Cross => {
            if tab.close_hovered {
                let fill = ui_style::subtle_fill(visuals, tab.close_pressed);
                painter.rect_filled(tab.close_paint, 4.0, fill);
            }
            let size = icons::TAB_BUTTON_SIZE;
            if !icons::paint(painter, Icon::Cancel, center, size, text_color) {
                title_bar::paint_cross(painter, center, 4.0, text_color);
            }
        }
        CloseSlot::Dot => {
            painter.circle_filled(center, 3.0, text_color);
        }
        CloseSlot::Empty => {}
    }
}

fn new_tab_button(ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>) {
    // Senses drags so a drag starting here does not move the window.
    let (rect, response) =
        ui.allocate_exact_size(vec2(NEW_TAB_SIZE.x, TAB_HEIGHT), Sense::click_and_drag());
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let face = Rect::from_center_size(rect.center() + vec2(0.0, NEW_TAB_DROP), NEW_TAB_SIZE);
        if response.hovered() {
            let pressed = response.is_pointer_button_down_on();
            ui.painter()
                .rect_filled(face, 4.0, ui_style::subtle_fill(visuals, pressed));
        }
        let (painter, color) = (ui.painter(), visuals.text_color());
        if !icons::paint(
            painter,
            Icon::Add,
            face.center(),
            icons::TAB_BUTTON_SIZE,
            color,
        ) {
            title_bar::paint_plus(painter, face.center(), 5.0, color);
        }
    }
    if response.on_hover_text("New tab (Ctrl+T)").clicked() {
        cmds.push(AppCommand::NewTab);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Width of a tab row holding `count` tabs of `width` and the `+`.
    fn row_width(count: usize, width: f32) -> f32 {
        let count = count as f32;
        2.0f32.mul_add(TAB_ROW_PADDING, NEW_TAB_SIZE.x)
            + count.mul_add(width, TAB_GAP * (count - 1.0))
    }

    #[test]
    fn tabs_keep_the_default_width_then_shrink_evenly_to_the_minimum() {
        // Room to spare, or exactly enough: Notepad's default width.
        assert!((tab_width(10_000.0, 1) - TAB_DEFAULT_WIDTH).abs() < f32::EPSILON);
        for count in 1..=5 {
            let exact = row_width(count, TAB_DEFAULT_WIDTH);
            assert!((tab_width(exact, count) - TAB_DEFAULT_WIDTH).abs() < f32::EPSILON);
            // One point less: every tab shrinks, by the same amount.
            let width = tab_width(exact - 1.0, count);
            assert!(width < TAB_DEFAULT_WIDTH, "{count}: {width}");
            assert!(row_width(count, width) <= exact - 1.0, "{count}: {width}");
            assert!((width - width.floor()).abs() < f32::EPSILON, "whole points");
        }
        // An even share of an 800-point row, all tabs still visible.
        let width = tab_width(800.0, 6);
        assert!((width - 124.0).abs() < f32::EPSILON, "{width}");
        assert!(row_width(6, width) <= 800.0);
        // Never below the minimum (the row scrolls instead).
        assert!((tab_width(800.0, 40) - TAB_MIN_WIDTH).abs() < f32::EPSILON);
        assert!((tab_width(0.0, 0) - TAB_MIN_WIDTH).abs() < f32::EPSILON);
    }

    #[test]
    fn long_titles_are_truncated_with_ellipsis() {
        let ctx = egui::Context::default();
        assert!(ui_style::configure_fonts(&ctx).is_ok());
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for width in [TAB_DEFAULT_WIDTH, TAB_MIN_WIDTH] {
                    let max = max_title_width(width);
                    // Title, gap, close button and insets fill the tab.
                    let used = TITLE_INSET + max + TITLE_CLOSE_GAP + CLOSE_SIZE.x + CLOSE_INSET;
                    assert!((used - width).abs() < f32::EPSILON);
                    let text = egui::RichText::new("a-very-long-file-name-".repeat(10))
                        .text_style(egui::TextStyle::Small);
                    let galley = egui::WidgetText::from(text).into_galley(
                        ui,
                        Some(TextWrapMode::Truncate),
                        max,
                        egui::TextStyle::Button,
                    );
                    assert!(galley.size().x <= max + 0.5);
                    assert!(galley.elided, "long titles end in an ellipsis");
                    assert_eq!(galley.rows.len(), 1);
                }
            });
        });
    }

    /// Left edges of the painted tab titles `doc-0.md`, `doc-1.md`, …
    fn title_lefts(count: usize) -> Vec<f32> {
        let mut ui = crate::test_harness::Harness::new();
        let mut app = RustdownApp::default();
        app.tab_mut().doc.path = Some("doc-0.md".into());
        for index in 1..count {
            app.new_tab();
            app.tab_mut().doc.path = Some(format!("doc-{index}.md").into());
        }
        let output = ui.settle(&mut app);
        (0..count)
            .filter_map(|index| {
                crate::test_harness::find_text(&output, &format!("doc-{index}.md"))
                    .map(|rect| rect.left())
            })
            .collect()
    }

    #[test]
    fn title_bar_tabs_are_notepad_wide_and_share_the_row_when_crowded() {
        use crate::title_bar::ICON_SLOT_WIDTH;
        // One tab: Notepad's position and default width.
        let lefts = title_lefts(1);
        let first = ICON_SLOT_WIDTH + TAB_ROW_PADDING + TITLE_INSET;
        assert!(
            lefts.len() == 1 && (lefts[0] - first).abs() < 2.0,
            "{lefts:?}"
        );
        // Three fit at the default width; five share the 798-point row
        // (1024 less the caption buttons, drag space and icon) evenly.
        for (count, pitch) in [(3, TAB_DEFAULT_WIDTH + TAB_GAP), (5, 148.0 + TAB_GAP)] {
            let lefts = title_lefts(count);
            assert_eq!(lefts.len(), count, "all titles visible");
            for pair in lefts.windows(2) {
                assert!(
                    (pair[1] - pair[0] - pitch).abs() < 0.01,
                    "{count}: {lefts:?}"
                );
            }
        }
    }

    #[test]
    fn tab_label_is_the_title_and_dirty_shows_in_the_close_slot() {
        let mut tab = Tab::default();
        assert_eq!(tab_label(&tab), "Untitled");
        tab.doc.dirty = true;
        assert_eq!(tab_label(&tab), "Untitled", "no marker in the title");

        // (active, dirty, hovered)
        assert_eq!(close_slot(true, false, false), CloseSlot::Cross);
        assert_eq!(close_slot(false, false, false), CloseSlot::Empty);
        assert_eq!(close_slot(false, false, true), CloseSlot::Cross);
        assert_eq!(close_slot(true, true, false), CloseSlot::Dot);
        assert_eq!(close_slot(false, true, false), CloseSlot::Dot);
        assert_eq!(close_slot(false, true, true), CloseSlot::Cross);
        assert_eq!(close_slot(true, true, true), CloseSlot::Cross);
    }
}
