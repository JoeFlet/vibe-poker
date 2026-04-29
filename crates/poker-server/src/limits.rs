//! Per-connection idle / rate-limit policy (step 22b).
//!
//! Two bounds, both applied in the reader loop:
//!
//! - **Idle timeout.** If no inbound frame arrives within
//!   `idle_timeout`, the connection is torn down. Clients can keep
//!   a long-lived connection alive by sending [`ClientMessage::Heartbeat`]s.
//! - **Inbound rate limit.** A token bucket caps how many frames a
//!   single peer can submit. The intent is not to throttle real
//!   players (one action every few seconds is typical) but to stop
//!   a misbehaving peer from filling the read path with garbage at
//!   line rate.
//!
//! [`ClientMessage::Heartbeat`]: poker_engine::net::protocol::ClientMessage::Heartbeat

use std::time::{Duration, Instant};

/// Tunables for [`ConnectionLimits`]. The defaults are deliberately
/// generous — anything stricter risks cutting a slow human off mid-hand.
#[derive(Clone, Copy, Debug)]
pub struct ConnectionLimits {
    /// Drop the connection if no frame arrives in this window.
    pub idle_timeout: Duration,
    /// Maximum tokens the bucket can hold (burst capacity).
    pub rate_burst: u32,
    /// Steady-state refill rate, tokens per second.
    pub rate_refill_per_sec: u32,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            // 60s without so much as a `Heartbeat` is plenty: the
            // client can hold the connection forever by tapping a
            // heartbeat once a minute.
            idle_timeout: Duration::from_secs(60),
            // Bursts: 30 frames in quick succession (e.g. catching up
            // on a chatty hand). Steady: 20/s sustained.
            rate_burst: 30,
            rate_refill_per_sec: 20,
        }
    }
}

/// Continuous-refill token bucket. `try_consume` returns false when
/// the caller is over budget.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(limits: ConnectionLimits) -> Self {
        Self {
            capacity: limits.rate_burst as f64,
            refill_per_sec: limits.rate_refill_per_sec as f64,
            tokens: limits.rate_burst as f64,
            last_refill: Instant::now(),
        }
    }

    /// Attempt to consume one token. On success, returns true and
    /// debits the bucket. On failure, returns false; the bucket is
    /// unchanged so a retry after refill will succeed.
    pub fn try_consume(&mut self) -> bool {
        self.refill(Instant::now());
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
            self.last_refill = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_bucket_allows_burst_then_refuses() {
        let limits = ConnectionLimits {
            idle_timeout: Duration::from_secs(1),
            rate_burst: 4,
            rate_refill_per_sec: 1,
        };
        let mut b = TokenBucket::new(limits);
        for _ in 0..4 {
            assert!(b.try_consume());
        }
        assert!(!b.try_consume(), "5th frame in a tight loop should be denied");
    }

    #[test]
    fn refills_at_configured_rate() {
        let limits = ConnectionLimits {
            idle_timeout: Duration::from_secs(1),
            rate_burst: 2,
            rate_refill_per_sec: 1000,
        };
        let mut b = TokenBucket::new(limits);
        assert!(b.try_consume());
        assert!(b.try_consume());
        assert!(!b.try_consume());
        std::thread::sleep(Duration::from_millis(20));
        assert!(b.try_consume(), "after 20ms @ 1000 tps the bucket should be full again");
    }
}
