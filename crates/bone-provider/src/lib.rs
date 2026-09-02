//! Thin model construction on top of Rig.
//!
//! Rig owns provider clients, messages, requests, responses, and streaming.
//! BONE adds only a cloneable, type-erased [`Model`] handle and concrete
//! provider construction.

#![forbid(unsafe_code)]

mod model;

pub mod openai;

pub use model::Model;
pub use rig_core as rig;
pub use rig_core::{
    completion::{CompletionError, CompletionRequest, CompletionResponse},
    streaming::{StreamedAssistantContent, StreamingCompletionResponse},
};
