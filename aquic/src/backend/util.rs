use slab::Slab;
use std::time::{Duration, Instant};

use crate::exec::Runtime;

/// A timer queue which allows events
/// to be scheduled for execution at some later point.
pub struct TimerWheel<T> {
    slots: Vec<Vec<(T, TimerKey)>>,
    entries: Slab<TimerEntry>,

    tick_duration: Duration,
    current_tick: u64,
    mask: usize,
}

impl<T> TimerWheel<T> {
    /// Create a new instance of `TimerWheel`.
    ///
    /// * `tick_duration`: what real duration does a single tick represent.
    /// * `max_schedule`: what is the maximum time event can be scheduled for.
    pub fn new(tick_duration: Duration, max_schedule: Duration) -> Self {
        assert!(tick_duration.as_millis() > 0, "tick_duration must be > 0");
        assert!(max_schedule.as_millis() > 0, "max_schedule must be > 0");

        let total_ticks = max_schedule.as_millis() / tick_duration.as_millis();
        let size = (total_ticks as u64 + 1).next_power_of_two() as usize;

        Self {
            slots: (0..size).map(|_| Vec::with_capacity(4)).collect(),
            entries: Slab::with_capacity(64),
            tick_duration,
            current_tick: 0,
            mask: size - 1,
        }
    }

    /// Schedule next event.
    ///
    /// * `event`: event to schedule.
    /// * `previous_val_key`: a previous key of the related event, that needs to be invalidated.
    /// * `duration_ticks` duration in ticks.
    ///
    /// # Panics
    ///
    /// If `duration_ticks` exceeds `max_schedule` value that was set at [`new`](TimerWheel::new) method.
    pub fn schedule(&mut self, event: T, duration_ticks: u64) -> TimerKey {
        assert!(
            duration_ticks <= self.mask as u64,
            "duration_ticks ({}) exceeds wheel capacity {}, please correctly estimate the capacity in the `new()` method",
            duration_ticks,
            self.mask + 1
        );

        let expiry = self.current_tick + duration_ticks;
        let slot_index = (expiry as usize) & self.mask;

        let bucket = &mut self.slots[slot_index];
        let bucket_index = bucket.len();

        let entry_index = self.entries.insert(TimerEntry {
            expiry,
            slot_index,
            bucket_index,
        });
        let timer_key = TimerKey(entry_index);

        bucket.push((event, timer_key));
        timer_key
    }

    /// Schedule next event, with `Instant` as a parameter instead of ticks.
    ///
    /// If it is impossible to precisely convert duration of `at - now` to ticks,
    /// it will be rounded towards positive infinity.
    ///
    /// Therefore, event won't be available sooner than `Instant`.
    ///
    /// See: [`schedule`](Self::schedule).
    pub fn schedule_instant_ceil(&mut self, event: T, now: Instant, at: Instant) -> TimerKey {
        let duration = at.saturating_duration_since(now);
        let ticks = duration
            .as_millis()
            .div_ceil(self.tick_duration.as_millis());

        self.schedule(event, ticks as u64)
    }

    /// Cancel a scheduled event with the specified key.
    pub fn cancel(&mut self, key: TimerKey) {
        let Some(entry) = self.entries.get(key.0) else {
            return;
        };

        let bucket = &mut self.slots[entry.slot_index];
        if !bucket.is_empty() {
            let index_to_remove = entry.bucket_index;
            let last_index = bucket.len() - 1;

            if index_to_remove != last_index {
                let moved_timer_key = bucket[last_index].1;
                bucket.swap_remove(index_to_remove);

                // After the `swap_remove()`
                // item that was at `last_index` is now at `index_to_remove`.

                if let Some(moved_entry) = self.entries.get_mut(moved_timer_key.0) {
                    moved_entry.bucket_index = index_to_remove;
                }
            } else {
                bucket.pop();
            }
        }

        self.entries.remove(key.0);
    }

    /// Make next tick.
    ///
    /// Writes all scheduled events for this tick into `out` vector.
    pub fn tick(&mut self, out: &mut Vec<T>) -> usize {
        let slot_index = (self.current_tick as usize) & self.mask;
        self.current_tick += 1;

        let slot = &mut self.slots[slot_index];
        let count = slot.len();

        for (event, key) in slot.drain(..) {
            out.push(event);

            if self.entries.contains(key.0) {
                self.entries.remove(key.0);
            }
        }

        count
    }

    /// Waits for the next [`tick`](Self::tick) until it's not empty.
    ///
    /// # Cancel Safety
    ///
    /// Cancel safe. If this method was interrupted at `sleep()` point,
    /// the `next_tick_at` variable should contain an updated time when the next tick should happen.
    pub async fn next(&mut self, out: &mut Vec<T>, next_tick_at: &mut Instant) -> usize {
        loop {
            Runtime::sleep_until(*next_tick_at).await;
            *next_tick_at += self.tick_duration;

            let count = self.tick(out);
            if count != 0 {
                return count;
            };
        }
    }
}


/// A key for a scheduled value.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TimerKey(usize);


#[derive(Debug, Copy, Clone)]
struct TimerEntry {
    pub expiry: u64,
    pub slot_index: usize,
    pub bucket_index: usize,
}
