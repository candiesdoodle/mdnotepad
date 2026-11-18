//! `mdnotepad_preview.dll`: `MdNotePad`'s preview handler for File
//! Explorer's preview pane.  Explorer (its `prevhost.exe`) loads it for
//! `.md` / `.markdown` files once Settings → Register .md file type has
//! registered it (the DLL sits next to `MdNotePad.exe`).  It shows the
//! document formatted, in a Windows rich-text control: the app's Markdown
//! parser turns it into rich text (`rtf`), and `handler` is the COM side.

#![cfg(windows)]

mod handler;
#[allow(dead_code, unreachable_pub)] // The parts the app uses and the DLL does not.
#[path = "../md/parse.rs"]
mod parse;
mod rtf;

pub use handler::{DllCanUnloadNow, DllGetClassObject};
