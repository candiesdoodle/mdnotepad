//! Explorer: panel drawing — header, tree rows, context menus and the
//! small painted icons (painted rather than font glyphs so they look the
//! same with any UI font).

use std::path::Path;

use eframe::egui::{
    self, Align, Color32, FontId, Layout, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, Vec2,
    text::{LayoutJob, TextFormat, TextWrapping},
    vec2,
};

use crate::{
    explorer::{ExplorerCommand, ExplorerState},
    explorer_fs::{display_path, folder_display_name},
    explorer_tree::{ExplorerTree, Row, RowKind, scroll_offset_to_show},
    menu::{self, Mark},
};

/// Indent per tree level.
const INDENT: f32 = 14.0;
/// Inset of row highlights from the panel edges.
const ROW_INSET: f32 = 6.0;
/// Space before the chevron / icon column.
const ROW_LEAD: f32 = 4.0;
const CHEVRON_WIDTH: f32 = 16.0;
const ICON_WIDTH: f32 = 22.0;
const HEADER_BUTTON: f32 = 24.0;
const CORNER: f32 = 4.0;
/// Folder icon colour: a muted amber that reads on white and dark panels.
const FOLDER_COLOR: Color32 = Color32::from_rgb(0xE0, 0xAA, 0x3E);
/// Title of the panel (the folder's name and path show on hover).
pub(crate) const PANEL_TITLE: &str = "Folder tree";

pub(crate) fn panel_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::side_top_panel(style).inner_margin(egui::Margin::symmetric(0, 4))
}

/// Tree font: the Body text style, the size of the menus and of the
/// document's body text at 100 % zoom.
fn tree_font(ui: &egui::Ui) -> FontId {
    egui::TextStyle::Body.resolve(ui.style())
}

fn row_height(font: &FontId) -> f32 {
    (font.size + 10.0).round()
}

/// Everything inside the side panel.  Clicks become [`ExplorerCommand`]s
/// in `out`, run by the caller after drawing.
pub(crate) fn panel_contents(
    ui: &mut egui::Ui,
    state: &mut ExplorerState,
    out: &mut Vec<ExplorerCommand>,
) {
    let font = tree_font(ui);
    let show_all = state.show_all_files;
    let Some(tree) = state.tree.as_mut() else {
        no_folder(ui, &font, out);
        return;
    };
    header(ui, tree, show_all, &font, out);
    if tree.root_missing() {
        root_missing(ui, tree, &font, out);
        return;
    }
    if let Some(err) = tree.watch_error() {
        ui.horizontal(|ui| {
            ui.add_space(ROW_INSET + ROW_LEAD);
            ui.label(
                RichText::new("Live refresh is off — use Refresh")
                    .text_style(egui::TextStyle::Small)
                    .color(ui.visuals().weak_text_color()),
            )
            .on_hover_text(err.to_owned());
        });
    }
    tree_rows(ui, tree, show_all, &font, out);
}

fn no_folder(ui: &mut egui::Ui, font: &FontId, out: &mut Vec<ExplorerCommand>) {
    ui.add_space(16.0);
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new("No folder open")
                .font(font.clone())
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(8.0);
        if ui
            .button(RichText::new("Open folder…").font(font.clone()))
            .clicked()
        {
            out.push(ExplorerCommand::OpenFolderDialog);
        }
    });
}

fn header(
    ui: &mut egui::Ui,
    tree: &ExplorerTree,
    show_all: bool,
    font: &FontId,
    out: &mut Vec<ExplorerCommand>,
) {
    let root = tree.root();
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        vec2(width, HEADER_BUTTON + 4.0),
        Layout::right_to_left(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.add_space(ROW_INSET);
            if icon_button(ui, Icon::Close, "Close folder").clicked() {
                out.push(ExplorerCommand::CloseFolder);
            }
            if icon_button(ui, Icon::CollapseAll, "Collapse all").clicked() {
                out.push(ExplorerCommand::CollapseAll);
            }
            if icon_button(ui, Icon::Refresh, "Refresh").clicked() {
                out.push(ExplorerCommand::Refresh);
            }
            if icon_button(ui, Icon::OpenFolder, "Open folder…").clicked() {
                out.push(ExplorerCommand::OpenFolderDialog);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add_space(ROW_INSET + ROW_LEAD);
                // The folder itself: its path on hover, its menu on a
                // right click.
                let name = egui::Label::new(RichText::new(PANEL_TITLE).font(font.clone()).strong())
                    .truncate()
                    .sense(Sense::click());
                let response = ui.add(name).on_hover_text(display_path(root));
                menu::context_menu(&response, |ui| {
                    folder_menu(ui, root, None, true, show_all, out);
                });
            });
        },
    );
    let y = ui.cursor().top();
    let x = ui.max_rect().x_range();
    ui.painter()
        .hline(x, y, ui.visuals().widgets.noninteractive.bg_stroke);
    ui.add_space(4.0);
}

fn root_missing(
    ui: &mut egui::Ui,
    tree: &ExplorerTree,
    font: &FontId,
    out: &mut Vec<ExplorerCommand>,
) {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!(
                    "“{}” is no longer available. It may have been moved, renamed or deleted.",
                    folder_display_name(tree.root())
                ))
                .font(font.clone())
                .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Close folder").clicked() {
                    out.push(ExplorerCommand::CloseFolder);
                }
                if ui.button("Try again").clicked() {
                    out.push(ExplorerCommand::Refresh);
                }
            });
        });
}

fn tree_rows(
    ui: &mut egui::Ui,
    tree: &mut ExplorerTree,
    show_all: bool,
    font: &FontId,
    out: &mut Vec<ExplorerCommand>,
) {
    tree.prepare_rows(show_all);
    let row_h = row_height(font);
    let mut area = egui::ScrollArea::vertical()
        .id_salt("explorer_tree")
        .auto_shrink([false, false]);
    if let Some(offset) = tree
        .take_scroll_index()
        .and_then(|index| scroll_offset_to_show(index, row_h, tree.viewport))
    {
        area = area.vertical_scroll_offset(offset);
    }
    // Rows are laid out back to back so `row index * row_h` is exact.
    ui.spacing_mut().item_spacing.y = 0.0;
    let rows = tree.rows();
    let selected = tree.selected();
    let output = area.show_rows(ui, row_h, rows.len(), |ui, range| {
        for row in rows.get(range).unwrap_or_default() {
            let is_selected = selected == Some(row.path.as_path()) && row.is_entry();
            tree_row(ui, row, is_selected, show_all, font, row_h, out);
        }
    });
    tree.viewport = (output.state.offset.y, output.inner_rect.height());
}

fn tree_row(
    ui: &mut egui::Ui,
    row: &Row,
    selected: bool,
    show_all: bool,
    font: &FontId,
    row_h: f32,
    out: &mut Vec<ExplorerCommand>,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
    // Ids come from the path, so an open context menu stays on its row
    // when rows above it appear or disappear.
    let id = egui::Id::new(("explorer_row", row.is_entry(), &row.path));
    let response = ui.interact(rect, id, Sense::click());

    let visuals = ui.visuals();
    let painter = ui.painter();
    let highlight = rect.shrink2(vec2(ROW_INSET, 1.0));
    if selected {
        painter.rect_filled(
            highlight,
            CORNER,
            visuals.selection.bg_fill.gamma_multiply(0.35),
        );
        let accent = Rect::from_center_size(
            Pos2::new(highlight.left() + 1.5, highlight.center().y),
            vec2(3.0, (row_h * 0.55).round()),
        );
        painter.rect_filled(accent, 1.5, crate::ui_style::accent_fill(visuals));
    } else if row.is_entry() && (response.hovered() || response.context_menu_opened()) {
        painter.rect_filled(highlight, CORNER, visuals.widgets.hovered.weak_bg_fill);
    }

    let weak = visuals.weak_text_color();
    let center_y = rect.center().y;
    let mut x = (row.depth as f32).mul_add(INDENT, rect.left() + ROW_INSET + ROW_LEAD);
    if let RowKind::Folder { expanded } = row.kind {
        paint_chevron(
            painter,
            Pos2::new(x + CHEVRON_WIDTH / 2.0, center_y),
            expanded,
            weak,
        );
    }
    x += CHEVRON_WIDTH;
    match row.kind {
        RowKind::Folder { .. } => {
            paint_folder(painter, Pos2::new(x + ICON_WIDTH / 2.0 - 3.0, center_y));
            x += ICON_WIDTH;
        }
        RowKind::File { markdown } => {
            paint_file(
                painter,
                Pos2::new(x + ICON_WIDTH / 2.0 - 3.0, center_y),
                markdown,
                weak,
            );
            x += ICON_WIDTH;
        }
        _ => {}
    }

    let (text, color, italics) = match &row.kind {
        RowKind::Folder { .. } | RowKind::File { .. } => {
            let color = if selected {
                visuals.strong_text_color()
            } else {
                visuals.text_color()
            };
            (row.name.as_str(), color, false)
        }
        RowKind::Loading => ("Loading…", weak, true),
        RowKind::Empty if show_all => ("Empty folder", weak, true),
        RowKind::Empty => ("No Markdown files", weak, true),
        RowKind::Error(message) => (message.as_str(), visuals.error_fg_color, false),
    };
    let mut job = LayoutJob::default();
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: font.clone(),
            color,
            italics,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping::truncate_at_width((highlight.right() - 4.0 - x).max(0.0));
    let galley = ui.painter().layout_job(job);
    let elided = galley.elided;
    painter.galley(
        Pos2::new(x, center_y - galley.size().y / 2.0),
        galley,
        color,
    );

    match &row.kind {
        RowKind::Folder { expanded } => {
            if response.clicked() {
                out.push(ExplorerCommand::ToggleExpanded(row.path.clone()));
            }
            menu::context_menu(&response, |ui| {
                folder_menu(ui, &row.path, Some(*expanded), false, show_all, out);
            });
        }
        RowKind::File { .. } => {
            if response.clicked() {
                out.push(ExplorerCommand::OpenFile(row.path.clone()));
            }
            menu::context_menu(&response, |ui| file_menu(ui, &row.path, out));
        }
        _ => {
            menu::context_menu(&response, |ui| {
                menu::hide_lead_column(ui);
                if item(ui, "Refresh") {
                    out.push(ExplorerCommand::RefreshFolder(row.path.clone()));
                }
            });
        }
    }
    if elided || matches!(row.kind, RowKind::Error(_)) {
        let _ = response.on_hover_text(ExplorerTree::row_tooltip(row));
    }
}

/// An item of the context menus.
fn item(ui: &mut egui::Ui, label: &str) -> bool {
    menu::plain_row(ui, label, Mark::None)
}

fn file_menu(ui: &mut egui::Ui, path: &Path, out: &mut Vec<ExplorerCommand>) {
    menu::hide_lead_column(ui);
    if item(ui, "Open") {
        out.push(ExplorerCommand::OpenFile(path.to_path_buf()));
    }
    menu::separator(ui);
    path_items(ui, path, out);
    menu::separator(ui);
    if item(ui, "Refresh")
        && let Some(parent) = path.parent()
    {
        out.push(ExplorerCommand::RefreshFolder(parent.to_path_buf()));
    }
}

/// Context menu for a folder row (`expanded` is `Some`) or the root header.
fn folder_menu(
    ui: &mut egui::Ui,
    dir: &Path,
    expanded: Option<bool>,
    is_root: bool,
    show_all: bool,
    out: &mut Vec<ExplorerCommand>,
) {
    // Only the root's menu has a check mark (Show all files).
    if !is_root {
        menu::hide_lead_column(ui);
    }
    if let Some(expanded) = expanded {
        let label = if expanded { "Collapse" } else { "Expand" };
        if item(ui, label) {
            out.push(ExplorerCommand::ToggleExpanded(dir.to_path_buf()));
        }
    }
    if item(ui, "New Markdown file") {
        out.push(ExplorerCommand::NewMarkdownFile(dir.to_path_buf()));
    }
    menu::separator(ui);
    path_items(ui, dir, out);
    menu::separator(ui);
    if item(ui, "Refresh") {
        out.push(if is_root {
            ExplorerCommand::Refresh
        } else {
            ExplorerCommand::RefreshFolder(dir.to_path_buf())
        });
    }
    if is_root {
        if item(ui, "Collapse all") {
            out.push(ExplorerCommand::CollapseAll);
        }
        if menu::plain_row(ui, "Show all files", Mark::Check(show_all)) {
            out.push(ExplorerCommand::ToggleShowAllFiles);
        }
        menu::separator(ui);
        if item(ui, "Close folder") {
            out.push(ExplorerCommand::CloseFolder);
        }
    }
}

fn path_items(ui: &mut egui::Ui, path: &Path, out: &mut Vec<ExplorerCommand>) {
    if item(ui, "Copy path") {
        out.push(ExplorerCommand::CopyPath(path.to_path_buf()));
    }
    if item(ui, "Reveal in File Explorer") {
        out.push(ExplorerCommand::RevealInFileExplorer(path.to_path_buf()));
    }
}

// ── Painted icons ───────────────────────────────────────────────────────

/// The panel header icons (the folder tree's and the outline's).
#[derive(Clone, Copy)]
pub(crate) enum Icon {
    OpenFolder,
    Refresh,
    CollapseAll,
    ExpandAll,
    /// Back to the top of the document.
    Top,
    Close,
}

/// A small frameless icon button (hover background only), Win11 style.
pub(crate) fn icon_button(ui: &mut egui::Ui, icon: Icon, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(HEADER_BUTTON), Sense::click());
    let visuals = ui.visuals();
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, CORNER, visuals.widgets.hovered.weak_bg_fill);
    }
    let color = if response.hovered() {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    let stroke = Stroke::new(1.3, color);
    let c = rect.center();
    match icon {
        Icon::Close => {
            let d = 4.0;
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], stroke);
            painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], stroke);
        }
        Icon::CollapseAll | Icon::ExpandAll => {
            let square = Rect::from_center_size(c, Vec2::splat(10.0));
            painter.rect_stroke(square, 1.5, stroke, StrokeKind::Middle);
            painter.line_segment([c + vec2(-2.5, 0.0), c + vec2(2.5, 0.0)], stroke);
            if matches!(icon, Icon::ExpandAll) {
                painter.line_segment([c + vec2(0.0, -2.5), c + vec2(0.0, 2.5)], stroke);
            }
        }
        Icon::Top => {
            // An arrow up to a bar.
            painter.line_segment([c + vec2(-4.5, -5.0), c + vec2(4.5, -5.0)], stroke);
            painter.line_segment([c + vec2(0.0, 5.0), c + vec2(0.0, -2.5)], stroke);
            painter.line_segment([c + vec2(-3.5, 1.0), c + vec2(0.0, -2.5)], stroke);
            painter.line_segment([c + vec2(3.5, 1.0), c + vec2(0.0, -2.5)], stroke);
        }
        Icon::Refresh => paint_refresh(painter, c, 5.0, stroke),
        Icon::OpenFolder => paint_open_folder(painter, c, stroke),
    }
    response.on_hover_text(tooltip)
}

/// An open folder: the back with its tab, and the front flap tilted open.
fn paint_open_folder(painter: &Painter, c: Pos2, stroke: Stroke) {
    let p = |x: f32, y: f32| c + vec2(x, y);
    let back = vec![
        p(-6.0, 4.5),
        p(-6.0, -4.5),
        p(-2.0, -4.5),
        p(-0.5, -3.0),
        p(4.5, -3.0),
        p(4.5, -0.5),
    ];
    painter.add(Shape::line(back, stroke));
    let front = vec![p(-6.0, 4.5), p(-3.5, -0.5), p(7.0, -0.5), p(4.5, 4.5)];
    painter.add(Shape::closed_line(front, stroke));
}

/// A clockwise circular arrow.
fn paint_refresh(painter: &Painter, center: Pos2, radius: f32, stroke: Stroke) {
    use std::f32::consts::{FRAC_PI_2, TAU};
    const SEGMENTS: u8 = 24;
    // Screen y points down, so increasing angles run clockwise.
    let start = -FRAC_PI_2 + 0.6;
    let sweep = TAU - 1.1;
    let points: Vec<Pos2> = (0..=SEGMENTS)
        .map(|i| center + radius * Vec2::angled(start + sweep * f32::from(i) / f32::from(SEGMENTS)))
        .collect();
    painter.add(Shape::line(points, stroke));
    let end_angle = start + sweep;
    let end = center + radius * Vec2::angled(end_angle);
    let along = Vec2::angled(end_angle + FRAC_PI_2);
    let across = Vec2::angled(end_angle);
    let head = 2.6;
    painter.add(Shape::convex_polygon(
        vec![end + along * head, end + across * head, end - across * head],
        stroke.color,
        Stroke::NONE,
    ));
}

fn paint_chevron(painter: &Painter, c: Pos2, expanded: bool, color: Color32) {
    let s = 3.5;
    let points = if expanded {
        vec![
            c + vec2(-s, -s / 2.0),
            c + vec2(0.0, s / 2.0),
            c + vec2(s, -s / 2.0),
        ]
    } else {
        vec![
            c + vec2(-s / 2.0, -s),
            c + vec2(s / 2.0, 0.0),
            c + vec2(-s / 2.0, s),
        ]
    };
    painter.add(Shape::line(points, Stroke::new(1.3, color)));
}

fn paint_folder(painter: &Painter, c: Pos2) {
    let body = Rect::from_center_size(c + vec2(0.0, 1.0), vec2(15.0, 10.5));
    let tab = Rect::from_min_size(body.left_top() + vec2(0.0, -2.5), vec2(6.5, 4.0));
    painter.rect_filled(tab, 1.0, FOLDER_COLOR);
    painter.rect_filled(body, 1.5, FOLDER_COLOR);
}

/// A page outline; Markdown files get text lines.
fn paint_file(painter: &Painter, c: Pos2, markdown: bool, color: Color32) {
    let page = Rect::from_center_size(c, vec2(11.0, 14.0));
    let stroke = Stroke::new(1.1, color);
    painter.rect_stroke(page, 1.5, stroke, StrokeKind::Inside);
    if markdown {
        for dy in [-3.0, 0.0, 3.0] {
            painter.line_segment(
                [
                    Pos2::new(page.left() + 3.0, c.y + dy),
                    Pos2::new(page.right() - 3.0, c.y + dy),
                ],
                Stroke::new(1.0, color),
            );
        }
    }
}
