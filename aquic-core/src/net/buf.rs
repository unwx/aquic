use std::{
    io::{IoSlice, IoSliceMut},
    ops::{Deref, DerefMut},
};

/// An immutable, read-only buffer.
pub trait Buf {
    /// Returns buffer's length.
    fn len(&self) -> usize;

    /// Returns `true` if buffer is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns an immutable `[..len()]` slice.
    fn as_slice(&self) -> &[u8];

    /// Returns an immutable `[..len()]` I/O slice.
    fn as_io_slice(&self) -> IoSlice<'_> {
        IoSlice::new(self.as_slice())
    }
}

/// A mutable buffer.
pub trait BufMut {
    /// Returns buffer's capacity.
    fn capacity(&self) -> usize;

    /// Returns a mutable `[..capacity()]` slice.
    fn as_slice(&mut self) -> &mut [u8];

    /// Returns a mutable `[..capacity()]` I/O slice.
    fn as_io_slice(&mut self) -> IoSliceMut<'_> {
        IoSliceMut::new(self.as_slice())
    }
}


/// A constant-sized buffer,
/// implements both [Buf] and [BufMut].
pub struct ConstBuf<const SIZE: usize> {
    storage: Storage<SIZE>,
    len: usize,
}

enum Storage<const SIZE: usize> {
    Stack([u8; SIZE]),
    Heap(Box<[u8; SIZE]>),
}


impl<const SIZE: usize> ConstBuf<SIZE> {
    /// Creates a new stack allocated buffer.
    pub fn new_stack() -> Self {
        Self {
            storage: Storage::Stack([0u8; SIZE]),
            len: 0,
        }
    }

    /// Creates a new heap allocated buffer.
    pub fn new_heap() -> Self {
        Self {
            storage: Storage::Heap(Box::new([0u8; SIZE])),
            len: 0,
        }
    }
}

impl<const SIZE: usize> Buf for ConstBuf<SIZE> {
    fn len(&self) -> usize {
        self.len
    }

    fn as_slice(&self) -> &[u8] {
        &self.storage[..self.len]
    }
}

impl<const SIZE: usize> BufMut for ConstBuf<SIZE> {
    fn capacity(&self) -> usize {
        SIZE
    }

    fn as_slice(&mut self) -> &mut [u8] {
        &mut self.storage[..]
    }
}


impl<const SIZE: usize> Deref for Storage<SIZE> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match self {
            Storage::Stack(arr) => arr.as_slice(),
            Storage::Heap(arr) => arr.as_slice(),
        }
    }
}

impl<const SIZE: usize> DerefMut for Storage<SIZE> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Storage::Stack(arr) => arr.as_mut_slice(),
            Storage::Heap(arr) => arr.as_mut_slice(),
        }
    }
}
