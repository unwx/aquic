use quinn_proto::Transmit;

use crate::{
    backend::quinn::{Config, Connection},
    net::{Buf, SendMsg, SoFeat},
};
use std::{
    cell::RefCell,
    collections::HashSet,
    net::IpAddr,
    ops::{Deref, DerefMut},
    rc::Rc,
    time::Instant,
};

pub(super) struct IO {
    /// Memory to use for output operations.
    ///
    /// **Performance Notes**:
    /// `quinn-proto` expects `&mut Vec<u8>` for its [`Connection::poll_transmit()`](quinn_proto::Connection::poll_transmit) method,
    /// and `BytesMut` + `&mut Vec<u8>` for [`Endpoint::handle()`](quinn_proto::Endpoint::handle) method.
    ///
    /// Because `quinn-proto` wants a reference to a `Vec` instead of `slice`,
    /// we're unable to use bip-buffer for multiple packets, because quinn might just extend the buf automatically.
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
    /// `sendmmsg`-like is enabled too, but with not ideal efficiency.
    primary_buffers: VecBufPool,

    /// Reserved buffers that are only used for the first packets
    /// like `Initial`, `Retry`, `Version Negotiation`, even when the primary `out_buffers` are busy.
    reserved_buffers: VecBufPool,

    /// Address our socket is bound to, used for constructing `SendMsg`.
    listen_addr: IpAddr,
}

impl IO {
    pub fn new(config: &Config, listen_addr: IpAddr, socket_features: &HashSet<SoFeat>) -> Self {
        let primary_buffers_count;
        let reserved_buffers_count;

        if socket_features.contains(&SoFeat::Mmsg) {
            primary_buffers_count = usize::max(config.out_buffers_count, 1);
            reserved_buffers_count = usize::max(config.reserved_out_buffers_count, 1);
        } else {
            primary_buffers_count = 1;
            reserved_buffers_count = 1;
        };

        let primary_buffer_capacity = if socket_features.contains(&SoFeat::GenericSegOffload) {
            usize::max(config.out_buffer_gso_size, 1500)
        } else {
            usize::max(config.out_buffer_size, 1500)
        };
        let reserved_buffer_capacity = 1500;

        Self {
            primary_buffers: VecBufPool::new(primary_buffers_count, primary_buffer_capacity),
            reserved_buffers: VecBufPool::new(reserved_buffers_count, reserved_buffer_capacity),
            listen_addr,
        }
    }

    /// Updates the socket `source` address, used for constructing `SendMsg`.
    pub fn update_socket_addr(&mut self, new_addr: IpAddr) {
        self.listen_addr = new_addr;
    }


    /// Returns true if at least one primary buffer is available.
    ///
    /// Primary buffer is only used for established connection packets.
    pub fn has_primary_buffer(&mut self) -> bool {
        !self.primary_buffers.is_empty()
    }

    /// Returns a reserved buffer if available, otherwise `None`.
    pub fn pop_reserved_buffer(&mut self) -> Option<VecBuf> {
        self.reserved_buffers.pop()
    }


    /// Writes pending connection packets into `&mut out`.
    ///
    /// Returns `true` on complete successful write,
    /// `false` on partial or absent write, due to lack of available buffers.
    pub fn write_pending(
        &mut self,
        connection: &mut Connection,
        out: &mut Vec<SendMsg<VecBuf>>,
        now: Instant,
    ) -> bool {
        let pmtu = connection.current_mtu() as usize;

        while let Some(mut buffer) = self.primary_buffers.pop() {
            buffer.clear();
            buffer.reserve(pmtu);

            let max_datagrams = buffer.capacity() / pmtu;

            match connection.poll_transmit(now, max_datagrams, &mut buffer) {
                Some(transmit) => {
                    out.push(self.transmit_to_msg(transmit, buffer));
                    continue;
                }
                None => {
                    return true;
                }
            }
        }

        false
    }

    /// Writes a single response packet into `&mut out`.
    pub fn write_response(
        &mut self,
        response: Transmit,
        buffer: VecBuf,
        out: &mut Vec<SendMsg<VecBuf>>,
    ) {
        out.push(self.transmit_to_msg(response, buffer));
    }


    fn transmit_to_msg(&self, transmit: Transmit, buffer: VecBuf) -> SendMsg<VecBuf> {
        let buffer_len = buffer.len();
        debug_assert_eq!(buffer_len, transmit.size);

        SendMsg::new(
            buffer,
            transmit.src_ip.unwrap_or(self.listen_addr),
            transmit.destination,
            transmit.ecn.into(),
            {
                if let Some(segment_size) = transmit.segment_size {
                    assert!(
                        buffer_len.is_multiple_of(segment_size),
                        "invalid quinn_proto::Transmit: segment_size ({}) must be a divisor of buffer length ({})",
                        segment_size,
                        buffer_len
                    );

                    buffer_len / segment_size
                } else {
                    1
                }
            },
        )
    }
}


/// [VecBuf] pool.
struct VecBufPool {
    buffers: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl VecBufPool {
    pub fn new(buffers_count: usize, buffer_capacity: usize) -> Self {
        let buffers: Vec<Vec<u8>> = (0..buffers_count)
            .map(|_| Vec::with_capacity(buffer_capacity))
            .collect();

        Self {
            buffers: Rc::new(RefCell::new(buffers)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.buffers.borrow().is_empty()
    }

    pub fn pop(&mut self) -> Option<VecBuf> {
        let buffer = self.buffers.borrow_mut().pop()?;
        Some(VecBuf {
            inner: buffer,
            source: self.buffers.clone(),
        })
    }
}


/// Simple implementation of [Buf] based on [Vec].
pub struct VecBuf {
    inner: Vec<u8>,
    source: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl Deref for VecBuf {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for VecBuf {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Buf for VecBuf {
    fn len(&self) -> usize {
        self.inner.len()
    }

    fn as_slice(&self) -> &[u8] {
        self.inner.as_slice()
    }
}

impl Drop for VecBuf {
    fn drop(&mut self) {
        self.inner.clear();
        self.source
            .borrow_mut()
            .push(std::mem::take(&mut self.inner));
    }
}
