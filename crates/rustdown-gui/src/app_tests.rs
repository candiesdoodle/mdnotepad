use super::*;
use crate::cli::{LaunchOptions, parse_launch_options};
use crate::disk::io::{DiskRevision, atomic_write_utf8, disk_revision};
use crate::disk::sync::{DiskChangeChoice, DiskReadMessage, DiskReloadOutcome, PendingDiskChange};
use crate::document::{EditorGalleyCache, TrackedTextBuffer};
use crate::md::MarkdownStyle;
use crate::scroll_math;
use crate::search::replace_all_occurrences;
use std::{
    borrow::Cow,
    cell::Cell,
    ffi::OsString,
    fs,
    sync::{Arc, mpsc},
    time::{Instant, SystemTime},
};

fn parse(args: &[&str]) -> LaunchOptions {
    parse_launch_options(args.iter().copied().map(OsString::from))
}

fn warm_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |_| {});
    ctx
}

fn test_rev(seconds: u64, len: u64) -> DiskRevision {
    DiskRevision {
        modified: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
        len,
        #[cfg(unix)]
        dev: 0,
        #[cfg(unix)]
        inode: 0,
    }
}

fn make_temp_dir(name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    dir.push(format!("{name}-{nanos}-{}", std::process::id()));
    let created = fs::create_dir_all(&dir);
    assert!(created.is_ok(), "Failed to create temp dir: {created:?}");
    dir
}

fn read_file(path: &Path) -> String {
    let text_res = fs::read_to_string(path);
    assert!(text_res.is_ok(), "Failed to read file: {text_res:?}");
    text_res.unwrap_or_else(|_| unreachable!())
}

fn dummy_editor_cache(ctx: &egui::Context, row_byte_offsets: Vec<(f32, u32)>) -> EditorGalleyCache {
    let galley = ctx.fonts_mut(|fonts| {
        fonts.layout_no_wrap(
            "dummy".to_owned(),
            egui::FontId::default(),
            egui::Color32::WHITE,
        )
    });
    EditorGalleyCache {
        content_seq: 0,
        content_color_mode: true,
        view: crate::document::EditorView::Raw,
        wrap_width_bits: 0,
        text_sizes_bits: [0; 3],
        pixels_per_point_bits: 0,
        layout_sections: Vec::new(),
        galley,
        row_byte_offsets,
        format_style: None,
        overlays: Vec::new(),
        diagrams_version: 0,
        pictures: Vec::new(),
    }
}

fn merge_tab(base_text: &str, text: &str, rev_seconds: u64, rev_len: u64, dirty: bool) -> Tab {
    let mut tab = Tab::default();
    tab.doc.path = Some(PathBuf::from("note.md"));
    tab.doc.base_text = Arc::new(base_text.to_owned());
    tab.doc.text = Arc::new(text.to_owned());
    tab.doc.disk_rev = Some(test_rev(rev_seconds, rev_len));
    tab.doc.dirty = dirty;
    // The texts below are LF files; a default tab is CRLF on Windows.
    tab.doc.line_ending = crate::line_ending::EndOfLine::Lf;
    tab
}

#[test]
fn parse_launch_options_covers_modes_paths_and_diagnostics() {
    let mode_cases = [
        (&[][..], Mode::Edit, None),
        (&["-p"][..], Mode::Preview, None),
        (&["-s"][..], Mode::SideBySide, None),
        (&["-e", "README.md"][..], Mode::Edit, Some("README.md")),
        (&["-r", "README.md"][..], Mode::Raw, Some("README.md")),
        (
            &["README.md", "OTHER.md"][..],
            Mode::Preview,
            Some("README.md"),
        ),
        (&["-p", "README.md"][..], Mode::Preview, Some("README.md")),
        (
            &["--gapplication-service", "README.md"][..],
            Mode::Preview,
            Some("README.md"),
        ),
        (
            &["--", "--scratch.md"][..],
            Mode::Preview,
            Some("--scratch.md"),
        ),
    ];

    for (args, mode, path) in mode_cases {
        let options = parse(args);
        assert_eq!(options.mode, mode);
        assert_eq!(options.path.as_deref(), path.map(PathBuf::from).as_deref());
        assert!(!options.print_version);
        assert_eq!(options.diagnostics, DiagnosticsMode::Off);
        assert_eq!(
            options.diagnostics_iterations,
            DIAGNOSTICS_DEFAULT_ITERATIONS
        );
        assert_eq!(options.diagnostics_runs, DIAGNOSTICS_DEFAULT_RUNS);
    }

    let options = parse(&["--diagnostics-open", "README.md"]);
    assert_eq!(options.diagnostics, DiagnosticsMode::OpenPipeline);
    assert_eq!(
        options.path.as_deref(),
        Some(PathBuf::from("README.md")).as_deref()
    );
    assert!(!options.print_version);
    assert_eq!(
        options.diagnostics_iterations,
        DIAGNOSTICS_DEFAULT_ITERATIONS
    );
    assert_eq!(options.diagnostics_runs, DIAGNOSTICS_DEFAULT_RUNS);

    let options = parse(&["-v"]);
    assert!(options.print_version);
    assert_eq!(options.mode, Mode::Edit);
    assert!(options.path.is_none());

    let options = parse(&["--version", "README.md"]);
    assert!(options.print_version);
    assert_eq!(
        options.path.as_deref(),
        Some(PathBuf::from("README.md")).as_deref()
    );

    let options = parse(&["--", "-v"]);
    assert!(options.print_version);
    assert_eq!(options.mode, Mode::Edit);
    assert!(options.path.is_none());

    let options = parse(&["--", "--version"]);
    assert!(options.print_version);
    assert_eq!(options.mode, Mode::Edit);
    assert!(options.path.is_none());

    let cases = [
        ("--diag-iterations=25", 25),
        ("--diagnostics-iterations=10", 10),
        ("--diag-iterations=0", DIAGNOSTICS_DEFAULT_ITERATIONS),
    ];
    for (flag, expected) in cases {
        let options = parse(&[flag, "README.md"]);
        assert_eq!(options.diagnostics_iterations, expected);
    }

    let run_cases = [
        ("--diag-runs=3", 3),
        ("--diagnostics-runs=7", 7),
        ("--diag-runs=0", DIAGNOSTICS_DEFAULT_RUNS),
    ];
    for (flag, expected) in run_cases {
        let options = parse(&[flag, "README.md"]);
        assert_eq!(options.diagnostics_runs, expected);
    }

    #[cfg(debug_assertions)]
    {
        let options = parse(&["--diagnostics-nav", "README.md"]);
        assert_eq!(options.diagnostics, DiagnosticsMode::NavPipeline);
        assert_eq!(
            options.path.as_deref(),
            Some(PathBuf::from("README.md")).as_deref()
        );

        let options = parse(&["--diag-nav", "README.md"]);
        assert_eq!(options.diagnostics, DiagnosticsMode::NavPipeline);
    }
}

#[test]
fn document_stats_and_path_helpers() {
    for (label, text, expected_lines) in [
        ("two lines", "one two\nthree", 2),
        ("unicode", "héllo 世界\n🙂", 2),
        ("empty", "", 1),
        ("single newline", "\n", 2),
    ] {
        assert_eq!(
            DocumentStats::from_text(text).lines,
            expected_lines,
            "{label}"
        );
    }
    assert_eq!(DocumentStats::from_text(""), DocumentStats::default());
    assert_eq!(Document::default().stats, DocumentStats::from_text(""));

    assert!(is_markdown_path(Path::new("note.md")));
    assert!(is_markdown_path(Path::new("README.Markdown")));
    assert!(!is_markdown_path(Path::new("notes.txt")));
    assert!(!is_markdown_path(Path::new("README")));
    let files = [
        Path::new("notes.txt"),
        Path::new("chapter.markdown"),
        Path::new("later.md"),
    ];
    assert_eq!(
        markdown_paths(files),
        vec![PathBuf::from("chapter.markdown"), PathBuf::from("later.md")]
    );
}

#[test]
fn search_and_replace_helpers_handle_empty_and_replacement_cases() {
    assert_eq!(find_match_count("abc abc", ""), 0);
    let (text, replaced) = replace_all_occurrences("alpha beta alpha", "alpha", "zeta");
    assert_eq!(text.as_ref(), "zeta beta zeta");
    assert_eq!(replaced, 2);
    let (text, replaced) = replace_all_occurrences("alpha beta", "alpha", "alpha");
    assert_eq!(text.as_ref(), "alpha beta");
    assert_eq!(replaced, 0);

    let mut search = SearchState::with_query("alpha");
    assert_eq!(search.match_count("alpha beta alpha", 1), 2);
    assert_eq!(search.match_count("alpha beta alpha", 1), 2);
    search.query = "beta".to_owned();
    assert_eq!(search.match_count("alpha beta alpha", 1), 1);
    assert_eq!(search.match_count("alpha beta alpha", 2), 1);
}

#[test]
fn shortcut_and_zoom_helpers_cover_keyboard_and_scroll_paths() {
    let cmd_shift = egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT);
    for (cmd, expected) in [
        (
            AppCommand::Save,
            egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S),
        ),
        (
            AppCommand::SaveAs,
            egui::KeyboardShortcut::new(cmd_shift, egui::Key::S),
        ),
        (
            AppCommand::NewTab,
            egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::N),
        ),
        (
            AppCommand::PrevTab,
            egui::KeyboardShortcut::new(cmd_shift, egui::Key::Tab),
        ),
    ] {
        assert_eq!(cmd.shortcut(), Some(expected), "{cmd:?}");
    }
    assert_eq!(AppCommand::ShowAbout.shortcut(), None);
    assert!((zoom_with_step(1.0, ZOOM_STEP) - 1.1).abs() < f32::EPSILON);
    assert_eq!(zoom_with_step(MAX_ZOOM_FACTOR, ZOOM_STEP), MAX_ZOOM_FACTOR);
    assert_eq!(zoom_with_step(MIN_ZOOM_FACTOR, -ZOOM_STEP), MIN_ZOOM_FACTOR);

    assert!((zoom_with_factor(1.0, 1.2) - 1.2).abs() < f32::EPSILON);
    assert_eq!(zoom_with_factor(MAX_ZOOM_FACTOR, 2.0), MAX_ZOOM_FACTOR);
    assert!((zoom_with_factor(1.0, 0.0) - 1.0).abs() < f32::EPSILON);
    assert!((zoom_with_factor(1.0, f32::NAN) - 1.0).abs() < f32::EPSILON);

    // Edge cases: invalid factors and clamping.
    for (label, input, expected) in [
        (
            "negative",
            zoom_with_factor(1.0, -1.0),
            clamped_zoom_factor(1.0),
        ),
        (
            "infinity",
            zoom_with_factor(1.0, f32::INFINITY),
            clamped_zoom_factor(1.0),
        ),
        ("clamp low", clamped_zoom_factor(0.1), MIN_ZOOM_FACTOR),
        ("clamp high", clamped_zoom_factor(10.0), MAX_ZOOM_FACTOR),
        ("clamp mid", clamped_zoom_factor(1.5), 1.5),
    ] {
        assert_eq!(input, expected, "{label}");
    }
}

#[test]
fn edit_seq_dirty_flags_stats_and_replace_all() {
    // bump_edit_seq and note_text_changed.
    let mut tab = Tab::default();
    let seq = tab.doc.edit_seq;
    tab.bump_edit_seq();
    assert_eq!(tab.doc.edit_seq, seq + 1);
    tab.note_text_changed(true);
    assert!(tab.doc.dirty && tab.doc.stats_dirty && tab.doc.preview_dirty);
    assert!(tab.doc.last_edit_at.is_some());

    // Deferred stats refresh.
    let mut tab = Tab::default();
    tab.doc.text = Arc::new("alpha beta".to_owned());
    tab.doc.stats = DocumentStats::from_text(tab.doc.text.as_str());
    tab.doc.base_text = tab.doc.text.clone();
    tab.doc.text = Arc::new("alpha beta gamma".to_owned());
    tab.bump_edit_seq();
    tab.note_text_changed(true);
    assert!(tab.doc.stats_dirty);
    tab.doc.last_edit_at = Instant::now().checked_sub(STATS_RECALC_DEBOUNCE);
    let ctx = egui::Context::default();
    tab.refresh_stats_if_due(&ctx);
    assert!(!tab.doc.stats_dirty);
    assert_eq!(tab.doc.stats, DocumentStats::from_text("alpha beta gamma"));

    // Replace all matches (app-wide search query, active tab's text).
    let mut app = RustdownApp::default();
    app.tab_mut().doc.text = Arc::new("alpha beta alpha".to_owned());
    app.search.query = "alpha".to_owned();
    app.search.replacement = "zeta".to_owned();
    assert_eq!(app.replace_all_matches(), 2);
    assert_eq!(app.tab().doc.text.as_str(), "zeta beta zeta");
    assert!(app.tab().doc.dirty);
}

#[test]
fn open_path_missing_file_treats_path_as_new_document() {
    let dir = make_temp_dir("rustdown-open-new-file-test");
    let path = dir.join("new.md");

    let mut app = RustdownApp::default();
    app.tab_mut().doc.path = Some(PathBuf::from("old.md"));
    app.tab_mut().doc.text = Arc::new("existing text".to_owned());
    app.tab_mut().doc.dirty = true;
    app.tab_mut().error = Some("old error".to_owned());

    app.open_path(path.clone());

    // Opened in a new tab; the dirty tab is left alone.
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active, 1);
    assert!(app.tabs[0].doc.dirty);
    let tab = app.tab();
    assert_eq!(tab.doc.path.as_deref(), Some(path.as_path()));
    assert_eq!(tab.doc.text.as_str(), "");
    assert_eq!(tab.doc.base_text.as_str(), "");
    assert_eq!(tab.doc.disk_rev, None);
    assert_eq!(tab.doc.stats, DocumentStats::default());
    assert!(!tab.doc.dirty);
    assert!(tab.error.is_none());
    assert!(!path.exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn incorporate_disk_text_reloads_a_clean_tab_and_asks_for_a_dirty_one() {
    // (base, ours, dirty, disk text, incoming rev, text, base, rev, asks)
    let cases = [
        // Clean: reloaded.
        ("old", "old", false, "new", 2, "new", "new", 2, false),
        // Dirty, changes apart or overlapping: asked, nothing changes.
        (
            "a\nb\n", "a\nB\n", true, "A\nb\n", 2, "a\nB\n", "a\nb\n", 1, true,
        ),
        (
            "a\nb\n", "a\nO\n", true, "a\nT\n", 2, "a\nO\n", "a\nb\n", 1, true,
        ),
        // Dirty, the file is what the edits started from: nothing to ask.
        (
            "a\nb\n", "a\nB\n", true, "a\nb\n", 2, "a\nB\n", "a\nb\n", 2, false,
        ),
    ];
    for (base, ours, dirty, disk_text, inc, exp_text, exp_base, exp_rev, asks) in cases {
        let mut tab = merge_tab(base, ours, 1, 4, dirty);
        tab.incorporate_disk_text(disk_text.to_owned(), test_rev(inc, 4));
        assert_eq!(tab.doc.text.as_str(), exp_text);
        assert_eq!(tab.doc.base_text.as_str(), exp_base);
        assert_eq!(tab.doc.disk_rev, Some(test_rev(exp_rev, 4)));
        assert_eq!(tab.doc.dirty, dirty);
        assert_eq!(tab.disk.awaits_answer(), asks, "{disk_text:?}");
    }
}

#[test]
fn a_changed_file_can_be_ignored_or_reloaded() {
    let asked = || {
        let mut tab = merge_tab("a\nb\n", "a\nO\n", 1, 4, true);
        tab.incorporate_disk_text("a\nT\n".to_owned(), test_rev(2, 4));
        assert!(tab.disk.awaits_answer());
        tab
    };

    // Ignore: the text stays exactly as it is; the file's text is the base.
    let mut tab = asked();
    tab.apply_disk_change_choice(DiskChangeChoice::Ignore);
    assert_eq!(tab.doc.text.as_str(), "a\nO\n");
    assert_eq!(tab.doc.base_text.as_str(), "a\nT\n");
    assert_eq!(tab.doc.disk_rev, Some(test_rev(2, 4)));
    assert!(tab.doc.dirty);
    assert!(!tab.disk.awaits_answer());

    // Reload: the file's text, the edits gone.
    let mut tab = asked();
    tab.apply_disk_change_choice(DiskChangeChoice::Reload);
    assert_eq!(tab.doc.text.as_str(), "a\nT\n");
    assert_eq!(tab.doc.base_text.as_str(), "a\nT\n");
    assert!(!tab.doc.dirty);
    assert!(!tab.disk.awaits_answer());
}

#[test]
fn document_metadata_title_path_debounce_and_bytecount() {
    let default_doc = Document::default();
    assert_eq!(default_doc.title().as_ref(), "Untitled");
    assert_eq!(default_doc.path_label().as_ref(), "Unsaved");
    assert!(
        default_doc
            .debounce_remaining(Duration::from_millis(500))
            .is_none()
    );

    let doc = Document {
        path: Some(PathBuf::from("/home/user/notes.md")),
        ..Document::default()
    };
    assert_eq!(doc.title().as_ref(), "notes.md");
    assert_eq!(doc.path_label().as_ref(), "/home/user/notes.md");

    for (label, text, expected) in [
        ("empty", "", 0),
        ("no nl", "no newline", 0),
        ("3 nl", "a\nb\nc\n", 3),
        ("all nl", "\n\n\n", 3),
    ] {
        assert_eq!(
            crate::md::bytecount_newlines(text.as_bytes()),
            expected,
            "{label}"
        );
    }
}

#[test]
fn mode_transitions_icons_and_uses_editor() {
    // Ctrl+Enter cycles Edit → Raw → Split → Preview → Edit, the order of
    // the status-bar switch and the View menu.
    assert_eq!(Mode::Edit.cycle(), Mode::Raw);
    assert_eq!(Mode::Raw.cycle(), Mode::SideBySide);
    assert_eq!(Mode::SideBySide.cycle(), Mode::Preview);
    assert_eq!(Mode::Preview.cycle(), Mode::Edit);
    for pair in Mode::ALL.windows(2) {
        assert_eq!(pair[0].cycle(), pair[1]);
    }
    let labels = Mode::ALL.map(Mode::short_label);
    assert_eq!(labels, ["Edit", "Raw", "Split", "Preview"]);
    for mode in Mode::ALL {
        assert!(mode.tooltip().starts_with(mode.short_label()), "{mode:?}");
        // Persisted names round-trip.
        assert_eq!(Mode::from_str_lossy(mode.as_str()), Some(mode));
    }
    assert_eq!(Mode::Raw.as_str(), "raw");
    assert_eq!(Mode::from_str_lossy("split"), Some(Mode::SideBySide));
    assert_eq!(Mode::from_str_lossy("syntax"), None);

    let ctx = egui::Context::default();
    let mut app = RustdownApp::default();
    assert_eq!(app.mode, Mode::Edit);
    assert!(app.mode.uses_editor());

    app.set_mode(Mode::Preview, &ctx);
    assert_eq!(app.mode, Mode::Preview);
    assert!(app.tab().doc.editor_galley_cache.is_none());
    assert!(!app.mode.uses_editor());

    app.set_mode(Mode::Edit, &ctx);
    assert_eq!(app.mode, Mode::Edit);

    // Same-mode is noop
    let app2 = RustdownApp::default();
    assert!(app2.tab().nav.pending_scroll.is_none());

    // Raw and SideBySide use the editor.
    app.set_mode(Mode::Raw, &ctx);
    assert!(app.mode.uses_editor());
    app.set_mode(Mode::SideBySide, &ctx);
    assert!(app.mode.uses_editor());
}

#[test]
fn persisted_mode_restores_raw_and_maps_the_old_markdown_syntax_setting() {
    let launch = |prefs: &str| {
        let prefs = crate::preferences::UserPreferences::from_toml(prefs);
        RustdownApp::from_launch_options_and_preferences(parse(&[]), &prefs).mode
    };
    assert_eq!(launch("mode = \"raw\"\n"), Mode::Raw);
    assert_eq!(
        launch("mode = \"edit\"\nmarkdown_syntax = true\n"),
        Mode::Raw
    );
    assert_eq!(launch("mode = \"edit\"\n"), Mode::Edit);
    assert_eq!(launch("mode = \"sidebyside\"\n"), Mode::SideBySide);

    // The mode is saved as chosen.
    let ctx = egui::Context::default();
    let mut app = RustdownApp::default();
    app.set_mode(Mode::Raw, &ctx);
    let saved = toml::to_string_pretty(&app.preferences()).unwrap_or_default();
    assert!(saved.contains("mode = \"raw\""), "{saved}");
    assert!(!saved.contains("markdown_syntax"), "{saved}");
    let prefs = crate::preferences::UserPreferences::from_toml(&saved);
    assert_eq!(
        RustdownApp::from_launch_options_and_preferences(parse(&[]), &prefs).mode,
        Mode::Raw
    );
    // A file on the command line opens in it too.
    assert_eq!(
        RustdownApp::from_launch_options_and_preferences(parse(&["missing.md"]), &prefs).mode,
        Mode::Raw
    );
}

#[test]
fn start_in_setting_picks_the_mode_with_or_without_a_file() {
    let launch = |prefs: &str, args: &[&str]| {
        let prefs = crate::preferences::UserPreferences::from_toml(prefs);
        RustdownApp::from_launch_options_and_preferences(parse(args), &prefs).mode
    };
    // Last used (the default): the mode last chosen.
    assert_eq!(launch("", &[]), Mode::Edit);
    assert_eq!(launch("", &["missing.md"]), Mode::Edit);
    let split = "mode = \"sidebyside\"\nstart_mode = \"last\"\n";
    assert_eq!(launch(split, &["missing.md"]), Mode::SideBySide);
    // A fixed mode, whatever was chosen last.
    let preview = "mode = \"raw\"\nstart_mode = \"preview\"\n";
    assert_eq!(launch(preview, &[]), Mode::Preview);
    assert_eq!(launch(preview, &["missing.md"]), Mode::Preview);
    // A mode flag wins; a text file opens as plain text.
    assert_eq!(launch(preview, &["-e", "missing.md"]), Mode::Edit);
    assert_eq!(launch(preview, &["notes.txt"]), Mode::Raw);

    // Saved: the setting, and the mode last chosen (not Raw picked for a
    // text file).
    let ctx = egui::Context::default();
    let prefs = crate::preferences::UserPreferences::from_toml("mode = \"edit\"\n");
    let mut app = RustdownApp::from_launch_options_and_preferences(parse(&["notes.txt"]), &prefs);
    assert_eq!(app.mode, Mode::Raw);
    assert_eq!(app.preferences().mode, "edit");
    assert_eq!(app.preferences().start_mode, "last");
    app.execute(AppCommand::SetStartMode(Some(Mode::Preview)), &ctx);
    assert_eq!(app.preferences().start_mode, "preview");
    // Choosing the mode already shown still counts.
    app.execute(AppCommand::SetMode(Mode::Raw), &ctx);
    assert_eq!(app.preferences().mode, "raw");
    app.execute(AppCommand::SetStartMode(None), &ctx);
    let saved = toml::to_string_pretty(&app.preferences()).unwrap_or_default();
    assert!(saved.contains("start_mode = \"last\""), "{saved}");
    let prefs = crate::preferences::UserPreferences::from_toml(&saved);
    assert_eq!(
        RustdownApp::from_launch_options_and_preferences(parse(&["missing.md"]), &prefs).mode,
        Mode::Raw
    );
}

#[test]
fn animate_side_by_side_scroll_advances_preview_and_snaps() {
    let ctx = warm_ctx();
    let md = "# A\n\n## B\n";
    let outline = nav::outline::extract_headings(md);
    let mut tab = Tab::default();
    tab.nav.outline = outline.clone();
    tab.side_by_side_scroll_source = Some(SideBySideScrollSource::Editor);
    tab.side_by_side_scroll_target = Some(120.0);
    tab.doc.preview_cache.total_height = 240.0;
    tab.doc.preview_cache.last_scroll_y = 20.0;

    tab.animate_side_by_side_scroll(&ctx);
    assert_eq!(
        tab.nav.pending_preview_scroll_y,
        Some((120.0_f32 - 20.0).mul_add(SIDE_BY_SIDE_SCROLL_LERP, 20.0))
    );
    assert_eq!(
        tab.side_by_side_scroll_source,
        Some(SideBySideScrollSource::Editor)
    );

    tab.doc.preview_cache.last_scroll_y = 119.5;
    tab.side_by_side_scroll_target = Some(120.0);
    tab.animate_side_by_side_scroll(&ctx);
    assert_eq!(tab.nav.pending_preview_scroll_y, Some(120.0));
    assert!(tab.side_by_side_scroll_target.is_none());
    assert!(tab.side_by_side_scroll_source.is_none());
    assert_eq!(
        tab.last_sync_preview_byte,
        Some(scroll_math::preview_scroll_y_to_byte(
            &outline,
            120.0,
            tab.doc.preview_cache.total_height
        ))
    );
}

#[test]
fn side_by_side_sync_respects_toggle_and_clear_helper() {
    let ctx = warm_ctx();
    let md = "# A\n\n## B\n";
    let outline = nav::outline::extract_headings(md);
    let mut app = RustdownApp {
        mode: Mode::SideBySide,
        side_by_side_scroll_sync: false,
        ..RustdownApp::default()
    };
    let tab = app.tab_mut();
    tab.nav.outline = outline.clone();
    tab.doc.text = Arc::new(md.to_owned());
    tab.doc.editor_galley_cache = Some(dummy_editor_cache(
        &ctx,
        vec![(0.0, 0), (40.0, outline[1].byte_offset as u32)],
    ));
    tab.doc.preview_cache.total_height = 200.0;
    tab.nav.pending_editor_scroll_y = Some(40.0);
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
    });
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
    });

    let linked = app.side_by_side_scroll_sync;
    let tab = app.tab_mut();
    tab.sync_side_by_side_scroll(&ctx, linked);
    assert!(tab.side_by_side_scroll_target.is_none());

    tab.side_by_side_scroll_source = Some(SideBySideScrollSource::Editor);
    tab.side_by_side_scroll_target = Some(120.0);
    tab.last_sync_editor_byte = Some(outline[1].byte_offset);
    tab.last_sync_preview_byte = Some(outline[0].byte_offset);
    tab.clear_side_by_side_scroll_state();
    assert!(tab.side_by_side_scroll_source.is_none());
    assert!(tab.side_by_side_scroll_target.is_none());
    assert!(tab.last_sync_editor_byte.is_none());
    assert!(tab.last_sync_preview_byte.is_none());
}

#[test]
fn tracked_text_buffer_seq_bumps_on_real_edits_only() {
    for (label, init, op, expected_seq, expected_text) in [
        ("insert", "hello", Op::Insert(" world", 5), 1, "hello world"),
        ("empty insert", "hello", Op::Insert("", 5), 0, "hello"),
        ("delete", "hello", Op::Delete(2..4), 1, "heo"),
        ("empty delete", "hello", Op::Delete(3..3), 0, "hello"),
    ] {
        let seq = Cell::new(0_u64);
        let mut text = Arc::new(init.to_owned());
        {
            let mut buf = TrackedTextBuffer {
                text: &mut text,
                seq: &seq,
                line_ending: crate::line_ending::EndOfLine::Lf,
            };
            match op {
                Op::Insert(s, pos) => {
                    egui::TextBuffer::insert_text(&mut buf, s, pos);
                }
                Op::Delete(r) => egui::TextBuffer::delete_char_range(&mut buf, r),
            }
        }
        assert_eq!(seq.get(), expected_seq, "{label}: seq");
        assert_eq!(text.as_str(), expected_text, "{label}: text");
    }
}

enum Op {
    Insert(&'static str, usize),
    Delete(std::ops::Range<usize>),
}

#[test]
fn search_state_caches_and_invalidates_across_loads() {
    // Caching by query and seq.
    let mut search = SearchState::with_query("a");
    assert_eq!(search.match_count("aaa", 1), 3);
    assert_eq!(search.match_count("aaa", 1), 3); // cached
    assert_eq!(search.match_count("aa", 2), 2); // seq change
    search.query = "b".to_owned();
    assert_eq!(search.match_count("bb", 2), 2); // query change

    // find_match_count edge cases.
    for (label, text, needle, expected) in [
        ("repeat", "aaa", "a", 3),
        ("pattern", "abcabc", "abc", 2),
        ("miss", "hello", "xyz", 0),
        ("empty text", "", "a", 0),
    ] {
        assert_eq!(find_match_count(text, needle), expected, "{label}");
    }

    // Cache invalidation across document loads.
    let mut app = RustdownApp::default();
    app.tab_mut()
        .load_document(PathBuf::from("a.md"), "hello hello".to_owned(), None);
    app.search.query = "hello".to_owned();
    assert_eq!(app.active_match_count(), 2);
    app.tab_mut()
        .load_document(PathBuf::from("b.md"), "hello world".to_owned(), None);
    assert_eq!(app.active_match_count(), 1, "stale cache");

    // replace_all_occurrences returns borrowed on noop
    for (label, haystack, needle) in [
        ("no match", "hello world", "xyz"),
        ("empty needle", "hello", ""),
    ] {
        let (result, count) = replace_all_occurrences(haystack, needle, "abc");
        assert_eq!(count, 0, "{label}");
        assert!(matches!(result, Cow::Borrowed(_)), "{label}");
    }
}

#[test]
fn default_image_uri_scheme_covers_all_paths() {
    assert_eq!(default_image_uri_scheme(None), "file://");
    assert_eq!(
        default_image_uri_scheme(Some(Path::new("/a/b/c/file.md"))),
        "file:///a/b/c/"
    );
    assert_eq!(
        default_image_uri_scheme(Some(Path::new("relative/file.md"))),
        "file:///relative/"
    );
    let dir = make_temp_dir("rustdown-image-uri-scheme-test");
    let path = dir.join("report.md");
    let scheme = default_image_uri_scheme(Some(path.as_path()));
    assert!(scheme.starts_with("file://"));
    assert!(scheme.ends_with('/'));
    let dir_name = dir.file_name().and_then(|name| name.to_str()).unwrap_or("");
    assert!(
        scheme.contains(dir_name),
        "Expected '{scheme}' to contain '{dir_name}'"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolve_nav_scroll_target_covers_modes_and_anchor() {
    let ctx = egui::Context::default();

    // Preview sets preview only.
    let mut tab = Tab::default();
    let mode = Mode::Preview;
    tab.nav.pending_scroll = Some(nav::panel::NavScrollTarget::Top);
    tab.resolve_nav_scroll_target(&ctx, mode);
    assert_eq!(tab.nav.pending_preview_scroll_y, Some(0.0));
    assert!(tab.nav.pending_editor_scroll_y.is_none());

    // SideBySide sets both panes and clears stale animation target.
    let mut tab = Tab::default();
    let mode = Mode::SideBySide;
    tab.side_by_side_scroll_target = Some(999.0);
    tab.nav.pending_scroll = Some(nav::panel::NavScrollTarget::Top);
    tab.resolve_nav_scroll_target(&ctx, mode);
    assert_eq!(tab.nav.pending_editor_scroll_y, Some(0.0));
    assert_eq!(tab.nav.pending_preview_scroll_y, Some(0.0));
    assert!(
        tab.side_by_side_scroll_target.is_none(),
        "nav scroll should cancel animation"
    );

    // Preview with heading anchor uses heading_y.
    let mut tab = Tab::default();
    let mode = Mode::Preview;
    let md = "# A\n\ntext\n\n## B\n";
    tab.nav.outline = nav::outline::extract_headings(md);
    let style = MarkdownStyle::from_visuals(&egui::Visuals::dark());
    tab.doc.preview_cache.ensure_parsed(md);
    tab.doc.preview_cache.ensure_heights(14.0, 400.0, &style);
    let b_offset = tab.nav.outline[1].byte_offset;
    let expected_y = tab.doc.preview_cache.heading_y(1).unwrap_or(0.0);
    tab.nav.pending_scroll = Some(nav::panel::NavScrollTarget::ByteOffset(b_offset));
    tab.resolve_nav_scroll_target(&ctx, mode);
    let actual_y = tab.nav.pending_preview_scroll_y.unwrap_or(0.0);
    assert!((actual_y - expected_y).abs() < 0.01);
    assert!(tab.nav.pending_editor_scroll_y.is_none());
}

#[test]
fn a_reload_leaves_the_tab_clean() {
    let mut tab = Tab::default();
    tab.doc.dirty = true;
    tab.doc.last_edit_at = Some(Instant::now());
    tab.apply_disk_text_state(Arc::new("test".to_owned()), test_rev(0, 4));
    assert!(!tab.doc.dirty);
    assert!(tab.doc.last_edit_at.is_none());
    assert_eq!(tab.doc.base_text.as_str(), "test");
}

#[test]
fn document_lifecycle_load_new_and_blank() {
    let dir = make_temp_dir("rustdown-doc-lifecycle-test");
    let path = dir.join("test.md");
    fs::write(&path, "test content").ok();
    let rev = disk::io::disk_revision(&path).ok();

    let mut tab = Tab::default();
    let seq0 = tab.doc.edit_seq;

    // Load resets state.
    tab.doc.dirty = true;
    tab.load_document(path.clone(), "test content".to_owned(), rev);
    assert_eq!(tab.doc.path.as_deref(), Some(path.as_path()));
    assert_eq!(tab.doc.text.as_str(), "test content");
    assert_eq!(tab.doc.base_text.as_str(), "test content");
    assert!(!tab.doc.dirty);
    assert!(!tab.doc.stats_dirty);
    assert_eq!(tab.doc.stats, DocumentStats::from_text("test content"));
    let seq1 = tab.doc.edit_seq;
    assert!(seq1 > seq0, "edit_seq should advance on first load");

    // Second load also advances edit_seq.
    tab.load_document(PathBuf::from("b.md"), "bbb".to_owned(), None);
    let seq2 = tab.doc.edit_seq;
    assert!(seq2 > seq1, "edit_seq should advance on second load");

    // Loading another document advances edit_seq and invalidates the outline.
    tab.load_document(PathBuf::from("a.md"), "# Heading\n".to_owned(), None);
    tab.nav.refresh_outline(&tab.doc.text, tab.doc.edit_seq);
    assert_eq!(tab.nav.outline.len(), 1);
    let seq_before = tab.doc.edit_seq;
    tab.init_document(None, String::new(), None);
    assert!(
        tab.doc.edit_seq > seq_before,
        "reload must advance edit_seq"
    );
    let text = tab.doc.text.clone();
    tab.nav.refresh_outline(&text, tab.doc.edit_seq);
    assert!(
        tab.nav.outline.is_empty(),
        "nav outline should be empty after loading a blank document"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn app_operations_search_format_stats_action_reload() {
    // Launch options.
    let opts = LaunchOptions {
        mode: Mode::Preview,
        mode_explicit: true,
        ..parse(&[])
    };
    assert_eq!(RustdownApp::from_launch_options(opts).mode, Mode::Preview);

    // Search open/close.
    let mut app = RustdownApp::default();
    assert!(!app.search.visible);
    app.open_search(false);
    assert!(app.search.visible && !app.search.replace_mode && app.focus_search);
    app.open_search(true);
    assert!(app.search.replace_mode);
    app.close_search();
    assert!(!app.search.visible && !app.search.replace_mode && !app.focus_search);

    // Format document.
    let mut tab = Tab::default();
    tab.doc.text = Arc::new("# Hello\n\nworld".to_owned());
    let seq_before = tab.doc.edit_seq;
    tab.format_document();
    assert!(tab.doc.text.ends_with('\n'));
    assert!(tab.doc.edit_seq > seq_before && tab.doc.dirty);

    // Refresh stats.
    let mut tab = Tab::default();
    tab.doc.text = Arc::new("a\nb\nc\n".to_owned());
    tab.doc.stats_dirty = true;
    tab.refresh_stats_now();
    assert_eq!(tab.doc.stats.lines, 4);
    assert!(!tab.doc.stats_dirty);

    // Closing a dirty tab asks first.
    let ctx = egui::Context::default();
    let mut app = RustdownApp::default();
    app.tab_mut().doc.dirty = true;
    app.execute(AppCommand::CloseActiveTab, &ctx);
    assert!(app.close_prompt.is_some());

    // Schedule disk reload keeps earliest time.
    let mut tab = Tab::default();
    let now = Instant::now();
    tab.schedule_disk_reload(now);
    assert!(tab.disk.pending_reload_at.is_some());
    let first = tab.disk.pending_reload_at;
    tab.schedule_disk_reload(now);
    assert_eq!(tab.disk.pending_reload_at, first);
}

#[test]
fn reload_clean_dirty_conflict_and_large_file() {
    // Clean buffer reload
    let mut tab = Tab::default();
    tab.doc.text = Arc::new("original".into());
    tab.doc.base_text = Arc::new("original".into());
    let old_seq = tab.doc.edit_seq;
    let new_text = Arc::new("new content".to_owned());
    tab.apply_disk_text_state(new_text, test_rev(11, 11));
    assert_eq!(tab.doc.text.as_str(), "new content");
    assert!(!tab.doc.dirty && !tab.doc.stats_dirty && !tab.doc.preview_dirty);
    assert!(tab.doc.edit_seq > old_seq);
    assert!(tab.doc.editor_galley_cache.is_none());

    // Successive reloads each advance edit_seq.
    let mut prev_seq = tab.doc.edit_seq;
    for i in 0u64..5 {
        let content = Arc::new(format!("version {i}\n"));
        tab.apply_disk_text_state(content, test_rev(i + 20, 12));
        assert!(tab.doc.edit_seq > prev_seq, "reload {i}");
        prev_seq = tab.doc.edit_seq;
    }

    // Dirty buffer: asked, the edits kept.
    let original = "line one\nline two\n";
    let ours = format!("{original}our addition\n");
    let mut tab = merge_tab(original, &ours, 1, original.len() as u64, true);
    tab.incorporate_disk_text("CHANGED\nline two\n".to_string(), test_rev(2, 18));
    assert!(tab.disk.awaits_answer());
    assert_eq!(tab.doc.text.as_str(), ours);

    // Disk truncated to empty while dirty: asked, the edits kept.
    let mut tab2 = merge_tab("original\n", "user edits here\n", 1, 9, true);
    tab2.incorporate_disk_text(String::new(), test_rev(2, 0));
    assert!(tab2.disk.awaits_answer());
    assert!(tab2.doc.text.contains("user edits"));

    // Large file reload
    use std::fmt::Write;
    let mut tab = Tab::default();
    let mut large_content = String::new();
    for i in 0..50_000 {
        writeln!(large_content, "line {i}").unwrap_or_default();
    }
    assert!(large_content.len() > 400_000);
    let text = Arc::new(large_content.clone());
    tab.apply_disk_text_state(text, test_rev(1, large_content.len() as u64));
    assert_eq!(tab.doc.text.as_str(), large_content.as_str());
    assert_eq!(tab.doc.base_text.as_str(), large_content.as_str());
    assert!(!tab.doc.dirty);
    assert!(
        tab.doc.stats.lines > 49_000,
        "stats should reflect large file"
    );
}

// ---------------------------------------------------------------
// Bundled document tests
// ---------------------------------------------------------------

#[test]
fn bundled_docs_are_parseable_and_loadable() {
    // Both bundled documents must be parseable.
    for (doc, min_len, min_events, heading_pat) in [
        (BundledDoc::Demo, 500, 50, "# "),
        (BundledDoc::Verification, 10_000, 500, "# 1 "),
    ] {
        let content = doc.content();
        assert!(content.len() > min_len);
        assert!(content.contains(heading_pat));
        assert!(pulldown_cmark::Parser::new(content).count() > min_events);
        if matches!(doc, BundledDoc::Verification) {
            let headings = nav::outline::extract_headings(content);
            assert_eq!(
                headings.first().map(|heading| heading.label(content)),
                Some("🔬 Rustdown Verification Document"),
                "verification doc should preserve its emoji-led title"
            );
        }
    }

    // Loading creates pathless clean document and advances edit_seq.
    let tab = Tab::from_bundled(BundledDoc::Demo);
    assert!(tab.doc.path.is_none() && !tab.doc.dirty);
    assert!(tab.doc.text.len() > 500);
    assert_eq!(tab.doc.text.as_str(), tab.doc.base_text.as_str());
    assert!(tab.doc.edit_seq > 0);

    // Opening it replaces an untouched Untitled tab.
    let mut app = RustdownApp::default();
    app.open_bundled(BundledDoc::Demo);
    assert_eq!(app.tabs.len(), 1);
    assert!(app.tab().doc.text.len() > 500);
}

// ---------------------------------------------------------------
// Auto-save unit tests
// ---------------------------------------------------------------

/// Build an app with a dirty document backed by a real temp file.
fn auto_save_tab(dir: &Path) -> Tab {
    let path = dir.join("auto.md");
    let _ = atomic_write_utf8(&path, "base content\n");
    let rev = disk_revision(&path).ok();
    let mut tab = Tab::default();
    tab.doc.path = Some(path);
    tab.doc.text = Arc::new("base content\nedited\n".to_owned());
    tab.doc.base_text = Arc::new("base content\n".to_owned());
    tab.doc.disk_rev = rev;
    tab.doc.dirty = true;
    tab
}

#[test]
fn auto_save_fires_after_interval_and_writes_file() {
    let dir = make_temp_dir("rustdown-autosave-fires");
    let ctx = warm_ctx();
    let mut tab = auto_save_tab(&dir);
    let path = tab.doc.path.clone().unwrap_or_else(|| unreachable!());

    // Set due_at to the past so it fires immediately.
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.tick_auto_save(&ctx, true);

    assert!(!tab.doc.dirty, "doc should be clean after auto-save");
    assert!(
        tab.auto_save_due_at.is_none(),
        "due_at cleared after success"
    );
    let on_disk = read_file(&path);
    assert_eq!(on_disk, "base content\nedited\n");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn auto_save_skips_when_not_dirty() {
    let dir = make_temp_dir("rustdown-autosave-clean");
    let ctx = warm_ctx();
    let mut tab = auto_save_tab(&dir);
    tab.doc.dirty = false;
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));

    tab.tick_auto_save(&ctx, true);

    // due_at should be cleared (not dirty → nothing to do).
    assert!(tab.auto_save_due_at.is_none());
    // File should still have original content.
    let path = tab.doc.path.clone().unwrap_or_else(|| unreachable!());
    assert_eq!(read_file(&path), "base content\n");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn auto_save_skips_untitled_documents() {
    let ctx = warm_ctx();
    let mut tab = Tab::default();
    tab.doc.text = Arc::new("some text".to_owned());
    tab.doc.dirty = true;
    // No path set.
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));

    tab.tick_auto_save(&ctx, true);

    // due_at cleared (no path → nothing to save).
    assert!(tab.auto_save_due_at.is_none());
}

#[test]
fn auto_save_skips_when_disabled() {
    let dir = make_temp_dir("rustdown-autosave-disabled");
    let ctx = warm_ctx();
    let mut tab = auto_save_tab(&dir);
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));

    tab.tick_auto_save(&ctx, false);

    // due_at cleared (disabled → no timer).
    assert!(tab.auto_save_due_at.is_none());
    assert!(tab.doc.dirty, "doc should still be dirty");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn auto_save_skips_when_conflict_pending() {
    let dir = make_temp_dir("rustdown-autosave-conflict");
    let ctx = warm_ctx();
    let mut tab = auto_save_tab(&dir);
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.disk.pending_change = Some(PendingDiskChange {
        disk_text: "d".into(),
        disk_rev: test_rev(1, 1),
    });

    tab.tick_auto_save(&ctx, true);

    // due_at preserved (conflict blocks save, but timer stays for retry).
    assert!(tab.auto_save_due_at.is_some());
    assert!(tab.doc.dirty);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn auto_save_skips_when_reload_in_flight() {
    let dir = make_temp_dir("rustdown-autosave-inflight");
    let ctx = warm_ctx();
    let mut tab = auto_save_tab(&dir);
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.disk.reload_in_flight = true;

    tab.tick_auto_save(&ctx, true);

    // due_at preserved (reload in flight blocks save, timer stays).
    assert!(tab.auto_save_due_at.is_some());
    assert!(tab.doc.dirty);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn auto_save_schedules_due_at_on_first_dirty_tick() {
    let dir = make_temp_dir("rustdown-autosave-schedule");
    let ctx = warm_ctx();
    let mut tab = auto_save_tab(&dir);
    assert!(tab.auto_save_due_at.is_none());

    let before = Instant::now();
    tab.tick_auto_save(&ctx, true);
    let after = Instant::now();

    // Should have scheduled a future auto-save.
    assert!(tab.auto_save_due_at.is_some());
    let due = tab.auto_save_due_at.unwrap_or_else(|| unreachable!());
    assert!(due >= before + AUTO_SAVE_INTERVAL);
    assert!(due <= after + AUTO_SAVE_INTERVAL);
    // Doc still dirty (interval hasn't elapsed yet).
    assert!(tab.doc.dirty);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn auto_save_retries_on_failure() {
    let ctx = warm_ctx();
    let mut tab = Tab::default();
    // Point to a path whose parent directory doesn't exist → write will fail.
    let dir = make_temp_dir("rustdown-autosave-retry");
    let bad_path = dir.join("no_such_parent").join("file.md");
    tab.doc.path = Some(bad_path);
    tab.doc.text = Arc::new("content".to_owned());
    tab.doc.base_text = Arc::new(String::new());
    tab.doc.dirty = true;
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));

    let before = Instant::now();
    tab.tick_auto_save(&ctx, true);

    // Save failed → doc still dirty, due_at rescheduled with retry delay.
    assert!(tab.doc.dirty);
    assert!(tab.auto_save_due_at.is_some());
    let retry = tab.auto_save_due_at.unwrap_or_else(|| unreachable!());
    assert!(retry >= before + AUTO_SAVE_RETRY);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn manual_save_clears_auto_save_timer() {
    let dir = make_temp_dir("rustdown-autosave-manual-clears");
    let ctx = warm_ctx();
    let mut tab = auto_save_tab(&dir);

    // Schedule auto-save for the future.
    tab.auto_save_due_at = Some(Instant::now() + Duration::from_secs(100));

    // Manual save.
    assert!(tab.save_doc(false));
    assert!(!tab.doc.dirty);

    // Next tick should clear due_at since doc is no longer dirty.
    tab.tick_auto_save(&ctx, true);
    assert!(tab.auto_save_due_at.is_none());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn auto_save_preference_persisted_in_round_trip() {
    let mut app = RustdownApp::default();
    app.auto_save = false;
    // Build the prefs struct the same way save_preferences_with_zoom does.
    let prefs = preferences::UserPreferences {
        nav_visible: app.nav_visible,
        heading_color_mode: app.heading_color_mode,
        side_by_side_scroll_sync: app.side_by_side_scroll_sync,
        zoom_factor: 1.0,
        mode: app.mode.as_str().to_owned(),
        auto_save: app.auto_save,
        ..preferences::UserPreferences::default()
    };
    let serialized = toml::to_string_pretty(&prefs).unwrap_or_default();
    assert!(serialized.contains("auto_save = false"));
    let loaded: preferences::UserPreferences = toml::from_str(&serialized).unwrap_or_default();
    assert!(!loaded.auto_save);
}

// ---------------------------------------------------------------
// drain_disk_read_results unit tests
// ---------------------------------------------------------------

/// Set up an app with a read channel and send a message into it.
fn tab_with_read_message(msg: DiskReadMessage) -> Tab {
    let mut tab = Tab::default();
    tab.doc.path = Some(msg.path.clone());
    tab.disk.reload_nonce = msg.nonce;
    tab.doc.edit_seq = msg.edit_seq;
    tab.disk.reload_in_flight = true;
    let (tx, rx) = mpsc::channel();
    tx.send(msg).ok();
    tab.disk.read_tx = Some(tx);
    tab.disk.read_rx = Some(rx);
    tab
}

#[test]
fn drain_results_clean_replace() {
    let rev = test_rev(5, 10);
    let msg = DiskReadMessage {
        path: PathBuf::from("note.md"),
        nonce: 1,
        edit_seq: 0,
        outcome: Ok(DiskReloadOutcome::Replace {
            disk_text: "new content".into(),
            disk_rev: rev,
        }),
    };
    let mut tab = tab_with_read_message(msg);
    tab.drain_disk_read_results();

    assert_eq!(tab.doc.text.as_str(), "new content");
    assert_eq!(tab.doc.base_text.as_str(), "new content");
    assert_eq!(tab.doc.disk_rev, Some(rev));
    assert!(!tab.doc.dirty);
    assert!(!tab.disk.reload_in_flight);
}

#[test]
fn drain_results_changed_asks_first() {
    let rev = test_rev(5, 20);
    let msg = DiskReadMessage {
        path: PathBuf::from("note.md"),
        nonce: 1,
        edit_seq: 0,
        outcome: Ok(DiskReloadOutcome::Changed {
            disk_text: "disk version".into(),
            disk_rev: rev,
        }),
    };
    let mut tab = tab_with_read_message(msg);
    let before = tab.doc.text.clone();
    tab.drain_disk_read_results();

    // Nothing changes until the user answers.
    assert_eq!(tab.doc.text, before);
    assert!(tab.disk.awaits_answer());
    assert!(!tab.disk.reload_in_flight);
}

#[test]
fn a_late_notice_of_an_unchanged_file_asks_nothing() {
    // The file read back is what the edits are already compared with (a
    // second notice of the same save, or one after an answer).
    let rev = test_rev(6, 20);
    let msg = DiskReadMessage {
        path: PathBuf::from("note.md"),
        nonce: 1,
        edit_seq: 0,
        outcome: Ok(DiskReloadOutcome::Changed {
            disk_text: "theirs".into(),
            disk_rev: rev,
        }),
    };
    let mut tab = tab_with_read_message(msg);
    tab.doc.text = Arc::new("mine".to_owned());
    tab.doc.base_text = Arc::new("theirs".to_owned());
    tab.doc.dirty = true;
    tab.drain_disk_read_results();
    assert!(!tab.disk.awaits_answer());
    assert_eq!(tab.doc.text.as_str(), "mine");
    assert_eq!(tab.doc.disk_rev, Some(rev));
    assert!(tab.doc.dirty);
}

#[test]
fn drain_results_stale_nonce_ignored() {
    let rev = test_rev(5, 10);
    let msg = DiskReadMessage {
        path: PathBuf::from("note.md"),
        nonce: 1,
        edit_seq: 0,
        outcome: Ok(DiskReloadOutcome::Replace {
            disk_text: "stale".into(),
            disk_rev: rev,
        }),
    };
    let mut tab = tab_with_read_message(msg);
    // Bump nonce so the message is stale.
    tab.disk.reload_nonce = 99;
    tab.drain_disk_read_results();

    // Text unchanged — stale message was ignored.
    assert_eq!(tab.doc.text.as_str(), "");
    assert!(tab.disk.reload_in_flight, "still in flight (stale ignored)");
}

#[test]
fn drain_results_path_mismatch_ignored() {
    let rev = test_rev(5, 10);
    let msg = DiskReadMessage {
        path: PathBuf::from("note.md"),
        nonce: 1,
        edit_seq: 0,
        outcome: Ok(DiskReloadOutcome::Replace {
            disk_text: "wrong doc".into(),
            disk_rev: rev,
        }),
    };
    let mut tab = tab_with_read_message(msg);
    // Change doc path so the message doesn't match.
    tab.doc.path = Some(PathBuf::from("other.md"));
    tab.drain_disk_read_results();

    assert_eq!(tab.doc.text.as_str(), "");
    assert!(tab.disk.reload_in_flight);
}

#[test]
fn drain_results_edit_seq_mismatch_reschedules() {
    let rev = test_rev(5, 10);
    let msg = DiskReadMessage {
        path: PathBuf::from("note.md"),
        nonce: 1,
        edit_seq: 0,
        outcome: Ok(DiskReloadOutcome::Replace {
            disk_text: "outdated".into(),
            disk_rev: rev,
        }),
    };
    let mut tab = tab_with_read_message(msg);
    // Simulate user editing after reload started.
    tab.doc.edit_seq = 5;
    tab.drain_disk_read_results();

    // Text unchanged — edit_seq mismatch triggered reschedule.
    assert_eq!(tab.doc.text.as_str(), "");
    assert!(!tab.disk.reload_in_flight, "cleared on seq mismatch");
    assert!(tab.disk.pending_reload_at.is_some(), "reload rescheduled");
}

#[test]
fn drain_results_io_error_sets_error() {
    let msg = DiskReadMessage {
        path: PathBuf::from("note.md"),
        nonce: 1,
        edit_seq: 0,
        outcome: Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file gone",
        )),
    };
    let mut tab = tab_with_read_message(msg);
    tab.drain_disk_read_results();

    assert!(tab.error.is_some());
    assert!(
        tab.error
            .as_ref()
            .unwrap_or_else(|| unreachable!())
            .contains("file gone")
    );
    assert!(!tab.disk.reload_in_flight);
}

#[test]
fn drain_results_channel_disconnect_clears_channels() {
    let mut tab = Tab::default();
    tab.doc.path = Some(PathBuf::from("note.md"));
    tab.disk.reload_in_flight = true;

    // Create channel then immediately drop the sender.
    let (tx, rx) = mpsc::channel::<DiskReadMessage>();
    drop(tx);
    tab.disk.read_rx = Some(rx);

    tab.drain_disk_read_results();

    assert!(!tab.disk.reload_in_flight);
    assert!(tab.disk.read_rx.is_none());
    assert!(tab.disk.read_tx.is_none());
}

#[test]
fn drain_results_multiple_messages_processes_matching() {
    let rev1 = test_rev(1, 5);
    let rev2 = test_rev(2, 10);
    let (tx, rx) = mpsc::channel();

    // First message: stale nonce → ignored.
    tx.send(DiskReadMessage {
        path: PathBuf::from("note.md"),
        nonce: 1,
        edit_seq: 0,
        outcome: Ok(DiskReloadOutcome::Replace {
            disk_text: "stale".into(),
            disk_rev: rev1,
        }),
    })
    .ok();
    // Second message: matching nonce → applied.
    tx.send(DiskReadMessage {
        path: PathBuf::from("note.md"),
        nonce: 2,
        edit_seq: 0,
        outcome: Ok(DiskReloadOutcome::Replace {
            disk_text: "current".into(),
            disk_rev: rev2,
        }),
    })
    .ok();

    let mut tab = Tab::default();
    tab.doc.path = Some(PathBuf::from("note.md"));
    tab.disk.reload_nonce = 2;
    tab.doc.edit_seq = 0;
    tab.disk.reload_in_flight = true;
    tab.disk.read_tx = Some(tx);
    tab.disk.read_rx = Some(rx);

    tab.drain_disk_read_results();

    assert_eq!(tab.doc.text.as_str(), "current");
    assert!(!tab.disk.reload_in_flight);
}

// ---------------------------------------------------------------
// Integration tests — auto-save with real files + live editing
// ---------------------------------------------------------------

#[test]
fn integration_auto_save_writes_dirty_content_to_disk() {
    let dir = make_temp_dir("rustdown-int-autosave-write");
    let path = dir.join("doc.md");
    let _ = atomic_write_utf8(&path, "original\n");
    let ctx = warm_ctx();

    let mut tab = Tab::default();
    tab.load_document(
        path.clone(),
        "original\n".to_owned(),
        disk_revision(&path).ok(),
    );

    // Simulate user edit.
    tab.doc.text = Arc::new("original\nnew line\n".to_owned());
    tab.note_text_changed(false);

    // Force auto-save to fire.
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.tick_auto_save(&ctx, true);

    assert!(!tab.doc.dirty);
    assert_eq!(read_file(&path), "original\nnew line\n");
    assert_eq!(tab.doc.base_text.as_str(), "original\nnew line\n");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn integration_external_change_asks_on_auto_save() {
    let dir = make_temp_dir("rustdown-int-autosave-merge");
    let path = dir.join("doc.md");
    let _ = atomic_write_utf8(&path, "line1\nline2\nline3\n");
    let ctx = warm_ctx();

    let mut tab = Tab::default();
    tab.load_document(
        path.clone(),
        "line1\nline2\nline3\n".to_owned(),
        disk_revision(&path).ok(),
    );

    // User edits line 3.
    tab.doc.text = Arc::new("line1\nline2\nuser-edited\n".to_owned());
    tab.note_text_changed(false);

    // External process edits line 1.
    let _ = atomic_write_utf8(&path, "external\nline2\nline3\n");

    // Auto-save triggers → pre-save reload sees the change → asks.
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.tick_auto_save(&ctx, true);

    assert!(tab.disk.awaits_answer());
    assert!(tab.doc.dirty);
    assert_eq!(read_file(&path), "external\nline2\nline3\n");
    assert_eq!(tab.doc.text.as_str(), "line1\nline2\nuser-edited\n");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn integration_auto_save_blocked_by_conflict_then_resumes() {
    let dir = make_temp_dir("rustdown-int-autosave-blocked");
    let path = dir.join("doc.md");
    let _ = atomic_write_utf8(&path, "shared line\n");
    let ctx = warm_ctx();

    let mut tab = Tab::default();
    tab.load_document(
        path.clone(),
        "shared line\n".to_owned(),
        disk_revision(&path).ok(),
    );

    // Both user and external edit the same line → asked.
    tab.doc.text = Arc::new("user version\n".to_owned());
    tab.note_text_changed(false);
    let _ = atomic_write_utf8(&path, "external version\n");

    // Auto-save triggers, pre-save reload asks.
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.tick_auto_save(&ctx, true);

    assert!(tab.disk.awaits_answer());
    // The question blocks further auto-saves.
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.tick_auto_save(&ctx, true);
    assert!(tab.disk.awaits_answer(), "question still pending");

    // Ignore: auto-save then replaces the file with the user's text.
    tab.apply_disk_change_choice(DiskChangeChoice::Ignore);
    assert!(!tab.disk.awaits_answer());
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.tick_auto_save(&ctx, true);
    assert!(!tab.doc.dirty, "saved after the answer");
    assert_eq!(read_file(&path), "user version\n");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn integration_successive_auto_saves_track_base_text() {
    let dir = make_temp_dir("rustdown-int-autosave-successive");
    let path = dir.join("doc.md");
    let _ = atomic_write_utf8(&path, "v1\n");
    let ctx = warm_ctx();

    let mut tab = Tab::default();
    tab.load_document(path.clone(), "v1\n".to_owned(), disk_revision(&path).ok());

    // First edit + auto-save.
    tab.doc.text = Arc::new("v2\n".to_owned());
    tab.note_text_changed(false);
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.tick_auto_save(&ctx, true);
    assert_eq!(tab.doc.base_text.as_str(), "v2\n");
    assert_eq!(read_file(&path), "v2\n");

    // Second edit + auto-save.
    tab.doc.text = Arc::new("v3\n".to_owned());
    tab.note_text_changed(false);
    tab.auto_save_due_at = Some(Instant::now() - Duration::from_secs(1));
    tab.tick_auto_save(&ctx, true);
    assert_eq!(tab.doc.base_text.as_str(), "v3\n");
    assert_eq!(read_file(&path), "v3\n");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn integration_load_document_clears_auto_save_timer() {
    let dir = make_temp_dir("rustdown-int-load-clears-timer");
    let path = dir.join("a.md");
    let _ = atomic_write_utf8(&path, "aaa");
    let ctx = warm_ctx();

    let mut tab = Tab::default();
    tab.load_document(path.clone(), "aaa".to_owned(), disk_revision(&path).ok());
    tab.doc.text = Arc::new("edited".to_owned());
    tab.note_text_changed(false);
    tab.auto_save_due_at = Some(Instant::now() + Duration::from_secs(100));

    // Load a different document.
    let path2 = dir.join("b.md");
    let _ = atomic_write_utf8(&path2, "bbb");
    tab.load_document(path2, "bbb".to_owned(), None);

    // After load, doc is clean → auto-save tick clears the timer.
    tab.tick_auto_save(&ctx, true);
    assert!(tab.auto_save_due_at.is_none());
    assert!(!tab.doc.dirty);

    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------
// Tabs, close prompts and commands
// ---------------------------------------------------------------

/// A clean tab backed by a real file containing `content`.
fn file_tab(dir: &Path, name: &str, content: &str) -> (Tab, PathBuf) {
    let path = dir.join(name);
    let _ = atomic_write_utf8(&path, content);
    let tab = Tab::from_path(path.clone()).unwrap_or_else(|_| unreachable!());
    (tab, path)
}

fn app_with_tabs(tabs: Vec<Tab>) -> RustdownApp {
    let mut app = RustdownApp::default();
    app.tabs = tabs;
    app
}

fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::RawInput {
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

#[test]
fn open_path_reuses_untouched_tab_and_focuses_already_open_file() {
    let dir = make_temp_dir("rustdown-tabs-open");
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    let _ = atomic_write_utf8(&a, "# A\n");
    let _ = atomic_write_utf8(&b, "# B\n");

    // The initial untouched Untitled tab is replaced.
    let mut app = RustdownApp::default();
    let untitled_id = app.tab().id;
    app.open_path(a);
    assert_eq!(app.tabs.len(), 1);
    assert_ne!(app.tab().id, untitled_id);
    assert_eq!(app.tab().doc.text.as_str(), "# A\n");

    // A second file gets its own tab.
    app.open_path(b.clone());
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active, 1);

    // Re-opening `a` via a different spelling focuses its tab.
    app.open_path(dir.join(".").join("a.md"));
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active, 0);
    assert!(app.scroll_to_active_tab && app.focus_editor);

    // An edited Untitled tab is not reused.
    app.new_tab();
    app.tab_mut().doc.text = Arc::new("draft".to_owned());
    app.tab_mut().note_text_changed(false);
    app.open_path(b);
    assert_eq!(app.tabs.len(), 3, "focused existing tab instead");
    assert_eq!(app.active, 1);

    // New tab always appends, even when the active tab is untouched.
    let mut app = RustdownApp::default();
    app.new_tab();
    assert_eq!((app.tabs.len(), app.active), (2, 1));

    // Open failures stay on the active tab's error banner.
    let mut app = RustdownApp::default();
    app.open_path(dir.clone());
    assert_eq!(app.tabs.len(), 1);
    assert!(
        app.tab()
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("Open failed"))
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn closing_tabs_moves_focus_and_last_tab_closes_window() {
    let ctx = egui::Context::default();
    let mut app = app_with_tabs((0..4).map(|_| Tab::default()).collect());
    // Settings → Close window with last tab (off by default).
    app.execute(AppCommand::ToggleCloseWindowWithLastTab, &ctx);
    assert!(app.close_window_with_last_tab);
    let ids: Vec<TabId> = app.tabs.iter().map(|tab| tab.id).collect();

    // Closing the active tab activates its right-hand neighbour.
    app.activate_tab(1);
    app.execute(AppCommand::CloseActiveTab, &ctx);
    assert_eq!(app.tabs.len(), 3);
    assert_eq!(app.tab().id, ids[2]);

    // Closing a tab left of the active one keeps the same tab active.
    app.execute(AppCommand::CloseTab(ids[0]), &ctx);
    assert_eq!(app.tab().id, ids[2]);

    // Closing the last (rightmost) active tab activates the new last tab.
    app.activate_tab(1);
    app.execute(AppCommand::CloseActiveTab, &ctx);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tab().id, ids[2]);
    assert!(!app.window_close_pending);

    // Closing the only tab closes the window instead; `tabs` never empties.
    app.execute(AppCommand::CloseActiveTab, &ctx);
    assert_eq!(app.tabs.len(), 1);
    assert!(app.window_close_pending);
}

#[test]
fn dragging_a_tab_moves_it_and_the_active_tab_stays_active() {
    let ctx = egui::Context::default();
    let mut app = app_with_tabs((0..4).map(|_| Tab::default()).collect());
    let ids: Vec<TabId> = app.tabs.iter().map(|tab| tab.id).collect();
    app.activate_tab(1);
    app.execute(AppCommand::MoveTab { id: ids[0], to: 3 }, &ctx);
    let order: Vec<TabId> = app.tabs.iter().map(|tab| tab.id).collect();
    assert_eq!(order, [ids[1], ids[2], ids[3], ids[0]]);
    assert_eq!(app.tab().id, ids[1], "the active tab stays active");
    // Past the end is the end; an unknown tab does nothing.
    app.execute(AppCommand::MoveTab { id: ids[1], to: 99 }, &ctx);
    assert_eq!(app.tabs.last().map(|tab| tab.id), Some(ids[1]));
    app.execute(AppCommand::MoveTab { id: 0, to: 0 }, &ctx);
    assert_eq!(app.tabs.len(), 4);
}

#[test]
fn recent_files_keep_the_newest_first_without_repeats() {
    let mut app = RustdownApp::default();
    for name in ["a.md", "b.md", "a.md"] {
        app.note_recent_file(Path::new(name));
    }
    assert_eq!(
        app.recent_files,
        [PathBuf::from("a.md"), PathBuf::from("b.md")]
    );
    for index in 0..20 {
        app.note_recent_file(&PathBuf::from(format!("{index}.md")));
    }
    assert_eq!(app.recent_files.len(), 10);
    assert_eq!(app.preferences().recent_files.len(), 10);
}

#[test]
fn closing_the_last_tab_can_keep_the_window_open() {
    let ctx = egui::Context::default();
    let mut app = app_with_tabs(vec![Tab::default()]);
    app.tab_mut().doc.path = Some(PathBuf::from("notes.md"));
    // The default: closing the last tab keeps the window.
    assert!(!app.close_window_with_last_tab);
    let old = app.tab().id;

    // An empty Untitled tab takes the closed one's place.
    app.execute(AppCommand::CloseActiveTab, &ctx);
    assert!(!app.window_close_pending);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active, 0);
    assert_ne!(app.tab().id, old);
    assert!(app.tab().doc.text.is_empty() && app.tab().doc.path.is_none());
}

#[test]
fn tab_cycling_wraps_and_commands_dispatch() {
    let ctx = egui::Context::default();
    let mut app = app_with_tabs((0..3).map(|_| Tab::default()).collect());
    app.execute(AppCommand::PrevTab, &ctx);
    assert_eq!(app.active, 2);
    app.execute(AppCommand::NextTab, &ctx);
    assert_eq!(app.active, 0);
    let id = app.tabs[1].id;
    app.execute(AppCommand::ActivateTab(id), &ctx);
    assert_eq!(app.active, 1);

    app.execute(AppCommand::NewTab, &ctx);
    assert_eq!((app.tabs.len(), app.active), (4, 3));

    app.execute(AppCommand::Replace, &ctx);
    assert!(app.search.visible && app.search.replace_mode);
    app.execute(AppCommand::CloseSearch, &ctx);
    assert!(!app.search.visible);

    let nav = app.nav_visible;
    app.execute(AppCommand::ToggleOutline, &ctx);
    assert_eq!(app.nav_visible, !nav);

    app.execute(AppCommand::SetMode(Mode::Preview), &ctx);
    assert_eq!(app.mode, Mode::Preview);
    app.execute(AppCommand::CycleMode, &ctx);
    assert_eq!(app.mode, Mode::Edit);
    app.execute(AppCommand::CycleMode, &ctx);
    assert_eq!(app.mode, Mode::Raw);

    app.execute(AppCommand::OpenBundled(BundledDoc::Demo), &ctx);
    assert!(app.tab().doc.text.len() > 500 && app.tab().doc.path.is_none());

    app.execute(AppCommand::ShowAbout, &ctx);
    assert!(app.show_about);

    // Exit with nothing unsaved closes straight away.
    app.execute(AppCommand::Exit, &ctx);
    assert!(app.window_close_pending && app.close_prompt.is_none());
}

#[test]
fn keyboard_shortcuts_drive_tabs_and_are_consumed() {
    let ctx = warm_ctx();
    let mut app = app_with_tabs((0..3).map(|_| Tab::default()).collect());
    let cmd = egui::Modifiers::COMMAND;
    let cmd_shift = cmd.plus(egui::Modifiers::SHIFT);
    let press = |app: &mut RustdownApp, key, modifiers| {
        let mut leftover = 0;
        let _ = ctx.run(key_event(key, modifiers), |ctx| {
            app.handle_keyboard_shortcuts(ctx);
            leftover = ctx.input(|i| i.events.len());
        });
        leftover
    };

    assert_eq!(press(&mut app, egui::Key::Tab, cmd), 0, "Ctrl+Tab consumed");
    assert_eq!(app.active, 1);
    press(&mut app, egui::Key::Tab, cmd_shift);
    assert_eq!(app.active, 0);
    press(&mut app, egui::Key::PageUp, cmd);
    assert_eq!(app.active, 2);
    press(&mut app, egui::Key::PageDown, cmd);
    assert_eq!(app.active, 0);

    // Ctrl+T opens a tab; Ctrl+Shift+T toggles the outline instead.
    press(&mut app, egui::Key::T, cmd);
    assert_eq!(app.tabs.len(), 4);
    let nav = app.nav_visible;
    press(&mut app, egui::Key::T, cmd_shift);
    assert_eq!(app.tabs.len(), 4);
    assert_eq!(app.nav_visible, !nav);

    press(&mut app, egui::Key::W, cmd);
    assert_eq!(app.tabs.len(), 3);

    // Shortcuts are ignored while a prompt is open.
    app.tab_mut().doc.dirty = true;
    press(&mut app, egui::Key::W, cmd);
    assert!(app.close_prompt.is_some());
    press(&mut app, egui::Key::N, cmd);
    assert_eq!(app.tabs.len(), 3);
}

#[test]
fn close_prompt_for_single_tab_save_dont_save_and_cancel() {
    let dir = make_temp_dir("rustdown-close-prompt-tab");
    let (mut tab_a, path_a) = file_tab(&dir, "a.md", "a\n");
    tab_a.doc.text = Arc::new("a edited\n".to_owned());
    tab_a.note_text_changed(false);
    let mut tab_b = Tab::default();
    tab_b.doc.text = Arc::new("scratch".to_owned());
    tab_b.note_text_changed(false);
    let (tab_c, _) = file_tab(&dir, "c.md", "c\n");
    let (id_a, id_b) = (tab_a.id, tab_b.id);
    let mut app = app_with_tabs(vec![tab_a, tab_b, tab_c]);
    app.activate_tab(2);

    // Cancel keeps the dirty tab.
    app.request_close_tab(id_b);
    assert_eq!(app.tab().id, id_b, "prompted tab is activated");
    assert_eq!(
        app.close_prompt,
        Some(ClosePrompt {
            queue: vec![id_b],
            closing_window: false,
        })
    );
    app.resolve_close_prompt(CloseChoice::Cancel);
    assert!(app.close_prompt.is_none());
    assert_eq!(app.tabs.len(), 3);

    // Don't save discards and closes.
    app.request_close_tab(id_b);
    app.resolve_close_prompt(CloseChoice::DontSave);
    assert!(app.close_prompt.is_none());
    assert!(app.tab_index(id_b).is_none());
    assert_eq!(app.tabs.len(), 2);

    // Save writes the file, then closes.
    app.request_close_tab(id_a);
    app.resolve_close_prompt(CloseChoice::Save);
    assert_eq!(read_file(&path_a), "a edited\n");
    assert!(app.tab_index(id_a).is_none());
    assert!(!app.window_close_pending);

    // A failed save cancels the close and keeps the tab.
    let mut app = RustdownApp::default();
    app.tab_mut().doc.path = Some(dir.join("missing-dir").join("x.md"));
    app.tab_mut().doc.dirty = true;
    let id = app.tab().id;
    app.request_close_tab(id);
    app.resolve_close_prompt(CloseChoice::Save);
    assert!(app.close_prompt.is_none());
    assert!(app.tab_index(id).is_some() && app.tab().doc.dirty);
    assert!(app.tab().error.is_some());
    assert!(!app.window_close_pending);

    // Clean tabs close without a prompt.
    let mut app = app_with_tabs(vec![Tab::default(), Tab::default()]);
    let id = app.tab().id;
    app.request_close_tab(id);
    assert!(app.close_prompt.is_none());
    assert_eq!(app.tabs.len(), 1);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn window_close_prompts_each_dirty_tab_in_turn() {
    let dir = make_temp_dir("rustdown-close-prompt-window");
    let (clean, _) = file_tab(&dir, "clean.md", "clean\n");
    let (mut dirty_a, path_a) = file_tab(&dir, "a.md", "a\n");
    dirty_a.doc.text = Arc::new("a2\n".to_owned());
    dirty_a.note_text_changed(false);
    let (mut dirty_b, path_b) = file_tab(&dir, "b.md", "b\n");
    dirty_b.doc.text = Arc::new("b2\n".to_owned());
    dirty_b.note_text_changed(false);
    let (id_a, id_b) = (dirty_a.id, dirty_b.id);
    let new_app = |a: Tab, b: Tab, c: Tab| app_with_tabs(vec![c, a, b]);

    let mut app = new_app(dirty_a, dirty_b, clean);
    assert!(!app.begin_window_close());
    assert_eq!(app.tab().id, id_a, "first dirty tab is activated");
    assert_eq!(
        app.close_prompt.as_ref().map(|p| p.queue.clone()),
        Some(vec![id_a, id_b])
    );

    // Save the first, discard the second → window closes, nothing removed.
    app.resolve_close_prompt(CloseChoice::Save);
    assert_eq!(read_file(&path_a), "a2\n");
    assert_eq!(app.tab().id, id_b, "next dirty tab is activated");
    assert!(app.close_prompt.is_some() && !app.window_close_pending);
    app.resolve_close_prompt(CloseChoice::DontSave);
    assert!(app.close_prompt.is_none());
    assert!(app.window_close_pending);
    assert_eq!(app.tabs.len(), 3);
    assert_eq!(read_file(&path_b), "b\n");

    // Cancel part-way keeps every tab and its edits.
    let mut tabs = std::mem::take(&mut app.tabs);
    let mut app = RustdownApp::default();
    for tab in &mut tabs {
        tab.doc.dirty = true;
    }
    app.tabs = tabs;
    app.request_exit();
    assert!(app.close_prompt.is_some());
    app.resolve_close_prompt(CloseChoice::DontSave);
    app.resolve_close_prompt(CloseChoice::Cancel);
    assert!(app.close_prompt.is_none() && !app.window_close_pending);
    assert_eq!(app.tabs.len(), 3);
    assert!(app.tabs.iter().all(|tab| tab.doc.dirty));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn os_close_request_is_cancelled_while_tabs_are_dirty() {
    let ctx = warm_ctx();
    let close_input = || {
        let mut raw = egui::RawInput::default();
        raw.viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        raw
    };
    let cancelled = |output: &egui::FullOutput| {
        output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.contains(&egui::ViewportCommand::CancelClose))
    };

    // Dirty → cancelled and prompted.
    let mut app = RustdownApp::default();
    app.tab_mut().doc.dirty = true;
    let output = ctx.run(close_input(), |ctx| app.handle_close_request(ctx));
    assert!(cancelled(&output));
    assert!(app.close_prompt.as_ref().is_some_and(|p| p.closing_window));

    // After the user answers, the deferred close goes through.
    app.resolve_close_prompt(CloseChoice::DontSave);
    let output = ctx.run(egui::RawInput::default(), |ctx| app.flush_window_close(ctx));
    let closed = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .is_some_and(|vp| vp.commands.contains(&egui::ViewportCommand::Close));
    assert!(closed);
    let output = ctx.run(close_input(), |ctx| app.handle_close_request(ctx));
    assert!(!cancelled(&output));

    // Clean → not cancelled.
    let mut app = RustdownApp::default();
    let output = ctx.run(close_input(), |ctx| app.handle_close_request(ctx));
    assert!(!cancelled(&output));
    assert!(app.close_prompt.is_none());
}

#[test]
fn disk_sync_reloads_background_tabs() {
    let dir = make_temp_dir("rustdown-tabs-disk-sync");
    let ctx = warm_ctx();
    let (active, _) = file_tab(&dir, "active.md", "active\n");
    let (clean, clean_path) = file_tab(&dir, "clean.md", "one\n");
    let (mut dirty, dirty_path) = file_tab(&dir, "dirty.md", "shared\n");
    dirty.doc.text = Arc::new("mine\n".to_owned());
    dirty.note_text_changed(false);
    let mut app = app_with_tabs(vec![active, clean, dirty]);

    // Every file-backed tab gets its own watcher (or poll fallback).
    app.tick_disk_sync(&ctx);
    assert!(
        app.tabs
            .iter()
            .all(|tab| tab.disk.watcher.is_some() || tab.disk.poll_at.is_some())
    );

    // External edits to both background tabs.
    let _ = atomic_write_utf8(&clean_path, "two\n");
    let _ = atomic_write_utf8(&dirty_path, "theirs\n");
    for tab in &mut app.tabs[1..] {
        tab.schedule_disk_reload(Instant::now() - Duration::from_secs(1));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while (app.tabs[1].doc.text.as_str() != "two\n" || !app.tabs[2].disk.awaits_answer())
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
        app.tick_disk_sync(&ctx);
    }

    assert_eq!(app.active, 0, "background reloads do not steal focus");
    // Clean background tab: silent reload.
    assert_eq!(app.tabs[1].doc.text.as_str(), "two\n");
    assert!(!app.tabs[1].doc.dirty);
    // Dirty background tab: the question waits on that tab.
    assert!(app.tabs[2].disk.awaits_answer());
    assert_eq!(app.tabs[2].doc.text.as_str(), "mine\n");
    assert!(!app.dialog_open(), "conflict waits until its tab is active");
    app.activate_tab(2);
    assert!(app.dialog_open());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn search_cache_and_window_title_follow_active_tab() {
    let ctx = egui::Context::default();
    let mut a = Tab::default();
    a.doc.text = Arc::new("a a a".to_owned());
    let mut b = Tab::default();
    b.doc.text = Arc::new("a".to_owned());
    b.doc.edit_seq = a.doc.edit_seq;
    b.doc.path = Some(PathBuf::from("notes.md"));
    let mut app = app_with_tabs(vec![a, b]);
    app.search.query = "a".to_owned();
    assert_eq!(app.active_match_count(), 3);
    app.activate_tab(1);
    assert_eq!(app.active_match_count(), 1, "cache invalidated on switch");

    app.update_viewport_title(&ctx);
    assert_eq!(app.last_viewport_title, "notes.md - MdNotePad");
    app.tab_mut().doc.dirty = true;
    app.update_viewport_title(&ctx);
    assert_eq!(app.last_viewport_title, "*notes.md - MdNotePad");
    app.activate_tab(0);
    app.update_viewport_title(&ctx);
    assert_eq!(app.last_viewport_title, "Untitled - MdNotePad");
}

#[test]
fn each_tab_keeps_its_own_editor_state() {
    let ctx = warm_ctx();
    let mut a = Tab::default();
    a.doc.text = Arc::new("first tab\n".to_owned());
    let mut b = Tab::default();
    b.doc.text = Arc::new("second\n".to_owned());
    assert_ne!(a.editor_id(), b.editor_id());
    let mut app = app_with_tabs(vec![a, b]);
    for index in [0, 1] {
        app.activate_tab(index);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
        });
    }
    // Set a cursor in tab 0's editor; tab 1's state is untouched.
    let id_a = app.tabs[0].editor_id();
    let id_b = app.tabs[1].editor_id();
    let mut state = egui::TextEdit::load_state(&ctx, id_a).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(5),
        )));
    state.store(&ctx, id_a);
    let cursor = |id| {
        egui::TextEdit::load_state(&ctx, id)
            .and_then(|s| s.cursor.char_range())
            .map(|r| r.primary.index)
    };
    assert_eq!(cursor(id_a), Some(5));
    assert_ne!(cursor(id_b), Some(5));
}

#[test]
fn editor_scroll_offset_is_readable_after_render() {
    // Real fonts are needed for the editor galley to have a height.
    let ctx = egui::Context::default();
    assert!(crate::ui_style::configure_fonts(&ctx).is_ok());
    let _ = ctx.run(egui::RawInput::default(), |_| {});
    let mut app = RustdownApp::default();
    app.tab_mut().doc.text = Arc::new("# A\n\ntext\n".repeat(400));
    assert!(app.tab_mut().current_editor_scroll_y(&ctx).is_none());
    let frame = |app: &mut RustdownApp| {
        let _ = ctx.run(crate::diagnostics::diagnostics_raw_input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
        });
    };
    frame(&mut app);
    assert_eq!(app.tab_mut().current_editor_scroll_y(&ctx), Some(0.0));
    app.tab_mut().nav.pending_editor_scroll_y = Some(120.0);
    frame(&mut app);
    frame(&mut app);
    let y = app.tab_mut().current_editor_scroll_y(&ctx);
    assert!(y.is_some_and(|y| (y - 120.0).abs() < 1.0), "{y:?}");
    assert!(
        app.tab_mut()
            .current_scroll_byte_offset(&ctx, Mode::Edit)
            .is_some_and(|byte| byte > 0)
    );
}
