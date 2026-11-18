//! Edit → Go to… (Ctrl+G): Notepad's "Go To Line" modal.

use eframe::egui::{
    self,
    text::{CCursor, CCursorRange},
};

use crate::{
    RustdownApp,
    edit_ops::{line_col, line_count},
};

const INPUT_ID: &str = "goto_line_input";

/// State of the open dialog.
#[derive(Debug, Default)]
pub(crate) struct GoToLineDialog {
    pub(crate) input: String,
    pub(crate) error: Option<GoToLineError>,
    /// Focus the input and select its text on the next frame.
    focus_input: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GoToLineError {
    NotANumber,
    BeyondEnd,
}

impl GoToLineError {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::NotANumber => "Type a line number.",
            Self::BeyondEnd => "The line number is beyond the total number of lines.",
        }
    }
}

/// Validate the dialog input against a document of `line_count` lines.
pub(crate) fn parse_line_number(input: &str, line_count: usize) -> Result<usize, GoToLineError> {
    let line: usize = input
        .trim()
        .parse()
        .map_err(|_| GoToLineError::NotANumber)?;
    match line {
        0 => Err(GoToLineError::NotANumber),
        line if line > line_count => Err(GoToLineError::BeyondEnd),
        line => Ok(line),
    }
}

impl RustdownApp {
    /// Open the dialog, prefilled with the caret's line (Notepad does too).
    pub(crate) fn open_goto_dialog(&mut self, ctx: &egui::Context) {
        if !self.mode.uses_editor() {
            return;
        }
        let caret = self.editor_selection(ctx).end;
        let (line, _) = line_col(self.tab().doc.text.as_str(), caret);
        self.goto_dialog = Some(GoToLineDialog {
            input: line.to_string(),
            error: None,
            focus_input: true,
        });
    }

    pub(crate) fn show_goto_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.goto_dialog.take() else {
            return;
        };
        let mut submit = false;
        let answer = crate::dialog::show(
            ctx,
            "goto_line",
            "Go to line",
            |ui| {
                ui.label("Line number:");
                let input_id = egui::Id::new(INPUT_ID);
                if dialog.focus_input {
                    // Select the prefilled number so typing replaces it.
                    let mut state = egui::TextEdit::load_state(ctx, input_id).unwrap_or_default();
                    state.cursor.set_char_range(Some(CCursorRange::two(
                        CCursor::new(0),
                        CCursor::new(dialog.input.chars().count()),
                    )));
                    state.store(ctx, input_id);
                }
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.input)
                        .id(input_id)
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut dialog.focus_input) {
                    response.request_focus();
                }
                if response.changed() {
                    dialog.error = None;
                }
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                if let Some(error) = dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, error.message());
                }
            },
            &[
                ("Go to", crate::dialog::Role::Primary),
                ("Cancel", crate::dialog::Role::Secondary),
            ],
            Some(1),
        );
        submit |= answer == Some(0);
        let cancel = answer == Some(1);

        if submit {
            let lines = line_count(self.tab().doc.text.as_str());
            match parse_line_number(&dialog.input, lines) {
                Ok(line) => {
                    self.go_to_line(ctx, line);
                    return;
                }
                Err(error) => {
                    dialog.error = Some(error);
                    dialog.focus_input = true;
                }
            }
        } else if cancel {
            self.focus_editor = true;
            return;
        }
        self.goto_dialog = Some(dialog);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_number_validates_range() {
        assert_eq!(parse_line_number("1", 3), Ok(1));
        assert_eq!(parse_line_number(" 3 ", 3), Ok(3));
        assert_eq!(parse_line_number("4", 3), Err(GoToLineError::BeyondEnd));
        assert_eq!(parse_line_number("0", 3), Err(GoToLineError::NotANumber));
        assert_eq!(parse_line_number("-1", 3), Err(GoToLineError::NotANumber));
        assert_eq!(parse_line_number("", 3), Err(GoToLineError::NotANumber));
        assert_eq!(parse_line_number("two", 3), Err(GoToLineError::NotANumber));
        assert_eq!(
            parse_line_number("99999999999999999999999", 3),
            Err(GoToLineError::NotANumber)
        );
        assert!(!GoToLineError::BeyondEnd.message().is_empty());
    }
}
