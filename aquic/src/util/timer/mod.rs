use crate::{
    runtime::AsyncRuntime,
    util::timer::level::{Level, LevelKey},
};
use std::{
    ops::{Deref, DerefMut},
    time::{Duration, Instant},
};

mod level;

/// A cancellable wheel timer implementation.
///
/// ## Architecture & Scaling
///
/// The timer is composed of multiple hierarchical `Level`s. Each level acts as an
/// independent ring buffer covering a specific time span.
///
/// - **Level 0** is the base level, defined by `start_hz` (tick duration) and `start_coverage` (max duration).
/// - **Level N** dynamically scales by a factor of 2 for each step up:
///   - Its tick duration (precision) becomes `start_hz * 2^N`.
///   - Its total coverage (max duration) becomes `start_coverage * 2^N`.
///
/// ## Performance
/// - Scheduling: `O(1)`. Events are mapped directly to their final destination bucket.
/// - Cancellation: `O(1)`. Requires no searching; the event is simply replaced with a default value.
/// - Execution: `O(1)` amortized per tick, `O(n)` where `n` is the number of ticks, which is usually `1` unless there is a time lag.
///   For each tick `WheelTimer` detaches a bucket of fired events directly to the caller, without copying it.
#[derive(Debug)]
pub struct WheelTimer<T> {
    /// Timer layers.
    ///
    /// Each covers twice as much time as the previous one, but has half the precision.
    levels: Vec<Level<T>>,

    /// Current absolute tick count.
    ///
    /// Used via trailing zeros to determine which higher levels should process a tick.
    current_tick: usize,

    /// Current clock, used to determine when the next tick is ready.
    current_clock: Instant,

    /// Initial tick duration (precision of Level 0), doubled for each subsequent level.
    start_hz: Duration,

    /// Initial time span covered by Level 0, doubled for each subsequent level.
    start_coverage: Duration,
}

impl<T> WheelTimer<T> {
    /// Creates a new `WheelTimer`.
    ///
    /// # Panics
    ///
    /// Panics if `start_coverage` is less than `start_hz`.
    pub fn new(start_hz: Duration, start_coverage: Duration, now: Instant) -> Self {
        assert!(
            start_coverage >= start_hz,
            "coverage must be at least the size of one tick (hz)"
        );

        let mut this = Self {
            levels: Vec::new(),
            current_tick: 0,
            current_clock: now,
            start_hz,
            start_coverage,
        };

        this.extend_levels(1);
        this
    }

    /// Schedules a next event.
    ///
    /// It is guaranteed that the event won't be available sooner than `at`.
    pub fn schedule(&mut self, event: T, at: Instant, now: Instant) -> TimerKey {
        let duration = at.saturating_duration_since(now);

        let level_index = self.find_index_by_duration(duration);
        if level_index >= self.levels.len() {
            self.extend_levels(level_index + 1);
        }

        let duration_ticks = self.duration_to_level_ticks(duration, level_index);
        let level = &mut self.levels[level_index];
        let level_key = level.schedule(event, duration_ticks);

        TimerKey {
            level_index,
            level_key,
        }
    }

    /// Waits until a scheduled event fires, collecting them into `out`.
    ///
    /// If a time-lag occurred, all previous ticks will be processed sequentially.
    ///
    /// # Cancel Safety
    ///
    /// Cancel safe, no side effects.
    pub async fn next<AR: AsyncRuntime>(&mut self, out: &mut Vec<TimerTick<T>>, now: Instant) {
        let out_start_len = out.len();

        {
            let mut clock = self.current_clock;

            while clock <= now {
                self.tick(out);

                self.current_clock = clock;
                clock += self.start_hz;
            }
        }

        while out.len() == out_start_len {
            let alarm = self.current_clock + self.start_hz;

            while self.current_clock < alarm {
                AR::sleep_until(alarm).await;
                self.current_clock = Instant::min(alarm, Instant::now());
            }

            self.tick(out);
        }
    }


    fn extend_levels(&mut self, desired_length: usize) {
        let required_buckets = {
            if self
                .start_coverage
                .as_nanos()
                .is_multiple_of(self.start_hz.as_nanos())
            {
                self.start_coverage.as_nanos() / self.start_hz.as_nanos()
            } else {
                self.start_coverage
                    .as_nanos()
                    .div_ceil(self.start_hz.as_nanos())
                    + 1
            }
        };

        let buckets_count = required_buckets.next_power_of_two().max(2) as usize;

        for _ in self.levels.len()..desired_length {
            self.levels.push(Level::new(buckets_count));
        }

        // Reset the current tick,
        // to prevent the next tick from processing new levels leading to premature firing.
        self.current_tick = 0;
    }

    fn tick(&mut self, out: &mut Vec<TimerTick<T>>) {
        // `current_tick` never starts with zero: it may be zero only if it overflows.
        self.current_tick = self.current_tick.wrapping_add(1);

        // Level `i` processes every 2^i ticks.
        //
        // Examples:
        // `current_tick` = 1 (...0001); `trailing_zeros` = 0; only Level 0 processes the tick.
        // `current_tick` = 2 (...0010); `trailing_zeros` = 1. Levels 0 and 1 process the tick.
        // `current_tick` = 3 (...0011); `trailing_zeros` = 0.
        // `current_tick` = 4 (...0100); `trailing_zeros` = 2. Levels 0, 1 and 2 process the tick.
        // `current_tick` = 0 `(...0000); `trailing_zeros` = usize::BITS; each possible levels processes the tick.
        let trailing_zeros = self.current_tick.trailing_zeros() as usize;

        for i in 0..=usize::min(trailing_zeros, self.levels.len() - 1) {
            out.push(self.levels[i].tick().into());
        }
    }


    fn find_index_by_duration(&self, duration: Duration) -> usize {
        let nanos = duration.as_nanos();
        let base_coverage = self.start_coverage.as_nanos();

        if nanos < base_coverage {
            0
        } else {
            // (nanos / base) gives the multiplier.
            // ilog2() bins factors of 2 into the respective level indices.
            (nanos / base_coverage).ilog2() as usize + 1
        }
    }

    fn duration_to_level_ticks(&self, duration: Duration, level_index: usize) -> usize {
        let level_hz_nanos = self.start_hz.as_nanos() * (1 << level_index);
        duration.as_nanos().div_ceil(level_hz_nanos) as usize
    }
}

impl<T: Default> WheelTimer<T> {
    /// Cancels a scheduled event with the specified key.
    ///
    /// The scheduled event will be replaced with a default value.
    pub fn cancel(&mut self, key: TimerKey) {
        if key.level_index < self.levels.len() {
            self.levels[key.level_index].cancel(key.level_key);
        }
    }
}


/// A key for a scheduled event, used to cancel the event.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TimerKey {
    level_key: LevelKey,
    level_index: usize,
}


/// A detached bucket of fired scheduled events.
/// It is used for zero-copy events processing.
///
/// It is highly recommended to drop the bucket as soon as possible, to avoid unnecessary allocations.
///
/// When `WheelTimer` iterates one bucket per tick.
/// If the previous bucket was not dropped, it needs to allocate a new one.
#[derive(Debug)]
pub struct TimerTick<T>(level::Bucket<T>);

impl<T> From<level::Bucket<T>> for TimerTick<T> {
    fn from(bucket: level::Bucket<T>) -> Self {
        Self(bucket)
    }
}

impl<T> Deref for TimerTick<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.0.elements
    }
}

impl<T> DerefMut for TimerTick<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.elements
    }
}
