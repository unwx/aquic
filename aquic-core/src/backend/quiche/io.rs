use crate::{
    backend::{quiche::Connection, write_stateless_reset_packet},
    net::{Ecn, Packet, SendMsg},
    util::{BipBuffer, BipSliceMut, BipView},
};
use bytes::Bytes;
use quiche::{BufFactory, BufSplit};
use std::{net::SocketAddr, ops::DerefMut, time::Instant};

pub(super) struct IO {
    buffer: BipBuffer,
}

impl IO {
    pub(super) fn new(buffer_size: usize) -> Self {
        Self {
            buffer: BipBuffer::new(buffer_size),
        }
    }

    /// Writes pending connection packets into `&mut out`.
    pub fn write_pending(
        &mut self,
        connection: &mut Connection,
        out: &mut Vec<SendMsg<BipView>>,
        now: Instant,
    ) -> Result<Write, quiche::Error> {
        struct Packet<'a> {
            reserve: BipSliceMut<'a>,
            reserve_offset: usize,
            segment_count: usize,
            segment_size: usize,
        }

        struct Path {
            source: SocketAddr,
            destination: SocketAddr,
            quantum: usize,
            quantum_consumed: usize,
        }

        impl<'a> Packet<'a> {
            fn as_mut_slice(&mut self) -> &mut [u8] {
                &mut self.reserve[self.reserve_offset..]
            }
        }

        fn commit_packet(path: &Path, packet: &mut Packet, out: &mut Vec<SendMsg<BipView>>) {
            let packet_len = packet.segment_count * packet.segment_size;
            if packet_len == 0 {
                return;
            }

            let view = packet.reserve.commit(packet_len).detach();
            debug_assert_ne!(view.len(), 0);
            debug_assert_eq!(view.len(), packet_len);

            let msg = SendMsg::new(
                view,
                path.source.ip(),
                path.destination,
                Ecn::NEct, // Quiche doesn't support ECN yet.
                packet.segment_count,
            );

            out.push(msg);
            packet.reserve_offset -= packet_len;
            packet.segment_count = 0;
            packet.segment_size = 0;
        }


        let mut packet = {
            let Some(reserve) = self.buffer.reserve(usize::MAX, false) else {
                return Ok(Write::BufferTooShort);
            };

            Packet {
                reserve,
                reserve_offset: 0,
                segment_count: 0,
                segment_size: 0,
            }
        };
        let mut path: Option<Path> = None;
        let mut result: Option<Write> = None;

        while {
            match path.as_ref() {
                // Note, that `path` is mutable: both `quantum_consumed` and `quantum` reset when path changes.
                Some(p) => p.quantum_consumed < p.quantum,
                None => true,
            }
        } {
            // Note: `connection.get_next_release_time()` returns a release time for an active path.
            // Therefore, if the next packet is actually a probing one (for another path), it will be delayed by mistake.
            //
            // As 99.9% of packets go through the active path, this should not be a problem.
            // And I'm not sure how to fix this delay, as I cannot find a `quiche::Connection` method
            // that might tell that there is a pending probing packet, on a non-active path.
            //
            // Also, I don't want to use `SendInfo.at` for pacing,
            // as this approach will allocate memory from the buffer and then just wait for N ms,
            // not letting other active connections to use this memory.
            if let Some(release_time) = connection.get_next_release_time(now) {
                result = Some(Write::Pacer(release_time));
                break;
            }

            let current_length;
            let current_source;
            let current_destination;

            match connection.send(packet.as_mut_slice()) {
                Ok((length, info)) => {
                    if length == 0 {
                        // This should not happen,
                        // but it doesn't break our app.
                        result = Some(Write::Complete);
                        break;
                    }

                    current_length = length;
                    current_source = info.from;
                    current_destination = info.to;
                }

                Err(quiche::Error::Done) => {
                    result = Some(Write::Complete);
                    break;
                }
                Err(quiche::Error::BufferTooShort) => {
                    result = Some(Write::BufferTooShort);
                    break;
                }
                Err(e) => {
                    return Err(e);
                }
            };

            // It is `None` on the very first packet, and never more.
            if path.is_none() {
                path = Some(Path {
                    source: current_source,
                    destination: current_destination,
                    quantum: connection.send_quantum_on_path(current_source, current_destination),
                    quantum_consumed: 0,
                });
            }

            let path_ref = path
                .as_mut()
                .expect("path cannot be 'None', it's initialized");

            packet.reserve_offset += current_length;
            path_ref.quantum_consumed += current_length;

            if path_ref.source == current_source && path_ref.destination == current_destination {
                // Batch it into a single GSO message, if possible.

                if packet.segment_count == 0 {
                    packet.segment_size = current_length;
                    packet.segment_count = 1;
                    continue;
                }
                if packet.segment_size == current_length {
                    packet.segment_count += 1;
                    continue;
                }
            }


            // Either path has changed, or generated packet is no more GSO-sized.
            // ---

            commit_packet(path_ref, &mut packet, out);
            debug_assert_eq!(packet.reserve_offset, current_length);

            if path_ref.source != current_source || path_ref.destination != current_destination {
                path = Some(Path {
                    source: current_source,
                    destination: current_destination,
                    quantum: connection.send_quantum_on_path(current_source, current_destination),
                    quantum_consumed: current_length,
                });
            }

            // Make the last packet an active one.
            packet.segment_count = 1;
            packet.segment_size = current_length;
        }


        if let Some(path) = path {
            commit_packet(&path, &mut packet, out);
            debug_assert_eq!(packet.reserve_offset, 0);
        }

        Ok(result.unwrap_or(Write::Quantum))
    }


    /// Writes `Version Negotiation` packet into `&mut out`.
    pub fn write_version_negotiation(
        &mut self,
        header: &quiche::Header,
        packet: &Packet,
        out: &mut Vec<SendMsg<BipView>>,
    ) -> Result<ForceWrite, quiche::Error> {
        let Some(mut reserve) = self.buffer.reserve(1500, false) else {
            return Ok(ForceWrite::BufferTooShort);
        };

        let result = quiche::negotiate_version(&header.scid, &header.dcid, reserve.deref_mut());
        Self::to_force_write_result(packet, reserve, result, out)
    }

    /// Writes `Retry` packet into `&mut out`.
    pub fn write_retry(
        &mut self,
        header: &quiche::Header,
        packet: &Packet,
        new_scid: &quiche::ConnectionId,
        token: &[u8],
        out: &mut Vec<SendMsg<BipView>>,
    ) -> Result<ForceWrite, quiche::Error> {
        let Some(mut reserve) = self.buffer.reserve(1500, false) else {
            return Ok(ForceWrite::BufferTooShort);
        };

        let result = quiche::retry(
            &header.scid,
            &header.dcid,
            new_scid,
            token,
            header.version,
            reserve.deref_mut(),
        );

        Self::to_force_write_result(packet, reserve, result, out)
    }

    /// Writes a Stateless Reset packet into `&mut out`,
    /// if `packet` length is sufficient for it, and if the buffer has enough space for it.
    pub fn write_stateless_reset(
        &mut self,
        packet: &Packet,
        reset_token: &[u8; 16],
        out: &mut Vec<SendMsg<BipView>>,
    ) {
        let Some(mut reserve) = self.buffer.reserve(40, true) else {
            return;
        };

        let Some(length) =
            write_stateless_reset_packet(packet.len(), reserve.len(), reset_token, &mut reserve)
        else {
            return;
        };

        out.push(SendMsg::new(
            reserve.commit(length).detach(),
            packet.to,
            packet.from,
            Ecn::NEct,
            0,
        ));
    }

    fn to_force_write_result(
        peer_packet: &Packet,
        mut reserve: BipSliceMut,
        result: quiche::Result<usize>,
        out: &mut Vec<SendMsg<BipView>>,
    ) -> Result<ForceWrite, quiche::Error> {
        match result {
            Ok(length) => {
                out.push(SendMsg::new(
                    reserve.commit(length).detach(),
                    peer_packet.to,
                    peer_packet.from,
                    Ecn::NEct,
                    0,
                ));
                Ok(ForceWrite::Complete)
            }
            Err(quiche::Error::BufferTooShort) => Ok(ForceWrite::BufferTooShort),
            Err(e) => Err(e),
        }
    }
}


#[derive(Debug, Copy, Clone)]
pub(super) enum Write {
    /// Everything is done, on each path.
    Complete,

    /// There is a pending packet,
    /// but it is deferred until `Instant`.
    Pacer(Instant),

    /// There is a pending packet,
    /// but it is required to flush existing ones into network.
    Quantum,

    /// Insufficient amount of memory available in the buffer,
    /// unable to write.
    BufferTooShort,
}

#[derive(Debug, Copy, Clone)]
pub(super) enum ForceWrite {
    /// Everything is done.
    Complete,

    /// Insufficient amount of memory available in the buffer,
    /// unable to write.
    BufferTooShort,
}


/// [Bytes] wrapper to be used for [BufFactory].
#[repr(transparent)]
#[derive(Debug, Clone)]
pub(super) struct View(Bytes);

impl From<Bytes> for View {
    fn from(bytes: Bytes) -> Self {
        Self(bytes)
    }
}

impl From<View> for Bytes {
    fn from(value: View) -> Self {
        value.0
    }
}

impl AsRef<[u8]> for View {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl BufSplit for View {
    fn split_at(&mut self, at: usize) -> Self {
        View::from(self.0.split_off(at))
    }
}


/// [Bytes] view wrapper to be used as [BufFactory],
/// instead of [quiche::range_buf::DefaultBufFactory].
///
/// It's primarily use is [Connection::stream_send_zc](quiche::Connection::stream_send_zc).
#[derive(Debug, Copy, Clone, Default)]
pub(super) struct BytesFactory;

impl BufFactory for BytesFactory {
    type Buf = View;

    fn buf_from_slice(buf: &[u8]) -> Self::Buf {
        View::from(Bytes::copy_from_slice(buf))
    }
}
