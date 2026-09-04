//! One model interface over explicit LLM protocol endpoints.
//!
//! The public boundary is deliberately small: select a [`Model`], build one
//! ordered [`Request`], then call [`Model::complete`] or [`Model::stream`].
//! Provider clients and wire-specific request types stay behind that boundary.

#![forbid(unsafe_code)]

mod endpoint;
mod error;
mod item;
mod model;
mod output;
mod request;
mod response;
mod streaming;
mod tool;

pub mod protocol;

pub mod service;

pub use endpoint::Endpoint;
pub use error::{ConfigError, Error, ErrorKind};
pub use item::{InputItem, InputSource};
pub use model::Model;
pub use output::OutputItem;
pub use protocol::Protocol;
pub use request::{Options, OutputFormat, Request};
pub use response::{FinishReason, Response, ResponseOrigin, Usage};
pub use streaming::{ResponseStream, StreamEvent, ToolCallDelta};
pub use tool::{ToolCall, ToolChoice, ToolDefinition, ToolOutput};

#[cfg(feature = "test-utils")]
#[doc(hidden)]
pub mod testing;
