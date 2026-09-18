use crate::llm::ToolCall;

/// A stable, model-independent view of useful response output.
///
/// Opaque reasoning state and provider bookkeeping are intentionally not
/// exposed here, because BONE never sends an assistant turn back to a provider.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum OutputItem {
    Text(String),
    ToolCall(ToolCall),
    ReasoningSummary(String),
}
