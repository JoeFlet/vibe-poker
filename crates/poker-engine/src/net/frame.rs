//! Length-prefixed msgpack codec.
//!
//! Wire format: `[u32 LE length][rmp-serde bytes]`. Identical to the
//! frame layout `FileSink` writes, so a server can spool its event
//! broadcasts straight into a log without re-encoding.
//!
//! All routines here operate on byte buffers — there is no I/O, no
//! `tokio`, no `std::net`. Consumers (the server, an eventual live
//! client) are expected to pair these helpers with whatever read/write
//! primitive their transport offers.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Width of the length prefix in bytes.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Hard cap on payload size to keep a misbehaving (or malicious) peer
/// from pushing the server into multi-gigabyte allocations. 4 MiB is
/// far above anything our actual protocol produces — full event logs
/// for a session are tens of KB.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Errors that can occur during framing/unframing.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame too large: {len} bytes (max {MAX_FRAME_BYTES})")]
    TooLarge { len: usize },

    #[error("encode failed: {0}")]
    Encode(#[from] rmp_serde::encode::Error),

    #[error("decode failed: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
}

/// Encode a value into a fully-framed `[length][payload]` byte vector,
/// ready to write to any `Write`-like sink in one shot.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let payload = rmp_serde::to_vec(value)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { len: payload.len() });
    }
    let mut out = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Parse the payload length out of a 4-byte prefix, applying the
/// `MAX_FRAME_BYTES` cap. Returned value is the number of payload
/// bytes the caller still needs to read.
pub fn parse_length_prefix(prefix: [u8; LENGTH_PREFIX_BYTES]) -> Result<usize, FrameError> {
    let len = u32::from_le_bytes(prefix) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { len });
    }
    Ok(len)
}

/// Decode a payload (just the msgpack body, no length prefix).
pub fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    Ok(rmp_serde::from_slice(payload)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::protocol::{ClientMessage, PROTOCOL_VERSION};

    #[test]
    fn encode_then_decode() {
        let msg = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            username: "alice".into(),
        };
        let framed = encode(&msg).unwrap();
        assert!(framed.len() > LENGTH_PREFIX_BYTES);

        let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
        prefix.copy_from_slice(&framed[..LENGTH_PREFIX_BYTES]);
        let len = parse_length_prefix(prefix).unwrap();
        assert_eq!(len, framed.len() - LENGTH_PREFIX_BYTES);

        let back: ClientMessage = decode(&framed[LENGTH_PREFIX_BYTES..]).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn rejects_oversized_prefix() {
        let bad_len = (MAX_FRAME_BYTES as u32 + 1).to_le_bytes();
        match parse_length_prefix(bad_len) {
            Err(FrameError::TooLarge { .. }) => {}
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }
}
