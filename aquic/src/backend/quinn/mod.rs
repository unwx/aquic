use crate::backend::cid::ConnectionIdGenerator;
use crate::backend::quinn::connection::{Connection, Connections};
use crate::backend::quinn::io::{IO, VecBuf};
use crate::backend::quinn::shared::{SyncConnectionIdGenerator, u64_into_varint};
use crate::backend::quinn::time::{Time, TimeoutEvent};
use crate::backend::{ConnectionEvent, Error, QuicBackend, StreamEvent};
use crate::backend::{OutEvents, Result};
use crate::core::ConstBuf;
use crate::net::{Buf, MAX_PACKET_SIZE, RecvMsg, SendMsg, ServerName, SoFeat};
use crate::stream::{Chunk, Priority, StreamId};
use bytes::{Bytes, BytesMut};
use quinn_proto::{
    ClientConfig, ConnectError, ConnectionError, DatagramEvent, Dir, Endpoint, Event, FinishError,
    IdleTimeout, ReadError, ReadableError, ServerConfig, Transmit, TransportConfig, WriteError,
};
use rustc_hash::{FxBuildHasher, FxHashSet};
use rustls::pki_types::CertificateDer;
use smallvec::SmallVec;
use std::any::type_name;
use std::collections::HashSet;
use std::future::{self};
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tracing::warn;


mod config;
mod connection;
mod io;
mod shared;
mod time;

pub use config::*;


type QuinnConnectionId = quinn_proto::ConnectionHandle;
type QuinnConnection = quinn_proto::Connection;
type QuinnConnectionEvent = quinn_proto::ConnectionEvent;
type QuinnStreamEvent = quinn_proto::StreamEvent;


/// The `quinn-proto` provider for the [QuicBackend].
///
/// [Quinn Project](https://github.com/quinn-rs/quinn)
pub struct QuinnBackend<CIdGen> {
    /// All quinn-related fields.
    quinn: Quinn,

    /// All connections-related fields.
    connections: Connections,

    /// All I/O-related fields.
    io: IO,

    /// All time/clock-related fields.
    time: Time,

    /// Socket's bind address.
    socket_addr: SocketAddr,

    /// Is backend open or not.
    open: bool,

    _phantom: PhantomData<CIdGen>,
}


impl<CIdGen> QuicBackend for QuinnBackend<CIdGen>
where
    CIdGen: ConnectionIdGenerator + 'static,
{
    type Config = Config;
    type WakeEvent = TimeoutEvent;
    type StableConnectionId = QuinnConnectionId;
    type ConnectionIdGenerator = CIdGen;
    type OutBuf = VecBuf;

    fn new(
        mut config: Self::Config,
        connection_id_generator: Self::ConnectionIdGenerator,
        socket_addr: SocketAddr,
        socket_features: &HashSet<SoFeat>,
    ) -> Self {
        config.validate();

        {
            let cid_gen = SyncConnectionIdGenerator::new(connection_id_generator);
            config
                .endpoint
                .cid_generator(move || Box::new(cid_gen.clone()));
        }

        let connections = Connections::new();
        let io = IO::new(&config, socket_features.contains(&SoFeat::Mmsg));
        let time = Time::new(&config);
        let quinn = Quinn::new(config, socket_features);

        Self {
            quinn,
            connections,
            io,
            time,
            socket_addr,
            open: true,
            _phantom: PhantomData,
        }
    }

    fn tick(&mut self, now: Instant) {
        self.time.clock = now;
    }


    fn prepare_to_shutdown(&self, _deadline: Instant) {
        // We don't have anything to do.
    }

    fn ready_to_shutdown(&self) -> bool {
        self.connections.all.is_empty()
    }

    fn close(&mut self, err: u64, reason: Bytes) {
        if !self.open {
            return;
        }

        self.open = false;

        for (connection_id, connection) in self.connections.all.iter_mut() {
            connection.close(self.time.clock, u64_into_varint(err), reason.clone());
            self.connections.modified.insert(*connection_id);
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
        let Some(client_config) = self.quinn.client_config.clone() else {
            return Err(Error::Illegal);
        };

        let (connection_id, connection) = self
            .quinn
            .endpoint
            .connect(
                self.time.clock,
                client_config,
                peer_addr,
                server_name.to_string().as_str(),
            )
            .map_err(|e| match e {
                ConnectError::EndpointStopping => Error::Closed,
                other => Error::Other(other.to_string().into()),
            })?;

        Self::register_connection(
            connection_id,
            connection,
            &mut self.connections,
            &mut self.time,
        );
        Ok(connection_id)
    }

    fn connection_close(
        &mut self,
        connection_id: &Self::StableConnectionId,
        err: u64,
        reason: Bytes,
    ) -> Result<()> {
        let Some(connection) = self.connections.all.get_mut(connection_id) else {
            return Err(Error::UnknownConnection);
        };

        connection.close(self.time.clock, u64_into_varint(err), reason);
        self.connections.modified.insert(*connection_id);

        Ok(())
    }


    fn send_prepare(
        &mut self,
        messages: &mut Vec<SendMsg<Self::OutBuf>>,
        _: &mut OutEvents<Self::StableConnectionId>,
    ) -> Result<()> {
        messages.append(&mut self.io.pending);

        if !self.io.has_primary_buffer() {
            return Ok(());
        }

        while let Some(connection_id) = self.connections.modified.pop() {
            if !self.io.has_primary_buffer() {
                return Ok(());
            }

            let Some(connection) = self.connections.all.get_mut(&connection_id) else {
                continue;
            };
            if connection.is_drained() {
                self.connections.all.remove(&connection_id);
                continue;
            }

            let buffer = self.io.peek_buffer();
            let max_datagrams = MAX_PACKET_SIZE / (connection.current_mtu() as usize);

            if let Some(transmit) = connection.poll_transmit(self.time.clock, max_datagrams, buffer)
            {
                let buffer = self.io.pop_buffer();
                messages.push(Self::make_send_msg(transmit, buffer, self.socket_addr));
            }

            Self::schedule_connection_timeout(connection_id, connection, &mut self.time);
        }

        Ok(())
    }


    fn send_done<I: IntoIterator<Item = Self::OutBuf>>(&mut self, buffers: I) {
        self.io.buffers.extend(buffers.into_iter().map(|mut buf| {
            buf.clear();
            buf
        }));
    }

    #[allow(clippy::explicit_counter_loop)]
    fn recv(
        &mut self,
        messages: &mut [RecvMsg<ConstBuf<MAX_PACKET_SIZE>>],
        out_events: &mut OutEvents<Self::StableConnectionId>,
    ) -> Result<usize> {
        if !self.io.has_any_buffer() {
            return Ok(0);
        }

        let mut processed = 0;
        for message in messages.iter_mut() {
            if !self.io.has_any_buffer() {
                return Ok(processed);
            }

            processed += 1;
            let buffer = self.io.peek_buffer();

            let Some(event) = self.quinn.endpoint.handle(
                self.time.clock,
                message.from(),
                Some(message.to()),
                message.ecn().into(),
                BytesMut::from(&message.slice_read()[..]),
                buffer,
            ) else {
                continue;
            };

            match event {
                DatagramEvent::ConnectionEvent(id, event) => {
                    Self::handle_connection_event(
                        id,
                        event,
                        &mut self.connections,
                        &mut self.quinn,
                        &mut self.time,
                        out_events,
                    );
                }

                DatagramEvent::Response(transmit) => {
                    Self::queue_send_msg(transmit, self.socket_addr, &mut self.io);
                }

                DatagramEvent::NewConnection(incoming) => {
                    let Some(server_config) = self.quinn.server_config.clone() else {
                        self.quinn.endpoint.ignore(incoming);
                        continue;
                    };

                    if !incoming.remote_address_validated() {
                        match self.quinn.endpoint.retry(incoming, buffer) {
                            Ok(transmit) => {
                                Self::queue_send_msg(transmit, self.socket_addr, &mut self.io);
                                continue;
                            }

                            Err(it) => {
                                // This should never happen, because if `incoming.remote_address_validated()` is false,
                                // then `endpoint.retry()` must always return true.

                                if cfg!(debug_assertions) {
                                    panic!(
                                        "unable to craft a 'retry' frame for an incoming connection"
                                    );
                                }

                                let incoming = it.into_incoming();

                                warn!(
                                    "unable to craft a 'retry' frame for an incoming connection \
                                    [remote_address: {}, original_source_id: {}]: is this a bug?; connection is dropped",
                                    incoming.remote_address(),
                                    incoming.orig_dst_cid()
                                );

                                self.quinn.endpoint.ignore(incoming);
                                continue;
                            }
                        }
                    }

                    match self.quinn.endpoint.accept(
                        incoming,
                        self.time.clock,
                        buffer,
                        Some(server_config),
                    ) {
                        Ok((connection_id, connection)) => {
                            Self::register_connection(
                                connection_id,
                                connection,
                                &mut self.connections,
                                &mut self.time,
                            );

                            let outgoing_event = ConnectionEvent::Created(connection_id);
                            out_events.push_connection_event(outgoing_event);
                        }
                        Err(e) => {
                            if let Some(transmit) = e.response {
                                Self::queue_send_msg(transmit, self.socket_addr, &mut self.io);
                            }

                            match e.cause {
                                ConnectionError::CidsExhausted => {
                                    warn!(
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
            };
        }

        Ok(messages.len())
    }


    async fn sleep(&mut self) {
        let Some(timer) = self.time.timeout_timer.as_mut() else {
            future::pending::<()>().await;
            return;
        };

        let Some(next_instant) = self.time.next_timeout_tick.as_mut() else {
            // No timeout event is scheduled yet.
            future::pending::<()>().await;
            return;
        };

        // `next_instant` is a &mut reference: it will be updated automatically.
        timer
            .next(&mut self.time.timeout_events, next_instant)
            .await;
    }

    fn on_alarm(&mut self) -> Result<()> {
        if !self.open {
            self.time.timeout_events.clear();
            return Ok(());
        }

        while let Some(event) = self.time.timeout_events.pop() {
            let connection_id = event.0;

            let Some(connection) = self.connections.all.get_mut(&connection_id) else {
                continue;
            };

            connection.handle_timeout(self.time.clock);
            self.connections.modified.insert(connection_id);
        }

        Ok(())
    }


    fn peer_cert_chain(
        &mut self,
        connection_id: &Self::StableConnectionId,
    ) -> Result<Vec<Vec<u8>>> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.all.get_mut(connection_id) else {
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


    fn stream_open(
        &mut self,
        connection_id: &Self::StableConnectionId,
        bidirectional: bool,
    ) -> Result<StreamId> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.all.get_mut(connection_id) else {
            return Err(Error::UnknownConnection);
        };

        let direction = if bidirectional { Dir::Bi } else { Dir::Uni };
        let Some(stream_id) = connection.streams().open(direction).map(|id| id.into()) else {
            return Err(Error::StreamsExhausted);
        };

        Ok(stream_id)
    }

    fn stream_recv(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        mut threshold: usize,
    ) -> Result<(SmallVec<[Chunk; 32]>, bool)> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.all.get_mut(connection_id) else {
            return Err(Error::UnknownConnection);
        };

        let ordered = !connection.unordered_streams.contains(&stream_id);

        let mut stream = connection.recv_stream(u64_into_varint(stream_id).into());
        let mut chunks = stream.read(ordered).map_err(|e| match e {
            ReadableError::ClosedStream => Error::UnknownStream,

            // This should never happen,
            // as we only allow to make an ordered stream -> unordered,
            ReadableError::IllegalOrderedRead => Error::Other(
                format!(
                    "unable to read from stream. \
                    [connection_id: {:?}, stream_id: {}, error: {}]",
                    connection_id, stream_id, e
                )
                .into(),
            ),
        })?;

        let mut output = SmallVec::<[Chunk; 32]>::new();
        let mut fin = false;

        while threshold != 0 && output.len() < output.inline_size() {
            match chunks.next(threshold) {
                Ok(Some(chunk)) => {
                    threshold = threshold.saturating_sub(chunk.bytes.len());

                    if ordered {
                        output.push(Chunk::Ordered(chunk.bytes));
                    } else {
                        output.push(Chunk::Unordered(chunk.bytes, chunk.offset));
                    }
                }
                Ok(None) => {
                    fin = true;
                    break;
                }

                Err(ReadError::Blocked) => {
                    break;
                }
                Err(ReadError::Reset(err)) => {
                    drop(chunks);
                    connection.unordered_streams.remove(&stream_id);
                    return Err(Error::StreamResetSending(err.into_inner()));
                }
            };
        }

        if fin {
            drop(chunks);
            connection.unordered_streams.remove(&stream_id);
        }
        if output.is_empty() && !fin {
            return Err(Error::StreamFinish);
        }

        Ok((output, fin))
    }

    fn stream_send(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        chunks: &mut SmallVec<[Bytes; 32]>,
        fin: bool,
    ) -> Result<bool> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.all.get_mut(connection_id) else {
            return Err(Error::UnknownConnection);
        };

        let mut stream = connection.send_stream(u64_into_varint(stream_id).into());
        while let Some(mut chunk) = chunks.pop() {
            match stream.write(chunk.as_ref()) {
                Ok(write) => {
                    if write == chunk.len() {
                        continue;
                    }

                    chunks.push(chunk.split_to(write));
                    break;
                }

                Err(WriteError::Blocked) => {
                    break;
                }
                Err(WriteError::ClosedStream) => {
                    return Err(Error::UnknownStream);
                }
                Err(WriteError::Stopped(err)) => {
                    return Err(Error::StreamStopSending(err.into_inner()));
                }
            };
        }

        if fin && chunks.is_empty() {
            match stream.finish() {
                Ok(_) => {
                    // Do nothing, success.
                }
                Err(FinishError::ClosedStream) => {
                    return Err(Error::UnknownStream);
                }
                Err(FinishError::Stopped(err)) => {
                    return Err(Error::StreamStopSending(err.into_inner()));
                }
            }
        }

        let partial_write = !chunks.is_empty();
        Ok(partial_write)
    }

    fn stream_stop_sending(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        err: u64,
    ) {
        if !self.open {
            return;
        }
        let Some(connection) = self.connections.all.get_mut(connection_id) else {
            return;
        };

        let mut stream = connection.recv_stream(u64_into_varint(stream_id).into());
        let _ = stream.stop(u64_into_varint(err));
    }

    fn stream_reset_sending(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        err: u64,
    ) {
        if !self.open {
            return;
        }
        let Some(connection) = self.connections.all.get_mut(connection_id) else {
            return;
        };

        let mut stream = connection.send_stream(u64_into_varint(stream_id).into());
        let _ = stream.reset(u64_into_varint(err));
    }

    fn stream_set_priority(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
        priority: Priority,
    ) -> Result<()> {
        // Only [Priority::urgency] is used, where
        // [Priority::urgency] `zero` means default priority.

        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.all.get_mut(connection_id) else {
            return Err(Error::UnknownConnection);
        };

        let mut stream = connection.send_stream(u64_into_varint(stream_id).into());
        if stream.set_priority(priority.urgency as i32).is_err() {
            return Err(Error::UnknownStream);
        }

        Ok(())
    }

    fn stream_set_unordered(
        &mut self,
        connection_id: &Self::StableConnectionId,
        stream_id: StreamId,
    ) -> Result<()> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.all.get_mut(connection_id) else {
            return Err(Error::UnknownConnection);
        };

        if connection.unordered_streams.contains(&stream_id) {
            return Ok(());
        }

        let mut stream = connection.recv_stream(u64_into_varint(stream_id).into());
        if let Err(e) = stream.read(true) {
            match e {
                ReadableError::ClosedStream => {
                    return Err(Error::UnknownStream);
                }
                ReadableError::IllegalOrderedRead => {
                    // Do nothing, though this should never happen...
                }
            }
        }

        connection.unordered_streams.insert(stream_id);
        Ok(())
    }
}

impl<CidGen> QuinnBackend<CidGen> {
    fn register_connection(
        connection_id: QuinnConnectionId,
        connection: QuinnConnection,
        connections: &mut Connections,
        time: &mut Time,
    ) {
        let mut internal_connection = Connection {
            inner: connection,
            unordered_streams: FxHashSet::with_hasher(FxBuildHasher),
            timeout_key: None,
        };

        Self::schedule_connection_timeout(connection_id, &mut internal_connection, time);
        connections.all.insert(connection_id, internal_connection);
        connections.modified.insert(connection_id);
    }

    fn make_send_msg(
        transmit: Transmit,
        buffer: VecBuf,
        socket_addr: SocketAddr,
    ) -> SendMsg<VecBuf> {
        debug_assert_eq!(buffer.len(), transmit.size);

        SendMsg::new(
            buffer,
            transmit.src_ip.unwrap_or(socket_addr.ip()),
            transmit.destination,
            transmit.ecn.into(),
            transmit.segment_size.unwrap_or(0),
        )
    }

    fn queue_send_msg(transmit: Transmit, socket_addr: SocketAddr, io: &mut IO) {
        let buffer = io.pop_buffer();
        let message = Self::make_send_msg(transmit, buffer, socket_addr);
        io.pending.push(message);
    }


    fn handle_connection_event(
        connection_id: QuinnConnectionId,
        connection_event: QuinnConnectionEvent,
        connections: &mut Connections,
        quinn: &mut Quinn,
        time: &mut Time,
        out_events: &mut OutEvents<QuinnConnectionId>,
    ) {
        let Some(connection) = connections.all.get_mut(&connection_id) else {
            return;
        };

        {
            let mut next_connection_event = Some(connection_event);
            while let Some(connection_event) = next_connection_event.take() {
                connection.handle_event(connection_event);

                while let Some(endpoint_event) = connection.poll_endpoint_events() {
                    if let Some(connection_event) =
                        quinn.endpoint.handle_event(connection_id, endpoint_event)
                    {
                        next_connection_event = Some(connection_event);
                        break;
                    }
                }
            }
        }

        while let Some(event) = connection.poll() {
            match event {
                Event::Connected => {
                    let outgoing_event = ConnectionEvent::Active(connection_id);
                    out_events.push_connection_event(outgoing_event);
                }
                Event::HandshakeDataReady => {
                    let outgoing_event = ConnectionEvent::HandshakeDataReady(connection_id);
                    out_events.push_connection_event(outgoing_event);
                }
                Event::ConnectionLost { reason } => {
                    let outgoing_event = match reason {
                        ConnectionError::VersionMismatch => {
                            Some(ConnectionEvent::ClosedVersionMismatch(connection_id))
                        }
                        ConnectionError::TransportError(error) => Some(ConnectionEvent::Closed {
                            connection_id,
                            code: error.code.into(),
                            reason: error.reason.into(),
                            is_transport: true,
                            is_local: true,
                        }),
                        ConnectionError::ConnectionClosed(close) => Some(ConnectionEvent::Closed {
                            connection_id,
                            code: close.error_code.into(),
                            reason: close.reason.clone(),
                            is_transport: true,
                            is_local: false,
                        }),
                        ConnectionError::ApplicationClosed(close) => {
                            Some(ConnectionEvent::Closed {
                                connection_id,
                                code: close.error_code.into(),
                                reason: close.reason.clone(),
                                is_transport: false,
                                is_local: false,
                            })
                        }
                        ConnectionError::TimedOut => {
                            Some(ConnectionEvent::ClosedTimeout(connection_id))
                        }
                        ConnectionError::Reset => Some(ConnectionEvent::ClosedUnknown {
                            connection_id,
                            reason: "connection reset".into(),
                        }),
                        ConnectionError::CidsExhausted => Some(ConnectionEvent::ClosedUnknown {
                            connection_id,
                            reason: "not enough CID space".into(),
                        }),
                        ConnectionError::LocallyClosed => None,
                    };

                    if let Some(event) = outgoing_event {
                        out_events.push_connection_event(event);
                    }
                    if connection.is_drained() {
                        connections.modified.swap_remove(&connection_id);
                        connections.all.remove(&connection_id);
                        return;
                    }
                }

                Event::Stream(stream_event) => {
                    Self::handle_stream_event(connection_id, stream_event, connection, out_events);
                }

                // TODO(feat): QUIC connection datagrams.
                Event::DatagramReceived => todo!(),
                Event::DatagramsUnblocked => todo!(),
            }
        }

        if !connection.is_drained() {
            connections.modified.insert(connection_id);
            Self::schedule_connection_timeout(connection_id, connection, time);
        }
    }

    fn handle_stream_event(
        connection_id: QuinnConnectionId,
        stream_event: QuinnStreamEvent,
        connection: &mut Connection,
        out_events: &mut OutEvents<QuinnConnectionId>,
    ) {
        let outgoing_event = match stream_event {
            QuinnStreamEvent::Opened { dir } => {
                let Some(stream_id) = connection.streams().accept(dir).map(StreamId::from) else {
                    return;
                };

                StreamEvent::Open {
                    connection_id,
                    stream_id,
                    bidirectional: matches!(dir, Dir::Bi),
                }
            }
            QuinnStreamEvent::Available { dir } => StreamEvent::Available {
                connection_id,
                bidirectional: matches!(dir, Dir::Bi),
            },

            QuinnStreamEvent::Readable { id } => StreamEvent::InActive(connection_id, id.into()),
            QuinnStreamEvent::Writable { id } => StreamEvent::OutActive(connection_id, id.into()),
            QuinnStreamEvent::Stopped { id, error_code } => {
                StreamEvent::InReset(connection_id, id.into(), error_code.into())
            }

            QuinnStreamEvent::Finished { .. } => {
                return;
            }
        };

        out_events.push_stream_event(outgoing_event);
    }


    fn schedule_connection_timeout(
        connection_id: QuinnConnectionId,
        connection: &mut Connection,
        time: &mut Time,
    ) {
        let Some(timer) = time.timeout_timer.as_mut() else {
            return;
        };

        let Some(timeout) = connection.poll_timeout() else {
            if let Some(previous_key) = connection.timeout_key {
                // No more need in `on_timeout()` call.
                timer.cancel(previous_key);
            }

            return;
        };

        if time.next_timeout_tick.is_none() {
            // Make sure to create an accurate TimerWheel clock, if it's absent.
            time.next_timeout_tick = Some(time.clock + time.timeout_tick_duration);
        }

        // `self.clock` may be in the past, but will never be in the future.
        //
        // The clock skew duration (actual_time - self.clock)
        // is an additional time we **may** wait until this `TimeoutEvent` fires.
        // But the event won't fire before the desired `timeout`.
        //
        // In most cases `self.clock` should be almost identical to `Instant::now()`,
        // therefore the lag should not exceed a few ms.
        let key = timer.schedule_instant_ceil(
            TimeoutEvent(connection_id),
            time.clock,
            timeout,
            connection.timeout_key,
        );

        connection.timeout_key = Some(key);
    }
}

struct Quinn {
    /// `quinn-proto` API.
    endpoint: Endpoint,

    /// Server configuration, or `None` to reject all server connection attempts.
    server_config: Option<Arc<ServerConfig>>,

    /// Client configuration, or `None` to reject all client connection attempts.
    client_config: Option<ClientConfig>,
}

impl Quinn {
    pub fn new(mut config: Config, socket_features: &HashSet<SoFeat>) -> Self {
        let setup_transport = |mut transport: TransportConfig| -> Arc<TransportConfig> {
            if socket_features.contains(&SoFeat::GenericSegOffload) {
                transport.enable_segmentation_offload(true);
            }

            transport.max_idle_timeout(Some(
                IdleTimeout::try_from(config.max_idle_timeout)
                    .expect("provided `conn_max_timeout` is too long"),
            ));

            Arc::new(transport)
        };

        if let Some(client) = &mut config.client {
            let transport = setup_transport(config.client_transport.unwrap_or_default());
            client.transport_config(transport);
        }
        if let Some(server) = &mut config.server {
            let transport = setup_transport(config.server_transport.unwrap_or_default());
            server.transport_config(transport);
        }

        let endpoint_config = Arc::new(config.endpoint);
        let server_config = config.server.map(Arc::new);
        let client_config = config.client;
        let endpoint = Endpoint::new(
            endpoint_config,
            None,
            socket_features.contains(&SoFeat::DontFragment),
            None,
        );

        Self {
            endpoint,
            client_config,
            server_config,
        }
    }
}
