use std::time::{Duration, Instant};

use crate::backend::{Config, quinn::QuinnConnectionId, util::TimerWheel};

pub(super) struct Time {
    /// Scheduled timeout events.
    pub timeout_timer: Option<TimerWheel<TimeoutEvent>>,

    /// Fired timeout events
    pub timeout_events: Vec<TimeoutEvent>,

    /// Duration of the single `timeout_timer` tick.
    pub timeout_tick_duration: Duration,

    /// Where the next timeout tick should happen.
    pub next_timeout_tick: Option<Instant>,

    /// Internal clock.
    pub clock: Instant,
}

impl Time {
    pub fn new(config: &Config) -> Self {
        let timeout_timer = if config.max_idle_timeout.as_millis() == 0 {
            Some(TimerWheel::new(
                config.max_timeout_miss,
                config.max_idle_timeout,
            ))
        } else {
            None
        };

        Self {
            timeout_timer,
            timeout_events: Vec::new(),
            timeout_tick_duration: config.max_idle_timeout,
            next_timeout_tick: None,
            clock: Instant::now(),
        }
    }
}


/// QUIC connection timed out.
#[derive(Debug, Copy, Clone)]
pub struct TimeoutEvent(pub QuinnConnectionId);
