use crate::ToolCall;

/// A stable, model-independent view of useful response output.
///
/// Opaque reasoning state and provider bookkeeping are intentionally not
/// exposed here; [`crate::Response::into_item`] still preserves them for the
/// next request.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum OutputItem {
    Text(String),
    ToolCall(ToolCall),
    ReasoningSummary(String),
}
