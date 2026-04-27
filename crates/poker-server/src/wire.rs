//! Async read/write helpers for the length-prefixed msgpack frames
//! defined in [`poker_engine::net::frame`].
//!
//! The framing logic itself lives in `poker-engine` (sync, no tokio).
//! These wrappers add `tokio::io` glue: read a length prefix, read the
//! payload, hand it back as a deserialized `T`. Decoupling lets the
//! engine stay transport-agnostic and lets a future sync client reuse
//! the same wire format.

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use poker_engine::net::frame::{
    self, FrameError, LENGTH_PREFIX_BYTES,
};

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame error: {0}")]
    Frame(#[from] FrameError),
    #[error("connection closed")]
    Closed,
}

/// Read one framed message from `reader`. Returns `Closed` if the peer
/// shut the connection cleanly between frames; bubbles up I/O errors
/// otherwise.
pub async fn read_message<R, T>(reader: &mut R) -> Result<T, WireError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; LENGTH_PREFIX_BYTES];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(WireError::Closed);
        }
        Err(e) => return Err(WireError::Io(e)),
    }
    let len = frame::parse_length_prefix(len_buf)?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(frame::decode(&payload)?)
}

/// Encode and write one message in a single `write_all`.
pub async fn write_message<W, T>(writer: &mut W, value: &T) -> Result<(), WireError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = frame::encode(value)?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
