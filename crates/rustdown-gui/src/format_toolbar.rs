//! Markdown formatting: the [`FormatCommand`] group, its key bindings, the
//! formatting controls in the menu row (heading and list dropdowns, inline
//! buttons, insert dropdown — grouped like Notepad's), and
//! [`RustdownApp::execute_format`], which applies an [`md_commands`]
//! transform to the active editor.

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, FontId, Key, KeyboardShortcut, Modifiers, Painter, Rect, Stroke,
    StrokeKind,
    emath::GuiRounding as _,
    pos2,
    text::{CCursor, CCursorRange, LayoutJob, TextFormat},
    vec2,
};

use crate::{
    RustdownApp,
    commands::AppCommand,
    icons::{self, Icon},
    md_commands::{self, MdCommand},
    menu::{self, Mark, ROW_CONTROL_HEIGHT, Trailing, lead_icon_rect, menu_row},
    ui_style,
};

/// Markdown formatting commands (text transforms in [`md_commands`]).
pub(crate) type FormatCommand = MdCommand;

/// Key bindings for [`FormatCommand`]; merged into `commands::shortcut_table`.
///
/// Ctrl+Shift+X/C/V are unusable: egui-winit turns any Ctrl+X/C/V into
/// cut/copy/paste events before shortcuts see them.
pub(crate) fn format_shortcuts() -> Vec<(KeyboardShortcut, AppCommand)> {
    const CMD: Modifiers = Modifiers::COMMAND;
    const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
    let key = KeyboardShortcut::new;
    let format = AppCommand::Format;
    let mut table = vec![
        (key(CMD, Key::B), format(MdCommand::Bold)),
        (key(CMD, Key::I), format(MdCommand::Italic)),
        (key(CMD, Key::D), format(MdCommand::Strikethrough)),
        (key(CMD, Key::E), format(MdCommand::InlineCode)),
        (key(CMD, Key::K), format(MdCommand::Link)),
        (key(CMD_SHIFT, Key::P), format(MdCommand::Paragraph)),
        (key(CMD_SHIFT, Key::U), format(MdCommand::BulletList)),
        (key(CMD_SHIFT, Key::O), format(MdCommand::NumberedList)),
        (key(CMD_SHIFT, Key::L), format(MdCommand::TaskList)),
        (key(CMD_SHIFT, Key::Q), format(MdCommand::Blockquote)),
        (key(CMD_SHIFT, Key::K), format(MdCommand::CodeBlock)),
        (key(CMD_SHIFT, Key::H), format(MdCommand::HorizontalRule)),
        (key(CMD_SHIFT, Key::G), format(MdCommand::Table)),
    ];
    let digits = [
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
    ];
    table.extend(
        (1..=6)
            .zip(digits)
            .map(|(level, digit)| (key(CMD, digit), format(MdCommand::Heading(level)))),
    );
    table
}

/// Menu / tooltip name of a formatting command.
pub(crate) const fn command_label(cmd: MdCommand) -> &'static str {
    match cmd {
        MdCommand::Bold => "Bold",
        MdCommand::Italic => "Italic",
        MdCommand::Strikethrough => "Strikethrough",
        MdCommand::InlineCode => "Inline code",
        MdCommand::Link => "Link",
        MdCommand::Heading(1) => "Heading 1",
        MdCommand::Heading(2) => "Heading 2",
        MdCommand::Heading(3) => "Heading 3",
        MdCommand::Heading(4) => "Heading 4",
        MdCommand::Heading(5) => "Heading 5",
        MdCommand::Heading(_) => "Heading 6",
        MdCommand::Paragraph => "Paragraph",
        MdCommand::BulletList => "Bulleted list",
        MdCommand::NumberedList => "Numbered list",
        MdCommand::TaskList => "Task list",
        MdCommand::Blockquote => "Quote",
        MdCommand::CodeBlock => "Code block",
        MdCommand::HorizontalRule => "Horizontal rule",
        MdCommand::Table => "Table",
    }
}

/// List dropdown entries.
pub(crate) const LIST_COMMANDS: [MdCommand; 3] = [
    MdCommand::BulletList,
    MdCommand::NumberedList,
    MdCommand::TaskList,
];

/// The buttons between the dropdowns.
pub(crate) const INLINE_COMMANDS: [MdCommand; 5] = [
    MdCommand::Bold,
    MdCommand::Italic,
    MdCommand::Strikethrough,
    MdCommand::InlineCode,
    MdCommand::Link,
];

/// Insert dropdown entries.
pub(crate) const INSERT_COMMANDS: [MdCommand; 4] = [
    MdCommand::Table,
    MdCommand::CodeBlock,
    MdCommand::Blockquote,
    MdCommand::HorizontalRule,
];

/// Heading dropdown entries, Paragraph first.
pub(crate) const HEADING_COMMANDS: [MdCommand; 7] = [
    MdCommand::Paragraph,
    MdCommand::Heading(1),
    MdCommand::Heading(2),
    MdCommand::Heading(3),
    MdCommand::Heading(4),
    MdCommand::Heading(5),
    MdCommand::Heading(6),
];

/// Ids of the dropdown buttons.
pub(crate) const HEADING_DROPDOWN_ID: &str = "format_heading";
pub(crate) const LIST_DROPDOWN_ID: &str = "format_lists";
pub(crate) const INSERT_DROPDOWN_ID: &str = "format_insert";

const BUTTON_SIZE: egui::Vec2 = vec2(30.0, ROW_CONTROL_HEIGHT);
/// Width of the heading dropdown: Notepad's.
const HEADING_DROPDOWN_WIDTH: f32 = 108.0;
/// Icon dropdowns as in Notepad: padding, a 16 icon, a gap, the chevron
/// (8 wide) and padding.
const ICON_DROPDOWN_WIDTH: f32 = 55.0;
const DROPDOWN_PADDING_X: f32 = 11.0;
/// Chevron centre from a dropdown's right edge.
const CHEVRON_INSET: f32 = DROPDOWN_PADDING_X + 4.0;
/// Space between the controls.
pub(crate) const CONTROL_GAP: f32 = 2.0;
const PREVIEW_HINT: &str = "Switch to Edit, Raw or Split to format text";

/// Width of the formatting controls laid out with [`CONTROL_GAP`] between
/// them (the menu row centres them).
pub(crate) fn controls_width() -> f32 {
    let buttons = INLINE_COMMANDS.len() as f32;
    let gaps = buttons + 2.0;
    let fixed = 2.0f32.mul_add(ICON_DROPDOWN_WIDTH, HEADING_DROPDOWN_WIDTH);
    buttons.mul_add(BUTTON_SIZE.x, gaps.mul_add(CONTROL_GAP, fixed))
}

/// Id of the toolbar button for `cmd`.
pub(crate) fn tool_button_id(cmd: MdCommand) -> egui::Id {
    egui::Id::new(("format_button", command_label(cmd)))
}

impl RustdownApp {
    /// Whether formatting commands can run (they need the editor).
    pub(crate) const fn format_enabled(&self) -> bool {
        self.mode.uses_editor()
    }

    /// Apply a [`FormatCommand`] to the active tab's editor selection.
    ///
    /// The change is pushed onto the editor's undo stack (before and after
    /// states), so Ctrl+Z / Ctrl+Y undo and redo it like typing.  Does
    /// nothing in Preview mode, where there is no editor.
    pub(crate) fn execute_format(&mut self, cmd: FormatCommand, ctx: &egui::Context) {
        if !self.format_enabled() {
            return;
        }
        let tab = self.tab_mut();
        let id = tab.editor_id();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        // Without stored state egui puts the caret at the end of the text.
        let before = state
            .cursor
            .char_range()
            .unwrap_or_else(|| CCursorRange::one(CCursor::new(tab.doc.text.chars().count())));
        let edit = md_commands::apply(
            cmd,
            &tab.doc.text,
            before.as_sorted_char_range(),
            tab.doc.line_ending,
        );
        let after = CCursorRange::two(
            CCursor::new(edit.selection.start),
            CCursor::new(edit.selection.end),
        );
        if !edit.is_noop() {
            let mut undoer = state.undoer();
            undoer.add_undo(&(before, tab.doc.text.as_str().to_owned()));
            edit.apply_to(Arc::make_mut(&mut tab.doc.text));
            undoer.add_undo(&(after, tab.doc.text.as_str().to_owned()));
            state.set_undoer(undoer);
            tab.bump_edit_seq();
            tab.note_text_changed(false);
        }
        state.cursor.set_char_range(Some(after));
        state.store(ctx, id);
        ctx.memory_mut(|mem| mem.request_focus(id));
        // A toolbar or menu click makes the editor give up focus later in the
        // same frame; this re-focuses it after it has been drawn.
        self.focus_editor = true;
    }

    /// Heading level of the active editor selection (`0` = paragraph,
    /// `None` = mixed), for the toolbar's heading dropdown.
    fn selection_heading_level(&self, ctx: &egui::Context) -> Option<u8> {
        let tab = self.tab();
        let range = egui::TextEdit::load_state(ctx, tab.editor_id())
            .and_then(|state| state.cursor.char_range())
            .map_or(usize::MAX..usize::MAX, |range| range.as_sorted_char_range());
        md_commands::heading_level(&tab.doc.text, range)
    }

    /// The formatting controls in the menu row (View → Formatting toolbar):
    /// heading and list dropdowns, the inline buttons, the insert dropdown.
    /// Greyed out in Preview mode.
    pub(crate) fn format_controls(&mut self, ui: &mut egui::Ui) {
        let enabled = self.format_enabled();
        let heading_level = if enabled {
            self.selection_heading_level(ui.ctx())
        } else {
            Some(0)
        };
        let mut clicked = None;
        ui.add_enabled_ui(enabled, |ui| {
            heading_dropdown(ui, heading_level, &mut clicked);
            let bullets = |painter: &Painter, rect: Rect, color: Color32| {
                paint_icon(painter, rect, MdCommand::BulletList, color);
            };
            icon_dropdown(
                ui,
                LIST_DROPDOWN_ID,
                "Lists",
                bullets,
                &LIST_COMMANDS,
                &mut clicked,
            );
            for cmd in INLINE_COMMANDS {
                if tool_button(ui, cmd).clicked() {
                    clicked = Some(cmd);
                }
            }
            let plus = |painter: &Painter, rect: Rect, color: Color32| {
                let size = icons::TOOLBAR_SIZE;
                if !icons::paint(painter, Icon::Add, rect.center(), size, color) {
                    crate::title_bar::paint_plus(painter, rect.center(), 5.0, color);
                }
            };
            icon_dropdown(
                ui,
                INSERT_DROPDOWN_ID,
                "Insert",
                plus,
                &INSERT_COMMANDS,
                &mut clicked,
            );
        });
        if let Some(cmd) = clicked {
            self.execute(AppCommand::Format(cmd), ui.ctx());
        }
    }
}

/// `"Bold (Ctrl+B)"`.
fn label_with_shortcut(ctx: &egui::Context, cmd: MdCommand) -> String {
    let label = command_label(cmd);
    match AppCommand::Format(cmd).shortcut() {
        Some(shortcut) => format!("{label} ({})", ctx.format_shortcut(&shortcut)),
        None => label.to_owned(),
    }
}

/// A flat dropdown button of the menu row; `content` paints its face left
/// of the chevron.
fn dropdown_button(
    ui: &mut egui::Ui,
    id: &str,
    width: f32,
    label: &str,
    content: impl FnOnce(&Painter, Rect, Color32),
) -> egui::Response {
    let (_, rect) = ui.allocate_space(vec2(width, ROW_CONTROL_HEIGHT));
    let response = ui.interact(rect, egui::Id::new(id), egui::Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, enabled, label));
    menu::register_bar_menu(&response, enabled);
    if ui.is_rect_visible(rect) {
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
        let visuals = ui.visuals();
        let painter = ui.painter();
        if enabled && (open || response.hovered()) {
            let pressed = response.is_pointer_button_down_on();
            painter.rect_filled(rect, 4.0, ui_style::subtle_fill(visuals, pressed));
        }
        let color = visuals.text_color();
        let chevron = pos2(rect.right() - CHEVRON_INSET, rect.center().y);
        let face = Rect::from_min_max(rect.min, pos2(chevron.x - 8.0, rect.max.y));
        content(painter, face, color);
        if !icons::paint(
            painter,
            Icon::ChevronDown,
            chevron,
            icons::CHEVRON_SIZE,
            color,
        ) {
            paint_chevron(painter, chevron, color);
        }
    }
    response
}

/// Paragraph / Heading 1–6 dropdown showing the selection's current style.
fn heading_dropdown(ui: &mut egui::Ui, level: Option<u8>, clicked: &mut Option<MdCommand>) {
    let current = |cmd: MdCommand| match cmd {
        MdCommand::Heading(n) => level == Some(n),
        _ => level == Some(0),
    };
    let selected = HEADING_COMMANDS
        .into_iter()
        .find(|&cmd| current(cmd))
        .map_or("Mixed", command_label);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let label = |painter: &Painter, rect: Rect, color: Color32| {
        let pos = pos2(rect.left() + DROPDOWN_PADDING_X, rect.center().y);
        painter.text(pos, Align2::LEFT_CENTER, selected, font, color);
    };
    let response = dropdown_button(
        ui,
        HEADING_DROPDOWN_ID,
        HEADING_DROPDOWN_WIDTH,
        "Heading",
        label,
    )
    .on_hover_text("Paragraph or heading level")
    .on_disabled_hover_text(PREVIEW_HINT);
    menu::popup_menu(&response).show(|ui| {
        menu::slide_in(ui);
        for cmd in HEADING_COMMANDS {
            let shortcut = Trailing::shortcut_of(ui.ctx(), &AppCommand::Format(cmd));
            let mark = Mark::Radio(current(cmd));
            let row = menu_row(ui, command_label(cmd), mark, &shortcut, true);
            // Picking the style the lines already have changes nothing
            // (running it would toggle the heading off).
            if row.clicked() && !current(cmd) {
                *clicked = Some(cmd);
            }
        }
    });
}

/// An icon dropdown (lists, insert) whose items show their icons.
fn icon_dropdown(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    icon: impl FnOnce(&Painter, Rect, Color32),
    items: &[MdCommand],
    clicked: &mut Option<MdCommand>,
) {
    let face = |painter: &Painter, rect: Rect, color: Color32| {
        let icon_rect = Rect::from_center_size(
            pos2(rect.left() + DROPDOWN_PADDING_X + 8.0, rect.center().y),
            vec2(16.0, 16.0),
        );
        icon(painter, icon_rect, color);
    };
    let response = dropdown_button(ui, id, ICON_DROPDOWN_WIDTH, label, face)
        .on_hover_text(label)
        .on_disabled_hover_text(format!("{label}. {PREVIEW_HINT}"));
    menu::popup_menu(&response).show(|ui| {
        menu::slide_in(ui);
        for &cmd in items {
            let shortcut = Trailing::shortcut_of(ui.ctx(), &AppCommand::Format(cmd));
            let row = menu_row(ui, command_label(cmd), Mark::None, &shortcut, true);
            let color = ui.style().interact(&row).text_color();
            paint_icon(ui.painter(), lead_icon_rect(&row), cmd, color);
            if row.clicked() {
                *clicked = Some(cmd);
            }
        }
    });
}

/// A small `v` marking a dropdown.
fn paint_chevron(painter: &Painter, center: egui::Pos2, color: Color32) {
    let points = vec![
        center + vec2(-3.5, -1.75),
        center + vec2(0.0, 1.75),
        center + vec2(3.5, -1.75),
    ];
    painter.add(egui::Shape::line(points, Stroke::new(1.0, color)));
}

/// A small flat toolbar button with a painted icon.
fn tool_button(ui: &mut egui::Ui, cmd: MdCommand) -> egui::Response {
    let enabled = ui.is_enabled();
    let (_, rect) = ui.allocate_space(BUTTON_SIZE);
    let response = ui.interact(rect, tool_button_id(cmd), egui::Sense::click());
    menu::register_bar_button(&response);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, command_label(cmd))
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        if enabled && (response.hovered() || response.has_focus()) {
            let pressed = response.is_pointer_button_down_on();
            ui.painter()
                .rect_filled(rect, 4.0, ui_style::subtle_fill(visuals, pressed));
        }
        paint_icon(ui.painter(), rect, cmd, visuals.text_color());
    }
    response
        .on_hover_ui(|ui| {
            ui.label(label_with_shortcut(ui.ctx(), cmd));
        })
        .on_disabled_hover_ui(|ui| {
            ui.label(format!("{}. {PREVIEW_HINT}", command_label(cmd)));
        })
}

/// The icon-font glyph of `cmd`, for the commands Notepad has icons for.
const fn font_icon(cmd: MdCommand) -> Option<Icon> {
    match cmd {
        MdCommand::Bold => Some(Icon::Bold),
        MdCommand::Italic => Some(Icon::Italic),
        MdCommand::Strikethrough => Some(Icon::Strikethrough),
        MdCommand::InlineCode => Some(Icon::Code),
        MdCommand::Link => Some(Icon::Link),
        MdCommand::BulletList => Some(Icon::BulletedList),
        _ => None,
    }
}

/// Draw the icon for `cmd` centred in `rect`: the icon-font glyph when
/// there is one, else vector shapes or ASCII text, which never depend on a
/// font's symbol coverage.
fn paint_icon(painter: &Painter, rect: Rect, cmd: MdCommand, color: Color32) {
    if let Some(icon) = font_icon(cmd)
        && icons::paint(painter, icon, rect.center(), icons::TOOLBAR_SIZE, color)
    {
        return;
    }
    let pixels_per_point = painter.pixels_per_point();
    let center = rect.center().round_to_pixel_center(pixels_per_point);
    let r = Rect::from_center_size(center, vec2(14.0, 12.0));
    let stroke = Stroke::new(1.25, color);
    let faint = color.gamma_multiply(0.45);
    let text = |s: &str, font: FontId| {
        painter.text(center, Align2::CENTER_CENTER, s, font, color);
    };
    let row_y = |rows: usize, i: usize| {
        let step = if rows == 3 { 5.0 } else { 7.0 };
        let top = if rows == 3 {
            r.top() + 1.0
        } else {
            r.top() + 2.5
        };
        (top + step * i as f32).round_to_pixel_center(pixels_per_point)
    };
    match cmd {
        MdCommand::Bold => {
            // Faux bold: the bundled bold face is not a separate family.
            for dx in [-0.35, 0.35] {
                painter.text(
                    center + vec2(dx, 0.0),
                    Align2::CENTER_CENTER,
                    "B",
                    FontId::proportional(16.0),
                    color,
                );
            }
        }
        MdCommand::Italic => {
            let job = LayoutJob::single_section(
                "I".to_owned(),
                TextFormat {
                    font_id: FontId::proportional(16.0),
                    color,
                    italics: true,
                    ..TextFormat::default()
                },
            );
            let galley = painter.layout_job(job);
            painter.galley(center - galley.size() / 2.0, galley, color);
        }
        MdCommand::Strikethrough => {
            text("S", FontId::proportional(16.0));
            painter.hline(r.left() + 2.0..=r.right() - 2.0, center.y, stroke);
        }
        MdCommand::InlineCode => text("<>", FontId::monospace(13.0)),
        // No space between the braces: it would be wider than the icons.
        MdCommand::CodeBlock => text("{}", FontId::monospace(13.0)),
        MdCommand::Link => {
            let pill = vec2(6.5, 6.0);
            let left = Rect::from_min_size(pos2(r.left(), center.y - 3.0), pill);
            let right = Rect::from_min_size(pos2(r.right() - pill.x, center.y - 3.0), pill);
            painter.rect_stroke(left, 3.0, stroke, StrokeKind::Middle);
            painter.rect_stroke(right, 3.0, stroke, StrokeKind::Middle);
            painter.hline(r.left() + 3.5..=r.right() - 3.5, center.y, stroke);
        }
        MdCommand::BulletList => {
            for i in 0..3 {
                let y = row_y(3, i);
                painter.circle_filled(pos2(r.left() + 1.5, y), 1.5, color);
                painter.hline(r.left() + 5.0..=r.right(), y, stroke);
            }
        }
        MdCommand::NumberedList => {
            for (i, digit) in ["1", "2"].into_iter().enumerate() {
                let y = row_y(2, i);
                painter.text(
                    pos2(r.left() + 2.0, y),
                    Align2::CENTER_CENTER,
                    digit,
                    FontId::proportional(9.0),
                    color,
                );
                painter.hline(r.left() + 6.0..=r.right(), y, stroke);
            }
        }
        MdCommand::TaskList => {
            for i in 0..2 {
                let y = row_y(2, i);
                let check_box = Rect::from_center_size(pos2(r.left() + 2.5, y), vec2(5.0, 5.0));
                painter.rect_stroke(check_box, 1.0, Stroke::new(1.0, color), StrokeKind::Middle);
                if i == 0 {
                    painter.line_segment(
                        [
                            pos2(check_box.left() + 1.0, y),
                            pos2(check_box.center().x, check_box.bottom() - 1.0),
                        ],
                        Stroke::new(1.0, color),
                    );
                    painter.line_segment(
                        [
                            pos2(check_box.center().x, check_box.bottom() - 1.0),
                            pos2(check_box.right() + 0.5, check_box.top() - 0.5),
                        ],
                        Stroke::new(1.0, color),
                    );
                }
                painter.hline(r.left() + 7.0..=r.right(), y, stroke);
            }
        }
        MdCommand::Blockquote => {
            let bar = Rect::from_min_max(
                pos2(r.left() + 1.0, r.top()),
                pos2(r.left() + 3.0, r.bottom()),
            );
            painter.rect_filled(bar, 1.0, color);
            for i in 0..3 {
                let right = if i == 2 { r.right() - 4.0 } else { r.right() };
                painter.hline(r.left() + 6.0..=right, row_y(3, i), stroke);
            }
        }
        MdCommand::HorizontalRule => {
            painter.hline(
                r.left() + 2.0..=r.right() - 2.0,
                r.top() + 1.5,
                Stroke::new(1.0, faint),
            );
            painter.hline(r.x_range(), center.y, Stroke::new(1.6, color));
            painter.hline(
                r.left() + 2.0..=r.right() - 2.0,
                r.bottom() - 1.5,
                Stroke::new(1.0, faint),
            );
        }
        MdCommand::Table => {
            let header = Rect::from_min_max(r.min, pos2(r.right(), r.top() + 4.0));
            painter.rect_filled(header, 1.5, faint);
            painter.rect_stroke(r, 1.5, stroke, StrokeKind::Middle);
            painter.hline(r.x_range(), r.top() + 4.0, stroke);
            painter.hline(r.x_range(), r.top() + 8.0, Stroke::new(1.0, color));
            painter.vline(center.x, r.y_range(), stroke);
        }
        MdCommand::Heading(_) | MdCommand::Paragraph => text("H", FontId::proportional(15.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mode, commands::shortcut_table};

    fn key_event(key: Key, modifiers: Modifiers) -> egui::RawInput {
        egui::RawInput {
            modifiers,
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        }
    }

    fn app_with_text(text: &str) -> RustdownApp {
        let mut app = RustdownApp::default();
        app.tab_mut().doc.text = Arc::new(text.to_owned());
        app
    }

    fn set_selection(app: &RustdownApp, ctx: &egui::Context, range: std::ops::Range<usize>) {
        let id = app.tab().editor_id();
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(range.start),
            CCursor::new(range.end),
        )));
        state.store(ctx, id);
    }

    fn selection(app: &RustdownApp, ctx: &egui::Context) -> Option<std::ops::Range<usize>> {
        egui::TextEdit::load_state(ctx, app.tab().editor_id())
            .and_then(|state| state.cursor.char_range())
            .map(|range| range.as_sorted_char_range())
    }

    /// Every command the controls offer: dropdown entries and buttons.
    pub(crate) fn toolbar_commands() -> Vec<MdCommand> {
        HEADING_COMMANDS
            .into_iter()
            .chain(LIST_COMMANDS)
            .chain(INLINE_COMMANDS)
            .chain(INSERT_COMMANDS)
            .collect()
    }

    #[test]
    fn every_command_is_on_the_toolbar_once_with_a_label_and_a_shortcut() {
        let commands = toolbar_commands();
        assert_eq!(commands.len(), 19);
        for (i, cmd) in commands.iter().enumerate() {
            assert!(!commands[i + 1..].contains(cmd), "{cmd:?} twice");
            assert!(command_label(*cmd).is_ascii(), "{cmd:?}");
            assert!(AppCommand::Format(*cmd).shortcut().is_some(), "{cmd:?}");
        }
    }

    #[test]
    fn shortcuts_are_unique_and_avoid_reserved_keys() {
        let table = shortcut_table();
        for (i, (a, _)) in table.iter().enumerate() {
            for (b, _) in &table[i + 1..] {
                assert!(
                    a.modifiers != b.modifiers || a.logical_key != b.logical_key,
                    "duplicate shortcut {a:?}"
                );
            }
        }
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND.plus(Modifiers::SHIFT);
        let reserved = [
            (cmd, "NTOSWFHGAZYXCV0+=-"),
            (cmd_shift, "STFENXCV"),
            (Modifiers::COMMAND.plus(Modifiers::ALT), "FS"),
        ];
        for (shortcut, _) in format_shortcuts() {
            let name = shortcut.logical_key.name();
            let is_reserved = reserved.iter().any(|(modifiers, keys)| {
                shortcut.modifiers == *modifiers && name.len() == 1 && keys.contains(name)
            });
            assert!(!is_reserved, "{shortcut:?} is reserved");
            assert!(
                !matches!(
                    shortcut.logical_key,
                    Key::Enter | Key::Tab | Key::PageUp | Key::PageDown | Key::F3 | Key::F5
                ),
                "{shortcut:?} is reserved"
            );
        }
    }

    #[test]
    fn format_applies_to_selection_and_moves_it() {
        let ctx = egui::Context::default();
        let mut app = app_with_text("say hello");
        set_selection(&app, &ctx, 4..9);
        app.execute(AppCommand::Format(MdCommand::Bold), &ctx);
        assert_eq!(app.tab().doc.text.as_str(), "say **hello**");
        assert_eq!(selection(&app, &ctx), Some(6..11));
        assert!(app.tab().doc.dirty);
        assert!(app.focus_editor);
        assert_eq!(
            ctx.memory(egui::Memory::focused),
            Some(app.tab().editor_id())
        );

        app.execute(AppCommand::Format(MdCommand::Heading(1)), &ctx);
        assert_eq!(app.tab().doc.text.as_str(), "# say **hello**");
        assert_eq!(selection(&app, &ctx), Some(8..13));
    }

    #[test]
    fn format_without_editor_state_uses_end_of_text() {
        let ctx = egui::Context::default();
        let mut app = app_with_text("日本");
        app.execute_format(MdCommand::Bold, &ctx);
        assert_eq!(app.tab().doc.text.as_str(), "日本****");
        assert_eq!(selection(&app, &ctx), Some(4..4));
    }

    #[test]
    fn format_only_touches_the_active_tab() {
        let ctx = egui::Context::default();
        let mut app = app_with_text("one");
        app.new_tab();
        app.tab_mut().doc.text = Arc::new("two".to_owned());
        set_selection(&app, &ctx, 0..3);
        app.execute_format(MdCommand::Italic, &ctx);
        assert_eq!(app.tabs[0].doc.text.as_str(), "one");
        assert_eq!(app.tab().doc.text.as_str(), "*two*");
    }

    #[test]
    fn format_is_disabled_in_preview_mode() {
        let ctx = egui::Context::default();
        let mut app = app_with_text("text");
        app.mode = Mode::Preview;
        assert!(!app.format_enabled());
        set_selection(&app, &ctx, 0..4);
        app.execute_format(MdCommand::Bold, &ctx);
        assert_eq!(app.tab().doc.text.as_str(), "text");
        assert!(!app.tab().doc.dirty);
        app.mode = Mode::SideBySide;
        app.execute_format(MdCommand::Bold, &ctx);
        assert_eq!(app.tab().doc.text.as_str(), "**text**");
    }

    #[test]
    fn noop_format_keeps_the_document_clean() {
        let ctx = egui::Context::default();
        let mut app = app_with_text("plain");
        set_selection(&app, &ctx, 1..3);
        let seq = app.tab().doc.edit_seq;
        app.execute_format(MdCommand::Paragraph, &ctx);
        assert!(!app.tab().doc.dirty);
        assert_eq!(app.tab().doc.edit_seq, seq);
        assert_eq!(selection(&app, &ctx), Some(1..3));
    }

    #[test]
    fn shortcut_runs_format_command() {
        let ctx = egui::Context::default();
        let mut app = app_with_text("x");
        set_selection(&app, &ctx, 0..1);
        let _ = ctx.run(key_event(Key::Num2, Modifiers::COMMAND), |ctx| {
            app.handle_keyboard_shortcuts(ctx);
        });
        assert_eq!(app.tab().doc.text.as_str(), "## x");
        let _ = ctx.run(
            key_event(Key::L, Modifiers::COMMAND | Modifiers::SHIFT),
            |ctx| {
                app.handle_keyboard_shortcuts(ctx);
            },
        );
        assert_eq!(app.tab().doc.text.as_str(), "- [ ] ## x");
    }

    /// Format through the real editor, then undo and redo with Ctrl+Z / Ctrl+Y.
    #[test]
    fn format_is_undoable_in_the_editor() {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        let mut app = app_with_text("hello world");
        app.focus_editor = true;
        let frame = |app: &mut RustdownApp, input: egui::RawInput| {
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
            });
        };
        frame(&mut app, egui::RawInput::default());
        frame(&mut app, egui::RawInput::default());
        set_selection(&app, &ctx, 0..5);

        app.execute_format(MdCommand::Bold, &ctx);
        frame(&mut app, egui::RawInput::default());
        app.execute_format(MdCommand::Heading(2), &ctx);
        frame(&mut app, egui::RawInput::default());
        assert_eq!(app.tab().doc.text.as_str(), "## **hello** world");

        frame(&mut app, key_event(Key::Z, Modifiers::COMMAND));
        assert_eq!(app.tab().doc.text.as_str(), "**hello** world");
        assert_eq!(selection(&app, &ctx), Some(2..7));
        frame(&mut app, key_event(Key::Z, Modifiers::COMMAND));
        assert_eq!(app.tab().doc.text.as_str(), "hello world");
        assert_eq!(selection(&app, &ctx), Some(0..5));

        frame(&mut app, key_event(Key::Y, Modifiers::COMMAND));
        assert_eq!(app.tab().doc.text.as_str(), "**hello** world");
        frame(&mut app, key_event(Key::Y, Modifiers::COMMAND));
        assert_eq!(app.tab().doc.text.as_str(), "## **hello** world");
    }

    #[test]
    fn toolbar_renders_in_light_dark_and_preview_and_hides() {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        let mut app = app_with_text("# Title\n\nbody");
        let bold = tool_button_id(MdCommand::Bold);
        let enabled = |ctx: &egui::Context| ctx.read_response(bold).map(|r| r.enabled());
        for (visuals, mode) in [
            (egui::Visuals::light(), Mode::Edit),
            (egui::Visuals::dark(), Mode::SideBySide),
            (egui::Visuals::light(), Mode::Preview),
        ] {
            ctx.set_visuals(visuals);
            app.mode = mode;
            for _ in 0..2 {
                let output = ctx.run(egui::RawInput::default(), |ctx| {
                    app.show_menu_bar(ctx);
                    egui::CentralPanel::default().show(ctx, |_| {});
                });
                assert!(!output.shapes.is_empty());
            }
            assert_eq!(enabled(&ctx), Some(mode != Mode::Preview), "{mode:?}");
        }
        // View → Formatting toolbar hides just the formatting controls.
        app.show_format_toolbar = false;
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| app.show_menu_bar(ctx));
        }
        assert_eq!(enabled(&ctx), None);
        assert!(
            ctx.read_response(egui::Id::new(HEADING_DROPDOWN_ID))
                .is_none()
        );
        assert!(
            crate::menu::settings_button_rect(&ctx).is_some(),
            "settings stay"
        );
    }

    #[test]
    fn menu_row_is_one_slim_row_at_default_size_and_wraps_when_narrow() {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        crate::ui_style::configure_style(&ctx);
        let mut app = app_with_text("text");
        let mut row_height = |width: f32| {
            let input = || egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 600.0))),
                ..Default::default()
            };
            let mut height = 0.0;
            for _ in 0..2 {
                let _ = ctx.run(input(), |ctx| {
                    app.show_menu_bar(ctx);
                    height = ctx.available_rect().top();
                });
            }
            height
        };
        let wide = row_height(1024.0);
        assert!((32.0..42.0).contains(&wide), "menu row height {wide}");
        // Everything fits on one line in a small window.
        let small = row_height(640.0);
        assert!((small - wide).abs() < 0.5, "{small} vs {wide}");
        let narrow = row_height(320.0);
        assert!(
            narrow > wide,
            "narrow menu row should wrap: {narrow} vs {wide}"
        );
    }

    #[test]
    fn heading_dropdown_follows_the_selection() {
        let ctx = egui::Context::default();
        let app = app_with_text("## a\nb\n");
        set_selection(&app, &ctx, 1..1);
        assert_eq!(app.selection_heading_level(&ctx), Some(2));
        set_selection(&app, &ctx, 5..5);
        assert_eq!(app.selection_heading_level(&ctx), Some(0));
        set_selection(&app, &ctx, 0..6);
        assert_eq!(app.selection_heading_level(&ctx), None);
    }
}
