#![forbid(unsafe_code)]

pub mod layout;
pub mod state;
pub mod terminal;
pub mod view;

mod run;
mod text;

pub use run::{RunError, run};
