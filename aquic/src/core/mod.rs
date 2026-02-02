use crate::net::{Buf, BufMut};
use std::io::{IoSlice, IoSliceMut};


/// A heap allocated array buffer with a constant `SIZE`.
pub struct ConstBuf<const SIZE: usize> {
    array: Box<[u8; SIZE]>,
    len: usize,
}

impl<const SIZE: usize> Buf for ConstBuf<SIZE> {
    fn len(&self) -> usize {
        self.len
    }

    fn capacity(&self) -> usize {
        SIZE
    }

    fn as_read_slice(&self) -> &[u8] {
        &self.array[..self.len]
    }

    fn as_read_io_slice(&self) -> IoSlice<'_> {
        IoSlice::new(self.as_read_slice())
    }
}

impl<const SIZE: usize> BufMut for ConstBuf<SIZE> {
    fn as_mut_read_slice(&mut self) -> &mut [u8] {
        &mut self.array[..self.len]
    }

    fn as_mut_write_io_slice(&mut self) -> IoSliceMut<'_> {
        IoSliceMut::new(&mut self.array[..])
    }
}


pub(crate) enum QuicCommand<CId> {
    _Private(CId), // TODO(feat): different commands should be here.
}

pub(crate) enum QuicResponse {
    // TODO(feat): different responses should be here.
}
