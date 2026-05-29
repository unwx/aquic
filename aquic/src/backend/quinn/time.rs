use crate::{
    backend::quinn::{Connection, connection::QuinnConnectionId},
    runtime::AsyncRuntime,
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


    /// Sleeps until there is a scheduled event to handle.
    ///
    /// # Cancel Safety
    ///
    /// Cancel safe, no side effects.
    pub async fn sleep<AR: AsyncRuntime>(&mut self) {
        self.timer
            .next::<AR>(&mut self.fired_events, self.clock)
            .await;
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
