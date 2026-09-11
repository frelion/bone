#![forbid(unsafe_code)]

pub mod layout;
pub mod state;
pub mod view;

mod editor;
mod input;
mod run;
mod terminal;
mod text;
mod ui;

pub use run::{RunError, run};
