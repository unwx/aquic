use crate::backend::stream::{StreamBackend, StreamError, StreamResult};
use crate::backend::{Address, ConnBackend, ConnError, ConnResult, PacerDecision};
use crate::stream::{Priority, StreamId};
use bytes::Bytes;
use quiche::{BufFactory, BufSplit, Connection, ConnectionId, Error, RecvInfo, SendInfo, Shutdown};
use std::net::SocketAddr;
use std::time::Instant;

impl From<Error> for StreamError {
    fn from(value: Error) -> Self {
        match value {
            Error::StreamStopped(e) => Self::StopSending(e),
            Error::StreamReset(e) => Self::ResetSending(e),
            other => Self::Other(format!("{}", other).into()),
        }
    }
}

impl From<Error> for ConnError {
    fn from(value: Error) -> Self {
        #[rustfmt::skip]
        let detail = match &value {
            Error::UnknownVersion => "The provided packet cannot be parsed because its version is unknown",
            Error::InvalidFrame => "The provided packet cannot be parsed because it contains an invalid frame",
            Error::InvalidPacket => "The provided packet cannot be parsed",
            Error::InvalidTransportParam => "The peer's transport params cannot be parsed",

            Error::FlowControl => "The peer violated the local flow control limits",
            Error::StreamLimit => "The peer violated the local stream limits",
            Error::FinalSize => "The received data exceeds the stream's final size",
            Error::InvalidAckRange => "The peer sent an ACK frame with an invalid range",
            Error::OptimisticAckDetected => "The peer send an ACK frame for a skipped packet used for Optimistic ACK mitigation",
            Error::IdLimit => "Too many identifiers were provided",
            Error::OutOfIdentifiers => "Not enough available identifiers",
            Error::CryptoBufferExceeded => "The peer sent more data in CRYPTO frames than we can buffer",

            Error::CryptoFail => "A cryptographic operation failed",
            Error::TlsFail => "The TLS handshake failed",
            Error::KeyUpdate => "Error in key update",

            // We should not receive these,
            // but convert them anyway.
            Error::Done => "quiche::Done",
            Error::BufferTooShort => "The provided buffer is too short",
            Error::InvalidState => "The operation cannot be completed because the connection is in an invalid state",
            Error::InvalidStreamState(_) => "The operation cannot be completed because the stream is in an invalid state",
            Error::StreamStopped(_) => "The specified stream was stopped by the peer",
            Error::StreamReset(_) => "The specified stream was reset by the peer",
            Error::CongestionControl => "Error in congestion control",
        };

        Self(detail.into())
    }
}

impl From<SendInfo> for Address {
    fn from(value: SendInfo) -> Self {
        Self {
            source: value.from,
            destination: value.to,
        }
    }
}

impl From<Address> for RecvInfo {
    fn from(value: Address) -> Self {
        Self {
            from: value.source,
            to: value.destination,
        }
    }
}


pub struct QuicheBackend {
    inner: Connection<BytesFactory>,
}

impl QuicheBackend {
    fn stream_shutdown(
        &mut self,
        stream_id: StreamId,
        direction: Shutdown,
        err: u64,
    ) -> StreamResult<()> {
        match self.inner.stream_shutdown(stream_id, direction, err) {
            Ok(_) => Ok(()),
            Err(Error::Done) => Ok(()),
            Err(other) => Err(other.into()),
        }
    }
}

impl ConnBackend for QuicheBackend {
    type Config = quiche::Config;

    fn accept(
        source_id: &[u8],
        original_source_id: Option<&[u8]>,
        local_addr: SocketAddr,
        peer_addr: SocketAddr,
        config: &mut Self::Config,
    ) -> ConnResult<Self> {
        let inner = quiche::accept_with_buf_factory::<BytesFactory>(
            &ConnectionId::from_ref(source_id),
            original_source_id
                .map(|id| ConnectionId::from_ref(id))
                .as_ref(),
            local_addr,
            peer_addr,
            config,
        )
        .map_err(ConnError::from)?;

        Ok(Self { inner })
    }

    fn connect(
        server_name: Option<&str>,
        source_id: &[u8],
        local_addr: SocketAddr,
        peer_addr: SocketAddr,
        config: &mut Self::Config,
    ) -> ConnResult<Self> {
        let inner = quiche::connect_with_buffer_factory::<BytesFactory>(
            server_name,
            &ConnectionId::from_ref(source_id),
            local_addr,
            peer_addr,
            config,
        )
        .map_err(ConnError::from)?;

        Ok(Self { inner })
    }

    fn retry(
        source_id: &[u8],
        destination_id: &[u8],
        new_source_id: &[u8],
        token: &[u8],
        version: u32,
        out: &mut [u8],
    ) -> ConnResult<usize> {
        quiche::retry(
            &ConnectionId::from_ref(source_id),
            &ConnectionId::from_ref(destination_id),
            &ConnectionId::from_ref(new_source_id),
            token,
            version,
            out,
        )
        .map_err(ConnError::from)
    }

    fn negotiate_version(
        source_id: &[u8],
        destination_id: &[u8],
        out: &mut [u8],
    ) -> ConnResult<usize> {
        quiche::negotiate_version(
            &ConnectionId::from_ref(source_id),
            &ConnectionId::from_ref(destination_id),
            out,
        )
        .map_err(ConnError::from)
    }

    fn is_version_supported(version: u32) -> bool {
        quiche::version_is_supported(version)
    }


    fn send(&mut self, out: &mut [u8]) -> ConnResult<Option<(usize, Address)>> {
        match self.inner.send(out) {
            Ok((len, info)) => Ok(Some((len, info.into()))),
            Err(Error::Done) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    fn recv(&mut self, buf: &mut [u8], addr: Address) -> ConnResult<()> {
        match self.inner.recv(buf, addr.into()) {
            Ok(_) | Err(Error::Done) => Ok(()),
            Err(other) => Err(other.into()),
        }
    }


    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    fn is_draining(&self) -> bool {
        self.inner.is_draining()
    }

    fn is_established(&self) -> bool {
        self.inner.is_established()
    }

    fn is_in_early_data(&self) -> bool {
        self.inner.is_in_early_data()
    }

    fn next_pacer_decision(&self, now: Instant) -> PacerDecision {
        self.inner
            .get_next_release_time()
            .map(|it| {
                if it.can_burst() {
                    return PacerDecision::Burst;
                }

                match it.time(now) {
                    None => PacerDecision::Burst,
                    Some(it) => PacerDecision::At(it),
                }
            })
            .unwrap_or(PacerDecision::Burst)
    }

    fn next_timeout(&self, _now: Instant) -> Option<Instant> {
        self.inner.timeout_instant()
    }

    fn on_timeout(&mut self) {
        self.inner.on_timeout();
    }

    fn peer_cert(&self) -> Option<&[u8]> {
        self.inner.peer_cert()
    }

    fn peer_cert_chain(&self) -> Option<Vec<&[u8]>> {
        self.inner.peer_cert_chain()
    }
}


impl StreamBackend for QuicheBackend {
    #[rustfmt::skip]
    fn stream_recv(&mut self, stream_id: StreamId, out: &mut [u8]) -> StreamResult<(usize, bool)> {
        match self.inner.stream_recv(stream_id, out) {
            Ok((read, fin)) => {
                Ok((read, fin))
            },
            Err(Error::Done) => {
                if self.inner.stream_finished(stream_id) {
                    return Err(StreamError::Finish);
                }

                Ok((0, false))
            }
            Err(other) => {
                Err(other.into())
            },
        }
    }

    #[rustfmt::skip]
    fn stream_send(
        &mut self,
        stream_id: StreamId,
        bytes: Bytes,
        fin: bool,
    ) -> StreamResult<Option<Bytes>> {
        let length = bytes.len();
        if length == 0 && !fin {
            return Ok(None);
        }

        match self.inner.stream_send_zc(stream_id, bytes.into(), Some(length), fin) {
            Ok((_, left)) => {
                Ok(left.map(Into::<Bytes>::into).filter(|it| it.len() > 0))
            },
            Err(Error::Done) => {
                if self.inner.stream_finished(stream_id) {
                    return Err(StreamError::Finish);
                }

                Ok(None)
            },
            Err(Error::FinalSize) => {
                Err(StreamError::Finish)
            },
            Err(other) => {
                Err(other.into())
            },
        }
    }

    fn stream_priority(&mut self, stream_id: StreamId, priority: Priority) -> StreamResult<()> {
        self.inner
            .stream_priority(stream_id, priority.urgency, priority.incremental)
            .map_err(|e| e.into())
    }


    fn stream_readable_next(&mut self) -> Option<StreamId> {
        self.inner.stream_readable_next()
    }

    fn stream_writable_next(&mut self) -> Option<StreamId> {
        self.inner.stream_writable_next()
    }


    fn stream_stop_sending(&mut self, stream_id: StreamId, err: u64) -> StreamResult<()> {
        self.stream_shutdown(stream_id, Shutdown::Read, err)
    }

    fn stream_reset_sending(&mut self, stream_id: StreamId, err: u64) -> StreamResult<()> {
        self.stream_shutdown(stream_id, Shutdown::Write, err)
    }
}


/// [Bytes] wrapper to be used for [BufFactory].
#[repr(transparent)]
#[derive(Debug, Clone)]
struct View(Bytes);

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
#[derive(Debug, Copy, Clone, Default)]
struct BytesFactory;

impl BufFactory for BytesFactory {
    type Buf = View;

    fn buf_from_slice(buf: &[u8]) -> Self::Buf {
        View::from(Bytes::copy_from_slice(buf))
    }
}
