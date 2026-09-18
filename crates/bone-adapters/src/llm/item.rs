use std::fmt;

use rig_core::message::{Message, Text, UserContent};

/// Who supplied an input item.
///
/// This is attribution, not authority. Named participants are encoded as
/// ordinary external input and can never become model instructions.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputSource {
    User,
    Named(String),
}

/// One ordered item supplied to a model request.
///
/// There is exactly one kind of input: text attributed to a source. An
/// assistant turn is never an input, because the kernel rebuilds every turn's
/// context instead of replaying provider history.
#[derive(Clone, PartialEq)]
pub struct InputItem {
    pub(crate) source: InputSource,
    pub(crate) text: String,
}

impl fmt::Debug for InputItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InputItem")
            .field("source", &self.source)
            .field("content", &"<redacted>")
            .finish()
    }
}

impl InputItem {
    /// External text attributed to a human or another named participant.
    pub fn external(source: InputSource, text: impl Into<String>) -> Self {
        Self {
            source,
            text: text.into(),
        }
    }

    /// Encode this item as the wire message a provider receives.
    pub(crate) fn into_message(self) -> Message {
        let text = match self.source {
            InputSource::User => self.text,
            InputSource::Named(name) => {
                let encoded =
                    serde_json::to_string(&name).expect("serializing a string as JSON cannot fail");
                format!(
                    "<bone_external source={encoded}>\n{}\n</bone_external>",
                    self.text
                )
            }
        };
        Message::User {
            content: vec![UserContent::Text(Text::new(text))],
        }
    }
}
