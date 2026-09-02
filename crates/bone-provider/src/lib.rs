//! Thin protocol endpoint and model construction on top of Rig.
//!
//! Rig owns provider clients, messages, requests, responses, and streaming.
//! BONE adds a configured [`Endpoint`], explicit [`Protocol`] identity, and a
//! cloneable type-erased [`Model`] handle. Endpoints that speak the same wire
//! protocol share one protocol implementation.

#![forbid(unsafe_code)]

mod endpoint;
mod error;
mod model;

pub mod protocol;

pub use endpoint::Endpoint;
pub use error::ConfigError;
pub use model::Model;
pub use protocol::Protocol;
pub use rig_core as rig;
pub use rig_core::{
    completion::{CompletionError, CompletionRequest, CompletionResponse},
    streaming::{StreamedAssistantContent, StreamingCompletionResponse},
};
