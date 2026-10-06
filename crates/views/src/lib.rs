//! Every screen and every piece of window chrome.
//!
//! Views read state and send commands; they never touch the database or the
//! audio engine directly.

pub mod actions;
pub mod chrome;
pub mod dialogs;
pub mod root;
pub mod screen;
pub mod screens;

pub use actions::SHORTCUTS;
pub use dialogs::Dialog;
pub use root::Root;
pub use screen::{History, Screen};
