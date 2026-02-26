// TODO(test): untested unsafe code.

use crate::net::{Buf, BufMut};
use core::slice;
use std::{
    cell::{Cell, RefCell},
    cmp::{Ordering, Reverse},
    collections::BinaryHeap,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

/// Lazy, `!Send`, heap allocated Bip Buffer implementation.
/// It is expected to be used in cases where memory is decommitted in order it was committed.
/// Reserved or committed memory may be dirty (not zeroed).
///
/// The difference between Ring Buffer and Bip Buffer: Bip Buffer always returns contiguous chunks of memory.
///
/// Inspiration:
/// - [Theory (codeproject.com article)](https://www.codeproject.com/articles/The-Bip-Buffer-The-Circular-Buffer-with-a-Twist).
/// - [Impl (github.com squidpickles/bipbuffer)](https://github.com/squidpickles/bipbuffer/blob/master/src/lib.rs).
///
/// What does the "Lazy" mean?
/// In described above Bip Buffer implementation, if there is more space on the left that on the right side of the buffer,
/// region B will be created immediately.
///
/// This lazy implementation will create region B only if it is unable to fulfill `commit` request using region A only.
#[derive(Debug)]
pub struct BipBuffer {
    /// A non-null pointer to contiguous bytes allocation.
    ///
    /// **Note**: allocation is created with [Vec::into_boxed_slice], that is `Box<[u8]>`.
    buffer: NonNull<u8>,

    /// Constant `buffer` length.
    length: usize,

    /// Start index (inclusive) of primary contiguous region.
    a_start: Cell<usize>,

    /// End index (exclusive) of primary contiguous region.
    a_end: Cell<usize>,

    /// Start index (inclusive) of secondary contiguous region (used for wrap-around).
    b_start: Cell<usize>,

    /// End index (exclusive) of secondary contiguous region.
    b_end: Cell<usize>,

    /// `true` if there is a mutable chunk reservation.
    reserved: Cell<bool>,

    /// Ordered ID sequence for committed slices, to ensure proper release order.
    commit_id_sequence: Cell<usize>,

    /// Next **ordered** committed slice ID that is expected to be decommitted.
    decommit_next_id: Cell<usize>,

    /// If a commit ID is greater than `decommit_next_id` and the slice wants to be released,
    /// a tombstone is written instead to be released later.
    ///
    /// In ideal scenario, this collection should always be empty.
    decommit_tombstones: RefCell<BinaryHeap<Reverse<Commit>>>,
}

/// Mutable [BipBuffer] reserved contiguous chunk of bytes, can be dereferenced as `&[u8]` or `&mut [u8]`.
///
/// Dropping it will automatically cancel the reservation.
///
/// See also: [BipSliceMut::commit].
#[derive(Debug)]
pub struct BipSliceMut<'a> {
    reservation: Reservation,
    source: &'a BipBuffer,
}

/// Shared [BipBuffer] contiguous chunk of bytes, can be dereferenced as `&[u8]`.
///
/// Each `BipSlice` has its own ID, that increments in order these slices were committed.
/// This slice implements [Eq] and [Ord] comparing this ID only.
///
/// **Note**: for performance reasons, it is **highly recommended** to drop different slices in a sorted order,
/// as `BipBuffer` expects all decommits to be done in the same order they were committed.
/// There is no reason to use this buffer otherwise.
#[derive(Debug)]
pub struct BipSlice<'a> {
    commit: Commit,
    source: &'a BipBuffer,
}


#[derive(Debug, Copy, Clone)]
struct Reservation {
    /// Start index (inclusive) of the reserve (`BipBuffer.buffer`).
    start: usize,

    /// End index (exclusive) of the reserve (`BipBuffer.buffer`).
    end: usize,

    /// Region that is reserved.
    region: Region,
}

#[derive(Debug, Copy, Clone)]
struct Commit {
    /// Commit ID.
    id: usize,

    /// Start index (inclusive) of the reserve (`BipBuffer.buffer`).
    start: usize,

    /// End index (exclusive) of the reserve (`BipBuffer.buffer`).
    end: usize,
}

#[derive(Debug, Copy, Clone)]
enum Region {
    A,
    B,
}


impl BipBuffer {
    /// Creates a new heap allocated bip-buffer.
    ///
    /// `length` is constant, buffer will never resize.
    pub fn new(length: usize) -> Self {
        let mut buffer_box = vec![0u8; length].into_boxed_slice();
        let thin_ptr = buffer_box.as_mut_ptr(); // *mut u8
        let buffer = NonNull::new(thin_ptr).unwrap();
        Box::into_raw(buffer_box);

        Self {
            buffer,
            length,

            a_start: Cell::new(0),
            a_end: Cell::new(0),

            b_start: Cell::new(0),
            b_end: Cell::new(0),

            reserved: Cell::new(false),

            commit_id_sequence: Cell::new(0),
            decommit_next_id: Cell::new(1),
            decommit_tombstones: RefCell::new(BinaryHeap::new()),
        }
    }


    /// Returns the buffer's length.
    ///
    /// It is equal to length this buffer was created with, and will never change.
    pub fn len(&self) -> usize {
        self.length
    }

    /// Returns number of bytes available for the next [`reserve()`][Self::reserve] call.
    pub fn available(&self) -> usize {
        // - : Free Space
        // * : Region A
        // # : Region B
        //
        // 1. -------------------------
        // 2. *****-------------------- ; region A commits.
        // 3. *******************------ ; more region A commits.
        // 4. -----------********------ ; some region A chunks are released.
        // 5. ######-----********------ ; unable to fulfill `commit` request, region B is created.
        // 6. #############---**-------
        // 7. #################*-------
        // 8. *****************-------- ; when region A is empty, while region B exists, region A becomes region B.
        // 9. -----************--------

        if self.b_len() > 0 {
            return self.a_start.get() - self.b_end.get();
        }

        // Do we have more space before A region starts, or after it ends?
        usize::max(self.a_start.get(), self.length - self.a_end.get())
    }

    /// Reserves a specified number of bytes for writing.
    ///
    /// Returns `None` if there is no space [`available()`](Self::available).
    ///
    /// If `strict` is true, the returned buffer will always have the requested size (no less, no more).
    /// Otherwise, the returned buffer may have less size than requested.
    ///
    /// # Panics
    ///
    /// If this method was called before, but the returned [BipSliceMut] was not dropped:
    /// you cannot have multiple `&mut` references pointing to the same memory.
    ///
    /// **Note**: you still can have multiple committed (read-only shared) areas, that is [BipSlice],
    /// as they point into different, committed chunks.
    pub fn reserve(&self, request: usize, strict: bool) -> Option<BipSliceMut<'_>> {
        if self.reserved.get() {
            panic!("drop previous reservation first, before trying to reserve a new one");
        }

        let reserve_start;
        let reserve_region;
        let available;

        'init: {
            if self.b_len() > 0 {
                reserve_start = self.b_end.get();
                available = self.a_start.get() - self.b_end.get();
                reserve_region = Region::B;
                break 'init;
            }

            let space_before_a = self.a_start.get();
            let space_after_a = self.length - self.a_end.get();
            available = usize::max(space_before_a, space_after_a);

            if request <= space_after_a {
                // We are lazy, and do not wrap-around without a specific need.
                reserve_start = self.a_end.get();
                reserve_region = Region::A;
                break 'init;
            }

            if cfg!(debug_assertions) {
                if strict {
                    debug_assert!(space_after_a <= space_before_a);
                }
            }

            if space_after_a >= space_before_a {
                reserve_start = self.a_end.get();
                reserve_region = Region::A;
            } else {
                reserve_start = 0;
                reserve_region = Region::B;
            };
        };

        if available == 0 || strict && available < request {
            return None;
        }

        let reserve_len = usize::min(available, request);
        let reserve_end = reserve_start + reserve_len;
        self.reserved.set(true);

        Some(BipSliceMut {
            reservation: Reservation {
                start: reserve_start,
                end: reserve_end,
                region: reserve_region,
            },
            source: self,
        })
    }


    fn commit(&self, reservation: Reservation, commit_len: usize) -> Option<BipSlice<'_>> {
        if commit_len == 0 {
            return None;
        }
        if commit_len > reservation.len() {
            panic!(
                "unable to commit more memory ({}) than it was initially reserved ({})",
                commit_len,
                reservation.len()
            );
        }

        let id = Self::get_and_inc(&self.commit_id_sequence);
        let commit_start = reservation.start;
        let commit_len = usize::min(commit_len, reservation.len());
        let commit_end = commit_start + commit_len;

        match reservation.region {
            Region::A => {
                self.a_end.set(commit_end);
            }
            Region::B => {
                self.b_end.set(commit_end);
            }
        }

        Some(BipSlice {
            commit: Commit {
                id,
                start: commit_start,
                end: commit_end,
            },
            source: self,
        })
    }

    fn decommit(&self, value: Commit) {
        if value.id != self.decommit_next_id.get() {
            self.decommit_tombstones.borrow_mut().push(Reverse(value));
            return;
        }

        self.decommit_single(value);
        Self::get_and_inc(&self.decommit_next_id);

        let mut tombstones = self.decommit_tombstones.borrow_mut();
        while !tombstones.is_empty() {
            // `peek()` returns lowest ID, because of `Reverse()`.
            let Some(value) = tombstones.peek() else {
                return;
            };

            if value.0.id != self.decommit_next_id.get() {
                if value.0.id < self.decommit_next_id.get() {
                    panic!(
                        "tombstone commit ID is lower than next expected ID to be deallocated: \
                        this lag should never happen"
                    );
                }

                return;
            }

            let Some(value) = tombstones.pop() else {
                return;
            };

            self.decommit_single(value.0);
            Self::get_and_inc(&self.decommit_next_id);
        }
    }

    fn decommit_single(&self, value: Commit) {
        'advance: {
            if value.start == self.a_start.get() {
                self.a_start.set(self.a_start.get() + value.len());
                break 'advance;
            }

            if value.start == self.b_start.get() {
                self.b_start.set(self.b_start.get() + value.len());
                break 'advance;
            }

            panic!(
                "'decommit_single()' call is out of order. [expected_id: {}, actual: {}]",
                self.decommit_next_id.get(),
                value.id
            );
        }

        if self.a_len() == 0 {
            self.a_start.set(self.b_start.get());
            self.a_end.set(self.b_end.get());

            self.b_start.set(0);
            self.b_end.set(0);
        }
    }


    fn a_len(&self) -> usize {
        self.a_end.get() - self.a_start.get()
    }

    fn b_len(&self) -> usize {
        self.b_end.get() - self.b_start.get()
    }

    fn get_and_inc(cell: &Cell<usize>) -> usize {
        cell.replace(cell.get().wrapping_add(1))
    }
}

impl Drop for BipBuffer {
    fn drop(&mut self) {
        let thin_ptr = self.buffer.as_ptr();

        // SAFETY: we point at valid, owned memory, and its size is constant and never changes.
        let slice_ptr = core::ptr::slice_from_raw_parts_mut(thin_ptr, self.length);

        // SAFERY: slice_ptr is valid, and we own this memory.
        // Other parts, like `BipSlice` or `BipSliceMut` do not deallocate this memory.
        unsafe { Box::from_raw(slice_ptr) };
    }
}


/*
 * BipSliceMut impl
 */

impl<'a> BipSliceMut<'a> {
    /// Requests a part (or complete) reserved area to be committed and ready to read,
    /// after a successful write operation for a `len` bytes.
    ///
    /// Returns a shared, read-only slice, or `None` if `len` is zero.
    ///
    /// # Panics
    ///
    /// If `len` is greater than initially reserved chunk size.
    pub fn commit(self, len: usize) -> Option<BipSlice<'a>> {
        self.source.commit(self.reservation, len)
    }
}

impl<'a> Deref for BipSliceMut<'a> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        // SAFETY: [BipBuffer] guarantees an exclusive access to area in [reservation.start..reservation.end] range.
        let thin_ptr =
            unsafe { self.source.buffer.as_ptr().add(self.reservation.start) } as *const _;

        // SAFETY: thin_ptr is valid, and [BipBuffer] guarantees its exclusive access.
        unsafe { slice::from_raw_parts(thin_ptr, self.reservation.len()) }
    }
}

impl<'a> DerefMut for BipSliceMut<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: [BipBuffer] guarantees an exclusive access to area in [reservation.start..reservation.end] range.
        let thin_ptr = unsafe { self.source.buffer.as_ptr().add(self.reservation.start) };

        // SAFETY: thin_ptr is valid, and [BipBuffer] guarantees its exclusive access.
        unsafe { slice::from_raw_parts_mut(thin_ptr, self.reservation.len()) }
    }
}

impl<'a> BufMut for BipSliceMut<'a> {
    fn capacity(&self) -> usize {
        self.reservation.len()
    }

    fn as_write_slice(&mut self) -> &mut [u8] {
        self.deref_mut()
    }
}

impl<'a> Drop for BipSliceMut<'a> {
    fn drop(&mut self) {
        self.source.reserved.set(false);
    }
}


/*
 * BipSlice impl
 */

impl<'a> BipSlice<'a> {
    /// Release the committed area to be accessible for write operations.
    ///
    /// It is highly recommended to decommit in sorted order, to avoid performance penalties.
    ///
    /// It is not mandatory to call this method, `drop()` will do just the same.
    pub fn decommit(self) {
        // Drop.
    }
}

impl<'a> Deref for BipSlice<'a> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        // SAFETY: [BipBuffer] guarantees an exclusive access to area in [self.start..self.end] range.
        //
        // We use this area only for read-only purposes.
        let thin_ptr = unsafe { self.source.buffer.as_ptr().add(self.commit.start) } as *const _;

        // SAFETY: thin_ptr is valid, and [BipBuffer] guarantees its exclusive access.
        unsafe { slice::from_raw_parts(thin_ptr, self.commit.len()) }
    }
}

impl<'a> Buf for BipSlice<'a> {
    fn len(&self) -> usize {
        self.commit.len()
    }

    fn as_read_slice(&self) -> &[u8] {
        self.deref()
    }
}

impl<'a> Drop for BipSlice<'a> {
    fn drop(&mut self) {
        self.source.decommit(self.commit);
    }
}


impl<'a> PartialEq for BipSlice<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.commit.id == other.commit.id
    }
}

impl<'a> Eq for BipSlice<'a> {}

impl<'a> PartialOrd for BipSlice<'a> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.commit.id.partial_cmp(&other.commit.id)
    }
}

impl<'a> Ord for BipSlice<'a> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.commit.id.cmp(&other.commit.id)
    }
}


/*
 * Reservation impl
 */

impl Reservation {
    fn len(&self) -> usize {
        self.end - self.start
    }
}


/*
 * Commit impl
 */

impl Commit {
    fn len(&self) -> usize {
        self.end - self.start
    }
}

impl PartialEq for Commit {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Commit {}

impl PartialOrd for Commit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for Commit {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
}
