use std::{
    io::IoSlice,
    ops::{Deref, DerefMut},
};

use crate::{
    backend::Config,
    net::{Buf, MAX_PACKET_SIZE, SendMsg},
};

pub(super) struct IO {
    /// Messages that are waiting to be sent.
    pub pending: Vec<SendMsg<VecBuf>>,

    /// Memory to use for output operations.
    ///
    /// **Performance Notes**:
    /// `quinn-proto` expects `&mut Vec<u8>` for its [Connection::poll_transmit] method,
    /// and `BytesMut` + `&mut Vec<u8>` for [Endpoint::handle] method.
    ///
    /// Because `quinn-proto` wants a reference to a `Vec` instead of `slice`,
    /// we're unable to create some sort of ring-buffer for multiple packets (because quinn might just extend the buf automatically).
    /// Instead, we have some options:
    ///  1. Just use `Vec<u8>`.
    ///  2. Create `Vec<Vec<u8>>` for multiple packets.
    ///  3. Use unsafe methods from `Vec` like `from_raw_parts`, `into_raw_parts`,
    ///     create multiple vectors from a single continuous block of memory
    ///     and control their allocations to achieve effect that we want.
    ///
    /// We will stick the second option with a custom configuration to let user choose number of buffers.
    ///
    /// Therefore, at this point, `GSO` optimization is enabled (as we have large buffers),
    /// `sendmmsg`-like is enabled too, but with not ideal efficiency, as there may be huge memory waste.
    pub buffers: Vec<VecBuf>,

    /// Reserved number of `out_buffers` for output operations,
    /// when primary `out_buffers` are not available yet.
    ///
    /// Reserve is required because some of `quinn-proto`
    /// methods return `Transmit` events, and we need to buffer them somewhere,
    /// before `send` is called.
    ///
    /// If there is no reserved buffer, `recv()` method will not process messages until there is a primary buffer available.
    pub reserved_buffers_count: usize,
}

impl IO {
    pub fn new(config: &Config, mmsg_supported: bool) -> Self {
        let buffers;
        let reserved_buffers_count;

        if mmsg_supported {
            buffers = (0..usize::max(
                1,
                config.out_buffers_count + config.reserved_out_buffers_count,
            ))
                .map(|_| VecBuf::new())
                .collect();

            reserved_buffers_count = config.reserved_out_buffers_count;
        } else {
            buffers = vec![VecBuf::new()];
            reserved_buffers_count = 0;
        }

        Self {
            pending: Vec::with_capacity(reserved_buffers_count),
            buffers,
            reserved_buffers_count,
        }
    }

    pub fn has_primary_buffer(&mut self) -> bool {
        !self.buffers.is_empty() && self.buffers.len() > self.reserved_buffers_count
    }

    pub fn has_any_buffer(&mut self) -> bool {
        !self.buffers.is_empty()
    }

    pub fn peek_buffer(&mut self) -> &mut VecBuf {
        self.buffers
            .last_mut()
            .expect("bug: an attempt to call `peek_buffer()`, where there is no buffer present")
    }

    pub fn pop_buffer(&mut self) -> VecBuf {
        self.buffers
            .pop()
            .expect("bug: an attempt to call `pop_buffer()`, where there is no buffer present")
    }
}


/// Simple implementation of [Buf] based on [Vec].
pub struct VecBuf(Vec<u8>);

impl VecBuf {
    pub fn new() -> Self {
        Self(Vec::with_capacity(MAX_PACKET_SIZE))
    }
}

impl Deref for VecBuf {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for VecBuf {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Buf for VecBuf {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }

    fn as_read_slice(&self) -> &[u8] {
        &self.as_slice()[..self.len()]
    }

    fn as_read_io_slice(&self) -> IoSlice<'_> {
        IoSlice::new(self.as_read_slice())
    }
}
