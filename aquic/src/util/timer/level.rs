use std::{
    cell::{RefCell, RefMut},
    rc::Rc,
};

/// An independent wheel timer level.
#[derive(Debug)]
pub(super) struct Level<T> {
    /// Scheduled event buckets, one for each tick.
    buckets: Rc<RefCell<Box<[Vec<T>]>>>,

    /// Current bucket index.
    index: usize,

    /// Current epoch.
    ///
    /// Every full buckets iteration increments epoch by one.
    epoch: usize,
}

impl<T> Level<T> {
    /// Creates a new `Level`.
    ///
    /// # Panics
    ///
    /// If buckets_count is not a power of two.
    pub fn new(buckets_count: usize) -> Self {
        assert!(
            buckets_count.is_power_of_two(),
            "buckets_count must be a power of two"
        );

        let buckets = Rc::new(RefCell::new(
            (0..buckets_count)
                .map(|_| Vec::new())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ));

        Self {
            buckets,
            index: 0,
            epoch: 0,
        }
    }

    /// Schedules a next event, returning a key, which can be used to cancel it later.
    ///
    /// # Warning
    ///
    /// The caller must guarantee that `duration_ticks` is less than the number of buckets in this level,
    /// otherwise unspecified behavior may occur.
    pub fn schedule(&mut self, event: T, duration_ticks: usize) -> LevelKey {
        let mut buckets = self.buckets.borrow_mut();
        debug_assert!(duration_ticks < buckets.len());

        let bucket_index = self.index.wrapping_add(duration_ticks) & Self::mask(&buckets);
        let epoch = self
            .epoch
            .wrapping_add((bucket_index < self.index) as usize);

        let element_index = buckets[bucket_index].len();
        buckets[bucket_index].push(event);

        LevelKey {
            bucket_index,
            element_index,
            epoch,
        }
    }

    /// Ticks, detaching a bucket of scheduled events for the current tick.
    ///
    /// It is recommended to drop the returned bucket as soon as possible,
    /// to avoid unnecessary allocations.
    pub fn tick(&mut self) -> Bucket<T> {
        let mut buckets = self.buckets.borrow_mut();

        let elements = std::mem::take(&mut buckets[self.index]);
        let bucket = Bucket {
            elements,
            source: self.buckets.clone(),
            source_index: self.index,
        };

        self.index = self.index.wrapping_add(1) & Self::mask(&buckets);
        self.epoch = self.epoch.wrapping_add((self.index == 0) as usize);

        bucket
    }


    fn mask(buckets: &RefMut<Box<[Vec<T>]>>) -> usize {
        debug_assert!(buckets.len().is_power_of_two());
        buckets.len() - 1
    }
}

impl<T: Default> Level<T> {
    /// Cancels a scheduled event with the specified key.
    ///
    /// The scheduled event will be replaced with a default value.
    pub fn cancel(&mut self, key: LevelKey) {
        if key.epoch < self.epoch {
            return;
        }
        if key.epoch == self.epoch {
            if key.bucket_index < self.index {
                return;
            }
        }

        let mut buckets = self.buckets.borrow_mut();
        let elements = &mut buckets[key.bucket_index];

        if key.element_index < elements.len() {
            elements[key.element_index] = T::default();
        }
    }
}

/// A key for a scheduled event, used to cancel the event.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) struct LevelKey {
    bucket_index: usize,
    element_index: usize,
    epoch: usize,
}

/// A previously detached from source bucket of scheduled events.
#[derive(Debug)]
pub(super) struct Bucket<T> {
    pub(super) elements: Vec<T>,
    source: Rc<RefCell<Box<[Vec<T>]>>>,
    source_index: usize,
}

impl<T> Drop for Bucket<T> {
    fn drop(&mut self) {
        let mut bucket_elements = std::mem::take(&mut self.elements);
        bucket_elements.clear();

        let mut source = self.source.borrow_mut();
        let source_elements = &mut source[self.source_index];

        if source_elements.is_empty() && source_elements.capacity() < bucket_elements.capacity() {
            *source_elements = bucket_elements;
        }
    }
}
