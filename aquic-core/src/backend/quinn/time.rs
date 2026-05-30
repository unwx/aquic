use crate::{
    backend::quinn::{Connection, connection::QuinnConnectionId},
    util::{TimerKey, TimerTick, WheelTimer},
};
use std::time::{Duration, Instant};

pub(super) struct Time {
    /// Timer to schedule and poll events.
    timer: WheelTimer<Option<TimerEvent>>,

    /// Fired events that are ready to be handled.
    fired_events: Vec<TimerTick<Option<TimerEvent>>>,

    /// Current monotonical time.
    clock: Instant,
}

impl Time {
    pub fn new() -> Self {
        let clock = Instant::now();

        let fired_events = Vec::new();
        let timer = WheelTimer::new(Duration::from_millis(1), Duration::from_millis(64), clock);

        Self {
            timer,
            fired_events,
            clock,
        }
    }


    /// Schedules an event to fire at the specified time.
    #[inline]
    pub fn schedule(&mut self, event: TimerEvent, at: Instant) -> TimerKey {
        self.timer.schedule(Some(event), at, self.clock)
    }

    /// Cancels a previously scheduled event and replaces it with a new one.
    pub fn reschedule(
        &mut self,
        event: TimerEvent,
        at: Instant,
        mut previous_key: Option<TimerKey>,
    ) -> TimerKey {
        if let Some(key) = previous_key.take() {
            self.timer.cancel(key);
        }

        self.schedule(event, at)
    }

    /// Cancels all events related to this connection.
    pub fn cancel_all(&mut self, connection: &mut Connection) {
        if let Some(key) = connection.pop_timer_key() {
            self.timer.cancel(key);
        }
    }


    /// Ticks, storing all fired events.
    /// In case of time-lag, previous events are handled too.
    ///
    /// Fired events can be retrived via [`fired_events()`][Self::fired_events].
    ///
    /// Returns the instant at which this method should be called again,
    /// and whether there are pending [`fired_events()`][Self::fired_events] to be handled.
    pub fn tick(&mut self) -> (Instant, bool) {
        let next_tick_instant = self.timer.progress(&mut self.fired_events, self.clock);
        (next_tick_instant, !self.fired_events.is_empty())
    }

    /// Returns all fired events, that are ready to be handled.
    pub fn fired_events(&mut self) -> &mut Vec<TimerTick<Option<TimerEvent>>> {
        &mut self.fired_events
    }


    /// Updates internal clocks.
    pub fn update_clock(&mut self) {
        self.clock = Instant::now();
    }

    /// Returns current monotonical time.
    pub fn now(&self) -> Instant {
        self.clock
    }
}


/*
 * Quinn proto doesn't support ability to send transport errors,
 * therefore we cannot provide `EstablishTimeout` timer,
 * as we cannot refuse the connection manually after it was accepted by `quinn_proto::Endpoint`.
 */

#[derive(Debug, Copy, Clone)]
pub(super) struct TimerEvent(pub QuinnConnectionId);
