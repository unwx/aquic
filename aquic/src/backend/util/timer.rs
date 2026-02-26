use slab::Slab;
use std::{
    ops::Div,
    time::{Duration, Instant},
};

use crate::exec::Runtime;

/// A timer queue.
/// Provides an API to schedule events.
pub struct TimerWheel<T> {
    buckets: Box<[Vec<(T, TimerKey)>]>,
    address_book: Slab<Location>,

    tick: usize,
    tick_duration: Duration,
}

impl<T> TimerWheel<T> {
    /// Creates a new instance of `TimerWheel`.
    ///
    /// * `tick_duration`: what real-time duration does a single tick represent.
    /// * `max_schedule`: what is the maximum time event can be scheduled for.
    ///
    /// # Panics
    ///
    /// - If `tick_duration` is zero.
    /// - If `max_schedule` is zero.
    /// - If `tick_duration` is greater than `max_schedule`.
    /// - If resulting number of ticks is greater than `usize::MAX`.
    pub fn new(tick_duration: Duration, max_schedule: Duration) -> Self {
        Self::with_capacity(tick_duration, max_schedule, 4, 64)
    }

    /// Creates a new instance of `TimerWheel` with provided capacity.
    ///
    /// * `tick_duration`: what real-time duration does a single tick represent.
    /// * `max_schedule`: what is the maximum time event can be scheduled for.
    /// * `bucket_capacity`: wheel contains bucket for each tick, specifies the capacity for each bucket.
    /// * `address_book_capacity`: `address_book` allows revoking scheduled items using a [TimerKey], therefore contains a key for each event;
    ///   specifies its capacity.
    ///
    /// # Panics
    ///
    /// - If `tick_duration` is zero.
    /// - If `max_schedule` is zero.
    /// - If `tick_duration` is greater than `max_schedule`.
    /// - If resulting number of ticks is greater than `usize::MAX`.
    pub fn with_capacity(
        tick_duration: Duration,
        max_schedule: Duration,
        bucket_capacity: usize,
        address_book_capacity: usize,
    ) -> Self {
        assert!(!tick_duration.is_zero(), "tick_duration cannot be zero");
        assert!(!max_schedule.is_zero(), "max_schedule cannot be zero");
        assert!(
            max_schedule > tick_duration,
            "tick_duration cannot be greater than max_schedule"
        );

        let total_ticks = {
            let raw = max_schedule.as_nanos() / tick_duration.as_nanos();
            (raw as usize + 1).next_power_of_two()
        };

        assert!(
            total_ticks > 0,
            "total estimated number of ticks is zero: usize overflow"
        );

        let buckets = {
            let vec: Vec<Vec<(T, TimerKey)>> = (0..total_ticks)
                .map(|_| Vec::with_capacity(bucket_capacity))
                .collect();

            vec.into_boxed_slice()
        };
        let address_book = Slab::with_capacity(address_book_capacity);

        Self {
            buckets,
            address_book,
            tick: 0,
            tick_duration,
        }
    }

    /// Schedules next event.
    ///
    /// * `event`: event to schedule.
    /// * `duration_ticks` duration in tick, may be zero.
    ///
    /// # Panics
    ///
    /// If `duration_ticks` exceeds `max_schedule * tick_duration` value that was set at [`new`](TimerWheel::new) method.
    pub fn schedule(&mut self, event: T, duration_ticks: usize) -> TimerKey {
        assert!(
            duration_ticks <= self.max_duration_ticks(),
            "duration_ticks ({}) exceeds wheel capacity {}, please correctly estimate the capacity in the `new()` method",
            duration_ticks,
            self.max_duration_ticks()
        );

        let bucket_index = self.index_of(self.tick + duration_ticks);
        let bucket = &mut self.buckets[bucket_index];
        let bucket_element_index = bucket.len();

        let location_index = self.address_book.insert(Location {
            bucket_index,
            bucket_element_index,
        });
        let timer_key = TimerKey(location_index);

        bucket.push((event, timer_key));
        timer_key
    }

    /// Schedules next event, using real-time `Instant` instead of ticks.
    ///
    /// If it is impossible to precisely convert duration of `at - now` to ticks,
    /// it will be rounded towards positive infinity.
    ///
    /// Therefore, event won't be available sooner than `Instant`.
    ///
    /// See: [`schedule`](Self::schedule).
    pub fn schedule_at(&mut self, event: T, now: Instant, at: Instant) -> TimerKey {
        let duration = at.saturating_duration_since(now);
        let ticks = duration.as_nanos().div_ceil(self.tick_duration.as_nanos());

        self.schedule(event, ticks as usize)
    }

    /// Cancels a scheduled event with the specified key.
    pub fn cancel(&mut self, key: TimerKey) {
        let Some(location) = self.address_book.try_remove(key.0) else {
            return;
        };

        let bucket = &mut self.buckets[location.bucket_index];
        if !bucket.is_empty() {
            let index_to_remove = location.bucket_element_index;
            let last_index = bucket.len() - 1;

            if index_to_remove != last_index {
                let moved_timer_key = bucket[last_index].1;
                bucket.swap_remove(index_to_remove);

                // After the `swap_remove()`
                // item that was at `last_index` is now at `index_to_remove`.

                if let Some(moved_location) = self.address_book.get_mut(moved_timer_key.0) {
                    moved_location.bucket_element_index = index_to_remove;
                }
            } else {
                bucket.pop();
            }
        }
    }

    /// Ticks, writing all fired events into `&mut out` vector.
    ///
    /// Returns number of fired events.
    pub fn tick(&mut self, out: &mut Vec<T>) -> usize {
        let bucket = &mut self.buckets[self.index_of(self.tick)];
        let fired_count = bucket.len();

        self.tick = self.tick.wrapping_add(1);
        out.reserve(fired_count);

        for (event, key) in bucket.drain(..) {
            out.push(event);
            let _ = self.address_book.try_remove(key.0);
        }

        fired_count
    }

    /// Iterates and waits for the next non-empty [`tick`](Self::tick).
    ///
    /// If `next_tick_at` is behind `now`, all previous ticks will be processed too.
    ///
    /// On result, updates `next_tick_at` with an up-to-date value,
    /// and returns number of total fired events.
    ///
    /// # Cancel Safety
    ///
    /// Cancel safe, `next_tick_at` will be up to date.
    pub async fn next(
        &mut self,
        out: &mut Vec<T>,
        now: Instant,
        next_tick_at: &mut Instant,
    ) -> usize {
        {
            let mut count = 0;

            if now > *next_tick_at {
                let skipped_ticks = now
                    .saturating_duration_since(*next_tick_at)
                    .as_nanos()
                    // Should be the same as `div_floor()` for u128.
                    .div(self.tick_duration.as_nanos());

                for _ in 0..skipped_ticks {
                    count += self.tick(out);
                }

                *next_tick_at += self.tick_duration * (skipped_ticks as u32);
                if count != 0 {
                    return count;
                }
            }
        }

        loop {
            Runtime::sleep_until(*next_tick_at).await;
            *next_tick_at += self.tick_duration;

            let count = self.tick(out);
            if count != 0 {
                return count;
            };
        }
    }


    fn index_of(&self, tick: usize) -> usize {
        tick & (self.buckets.len() - 1)
    }

    fn max_duration_ticks(&self) -> usize {
        self.buckets.len() - 1
    }
}


/// A key for a scheduled value.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TimerKey(usize);


#[derive(Debug, Copy, Clone)]
struct Location {
    pub bucket_index: usize,
    pub bucket_element_index: usize,
}
