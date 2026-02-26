use std::time::{Duration, Instant};

use crate::backend::{
    quinn::{Config, QuinnConnectionId},
    util::TimerWheel,
};

pub(super) struct Time {
    /// Scheduled events.
    pub timer: TimerWheel<TimeoutEvent>,

    /// Fired events.
    pub fired_events: Vec<TimeoutEvent>,

    /// When the next `timer` tick should happen.
    pub timer_next_tick_time: Instant,

    /// Internal clock, shows current time.
    pub clock: Instant,
}

impl Time {
    pub fn new(config: &Config) -> Self {
        debug_assert!(
            config.max_timeout_miss.as_millis() > 0,
            "provided config is invalid: 'max_timeout_miss.as_millis()' is zero"
        );

        let clock = Instant::now();

        let timer = if config.max_idle_timeout.as_millis() != 0 {
            TimerWheel::new(config.max_timeout_miss, config.max_idle_timeout)
        } else {
            TimerWheel::with_capacity(Duration::from_hours(1), Duration::from_hours(1), 0, 0)
        };

        Self {
            timer,
            fired_events: Vec::new(),
            timer_next_tick_time: clock,
            clock,
        }
    }
}


/// QUIC connection timed out.
#[derive(Debug, Copy, Clone)]
pub(super) struct TimeoutEvent(pub QuinnConnectionId);
