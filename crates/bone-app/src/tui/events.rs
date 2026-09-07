//! A JSON Lines consumer of the public observation port.

use std::io;

use bone_agent::Observation;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::broadcast::error::RecvError,
};

/// Write the atomic baseline, then live steps until the runtime closes.
/// A slow writer affects only this consumer. Missing steps are explicit gap
/// records; this file is an observation log, not a durable recovery journal.
pub async fn write_events(
    mut observation: Observation,
    mut writer: impl AsyncWrite + Unpin,
) -> io::Result<()> {
    write_line(
        &mut writer,
        json!({
            "type": "snapshot",
            "sequence": observation.sequence,
            "snapshot": observation.snapshot,
        }),
    )
    .await?;
    loop {
        match observation.events.recv().await {
            Ok(step) => {
                write_line(&mut writer, json!({"type": "step", "step": &*step})).await?;
            }
            Err(RecvError::Lagged(missed)) => {
                write_line(&mut writer, json!({"type": "gap", "missed_steps": missed})).await?;
            }
            Err(RecvError::Closed) => return writer.flush().await,
        }
    }
}

async fn write_line(writer: &mut (impl AsyncWrite + Unpin), value: Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}
