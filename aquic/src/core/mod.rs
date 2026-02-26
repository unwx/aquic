use crate::net::{Buf, BufMut};


/// A heap allocated array buffer with a constant `SIZE`.
pub struct ConstBuf<const SIZE: usize> {
    array: Box<[u8; SIZE]>,
    len: usize,
}

impl<const SIZE: usize> Buf for ConstBuf<SIZE> {
    fn len(&self) -> usize {
        self.len
    }

    fn as_read_slice(&self) -> &[u8] {
        &self.array[..self.len]
    }
}

impl<const SIZE: usize> BufMut for ConstBuf<SIZE> {
    fn capacity(&self) -> usize {
        SIZE
    }

    fn as_write_slice(&mut self) -> &mut [u8] {
        &mut self.array[..]
    }
}


pub(crate) enum QuicCommand<CId> {
    _Private(CId), // TODO(feat): different commands should be here.
}

pub(crate) enum QuicResponse {
    // TODO(feat): different responses should be here.
}
