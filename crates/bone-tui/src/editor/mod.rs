//! Text editing primitives shared by the composer and session title.
//!
//! [`EditorBuffer`] owns one document's text, cursor, revision, selection and
//! undo history so those values cannot drift apart when a draft changes owner.

mod buffer;
mod layout;

pub(crate) use buffer::{EditorBuffer, floor_grapheme_boundary};
pub(crate) use layout::{
    cursor_at_origin, cursor_at_single_line, editor_rows, selection_cells,
    single_line_editor_viewport, single_line_selection_cells, stable_editor_viewport,
};
