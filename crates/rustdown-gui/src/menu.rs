//! Row 2 under the title bar, Windows 11 Notepad layout: the File · Edit ·
//! View menus at the left, the formatting controls (`format_toolbar.rs`)
//! centred in the row and, at the right end, a settings button (theme, zoom,
//! text contrast, Windows integration, bundled documents, About).
//!
//! One function per menu so separate changes can edit them independently.
//! Items run [`AppCommand`]s; shortcut text comes from the command's key
//! binding (see `commands::shortcut_table`).
//!
//! Every popup (the menus, the View → Zoom submenu, the formatting
//! dropdowns, the settings menu, the explorer's context menus) uses
//! [`popup_style`] and draws its items with [`menu_row`], so they all have
//! the geometry of Windows 11 menus (`WinUI` `MenuFlyout`, mouse input).

use eframe::egui::{
    self, Color32, Painter, Pos2, Rangef, Rect, Sense, Stroke,
    containers::menu::{MenuConfig, MenuState, SubMenu},
    emath::{GuiRounding as _, TSTransform, easing},
    pos2, vec2,
};

use crate::{
    AppCommand, BundledDoc, Mode, RustdownApp,
    edit_ops::EditAvailability,
    format_toolbar,
    icons::{self, Icon},
    ui_style,
    ui_style::ThemeChoice,
};

/// Height of a menu item.
pub(crate) const MENU_ROW_HEIGHT: f32 = 30.0;
/// How far a popup slides down into place as it opens ([`slide_in`]).
const SLIDE_IN_DISTANCE: f32 = 8.0;
/// How long the slide takes, in seconds (Windows 11 menus take about as
/// long; egui's own fade-in is quicker).
const SLIDE_IN_TIME: f32 = 0.2;
/// Inner margin of a popup: its items' hover highlight is inset this much
/// from the popup's edges, and the first and last items too.
pub(crate) const MENU_INSET: f32 = 4.0;
/// Space between an item's highlight edge and its content, left and right.
pub(crate) const MENU_ROW_PADDING_X: f32 = 12.0;
/// The lead column of check marks, radio dots and icons, before the
/// labels (`WinUI`'s placeholder).  Only popups with marks or icons have
/// it (see [`hide_lead_column`]).
pub(crate) const MENU_LEAD_WIDTH: f32 = 28.0;
/// Size of the icons in the lead column (marks are centred on it too).
pub(crate) const MENU_ICON_SIZE: f32 = 16.0;
/// Least space between a label and its shortcut text or submenu arrow.
pub(crate) const MENU_SHORTCUT_MIN_GAP: f32 = 32.0;
/// Space above and below a separator line.
pub(crate) const MENU_SEPARATOR_MARGIN: f32 = 4.0;
/// Height of a section caption in the settings menu.
const MENU_CAPTION_HEIGHT: f32 = 24.0;
/// Corner radius of an item's hover highlight.
const MENU_ROW_CORNER_RADIUS: f32 = 4.0;
/// Width of the submenu arrow's slot at the right end of an item.
const SUBMENU_ARROW_WIDTH: f32 = 12.0;
/// Height of the buttons and dropdowns in the menu row.
pub(crate) const ROW_CONTROL_HEIGHT: f32 = 28.0;
/// Around the row's controls: 34 high in all, as Notepad's row; its first
/// menu title starts 14 from the window edge (with the title padding).
const ROW_MARGIN: egui::Margin = egui::Margin {
    left: 1,
    right: 6,
    top: 3,
    bottom: 3,
};
/// Menu titles: Notepad's padding and spacing.
const MENU_TITLE_PADDING_X: f32 = 12.0;
const MENU_TITLE_GAP: f32 = 4.0;
/// Least space between the formatting controls and the menus or the gear.
const FORMAT_GROUP_MIN_GAP: f32 = 12.0;
const SETTINGS_BUTTON_SIZE: egui::Vec2 = vec2(36.0, ROW_CONTROL_HEIGHT);
/// Id of the settings (gear) button.
pub(crate) const SETTINGS_BUTTON_ID: &str = "settings_button";
/// Width of the settings menu.
const SETTINGS_MENU_WIDTH: f32 = 320.0;
/// Least width of the zoom value right of its slider.
const SLIDER_VALUE_WIDTH: f32 = 52.0;
/// Height of the zoom slider (egui's default).
const SLIDER_HEIGHT: f32 = 18.0;
/// Tooltip of the zoom slider.
const ZOOM_SLIDER_HINT: &str = "Zoom the whole window (Ctrl+Plus, Ctrl+Minus, Ctrl+0)";

/// Edit menu items, in separator-delimited groups.
pub(crate) const EDIT_MENU_GROUPS: [&[(&str, AppCommand)]; 5] = [
    &[("Undo", AppCommand::Undo), ("Redo", AppCommand::Redo)],
    &[
        ("Cut", AppCommand::Cut),
        ("Copy", AppCommand::Copy),
        ("Copy as formatted text", AppCommand::CopyFormatted),
        ("Paste", AppCommand::Paste),
        ("Paste image", AppCommand::PasteImage),
        ("Delete", AppCommand::Delete),
    ],
    &[
        ("Find…", AppCommand::Find),
        ("Find next", AppCommand::FindNext),
        ("Find previous", AppCommand::FindPrevious),
        ("Replace…", AppCommand::Replace),
        ("Go to…", AppCommand::GoToLine),
    ],
    &[
        ("Select all", AppCommand::SelectAll),
        ("Insert time/date", AppCommand::InsertTimeDate),
    ],
    &[("Format document", AppCommand::FormatDocument)],
];

/// Settings-menu items after the Windows integration items.
pub(crate) const SETTINGS_DOCUMENT_ITEMS: [(&str, AppCommand); 2] = [
    ("Demo document", AppCommand::OpenBundled(BundledDoc::Demo)),
    (
        "Verification document",
        AppCommand::OpenBundled(BundledDoc::Verification),
    ),
];
/// Settings → Start in: the item that starts in the mode last chosen.
const START_IN_LAST_USED: &str = "Last used mode";
pub(crate) const SETTINGS_ABOUT_ITEM: (&str, AppCommand) =
    (crate::about::ABOUT_TITLE, AppCommand::ShowAbout);

/// How a menu item shows its state in the gutter.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    None,
    /// On/off setting: a check mark when on.
    Check(bool),
    /// One of several choices: a dot when selected.
    Radio(bool),
}

/// What a menu item shows at its right end.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Trailing {
    Nothing,
    /// Keyboard shortcut, right-aligned in a column, in the weak colour.
    Shortcut(String),
    /// An arrow: the item opens a submenu.
    Submenu,
}

impl Trailing {
    /// The shortcut of `cmd`, if it has one.
    pub(crate) fn shortcut_of(ctx: &egui::Context, cmd: &AppCommand) -> Self {
        cmd.shortcut().map_or(Self::Nothing, |shortcut| {
            Self::Shortcut(ctx.format_shortcut(&shortcut))
        })
    }
}

/// Style of the menu row: flat buttons with the Windows 11 subtle hover
/// fill and rounded corners, Notepad's roomier menu titles.
fn menu_row_style(style: &mut egui::Style) {
    egui::containers::menu::menu_style(style);
    style.spacing.button_padding = vec2(MENU_TITLE_PADDING_X, 4.0);
    style.spacing.interact_size.y = ROW_CONTROL_HEIGHT;
    flat_widgets(style);
}

/// Style of every popup: a Windows 11 menu.  Rows of [`MENU_ROW_HEIGHT`]
/// without gaps, [`MENU_INSET`] between them and the popup's edges, and
/// egui widgets inside (the settings menu's zoom buttons and slider) as
/// tall as the rows, with the same hover fill.
pub(crate) fn popup_style(style: &mut egui::Style) {
    egui::containers::menu::menu_style(style);
    // Whole points: the egui margin type is integer.
    style.spacing.menu_margin = egui::Margin::same(MENU_INSET as i8);
    style.spacing.item_spacing.y = 0.0;
    style.spacing.interact_size.y = MENU_ROW_HEIGHT;
    flat_widgets(style);
}

/// A [`egui::Popup::menu`] for `response` (a button) with [`popup_style`].
pub(crate) fn popup_menu(response: &egui::Response) -> egui::Popup<'static> {
    egui::Popup::menu(response).style(popup_style)
}

/// Show a context menu (secondary click on `response`) with
/// [`popup_style`].
pub(crate) fn context_menu<R>(response: &egui::Response, content: impl FnOnce(&mut egui::Ui) -> R) {
    let _ = egui::Popup::context_menu(response)
        .style(popup_style)
        .show(|ui| {
            slide_in(ui);
            content(ui)
        });
}

/// Slide the popup `ui` belongs to down into place over [`SLIDE_IN_TIME`]
/// after it opens, as Windows 11 menus do (egui already fades every popup
/// in).  Call it first thing in a popup's contents, every frame.
pub(crate) fn slide_in(ui: &egui::Ui) {
    if !ui_style::motion() {
        return;
    }
    let ctx = ui.ctx();
    let layer = ui.layer_id();
    let id = egui::Id::new(("popup_slide_in", layer));
    let now = ctx.input(|input| input.time);
    let frame = ctx.cumulative_frame_nr();
    let opened_at = ctx.data_mut(|data| {
        let (opened_at, shown_in) = data.get_temp::<(f64, u64)>(id).unwrap_or((now, frame));
        // Not shown in the previous frame: it has just opened (again).
        let opened_at = if frame > shown_in + 1 { now } else { opened_at };
        data.insert_temp(id, (opened_at, frame));
        opened_at
    });
    let t = ((now - opened_at) as f32 / SLIDE_IN_TIME).min(1.0);
    let offset = (1.0 - easing::cubic_out(t)) * -SLIDE_IN_DISTANCE;
    // The identity (at the end) removes the layer's transform.
    ctx.set_transform_layer(layer, TSTransform::from_translation(vec2(0.0, offset)));
    if t < 1.0 {
        ctx.request_repaint();
    }
}

/// Where the menu row's controls of this frame are listed (see
/// [`BarControl`]).
fn bar_menus_id() -> egui::Id {
    egui::Id::new("menu_row_menus")
}

/// Where the menu row keeps whether it is in menu mode (see
/// [`switch_menu_on_hover`]).
fn menu_mode_id() -> egui::Id {
    egui::Id::new("menu_row_menu_mode")
}

/// Where the menu row keeps the menu that pointing opened (see
/// [`switch_menu_on_hover`]).
fn hover_opened_id() -> egui::Id {
    egui::Id::new("menu_row_hover_opened")
}

/// One control of the menu row for [`switch_menu_on_hover`]: the popup it
/// opens (`None` for a plain button), where it is, and whether it is
/// enabled.
#[derive(Clone, Copy)]
struct BarControl {
    popup: Option<egui::Id>,
    rect: Rect,
    enabled: bool,
}

fn register_bar_control(button: &egui::Response, control: BarControl) {
    button.ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Vec<BarControl>>(bar_menus_id())
            .push(control);
    });
}

/// List `button` (it opens a [`popup_menu`]) as one of the menu row's
/// menus for [`switch_menu_on_hover`]: File, Edit, View, the formatting
/// dropdowns and the settings gear.
pub(crate) fn register_bar_menu(button: &egui::Response, enabled: bool) {
    let control = BarControl {
        popup: Some(egui::Popup::default_response_id(button)),
        rect: button.rect,
        enabled,
    };
    register_bar_control(button, control);
}

/// List `button`, a plain button of the menu row (Bold, Italic, …), for
/// [`switch_menu_on_hover`].
pub(crate) fn register_bar_button(button: &egui::Response) {
    let control = BarControl {
        popup: None,
        rect: button.rect,
        enabled: true,
    };
    register_bar_control(button, control);
}

/// The menu row's menu mode (see [`switch_menu_on_hover`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum MenuMode {
    /// Pointing opens nothing.
    #[default]
    Off,
    /// One of the row's menus is open.
    Open,
    /// The pointer moved onto a plain button, which closed the open menu;
    /// pointing at a menu's button opens it again.
    Suspended,
}

/// The menu row (`row`) works as one Windows menu bar.  Opening one of its
/// menus with a click starts menu mode; while it lasts, pointing at a
/// menu's button opens that menu and pointing at a plain button (Bold,
/// Italic, …) closes the open one without ending the mode.  It ends when
/// a menu closes any other way (a click, Escape, an item chosen), or with
/// a click, Escape or the pointer leaving the row while no menu is open;
/// then nothing opens on hover until the next click.  Uses (and clears)
/// the controls listed this frame by [`register_bar_menu`] and
/// [`register_bar_button`]; where the pointer is comes from its position,
/// not from egui's hover state (which an open menu holds back).
fn switch_menu_on_hover(ctx: &egui::Context, row: Rect) {
    let controls = ctx
        .data_mut(|data| data.remove_temp::<Vec<BarControl>>(bar_menus_id()))
        .unwrap_or_default();
    let is_open = |control: &BarControl| {
        control
            .popup
            .is_some_and(|popup| egui::Popup::is_id_open(ctx, popup))
    };
    let open = controls.iter().position(is_open);
    let (pointer, clicked, escape) = ctx.input(|input| {
        (
            input.pointer.latest_pos(),
            input.pointer.any_click(),
            input.key_pressed(egui::Key::Escape),
        )
    });
    let in_row = pointer.is_some_and(|pos| row.contains(pos));
    let before = ctx
        .data(|data| data.get_temp::<MenuMode>(menu_mode_id()))
        .unwrap_or_default();
    let mut mode = if open.is_some() {
        MenuMode::Open
    } else if before == MenuMode::Suspended && !clicked && !escape && in_row {
        MenuMode::Suspended
    } else {
        // A menu closed, and not by this function (a click, Escape, an
        // item chosen), or menu mode ended.
        MenuMode::Off
    };
    let target = pointer.and_then(|pos| {
        controls
            .iter()
            .position(|control| control.enabled && control.rect.contains(pos))
    });
    // A click on a button whose menu pointing has just opened confirms
    // it: the button's own click would toggle it closed again.
    let hover_opened = ctx
        .data_mut(|data| data.remove_temp::<Option<egui::Id>>(hover_opened_id()))
        .flatten();
    if let Some(popup) = hover_opened {
        let confirmed = open.is_none()
            && clicked
            && target.is_some_and(|target| controls[target].popup == Some(popup));
        if confirmed {
            egui::Popup::open_id(ctx, popup);
            ctx.data_mut(|data| data.insert_temp(menu_mode_id(), MenuMode::Open));
            ctx.request_repaint();
            return;
        }
        if open.is_some_and(|open| controls[open].popup == Some(popup)) {
            // Still the menu pointing opened: keep remembering it.
            ctx.data_mut(|data| data.insert_temp(hover_opened_id(), Some(popup)));
        }
    }
    if mode != MenuMode::Off
        && let Some(target) = target
        && Some(target) != open
    {
        match controls[target].popup {
            // Closes the open one; the new one shows from the next frame.
            Some(popup) => {
                egui::Popup::open_id(ctx, popup);
                ctx.data_mut(|data| data.insert_temp(hover_opened_id(), Some(popup)));
                mode = MenuMode::Open;
                ctx.request_repaint();
            }
            None if open.is_some() => {
                egui::Popup::close_all(ctx);
                mode = MenuMode::Suspended;
                ctx.request_repaint();
            }
            None => {}
        }
    }
    ctx.data_mut(|data| data.insert_temp(menu_mode_id(), mode));
}

/// Flat buttons: transparent until hovered, then the Windows 11 subtle
/// fill, with rounded corners.
fn flat_widgets(style: &mut egui::Style) {
    let hover = ui_style::subtle_fill(&style.visuals, false);
    let pressed = ui_style::subtle_fill(&style.visuals, true);
    let widgets = &mut style.visuals.widgets;
    widgets.hovered.weak_bg_fill = hover;
    widgets.open.weak_bg_fill = hover;
    widgets.active.weak_bg_fill = pressed;
    for visuals in [
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        visuals.corner_radius = egui::CornerRadius::same(4);
        visuals.expansion = 0.0;
    }
}

impl RustdownApp {
    /// Row 2: menus, formatting controls and the settings button, on the
    /// background the active tab joins.
    ///
    /// The formatting controls are centred in the window, as in Notepad,
    /// but never closer than [`FORMAT_GROUP_MIN_GAP`] to the menus or the
    /// gear; when they do not fit between them they go to a second line.
    pub(crate) fn show_menu_bar(&mut self, ctx: &egui::Context) {
        let center = ctx.viewport_rect().center().x;
        let (fill, line) = {
            let visuals = &ctx.style().visuals;
            (
                ui_style::toolbar_background(visuals),
                ui_style::chrome_separator(visuals),
            )
        };
        let frame = egui::Frame::new().fill(fill).inner_margin(ROW_MARGIN);
        let panel = egui::TopBottomPanel::top("menu_bar")
            .frame(frame)
            .show_separator_line(false)
            .show(ctx, |ui| {
                let menus = MenuConfig::new().style(popup_style);
                let bar = egui::MenuBar::new().style(menu_row_style).config(menus);
                bar.ui(ui, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.settings_button(ui);
                        // Wraps onto a second line only in narrow windows.
                        let layout =
                            egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
                        ui.with_layout(layout, |ui| {
                            ui.spacing_mut().item_spacing.x = MENU_TITLE_GAP;
                            self.menus(ui);
                            if self.show_format_toolbar {
                                ui.spacing_mut().item_spacing.x = format_toolbar::CONTROL_GAP;
                                let free = Rangef::new(
                                    ui.cursor().left() + FORMAT_GROUP_MIN_GAP,
                                    ui.max_rect().right() - FORMAT_GROUP_MIN_GAP,
                                );
                                let width = format_toolbar::controls_width();
                                match centered_group_left(center, free, width) {
                                    Some(left) => ui.add_space(left - ui.cursor().left()),
                                    None => ui.end_row(),
                                }
                                self.format_controls(ui);
                            }
                        });
                    });
                });
            });
        // The thin line between the row and the document.
        let rect = panel.response.rect;
        switch_menu_on_hover(ctx, rect);
        ctx.layer_painter(panel.response.layer_id).hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, line),
        );
    }

    fn menus(&mut self, ui: &mut egui::Ui) {
        let file = ui.menu_button("File", |ui| {
            slide_in(ui);
            self.file_menu(ui);
        });
        let edit = ui.menu_button("Edit", |ui| {
            slide_in(ui);
            self.edit_menu(ui);
        });
        if edit.inner.is_none() {
            self.forget_undo_cache();
        }
        let view = ui.menu_button("View", |ui| {
            slide_in(ui);
            self.view_menu(ui);
        });
        for title in [&file.response, &edit.response, &view.response] {
            register_bar_menu(title, true);
        }
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        hide_lead_column(ui);
        self.menu_item(ui, "New tab", AppCommand::NewTab);
        self.menu_item(ui, "Open…", AppCommand::OpenDialog);
        self.menu_item(ui, "Quick open…", AppCommand::QuickOpen);
        self.recent_files_menu(ui);
        self.explorer_file_menu_items(ui);
        self.menu_item(ui, "Save", AppCommand::Save);
        self.menu_item(ui, "Save as…", AppCommand::SaveAs);
        self.menu_item(ui, "Save all", AppCommand::SaveAll);
        separator(ui);
        self.menu_item(ui, "Print…", AppCommand::Print);
        self.menu_item(ui, "Save as PDF…", AppCommand::ExportPdf);
        separator(ui);
        self.menu_item(ui, "Close tab", AppCommand::CloseActiveTab);
        separator(ui);
        self.menu_item(ui, "Exit", AppCommand::Exit);
    }

    /// File → Recent files: the files that still exist, newest first, and
    /// Clear recent files.
    fn recent_files_menu(&mut self, ui: &mut egui::Ui) {
        let recent: Vec<std::path::PathBuf> = self
            .recent_files
            .iter()
            .filter(|path| path.exists())
            .cloned()
            .collect();
        submenu(ui, "Recent files", |ui| {
            hide_lead_column(ui);
            if recent.is_empty() {
                ui.add_enabled_ui(false, |ui| {
                    self.menu_item(ui, "No recent files", AppCommand::ClearRecentFiles);
                });
                return;
            }
            for path in recent {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                let folder = path
                    .parent()
                    .and_then(std::path::Path::file_name)
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                let label = if folder.is_empty() {
                    name
                } else {
                    format!("{name}  ·  {folder}")
                };
                self.menu_item(ui, &label, AppCommand::OpenPath(path));
            }
            separator(ui);
            self.menu_item(ui, "Clear recent files", AppCommand::ClearRecentFiles);
        });
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        hide_lead_column(ui);
        let can = self.edit_availability(ui.ctx());
        for (index, group) in EDIT_MENU_GROUPS.into_iter().enumerate() {
            if index > 0 {
                separator(ui);
            }
            for (label, cmd) in group {
                self.edit_menu_item(ui, can, label, cmd.clone());
            }
        }
    }

    fn edit_menu_item(
        &mut self,
        ui: &mut egui::Ui,
        can: EditAvailability,
        label: &str,
        cmd: AppCommand,
    ) {
        let enabled = can.allows(&cmd);
        self.menu_entry(ui, label, Mark::None, enabled, cmd);
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        for mode in Mode::ALL {
            let mark = Mark::Radio(self.mode == mode);
            let label = mode.short_label();
            self.menu_entry(ui, label, mark, true, AppCommand::SetMode(mode));
        }
        separator(ui);
        submenu(ui, "Zoom", |ui| {
            hide_lead_column(ui);
            self.menu_item(ui, "Zoom in", AppCommand::ZoomIn);
            self.menu_item(ui, "Zoom out", AppCommand::ZoomOut);
            self.menu_item(ui, "Restore default zoom", AppCommand::ZoomReset);
        });
        for (label, checked, cmd) in [
            (
                "Status bar",
                self.status_bar_visible,
                AppCommand::ToggleStatusBar,
            ),
            ("Word wrap", self.word_wrap, AppCommand::ToggleWordWrap),
            (
                "Spell check",
                self.spell.enabled,
                AppCommand::ToggleSpellCheck,
            ),
            (
                "Formatting toolbar",
                self.show_format_toolbar,
                AppCommand::ToggleFormatToolbar,
            ),
            ("Outline", self.nav_visible, AppCommand::ToggleOutline),
            ("Focus mode", self.focus_mode, AppCommand::ToggleFocusMode),
        ] {
            self.menu_toggle(ui, label, checked, cmd);
        }
        self.explorer_view_menu_items(ui);
        separator(ui);
        self.menu_toggle(
            ui,
            "Heading colours",
            self.heading_color_mode,
            AppCommand::ToggleHeadingColors,
        );
        self.menu_toggle(
            ui,
            "Linked scrolling",
            self.side_by_side_scroll_sync,
            AppCommand::ToggleLinkedScrolling,
        );
    }

    /// The gear at the right end of the row and its settings menu.
    fn settings_button(&mut self, ui: &mut egui::Ui) {
        let (_, rect) = ui.allocate_space(SETTINGS_BUTTON_SIZE);
        let response = ui.interact(rect, egui::Id::new(SETTINGS_BUTTON_ID), Sense::click());
        register_bar_menu(&response, true);
        let popup = popup_menu(&response)
            // Stays open while the slider is dragged and the zoom buttons
            // are clicked; items close it.
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .align(egui::RectAlign::BOTTOM_END)
            .width(SETTINGS_MENU_WIDTH);
        let open = popup.is_open();
        if ui.is_rect_visible(rect) {
            let visuals = ui.visuals();
            if open || response.hovered() {
                let pressed = response.is_pointer_button_down_on();
                ui.painter()
                    .rect_filled(rect, 4.0, ui_style::subtle_fill(visuals, pressed));
            }
            let (painter, color) = (ui.painter(), visuals.text_color());
            if !icons::paint(
                painter,
                Icon::Settings,
                rect.center(),
                icons::TOOLBAR_SIZE,
                color,
            ) {
                paint_gear(painter, rect.center(), color);
            }
        }
        response.on_hover_text("Settings");
        popup.show(|ui| {
            slide_in(ui);
            self.settings_menu(ui);
        });
    }

    fn settings_menu(&mut self, ui: &mut egui::Ui) {
        caption(ui, "Theme");
        for theme in ThemeChoice::ALL {
            let mark = Mark::Radio(self.theme == theme);
            self.menu_entry(ui, theme.label(), mark, true, AppCommand::SetTheme(theme));
        }
        separator(ui);
        // The mode to start in and open Markdown files in.
        caption(ui, "Start in");
        for mode in std::iter::once(None).chain(Mode::ALL.map(Some)) {
            let label = mode.map_or(START_IN_LAST_USED, Mode::short_label);
            let mark = Mark::Radio(self.start_mode == mode);
            self.menu_entry(ui, label, mark, true, AppCommand::SetStartMode(mode));
        }
        separator(ui);
        self.zoom_slider(ui);
        separator(ui);
        self.menu_toggle(
            ui,
            "Close window with last tab",
            self.close_window_with_last_tab,
            AppCommand::ToggleCloseWindowWithLastTab,
        );

        self.windows_help_menu_items(ui);
        separator(ui);
        for (label, cmd) in SETTINGS_DOCUMENT_ITEMS {
            self.menu_item(ui, label, cmd);
        }
        separator(ui);
        let (label, cmd) = SETTINGS_ABOUT_ITEM;
        self.menu_item(ui, label, cmd);
    }

    /// Settings → Zoom: a slider over the zoom range in the steps of
    /// Ctrl+Plus / Ctrl+Minus (10 %).  Zoom scales the whole window,
    /// this menu too, so a drag takes effect when it ends (the slider
    /// would otherwise move under the pointer).  The slider starts at the
    /// item text column and its value ends at the shortcut column.
    fn zoom_slider(&mut self, ui: &mut egui::Ui) {
        caption(ui, "Zoom");
        let dragged_id = egui::Id::new("zoom_slider_dragged");
        let mut percent = ui
            .data(|data| data.get_temp::<f32>(dragged_id))
            .unwrap_or(self.zoom * 100.0);
        let indent = text_offset(ui);
        let mut zoom = None;
        // A row as tall as the items, with egui's usual slider in it.
        ui.horizontal(|ui| {
            ui.add_space(indent);
            let value = SLIDER_VALUE_WIDTH + ui.spacing().item_spacing.x + MENU_ROW_PADDING_X;
            let slider_width = (ui.available_width() - value).max(80.0);
            let spacing = ui.spacing_mut();
            spacing.interact_size = vec2(SLIDER_VALUE_WIDTH, SLIDER_HEIGHT);
            spacing.slider_width = slider_width;
            let range = crate::MIN_ZOOM_FACTOR * 100.0..=crate::MAX_ZOOM_FACTOR * 100.0;
            let slider = egui::Slider::new(&mut percent, range)
                .suffix(" %")
                .step_by(10.0)
                .fixed_decimals(0);
            let response = ui.add(slider).on_hover_text(ZOOM_SLIDER_HINT);
            if response.dragged() {
                ui.data_mut(|data| data.insert_temp(dragged_id, percent));
            } else {
                ui.data_mut(|data| data.remove::<f32>(dragged_id));
                if response.changed() || response.drag_stopped() {
                    zoom = Some(percent / 100.0);
                }
            }
        });
        if let Some(zoom) = zoom {
            self.set_zoom(zoom);
        }
    }

    /// A menu button that runs `cmd`, showing its shortcut if it has one.
    pub(crate) fn menu_item(&mut self, ui: &mut egui::Ui, label: &str, cmd: AppCommand) {
        self.menu_entry(ui, label, Mark::None, true, cmd);
    }

    /// Like [`Self::menu_item`] with a check mark while `checked`.
    pub(crate) fn menu_toggle(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        checked: bool,
        cmd: AppCommand,
    ) {
        self.menu_entry(ui, label, Mark::Check(checked), true, cmd);
    }

    fn menu_entry(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        mark: Mark,
        enabled: bool,
        cmd: AppCommand,
    ) {
        let trailing = Trailing::shortcut_of(ui.ctx(), &cmd);
        let response = menu_row(ui, label, mark, &trailing, enabled);
        if response.clicked() {
            self.execute(cmd, ui.ctx());
            // Menus close on any click anyway; the settings menu (which
            // only closes on clicks outside, for its slider) closes here.
            ui.close();
        }
    }
}

/// An item without a shortcut, for popups whose actions are not
/// [`AppCommand`]s (the explorer's context menus).  Whether it was clicked.
pub(crate) fn plain_row(ui: &mut egui::Ui, label: &str, mark: Mark) -> bool {
    menu_row(ui, label, mark, &Trailing::Nothing, true).clicked()
}

/// One menu item, as Windows 11 draws it: [`MENU_ROW_HEIGHT`] tall and as
/// wide as the popup; `mark` in the lead column, `label` at the text
/// column, `trailing` right-aligned at least [`MENU_SHORTCUT_MIN_GAP`]
/// from the label; the subtle hover fill with rounded corners.  Callers
/// that show an icon paint it into [`lead_icon_rect`].
pub(crate) fn menu_row(
    ui: &mut egui::Ui,
    label: &str,
    mark: Mark,
    trailing: &Trailing,
    enabled: bool,
) -> egui::Response {
    ui.add_enabled(enabled, |ui: &mut egui::Ui| {
        row_ui(ui, label, mark, trailing, false)
    })
}

/// An item that opens a submenu with `content` while hovered (or
/// clicked), with an arrow at its right end.
pub(crate) fn submenu<R>(
    ui: &mut egui::Ui,
    label: &str,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    // The id the row is about to get (as egui's submenu button does).
    let submenu_id = SubMenu::id_from_widget_id(ui.next_auto_id());
    let open = MenuState::from_ui(ui, |state, _| state.open_item == Some(submenu_id));
    let response = row_ui(ui, label, Mark::None, &Trailing::Submenu, open);
    SubMenu::new()
        .show(ui, &response, |ui| {
            slide_in(ui);
            content(ui)
        })
        .map(|inner| inner.inner)
}

/// Least width of an item: its content and padding.
pub(crate) fn row_width(lead: f32, label: f32, trailing: f32) -> f32 {
    let trailing = if trailing > 0.0 {
        MENU_SHORTCUT_MIN_GAP + trailing
    } else {
        0.0
    };
    2.0f32.mul_add(MENU_ROW_PADDING_X, lead + label + trailing)
}

fn row_ui(
    ui: &mut egui::Ui,
    label: &str,
    mark: Mark,
    trailing: &Trailing,
    open: bool,
) -> egui::Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let painter = ui.painter();
    let label_galley = painter.layout_no_wrap(label.to_owned(), font.clone(), Color32::PLACEHOLDER);
    let shortcut_galley = match trailing {
        Trailing::Shortcut(text) => {
            Some(painter.layout_no_wrap(text.clone(), font, Color32::PLACEHOLDER))
        }
        Trailing::Nothing | Trailing::Submenu => None,
    };
    let trailing_width = match (trailing, &shortcut_galley) {
        (_, Some(galley)) => galley.size().x,
        (Trailing::Submenu, None) => SUBMENU_ARROW_WIDTH,
        _ => 0.0,
    };
    let text_x = text_offset(ui);
    let width = row_width(
        text_x - MENU_ROW_PADDING_X,
        label_galley.size().x,
        trailing_width,
    );
    // As wide as the popup: its layout is justified.
    let response = ui.allocate_response(vec2(width, MENU_ROW_HEIGHT), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let rect = response.rect;
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let visuals = ui.visuals();
    let painter = ui.painter();
    let highlighted = response.hovered() || response.has_focus() || open;
    if ui.is_enabled() && highlighted {
        let pressed = response.is_pointer_button_down_on();
        let fill = ui_style::subtle_fill(visuals, pressed);
        painter.rect_filled(rect, MENU_ROW_CORNER_RADIUS, fill);
    }
    let color = ui.style().interact(&response).text_color();
    let ppp = ui.pixels_per_point();
    let at = |x: f32, height: f32| pos2(x, rect.center().y - height / 2.0).round_to_pixels(ppp);
    let label_pos = at(rect.left() + text_x, label_galley.size().y);
    painter.galley(label_pos, label_galley, color);
    let right = rect.right() - MENU_ROW_PADDING_X;
    if let Some(galley) = shortcut_galley {
        let pos = at(right - galley.size().x, galley.size().y);
        painter.galley(pos, galley, visuals.weak_text_color());
    } else if *trailing == Trailing::Submenu {
        let center = pos2(right - SUBMENU_ARROW_WIDTH / 2.0, rect.center().y);
        paint_submenu_arrow(painter, center, color);
    }
    paint_mark(painter, lead_icon_rect(&response).center(), mark, color);
    response
}

/// Where a popup's rows put their labels, from the row's left edge: after
/// the padding and, unless the popup hides it, the lead column.
pub(crate) fn text_offset(ui: &egui::Ui) -> f32 {
    let id = lead_column_id(ui);
    let lead = ui
        .data_mut(|data| data.get_temp::<bool>(id))
        .unwrap_or(true);
    MENU_ROW_PADDING_X + if lead { MENU_LEAD_WIDTH } else { 0.0 }
}

/// Leave out the lead column in this popup: none of its items has a mark
/// or an icon (as `WinUI` menus do).  Call before adding the items.
pub(crate) fn hide_lead_column(ui: &egui::Ui) {
    let id = lead_column_id(ui);
    ui.data_mut(|data| data.insert_temp(id, false));
}

/// One setting per popup (each popup is a layer of its own).
fn lead_column_id(ui: &egui::Ui) -> egui::Id {
    egui::Id::new("menu_lead_column").with(ui.layer_id())
}

/// The icon square in the lead column of the row `response`.
pub(crate) fn lead_icon_rect(response: &egui::Response) -> Rect {
    let rect = response.rect;
    let center = pos2(
        rect.left() + MENU_ROW_PADDING_X + MENU_ICON_SIZE / 2.0,
        rect.center().y,
    );
    Rect::from_center_size(center, vec2(MENU_ICON_SIZE, MENU_ICON_SIZE))
}

/// A thin line across the popup with [`MENU_SEPARATOR_MARGIN`] above and
/// below it.
pub(crate) fn separator(ui: &mut egui::Ui) {
    let height = 2.0f32.mul_add(MENU_SEPARATOR_MARGIN, 1.0);
    let (_, rect) = ui.allocate_space(vec2(0.0, height));
    if ui.is_rect_visible(rect) {
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        let y = rect.center().y.round_to_pixel_center(ui.pixels_per_point());
        // Edge to edge: through the popup's margin too.
        let x = rect.x_range().expand(MENU_INSET);
        ui.painter().hline(x, y, Stroke::new(1.0, stroke.color));
    }
}

/// A small caption over a group of settings, at the item text column.
fn caption(ui: &mut egui::Ui, text: &str) {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    let text_x = text_offset(ui);
    let width = text_x + galley.size().x + MENU_ROW_PADDING_X;
    let (_, rect) = ui.allocate_space(vec2(width, MENU_CAPTION_HEIGHT));
    if ui.is_rect_visible(rect) {
        let y = rect.center().y - galley.size().y / 2.0;
        let pos = pos2(rect.left() + text_x, y).round_to_pixels(ui.pixels_per_point());
        ui.painter()
            .galley(pos, galley, ui.visuals().weak_text_color());
    }
}

/// Draw a check mark or radio dot centred on `center`.
fn paint_mark(painter: &Painter, center: Pos2, mark: Mark, color: Color32) {
    match mark {
        Mark::Check(true) => {
            let s = 4.0;
            let points = vec![
                center + vec2(-s, 0.0),
                center + vec2(-s * 0.35, s * 0.7),
                center + vec2(s, -s * 0.8),
            ];
            painter.add(egui::Shape::line(points, Stroke::new(1.5, color)));
        }
        Mark::Radio(true) => {
            painter.circle_filled(center, 3.0, color);
        }
        Mark::None | Mark::Check(false) | Mark::Radio(false) => {}
    }
}

/// The arrow of a submenu item: the icon font's chevron, else line art
/// (never a text symbol, which the UI fonts may lack).
fn paint_submenu_arrow(painter: &Painter, center: Pos2, color: Color32) {
    if icons::paint(
        painter,
        Icon::ChevronRight,
        center,
        icons::CHEVRON_SIZE,
        color,
    ) {
        return;
    }
    let center = center.round_to_pixel_center(painter.pixels_per_point());
    let points = vec![
        center + vec2(-1.75, -3.5),
        center + vec2(1.75, 0.0),
        center + vec2(-1.75, 3.5),
    ];
    painter.add(egui::Shape::line(points, Stroke::new(1.0, color)));
}

/// Left edge of a group `width` wide centred on `center`, moved right or
/// left as needed to stay within `free` (the row between the menus and the
/// gear, less margins).  `None` when it does not fit there.
pub(crate) fn centered_group_left(center: f32, free: Rangef, width: f32) -> Option<f32> {
    (free.span() >= width).then(|| (center - width / 2.0).clamp(free.min, free.max - width))
}

/// A gear (settings) icon: eight teeth around a ring, as line art.
fn paint_gear(painter: &Painter, center: Pos2, color: Color32) {
    const TEETH: usize = 8;
    let (outer, inner, hole) = (7.0_f32, 5.2_f32, 2.4_f32);
    let step = std::f32::consts::TAU / TEETH as f32;
    let mut points = Vec::with_capacity(TEETH * 4);
    for tooth in 0..TEETH {
        let a = tooth as f32 * step;
        // Each tooth: flat top on the outer radius, valleys on the inner one.
        for (angle, radius) in [
            (step.mul_add(-0.30, a), inner),
            (step.mul_add(-0.17, a), outer),
            (step.mul_add(0.17, a), outer),
            (step.mul_add(0.30, a), inner),
        ] {
            points.push(center + radius * vec2(angle.cos(), angle.sin()));
        }
    }
    let stroke = Stroke::new(1.0, color);
    painter.add(egui::Shape::closed_line(points, stroke));
    painter.circle_stroke(center, hole, stroke);
}

/// The rect of the gear button from the last frame (tests click it).
#[cfg(test)]
pub(crate) fn settings_button_rect(ctx: &egui::Context) -> Option<egui::Rect> {
    ctx.read_response(egui::Id::new(SETTINGS_BUTTON_ID))
        .map(|response| response.rect)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc};

    use super::*;
    use crate::{
        format_toolbar::{
            HEADING_COMMANDS, HEADING_DROPDOWN_ID, INLINE_COMMANDS, INSERT_COMMANDS,
            INSERT_DROPDOWN_ID, LIST_COMMANDS, LIST_DROPDOWN_ID, command_label, tool_button_id,
        },
        md_commands::MdCommand,
        test_harness::{Harness, find_text},
        windows_integration::{WINDOWS_MENU_ITEMS, WindowsCommand},
    };

    fn app_with_text(text: &str) -> RustdownApp {
        let mut app = RustdownApp::default();
        app.tab_mut().doc.text = Arc::new(text.to_owned());
        app
    }

    /// Centre of the widget `id` from the last frame.
    fn widget_center(ui: &Harness, id: egui::Id) -> egui::Pos2 {
        let rect = ui.ctx.read_response(id).map(|response| response.rect);
        assert!(rect.is_some(), "{id:?} is not on screen");
        rect.map_or(egui::Pos2::ZERO, |rect| rect.center())
    }

    /// Open the popup of the button `id`; the frame showing it.
    fn open(ui: &mut Harness, app: &mut RustdownApp, id: egui::Id) -> egui::FullOutput {
        let _ = ui.settle(app);
        let _ = ui.click(app, widget_center(ui, id));
        ui.settle(app)
    }

    fn close_popups(ui: &mut Harness, app: &mut RustdownApp) {
        egui::Popup::close_all(&ui.ctx);
        let _ = ui.settle(app);
    }

    /// The menu row as one menu bar: after a click opens a dropdown,
    /// pointing at a plain button closes it and pointing at another
    /// dropdown opens that; after a click elsewhere, pointing opens nothing.
    #[test]
    fn menu_row_works_as_one_menu_bar() {
        let mut ui = Harness::new();
        let mut app = app_with_text("text");
        let is_open = |ui: &Harness, id: &str| {
            ui.ctx
                .read_response(egui::Id::new(id))
                .is_some_and(|button| {
                    egui::Popup::is_id_open(&ui.ctx, egui::Popup::default_response_id(&button))
                })
        };
        let point = |ui: &mut Harness, app: &mut RustdownApp, pos: egui::Pos2| {
            let _ = ui.hover(app, pos);
            let _ = ui.settle(app);
        };

        let _ = open(&mut ui, &mut app, egui::Id::new(HEADING_DROPDOWN_ID));
        assert!(is_open(&ui, HEADING_DROPDOWN_ID), "a click opens Paragraph");

        // A plain button closes it; menu mode goes on.
        let bold = widget_center(&ui, tool_button_id(MdCommand::Bold));
        point(&mut ui, &mut app, bold);
        assert!(!egui::Popup::is_any_open(&ui.ctx), "Bold closes the menu");

        // The next dropdown opens by itself, and the one after.
        let insert = widget_center(&ui, egui::Id::new(INSERT_DROPDOWN_ID));
        point(&mut ui, &mut app, insert);
        assert!(is_open(&ui, INSERT_DROPDOWN_ID), "+ opens on hover");
        let list = widget_center(&ui, egui::Id::new(LIST_DROPDOWN_ID));
        point(&mut ui, &mut app, list);
        assert!(is_open(&ui, LIST_DROPDOWN_ID), "lists open on hover");
        assert!(!is_open(&ui, INSERT_DROPDOWN_ID), "one menu at a time");
        // Clicking the menu pointing opened keeps it open.
        let _ = ui.click(&mut app, list);
        let _ = ui.settle(&mut app);
        assert!(is_open(&ui, LIST_DROPDOWN_ID), "a click confirms it");

        // A click in the document ends menu mode: hovering opens nothing.
        let _ = ui.click(&mut app, pos2(500.0, 500.0));
        let _ = ui.settle(&mut app);
        assert!(!egui::Popup::is_any_open(&ui.ctx));
        point(&mut ui, &mut app, insert);
        assert!(
            !egui::Popup::is_any_open(&ui.ctx),
            "no menu without a click"
        );

        // Leaving the row with no menu open ends it too.
        let _ = open(&mut ui, &mut app, egui::Id::new(HEADING_DROPDOWN_ID));
        point(&mut ui, &mut app, bold);
        point(&mut ui, &mut app, pos2(500.0, 500.0));
        point(&mut ui, &mut app, insert);
        assert!(!egui::Popup::is_any_open(&ui.ctx), "left the row: no menu");
    }

    #[test]
    fn group_is_centred_unless_that_would_overlap_the_menus_or_the_gear() {
        let free = Rangef::new(200.0, 900.0);
        // Room on both sides: centred.
        assert_eq!(centered_group_left(500.0, free, 300.0), Some(350.0));
        // Centring would cover the menus or the gear: pushed aside.
        assert_eq!(centered_group_left(300.0, free, 300.0), Some(200.0));
        assert_eq!(centered_group_left(800.0, free, 300.0), Some(600.0));
        // Exactly fits; does not fit.
        assert_eq!(centered_group_left(500.0, free, 700.0), Some(200.0));
        assert_eq!(centered_group_left(500.0, free, 701.0), None);
    }

    /// Lay out the menu row in a window `width` wide; the rects of the
    /// formatting group, the gear and the View menu title (text).
    fn menu_row_rects(width: f32) -> (egui::Rect, egui::Rect, egui::Rect) {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        crate::ui_style::configure_style(&ctx);
        let mut app = app_with_text("text");
        let mut output = None;
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(width, 600.0),
                )),
                ..Default::default()
            };
            output = Some(ctx.run(input, |ctx| app.show_menu_bar(ctx)));
        }
        let rect = |id: &str| {
            ctx.read_response(egui::Id::new(id))
                .map_or(egui::Rect::NOTHING, |response| response.rect)
        };
        let group = rect(HEADING_DROPDOWN_ID).union(rect(INSERT_DROPDOWN_ID));
        let gear = rect(SETTINGS_BUTTON_ID);
        let view = output
            .and_then(|output| find_text(&output, "View"))
            .unwrap_or(egui::Rect::NOTHING);
        (group, gear, view)
    }

    #[test]
    fn formatting_controls_are_centred_in_the_menu_row() {
        let (group, gear, view) = menu_row_rects(1024.0);
        assert!(
            (group.width() - format_toolbar::controls_width()).abs() < 0.01,
            "{group:?}"
        );
        assert!((group.center().x - 512.0).abs() <= 0.5, "{group:?}");
        assert!((group.center().y - view.center().y).abs() < 2.0, "one row");
        assert!(gear.right() <= 1024.0 && gear.left() > group.right());

        // Narrower: no longer centred, but clear of the menus and the gear,
        // and still on one row.
        let (group, gear, view) = menu_row_rects(640.0);
        let menus_right = view.right() + MENU_TITLE_PADDING_X;
        assert!(group.center().x > 320.5, "{group:?}");
        assert!(group.left() >= menus_right + FORMAT_GROUP_MIN_GAP - 1.0);
        assert!(group.right() <= gear.left() - FORMAT_GROUP_MIN_GAP + 0.01);
        assert!((group.center().y - view.center().y).abs() < 2.0, "one row");

        // Too narrow for one row: the group moves below the menus.
        let (group, _, view) = menu_row_rects(480.0);
        assert!(group.top() >= view.bottom(), "{group:?} {view:?}");
        assert!(group.left() < view.right(), "starts the second line");
    }

    #[test]
    fn menu_row_has_file_edit_view_only() {
        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        let output = ui.settle(&mut app);
        for label in ["File", "Edit", "View"] {
            assert!(find_text(&output, label).is_some(), "{label}");
        }
        for label in ["Format", "Help"] {
            assert!(find_text(&output, label).is_none(), "{label} menu is gone");
        }
    }

    /// Every command of the removed Format and Help menus is still on
    /// screen somewhere (toolbar, settings menu, Edit menu), and keeps its
    /// shortcut.
    #[test]
    fn commands_of_the_removed_format_and_help_menus_stay_reachable() {
        let former_format = HEADING_COMMANDS
            .into_iter()
            .chain([
                MdCommand::Bold,
                MdCommand::Italic,
                MdCommand::Strikethrough,
                MdCommand::InlineCode,
                MdCommand::Link,
                MdCommand::BulletList,
                MdCommand::NumberedList,
                MdCommand::TaskList,
                MdCommand::Blockquote,
                MdCommand::CodeBlock,
                MdCommand::HorizontalRule,
                MdCommand::Table,
            ])
            .map(AppCommand::Format)
            .chain([AppCommand::FormatDocument]);
        let former_help = [
            AppCommand::OpenBundled(BundledDoc::Demo),
            AppCommand::OpenBundled(BundledDoc::Verification),
            AppCommand::Windows(WindowsCommand::RegisterFileTypes),
            AppCommand::Windows(WindowsCommand::UnregisterFileTypes),
            AppCommand::ShowAbout,
        ];
        let former: Vec<AppCommand> = former_format.chain(former_help).collect();
        assert_eq!(former.len(), 25);

        let mut ui = Harness::new();
        let mut app = app_with_text("text");
        let mut reachable = HashSet::new();
        let collect = |reachable: &mut HashSet<String>,
                       output: &egui::FullOutput,
                       items: &[(&str, AppCommand)]| {
            for (label, cmd) in items {
                if find_text(output, label).is_some() {
                    reachable.insert(format!("{cmd:?}"));
                }
            }
        };

        // Formatting buttons and dropdowns.
        let _ = ui.settle(&mut app);
        for cmd in INLINE_COMMANDS {
            if ui.ctx.read_response(tool_button_id(cmd)).is_some() {
                reachable.insert(format!("{:?}", AppCommand::Format(cmd)));
            }
        }
        for (id, items) in [
            (HEADING_DROPDOWN_ID, &HEADING_COMMANDS[..]),
            (LIST_DROPDOWN_ID, &LIST_COMMANDS[..]),
            (INSERT_DROPDOWN_ID, &INSERT_COMMANDS[..]),
        ] {
            let output = open(&mut ui, &mut app, egui::Id::new(id));
            let items: Vec<_> = items
                .iter()
                .map(|&cmd| (command_label(cmd), AppCommand::Format(cmd)))
                .collect();
            collect(&mut reachable, &output, &items);
            close_popups(&mut ui, &mut app);
        }

        // Edit menu (Format document) and the settings menu.
        let _ = ui.click_text(&mut app, "Edit");
        let output = ui.settle(&mut app);
        collect(&mut reachable, &output, &EDIT_MENU_GROUPS.concat());
        close_popups(&mut ui, &mut app);

        let output = open(&mut ui, &mut app, egui::Id::new(SETTINGS_BUTTON_ID));
        let windows_items =
            WINDOWS_MENU_ITEMS.map(|(label, cmd)| (label, AppCommand::Windows(cmd)));
        collect(&mut reachable, &output, &windows_items);
        collect(&mut reachable, &output, &SETTINGS_DOCUMENT_ITEMS);
        collect(&mut reachable, &output, &[SETTINGS_ABOUT_ITEM]);

        for cmd in &former {
            assert!(
                reachable.contains(&format!("{cmd:?}")),
                "{cmd:?} is unreachable"
            );
        }
        // Shortcuts of the old menus still work.
        for cmd in former
            .iter()
            .filter(|cmd| !matches!(cmd, AppCommand::ShowAbout | AppCommand::Windows(_)))
        {
            assert!(cmd.shortcut().is_some(), "{cmd:?} lost its shortcut");
        }
    }

    #[test]
    fn toolbar_dropdowns_and_buttons_format_the_selection() {
        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        let reset = |ui: &Harness, app: &mut RustdownApp| {
            app.tab_mut().doc.text = Arc::new("word".to_owned());
            let id = app.tab().editor_id();
            let mut state = egui::TextEdit::load_state(&ui.ctx, id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(4),
                )));
            state.store(&ui.ctx, id);
        };

        reset(&ui, &mut app);
        let _ = ui.settle(&mut app);
        let _ = ui.click(
            &mut app,
            widget_center(&ui, tool_button_id(MdCommand::Bold)),
        );
        assert_eq!(app.tab().doc.text.as_str(), "**word**");

        for (dropdown, item, expected) in [
            (LIST_DROPDOWN_ID, "Numbered list", "1. word"),
            (HEADING_DROPDOWN_ID, "Heading 3", "### word"),
            (INSERT_DROPDOWN_ID, "Quote", "> word"),
        ] {
            reset(&ui, &mut app);
            let _ = open(&mut ui, &mut app, egui::Id::new(dropdown));
            let _ = ui.click_text(&mut app, item);
            assert_eq!(app.tab().doc.text.as_str(), expected, "{item}");
        }
    }

    #[test]
    fn settings_menu_runs_its_items() {
        let mut ui = Harness::new();
        let mut app = RustdownApp::default();
        let gear = egui::Id::new(SETTINGS_BUTTON_ID);

        let _ = open(&mut ui, &mut app, gear);
        let _ = ui.click_text(&mut app, "Dark");
        assert_eq!(app.theme, ThemeChoice::Dark);
        assert_eq!(ui.ctx.theme(), egui::Theme::Dark);

        app.start_mode = Some(Mode::Preview);
        let _ = open(&mut ui, &mut app, gear);
        let _ = ui.click_text(&mut app, START_IN_LAST_USED);
        assert_eq!(app.start_mode, None);

        let _ = open(&mut ui, &mut app, gear);
        let _ = ui.click_text(&mut app, SETTINGS_ABOUT_ITEM.0);
        assert!(app.show_about);
        let output = ui.settle(&mut app);
        assert!(
            find_text(&output, "Demo document").is_none(),
            "item closed the menu"
        );

        let _ = open(&mut ui, &mut app, gear);
        let _ = ui.click_text(&mut app, "Demo document");
        assert_eq!(app.tab().doc.text.as_str(), BundledDoc::Demo.content());

        // No text contrast setting any more.
        let output = open(&mut ui, &mut app, gear);
        assert!(find_text(&output, "Text contrast").is_none());
    }

    // ── Popup geometry ──────────────────────────────────────────────────

    /// Every text shape painted in `output`, in paint order.
    fn text_shapes(output: &egui::FullOutput) -> Vec<egui::epaint::TextShape> {
        fn walk(shape: &egui::Shape, out: &mut Vec<egui::epaint::TextShape>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.clone()),
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

    /// Every shape painted in `output` (groups flattened).
    fn all_shapes(output: &egui::FullOutput) -> Vec<egui::Shape> {
        fn walk(shape: &egui::Shape, out: &mut Vec<egui::Shape>) {
            match shape {
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, out)),
                shape => out.push(shape.clone()),
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    /// Line box of the last painted text `text` (popups paint last).
    fn text_box(output: &egui::FullOutput, text: &str) -> Rect {
        let found = text_shapes(output)
            .into_iter()
            .rev()
            .find(|shape| shape.galley.text() == text)
            .map(|shape| Rect::from_min_size(shape.pos, shape.galley.size()));
        assert!(found.is_some(), "{text:?} is not on screen");
        found.unwrap_or(Rect::NOTHING)
    }

    /// Every character painted in `output` is in its font (no tofu).
    fn assert_no_missing_glyphs(ctx: &egui::Context, output: &egui::FullOutput) {
        for shape in text_shapes(output) {
            let job = &shape.galley.job;
            for section in &job.sections {
                let font_id = &section.format.font_id;
                for chr in job.text[section.byte_range.clone()].chars() {
                    if chr.is_whitespace() {
                        continue;
                    }
                    let found = ctx.fonts_mut(|fonts| fonts.has_glyph(font_id, chr));
                    assert!(found, "{chr:?} in {:?} has no glyph", job.text);
                }
            }
        }
    }

    /// Rect of the popup painted at `pos` (last frame).
    fn popup_rect(ctx: &egui::Context, pos: Pos2) -> Rect {
        let layer = ctx.layer_id_at(pos);
        assert!(layer.is_some(), "no popup at {pos:?}");
        let rect = layer.and_then(|layer| ctx.memory(|memory| memory.area_rect(layer.id)));
        assert!(rect.is_some(), "{layer:?} is not an area");
        rect.unwrap_or(Rect::NOTHING)
    }

    /// The menu rows, separators and lead column on their own, in a
    /// popup-styled area.
    #[test]
    #[allow(clippy::float_cmp)] // Whole points: exact.
    fn menu_rows_have_the_windows_11_geometry() {
        let ctx = egui::Context::default();
        assert!(ui_style::configure_fonts(&ctx).is_ok());
        ui_style::configure_style(&ctx);
        let shortcut = Trailing::Shortcut("Ctrl+Alt+F".to_owned());
        let mut rows = Vec::new();
        let mut plain_row = None;
        let mut output = None;
        for _ in 0..3 {
            rows.clear();
            output = Some(ctx.run(egui::RawInput::default(), |ctx| {
                for (id, x, lead) in [("with_lead", 10.0, true), ("without_lead", 400.0, false)] {
                    egui::Area::new(egui::Id::new(id))
                        .fixed_pos(pos2(x, 10.0))
                        .show(ctx, |ui| {
                            popup_style(ui.style_mut());
                            egui::Frame::menu(ui.style()).show(ui, |ui| {
                                let layout = egui::Layout::top_down_justified(egui::Align::Min);
                                ui.with_layout(layout, |ui| {
                                    if lead {
                                        rows.push(menu_row(
                                            ui,
                                            "Format document",
                                            Mark::None,
                                            &shortcut,
                                            true,
                                        ));
                                        rows.push(menu_row(
                                            ui,
                                            "Word wrap",
                                            Mark::Check(true),
                                            &Trailing::Nothing,
                                            true,
                                        ));
                                        separator(ui);
                                        rows.push(menu_row(
                                            ui,
                                            "Table",
                                            Mark::None,
                                            &shortcut,
                                            false,
                                        ));
                                    } else {
                                        hide_lead_column(ui);
                                        plain_row = Some(menu_row(
                                            ui,
                                            "Exit",
                                            Mark::None,
                                            &Trailing::Nothing,
                                            true,
                                        ));
                                    }
                                });
                            });
                        });
                }
            }));
        }
        let output = output.unwrap_or_default();
        let [first, second, third] = [&rows[0], &rows[1], &rows[2]].map(|row| row.rect);

        // Rows: 30 high, no gaps, a 9-high separator, all as wide as the
        // widest (the one with a shortcut decides it).
        for row in [first, second, third] {
            assert_eq!(row.height(), MENU_ROW_HEIGHT, "{row:?}");
            assert_eq!(row.x_range(), first.x_range());
        }
        assert_eq!(second.top(), first.bottom());
        assert_eq!(
            third.top() - second.bottom(),
            2.0f32.mul_add(MENU_SEPARATOR_MARGIN, 1.0)
        );

        // Inset from the popup's edges (inside its 1-wide outline).
        let popup = popup_rect(&ctx, first.center());
        let outline = ctx.style().visuals.window_stroke.width;
        assert_eq!(first.left() - popup.left(), MENU_INSET + outline);
        assert_eq!(first.top() - popup.top(), MENU_INSET + outline);
        assert_eq!(popup.right() - first.right(), MENU_INSET + outline);

        // Text column, shortcut column and the gap between them.
        let label = text_box(&output, "Format document");
        let keys = text_box(&output, "Ctrl+Alt+F");
        assert_eq!(
            label.left() - first.left(),
            MENU_ROW_PADDING_X + MENU_LEAD_WIDTH
        );
        assert!(
            (first.right() - keys.right() - MENU_ROW_PADDING_X).abs() <= 1.0,
            "{keys:?}"
        );
        let gap = keys.left() - label.right();
        assert!((gap - MENU_SHORTCUT_MIN_GAP).abs() <= 1.0, "gap {gap}");
        assert_eq!(text_box(&output, "Word wrap").left(), label.left());
        assert!((label.center().y - first.center().y).abs() <= 1.0);
        assert!(
            (first.width() - row_width(MENU_LEAD_WIDTH, label.width(), keys.width())).abs() <= 1.0
        );

        // The icon square starts at the padding and leaves 12 before the
        // label; marks are centred on it.
        let icon = lead_icon_rect(&rows[2]);
        assert_eq!(icon.left() - third.left(), MENU_ROW_PADDING_X);
        assert_eq!(icon.size(), vec2(MENU_ICON_SIZE, MENU_ICON_SIZE));
        let table = text_box(&output, "Table");
        assert_eq!(
            table.left() - icon.right(),
            MENU_LEAD_WIDTH - MENU_ICON_SIZE
        );

        // A popup without marks or icons has no lead column.
        let exit = plain_row.map_or(Rect::NOTHING, |row| row.rect);
        assert_eq!(exit.height(), MENU_ROW_HEIGHT);
        assert_eq!(
            text_box(&output, "Exit").left() - exit.left(),
            MENU_ROW_PADDING_X
        );
    }

    /// How a test opens a popup.
    #[derive(Clone, Copy, Debug)]
    enum Opener {
        /// A menu title of the menu row.
        Title(&'static str),
        /// The button with this id.
        Button(&'static str),
        /// View → Zoom.
        ZoomSubmenu,
    }

    /// Open a popup; the frame showing it.
    fn open_popup(ui: &mut Harness, app: &mut RustdownApp, opener: Opener) -> egui::FullOutput {
        close_popups(ui, app);
        match opener {
            Opener::Title(title) => {
                let _ = ui.click_text(app, title);
                ui.settle(app)
            }
            Opener::Button(id) => open(ui, app, egui::Id::new(id)),
            Opener::ZoomSubmenu => {
                let output = open_popup(ui, app, Opener::Title("View"));
                let _ = ui.hover(app, text_box(&output, "Zoom").center());
                ui.settle(app)
            }
        }
    }

    /// Every popup: two adjacent items 30 apart, labels at the shared text
    /// column (after the lead column where the popup has one), the first
    /// item inset by the popup margin, and no missing glyphs.
    #[test]
    #[allow(clippy::float_cmp)] // Whole points: exact.
    fn every_popup_draws_its_items_with_the_shared_row() {
        let mut ui = Harness::new();
        let mut app = app_with_text("text");
        let mut style = (*ui.ctx.style()).clone();
        popup_style(&mut style);
        let margin = egui::Frame::menu(&style).total_margin();
        let cases = [
            (Opener::Title("File"), ["New tab", "Open…"], false),
            (Opener::Title("Edit"), ["Undo", "Redo"], false),
            (Opener::Title("View"), ["Raw", "Split"], true),
            (Opener::ZoomSubmenu, ["Zoom in", "Zoom out"], false),
            (
                Opener::Button(HEADING_DROPDOWN_ID),
                ["Heading 1", "Heading 2"],
                true,
            ),
            (
                Opener::Button(LIST_DROPDOWN_ID),
                ["Bulleted list", "Numbered list"],
                true,
            ),
            (
                Opener::Button(INSERT_DROPDOWN_ID),
                ["Table", "Code block"],
                true,
            ),
            (Opener::Button(SETTINGS_BUTTON_ID), ["Light", "Dark"], true),
            (
                Opener::Button(SETTINGS_BUTTON_ID),
                ["Demo document", "Verification document"],
                true,
            ),
        ];
        for (opener, [a, b], lead) in cases {
            let output = open_popup(&mut ui, &mut app, opener);
            let (first, second) = (text_box(&output, a), text_box(&output, b));
            assert_eq!(second.top() - first.top(), MENU_ROW_HEIGHT, "{opener:?}");
            assert_eq!(first.left(), second.left(), "{opener:?}");
            let popup = popup_rect(&ui.ctx, first.center());
            let lead = if lead { MENU_LEAD_WIDTH } else { 0.0 };
            let column = popup.left() + margin.left + MENU_ROW_PADDING_X + lead;
            assert!(
                (first.left() - column).abs() <= 0.5,
                "{opener:?}: {first:?} {popup:?}"
            );
            assert_no_missing_glyphs(&ui.ctx, &output);
        }
        // The first item of each menu starts right after the margin.
        for (opener, first) in [
            (Opener::Title("File"), "New tab"),
            (Opener::Button(HEADING_DROPDOWN_ID), "Paragraph"),
        ] {
            let output = open_popup(&mut ui, &mut app, opener);
            let label = text_box(&output, first);
            let popup = popup_rect(&ui.ctx, label.center());
            let row_center = popup.top() + margin.top + MENU_ROW_HEIGHT / 2.0;
            assert!((label.center().y - row_center).abs() <= 1.0, "{opener:?}");
        }
    }

    #[test]
    fn shortcuts_are_right_aligned_and_clear_of_their_labels() {
        let mut ui = Harness::new();
        let mut app = app_with_text("text");
        let shortcut_text = |ctx: &egui::Context, cmd: &AppCommand| {
            cmd.shortcut()
                .map(|shortcut| ctx.format_shortcut(&shortcut))
        };
        let edit: Vec<(&str, AppCommand)> = EDIT_MENU_GROUPS.concat();
        let format = |commands: &[MdCommand]| -> Vec<(&str, AppCommand)> {
            commands
                .iter()
                .map(|&cmd| (command_label(cmd), AppCommand::Format(cmd)))
                .collect()
        };
        for (opener, items) in [
            (Opener::Title("Edit"), edit),
            (
                Opener::Button(HEADING_DROPDOWN_ID),
                format(&HEADING_COMMANDS),
            ),
            (Opener::Button(LIST_DROPDOWN_ID), format(&LIST_COMMANDS)),
            (Opener::Button(INSERT_DROPDOWN_ID), format(&INSERT_COMMANDS)),
        ] {
            let output = open_popup(&mut ui, &mut app, opener);
            let mut rights = Vec::new();
            let mut least_gap = f32::INFINITY;
            for (label, cmd) in &items {
                let Some(keys) = shortcut_text(&ui.ctx, cmd) else {
                    continue;
                };
                if keys == *label {
                    continue; // Delete: the same text twice.
                }
                let (label, keys) = (text_box(&output, label), text_box(&output, &keys));
                rights.push(keys.right());
                least_gap = least_gap.min(keys.left() - label.right());
            }
            assert!(rights.len() >= 3, "{opener:?}");
            for right in &rights {
                // Within the rounding of text positions to whole pixels.
                assert!((right - rights[0]).abs() <= 1.0, "{opener:?}: {rights:?}");
            }
            assert!(
                least_gap >= MENU_SHORTCUT_MIN_GAP - 1.0,
                "{opener:?}: {least_gap}"
            );
        }
    }

    #[test]
    fn zoom_submenu_arrow_is_painted_not_a_missing_glyph() {
        let mut ui = Harness::new();
        let mut app = app_with_text("text");
        let output = open_popup(&mut ui, &mut app, Opener::Title("View"));
        let zoom = text_box(&output, "Zoom");
        // A small shape at the right end of the Zoom row: the icon font's
        // chevron, or line art without it.
        let arrow = all_shapes(&output).into_iter().find_map(|shape| {
            let rect = shape.visual_bounding_rect();
            let small = rect.width() <= SUBMENU_ARROW_WIDTH && rect.height() <= 12.0;
            let at_end = rect.center().x > zoom.right() + MENU_SHORTCUT_MIN_GAP;
            let on_row = (rect.center().y - zoom.center().y).abs() <= 2.0;
            (small && at_end && on_row).then_some(rect)
        });
        assert!(arrow.is_some(), "no submenu arrow");
        let arrow = arrow.unwrap_or(Rect::NOTHING);
        let popup = popup_rect(&ui.ctx, zoom.center());
        let row_right = popup.right() - MENU_INSET - 1.0;
        assert!(
            (row_right - MENU_ROW_PADDING_X - SUBMENU_ARROW_WIDTH / 2.0 - arrow.center().x).abs()
                <= 1.0,
            "{arrow:?} {popup:?}"
        );
        let arrow_text = egui::containers::menu::SubMenuButton::RIGHT_ARROW;
        assert!(
            text_shapes(&output)
                .iter()
                .all(|shape| !shape.galley.text().contains(arrow_text)),
            "egui's text arrow"
        );
        assert_no_missing_glyphs(&ui.ctx, &output);

        // Hovering opens the submenu, its first item level with Zoom.
        let output = open_popup(&mut ui, &mut app, Opener::ZoomSubmenu);
        let zoom_in = text_box(&output, "Zoom in");
        assert!(
            (zoom_in.center().y - zoom.center().y).abs() <= 1.0,
            "{zoom_in:?}"
        );
        assert!(zoom_in.left() > arrow.right());
        assert_no_missing_glyphs(&ui.ctx, &output);
        // Its items run their commands.
        let _ = ui.click(&mut app, zoom_in.center());
        assert!((app.zoom - 1.1).abs() < 1e-6, "{}", app.zoom);
    }

    /// The settings menu's captions and the zoom slider start at the item
    /// text column; the slider's value ends at the shortcut column.
    #[test]
    #[allow(clippy::float_cmp)] // Whole points: exact.
    fn settings_menu_rows_line_up_with_the_item_text() {
        let mut ui = Harness::new();
        let mut app = app_with_text("text");
        let output = open_popup(&mut ui, &mut app, Opener::Button(SETTINGS_BUTTON_ID));
        let column = text_box(&output, "Light").left();
        for text in [
            "Theme",
            "Dark",
            "Start in",
            START_IN_LAST_USED,
            "Zoom",
            "Close window with last tab",
            "Demo document",
            SETTINGS_ABOUT_ITEM.0,
        ] {
            let left = text_box(&output, text).left();
            assert!((left - column).abs() <= 1.0, "{text:?}: {left} vs {column}");
        }
        // The slider's rail: a long thin rect under its caption.
        let caption = text_box(&output, "Zoom");
        let rail = all_shapes(&output)
            .into_iter()
            .find_map(|shape| match shape {
                egui::Shape::Rect(rect)
                    if rect.rect.width() > 100.0
                        && rect.rect.height() <= 8.0
                        && rect.rect.top() > caption.bottom() =>
                {
                    Some(rect.rect)
                }
                _ => None,
            });
        assert!(rail.is_some(), "no slider");
        let rail = rail.unwrap_or(Rect::NOTHING);
        assert!((rail.left() - column).abs() <= 1.0, "{rail:?}");
        let demo = AppCommand::OpenBundled(BundledDoc::Demo).shortcut();
        let keys = demo.map(|shortcut| ui.ctx.format_shortcut(&shortcut));
        let keys = text_box(&output, keys.as_deref().unwrap_or_default());
        let value = text_box(&output, "100 %");
        assert!(value.left() > rail.right(), "{value:?}");
        assert!(value.right() <= keys.right() + 0.5, "{value:?} {keys:?}");
        assert!(
            value.right() >= keys.right() - SLIDER_VALUE_WIDTH,
            "{value:?}"
        );
        // The slider's row is as tall as the items, under its caption.
        let slider_row = caption.center().y + f32::midpoint(MENU_CAPTION_HEIGHT, MENU_ROW_HEIGHT);
        assert!((rail.center().y - slider_row).abs() <= 1.0, "{rail:?}");
        assert_no_missing_glyphs(&ui.ctx, &output);
    }
}
