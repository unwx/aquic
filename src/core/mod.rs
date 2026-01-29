use crate::net::{Buf, BufMut};
use std::io::{IoSlice, IoSliceMut};


/// A heap allocated array buffer with a constant `SIZE`.
pub struct ConstBuf<const SIZE: usize> {
    array: Box<[u8; SIZE]>,
    len: usize,
}

impl<'a, const SIZE: usize> Buf<'a> for ConstBuf<SIZE> {
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn len(&self) -> usize {
        self.len
    }

    fn as_slice(&'a self) -> &'a [u8] {
        &self.array[..self.len]
    }

    fn as_io_slice(&'a self) -> IoSlice<'a> {
        IoSlice::new(self.as_slice())
    }
}

impl<'a, const SIZE: usize> BufMut<'a> for ConstBuf<SIZE> {
    fn as_mut_slice(&'a mut self) -> &'a mut [u8] {
        &mut self.array[..self.len]
    }

    fn as_mut_io_slice(&'a mut self) -> IoSliceMut<'a> {
        IoSliceMut::new(self.as_mut_slice())
    }
}


pub(crate) enum QuicCommand<CId> {
    _Private(CId), // TODO(feat): different commands should be here.
}

pub(crate) enum QuicResponse<CId> {
    _Private(CId), // TODO(feat): different responses should be here.
}
