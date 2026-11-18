//! Look and feel: fonts, text sizes and the Notepad-style light / dark
//! themes.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

use eframe::egui::{self, Color32, CornerRadius, Stroke};

// Bundled primary fonts (Source Pro family, OFL-licensed).
// See THIRD-PARTY-NOTICES.md for license details.
const BUNDLED_PROPORTIONAL: &str = "source-sans-3";
const BUNDLED_PROPORTIONAL_DATA: &[u8] =
    include_bytes!("../../../assets/fonts/SourceSans3-Regular.ttf");
const BUNDLED_PROPORTIONAL_BOLD: &str = "source-sans-3-bold";
const BUNDLED_PROPORTIONAL_BOLD_DATA: &[u8] =
    include_bytes!("../../../assets/fonts/SourceSans3-Bold.ttf");
const BUNDLED_MONOSPACE: &str = "source-code-pro";
const BUNDLED_MONOSPACE_DATA: &[u8] =
    include_bytes!("../../../assets/fonts/SourceCodePro-Regular.ttf");
const BUNDLED_MONOSPACE_BOLD: &str = "source-code-pro-bold";
const BUNDLED_MONOSPACE_BOLD_DATA: &[u8] =
    include_bytes!("../../../assets/fonts/SourceCodePro-Bold.ttf");

// Bundled symbol/emoji fallbacks shared by both families.
const BUNDLED_SYMBOL_FALLBACKS: &[(&str, &[u8])] = &[
    (
        "rustdown-symbola-subset",
        include_bytes!("../../../assets/fonts/rustdown-symbola-subset.ttf"),
    ),
    (
        "rustdown-noto-symbols2-subset",
        include_bytes!("../../../assets/fonts/rustdown-noto-symbols2-subset.ttf"),
    ),
];

/// Windows 11 UI font (Segoe UI Variable), the primary proportional font
/// when the system fonts directory has it.  egui draws a variable font's
/// default instance, which is the regular weight at the Text optical size,
/// Notepad's menu and tab font.
const SYSTEM_PROPORTIONAL_VARIABLE: &str = "segoe-ui-variable";
const SYSTEM_PROPORTIONAL_VARIABLE_FILE: &str = "SegUIVar.ttf";
/// Windows UI font (Segoe UI), the primary proportional font on Windows 10
/// and the next fallback on Windows 11.
const SYSTEM_PROPORTIONAL: &str = "segoe-ui";
const SYSTEM_PROPORTIONAL_FILE: &str = "segoeui.ttf";

/// Windows icon fonts, newest first: Segoe Fluent Icons (Windows 11), then
/// Segoe MDL2 Assets (Windows 10).  They map the icons the chrome uses to
/// the same code points (see `icons.rs`).
const ICON_FONTS: [(&str, &str); 2] = [
    ("segoe-fluent-icons", "SegoeIcons.ttf"),
    ("segoe-mdl2-assets", "segmdl2.ttf"),
];
/// Name of [`icon_family`].
const ICON_FAMILY: &str = "rustdown-icons";
/// Windows bold UI font (Segoe UI Bold), the primary font of
/// [`bold_family`] when the system fonts directory has it.
const SYSTEM_PROPORTIONAL_BOLD: &str = "segoe-ui-bold";
const SYSTEM_PROPORTIONAL_BOLD_FILE: &str = "segoeuib.ttf";
/// Name of [`bold_family`].
const BOLD_FAMILY: &str = "rustdown-bold";
/// Name of [`heading_family`].
const HEADING_FAMILY: &str = "rustdown-heading";
/// The Paper theme's heading font (Georgia Bold, in every Windows), loaded
/// only when that theme is chosen.
const SERIF_HEADING: &str = "georgia-bold";
const SERIF_HEADING_FILE: &str = "georgiab.ttf";

/// Notepad's editor font (Consolas), used as the primary monospace font when
/// the system fonts directory has it.
const SYSTEM_MONOSPACE: &str = "consolas";
const SYSTEM_MONOSPACE_FILE: &str = "consola.ttf";

/// Prefix of the system fallback font names (extended glyph coverage:
/// emoji, symbols, CJK).
const SYSTEM_FALLBACK_PREFIX: &str = "rustdown-fallback-";

// System font fallbacks for extended glyph coverage.  On Windows they are
// file names inside the fonts directory; elsewhere (dev hosts only) they are
// absolute paths.
#[cfg(target_os = "windows")]
const FALLBACK_FONT_FILES: &[&str] = &["seguisym.ttf", "seguiemj.ttf", "arial.ttf", "arialuni.ttf"];
#[cfg(target_os = "linux")]
const FALLBACK_FONT_FILES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoEmoji-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
    "/usr/share/fonts/truetype/unifont/unifont.ttf",
];
#[cfg(target_os = "macos")]
const FALLBACK_FONT_FILES: &[&str] = &[
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/Library/Fonts/Arial.ttf",
    "/System/Library/Fonts/Apple Color Emoji.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Supplemental/Symbol.ttf",
];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const FALLBACK_FONT_FILES: &[&str] = &[];

/// The one base text size, in egui points (an egui font size is the em
/// size, so 14 is Segoe UI at 14 px at 100 % display scaling): Notepad's,
/// the Windows 11 Body size, which its menus use.  Menus and their popups,
/// the explorer tree, the outline, dialogs and the find bar use it as is;
/// the document's body text (preview and editor) uses it times the zoom.
pub const TEXT_SIZE: f32 = 14.0;
/// Tab titles, the status bar and other small print: Windows 11's Caption
/// size against its Body size (12 / 14), as in Notepad.
const CAPTION_PER_BODY: (f32, f32) = (12.0, 14.0);
/// Monospace (code, the raw view) per proportional size, so that code
/// looks as big as the text around it: Consolas at this scale has the
/// x-height of Segoe UI Variable / Segoe UI at the base size.  Their
/// x-heights are 0.4902 and 0.5000 em (`fontTools`, see the font report
/// of `.github/workflows/screenshots.yml`); measured in Windows
/// screenshots at 21 px: 10.39 and 10.61 px.  Consolas's capitals stay a
/// little shorter (cap height 0.638 against 0.700 em).  The bundled Source
/// Code Pro and Source Sans 3 share one x-height (0.486 em).
pub const MONOSPACE_SCALE: f32 = 1.02;
const DEFAULT_SCROLL_ANIMATION_POINTS_PER_SECOND: f32 = 1150.0;

/// The text sizes derived from the base size [`TEXT_SIZE`].  The chrome
/// uses them as they are; the document views use them
/// [`zoomed`](Self::zoomed) by the zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSizes {
    /// Body and Button text styles: menus, the explorer tree, dialogs,
    /// the document's body text.  Headings scale it by
    /// [`crate::md::HEADING_FONT_SCALES`].
    pub body: f32,
    /// Small text style: tab titles, the status bar, captions.
    pub small: f32,
    /// Monospace text style: code and the raw view.
    pub monospace: f32,
}

impl TextSizes {
    /// The sizes for the base size [`TEXT_SIZE`].
    #[must_use]
    pub fn base() -> Self {
        let body = TEXT_SIZE;
        let (caption, per_body) = CAPTION_PER_BODY;
        Self {
            body,
            small: body * caption / per_body,
            monospace: body * MONOSPACE_SCALE,
        }
    }

    /// Size of the Heading text style: an H1's.
    #[must_use]
    pub fn heading(self) -> f32 {
        self.body * crate::md::HEADING_FONT_SCALES[0]
    }

    /// Give the text styles of `style` these sizes.
    pub fn apply(self, style: &mut egui::Style) {
        for (text_style, size) in [
            (egui::TextStyle::Body, self.body),
            (egui::TextStyle::Button, self.body),
            (egui::TextStyle::Heading, self.heading()),
            (egui::TextStyle::Monospace, self.monospace),
            (egui::TextStyle::Small, self.small),
        ] {
            if let Some(font_id) = style.text_styles.get_mut(&text_style) {
                font_id.size = size;
            }
        }
    }
}

/// Windows accent blue, used for text selection.
const WINDOWS_BLUE: Color32 = Color32::from_rgb(0x00, 0x78, 0xD4);

/// The Windows accent colour (Settings > Personalization > Colors) and the
/// shades Windows derives from it.  As in Windows 11, text is selected
/// with the accent itself, and accent fills and links use its "Dark 1"
/// shade on the light theme and its "Light 2" shade on the dark one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accent {
    pub base: Color32,
    pub dark1: Color32,
    pub light2: Color32,
}

impl Accent {
    /// Windows' default blue.
    pub const DEFAULT: Self = Self {
        base: WINDOWS_BLUE,
        dark1: Color32::from_rgb(0x00, 0x5F, 0xB8),
        light2: Color32::from_rgb(0x4C, 0xC2, 0xFF),
    };

    /// The accent from Windows' `AccentPalette` registry value: eight RGBA
    /// colours, from "Light 3" to "Dark 3" with the accent fourth.
    #[must_use]
    pub fn from_palette(bytes: &[u8]) -> Option<Self> {
        let shade = |index: usize| {
            let start = index * 4;
            let rgb = bytes.get(start..start + 3)?;
            Some(Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
        };
        Some(Self {
            light2: shade(1)?,
            base: shade(3)?,
            dark1: shade(4)?,
        })
    }

    /// Text selection, links and accent fills of `visuals` in this accent.
    const fn apply(self, visuals: &mut egui::Visuals) {
        visuals.selection.bg_fill = self.base;
        visuals.hyperlink_color = if visuals.dark_mode {
            self.light2
        } else {
            self.dark1
        };
    }
}

/// Colour Windows 11 marks a selected item with (the small bar at the left
/// of the current row in a list): the accent fill, which the links share.
#[must_use]
pub const fn accent_fill(visuals: &egui::Visuals) -> Color32 {
    visuals.hyperlink_color
}

/// Give both themes the accent colour `accent`.
pub fn apply_accent(ctx: &egui::Context, accent: Accent) {
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        ctx.style_mut_of(theme, |style| accent.apply(&mut style.visuals));
    }
}

/// Whether the chrome animates (menus slide in, the document fades in
/// after a switch): Windows' "Animation effects" setting, read at
/// startup.  Off in tests, which measure where things are drawn.
static MOTION: AtomicBool = AtomicBool::new(!cfg!(test));

/// Turn the chrome's animations on or off.
pub fn set_motion(on: bool) {
    MOTION.store(on, Ordering::Relaxed);
}

/// Whether the chrome animates.
#[must_use]
pub fn motion() -> bool {
    MOTION.load(Ordering::Relaxed)
}

/// Load fonts and configure the font families.
///
/// On Windows, Segoe UI and Consolas from `%WINDIR%\Fonts` are preferred
/// when present; the bundled fonts always follow as fallbacks.  The only
/// failure path is a broken `RUSTDOWN_FONT_PATH` environment variable.
pub fn configure_fonts(ctx: &egui::Context) -> Result<(), String> {
    let env_override = load_env_font_override()?;
    let fonts = font_definitions(system_fonts_dir().as_deref(), env_override);
    if let Ok(mut stored) = FONT_DEFINITIONS.lock() {
        *stored = Some(fonts.clone());
    }
    ctx.set_fonts(fonts);
    Ok(())
}

/// `%WINDIR%\Fonts` on Windows; `None` elsewhere (no system UI fonts to
/// prefer on the dev host).
fn system_fonts_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        let windir =
            std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        Some(windir.join("Fonts"))
    } else {
        None
    }
}

/// Build the font definitions.
///
/// `fonts_dir` is searched for Segoe UI (`segoeui.ttf`) and Consolas
/// (`consola.ttf`); each one found becomes the primary font of its family.
/// `env_override` (`RUSTDOWN_FONT_PATH`) goes in front of both families.
/// Chains: override, system primary, bundled primary, bundled symbol
/// fallbacks, then system fallbacks for extended coverage.
pub(crate) fn font_definitions(
    fonts_dir: Option<&Path>,
    env_override: Option<(String, Vec<u8>)>,
) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.clear();
    fonts.families.clear();

    let mut proportional = Vec::new();
    let mut monospace = Vec::new();

    if let Some((name, data)) = env_override {
        proportional.push(name.clone());
        monospace.push(name.clone());
        insert_owned(&mut fonts, name, data);
    }

    if let Some(dir) = fonts_dir {
        for (name, file, is_monospace) in [
            (
                SYSTEM_PROPORTIONAL_VARIABLE,
                SYSTEM_PROPORTIONAL_VARIABLE_FILE,
                false,
            ),
            (SYSTEM_PROPORTIONAL, SYSTEM_PROPORTIONAL_FILE, false),
            (SYSTEM_MONOSPACE, SYSTEM_MONOSPACE_FILE, true),
        ] {
            if let Ok(data) = fs::read(dir.join(file)) {
                insert_owned(&mut fonts, name.to_owned(), data);
                let chain = if is_monospace {
                    &mut monospace
                } else {
                    &mut proportional
                };
                chain.push(name.to_owned());
            }
        }

        // The icon font gets a family of its own, so text never picks up
        // its glyphs (and icons never fall back to a text font).
        let mut icons = Vec::new();
        for (name, file) in ICON_FONTS {
            if let Ok(data) = fs::read(dir.join(file)) {
                insert_owned(&mut fonts, name.to_owned(), data);
                icons.push(name.to_owned());
            }
        }
        if !icons.is_empty() {
            fonts.families.insert(icon_family(), icons);
        }
    }

    // Bundled primary fonts (zero-copy from binary .rodata).
    for (name, data, is_monospace) in [
        (BUNDLED_PROPORTIONAL, BUNDLED_PROPORTIONAL_DATA, false),
        (
            BUNDLED_PROPORTIONAL_BOLD,
            BUNDLED_PROPORTIONAL_BOLD_DATA,
            false,
        ),
        (BUNDLED_MONOSPACE, BUNDLED_MONOSPACE_DATA, true),
        (BUNDLED_MONOSPACE_BOLD, BUNDLED_MONOSPACE_BOLD_DATA, true),
    ] {
        insert_static(&mut fonts, name, data);
        let chain = if is_monospace {
            &mut monospace
        } else {
            &mut proportional
        };
        chain.push(name.to_owned());
    }

    // Bundled symbol fallbacks, then system fallbacks, for both families:
    // the editor is monospace, so it needs the same emoji / CJK coverage.
    for (name, data) in BUNDLED_SYMBOL_FALLBACKS {
        insert_static(&mut fonts, name, data);
        proportional.push((*name).to_owned());
        monospace.push((*name).to_owned());
    }
    for (index, path) in fallback_font_paths(fonts_dir).iter().enumerate() {
        let Ok(data) = fs::read(path) else {
            continue;
        };
        let name = format!("{SYSTEM_FALLBACK_PREFIX}{index}");
        insert_owned(&mut fonts, name.clone(), data);
        proportional.push(name.clone());
        monospace.push(name);
    }

    // Bold: Segoe UI Bold (Windows) or Source Sans 3 Bold, then the
    // proportional chain for glyph coverage.
    let mut bold = Vec::new();
    if let Some(dir) = fonts_dir
        && let Ok(data) = fs::read(dir.join(SYSTEM_PROPORTIONAL_BOLD_FILE))
    {
        insert_owned(&mut fonts, SYSTEM_PROPORTIONAL_BOLD.to_owned(), data);
        bold.push(SYSTEM_PROPORTIONAL_BOLD.to_owned());
    }
    bold.push(BUNDLED_PROPORTIONAL_BOLD.to_owned());
    bold.extend(
        proportional
            .iter()
            .filter(|name| *name != BUNDLED_PROPORTIONAL_BOLD)
            .cloned(),
    );
    fonts.families.insert(bold_family(), bold);

    fonts
        .families
        .insert(egui::FontFamily::Proportional, proportional);
    fonts
        .families
        .insert(egui::FontFamily::Monospace, monospace);
    fonts
}

/// The bold proportional font family: `**strong**` text in the preview and
/// the formatted editor.  Contexts without the app's fonts (some tests) do
/// not have it; check with [`has_bold_family`].
#[must_use]
pub fn bold_family() -> egui::FontFamily {
    egui::FontFamily::Name(BOLD_FAMILY.into())
}

/// The headings' font family while the Paper theme is on: its serif
/// bold, then the bold chain.  Registered only then (see
/// [`set_serif_headings`]); headings use [`bold_family`] otherwise.
#[must_use]
pub fn heading_family() -> egui::FontFamily {
    egui::FontFamily::Name(HEADING_FAMILY.into())
}

/// Whether the context's fonts include [`heading_family`].
#[must_use]
pub fn has_heading_family(ui: &egui::Ui) -> bool {
    ui.fonts(|fonts| fonts.definitions().families.contains_key(&heading_family()))
}

/// The app's font definitions as [`configure_fonts`] set them, kept so
/// [`set_serif_headings`] can add the serif font later (egui hands out its
/// fonts only inside a frame).
static FONT_DEFINITIONS: Mutex<Option<egui::FontDefinitions>> = Mutex::new(None);

/// Give headings the serif font (`on`, the Paper theme) or the usual bold
/// one: registers or drops [`heading_family`].  The serif font is read
/// from the Windows fonts folder the first time; without it headings stay
/// bold sans.  Takes effect from the next frame.
pub fn set_serif_headings(ctx: &egui::Context, on: bool) {
    let defs = {
        let Ok(mut stored) = FONT_DEFINITIONS.lock() else {
            return;
        };
        let Some(defs) = stored.as_mut() else {
            return;
        };
        if defs.families.contains_key(&heading_family()) == on {
            return;
        }
        if on {
            if !defs.font_data.contains_key(SERIF_HEADING) {
                let Some(data) =
                    system_fonts_dir().and_then(|dir| fs::read(dir.join(SERIF_HEADING_FILE)).ok())
                else {
                    return;
                };
                insert_owned(defs, SERIF_HEADING.to_owned(), data);
            }
            let mut chain = vec![SERIF_HEADING.to_owned()];
            chain.extend(
                defs.families
                    .get(&bold_family())
                    .cloned()
                    .unwrap_or_default(),
            );
            defs.families.insert(heading_family(), chain);
        } else {
            defs.families.remove(&heading_family());
        }
        defs.clone()
    };
    ctx.set_fonts(defs);
}

/// Whether the context's fonts include [`bold_family`].
#[must_use]
pub fn has_bold_family(ui: &egui::Ui) -> bool {
    ui.fonts(|fonts| fonts.definitions().families.contains_key(&bold_family()))
}

/// The Windows icon font family (Segoe Fluent Icons, else Segoe MDL2
/// Assets).  Only defined when one of them was found, so never on the
/// Linux dev host; `icons.rs` checks before drawing with it.
#[must_use]
pub fn icon_family() -> egui::FontFamily {
    egui::FontFamily::Name(ICON_FAMILY.into())
}

/// Candidate paths of the system fallback fonts.
fn fallback_font_paths(fonts_dir: Option<&Path>) -> Vec<PathBuf> {
    if cfg!(windows) {
        fonts_dir.map_or_else(Vec::new, |dir| {
            FALLBACK_FONT_FILES
                .iter()
                .map(|file| dir.join(file))
                .collect()
        })
    } else {
        FALLBACK_FONT_FILES.iter().map(PathBuf::from).collect()
    }
}

/// Apply the text sizes, spacing and the Notepad light / dark
/// visuals (both themes, so switching theme keeps them).
///
/// egui's own Ctrl+Plus / Ctrl+Minus / Ctrl+0 zoom (of the whole UI) is
/// off: the zoom commands zoom the document only, as in Notepad.
pub fn configure_style(ctx: &egui::Context) {
    ctx.set_visuals_of(egui::Theme::Light, notepad_light_visuals());
    ctx.set_visuals_of(egui::Theme::Dark, notepad_dark_visuals());
    ctx.options_mut(|options| options.zoom_with_keyboard = false);
    let sizes = TextSizes::base();
    ctx.all_styles_mut(|style| {
        sizes.apply(style);
        style.scroll_animation.points_per_second = DEFAULT_SCROLL_ANIMATION_POINTS_PER_SECOND;
        // Roomier, Windows 11-like buttons and tooltips (menus and other
        // popups get their own spacing from `menu::popup_style`).
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.menu_width = 260.0;
        // Visible column separators in markdown tables.
        style.visuals.widgets.noninteractive.bg_stroke.width = 1.0;
    });
}

/// Which colour scheme the themes use, beyond light or dark: the
/// headings' palette and look follow it (see `md::heading_palette`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Flavor {
    #[default]
    Standard,
    Paper,
    Neon,
}

/// The current [`Flavor`], set by [`apply_theme`].
static FLAVOR: AtomicU8 = AtomicU8::new(0);

/// The current colour scheme flavour.
#[must_use]
pub fn flavor() -> Flavor {
    match FLAVOR.load(Ordering::Relaxed) {
        1 => Flavor::Paper,
        2 => Flavor::Neon,
        _ => Flavor::Standard,
    }
}

fn set_flavor(flavor: Flavor) {
    let value = match flavor {
        Flavor::Standard => 0,
        Flavor::Paper => 1,
        Flavor::Neon => 2,
    };
    FLAVOR.store(value, Ordering::Relaxed);
}

/// Paper theme: a warm off-white page and warm chrome, soft ink text (and
/// serif headings, see [`set_serif_headings`]); calm, for reading.
#[must_use]
pub fn paper_visuals() -> egui::Visuals {
    let ink = Color32::from_rgb(0x33, 0x30, 0x2B);
    let mut v = notepad_light_visuals();
    v.panel_fill = Color32::from_rgb(0xEF, 0xEA, 0xDF);
    v.window_fill = Color32::from_rgb(0xF8, 0xF4, 0xEA);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(0xD9, 0xD1, 0xC0));
    v.extreme_bg_color = Color32::from_rgb(0xFB, 0xF8, 0xF1);
    v.faint_bg_color = Color32::from_rgb(0xF1, 0xEB, 0xDE);
    v.code_bg_color = Color32::from_rgb(0xEC, 0xE5, 0xD5);
    v.weak_text_color = Some(Color32::from_rgb(0x7A, 0x72, 0x66));
    v.text_cursor.stroke.color = ink;
    let w = &mut v.widgets;
    w.noninteractive.fg_stroke.color = ink;
    w.noninteractive.bg_fill = v.panel_fill;
    w.noninteractive.weak_bg_fill = v.panel_fill;
    w.noninteractive.bg_stroke.color = Color32::from_rgb(0xD6, 0xCD, 0xBB);
    w.inactive.fg_stroke.color = ink;
    w.inactive.weak_bg_fill = Color32::from_rgb(0xF7, 0xF3, 0xE9);
    w.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xDE, 0xD6, 0xC6));
    w.hovered.weak_bg_fill = Color32::from_rgb(0xE6, 0xDF, 0xD0);
    w.hovered.bg_fill = w.hovered.weak_bg_fill;
    w.active.weak_bg_fill = Color32::from_rgb(0xDB, 0xD3, 0xC2);
    w.active.bg_fill = w.active.weak_bg_fill;
    w.open.weak_bg_fill = Color32::from_rgb(0xE2, 0xDB, 0xCB);
    w.open.bg_fill = w.open.weak_bg_fill;
    v
}

/// Neon theme: a deep charcoal page, glowing neon headings (in the
/// preview) and a bright cyan cursor.
#[must_use]
pub fn neon_visuals() -> egui::Visuals {
    let mut v = notepad_dark_visuals();
    v.panel_fill = Color32::from_rgb(0x12, 0x14, 0x18);
    v.window_fill = Color32::from_rgb(0x1A, 0x1D, 0x23);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(0x2E, 0x33, 0x3D));
    v.extreme_bg_color = Color32::from_rgb(0x0D, 0x0F, 0x12);
    v.faint_bg_color = Color32::from_rgb(0x1A, 0x1E, 0x25);
    v.code_bg_color = Color32::from_rgb(0x1A, 0x1E, 0x25);
    v.weak_text_color = Some(Color32::from_rgb(0x8A, 0x91, 0x9C));
    v.text_cursor.stroke = Stroke::new(2.0, Color32::from_rgb(0x5C, 0xE1, 0xE6));
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = v.panel_fill;
    w.noninteractive.weak_bg_fill = v.panel_fill;
    w.noninteractive.bg_stroke.color = Color32::from_rgb(0x2A, 0x2F, 0x38);
    v
}

/// Notepad light theme: white editor, light-grey chrome, dark text,
/// Windows-blue selection.
#[must_use]
pub fn notepad_light_visuals() -> egui::Visuals {
    let text = Color32::from_rgb(0x1B, 0x1B, 0x1B);
    let mut v = egui::Visuals::light();
    v.panel_fill = Color32::from_gray(0xF3);
    v.window_fill = Color32::from_gray(0xF9);
    v.window_stroke = Stroke::new(1.0, Color32::from_gray(0xD5));
    v.extreme_bg_color = Color32::WHITE;
    v.faint_bg_color = Color32::from_gray(0xF0);
    v.code_bg_color = Color32::from_gray(0xEB);
    v.weak_text_color = Some(Color32::from_gray(0x5F));
    v.warn_fg_color = Color32::from_rgb(0x9D, 0x5D, 0x00);
    v.error_fg_color = Color32::from_rgb(0xC4, 0x2B, 0x1C);
    Accent::DEFAULT.apply(&mut v);
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.text_cursor.stroke.color = text;
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_corner_radius = CornerRadius::same(8);
    v.text_alpha_from_coverage = egui::epaint::AlphaFromCoverage::Gamma(LIGHT_TEXT_GAMMA);

    let w = &mut v.widgets;
    w.noninteractive.fg_stroke.color = text;
    w.noninteractive.bg_fill = v.panel_fill;
    w.noninteractive.weak_bg_fill = v.panel_fill;
    w.noninteractive.bg_stroke.color = Color32::from_gray(0xD0);
    w.inactive.fg_stroke.color = text;
    w.inactive.weak_bg_fill = Color32::from_gray(0xFB);
    w.inactive.bg_stroke = Stroke::new(1.0, Color32::from_gray(0xE0));
    w.hovered.fg_stroke.color = Color32::BLACK;
    w.hovered.weak_bg_fill = Color32::from_gray(0xE8);
    w.hovered.bg_fill = Color32::from_gray(0xE8);
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(0xD0));
    w.active.fg_stroke.color = Color32::BLACK;
    w.active.weak_bg_fill = Color32::from_gray(0xDC);
    w.active.bg_fill = Color32::from_gray(0xDC);
    w.active.bg_stroke = Stroke::new(1.0, Color32::from_gray(0xB8));
    w.open.weak_bg_fill = Color32::from_gray(0xE4);
    w.open.bg_fill = Color32::from_gray(0xE4);
    v
}

/// Notepad dark theme: dark-grey editor on darker chrome, light text,
/// Windows-blue selection.
#[must_use]
pub fn notepad_dark_visuals() -> egui::Visuals {
    let text = Color32::from_gray(0xE6);
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_gray(0x20);
    v.window_fill = Color32::from_gray(0x2C);
    v.extreme_bg_color = Color32::from_gray(0x27);
    v.faint_bg_color = Color32::from_gray(0x34);
    v.code_bg_color = Color32::from_gray(0x38);
    v.weak_text_color = Some(Color32::from_gray(0xA0));
    Accent::DEFAULT.apply(&mut v);
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.text_cursor.stroke.color = text;
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_corner_radius = CornerRadius::same(8);

    let w = &mut v.widgets;
    w.noninteractive.fg_stroke.color = text;
    w.noninteractive.bg_fill = v.panel_fill;
    w.noninteractive.weak_bg_fill = v.panel_fill;
    w.inactive.fg_stroke.color = text;
    w.hovered.fg_stroke.color = Color32::WHITE;
    w.active.fg_stroke.color = Color32::WHITE;
    v
}

/// Background of the editor and preview.
#[must_use]
pub const fn content_background(visuals: &egui::Visuals) -> Color32 {
    visuals.extreme_bg_color
}

/// Background of the title bar (row 1): Mica Alt as Notepad shows it
/// (light: #E8E8E8, sampled from Notepad, a shade darker than the panels;
/// dark: Mica Alt's dark fallback, the panels' colour), tinted a little
/// with the accent colour while the window is `focused`, as Mica is with
/// the wallpaper.  Derived from the panels' colour, so every theme gets
/// its own.  (DWM's real Mica needs a transparent window, which with this
/// frameless window brought back Windows' own caption bar.)
#[must_use]
pub fn title_bar_background(visuals: &egui::Visuals, focused: bool) -> Color32 {
    let (gray, tint) = if visuals.dark_mode {
        (visuals.panel_fill, TITLE_BAR_TINT_DARK)
    } else {
        (
            visuals.panel_fill.lerp_to_gamma(Color32::BLACK, 0.045),
            TITLE_BAR_TINT_LIGHT,
        )
    };
    if focused {
        gray.lerp_to_gamma(accent_fill(visuals), tint)
    } else {
        gray
    }
}

/// How much of the accent colour tints the light title bar.
const TITLE_BAR_TINT_LIGHT: f32 = 0.07;
/// How much of the accent colour tints the dark title bar.
const TITLE_BAR_TINT_DARK: f32 = 0.10;

/// Background of the menu / formatting row (row 2) and of the active tab,
/// which joins it like a folder tab.  Windows 11's layer on Mica Alt:
/// white at 70 % over the light title bar (#F8F8F8, as sampled from
/// Notepad), #3A3A3A at 45 % over the dark one: a little lighter than
/// the panels (#F3F3F3 / #202020), so every theme gets its own.
#[must_use]
pub fn toolbar_background(visuals: &egui::Visuals) -> Color32 {
    let toward_white = if visuals.dark_mode { 0.054 } else { 0.42 };
    visuals
        .panel_fill
        .lerp_to_gamma(Color32::WHITE, toward_white)
}

/// Thin line between the menu row and the document (light: Notepad's,
/// a 6 % black divider over the row).
#[must_use]
pub const fn chrome_separator(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_gray(0x1C)
    } else {
        Color32::from_gray(0xE9)
    }
}

/// Outline of the active tab (light sampled from Notepad; dark a little
/// darker than the title bar, like the menu-row line).
#[must_use]
pub const fn tab_outline(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_gray(0x1C)
    } else {
        Color32::from_gray(0xD5)
    }
}

/// Dividers between inactive tabs.
#[must_use]
pub const fn chrome_divider(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_gray(0x48)
    } else {
        Color32::from_gray(0xD4)
    }
}

/// Windows 11 "subtle" fill of a flat button (tabs, toolbar and caption
/// buttons, menu titles) while hovered or pressed.  Translucent, so it
/// works on every chrome background.
#[must_use]
pub const fn subtle_fill(visuals: &egui::Visuals, pressed: bool) -> Color32 {
    // Premultiplied: black or white at the given alpha.
    let (gray, alpha) = match (visuals.dark_mode, pressed) {
        (false, false) => (0x00, 0x0F),
        (false, true) => (0x00, 0x09),
        (true, false) => (0x12, 0x12),
        (true, true) => (0x0B, 0x0B),
    };
    Color32::from_rgba_premultiplied(gray, gray, gray, alpha)
}

/// Glyph coverage gamma of the light theme: below 1 darkens the
/// anti-aliased edges, so small dark text on white looks crisper, closer
/// to `ClearType` (egui's default, 1, looks grey and thin; 0.4 starts to
/// smear).  The dark theme keeps egui's default, which is tuned for
/// light-on-dark text.
const LIGHT_TEXT_GAMMA: f32 = 0.7;

/// View → Theme choice (persisted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    #[default]
    Light,
    Dark,
    /// Warm paper and serif headings (light).
    Paper,
    /// Charcoal and glowing neon headings (dark).
    Neon,
    /// Follow the Windows app theme.
    System,
}

impl ThemeChoice {
    pub const ALL: [Self; 5] = [
        Self::Light,
        Self::Dark,
        Self::Paper,
        Self::Neon,
        Self::System,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Paper => "paper",
            Self::Neon => "neon",
            Self::System => "system",
        }
    }

    #[must_use]
    pub fn from_str_lossy(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.as_str() == s)
    }

    /// Menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::Paper => "Paper",
            Self::Neon => "Neon",
            Self::System => "Use system setting",
        }
    }

    #[must_use]
    pub const fn preference(self) -> egui::ThemePreference {
        match self {
            Self::Light | Self::Paper => egui::ThemePreference::Light,
            Self::Dark | Self::Neon => egui::ThemePreference::Dark,
            Self::System => egui::ThemePreference::System,
        }
    }
}

/// Switch egui to `choice` (System follows the OS theme): the visuals of
/// its light and dark slots, its flavour and heading font, then the theme.
/// The accent colour goes on top afterwards (`apply_accent`).
pub fn apply_theme(ctx: &egui::Context, choice: ThemeChoice) {
    let (light, dark, flavor) = match choice {
        ThemeChoice::Paper => (paper_visuals(), notepad_dark_visuals(), Flavor::Paper),
        ThemeChoice::Neon => (notepad_light_visuals(), neon_visuals(), Flavor::Neon),
        ThemeChoice::Light | ThemeChoice::Dark | ThemeChoice::System => (
            notepad_light_visuals(),
            notepad_dark_visuals(),
            Flavor::Standard,
        ),
    };
    ctx.set_visuals_of(egui::Theme::Light, light);
    ctx.set_visuals_of(egui::Theme::Dark, dark);
    ctx.all_styles_mut(|style| {
        // Visible column separators in markdown tables.
        style.visuals.widgets.noninteractive.bg_stroke.width = 1.0;
    });
    set_flavor(flavor);
    set_serif_headings(ctx, flavor == Flavor::Paper);
    ctx.set_theme(choice.preference());
}

impl crate::RustdownApp {
    /// Drop colour-dependent caches when the effective theme changes
    /// (View → Theme, or the OS theme while following the system).  The
    /// editor's galley cache is keyed on text and layout, not on colours.
    pub(crate) fn sync_theme_caches(&mut self, ctx: &egui::Context) {
        let dark = ctx.style().visuals.dark_mode;
        if self.last_dark_mode != Some(dark) {
            // The backdrop and the window border follow the theme too.
            self.windows.set_dark_frame(dark);
        }
        if self
            .last_dark_mode
            .replace(dark)
            .is_some_and(|was| was != dark)
        {
            self.drop_colour_caches();
        }
    }

    /// Switch to the chosen theme (View → Theme), with the accent colour
    /// on top, and lay the documents out again in its colours and fonts.
    pub(crate) fn apply_theme(&mut self, ctx: &egui::Context) {
        apply_theme(ctx, self.theme);
        if let Some(accent) = self.windows.accent() {
            apply_accent(ctx, accent);
        }
        self.drop_colour_caches();
    }

    /// Drop the caches that hold colours (theme or accent changes).
    pub(crate) fn drop_colour_caches(&mut self) {
        for tab in &mut self.tabs {
            tab.doc.editor_galley_cache = None;
            tab.doc.preview_cache.clear();
            // Links and quote bars use the accent colour.
            tab.preview_style_cache.style = None;
        }
    }
}

fn insert_static(fonts: &mut egui::FontDefinitions, name: &str, data: &'static [u8]) {
    fonts
        .font_data
        .insert(name.to_owned(), Arc::new(egui::FontData::from_static(data)));
}

fn insert_owned(fonts: &mut egui::FontDefinitions, name: String, data: Vec<u8>) {
    fonts
        .font_data
        .insert(name, Arc::new(egui::FontData::from_owned(data)));
}

fn load_env_font_override() -> Result<Option<(String, Vec<u8>)>, String> {
    font_override_from(std::env::var("RUSTDOWN_FONT_PATH").ok())
}

/// Font override from the value of `RUSTDOWN_FONT_PATH` (`None` = unset).
/// Split out so tests never touch the process environment, which other
/// tests read concurrently via `configure_fonts`.
fn font_override_from(value: Option<String>) -> Result<Option<(String, Vec<u8>)>, String> {
    let path = match value {
        Some(p) if !p.trim().is_empty() => p,
        Some(_) => return Err("RUSTDOWN_FONT_PATH is set but empty".to_owned()),
        None => return Ok(None),
    };
    let data = fs::read(&path)
        .map_err(|err| format!("Failed to read font from RUSTDOWN_FONT_PATH '{path}': {err}"))?;
    Ok(Some(("rustdown-env-override".to_owned(), data)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn families(defs: &egui::FontDefinitions) -> (&[String], &[String]) {
        let get = |family| defs.families.get(&family).map_or(&[][..], Vec::as_slice);
        (
            get(egui::FontFamily::Proportional),
            get(egui::FontFamily::Monospace),
        )
    }

    /// A fresh fonts directory under the system temp dir.
    fn temp_fonts_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rustdown-fonts-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn configure_style_applies_default_font_sizes_to_both_themes() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let base = TEXT_SIZE;
        let expected: &[(egui::TextStyle, f32)] = &[
            (egui::TextStyle::Body, base),
            (egui::TextStyle::Button, base),
            (egui::TextStyle::Monospace, base * MONOSPACE_SCALE),
            (egui::TextStyle::Small, 12.0),
            (
                egui::TextStyle::Heading,
                base * crate::md::HEADING_FONT_SCALES[0],
            ),
        ];
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let style = ctx.style_of(theme);
            for (text_style, size) in expected {
                assert_eq!(
                    style.text_styles.get(text_style).map(|f| f.size),
                    Some(*size),
                    "{theme:?} {text_style:?} should have size {size}"
                );
            }
        }
        assert!(
            !ctx.options(|o| o.zoom_with_keyboard),
            "egui's whole-UI keyboard zoom is off"
        );
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn one_base_size_gives_every_text_size() {
        // Notepad's sizes: Body 14 (menus, tree, document body), Caption 12
        // (tabs, status bar); code 1.02 ×, H1 1.5 ×.
        let sizes = TextSizes::base();
        assert_eq!(TEXT_SIZE, 14.0);
        assert_eq!(sizes.body, 14.0);
        assert_eq!(sizes.small, 12.0);
        assert_eq!(sizes.monospace, 14.0 * MONOSPACE_SCALE);
        assert_eq!(sizes.heading(), 21.0);
    }

    #[test]
    fn light_theme_text_is_drawn_a_little_darker() {
        use egui::epaint::AlphaFromCoverage;
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let alpha = |theme| ctx.style_of(theme).visuals.text_alpha_from_coverage;
        let light = AlphaFromCoverage::Gamma(LIGHT_TEXT_GAMMA);
        assert_eq!(alpha(egui::Theme::Light), light);
        assert_eq!(
            alpha(egui::Theme::Dark),
            egui::Visuals::dark().text_alpha_from_coverage,
            "dark theme keeps egui's default"
        );
        // Theme switches (incl. following the system) keep it.  (Not Paper
        // or Neon: their flavour is global and other tests run alongside.)
        for theme in [ThemeChoice::Light, ThemeChoice::Dark, ThemeChoice::System] {
            apply_theme(&ctx, theme);
            assert_eq!(alpha(egui::Theme::Light), light, "{theme:?}");
        }
    }

    #[test]
    fn monospace_scale_matches_the_x_heights_of_consolas_and_segoe_ui() {
        // Ink x-height in em (fontTools on the Windows runner's fonts).
        const CONSOLAS: f32 = 0.4902;
        const SEGOE_UI_VARIABLE: f32 = 0.5000;
        let at_scale = MONOSPACE_SCALE * CONSOLAS;
        assert!(
            (at_scale - SEGOE_UI_VARIABLE).abs() < 0.001,
            "Consolas x-height {at_scale:.4} em per body em"
        );
    }

    #[test]
    fn configure_style_sets_table_separator_stroke_width() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let style = ctx.style();
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(
                style.visuals.widgets.noninteractive.bg_stroke.width, 1.0,
                "table column separator stroke should be 1.0"
            );
        }
        assert!(
            (style.scroll_animation.points_per_second - DEFAULT_SCROLL_ANIMATION_POINTS_PER_SECOND)
                .abs()
                < f32::EPSILON,
            "scroll animation speed should be nudged up slightly"
        );
    }

    #[test]
    fn heading_size_is_larger_than_body() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let style = ctx.style();
        let size_of = |ts: &egui::TextStyle| style.text_styles.get(ts).map_or(0.0, |f| f.size);
        assert!(size_of(&egui::TextStyle::Heading) > size_of(&egui::TextStyle::Body));
    }

    /// Relative luminance difference is a cheap readability proxy.
    fn luma(c: Color32) -> f32 {
        0.114f32.mul_add(
            f32::from(c.b()),
            0.299f32.mul_add(f32::from(c.r()), 0.587 * f32::from(c.g())),
        )
    }

    #[test]
    fn light_theme_is_notepad_like() {
        let v = notepad_light_visuals();
        assert!(!v.dark_mode);
        assert_eq!(content_background(&v), Color32::WHITE, "white editor");
        assert!(
            luma(v.panel_fill) < 255.0 && luma(v.panel_fill) > 220.0,
            "light-grey chrome"
        );
        assert_eq!(v.selection.bg_fill, WINDOWS_BLUE);
        assert!(luma(v.text_color()) < 60.0, "dark text");
        // Everything drawn on the white editor must stay readable.
        let bg = luma(Color32::WHITE);
        for (what, color) in [
            ("text", v.text_color()),
            ("weak text", v.weak_text_color()),
            ("link", v.hyperlink_color),
        ] {
            assert!(bg - luma(color) > 100.0, "{what} too faint on white");
        }
        for (level, color) in crate::md::LIGHT_HEADING_COLORS.iter().enumerate() {
            assert!(
                bg - luma(*color) > 100.0,
                "H{} too faint on white",
                level + 1
            );
        }
        // Code background must be visible on white (an additive colour is not).
        assert!(v.faint_bg_color.a() == 255 && luma(v.faint_bg_color) < bg);
    }

    #[test]
    fn dark_theme_keeps_text_readable() {
        let v = notepad_dark_visuals();
        assert!(v.dark_mode);
        let bg = luma(content_background(&v));
        for color in [v.text_color(), v.weak_text_color(), v.hyperlink_color] {
            assert!(luma(color) - bg > 90.0);
        }
        assert_eq!(v.selection.bg_fill, WINDOWS_BLUE);
    }

    #[test]
    fn configure_style_installs_both_notepad_themes() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        assert_eq!(
            ctx.style_of(egui::Theme::Light).visuals.panel_fill,
            notepad_light_visuals().panel_fill
        );
        assert_eq!(
            ctx.style_of(egui::Theme::Dark).visuals.panel_fill,
            notepad_dark_visuals().panel_fill
        );
    }

    #[test]
    fn theme_choice_round_trips_and_applies() {
        for choice in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::from_str_lossy(choice.as_str()), Some(choice));
            assert!(!choice.label().is_empty());
        }
        assert_eq!(ThemeChoice::from_str_lossy("purple"), None);
        assert_eq!(ThemeChoice::default(), ThemeChoice::Light);

        let ctx = egui::Context::default();
        configure_style(&ctx);
        apply_theme(&ctx, ThemeChoice::Light);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        apply_theme(&ctx, ThemeChoice::Dark);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        apply_theme(&ctx, ThemeChoice::System);
        assert_eq!(
            ctx.options(|o| o.theme_preference),
            egui::ThemePreference::System
        );
    }

    #[test]
    fn theme_command_applies_and_drops_colour_caches() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let mut app = crate::RustdownApp::default();
        apply_theme(&ctx, app.theme);
        app.tab_mut().doc.text = Arc::new("# Title\n".to_owned());
        let render = |app: &mut crate::RustdownApp| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                app.sync_theme_caches(ctx);
                egui::CentralPanel::default().show(ctx, |ui| app.show_editor(ui));
            });
        };
        render(&mut app);
        assert!(!ctx.style().visuals.dark_mode, "light by default");
        assert!(app.tab().doc.editor_galley_cache.is_some());

        app.execute(crate::AppCommand::SetTheme(ThemeChoice::Dark), &ctx);
        assert_eq!(app.theme, ThemeChoice::Dark);
        let _ = ctx.run(egui::RawInput::default(), |ctx| app.sync_theme_caches(ctx));
        assert!(ctx.style().visuals.dark_mode);
        assert!(
            app.tab().doc.editor_galley_cache.is_none(),
            "editor colours are rebuilt for the new theme"
        );
        render(&mut app);
        let _ = ctx.run(egui::RawInput::default(), |ctx| app.sync_theme_caches(ctx));
        assert!(
            app.tab().doc.editor_galley_cache.is_some(),
            "no change: kept"
        );
    }

    #[test]
    fn configure_fonts_succeeds_with_bundled_fonts() {
        let ctx = egui::Context::default();
        let result = configure_fonts(&ctx);
        assert!(
            result.is_ok(),
            "configure_fonts should always succeed with bundled fonts: {result:?}"
        );
    }

    #[test]
    fn system_fonts_absent_uses_bundled_primaries() {
        let empty = temp_fonts_dir("absent");
        for dir in [None, Some(empty.as_path())] {
            let defs = font_definitions(dir, None);
            let (proportional, monospace) = families(&defs);
            assert_eq!(proportional[0], BUNDLED_PROPORTIONAL);
            assert_eq!(monospace[0], BUNDLED_MONOSPACE);
            assert!(!defs.font_data.contains_key(SYSTEM_PROPORTIONAL));
            assert!(!defs.font_data.contains_key(SYSTEM_MONOSPACE));
        }
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn bold_family_prefers_segoe_ui_bold_then_the_bundled_bold() {
        let bold = |defs: &egui::FontDefinitions| defs.families.get(&bold_family()).cloned();
        let defs = font_definitions(None, None);
        let chain = bold(&defs).unwrap_or_default();
        assert_eq!(
            chain.first().map(String::as_str),
            Some(BUNDLED_PROPORTIONAL_BOLD)
        );
        // The regular chain follows for glyph coverage, without a second copy.
        let (proportional, _) = families(&defs);
        assert_eq!(chain.len(), proportional.len());
        assert!(chain.contains(&BUNDLED_PROPORTIONAL.to_owned()));

        let dir = temp_fonts_dir("bold");
        let _ = fs::write(
            dir.join(SYSTEM_PROPORTIONAL_BOLD_FILE),
            BUNDLED_PROPORTIONAL_BOLD_DATA,
        );
        let defs = font_definitions(Some(&dir), None);
        let chain = bold(&defs).unwrap_or_default();
        assert_eq!(
            chain[..2],
            [SYSTEM_PROPORTIONAL_BOLD, BUNDLED_PROPORTIONAL_BOLD]
        );
        let _ = fs::remove_dir_all(&dir);

        // A context with the app's fonts has the family; a bare one does not.
        let ctx = egui::Context::default();
        assert!(configure_fonts(&ctx).is_ok());
        let mut has = (false, true);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| has.0 = has_bold_family(ui));
        });
        let bare = egui::Context::default();
        let _ = bare.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| has.1 = has_bold_family(ui));
        });
        assert_eq!(has, (true, false));
    }

    #[test]
    fn icon_fonts_prefer_fluent_icons_then_mdl2_in_their_own_family() {
        let icons = |defs: &egui::FontDefinitions| defs.families.get(&icon_family()).cloned();
        let names = |names: &[&str]| Some(names.iter().map(|&n| n.to_owned()).collect());
        let [(fluent, fluent_file), (mdl2, mdl2_file)] = ICON_FONTS;

        // Dev host (no fonts dir) or neither font: no icon family at all.
        let empty = temp_fonts_dir("icons-none");
        for dir in [None, Some(empty.as_path())] {
            assert_eq!(icons(&font_definitions(dir, None)), None);
        }
        let _ = fs::remove_dir_all(&empty);

        // Windows 10: Segoe MDL2 Assets.
        let dir = temp_fonts_dir("icons");
        let _ = fs::write(dir.join(mdl2_file), BUNDLED_PROPORTIONAL_DATA);
        assert_eq!(icons(&font_definitions(Some(&dir), None)), names(&[mdl2]));
        // Windows 11: Segoe Fluent Icons first, MDL2 behind it.
        let _ = fs::write(dir.join(fluent_file), BUNDLED_PROPORTIONAL_DATA);
        let defs = font_definitions(Some(&dir), None);
        assert_eq!(icons(&defs), names(&[fluent, mdl2]));
        // Text never picks up icon glyphs.
        for family in [
            egui::FontFamily::Proportional,
            egui::FontFamily::Monospace,
            bold_family(),
        ] {
            let chain = defs.families.get(&family).cloned().unwrap_or_default();
            assert!(
                !chain.iter().any(|n| n == fluent || n == mdl2),
                "{family:?}"
            );
        }
        let _ = fs::remove_file(dir.join(mdl2_file));
        assert_eq!(icons(&font_definitions(Some(&dir), None)), names(&[fluent]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn segoe_ui_variable_goes_before_segoe_ui() {
        let dir = temp_fonts_dir("variable");
        let _ = fs::write(
            dir.join(SYSTEM_PROPORTIONAL_VARIABLE_FILE),
            BUNDLED_PROPORTIONAL_DATA,
        );
        let defs = font_definitions(Some(&dir), None);
        let (proportional, _) = families(&defs);
        assert_eq!(
            proportional[..2],
            [SYSTEM_PROPORTIONAL_VARIABLE, BUNDLED_PROPORTIONAL]
        );
        let _ = fs::write(
            dir.join(SYSTEM_PROPORTIONAL_FILE),
            BUNDLED_PROPORTIONAL_DATA,
        );
        let defs = font_definitions(Some(&dir), None);
        let (proportional, _) = families(&defs);
        assert_eq!(
            proportional[..3],
            [
                SYSTEM_PROPORTIONAL_VARIABLE,
                SYSTEM_PROPORTIONAL,
                BUNDLED_PROPORTIONAL
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn system_fonts_present_become_primaries_with_bundled_fallbacks() {
        let dir = temp_fonts_dir("present");
        let _ = fs::write(
            dir.join(SYSTEM_PROPORTIONAL_FILE),
            BUNDLED_PROPORTIONAL_DATA,
        );
        let _ = fs::write(dir.join(SYSTEM_MONOSPACE_FILE), BUNDLED_MONOSPACE_DATA);

        let defs = font_definitions(Some(&dir), None);
        let (proportional, monospace) = families(&defs);
        assert_eq!(proportional[0], SYSTEM_PROPORTIONAL);
        assert_eq!(monospace[0], SYSTEM_MONOSPACE);
        // Bundled fonts and symbol fallbacks are still in both chains.
        assert_eq!(proportional[1], BUNDLED_PROPORTIONAL);
        assert_eq!(monospace[1], BUNDLED_MONOSPACE);
        for (name, _) in BUNDLED_SYMBOL_FALLBACKS {
            assert!(proportional.iter().any(|n| n == name));
            assert!(monospace.iter().any(|n| n == name));
        }

        // Only one of the two present: each family picks independently.
        let _ = fs::remove_file(dir.join(SYSTEM_MONOSPACE_FILE));
        let defs = font_definitions(Some(&dir), None);
        let (proportional, monospace) = families(&defs);
        assert_eq!(proportional[0], SYSTEM_PROPORTIONAL);
        assert_eq!(monospace[0], BUNDLED_MONOSPACE);

        // The env override still goes first.
        let defs = font_definitions(
            Some(&dir),
            Some(("override".to_owned(), BUNDLED_MONOSPACE_DATA.to_vec())),
        );
        let (proportional, monospace) = families(&defs);
        assert_eq!(proportional[..2], ["override", SYSTEM_PROPORTIONAL]);
        assert_eq!(monospace[0], "override");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn system_fonts_present_render_in_a_context() {
        let dir = temp_fonts_dir("ctx");
        let _ = fs::write(dir.join(SYSTEM_MONOSPACE_FILE), BUNDLED_MONOSPACE_DATA);
        let ctx = egui::Context::default();
        ctx.set_fonts(font_definitions(Some(&dir), None));
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        let has_glyphs = ctx.fonts_mut(|fonts| {
            fonts
                .fonts
                .font(&egui::FontFamily::Monospace)
                .has_glyphs("fn main() {}")
        });
        assert!(has_glyphs);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_fonts_cover_emoji_glyphs_in_both_families() {
        let ctx = egui::Context::default();
        configure_fonts(&ctx).unwrap_or_else(|e| unreachable!("{e}"));
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let has_glyphs =
                ctx.fonts_mut(|fonts| fonts.fonts.font(&family).has_glyphs("🔬🎉✨🟢"));
            assert!(
                has_glyphs,
                "{family:?}: bundled fallbacks should cover the bundled-doc emoji set"
            );
        }
    }

    #[test]
    fn system_fallbacks_come_after_every_primary_font() {
        let defs = font_definitions(None, None);
        let (proportional, monospace) = families(&defs);
        for (chain, primaries) in [
            (
                proportional,
                [BUNDLED_PROPORTIONAL, BUNDLED_PROPORTIONAL_BOLD],
            ),
            (monospace, [BUNDLED_MONOSPACE, BUNDLED_MONOSPACE_BOLD]),
        ] {
            let first_fallback = chain
                .iter()
                .position(|name| name.starts_with(SYSTEM_FALLBACK_PREFIX))
                .unwrap_or(chain.len());
            for primary in primaries {
                let pos = chain.iter().position(|name| name == primary);
                assert!(pos.is_some_and(|pos| pos < first_fallback), "{primary}");
            }
        }
    }

    #[test]
    fn env_override_empty_is_error() {
        assert!(font_override_from(Some(String::new())).is_err());
        assert!(font_override_from(Some("  ".to_owned())).is_err());
    }

    #[test]
    fn env_override_missing_is_none() {
        assert!(matches!(font_override_from(None), Ok(None)));
    }

    #[test]
    fn env_override_unreadable_path_is_error() {
        let missing = std::env::temp_dir().join("rustdown-no-such-font.ttf");
        assert!(font_override_from(Some(missing.to_string_lossy().into_owned())).is_err());
    }

    #[test]
    fn accent_comes_from_the_windows_palette() {
        // Windows' AccentPalette for its default blue: Light 3 to Dark 3,
        // then one unused colour.
        let palette: [u8; 32] = [
            0x99, 0xEB, 0xFF, 0, 0x4C, 0xC2, 0xFF, 0, 0x00, 0x91, 0xF8, 0, 0x00, 0x78, 0xD4, 0,
            0x00, 0x5F, 0xB8, 0, 0x00, 0x3E, 0x92, 0, 0x00, 0x1A, 0x68, 0, 0xF7, 0x63, 0x0C, 0,
        ];
        assert_eq!(Accent::from_palette(&palette), Some(Accent::DEFAULT));
        assert_eq!(Accent::from_palette(&palette[..18]), None);
    }

    #[test]
    fn accent_sets_selection_and_links_per_theme() {
        let accent = Accent {
            base: Color32::from_rgb(0xC3, 0x00, 0x52),
            dark1: Color32::from_rgb(0x9E, 0x00, 0x41),
            light2: Color32::from_rgb(0xFF, 0x8C, 0xB5),
        };
        let ctx = egui::Context::default();
        configure_style(&ctx);
        apply_accent(&ctx, accent);
        for (theme, link) in [
            (egui::Theme::Light, accent.dark1),
            (egui::Theme::Dark, accent.light2),
        ] {
            let visuals = ctx.style_of(theme).visuals.clone();
            assert_eq!(visuals.selection.bg_fill, accent.base);
            assert_eq!(visuals.hyperlink_color, link);
            assert_eq!(accent_fill(&visuals), link);
        }
    }
}
