#![forbid(unsafe_code)]

mod layout;
mod state;
mod view;

mod editor;
mod input;
mod run;
mod terminal;
mod text;
mod ui;

pub use run::{RunError, run};

#[cfg(test)]
mod tests;
