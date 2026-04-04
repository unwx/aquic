use crate::backend::quinn::connection::{
    Connection, ConnectionKey, Connections, QuinnConnectionId,
};
use crate::backend::quinn::io::{IO, VecBuf};
use crate::backend::quinn::time::{Time, TimerEvent};
use crate::backend::{BackendEvents, ConnectionEvent, DatagramEvent, StreamEvent};
use crate::backend::{Error, QuicBackend};
use crate::backend::{Event, HaltKind, Result};
use crate::debug_panic;
use crate::net::{BufMut, MultiMsgFlattenIter, SendMsg, ServerName, SoFeat};
use crate::stream::{Priority, StreamId};
use bytes::{Bytes, BytesMut};
use quinn_proto::{
    ConnectionError, Dir, Endpoint, FinishError, Incoming, ReadError, ReadableError,
    SendDatagramError, VarInt, WriteError,
};
use rustls::pki_types::CertificateDer;
use std::any::type_name;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;
use tracing::error;

use quinn_proto::ConnectionEvent as QuinnConnectionEvent;
use quinn_proto::DatagramEvent as QuinnDatagramEvent;
use quinn_proto::Event as QuinnEvent;
use quinn_proto::StreamEvent as QuinnStreamEvent;

mod cid;
mod config;
mod connection;
mod io;
mod shared;
mod time;

pub use cid::*;
pub use config::*;

/// The `quinn-proto` QUIC implementation provider.
///
/// [Quinn Project](https://github.com/quinn-rs/quinn)
///
/// # Privacy Warning
///
/// This backend violates one of the [`QuicBackend::migrate_network`] requirements:
/// > **It is forbidden** to perform an active migration using old connection IDs.
///
/// **Active migration** is not strictly implemented in the `quinn-proto` implementation:
/// if there is no spare server destination ID, `quinn-proto` will ignore that,
/// leading to an external observer being able to correlate new and old connection IDs, and thus track the connection across migrations:
/// [quinn-proto/0.11.14](https://docs.rs/quinn-proto/0.11.14/src/quinn_proto/connection/mod.rs.html#3093-3096).
pub struct QuinnBackend {
    /// Client/Server configuration.
    config: InnerConfig,

    /// `quinn-proto` endpoint.
    endpoint: Endpoint,

    /// Everything related to connections.
    connections: Connections,

    /// Everything related to I/O.
    io: IO,

    /// Everything related to time & scheduling.
    time: Time,

    /// Events produced by this backend.
    events: BackendEvents<QuinnConnectionId>,

    /// Whether backend is open or not.
    open: bool,
}


impl QuicBackend for QuinnBackend {
    type Config = Config;
    type StableConnectionId = QuinnConnectionId;
    type OutBuf = VecBuf;

    fn new(mut config: Config, listen_addr: IpAddr, socket_features: &HashSet<SoFeat>) -> Self {
        config.fix();

        let connections = Connections::new();
        let events = BackendEvents::new();
        let io = IO::new(&config, listen_addr, socket_features);
        let time = Time::new();

        let endpoint_config = config.endpoint;
        let config = InnerConfig {
            client: config.client.map(|it| it.config),
            server: config.server.map(|it| Arc::new(it.config)),
        };
        let endpoint = Endpoint::new(
            Arc::new(endpoint_config),
            config.server.clone(),
            socket_features.contains(&SoFeat::DontFragment),
            None,
        );

        Self {
            config,
            endpoint,
            connections,
            io,
            time,
            events,
            open: true,
        }
    }

    fn clock_update(&mut self) {
        self.time.update_clock();
    }

    fn events(&mut self) -> &mut BackendEvents<QuinnConnectionId> {
        &mut self.events
    }


    fn close_prepare(&mut self, _deadline: Instant) {
        // We don't have anything to do.
    }

    fn close_ready(&self) -> bool {
        self.endpoint.open_connections() == 0
    }

    fn close(&mut self, err: u64, reason: Bytes) {
        if !self.open {
            return;
        }

        self.open = false;

        for (_, connection) in self.connections.all_iter_mark_active() {
            connection.close(self.time.now(), u64_into_varint(err), reason.clone());
        }
    }


    fn connect(
        &mut self,
        server_name: &ServerName,
        peer_addr: SocketAddr,
    ) -> Result<Self::StableConnectionId> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(client_config) = self.config.client.clone() else {
            return Err(Error::Illegal);
        };

        let (quinn_connection_key, quinn_connection) = self
            .endpoint
            .connect(
                self.time.now(),
                client_config,
                peer_addr,
                server_name.to_string().as_str(),
            )
            .map_err(|e| Error::Other(e.to_string().into()))?;

        let connection_id = self.register_new_connection(quinn_connection_key, quinn_connection);
        Ok(connection_id)
    }

    #[rustfmt::skip]
    fn connection_close(
        &mut self,
        connection_id: &QuinnConnectionId,
        err: u64,
        reason: Bytes,
    ) -> Result<()> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self
            .connections
            .get_mark_active_if(*connection_id, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        connection.close(self.time.now(), u64_into_varint(err), reason.clone());

        if let Some(event) = connection.get_close_event(
            connection_id.key,
            err,
            reason,
            true,
            true
        ) {
            self.events.push(event);
        }

        Ok(())
    }


    fn send(&mut self, out: &mut Vec<SendMsg<VecBuf>>) {
        self.handle_scheduled_events();

        while let Some((connection_id, connection)) = self.connections.peek_active() {
            if !self.io.has_primary_buffer() {
                break;
            }

            if connection.is_drained() {
                self.forget_connection(connection_id);
                self.connections.next_active();
                continue;
            }

            if !self.io.write_pending(connection, out, self.time.now()) {
                break;
            }

            self.connections.next_active();
            self.schedule_quinn_timeout(connection_id, None);
        }
    }

    fn recv<B: BufMut>(
        &mut self,
        in_packets: &mut MultiMsgFlattenIter<B>,
        out_messages: &mut Vec<SendMsg<Self::OutBuf>>,
    ) {
        while let Some(packet) = in_packets.peek() {
            let Some(mut buffer) = self.io.pop_reserved_buffer() else {
                break;
            };

            if let Some(event) = self.endpoint.handle(
                self.time.now(),
                packet.from,
                Some(packet.to),
                packet.ecn.into(),
                BytesMut::from(packet.as_ref()),
                &mut buffer,
            ) {
                buffer.clear();
                self.handle_quinn_datagram_event(event, buffer, out_messages);
            }

            in_packets.next();
        }
    }


    async fn sleep(&mut self) {
        self.time.sleep().await;
    }


    fn migrate_network(&mut self, new_listen_addr: Option<IpAddr>) -> Result<()> {
        if !self.open {
            return Err(Error::Closed);
        }
        if let Some(new_listen_addr) = new_listen_addr {
            if new_listen_addr.is_unspecified() {
                return Err(Error::Illegal);
            }

            self.io.update_socket_addr(new_listen_addr);
        }

        for (connection_id, connection) in &mut self.connections.all_iter_mark_active() {
            connection.local_address_changed();
            connection.path_changed(self.time.now());

            if let Some(event) = connection.get_dgram_out_size_event(connection_id.key) {
                self.events.push(event);
            }
        }

        Ok(())
    }

    fn peer_cert_chain(&mut self, connection_id: &QuinnConnectionId) -> Result<Vec<Vec<u8>>> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.get(*connection_id) else {
            return Err(Error::UnknownConnection);
        };
        let Some(identity) = connection.crypto_session().peer_identity() else {
            return Ok(Vec::new());
        };

        let Ok(certificates) = identity.downcast::<Vec<CertificateDer<'static>>>() else {
            return Err(Error::Other(
                format!(
                    "unable to cast peer_identity as '{}'",
                    type_name::<Vec<CertificateDer<'static>>>()
                )
                .into(),
            ));
        };

        Ok(certificates
            .into_iter()
            .map(|it| Vec::from(it.as_ref()))
            .collect())
    }


    fn stream_recv(
        &mut self,
        connection_id: &QuinnConnectionId,
        stream_id: StreamId,
        out: &mut [u8],
    ) -> Result<(usize, bool)> {
        if !self.open {
            return Err(Error::Closed);
        }
        if stream_id.is_client() && stream_id.is_uni() {
            return Err(Error::Illegal);
        }
        if out.is_empty() {
            return Err(Error::BufferSize);
        }

        let Some(connection) = self
            .connections
            .get_mark_active_if(*connection_id, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        let mut stream = connection.recv_stream(stream_id.into());
        let mut chunks = stream.read(true).map_err(|e| match &e {
            ReadableError::ClosedStream => Error::UnknownStream,

            // This should never happen.
            ReadableError::IllegalOrderedRead => {
                error!(
                    connection_id = %connection_id,
                    stream_id = %stream_id,
                    "unable to read from stream: Quinn returned an unexpected error: {e}"
                );
                debug_panic!("unable to read from stream, see logs above");
                Error::Other(e.to_string().into())
            }
        })?;

        let mut offset = 0;

        while offset < out.len() {
            match chunks.next(out.len() - offset) {
                Ok(Some(chunk)) => {
                    out[offset..chunk.bytes.len()].copy_from_slice(chunk.bytes.as_ref());
                    offset += chunk.bytes.len();
                }

                Ok(None) => {
                    return Ok((offset, true));
                }
                Err(ReadError::Blocked) => {
                    return Ok((offset, false));
                }
                Err(ReadError::Reset(err)) => {
                    return Err(Error::StreamReset(err.into_inner()));
                }
            };
        }

        Ok((offset, false))
    }

    fn stream_open_send(
        &mut self,
        connection_id: &QuinnConnectionId,
        bidirectional: bool,
        bytes: Bytes,
        fin: bool,
    ) -> Result<(StreamId, Option<Bytes>)> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self
            .connections
            .get_mark_active_if(*connection_id, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        use quinn_proto::Dir;

        let direction = if bidirectional { Dir::Bi } else { Dir::Uni };
        let Some(stream_id) = connection.streams().open(direction).map(|id| id.into()) else {
            return Err(Error::StreamsExhausted);
        };

        self.stream_send(connection_id, stream_id, bytes, fin)
            .map(|unsent_bytes| (stream_id, unsent_bytes))
    }

    fn stream_send(
        &mut self,
        connection_id: &QuinnConnectionId,
        stream_id: StreamId,
        bytes: Bytes,
        fin: bool,
    ) -> Result<Option<Bytes>> {
        if !self.open {
            return Err(Error::Closed);
        }
        if stream_id.is_server() && stream_id.is_uni() {
            return Err(Error::Illegal);
        }

        let Some(connection) = self
            .connections
            .get_mark_active_if(*connection_id, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        let mut stream = connection.send_stream(stream_id.into());
        let mut chunks = [bytes];
        let mut chunk_completely_written = false;

        if !chunks[0].is_empty() {
            match stream.write_chunks(&mut chunks) {
                Ok(written) => {
                    debug_assert!(written.chunks <= 1);

                    if written.chunks == 1 {
                        chunk_completely_written = true;
                    }
                }

                Err(WriteError::Blocked) => {
                    // Do nothing.
                }
                Err(WriteError::Stopped(err)) => {
                    return Err(Error::StreamStop(err.into_inner()));
                }
                Err(WriteError::ClosedStream) => {
                    return Err(Error::UnknownStream);
                }
            };
        } else {
            chunk_completely_written = true;
        }

        if fin && chunk_completely_written {
            match stream.finish() {
                Ok(_) => {
                    // Do nothing, success.
                }
                Err(FinishError::Stopped(err)) => {
                    return Err(Error::StreamStop(err.into_inner()));
                }
                Err(FinishError::ClosedStream) => {
                    return Err(Error::UnknownStream);
                }
            }
        }

        if chunk_completely_written {
            Ok(None)
        } else {
            Ok(Some(std::mem::take(&mut chunks[0])))
        }
    }

    fn stream_stop_sending(
        &mut self,
        connection_id: &QuinnConnectionId,
        stream_id: StreamId,
        err: u64,
    ) {
        if !self.open {
            return;
        }
        if stream_id.is_client() && stream_id.is_uni() {
            return;
        }
        let Some(connection) = self
            .connections
            .get_mark_active_if(*connection_id, is_connection_open)
        else {
            return;
        };

        let mut stream = connection.recv_stream(stream_id.into());
        let _ = stream.stop(u64_into_varint(err));
    }

    fn stream_reset_sending(
        &mut self,
        connection_id: &QuinnConnectionId,
        stream_id: StreamId,
        err: u64,
    ) {
        if !self.open {
            return;
        }
        if stream_id.is_server() && stream_id.is_uni() {
            return;
        }
        let Some(connection) = self
            .connections
            .get_mark_active_if(*connection_id, is_connection_open)
        else {
            return;
        };

        let mut stream = connection.send_stream(stream_id.into());
        let _ = stream.reset(u64_into_varint(err));
    }

    fn stream_set_priority(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        priority: Priority,
    ) {
        if !self.open {
            return;
        }
        if stream_id.is_server() && stream_id.is_uni() {
            return;
        }
        let Some(connection) = self
            .connections
            .get_mark_active_if(*connection_id, is_connection_open)
        else {
            return;
        };

        let mut stream = connection.send_stream(stream_id.into());
        let _ = stream.set_priority(priority.urgency as i32);
    }


    fn dgram_recv(&mut self, connection_id: &QuinnConnectionId, out: &mut [u8]) -> Result<usize> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.get_mut(*connection_id) else {
            return Err(Error::UnknownConnection);
        };

        let mut datagrams = connection.datagrams();
        let Some(datagram) = datagrams.recv() else {
            return Ok(0);
        };

        let length = datagram.len();
        if length > out.len() {
            return Err(Error::BufferSize);
        }

        out[..length].copy_from_slice(datagram.as_ref());
        Ok(length)
    }

    fn dgram_recv_zc<F, R>(
        &mut self,
        connection_id: &QuinnConnectionId,
        consumer: F,
    ) -> Result<Option<R>>
    where
        F: FnOnce(&[u8]) -> R,
    {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.get_mut(*connection_id) else {
            return Err(Error::UnknownConnection);
        };

        let mut datagrams = connection.datagrams();
        match datagrams.recv() {
            Some(datagram) => Ok(Some(consumer(datagram.as_ref()))),
            None => Ok(None),
        }
    }

    fn dgram_send(
        &mut self,
        connection_id: &QuinnConnectionId,
        bytes: Bytes,
    ) -> Result<Option<Bytes>> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self
            .connections
            .get_mark_active_if(*connection_id, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        if let Err(e) = connection.datagrams().send(bytes, false) {
            return match e {
                SendDatagramError::UnsupportedByPeer | SendDatagramError::Disabled => {
                    Err(Error::DgramDisabled)
                }
                SendDatagramError::TooLarge => Err(Error::BufferSize),
                SendDatagramError::Blocked(bytes) => Ok(Some(bytes)),
            };
        }

        Ok(None)
    }

    fn dgram_send_max_size(&mut self, connection_id: &QuinnConnectionId) -> Result<usize> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self
            .connections
            .get_mut(*connection_id)
            .filter(|it| is_connection_open(it))
        else {
            return Err(Error::UnknownConnection);
        };

        Ok(connection.datagrams().max_size().unwrap_or(0))
    }
}

impl QuinnBackend {
    /// Registers a new, unestablished connection.
    ///
    /// Schedules connection events,
    /// that are available on this moment.
    ///
    /// Accepts `establish_timeout_at` to schedule an establishment timeout,
    /// or `None` to use config's default.
    ///
    /// Returns a unique assigned ID for this connection,
    /// that will never change and should be used to get this connection next time.
    fn register_new_connection(
        &mut self,
        connection_key: ConnectionKey,
        connection: quinn_proto::Connection,
    ) -> QuinnConnectionId {
        let is_server = connection.side().is_server();
        let connection_id = self.connections.insert_raw(connection_key, connection);

        self.schedule_quinn_timeout(connection_id, None);
        self.events.push(ConnectionEvent::New {
            connection_id,
            is_server,
        });

        connection_id
    }

    /// Forgets closed connection, clean ups its resources.
    fn forget_connection(&mut self, connection_id: QuinnConnectionId) {
        let Some(mut connection) = self.connections.remove(connection_id) else {
            return;
        };

        if let Some(event) = connection.get_halt_event(
            connection_id.key,
            HaltKind::Error(Bytes::from_static(
                b"close event should've been already produced",
            )),
        ) {
            debug_panic!("no backend ConnectionEvent::Close/Halt was sent");
            self.events.push(event);
        }

        self.time.cancel_all(&mut connection);
    }


    /*
     * Handle an event.
     */

    /// Handles fired, previously scheduled events.
    fn handle_scheduled_events(&mut self) {
        while let Some(mut bucket) = self.time.fired_events().pop() {
            while let Some(event) = bucket.pop() {
                let Some(event) = event else {
                    continue;
                };

                if let Some(connection) = self.connections.get_mark_active(event.0) {
                    connection.handle_timeout(self.time.now())
                }
            }
        }
    }


    /// Handles a [QuinnDatagramEvent] event.
    ///
    /// The provided `buffer` can be used to write a response datagram.
    fn handle_quinn_datagram_event(
        &mut self,
        event: QuinnDatagramEvent,
        buffer: VecBuf,
        out: &mut Vec<SendMsg<VecBuf>>,
    ) {
        match event {
            QuinnDatagramEvent::ConnectionEvent(id, event) => {
                self.handle_quinn_connection_event(id, event);
            }
            QuinnDatagramEvent::Response(transmit) => {
                self.io.write_response(transmit, buffer, out);
            }
            QuinnDatagramEvent::NewConnection(incoming) => {
                self.handle_quinn_new_connection_event(incoming, buffer, out);
            }
        };
    }

    /// Handles a [QuinnConnectionEvent] event.
    fn handle_quinn_connection_event(
        &mut self,
        connection_key: ConnectionKey,
        connection_event: QuinnConnectionEvent,
    ) {
        let Some(connection) = self.connections.get_mark_active_raw(connection_key) else {
            return;
        };

        let connection_id = connection.id(connection_key);

        {
            let mut next_connection_event = Some(connection_event);
            while let Some(connection_event) = next_connection_event.take() {
                connection.handle_event(connection_event);

                while let Some(endpoint_event) = connection.poll_endpoint_events() {
                    if let Some(connection_event) = self
                        .endpoint
                        .handle_event(connection_id.key, endpoint_event)
                    {
                        next_connection_event = Some(connection_event);
                    }
                }
            }
        }


        loop {
            // Already marked as active, so no need to mark it again.
            let Some(connection) = self.connections.get_mut(connection_id) else {
                return;
            };
            let Some(event) = connection.poll() else {
                break;
            };

            match event {
                QuinnEvent::Connected => {
                    self.events
                        .push(ConnectionEvent::Established(connection_id));
                }
                QuinnEvent::ConnectionLost { reason } => {
                    self.handle_quinn_connection_lost_event(connection_id, reason);
                }

                QuinnEvent::Stream(stream_event) => {
                    self.handle_quinn_stream_event(connection_id, stream_event);
                }
                QuinnEvent::DatagramReceived => {
                    self.events.push(DatagramEvent::InAvailable(connection_id));
                }
                QuinnEvent::DatagramsUnblocked => {
                    self.events.push(DatagramEvent::OutAvailable(connection_id));
                }

                QuinnEvent::HandshakeDataReady => {}
            }
        }


        // Already marked as active, so no need to mark again.
        let Some(connection) = self.connections.get_mut(connection_id) else {
            return;
        };

        match connection.is_drained() {
            true => {
                self.forget_connection(connection_id);
            }
            false => {
                if let Some(event) = connection.get_dgram_out_size_event(connection_id.key) {
                    self.events.push(event);
                }

                self.schedule_quinn_timeout(connection_id, None);
            }
        }
    }

    /// Handles a [Incoming] new connection event.
    ///
    /// The provided `buffer` can be used to write a response datagram, e.g., a retry packet.
    fn handle_quinn_new_connection_event(
        &mut self,
        incoming: Incoming,
        mut buffer: VecBuf,
        out: &mut Vec<SendMsg<VecBuf>>,
    ) {
        let Some(server_config) = self.config.server.clone() else {
            self.endpoint.ignore(incoming);
            return;
        };

        if !incoming.remote_address_validated() {
            match self.endpoint.retry(incoming, buffer.as_mut()) {
                Ok(response) => {
                    self.io.write_response(response, buffer, out);
                }

                Err(e) => {
                    // This should never happen, because if `incoming.remote_address_validated()` is false,
                    // then `endpoint.retry()` must always return Ok().

                    let error = e.to_string();
                    let incoming = e.into_incoming();

                    error!(
                        peer_addr = %incoming.remote_address(),
                        original_dcid = %incoming.orig_dst_cid(),
                        "unable to send a 'retry' packet for an incoming connection
                        Quinn returned an unexpected error: {error}",
                    );
                    debug_panic!(
                        "cannot send retry packet for an incoming connection, see log above"
                    );

                    self.endpoint.ignore(incoming);
                }
            }

            return;
        }

        match self.endpoint.accept(
            incoming,
            self.time.now(),
            buffer.as_mut(),
            Some(server_config),
        ) {
            Ok((connection_key, connection)) => {
                self.register_new_connection(connection_key, connection);
            }
            Err(e) => {
                if let Some(response) = e.response {
                    self.io.write_response(response, buffer, out);
                }

                match e.cause {
                    ConnectionError::CidsExhausted => {
                        error!(
                            "unable to accept new server connection: \
                            not enough of connection ID space is available, \
                            try using longer connection IDs"
                        );
                    }

                    ConnectionError::VersionMismatch
                    | ConnectionError::TransportError(_)
                    | ConnectionError::ConnectionClosed(_)
                    | ConnectionError::ApplicationClosed(_)
                    | ConnectionError::Reset
                    | ConnectionError::TimedOut
                    | ConnectionError::LocallyClosed => {
                        // Noise, do nothing.
                    }
                }
            }
        };
    }

    /// Handles a Quinn connection lost event.
    fn handle_quinn_connection_lost_event(
        &mut self,
        connection_id: QuinnConnectionId,
        error: quinn_proto::ConnectionError,
    ) {
        let Some(connection) = self.connections.get_mark_active(connection_id) else {
            return;
        };

        let backend_event = match error {
            ConnectionError::VersionMismatch => {
                connection.get_halt_event(connection_id.key, HaltKind::UnknownVersion)
            }
            ConnectionError::TransportError(close) => connection.get_close_event(
                connection_id.key,
                close.code.into(),
                close.reason.into(),
                false,
                true,
            ),
            ConnectionError::ConnectionClosed(close) => connection.get_close_event(
                connection_id.key,
                close.error_code.into(),
                close.reason,
                false,
                false,
            ),
            ConnectionError::ApplicationClosed(close) => connection.get_close_event(
                connection_id.key,
                close.error_code.into(),
                close.reason,
                true,
                false,
            ),
            ConnectionError::TimedOut => {
                connection.get_halt_event(connection_id.key, HaltKind::Timeout)
            }
            ConnectionError::Reset => {
                connection.get_halt_event(connection_id.key, HaltKind::StatelessReset)
            }
            ConnectionError::CidsExhausted => {
                error!(
                    "connection is dropped: \
                    not enough of connection ID space is available, \
                    try using longer connection IDs"
                );
                connection.get_halt_event(
                    connection_id.key,
                    HaltKind::Error(Bytes::from_static(b"not enough CID space")),
                )
            }
            ConnectionError::LocallyClosed => {
                // The event should have already been sent on `connection_close()`.
                None
            }
        };

        if let Some(backend_event) = backend_event {
            self.events.push(backend_event);
        }
        if connection.is_drained() {
            self.forget_connection(connection_id);
        }
    }

    /// Handles a [QuinnStreamEvent] event.
    fn handle_quinn_stream_event(
        &mut self,
        connection_id: QuinnConnectionId,
        stream_event: QuinnStreamEvent,
    ) {
        let Some(connection) = self.connections.get_mark_active(connection_id) else {
            return;
        };

        let backend_event = match stream_event {
            QuinnStreamEvent::Opened { dir } => {
                let Some(stream_id) = connection.streams().accept(dir).map(StreamId::from) else {
                    return;
                };

                Event::Stream(StreamEvent::Available(connection_id, stream_id))
            }
            QuinnStreamEvent::Available { dir } => {
                Event::Connection(ConnectionEvent::StreamsAvailable {
                    connection_id,
                    is_bidirectional: matches!(dir, Dir::Bi),
                })
            }

            QuinnStreamEvent::Readable { id }
            | QuinnStreamEvent::Writable { id }
            | QuinnStreamEvent::Finished { id } => {
                Event::Stream(StreamEvent::Available(connection_id, id.into()))
            }
            QuinnStreamEvent::Stopped { id, error_code } => Event::Stream(StreamEvent::Reset(
                connection_id,
                id.into(),
                error_code.into_inner(),
            )),
        };

        self.events.push(backend_event);
    }


    /// Schedules an Quinn timeout event.
    ///
    /// If `timeout_at` is `None`, the default [quinn_proto::Connection::poll_timeout] will be used.
    ///
    /// [quinn_proto::Connection::handle_timeout] has to be invoked if the event fires.
    fn schedule_quinn_timeout(
        &mut self,
        connection_id: QuinnConnectionId,
        timeout_at: Option<Instant>,
    ) {
        let Some(connection) = self.connections.get_mut(connection_id) else {
            return;
        };
        let Some(timeout_at) = timeout_at.or_else(|| connection.poll_timeout()) else {
            return;
        };

        let key = self.time.reschedule(
            TimerEvent(connection_id),
            timeout_at,
            connection.pop_timer_key(),
        );
        connection.set_timer_key(Some(key));
    }
}


struct InnerConfig {
    client: Option<quinn_proto::ClientConfig>,
    server: Option<Arc<quinn_proto::ServerConfig>>,
}


fn is_connection_open(connection: &Connection) -> bool {
    !connection.is_closed() && !connection.is_drained()
}

fn u64_into_varint(value: u64) -> VarInt {
    VarInt::from_u64(value).unwrap_or_else(|e| {
        panic!("unable to convert `u64` ({value}) into quinn_proto::VarInt: {e}");
    })
}
