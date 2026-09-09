//! Concrete LLM and native-tool adapters for the BONE agent core.
//!
//! [`llm`] owns model-provider protocols and transports. [`tools`] owns the
//! native coding tools. The crate-root adapters connect both capability sets
//! to `bone-core` without leaking infrastructure dependencies into the core.

#![forbid(unsafe_code)]

mod agent;

pub mod llm;
pub mod tools;

pub use agent::{ConfiguredModel, ConfiguredModelError, ModelAdapter, read_only_tools};
