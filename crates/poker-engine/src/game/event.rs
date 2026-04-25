use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::Card;
use crate::game::action::Action;
use crate::game::state::{HandId, SeatIndex, Street};

/// Every meaningful moment in a hand is emitted as an event.
/// The engine owns no log — callers consume events via an EventSink.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EngineEvent {
    HandStarted {
        hand_id: HandId,
        dealer: SeatIndex,
        /// Seed used to shuffle this hand's deck. Pin this to replay.
        deck_seed: u64,
    },
    HoleCardsDealt {
        seat: SeatIndex,
        cards: [Card; 2],
    },
    BoardDealt {
        street: Street,
        cards: Vec<Card>,
    },
    ActionTaken {
        seat: SeatIndex,
        action: Action,
        /// Total pot size after the action (main + all side pots).
        pot_total: u32,
    },
    PlayerAllIn {
        seat: SeatIndex,
        total_committed: u32,
    },
    HandEnded {
        hand_id: HandId,
        result: HandResult,
    },
}

/// Full outcome of a hand, emitted at HandEnded.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandResult {
    pub hand_id: HandId,
    pub board: Vec<Card>,
    /// Each entry: (seat, hole_cards_if_shown, chip_delta).
    pub seats: Vec<SeatOutcome>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeatOutcome {
    pub seat: SeatIndex,
    pub hole_cards: Option<[Card; 2]>,
    /// Positive = won chips, negative = lost chips this hand.
    pub chip_delta: i32,
    /// True when this seat did not play a live hand (e.g. dead hand). Stats
    /// consumers should exclude such seats from per-hand metrics.
    #[serde(default)]
    pub sat_out: bool,
}

/// Consumers implement this to receive events.
pub trait EventSink: Send {
    fn on_event(&mut self, event: &EngineEvent);
}

/// Default: discards all events. Zero overhead.
pub struct NullSink;
impl EventSink for NullSink {
    #[inline]
    fn on_event(&mut self, _: &EngineEvent) {}
}

/// Collects events into a Vec. Useful for tests and replay capture.
#[derive(Default)]
pub struct VecSink {
    pub events: Vec<EngineEvent>,
}
impl EventSink for VecSink {
    fn on_event(&mut self, event: &EngineEvent) {
        self.events.push(event.clone());
    }
}

/// Streams events to a MessagePack file for post-hoc analysis and replay.
///
/// Format: each event is a length-prefixed frame —
/// `[u32 LE length][rmp-serde bytes]` — allowing streaming reads.
///
/// The file is flushed when the sink is dropped.
pub struct FileSink {
    writer: BufWriter<File>,
}

impl FileSink {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(FileSink { writer: BufWriter::new(file) })
    }

    /// Explicitly flush the underlying write buffer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl EventSink for FileSink {
    fn on_event(&mut self, event: &EngineEvent) {
        let bytes = rmp_serde::to_vec(event).expect("event serialization failed");
        let len = bytes.len() as u32;
        // Ignore write errors here; callers who need guarantees should call flush().
        let _ = self.writer.write_all(&len.to_le_bytes());
        let _ = self.writer.write_all(&bytes);
    }
}

impl Drop for FileSink {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

/// Read all events from a log previously written by `FileSink`.
///
/// The format is `[u32 LE length][msgpack bytes]` per event. Returns the
/// full list; this is fine for interactive replay (single-hand and
/// multi-hand logs are small), and simple for callers that just want
/// random access by index. Streaming readers can be added later if we
/// ever feed logs larger than memory.
pub fn read_event_log(path: impl AsRef<Path>) -> io::Result<Vec<EngineEvent>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut len_buf = [0u8; 4];
    loop {
        match reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut frame = vec![0u8; len];
        reader.read_exact(&mut frame)?;
        let event: EngineEvent = rmp_serde::from_slice(&frame)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_sink_roundtrip_via_read_event_log() {
        let mut path = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        path.push(format!("event_log_{}_{}.mp", std::process::id(), nonce));

        {
            let mut sink = FileSink::create(&path).expect("create sink");
            sink.on_event(&EngineEvent::HandStarted {
                hand_id: 1,
                dealer: 0,
                deck_seed: 42,
            });
            sink.on_event(&EngineEvent::BoardDealt {
                street: Street::Flop,
                cards: vec![],
            });
        }

        let events = read_event_log(&path).expect("read log");
        let _ = std::fs::remove_file(&path);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], EngineEvent::HandStarted { hand_id: 1, .. }));
        assert!(matches!(events[1], EngineEvent::BoardDealt { street: Street::Flop, .. }));
    }
}
