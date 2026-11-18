//! The window's position, size and maximized state, remembered between
//! runs in the settings file.

use eframe::egui;

use crate::{RustdownApp, preferences::UserPreferences};

/// The window's inner size on a first run.
pub(crate) const DEFAULT_SIZE: [f32; 2] = [1024.0, 768.0];
/// The window's least inner size.
pub(crate) const MIN_SIZE: [f32; 2] = [480.0, 320.0];
/// Largest side restored, so a broken settings file cannot make a huge
/// window.
const MAX_SIDE: f32 = 16_384.0;

/// Where and how big the window was, in logical pixels (egui points at
/// 100 % zoom: what the window builder takes).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct WindowState {
    /// Top-left corner of the window, while not maximized.
    pub(crate) position: Option<[f32; 2]>,
    /// Inner size, while not maximized.
    pub(crate) size: Option<[f32; 2]>,
    pub(crate) maximized: bool,
}

impl WindowState {
    /// The state saved in `prefs`.
    pub(crate) const fn from_preferences(prefs: &UserPreferences) -> Self {
        Self {
            position: prefs.window_position,
            size: prefs.window_size,
            maximized: prefs.window_maximized,
        }
    }

    /// The inner size to open with: the saved one (within limits) or the
    /// default.
    fn inner_size(self) -> [f32; 2] {
        self.size
            .filter(|size| size.iter().all(|side| side.is_finite()))
            .map_or(DEFAULT_SIZE, |[width, height]| {
                [
                    width.clamp(MIN_SIZE[0], MAX_SIDE),
                    height.clamp(MIN_SIZE[1], MAX_SIDE),
                ]
            })
    }

    /// `builder` with the saved size, the saved position when it is still
    /// on a screen, and maximized when the window was.
    pub(crate) fn apply(self, builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
        let size = self.inner_size();
        let mut builder = builder.with_inner_size(size).with_min_inner_size(MIN_SIZE);
        if let Some(position) = self.position.filter(|&position| {
            position.iter().all(|value| value.is_finite())
                && crate::windows_integration::on_a_screen(position, size)
        }) {
            builder = builder.with_position(position);
        }
        if self.maximized {
            builder = builder.with_maximized(true);
        }
        builder
    }
}

impl RustdownApp {
    /// Note the window's position and size (while it is neither maximized,
    /// minimized nor full screen) and whether it is maximized; they are
    /// saved with the settings when the window closes.
    pub(crate) fn track_window_state(&mut self, ctx: &egui::Context) {
        let zoom = ctx.zoom_factor();
        let (outer, inner, maximized, minimized, fullscreen) = ctx.input(|input| {
            let viewport = input.viewport();
            (
                viewport.outer_rect,
                viewport.inner_rect,
                viewport.maximized,
                viewport.minimized,
                viewport.fullscreen,
            )
        });
        if minimized == Some(true) || fullscreen == Some(true) {
            return;
        }
        self.window.maximized = maximized.unwrap_or(false);
        if self.window.maximized {
            return;
        }
        if let (Some(outer), Some(inner)) = (outer, inner) {
            // Points times the zoom: the logical pixels the builder takes.
            self.window.position = Some([outer.min.x * zoom, outer.min.y * zoom]);
            self.window.size = Some([inner.width() * zoom, inner.height() * zoom]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)] // Exact constants in, exact constants out.
    fn saved_size_is_kept_within_limits() {
        let state = |size| WindowState {
            size,
            ..WindowState::default()
        };
        assert_eq!(state(None).inner_size(), DEFAULT_SIZE);
        assert_eq!(state(Some([800.0, 600.0])).inner_size(), [800.0, 600.0]);
        assert_eq!(
            state(Some([10.0, 1e9])).inner_size(),
            [MIN_SIZE[0], MAX_SIDE]
        );
        assert_eq!(state(Some([f32::NAN, 600.0])).inner_size(), DEFAULT_SIZE);
    }
}
