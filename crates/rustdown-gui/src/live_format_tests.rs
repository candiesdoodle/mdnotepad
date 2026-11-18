//! Headless tests of the editor in the formatted view (Edit) and the raw
//! view (Raw, Split): the caret, selection, Ln/Col, find, go to line,
//! clicks and keys still map to the right chars while syntax marks are
//! hidden; the raw view is one font, one size and one colour.

use std::sync::Arc;

use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, pos2};

use crate::{
    AppCommand, Mode, RustdownApp,
    document::EditorView,
    edit_ops::{char_to_byte, line_col, line_start_char},
    live_format::{self, OverlayKind},
};

/// Runs editor frames with synthetic input.
struct Driver {
    ctx: egui::Context,
    time: f64,
}

impl Driver {
    fn new() -> Self {
        let ctx = egui::Context::default();
        assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
        crate::ui_style::configure_style(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        Self { ctx, time: 0.0 }
    }

    fn input(&mut self, events: Vec<Event>) -> egui::RawInput {
        self.time += 0.5; // far enough apart that clicks are not double clicks
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(1024.0, 768.0),
            )),
            time: Some(self.time),
            events,
            ..Default::default()
        }
    }

    /// One frame of the editor (Edit and Raw) or of all content panels.
    fn frame(&mut self, app: &mut RustdownApp, events: Vec<Event>) -> egui::FullOutput {
        let input = self.input(events);
        self.ctx.run(input, |ctx| {
            if matches!(app.mode, Mode::Edit | Mode::Raw) {
                egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
            } else {
                app.show_content_panels(ctx);
            }
        })
    }

    fn click(&mut self, app: &mut RustdownApp, pos: Pos2) -> egui::FullOutput {
        let _ = self.frame(app, vec![Event::PointerMoved(pos)]);
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let _ = self.frame(app, vec![button(true)]);
        self.frame(app, vec![button(false)])
    }

    fn key(&mut self, app: &mut RustdownApp, key: Key) -> egui::FullOutput {
        self.frame(
            app,
            vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        )
    }
}

fn app_with_text(text: &str) -> RustdownApp {
    let mut app = RustdownApp::default();
    app.tab_mut().doc.text = Arc::new(text.to_owned());
    app
}

fn view(app: &RustdownApp) -> Option<EditorView> {
    app.tab()
        .doc
        .editor_galley_cache
        .as_ref()
        .map(|cache| cache.view)
}

fn galley(app: &RustdownApp) -> Arc<egui::Galley> {
    let cache = app.tab().doc.editor_galley_cache.as_ref();
    assert!(cache.is_some(), "the editor was laid out");
    cache.map_or_else(|| unreachable!(), |cache| cache.galley.clone())
}

/// Where the editor's galley was painted (its text shape).
fn galley_pos(output: &egui::FullOutput, text: &str) -> Pos2 {
    fn walk(shape: &egui::Shape, text: &str) -> Option<Pos2> {
        match shape {
            egui::Shape::Text(shape) if shape.galley.text() == text => Some(shape.pos),
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| walk(shape, text)),
            _ => None,
        }
    }
    let pos = output
        .shapes
        .iter()
        .find_map(|clipped| walk(&clipped.shape, text));
    assert!(pos.is_some(), "the editor text was painted");
    pos.unwrap_or(Pos2::ZERO)
}

/// Width of the glyph of char `index` in the galley.
fn glyph_width(galley: &egui::Galley, index: usize) -> f32 {
    let x = |index| {
        galley
            .pos_from_cursor(egui::text::CCursor::new(index))
            .min
            .x
    };
    x(index + 1) - x(index)
}

fn caret(app: &RustdownApp, ctx: &egui::Context) -> std::ops::Range<usize> {
    app.editor_selection(ctx)
}

/// Put the caret at char `index` (focused) and run a frame.
fn place_caret(driver: &mut Driver, app: &mut RustdownApp, index: usize) -> egui::FullOutput {
    app.select_in_editor(&driver.ctx, index..index);
    driver.frame(app, Vec::new())
}

/// The distinct fonts and colours the editor's galley lays its text out in.
fn glyph_formats(galley: &egui::Galley) -> Vec<(egui::FontId, egui::Color32)> {
    let mut formats: Vec<(egui::FontId, egui::Color32)> = Vec::new();
    for section in &galley.job.sections {
        let entry = (section.format.font_id.clone(), section.format.color);
        if !section.byte_range.is_empty() && !formats.contains(&entry) {
            formats.push(entry);
        }
    }
    formats
}

const MIXED_DOC: &str = "# Title\n\n## Sub\n\nSome **bold**, *italic* and `code`.\n\n\
    - [ ] task\n> quote\n\n```\nfenced\n```\n";

#[test]
fn edit_is_the_formatted_view_and_raw_is_plain_text() {
    let mut driver = Driver::new();
    let mut app = app_with_text("## Title\nbody\n");
    assert_eq!(app.mode, Mode::Edit);
    assert!(app.formatted_view());
    let _ = driver.frame(&mut app, Vec::new());
    assert!(matches!(view(&app), Some(EditorView::Formatted(_))));
    assert!(glyph_width(&galley(&app), 0) < 0.01, "`#` hidden");

    app.execute(AppCommand::SetMode(Mode::Raw), &driver.ctx);
    assert!(!app.formatted_view());
    let _ = driver.frame(&mut app, Vec::new());
    assert_eq!(view(&app), Some(EditorView::Raw));
    let galley = galley(&app);
    assert!(glyph_width(&galley, 0) > 5.0, "`#` shown");
    // Monospace: `#` and `i` are as wide (to the pixel the glyphs snap to).
    assert!((glyph_width(&galley, 0) - glyph_width(&galley, 4)).abs() <= 1.0);
    assert!(
        app.tab().doc.formatted_analysis.is_none(),
        "no formatting analysis kept"
    );

    app.execute(AppCommand::SetMode(Mode::Edit), &driver.ctx);
    let _ = driver.frame(&mut app, Vec::new());
    assert!(matches!(view(&app), Some(EditorView::Formatted(_))));
}

#[test]
fn raw_view_has_one_font_one_size_and_one_colour() {
    let mut driver = Driver::new();
    let mut app = app_with_text(MIXED_DOC);
    app.mode = Mode::Raw;
    let output = driver.frame(&mut app, Vec::new());
    let galley = galley(&app);
    let formats = glyph_formats(&galley);
    let expected_font = egui::FontId::monospace(crate::ui_style::TextSizes::base().monospace);
    let text_color = driver.ctx.style().visuals.text_color();
    assert_eq!(formats, vec![(expected_font.clone(), text_color)]);
    assert!(
        14.0f32
            .mul_add(-crate::ui_style::MONOSPACE_SCALE, expected_font.size)
            .abs()
            < 1e-4,
        "base size × code scale: {}",
        expected_font.size
    );
    // Every row is as tall: no heading sizes.
    let heights: Vec<f32> = galley.rows.iter().map(|row| row.height()).collect();
    assert!(
        heights.iter().all(|h| (h - heights[0]).abs() < 0.01),
        "{heights:?}"
    );
    // No overlays (bullets, checkboxes, code backgrounds) and no hidden marks.
    let cache = app.tab().doc.editor_galley_cache.as_ref();
    assert!(cache.is_some_and(|cache| cache.overlays.is_empty() && cache.format_style.is_none()));
    for index in 0..MIXED_DOC.chars().count() {
        if MIXED_DOC.chars().nth(index) != Some('\n') {
            assert!(glyph_width(&galley, index) > 3.0, "char {index} is shown");
        }
    }
    let _ = galley_pos(&output, MIXED_DOC);

    // The same document in Edit: several sizes and colours.
    app.mode = Mode::Edit;
    let _ = driver.frame(&mut app, Vec::new());
    assert!(glyph_formats(&self::galley(&app)).len() > 2);
}

#[test]
fn raw_view_keeps_find_selection_line_col_go_to_line_and_word_wrap() {
    let mut driver = Driver::new();
    let body = (1..=120).fold(String::new(), |mut body, n| {
        body.push_str(if n % 10 == 0 {
            "## Section "
        } else {
            "line **"
        });
        body.push_str(&n.to_string());
        body.push_str(if n % 10 == 0 { "\n" } else { "** target\n" });
        body
    });
    let mut app = app_with_text(&body);
    app.mode = Mode::Raw;
    let _ = driver.frame(&mut app, Vec::new());

    // Find: the match is selected, and Ln/Col counts every char.
    app.search.query = "target".to_owned();
    app.execute(AppCommand::FindNext, &driver.ctx);
    let start = body.find("target").unwrap_or_default();
    assert_eq!(caret(&app, &driver.ctx), start..start + 6);
    let _ = driver.frame(&mut app, Vec::new());
    let info = app.status_info(&driver.ctx);
    assert_eq!(info.line_col, Some(line_col(&body, start + 6)));
    assert_eq!(info.selected, 6);

    // Go to line: every row is as tall, so line 100 is at 99 rows.
    app.go_to_line(&driver.ctx, 100);
    let line = line_start_char(&body, 100);
    assert_eq!(caret(&app, &driver.ctx), line..line);
    let _ = driver.frame(&mut app, Vec::new());
    let _ = driver.frame(&mut app, Vec::new());
    let galley = galley(&app);
    let row = galley.rows[0].height();
    let caret_y = galley.pos_from_cursor(egui::text::CCursor::new(line)).min.y;
    assert!((caret_y / row - 99.0).abs() < 0.01, "{caret_y} vs {row}");
    let scrolled = app.tab_mut().current_editor_scroll_y(&driver.ctx);
    assert!(
        scrolled.is_some_and(|y| y > caret_y - 768.0 && y < caret_y),
        "{scrolled:?}"
    );

    // Word wrap: a long line wraps, and stays one row with wrap off.
    let mut app = app_with_text(&"word ".repeat(400));
    app.mode = Mode::Raw;
    let _ = driver.frame(&mut app, Vec::new());
    assert!(self::galley(&app).rows.len() > 1);
    app.execute(AppCommand::ToggleWordWrap, &driver.ctx);
    let _ = driver.frame(&mut app, Vec::new());
    assert_eq!(self::galley(&app).rows.len(), 1);
}

#[test]
fn the_caret_line_shows_its_marks_in_the_same_frame() {
    let mut driver = Driver::new();
    let text = "# Title\n\nsome **bold** text\n";
    let mut app = app_with_text(text);
    let body = text.find("some").unwrap_or_default();
    let _ = place_caret(&mut driver, &mut app, body);
    assert_eq!(view(&app), Some(EditorView::Formatted(Some((2, 2)))));
    let galley = galley(&app);
    assert!(
        glyph_width(&galley, 0) < 0.01,
        "`#` hidden off the caret line"
    );
    assert!(glyph_width(&galley, body + 5) > 3.0, "`**` shown on it");

    // One frame after the caret moves to the heading, its marks show.
    let _ = place_caret(&mut driver, &mut app, 3);
    assert_eq!(view(&app), Some(EditorView::Formatted(Some((0, 0)))));
    let galley = self::galley(&app);
    assert!(glyph_width(&galley, 0) > 3.0);
    assert!(glyph_width(&galley, body + 5) < 0.01);

    // A selection reveals every line it touches.
    app.select_in_editor(&driver.ctx, 2..body + 2);
    let _ = driver.frame(&mut app, Vec::new());
    assert_eq!(view(&app), Some(EditorView::Formatted(Some((0, 2)))));
}

#[test]
fn moving_within_a_line_keeps_the_galley() {
    let mut driver = Driver::new();
    let mut app = app_with_text("# Title\nbody **text**\nplain one\nplain two\n");
    let _ = place_caret(&mut driver, &mut app, 10);
    let before = galley(&app);
    let _ = place_caret(&mut driver, &mut app, 12);
    assert!(
        Arc::ptr_eq(&before, &galley(&app)),
        "same line: no new layout"
    );
    let _ = place_caret(&mut driver, &mut app, 2);
    assert!(
        !Arc::ptr_eq(&before, &galley(&app)),
        "other line: laid out again"
    );
    // Between lines without marks nothing changes either.
    let _ = place_caret(&mut driver, &mut app, 25);
    let plain = galley(&app);
    assert_eq!(view(&app), Some(EditorView::Formatted(None)));
    let _ = place_caret(&mut driver, &mut app, 35);
    assert!(
        Arc::ptr_eq(&plain, &galley(&app)),
        "plain to plain: no new layout"
    );
}

#[test]
fn status_bar_position_and_selection_count_ignore_hidden_marks() {
    let mut driver = Driver::new();
    let text = "## Héading\n[link](https://example.com) **bold**\n";
    let mut app = app_with_text(text);
    let bold = text.chars().count() - "bold**\n".chars().count();
    let _ = place_caret(&mut driver, &mut app, bold);
    let info = app.status_info(&driver.ctx);
    assert_eq!(info.line_col, Some((2, 31)));
    assert_eq!(info.line_col, Some(line_col(text, bold)));

    app.select_in_editor(&driver.ctx, bold..bold + 4);
    let _ = driver.frame(&mut app, Vec::new());
    assert_eq!(app.status_info(&driver.ctx).selected, 4);
}

#[test]
fn find_selects_the_match_where_it_is_drawn() {
    let mut driver = Driver::new();
    let text = "# Head\n\nA [link](https://x.y) and target here\n";
    let mut app = app_with_text(text);
    let output = driver.frame(&mut app, Vec::new());
    let origin = galley_pos(&output, text);
    app.search.query = "target".to_owned();
    app.execute(AppCommand::FindNext, &driver.ctx);
    let start = text.find("target").unwrap_or_default();
    assert_eq!(caret(&app, &driver.ctx), start..start + 6);
    // Unfocused: the match is painted as an inactive selection over the
    // glyphs of "target", which sit after the hidden URL.
    app.focus_editor = false;
    driver.ctx.memory_mut(|memory| {
        if let Some(id) = memory.focused() {
            memory.surrender_focus(id);
        }
    });
    let output = driver.frame(&mut app, Vec::new());
    let galley = galley(&app);
    let target = galley.pos_from_cursor(egui::text::CCursor::new(start));
    let painted: Vec<egui::Rect> = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Rect(rect) => Some(rect.rect),
            _ => None,
        })
        .collect();
    let target_on_screen = target.translate(origin.to_vec2());
    assert!(
        painted
            .iter()
            .any(|rect| (rect.left() - target_on_screen.left()).abs() < 0.5
                && rect.center().y > target_on_screen.top()
                && rect.center().y < target_on_screen.bottom()),
        "selection at {target_on_screen:?} in {painted:?}"
    );
}

#[test]
fn go_to_line_scrolls_to_the_line_with_variable_row_heights() {
    let mut driver = Driver::new();
    let body = (1..=300).fold(String::new(), |mut body, n| {
        if n % 10 == 0 {
            body.push_str("## Section ");
        } else {
            body.push_str("line **");
        }
        body.push_str(&n.to_string());
        body.push_str(if n % 10 == 0 { "\n" } else { "**\n" });
        body
    });
    let mut app = app_with_text(&body);
    let _ = driver.frame(&mut app, Vec::new());
    app.go_to_line(&driver.ctx, 200);
    let start = line_start_char(&body, 200);
    assert_eq!(caret(&app, &driver.ctx), start..start);
    let _ = driver.frame(&mut app, Vec::new());
    let _ = driver.frame(&mut app, Vec::new());
    assert_eq!(view(&app), Some(EditorView::Formatted(Some((199, 199)))));
    app.tab_mut().ensure_row_byte_offsets();
    let line_y = app.tab().editor_byte_to_y(char_to_byte(&body, start));
    let scrolled = app.tab_mut().current_editor_scroll_y(&driver.ctx);
    assert!(
        matches!((line_y, scrolled), (Some(line), Some(y)) if y > line - 768.0 && y < line),
        "{line_y:?} {scrolled:?}"
    );
    // Headings are taller than body lines, so y is not line × row height.
    let galley = galley(&app);
    let heights: Vec<f32> = galley.rows.iter().map(|row| row.height()).collect();
    assert!(heights[9] > heights[0] + 5.0, "{:?}", &heights[..10]);
}

#[test]
fn clicking_the_left_edge_of_a_heading_lands_before_its_text() {
    let mut driver = Driver::new();
    let text = "intro\n\n## Heading\n> quoted\n";
    let mut app = app_with_text(text);
    let _ = place_caret(&mut driver, &mut app, 0);
    let output = driver.frame(&mut app, Vec::new());
    let origin = galley_pos(&output, text);
    let galley = galley(&app);
    let heading_row = galley.rows[2].rect().translate(origin.to_vec2());
    let _ = driver.click(&mut app, pos2(origin.x + 0.5, heading_row.center().y));
    let after_marks = "intro\n\n## ".chars().count();
    assert_eq!(caret(&app, &driver.ctx), after_marks..after_marks);
    assert_eq!(view(&app), Some(EditorView::Formatted(Some((2, 2)))));

    // Same for a quote; a click in the text keeps its position.
    let galley = self::galley(&app);
    let quote_row = galley.rows[3].rect().translate(origin.to_vec2());
    let _ = driver.click(&mut app, pos2(origin.x + 0.5, quote_row.center().y));
    let quote_text = text.find("quoted").unwrap_or_default();
    assert_eq!(caret(&app, &driver.ctx), quote_text..quote_text);
    let galley = self::galley(&app);
    let on_d = galley
        .pos_from_cursor(egui::text::CCursor::new(text.len() - 2))
        .translate(origin.to_vec2());
    let _ = driver.click(&mut app, on_d.center() - egui::vec2(1.0, 0.0));
    assert_eq!(caret(&app, &driver.ctx), text.len() - 2..text.len() - 2);
}

#[test]
fn arrow_keys_home_and_end_move_through_shown_marks() {
    let mut driver = Driver::new();
    let text = "intro\n## Title\nbody\n";
    let mut app = app_with_text(text);
    let _ = place_caret(&mut driver, &mut app, 0);
    let _ = driver.frame(&mut app, Vec::new());
    assert!(driver.ctx.memory(|m| m.has_focus(app.tab().editor_id())));

    let _ = driver.key(&mut app, Key::ArrowDown);
    assert_eq!(caret(&app, &driver.ctx), 6..6, "column 0: before the marks");
    assert_eq!(view(&app), Some(EditorView::Formatted(Some((1, 1)))));
    let _ = driver.key(&mut app, Key::End);
    assert_eq!(caret(&app, &driver.ctx), 14..14);
    let _ = driver.key(&mut app, Key::Home);
    assert_eq!(caret(&app, &driver.ctx), 6..6);
    for _ in 0..3 {
        let _ = driver.key(&mut app, Key::ArrowRight);
    }
    assert_eq!(caret(&app, &driver.ctx), 9..9, "after `## `");
    let _ = driver.key(&mut app, Key::ArrowDown);
    assert_eq!(line_col(text, caret(&app, &driver.ctx).start).0, 3);
    assert_eq!(
        view(&app),
        Some(EditorView::Formatted(None)),
        "a plain line hides the heading's marks again"
    );
}

#[test]
fn clicking_a_task_checkbox_ticks_it_and_undo_unticks() {
    let mut driver = Driver::new();
    let text = "intro\n- [ ] todo\n";
    let mut app = app_with_text(text);
    let _ = place_caret(&mut driver, &mut app, 0);
    let output = driver.frame(&mut app, Vec::new());
    let origin = galley_pos(&output, text);
    let overlays = app
        .tab()
        .doc
        .editor_galley_cache
        .as_ref()
        .map(|cache| cache.overlays.clone())
        .unwrap_or_default();
    let checkbox = overlays
        .iter()
        .find(|overlay| matches!(overlay.kind, OverlayKind::Checkbox { .. }));
    assert!(checkbox.is_some(), "{overlays:?}");
    let center = checkbox.map_or(Pos2::ZERO, |overlay| overlay.rect.center()) + origin.to_vec2();
    let _ = driver.click(&mut app, center);
    assert_eq!(app.tab().doc.text.as_str(), "intro\n- [x] todo\n");
    assert!(app.tab().doc.dirty);
    assert_eq!(caret(&app, &driver.ctx), 0..0, "the caret stays");

    app.execute(AppCommand::Undo, &driver.ctx);
    let _ = driver.frame(&mut app, Vec::new());
    let _ = driver.frame(&mut app, Vec::new());
    assert_eq!(app.tab().doc.text.as_str(), text);
}

#[test]
fn split_shows_the_raw_editor_left_of_the_preview() {
    let mut driver = Driver::new();
    let text = "# Title\n\nSome **bold** text\n";
    let mut app = app_with_text(text);
    app.mode = Mode::SideBySide;
    let _ = driver.frame(&mut app, Vec::new());
    let output = driver.frame(&mut app, Vec::new());
    assert_eq!(view(&app), Some(EditorView::Raw));
    assert_eq!(glyph_formats(&galley(&app)).len(), 1, "one font and colour");
    let editor = galley_pos(&output, text);
    let preview = crate::test_harness::find_text(&output, "Title");
    assert!(
        preview.is_some_and(|title| title.left() > editor.x + 200.0),
        "editor at {editor:?}, preview heading at {preview:?}"
    );
}

#[test]
fn large_documents_show_as_raw_text_in_edit() {
    let mut driver = Driver::new();
    let line = "Some **bold** text and a [link](https://example.com).\n";
    let big = line.repeat(live_format::MAX_FORMATTED_BYTES / line.len() + 1);
    assert!(big.len() > live_format::MAX_FORMATTED_BYTES);
    let mut app = app_with_text(&big);
    assert_eq!(app.mode, Mode::Edit);
    assert!(app.formatted_view_too_large() && !app.formatted_view());
    assert!(app.large_file_hint().is_some());
    let _ = driver.frame(&mut app, Vec::new());
    assert_eq!(view(&app), Some(EditorView::Raw));
    // Raw anyway in Raw and Split: no hint there.
    app.mode = Mode::Raw;
    assert!(app.large_file_hint().is_none());

    // At the limit the formatted view is still on.
    let app = app_with_text(&big[..live_format::MAX_FORMATTED_BYTES]);
    assert!(app.formatted_view());
    assert!(app.large_file_hint().is_none());
}

#[test]
fn status_bar_switch_shows_edit_raw_split_preview_and_sets_the_mode() {
    let ctx = egui::Context::default();
    assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
    crate::ui_style::configure_style(&ctx);
    let mut app = app_with_text("# x\n");
    let mut time = 0.0;
    let mut frame = |app: &mut RustdownApp, events: Vec<Event>| {
        time += 0.5;
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(1024.0, 768.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| app.show_status_bar(ctx),
        )
    };
    let _ = frame(&mut app, Vec::new());
    let output = frame(&mut app, Vec::new());
    let switch = Mode::ALL.map(|mode| crate::test_harness::find_text(&output, mode.short_label()));
    assert!(switch.iter().all(Option::is_some), "{switch:?}");
    let x = switch.map(|rect| rect.map_or(0.0, |rect| rect.center().x));
    assert!(
        x.windows(2).all(|pair| pair[0] < pair[1]),
        "Edit Raw Split Preview: {x:?}"
    );
    assert!(crate::test_harness::find_text(&output, "Syntax").is_none());

    for (mode, rect) in Mode::ALL.into_iter().zip(switch).rev() {
        let pos = rect.map_or(Pos2::ZERO, |rect| rect.center());
        let _ = frame(&mut app, vec![Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            let _ = frame(
                &mut app,
                vec![Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                }],
            );
        }
        assert_eq!(app.mode, mode);
    }

    // A large file in Edit: the hint, left of the switch.
    app.tab_mut().doc.text = Arc::new("x".repeat(live_format::MAX_FORMATTED_BYTES + 1));
    let _ = frame(&mut app, Vec::new());
    let output = frame(&mut app, Vec::new());
    let hint = crate::test_harness::find_text(&output, crate::status_bar::LARGE_FILE_LABEL);
    let edit = crate::test_harness::find_text(&output, "Edit");
    assert!(
        matches!((hint, edit), (Some(hint), Some(edit)) if hint.right() < edit.left()),
        "{hint:?} {edit:?}"
    );
}

/// A ~`bytes` document mixing every construct the formatted view styles.
fn generated_document(bytes: usize) -> String {
    let block = "## Section heading\n\n\
        Some **bold** text, *italic*, ~~struck~~, `inline code` and a \
        [link](https://example.com/some/long/path) in a paragraph that goes on \
        long enough to wrap in a normal window width, like real prose does.\n\n\
        - first item with **bold**\n  - nested item\n- [ ] a task\n- [x] done task\n\n\
        > A quote with a [link](https://example.com).\n\n\
        ```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n\
        | a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\n";
    let mut text = String::with_capacity(bytes + block.len());
    while text.len() < bytes {
        text.push_str(block);
    }
    text
}

/// Layout cost of the formatted view at the size limit and at 1 MB, against the
/// raw view.  Run with
/// `cargo test --release timing -- --ignored --nocapture`.
#[test]
#[ignore = "timing report, not a check"]
fn formatted_view_timing_for_a_1mb_document() {
    for bytes in [live_format::MAX_FORMATTED_BYTES, 1024 * 1024] {
        report_timing(bytes);
    }
}

fn report_timing(bytes: usize) {
    use std::time::Instant;

    let ctx = Driver::new().ctx;
    let text = generated_document(bytes);
    let style = ctx.style();
    let format_style = live_format::FormatStyle::new(
        &style,
        &style.visuals,
        true,
        Some(crate::ui_style::bold_family()),
        None,
        |font| ctx.fonts_mut(|fonts| fonts.row_height(font)),
    );
    let wrap = |mut job: egui::text::LayoutJob| {
        job.wrap.max_width = 1000.0;
        job
    };
    let ms = |start: Instant| start.elapsed().as_secs_f64() * 1000.0;
    let runs = 3;
    let lines = text.matches('\n').count();
    println!(
        "document: {} bytes, {lines} lines ({} build)",
        text.len(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    for run in 0..runs {
        // Typing: a new text, so everything runs.
        let edited = format!("{run}{text}");
        let start = Instant::now();
        let analysis = live_format::analyze(&edited);
        let analyze_ms = ms(start);
        let start = Instant::now();
        let reveal = analysis.reveal_for(10 + run, 10 + run);
        let job = wrap(live_format::layout_job(
            &edited,
            &analysis,
            reveal,
            &format_style,
            |piece, font| {
                ctx.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(piece.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
                        .size()
                        .x
                })
            },
        ));
        let job_ms = ms(start);
        let start = Instant::now();
        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        let layout_ms = ms(start);
        let start = Instant::now();
        let overlays = live_format::place_overlays(&galley, &analysis, reveal, &format_style);
        let overlay_ms = ms(start);

        // Caret to another line: same analysis, new sections and galley.
        let start = Instant::now();
        let reveal = analysis.reveal_for(500 + run, 500 + run);
        let job = wrap(live_format::layout_job(
            &edited,
            &analysis,
            reveal,
            &format_style,
            |piece, font| {
                ctx.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(piece.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
                        .size()
                        .x
                })
            },
        ));
        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        let _ = live_format::place_overlays(&galley, &analysis, reveal, &format_style);
        let caret_ms = ms(start);

        // Raw view on the same edit.
        let start = Instant::now();
        let raw = format!("{run}{edited}");
        let job = wrap(egui::text::LayoutJob {
            sections: crate::editor::raw_sections(&style, &raw),
            text: raw,
            ..Default::default()
        });
        let _ = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        let raw_ms = ms(start);

        println!(
            "run {run}: typing {:.0} ms (analyze {analyze_ms:.0}, sections {job_ms:.0}, \
             egui layout {layout_ms:.0}, overlays {overlay_ms:.1}; {} rows, {} overlays) | \
             caret to another line {caret_ms:.0} ms | raw view typing {raw_ms:.0} ms",
            analyze_ms + job_ms + layout_ms + overlay_ms,
            galley.rows.len(),
            overlays.len(),
        );
    }
}

#[test]
fn view_menu_lists_the_modes_and_sets_them() {
    let mut ui = crate::test_harness::Harness::new();
    let mut app = app_with_text("# x\n");
    let _ = ui.click_text(&mut app, "View");
    let output = ui.settle(&mut app);
    assert!(crate::test_harness::find_text(&output, "Markdown syntax").is_none());
    let y = ["Raw", "Split", "Preview"]
        .map(|label| crate::test_harness::find_text(&output, label).map(|rect| rect.center().y));
    assert!(y.iter().all(Option::is_some), "{y:?}");
    assert!(y.windows(2).all(|pair| pair[0] < pair[1]), "{y:?}");
    egui::Popup::close_all(&ui.ctx);
    for (label, mode) in [
        ("Raw", Mode::Raw),
        ("Split", Mode::SideBySide),
        ("Preview", Mode::Preview),
    ] {
        let _ = ui.click_text(&mut app, "View");
        let _ = ui.click_text(&mut app, label);
        assert_eq!(app.mode, mode, "{label}");
    }
}

#[test]
fn large_file_notice_names_the_limit() {
    let kb = live_format::MAX_FORMATTED_BYTES / 1024;
    assert_eq!(live_format::MAX_FORMATTED_LABEL, format!("{kb} KB"));
    let big = "x".repeat(live_format::MAX_FORMATTED_BYTES + 1);
    let app = app_with_text(&big);
    let hover = app.large_file_hint().unwrap_or_default();
    assert!(
        hover.contains(&format!("over {}", live_format::MAX_FORMATTED_LABEL)),
        "{hover}"
    );
}
