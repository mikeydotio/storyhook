pub mod action;
pub mod app;
pub mod components;
pub mod data;
pub mod event;
pub mod focus;
pub mod keymap;
pub mod state;
pub mod terminal;
pub mod theme;

use std::path::Path;

use crate::error::AppError;

pub fn run(root: &Path) -> Result<(), AppError> {
    app::run(root)
}
