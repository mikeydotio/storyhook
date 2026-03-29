mod app;
mod action;
mod state;
mod event;
mod terminal;
mod theme;
mod data;
mod keymap;
mod focus;
mod components;

use std::path::Path;

use crate::error::AppError;

pub fn run(root: &Path) -> Result<(), AppError> {
    app::run(root)
}
