use slab::Slab;
use std::time::Duration;

/// A timer queue which allows events
/// to be scheduled for execution at some later point.
struct TimerWheel<T> {
    slots: Vec<Vec<(T, TimerKey)>>,
    entries: Slab<TimerEntry>,

    current_tick: u64,
    mask: usize,
}

impl<T> TimerWheel<T> {
    const DEFAULT_BUCKET_SIZE: usize = 4;

    /// Create a new instance of `TimerWheel`.
    ///
    /// Parameters:
    /// Note, they required only to calculate a number of buckets,
    /// `TimerWheel` does not have a sense of time and doesn't adjust according to current time.
    ///
    /// * `tick_duration`: what real duration does a single tick represent.
    /// * `max_schedule`: in real time, what is the maximum for
    pub fn new(tick_duration: Duration, max_schedule: Duration) -> Self {
        assert!(tick_duration.as_millis() > 0, "tick_duration must be > 0");
        assert!(max_schedule.as_millis() > 0, "max_schedule must be > 0");

        let total_ticks = max_schedule.as_millis() / tick_duration.as_millis();
        let size = (total_ticks as u64 + 1).next_power_of_two() as usize;

        Self {
            slots: (0..size)
                .map(|_| Vec::with_capacity(Self::DEFAULT_BUCKET_CAPACITY))
                .collect(),
            entries: Slab::with_capacity(64),
            current_tick: 0,
            mask: size - 1,
        }
    }

    /// Schedule next value.
    ///
    /// * `value`: value to schedule.
    /// * `previous_val_key`: a previous key of the related value, that needs to be invalidated.
    /// * `duration_ticks` duration in ticks.
    ///
    /// # Panics
    ///
    /// If `duration_ticks` exceeds `max_schedule` value that was set at [`new`](TimerWheel::new) method.
    pub fn schedule(
        &mut self,
        value: T,
        duration_ticks: u64,
        previous_val_key: Option<TimerKey>,
    ) -> TimerKey {
        if let Some(key) = previous_val_key {
            self.cancel(key);
        }

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

        bucket.push((value, timer_key));
        timer_key
    }

    /// Cancel a scheduled value with the specified key.
    pub fn cancel(&mut self, key: TimerKey) {
        let Some(entry) = self.entries.get(key.0) else {
            return;
        };

        let bucket = &mut self.slots[entry.slot_index];
        if bucket.len() > 0 {
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
    /// Writes all scheduled values for this tick into `out` vector.
    pub fn tick(&mut self, out: &mut Vec<T>) -> usize {
        let slot_index = (self.current_tick as usize) & self.mask;
        self.current_tick += 1;

        let slot = &mut self.slots[slot_index];
        let count = slot.len();

        for (value, key) in slot.drain(..) {
            out.push(value);

            if self.entries.contains(key.0) {
                self.entries.remove(key.0);
            }
        }
        count
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
