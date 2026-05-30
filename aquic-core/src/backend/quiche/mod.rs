use crate::{
    backend::{
        BackendEvents, ConnectionEvent, Error, HaltKind, QuicBackend, Result, StreamEvent, Token,
        TokenError, TokenGenerator, TokenKind,
        cid::ConnectionIdGenerator,
        quiche::{
            connection::{Connection, ConnectionKey, Connections, QuicheConnectionId},
            io::{ForceWrite, IO},
            session::{QuicSession, SessionCache},
            time::{Time, TimerEvent, TimerKind},
        },
    },
    net::{BufMut, SendMsg, ServerName, SoFeat},
    runtime::AsyncRuntime,
    stream::{Priority, StreamId},
    util::BipView,
};
use crate::{
    backend::{
        cid::MAX_CID_LEN,
        firewall::Firewall,
        quiche::{config::ServerTypes, io::BytesFactory},
    },
    debug_panic,
    net::{MultiMsgFlattenIter, Packet},
};
use bytes::Bytes;
use quiche::{RecvInfo, WireErrorCode};
use std::{
    any::type_name,
    collections::HashSet,
    fmt::Display,
    net::{IpAddr, SocketAddr},
    time::Instant,
};
use tracing::error;


mod config;
mod connection;
mod io;
mod session;
mod time;

pub use config::Config;


const _: () = assert!(
    quiche::MAX_CONN_ID_LEN == MAX_CID_LEN,
    "quiche::MAX_CONN_ID_LEN is not equal to aquic::MAX_CID_LEN",
);


/// The `quiche` QUIC implementation provider.
///
/// [Quiche GitHub](https://github.com/cloudflare/quiche)
pub struct QuicheBackend<CIdG: ConnectionIdGenerator, SCfg: ServerTypes = ()> {
    /// Client/Server configuration.
    config: Config<CIdG, SCfg>,

    /// Everything related to connections.
    connections: Connections,

    /// Everything related to I/O.
    io: IO,

    /// Everything related to time & scheduling.
    time: Time,

    /// Client sessions cache, used for 0-RTT connection resumption.
    session_cache: SessionCache,

    /// Events produced by this backend.
    events: BackendEvents<QuicheConnectionId>,

    /// Address which I/O socket listens to.
    listen_addr: IpAddr,

    /// Whether backend is open or not.
    open: bool,
}

impl<CIdG, SCfg> QuicBackend for QuicheBackend<CIdG, SCfg>
where
    CIdG: ConnectionIdGenerator,
    SCfg: ServerTypes,
{
    type Config = Config<CIdG, SCfg>;

    type StableConnectionId = QuicheConnectionId;

    type OutBuf = BipView;


    fn new(
        mut config: Config<CIdG, SCfg>,
        listen_addr: IpAddr,
        socket_features: &HashSet<SoFeat>,
    ) -> Self {
        config.fix();

        {
            let enable_pmtud = socket_features.contains(&SoFeat::DontFragment);

            if let Some(client) = config.client.as_mut() {
                client.quiche.discover_pmtu(enable_pmtud);
            }
            if let Some(server) = config.server.as_mut() {
                server.quiche.discover_pmtu(enable_pmtud);
            }
        }

        let session_cache = match config.client.as_ref() {
            Some(config) => SessionCache::new(
                config.session_cache_max_capacity,
                config.session_cache_time_to_live,
                config.session_cache_time_to_idle,
            ),
            None => SessionCache::new(0, None, None),
        };

        Self {
            connections: Connections::new(),
            io: IO::new(config.out_buffer_size),
            time: Time::new(),
            session_cache,
            events: BackendEvents::new(),
            listen_addr,
            config,
            open: true,
        }
    }

    fn clock_update(&mut self) {
        self.time.update_clock();
    }

    fn events(&mut self) -> &mut BackendEvents<QuicheConnectionId> {
        &mut self.events
    }


    fn close_prepare(&mut self, _deadline: Instant) {
        // We don't have anything to do.
    }

    fn close_ready(&self) -> bool {
        self.connections.all_len() == 0
    }

    fn close(&mut self, err: u64, reason: Bytes) {
        if !self.open {
            return;
        }

        self.open = false;

        for (_, connection) in self.connections.all_iter_mark_active() {
            let _ = connection.close(true, err, reason.as_ref());
        }
    }


    fn connect(
        &mut self,
        server_name: &ServerName,
        peer_addr: SocketAddr,
    ) -> Result<QuicheConnectionId> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(config) = self.config.client.as_mut() else {
            return Err(Error::Illegal);
        };

        let mut connection = {
            let server_name = server_name.to_string();
            let scid = self.config.connection_id_generator.generate();

            let server_name_ref = Some(server_name.as_str());
            let scid_ref = quiche::ConnectionId::from_ref(scid.as_slice());

            let local_addr = to_sock_addr(self.listen_addr);

            quiche::connect_with_buffer_factory::<BytesFactory>(
                server_name_ref,
                &scid_ref,
                local_addr,
                peer_addr,
                &mut config.quiche,
            )
            .map(|it| Connection::new(it, true))
            .map_err(|e| Error::Other(e.to_string().into()))?
        };

        if let Some(session) = self.session_cache.get(server_name) {
            if let Err(e) = connection.set_session(session.tls_ticket.as_ref()) {
                error!(
                    scid = ?connection.source_id(),
                    dcid = ?connection.destination_id(),
                    "unable to reuse {server_name} TLS session for a new connection: {e}"
                );
                debug_panic!("unable to reuse TLS session for a new connection, see logs above");
            }
        }

        let connection_id = self.register_new_connection(connection, peer_addr);
        Ok(connection_id)
    }

    fn connection_close(
        &mut self,
        connection_id: &QuicheConnectionId,
        err: u64,
        reason: Bytes,
    ) -> Result<()> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self
            .connections
            .get_mark_active_if(connection_id.key, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        if let Err(e) = connection.close(true, err, reason.as_ref()) {
            return match e {
                quiche::Error::Done => {
                    // Connection is already closed.
                    Err(Error::UnknownConnection)
                }
                e => Err(Error::Other(e.to_string().into())),
            };
        }

        if let Some(event) = connection.get_close_event(connection_id.key) {
            self.events.push(event);
        }

        Ok(())
    }


    fn send(&mut self, out: &mut Vec<SendMsg<BipView>>) {
        self.handle_scheduled_events();
        let mut active_connections_count = self.connections.active_len();

        while let Some((connection_key, connection)) = self.connections.peek_active() {
            if active_connections_count == 0 {
                // Break here, as we don't want to handle reactivated connections.
                // We will handle them later, on the next `send()` call.
                break;
            }
            active_connections_count -= 1;

            if connection.is_closed() {
                self.forget_connection(connection_key);
                self.connections.next_active();
                continue;
            }

            match self.send_packets(connection_key, out) {
                SendResult::Done => {
                    self.connections.next_active();
                }
                SendResult::Reactivate => {
                    self.connections.next_active();
                    self.connections.mark_active(connection_key);
                }
                SendResult::StopAndRetry => {
                    break;
                }
            }
        }
    }

    fn recv<B: BufMut>(
        &mut self,
        in_packets: &mut MultiMsgFlattenIter<B>,
        out_messages: &mut Vec<SendMsg<BipView>>,
    ) {
        let cid_length = self.config.connection_id_generator.cid_len();

        while let Some(mut packet) = in_packets.peek() {
            let mut header = match quiche::Header::from_slice(packet.payload, cid_length) {
                Ok(it) => it,
                Err(_) => {
                    in_packets.next();
                    continue;
                }
            };

            if matches!(header.ty, quiche::Type::Initial) {
                if self.open
                    && !self.handle_initial_packet_as_server(&mut header, &mut packet, out_messages)
                {
                    // We need retry this packet when buffer becomes available.
                    break;
                }

                in_packets.next();
                continue;
            }

            // If there is a registered quiche connection, it will handle this packet.
            //
            // For client connections, we immediately register them.
            //
            // For server connections, we register them on the first `Initial` packet,
            // or on the second `Initial` with a retry token.
            if self.delegate_packet(&header, &mut packet) {
                in_packets.next();
                continue;
            }

            self.handle_packet_with_stateless_reset(&header, &packet, out_messages);
            in_packets.next();
        }
    }


    async fn sleep<AR: AsyncRuntime>(&mut self) {
        self.time.sleep::<AR>().await;
    }


    fn migrate_network(&mut self, new_listen_addr: Option<IpAddr>) -> Result<()> {
        if !self.open {
            return Err(Error::Closed);
        }
        if let Some(new_listen_addr) = new_listen_addr {
            if new_listen_addr.is_unspecified() {
                return Err(Error::Illegal);
            }

            self.listen_addr = new_listen_addr;
        }

        enum Decision {
            MarkActive,
            Forget,
        }

        let mut decisions: Vec<(ConnectionKey, Decision)> =
            Vec::with_capacity(usize::min(256, self.connections.all_len()));

        for (connection_key, connection) in self
            .connections
            // We just iterate over all existing connections
            // instead of tracking specifically client ones using a separate collection,
            // because:
            // - Active migrations should not happen often.
            // - If an application decides to use this backend as a server,
            //   then most likely it's a stable network environment.
            //
            //   Therefore, we should iterate over client connections only here,
            //   without much miss penalties.
            .all_iter()
            .filter(|(_, conn)| !conn.is_server())
        {
            match connection.migrate_source(to_sock_addr(self.listen_addr)) {
                Ok(_) => {
                    if let Some(event) = connection.get_dgram_out_size_event(connection_key) {
                        self.events.push(event);
                    }

                    decisions.push((connection_key, Decision::MarkActive));
                }
                Err(_) => {
                    if let Some(event) =
                        connection.get_halt_event(connection_key, HaltKind::MigrationFailed)
                    {
                        self.events.push(event);
                    }

                    decisions.push((connection_key, Decision::Forget));
                }
            }
        }

        for (connection_key, decision) in decisions {
            match decision {
                Decision::MarkActive => {
                    self.connections.mark_active(connection_key);
                }
                Decision::Forget => {
                    self.forget_connection(connection_key);
                }
            }
        }

        Ok(())
    }

    fn peer_cert_chain(&mut self, connection_id: &QuicheConnectionId) -> Result<Vec<Vec<u8>>> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.get(connection_id.key) else {
            return Err(Error::UnknownConnection);
        };

        match connection.peer_cert_chain() {
            Some(certs) => Ok(certs.into_iter().map(Vec::from).collect()),
            None => Ok(Vec::new()),
        }
    }


    fn stream_recv(
        &mut self,
        connection_id: &QuicheConnectionId,
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
            .get_mark_active_if(connection_id.key, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        match connection.stream_recv(stream_id.raw(), out) {
            Ok(result) => Ok(result),
            Err(quiche::Error::Done) => Ok((0, false)),
            Err(quiche::Error::StreamReset(err)) => Err(Error::StreamReset(err)),
            Err(quiche::Error::InvalidStreamState(_)) => Err(Error::UnknownStream),
            Err(quiche::Error::StreamStopped(_)) => Err(Error::UnknownStream),
            Err(other) => Err(Error::Other(other.to_string().into())),
        }
    }

    fn stream_open_send(
        &mut self,
        connection_id: &QuicheConnectionId,
        bidirectional: bool,
        bytes: Bytes,
        fin: bool,
    ) -> Result<(StreamId, Option<Bytes>)> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self
            .connections
            .get_mark_active_if(connection_id.key, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        if connection.peer_streams_left(bidirectional) == 0 {
            connection.set_streams_exhausted(bidirectional);
            return Err(Error::StreamsExhausted);
        }
        let Ok(stream_id) = connection.allocate_stream_id(bidirectional) else {
            connection.set_streams_exhausted(bidirectional);
            return Err(Error::StreamsExhausted);
        };

        self.events
            .push(StreamEvent::Available(*connection_id, stream_id));

        self.stream_send(connection_id, stream_id, bytes, fin)
            .map(|unsent_bytes| (stream_id, unsent_bytes))
    }

    fn stream_send(
        &mut self,
        connection_id: &QuicheConnectionId,
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
            .get_mark_active_if(connection_id.key, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        match (connection.is_server(), stream_id.is_server()) {
            (true, true) | (false, false) => {
                // Locally-initiated stream.

                match connection.get_last_local_stream_id(stream_id.is_bidi()) {
                    Some(last_opened_stream_id) => {
                        if last_opened_stream_id < stream_id {
                            return Err(Error::UnknownStream);
                        }
                    }
                    None => {
                        return Err(Error::UnknownStream);
                    }
                }
            }
            _ => {
                // Quiche should return IllegalStreamState on unknown non-local streams.
            }
        }

        match connection.stream_send_zc(
            stream_id.raw(),
            bytes.clone().into(),
            Some(bytes.len()),
            fin,
        ) {
            Ok((_, unsent)) => Ok(unsent.map(Bytes::from)),
            Err(quiche::Error::Done) => Ok(Some(bytes)),
            Err(quiche::Error::StreamStopped(err)) => Err(Error::StreamStop(err)),
            Err(quiche::Error::InvalidStreamState(_)) => Err(Error::UnknownStream),
            Err(quiche::Error::StreamReset(_)) => Err(Error::UnknownStream),
            Err(other) => Err(Error::Other(other.to_string().into())),
        }
    }

    fn stream_stop_sending(
        &mut self,
        connection_id: &QuicheConnectionId,
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
            .get_mark_active_if(connection_id.key, is_connection_open)
        else {
            return;
        };

        let _ = connection.stream_shutdown(stream_id.raw(), quiche::Shutdown::Read, err);
    }

    fn stream_reset_sending(
        &mut self,
        connection_id: &QuicheConnectionId,
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
            .get_mark_active_if(connection_id.key, is_connection_open)
        else {
            return;
        };

        let _ = connection.stream_shutdown(stream_id.raw(), quiche::Shutdown::Write, err);
    }

    fn stream_set_priority(
        &mut self,
        connection_id: &QuicheConnectionId,
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
            .get_mark_active_if(connection_id.key, is_connection_open)
        else {
            return;
        };

        let _ = connection.stream_priority(
            stream_id.raw(),
            (127_i16 - (priority.urgency as i16)) as u8,
            priority.incremental,
        );
    }


    fn dgram_recv(&mut self, connection_id: &QuicheConnectionId, out: &mut [u8]) -> Result<usize> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.get_mut(connection_id.key) else {
            return Err(Error::UnknownConnection);
        };

        let datagram = match connection.dgram_recv_vec() {
            Ok(it) => it,
            Err(quiche::Error::Done) => {
                connection.set_dgram_in_drained();
                return Ok(0);
            }
            Err(e) => {
                return Err(Error::Other(e.to_string().into()));
            }
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
        connection_id: &QuicheConnectionId,
        consumer: F,
    ) -> Result<Option<R>>
    where
        F: FnOnce(&[u8]) -> R,
    {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self.connections.get_mut(connection_id.key) else {
            return Err(Error::UnknownConnection);
        };

        let datagram = match connection.dgram_recv_vec() {
            Ok(it) => it,
            Err(quiche::Error::Done) => {
                connection.set_dgram_in_drained();
                return Ok(None);
            }
            Err(e) => {
                return Err(Error::Other(e.to_string().into()));
            }
        };

        Ok(Some(consumer(datagram.as_ref())))
    }

    fn dgram_send(
        &mut self,
        connection_id: &QuicheConnectionId,
        bytes: Bytes,
    ) -> Result<Option<Bytes>> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self
            .connections
            .get_mark_active_if(connection_id.key, is_connection_open)
        else {
            return Err(Error::UnknownConnection);
        };

        match connection.dgram_send(bytes.as_ref()) {
            Ok(_) => Ok(None),
            Err(quiche::Error::Done) => {
                connection.set_dgram_out_drained();
                Ok(Some(bytes))
            }
            Err(quiche::Error::BufferTooShort) => Err(Error::BufferSize),
            Err(e) => Err(Error::Other(e.to_string().into())),
        }
    }

    fn dgram_send_max_size(&mut self, connection_id: &QuicheConnectionId) -> Result<usize> {
        if !self.open {
            return Err(Error::Closed);
        }
        let Some(connection) = self
            .connections
            .get(connection_id.key)
            .filter(|it| is_connection_open(it))
        else {
            return Err(Error::UnknownConnection);
        };

        Ok(connection.dgram_max_writable_len().unwrap_or(0))
    }
}

impl<CIdG, SCfg> QuicheBackend<CIdG, SCfg>
where
    CIdG: ConnectionIdGenerator,
    SCfg: ServerTypes,
{
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
        connection: Connection,
        peer_addr: SocketAddr,
    ) -> QuicheConnectionId {
        debug_assert!(!connection.is_established());
        let is_server = connection.is_server();
        let connection_id = self.connections.insert(connection, peer_addr);

        self.schedule_quiche_timeout(connection_id.key, None);
        self.schedule_establish_timeout(connection_id.key, None);

        self.events.push(ConnectionEvent::New {
            connection_id,
            is_server,
        });

        connection_id
    }

    /// Checks the `Handshake` progress on this connection.
    ///
    /// If connection is fully established,
    /// cancels an establishment timeout and
    /// generates spare CIDs for the peer to use.
    fn check_connection_handshake_progress(&mut self, connection_key: ConnectionKey) {
        let Some(connection) = self.connections.get_mut(connection_key) else {
            return;
        };

        if connection.is_established() {
            if let Some(key) = connection.pop_timer_key(TimerKind::EstablishTimeout) {
                self.time.cancel(key);
            }

            'session: {
                if connection.is_server() {
                    break 'session;
                }
                let Some(session) = connection.session() else {
                    break 'session;
                };
                let Some(server_name) = connection.server_name() else {
                    break 'session;
                };

                match ServerName::try_from(server_name) {
                    Ok(name) => {
                        self.session_cache.insert(
                            name,
                            QuicSession::new(Vec::from(session).into_boxed_slice()),
                        );
                    }
                    Err(e) => {
                        error!(
                            connection_id = %connection.id(connection_key),
                            "unable to store client connection TLS session into the session cache: \
                            unable to parse server_name ({server_name}) into aquic::net::ServerName: {e}"
                        );
                        debug_panic!(
                            "unable to store client connection TLS session into the session cache, see logs above"
                        );
                    }
                }
            }

            self.allocate_scids(connection_key);
        }
    }

    /// Forgets closed or dead connection, clean ups its resources.
    fn forget_connection(&mut self, connection_key: ConnectionKey) {
        let Some(mut connection) = self.connections.remove(connection_key) else {
            return;
        };

        if let Some(event) = connection.get_close_event(connection_key) {
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
                if let Some(event) = event {
                    self.handle_scheduled_event(event);
                }
            }
        }
    }

    /// Handles a fired event.
    fn handle_scheduled_event(&mut self, event: TimerEvent) {
        let connection_key = event.connection_key;

        let Some(connection) = self.connections.get_mark_active(connection_key) else {
            return;
        };

        match event.kind {
            TimerKind::Pacer => {
                // We've already marked a connection as active,
                // that's enough.
            }

            TimerKind::QuicheTimeout => {
                connection.on_timeout();
            }

            TimerKind::EstablishTimeout => {
                if connection.is_established() {
                    return;
                }

                if let Some(close_event) =
                    connection.get_halt_event(connection_key, HaltKind::Timeout)
                {
                    self.events.push(close_event);
                }

                if connection.is_server() {
                    let _ = connection.close(
                        false,
                        WireErrorCode::ConnectionRefused as u64,
                        b"timeout",
                    );
                } else {
                    self.forget_connection(connection_key);
                }
            }
        }
    }

    /// Handles connection [quiche::PathEvent]s.
    fn handle_path_events(&mut self, connection_key: ConnectionKey) {
        let Some(connection) = self.connections.get_mut(connection_key) else {
            return;
        };

        let mut validated = false;
        let mut migrated = false;

        while let Some(event) = connection.path_event_next() {
            use quiche::PathEvent;

            match event {
                PathEvent::Validated(_, _) => {
                    validated = true;
                }
                PathEvent::PeerMigrated(_, _) => {
                    migrated = true;
                }
                _ => {}
            }
        }

        if validated {
            self.connections.mark_verified(connection_key);
        }
        if migrated {
            self.allocate_scids(connection_key);
        }
    }


    /*
     * Handle a packet.
     */

    /// Delegates a `packet` directly to matching quiche connection.
    ///
    /// Returns `true` if connection was found and the `packet` was processed.
    fn delegate_packet(&mut self, header: &quiche::Header, packet: &mut Packet) -> bool {
        let Some(connection_key) = self
            .connections
            .find_connection_key(&header.dcid, packet.from)
        else {
            return false;
        };
        let Some(connection) = self.connections.get_mark_active(connection_key) else {
            return false;
        };

        if connection.is_closed() {
            self.forget_connection(connection_key);
            return true;
        }

        let recv_info = RecvInfo {
            from: packet.from,
            to: to_sock_addr(packet.to),
        };

        match connection.recv(packet.payload, recv_info) {
            Ok(_) => {
                if matches!(header.ty, quiche::Type::Handshake) {
                    self.check_connection_handshake_progress(connection_key);
                }
            }
            Err(e) => {
                use quiche::Error;
                debug_assert!(
                    connection.local_error().is_some(),
                    "quiche closes the connection on 'recv()' error"
                );

                match e {
                    Error::UnknownVersion => {
                        if connection.is_draining() || connection.is_closed() {
                            if let Some(event) =
                                connection.get_halt_event(connection_key, HaltKind::UnknownVersion)
                            {
                                self.events.push(event);
                            }
                        }
                    }

                    Error::InvalidFrame
                    | Error::InvalidPacket
                    | Error::InvalidTransportParam
                    | Error::CryptoFail
                    | Error::TlsFail
                    | Error::FlowControl
                    | Error::StreamLimit
                    | Error::FinalSize
                    | Error::KeyUpdate
                    | Error::CryptoBufferExceeded
                    | Error::InvalidAckRange
                    | Error::OptimisticAckDetected => {}

                    Error::Done
                    | Error::BufferTooShort
                    | Error::InvalidState
                    | Error::InvalidStreamState(_)
                    | Error::StreamStopped(_)
                    | Error::StreamReset(_)
                    | Error::CongestionControl
                    | Error::IdLimit
                    | Error::OutOfIdentifiers
                    | Error::InvalidDcidInitialization => {
                        error!(
                            peer_addr = %packet.from,
                            peer_scid = ?header.scid,
                            peer_dcid = ?header.dcid,
                            peer_version = header.version,
                            connection_id = %connection.id(connection_key),
                            "unable to handle a packet: Quiche returned an unexpected error: {e}"
                        );

                        debug_panic!("unable to handle a packet, see logs above");
                    }
                }

                if connection.is_draining() || connection.is_closed() {
                    if let Some(event) = connection.get_close_event(connection_key) {
                        self.events.push(event);
                    }
                }
            }
        }

        self.handle_path_events(connection_key);
        self.write_stream_dgram_events(connection_key);
        self.schedule_quiche_timeout(connection_key, None);
        true
    }

    /// Handles an `Initial` `packet` as a server.
    ///
    /// # Returns
    ///
    /// - `true` if the `packet` was processed.
    /// - `false` if the `packet` needs to be retried when internal I/O buffer becomes available again.
    fn handle_initial_packet_as_server(
        &mut self,
        header: &mut quiche::Header,
        packet: &mut Packet,
        out: &mut Vec<SendMsg<BipView>>,
    ) -> bool {
        debug_assert_eq!(header.ty, quiche::Type::Initial);

        let Some(config) = self.config.server.as_mut() else {
            return true;
        };

        fn populate_and_log(msg: impl Display, header: &quiche::Header, packet: &Packet) {
            let msg = format!("unable to accept a new server connection: {msg}");

            error!(
                peer_addr = %packet.from,
                peer_scid = ?header.scid,
                peer_dcid = ?header.dcid,
                peer_version = header.version,
                msg
            );

            debug_panic!("unable to accept a new server connection, see logs above");
        }


        /*
         * Verify version compliance.
         */

        if !quiche::version_is_supported(header.version) {
            return match self.io.write_version_negotiation(header, packet, out) {
                Ok(ForceWrite::Complete) => true,
                Ok(ForceWrite::BufferTooShort) => false,
                Err(e) => {
                    populate_and_log(
                        format!(
                            "unable to write 'version negotiation' packet: \
                            Quiche returned an unexpected error: {e}"
                        ),
                        header,
                        packet,
                    );

                    true
                }
            };
        }

        if header.scid.len() > MAX_CID_LEN || header.dcid.len() > MAX_CID_LEN {
            // Silently drop the packet.
            //
            // This packet might be malicious: its version matches the one we support,
            // but its connection IDs do not follow the standard.
            return true;
        }


        /*
         * `Initial` token verification,
         * conditional `Retry` response.
         */

        let token = match header.token.as_ref() {
            Some(token) => {
                match config.token_generator.verify_token(
                    packet.from.ip(),
                    header.scid.as_ref(),
                    header.dcid.as_ref(),
                    token.as_slice(),
                    self.time.now_sys(),
                ) {
                    Ok(token) => Some(token),

                    Err(TokenError::Expired(TokenKind::Identity, _)) | Err(TokenError::Invalid) => {
                        // https://datatracker.ietf.org/doc/html/rfc9000#section-8.1.3-10
                        None
                    }

                    Err(TokenError::Expired(TokenKind::Retry, _)) => {
                        // https://datatracker.ietf.org/doc/html/rfc9000#section-8.1.2-5
                        //
                        // Silently drop the packet.
                        //
                        // Maybe we should return `INVALID_TOKEN` in the future,
                        // but I don't see a reason of implementing this, at least right now.
                        //
                        // Clients should use retry tokens as soon as they receive them,
                        // so a scenario of expired `Retry` should be highly unprobable in practice,
                        // unless it's malicious.
                        return true;
                    }
                }
            }
            None => None,
        };

        let server_new_scid = self.config.connection_id_generator.generate();
        if server_new_scid.len() > MAX_CID_LEN {
            populate_and_log(
                format!(
                    "{} generated a connection ID longer than {} bytes: {}",
                    type_name::<CIdG>(),
                    MAX_CID_LEN,
                    server_new_scid
                ),
                header,
                packet,
            );

            return true;
        }

        if token.is_none()
            && !config.firewall.allow_unverified(
                packet.from,
                self.connections.server_connections_count(),
                self.connections.server_unverified_connections_count(),
            )
        {
            let Some(retry_token) = config.token_generator.generate_retry_token(
                packet.from.ip(),
                header.scid.as_ref(),
                header.dcid.as_ref(),
                server_new_scid.as_slice(),
                self.time.now_sys() + config.establish_timeout,
            ) else {
                // Should never happen,
                // as we've verified the length of the every connection ID.

                populate_and_log("unable to generate 'retry' token", header, packet);
                return true;
            };

            return match self.io.write_retry(
                header,
                packet,
                &quiche::ConnectionId::from_ref(server_new_scid.as_slice()),
                retry_token,
                out,
            ) {
                Ok(ForceWrite::Complete) => true,
                Ok(ForceWrite::BufferTooShort) => false,
                Err(e) => {
                    populate_and_log(
                        format!(
                            "unable to write 'retry' packet: \
                            Quiche returned an unexpected error: {e}"
                        ),
                        header,
                        packet,
                    );

                    true
                }
            };
        }


        /*
         * Accept connection.
         */

        let connection = {
            let server_new_scid_ref = quiche::ConnectionId::from_ref(server_new_scid.as_slice());
            let peer_original_dcid_ref = match token.as_ref() {
                Some(Token::Retry {
                    peer_original_dcid, ..
                }) => {
                    let id = quiche::ConnectionId::from_ref(peer_original_dcid.as_slice());
                    Some(id)
                }
                _ => None,
            };
            let local_addr = to_sock_addr(self.listen_addr);
            let peer_addr = packet.from;

            match quiche::accept_with_buf_factory::<BytesFactory>(
                &server_new_scid_ref,
                peer_original_dcid_ref.as_ref(),
                local_addr,
                peer_addr,
                &mut config.quiche,
            ) {
                Ok(connection) => connection,
                Err(e) => {
                    populate_and_log(
                        format!("Quiche returned an unexpected error: {e}"),
                        header,
                        packet,
                    );

                    return true;
                }
            }
        };

        self.register_new_connection(Connection::new(connection, token.is_some()), packet.from);
        true
    }

    /// Handles a `packet` as lost.
    ///
    /// This is a last resort for packets that we can't accept or delegate to a connection,
    /// so we send a stateless reset if possible.
    fn handle_packet_with_stateless_reset(
        &mut self,
        header: &quiche::Header,
        packet: &Packet,
        out: &mut Vec<SendMsg<BipView>>,
    ) {
        let Some(config) = self.config.server.as_mut() else {
            return;
        };

        let reset_token = config
            .token_generator
            .generate_reset_token(header.dcid.as_ref());

        self.io.write_stateless_reset(packet, &reset_token, out);
    }


    /*
     * Schedule an event.
     */

    /// Schedules a pacer event.
    ///
    /// If `release_time` is `None`, the default [Connection::get_next_release_time] will be used.
    ///
    /// It is forbidden by pacing algorithm to send packets until the event fires.
    fn schedule_pacer(&mut self, connection_key: ConnectionKey, release_time: Option<Instant>) {
        let Some(connection) = self.connections.get_mut(connection_key) else {
            return;
        };

        let Some(release_time) =
            release_time.or_else(|| connection.get_next_release_time(self.time.now_mono()))
        else {
            return;
        };

        let kind = TimerKind::Pacer;
        let key = self.time.reschedule(
            TimerEvent {
                connection_key,
                kind,
            },
            release_time,
            connection.pop_timer_key(kind),
        );

        connection.set_timer_key(kind, Some(key));
    }

    /// Schedules an Quiche timeout event.
    ///
    /// If `timeout_at` is `None`, the default [quiche::Connection::timeout_instant] will be used.
    ///
    /// [quiche::Connection::on_timeout] has to be invoked if the event fires.
    fn schedule_quiche_timeout(
        &mut self,
        connection_key: ConnectionKey,
        timeout_at: Option<Instant>,
    ) {
        let Some(connection) = self.connections.get_mut(connection_key) else {
            return;
        };
        let Some(timeout_at) = timeout_at.or_else(|| connection.timeout_instant()) else {
            return;
        };

        let kind = TimerKind::QuicheTimeout;
        let key = self.time.reschedule(
            TimerEvent {
                connection_key,
                kind,
            },
            timeout_at,
            connection.pop_timer_key(kind),
        );

        connection.set_timer_key(kind, Some(key));
    }

    /// Schedules an establishment timeout event.
    ///
    /// If `timeout_at` is `None`, the default config's timeout will be used.
    ///
    /// Connection has to become [`established`]([quiche::Connection::is_established])
    /// before the event fires, or else the connection has to be closed.
    fn schedule_establish_timeout(
        &mut self,
        connection_key: ConnectionKey,
        timeout_at: Option<Instant>,
    ) {
        let Some(connection) = self.connections.get_mut(connection_key) else {
            return;
        };

        let timeout_at = timeout_at.or_else(|| {
            let connection_establish_timeout = {
                if connection.is_server() {
                    self.config.server.as_ref().map(|it| it.establish_timeout)
                } else {
                    self.config
                        .client
                        .as_ref()
                        .and_then(|it| it.establish_timeout)
                }
            };

            connection_establish_timeout.map(|timeout| self.time.now_mono() + timeout)
        });

        let Some(timeout_at) = timeout_at else {
            return;
        };

        let kind = TimerKind::EstablishTimeout;
        let key = self.time.reschedule(
            TimerEvent {
                connection_key,
                kind,
            },
            timeout_at,
            connection.pop_timer_key(kind),
        );

        connection.set_timer_key(kind, Some(key));
    }


    /*
     * Other
     */

    /// Writes new stream/datagram readable/writable events.
    fn write_stream_dgram_events(&mut self, connection_key: ConnectionKey) {
        let Some(connection) = self.connections.get_mut(connection_key) else {
            return;
        };

        self.events
            .extend(connection.get_connection_streams_available_events(connection_key));

        {
            fn on_stream_event(
                connection_key: ConnectionKey,
                stream_id: u64,
                connection: &mut Connection,
                events: &mut BackendEvents<QuicheConnectionId>,
            ) {
                let Ok(stream_id) = StreamId::new(stream_id) else {
                    error!(
                        connection_id = %connection.id(connection_key),
                        "unable to handle next readable/writable stream: \
                        unable to convert Quiche StreamID ({stream_id}) into aquic StreamID"
                    );
                    debug_panic!("unable to convert Quiche StreamID, see logs above");
                    return;
                };

                events.push(StreamEvent::Available(
                    connection.id(connection_key),
                    stream_id,
                ));
            }

            while let Some(stream_id) = connection.stream_readable_next() {
                on_stream_event(connection_key, stream_id, connection, &mut self.events);
            }
            while let Some(stream_id) = connection.stream_writable_next() {
                on_stream_event(connection_key, stream_id, connection, &mut self.events);
            }
        }

        if !connection.is_dgram_recv_queue_full() {
            if let Some(event) = connection.get_dgram_in_available_event(connection_key) {
                self.events.push(event);
            }
        }
        if !connection.is_dgram_send_queue_full() {
            if let Some(event) = connection.get_dgram_out_available_event(connection_key) {
                self.events.push(event);
            }
        }

        if let Some(event) = connection.get_dgram_out_size_event(connection_key) {
            self.events.push(event);
        }
    }

    /// Writes packets into `&mut out`,
    /// using a connection associated with the specified `connection_key`.
    ///
    /// Additionally polls all neccessary functions afterwards: timeout, stream/dgram limits.
    fn send_packets(
        &mut self,
        connection_key: ConnectionKey,
        out: &mut Vec<SendMsg<BipView>>,
    ) -> SendResult {
        let Some(connection) = self.connections.get_mut(connection_key) else {
            return SendResult::Done;
        };

        let connection_id = connection.id(connection_key);
        let write_result = self.io.write_pending(connection, out, self.time.now_mono());

        self.write_stream_dgram_events(connection_key);
        self.schedule_quiche_timeout(connection_key, None);

        match write_result {
            Ok(result) => {
                use io::Write;

                match result {
                    Write::Complete => SendResult::Done,
                    Write::Pacer(release_at) => {
                        // Fired pacer event will reactivate the connection.
                        self.schedule_pacer(connection_key, Some(release_at));
                        SendResult::Done
                    }
                    Write::Quantum => SendResult::Reactivate,
                    Write::BufferTooShort => SendResult::StopAndRetry,
                }
            }
            Err(e) => {
                // This should not happen...

                error!(
                    connection_id = %connection_id,
                    "unable to write an outgoing packet: \
                    Quiche returned an unexpected error: {e}"
                );

                debug_panic!("unable to write an outgoing packet, see logs above");
                SendResult::Done
            }
        }
    }

    /// Allocates new server source connection IDs for the peer,
    /// and automatically registers them in local CIDs dictionary.
    ///
    /// Without spare SCIDs (peer's Destination CIDs),
    /// peer will not be able to perform active migration.
    ///
    /// If current CID generator generates zero-length CIDs,
    /// this method is no-op.
    fn allocate_scids(&mut self, connection_key: ConnectionKey) {
        if self.config.connection_id_generator.cid_len() == 0 {
            return;
        }
        let Some(config) = self.config.server.as_mut() else {
            return;
        };
        let Some(connection) = self.connections.get_mark_active(connection_key) else {
            return;
        };

        if !connection.is_established() {
            return;
        }

        for _ in 0..connection.scids_left() {
            let scid = self.config.connection_id_generator.generate();
            let scid_ref = quiche::ConnectionId::from_ref(scid.as_slice());
            let reset_token = config.token_generator.generate_reset_token(scid.as_ref());

            if let Err(e) = connection.new_scid(&scid_ref, u128::from_be_bytes(reset_token), false)
            {
                error!(
                    connection_id = %connection.id(connection_key),
                    "unable to allocate a new source connection ID: \
                    Quiche returned an unexpected error: {e}",
                );

                debug_panic!("unable to allocate SCID, see logs above");
                break;
            }
        }

        self.connections.update_scids_dict(connection_key);
    }
}


/// Converts an IP address into socket address,
/// for Quiche to consume.
///
/// Quiche doesn't perform any special logic using port,
/// therefore we just hardcode it.
fn to_sock_addr(ip: IpAddr) -> SocketAddr {
    SocketAddr::new(ip, u16::MAX)
}

fn is_connection_open(connection: &Connection) -> bool {
    !connection.is_draining() && !connection.is_closed()
}


/// A result of an attempt to send packets.
#[derive(Debug, Copy, Clone)]
enum SendResult {
    /// Everything is done,
    /// connection should be no more active.
    Done,

    /// Connection needs to be polled,
    /// but it should lose it's place in the queue.
    ///
    /// Can move to the next connection.
    Reactivate,

    /// Connection needs to be polled
    /// without losing it's place in the queue.
    ///
    /// No further connections may be handled.
    StopAndRetry,
}
