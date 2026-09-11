//! Text editing primitives shared by every composer draft.
//!
//! Product state owns the text so drafts can be persisted without translating
//! between models. [`EditBuffer`] temporarily borrows that state and is the
//! only place that mutates text, cursor, selection and undo history together.

mod buffer;
mod layout;

pub(crate) use buffer::{EditBuffer, EditorState, floor_grapheme_boundary};
pub(crate) use layout::{
    cursor_at_origin, cursor_at_single_line, editor_rows, selection_cells,
    single_line_editor_viewport, single_line_selection_cells, stable_editor_viewport,
};
