use crate::backend::cid::{ConnectionId, MAX_CID_LEN};
use crate::backend::quiche::time::TimerKind;
use crate::backend::{ConnectionEvent, DatagramEvent, HaltKind, StableConnectionId};
use crate::debug_panic;
use crate::stream::{StreamId, StreamIdError};
use crate::{backend::quiche::BytesFactory, util::TimerKey};
use ahash::AHashMap;
use bytes::Bytes;
use hashlink::LinkedHashSet;
use rand::{Rng, rng};
use slotmap::{SlotMap, new_key_type};
use std::fmt::{self, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::{
    net::SocketAddr,
    ops::{Deref, DerefMut},
    time::{Duration, Instant},
};
use tracing::error;
use uuid::Uuid;


/// A Quiche stable connection ID.
#[derive(Debug, Copy, Clone)]
pub struct QuicheConnectionId {
    pub(super) key: ConnectionKey,
    pub(super) trace: Uuid,
}

impl QuicheConnectionId {
    pub(super) fn new(key: ConnectionKey, trace: Uuid) -> Self {
        Self { key, trace }
    }
}

impl PartialEq for QuicheConnectionId {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for QuicheConnectionId {}

impl Hash for QuicheConnectionId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl Display for QuicheConnectionId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.trace)
    }
}

impl StableConnectionId for QuicheConnectionId {}

new_key_type! {
    pub(super) struct ConnectionKey;
}


pub(super) struct Connections {
    /// Open server and client connections.
    ///
    /// May also contain newly-created client connections,
    /// and unverified server connections as well.
    ///
    /// **Note**: unverified means that path between peer <-> server
    /// has not verified yet, and the peer may be spoofed.
    all: SlotMap<ConnectionKey, Connection>,

    /// Source connection ID (peer's DCID) -> `ConnectionKey` mapping.
    cid_dictionary: AHashMap<ConnectionId, ConnectionKey>,

    /// Peer's socket address -> `ConnectionKey` mapping.
    ///
    /// This map is used only if application decides to generate zero-length source connection IDs
    /// and to use peer's socket address for routing.
    addr_dictionary: AHashMap<SocketAddr, ConnectionKey>,

    /// Modified `all` connections that wait to be polled.
    ///
    /// LinkedHashSet is used for effective, fair (FIFO) polling.
    active: LinkedHashSet<ConnectionKey, foldhash::fast::RandomState>,

    /// Number of server connections.
    server_connections_count: usize,

    /// Number of unverified server connections.
    server_unverified_connections_count: usize,
}

impl Connections {
    pub fn new() -> Self {
        Self {
            all: SlotMap::with_key(),
            cid_dictionary: AHashMap::new(),
            addr_dictionary: AHashMap::new(),
            active: LinkedHashSet::with_hasher(foldhash::fast::RandomState::default()),
            server_connections_count: 0,
            server_unverified_connections_count: 0,
        }
    }

    /// Inserts a new connection and marks it as active.
    pub fn insert(&mut self, connection: Connection, peer_addr: SocketAddr) -> QuicheConnectionId {
        let source_cid = {
            let cid = connection.source_id();

            ConnectionId::try_from_slice(cid.as_ref()).unwrap_or_else(|| {
                // Should never happen.

                panic!(
                    "Connection ID ({:?}) exceeds maximum allowed CID size: {}",
                    cid, MAX_CID_LEN
                )
            })
        };

        let is_server = connection.is_server();
        let is_verified = connection.peer_address_verified;

        // Probably we don't need a `peer_addr` parameter,
        // but it's better than `unwrap()`.
        let peer_addr = connection
            .path_stats()
            .find(|it| it.active)
            .map(|it| it.peer_addr)
            .unwrap_or(peer_addr);

        let connection_trace = connection.trace;
        let connection_key = self.all.insert(connection);
        self.active.replace(connection_key);

        if source_cid.is_empty() {
            self.addr_dictionary.insert(peer_addr, connection_key);
        } else {
            self.cid_dictionary.insert(source_cid, connection_key);
        }

        if is_server {
            self.server_connections_count += 1;

            if !is_verified {
                self.server_unverified_connections_count += 1;
            }
        }

        QuicheConnectionId::new(connection_key, connection_trace)
    }


    /// Returns an immutable connection.
    pub fn get(&self, key: ConnectionKey) -> Option<&Connection> {
        self.all.get(key)
    }

    /// Returns a mutable connection.
    pub fn get_mut(&mut self, key: ConnectionKey) -> Option<&mut Connection> {
        self.all.get_mut(key)
    }

    /// Returns a mutable connection and marks it as active, to be polled later.
    ///
    /// If connection is already marked as active, it's no-op.
    /// Otherwise, it will be put in the back of the awaiting queue.
    pub fn get_mark_active(&mut self, key: ConnectionKey) -> Option<&mut Connection> {
        self.get_mark_active_if(key, |_| true)
    }

    /// Returns a mutable connection if it satisfies the specified `filter`, and marks it as active.
    ///
    /// If connection is already marked as active, it's no-op.
    /// Otherwise, it will be put in the back of the awaiting queue.
    pub fn get_mark_active_if<F>(
        &mut self,
        key: ConnectionKey,
        filter: F,
    ) -> Option<&mut Connection>
    where
        F: FnOnce(&Connection) -> bool,
    {
        self.all.get_mut(key).filter(|it| filter(it)).inspect(|_| {
            self.active.replace(key);
        })
    }

    /// Returns a mutable iterator for all present connections.
    pub fn all_iter(&mut self) -> impl Iterator<Item = (ConnectionKey, &mut Connection)> + '_ {
        self.all.iter_mut()
    }

    /// Returns a mutable iterator for all present connections,
    /// marking each active.
    pub fn all_iter_mark_active(
        &mut self,
    ) -> impl Iterator<Item = (ConnectionKey, &mut Connection)> + '_ {
        self.all.iter_mut().inspect(|(key, _)| {
            self.active.replace(*key);
        })
    }

    /// Returns a stable connection key based on peer's destination ID,
    /// or his socket address if DCID is empty.
    pub fn find_connection_key(
        &self,
        peer_dcid: &quiche::ConnectionId,
        peer_addr: SocketAddr,
    ) -> Option<ConnectionKey> {
        let connection_key = {
            if !peer_dcid.is_empty() {
                self.cid_dictionary.get(peer_dcid.as_ref()).copied()?
            } else {
                self.addr_dictionary.get(&peer_addr).copied()?
            }
        };

        debug_assert!(self.all.contains_key(connection_key));
        Some(connection_key)
    }

    /// Returns a number of all present connections.
    pub fn all_len(&self) -> usize {
        self.all.len()
    }


    /// Marks a connection with the specified `key` as active.
    ///
    /// If it wasn't active before, this connection will be positioned in the end of the active queue.
    pub fn mark_active(&mut self, key: ConnectionKey) {
        if self.all.contains_key(key) {
            self.active.replace(key);
        }
    }

    /// Returns an active connection.
    ///
    /// This method will return the same connection, until [`next_active`](Self::next_active) is called.
    ///
    /// Connections are iterated in fair, FIFO order.
    pub fn peek_active(&mut self) -> Option<(ConnectionKey, &mut Connection)> {
        loop {
            let connection_key = self.active.front().copied()?;

            // Should be always `true`.
            if self.all.contains_key(connection_key) {
                let connection = self.all.get_mut(connection_key).unwrap();
                return Some((connection_key, connection));
            }

            self.next_active();
        }
    }

    /// Removes an `active` mark from the last [`peeked`](Self::peek_active) connection
    /// by removing it from the queue, and moves the cursor to the next one.
    pub fn next_active(&mut self) {
        self.active.pop_front();
    }

    /// Returns number of active connections that wait to be polled.
    pub fn active_len(&self) -> usize {
        self.active.len()
    }


    /// Marks connection as verified.
    ///
    /// It is applied only to server connections,
    /// where "verified" means that there is at least one validated path,
    /// where the peer confirmed that his address is not spoofed.
    ///
    /// This is used for [`Firewall::allow_unverified()`](crate::backend::Firewall::allow_unverified).
    pub fn mark_verified(&mut self, key: ConnectionKey) {
        let Some(connection) = self.all.get_mut(key) else {
            return;
        };
        if !connection.is_server() {
            return;
        }

        if !connection.peer_address_verified {
            connection.peer_address_verified = true;
            self.server_unverified_connections_count -= 1;
        }
    }

    /// Updates the internal `SCID` to `ConnectionKey` mapping,
    /// based on a connection associated with the provided `key`.
    ///
    /// It is required to be updated after new CIDs allocations,
    /// or connection migrations.
    pub fn update_scids_dict(&mut self, key: ConnectionKey) {
        let Some(connection) = self.all.get_mut(key) else {
            return;
        };

        for cid in connection.source_ids() {
            if let Some(cid) = ConnectionId::try_from_slice(cid.as_ref()) {
                self.cid_dictionary.insert(cid, key);
            } else {
                // Should never happen.
                //
                // If this for some reason happens in the real world,
                // we just skip this CID,
                // and the backend won't be able recognize incoming packets with this CID.

                error!(
                    connection_id = %connection.id(key),
                    "bug: unable to convert Quiche Connection ID into aquic internal ID representation, \
                    ID: {cid:?}"
                );
                debug_panic!("unable to convert quiche ID into internal one, see logs above");
            }
        }

        while let Some(cid) = connection.retired_scid_next() {
            self.cid_dictionary.remove(cid.as_ref());
        }
    }


    /// Forgets a connection associated with the specified `key`.
    pub fn remove(&mut self, key: ConnectionKey) -> Option<Connection> {
        let mut connection = self.all.remove(key)?;
        self.active.remove(&key);

        let mut scids_empty = true;

        for cid in connection.source_ids() {
            if !cid.is_empty() {
                scids_empty = false;
            }

            self.cid_dictionary.remove(cid.as_ref());
        }
        while let Some(cid) = connection.retired_scid_next() {
            if !cid.is_empty() {
                scids_empty = false;
            }

            self.cid_dictionary.remove(cid.as_ref());
        }

        if scids_empty {
            match connection
                .path_stats()
                .find(|it| {
                    connection
                        .is_path_validated(it.local_addr, it.peer_addr)
                        .ok()
                        .unwrap_or(false)
                })
                .map(|it| it.peer_addr)
            {
                Some(addr) => {
                    self.addr_dictionary.remove(&addr);
                }
                None => {
                    error!(
                        connection_id = %connection.id(key),
                        "bug: unable to clean-up internal dictionaries: \
                        unable to find an active network path for a connection with a zero-length source ID, \
                        potential memory leak"
                    );
                    debug_panic!(
                        "unable to find an active peer_addr for an IP routed connection, \
                        see logs above"
                    );
                }
            }
        }

        if connection.is_server() {
            self.server_connections_count -= 1;

            if !connection.peer_address_verified {
                self.server_unverified_connections_count -= 1;
            }
        }

        Some(connection)
    }

    /// Returns total number of server connections.
    pub fn server_connections_count(&self) -> usize {
        self.server_connections_count
    }

    /// Returns number of unverified server connections.
    ///
    /// Connection is considered unverified when the client has not
    /// proved that he sends packets from his genuine network address.
    pub fn server_unverified_connections_count(&self) -> usize {
        self.server_unverified_connections_count
    }
}


/// QUIC connection and its context.
pub(super) struct Connection {
    /// `quiche` connection.
    inner: quiche::Connection<BytesFactory>,

    /// [WheelTimer][crate::util::WheelTimer]
    /// key for a scheduled pacer event.
    ///
    /// When the event is fired, we're allowed to send a next packet.
    timer_pacer_key: Option<TimerKey>,

    /// [WheelTimer][crate::util::WheelTimer]
    /// key for a scheduled Quiche timeout event.
    timer_quiche_timeout_key: Option<TimerKey>,

    /// [WheelTimer][crate::util::WheelTimer]
    /// key for a scheduled establish timeout event.
    ///
    /// When event fires and connection is not established yet,
    /// it gets removed.
    timer_establish_timeout_key: Option<TimerKey>,

    /// It's unique trace ID: the same as [QuicheConnectionId::trace].
    trace: Uuid,

    /// An outgoing bidirectional stream ID sequence.
    stream_bidi_sequence: StreamId,

    /// An outgoing unidirectional stream ID sequence.
    stream_uni_sequence: StreamId,

    /// `true` if the peer has verified his address against spoofing.
    ///
    /// This value is private and set via [Connections::mark_verified].
    peer_address_verified: bool,

    /// `true` if an application hit the peer's bidirectional stream limit.
    ///
    /// It is used for tracking whether to send a [ConnectionEvent::Available] or not.
    streams_bidi_exhausted: bool,

    /// `true` if an application hit the peer's unidirectional stream limit.
    ///
    /// It is used for tracking whether to send a [ConnectionEvent::Available] or not.
    streams_uni_exhausted: bool,

    /// `true` if an application read all available datagrams, at this moment.
    ///
    /// It is used for tracking whether to send a [DatagramEvent::InAvailable] or not.
    dgram_in_drained: bool,

    /// `true` if an application hit the writing datagram limit, at this moment.
    ///
    /// It is used for tracking whether to send a [DatagramEvent::OutAvailable] or not.
    dgram_out_drained: bool,

    /// Last known maximum size for an outgoing datagram.
    ///
    /// It is used for tracking whether to send a [DatagramEvent::OutSize] or not.
    dgram_out_max_size: u16,

    /// `true` if a `Connection::Closed` or similar event was sent.
    ///
    /// It is used to avoid close events duplications.
    closed_event_sent: bool,
}

impl Connection {
    /// Creates a new connection context, wrapping orignal quiche connection.
    ///
    /// # Arguments:
    ///
    /// - `peer_address_verified`: `true` if the connection is a server connection,
    ///   and peer has verified his address using an `Initial` token.
    pub fn new(
        quiche_connection: quiche::Connection<BytesFactory>,
        peer_address_verified: bool,
    ) -> Self {
        let is_client = !quiche_connection.is_server();

        Self {
            inner: quiche_connection,
            timer_pacer_key: None,
            timer_quiche_timeout_key: None,
            timer_establish_timeout_key: None,
            stream_bidi_sequence: StreamId::initial(is_client, true),
            stream_uni_sequence: StreamId::initial(is_client, false),
            peer_address_verified: is_client || peer_address_verified,
            streams_uni_exhausted: false,
            streams_bidi_exhausted: false,
            dgram_in_drained: false,
            dgram_out_drained: false,
            dgram_out_max_size: 0,
            closed_event_sent: false,
            trace: {
                let mut id = [0u8; 16];
                rng().fill_bytes(id.as_mut());

                uuid::Builder::from_random_bytes(id).into_uuid()
            },
        }
    }


    /// Returns a timer key associated with the specified `kind`.
    ///
    /// The original key is removed afterwards.
    pub fn pop_timer_key(&mut self, kind: TimerKind) -> Option<TimerKey> {
        match kind {
            TimerKind::Pacer => self.timer_pacer_key.take(),
            TimerKind::QuicheTimeout => self.timer_quiche_timeout_key.take(),
            TimerKind::EstablishTimeout => self.timer_establish_timeout_key.take(),
        }
    }

    /// Sets the specified timer `key`, may be `None` to reset.
    pub fn set_timer_key(&mut self, kind: TimerKind, key: Option<TimerKey>) {
        match kind {
            TimerKind::Pacer => self.timer_pacer_key = key,
            TimerKind::QuicheTimeout => self.timer_quiche_timeout_key = key,
            TimerKind::EstablishTimeout => self.timer_establish_timeout_key = key,
        }
    }


    /// Returns an ID of the last locally opened stream on the specified directionality.
    pub fn get_last_local_stream_id(&self, bidirectional: bool) -> Option<StreamId> {
        match bidirectional {
            true => match self.stream_bidi_sequence.previous().is_ok() {
                true => Some(self.stream_bidi_sequence),
                false => None,
            },
            false => match self.stream_uni_sequence.previous().is_ok() {
                true => Some(self.stream_uni_sequence),
                false => None,
            },
        }
    }

    /// Returns the number of streams of the specified directionality
    /// that can be created before the peer’s stream count limit is reached.
    ///
    /// See:
    /// - [quiche::Connection::peer_streams_left_bidi].
    /// - [quiche::Connection::peer_streams_left_uni].
    pub fn peer_streams_left(&mut self, bidirectional: bool) -> u64 {
        if bidirectional {
            self.peer_streams_left_bidi()
        } else {
            self.peer_streams_left_uni()
        }
    }

    /// Allocates a new stream ID for the specified direction, incrementing it locally afterwards.
    pub fn allocate_stream_id(&mut self, bidirectional: bool) -> Result<StreamId, StreamIdError> {
        let sequence = match bidirectional {
            true => &mut self.stream_bidi_sequence,
            false => &mut self.stream_uni_sequence,
        };

        let old = *sequence;
        *sequence = sequence.next()?;

        Ok(old)
    }


    /// Returns a timestamp when a next packet can be released and sent to a peer,
    /// or `None` if it can be done immediately.
    pub fn get_next_release_time(&self, now: Instant) -> Option<Instant> {
        let decision = self.inner.get_next_release_time()?;

        if decision.can_burst() {
            return None;
        }

        let time = decision.time(now)?;
        if time.saturating_duration_since(now) <= Duration::from_micros(50) {
            return None;
        }

        Some(time)
    }


    /// Notifies that an application exhausted the peer's stream limit
    /// on the specified direction.
    pub fn set_streams_exhausted(&mut self, bidirectional: bool) {
        if bidirectional {
            self.streams_bidi_exhausted = true;
        } else {
            self.streams_uni_exhausted = true;
        }
    }

    /// Returns [ConnectionEvent::StreamsAvailable] event iterator with the events inside,
    /// if an application waits for one of them.
    pub fn get_connection_streams_available_events(
        &mut self,
        key: ConnectionKey,
    ) -> impl Iterator<Item = ConnectionEvent<QuicheConnectionId>> {
        let mut events = [None, None];

        if self.streams_bidi_exhausted
            && self.peer_streams_left_bidi() != 0
            && self.stream_bidi_sequence.next().is_ok()
        {
            self.streams_bidi_exhausted = false;
            events[0] = Some(ConnectionEvent::StreamsAvailable {
                connection_id: self.id(key),
                is_bidirectional: true,
            });
        }

        if self.streams_uni_exhausted
            && self.peer_streams_left_uni() != 0
            && self.stream_uni_sequence.next().is_ok()
        {
            events[1] = Some(ConnectionEvent::StreamsAvailable {
                connection_id: self.id(key),
                is_bidirectional: false,
            });
        }

        events.into_iter().flatten()
    }


    /// Notifies that application read all incoming datagrams,
    /// and waits for a next [DatagramEvent::InAvailable] event.
    pub fn set_dgram_in_drained(&mut self) {
        self.dgram_in_drained = true;
    }

    /// Notifies that application hit the outgoint datagram limit (internal buffers are full),
    /// and waits for a next [DatagramEvent::OutAvailable] event.
    pub fn set_dgram_out_drained(&mut self) {
        self.dgram_out_drained = true;
    }

    /// Returns an [DatagramEvent::InAvailable] event if an application waits for it.
    pub fn get_dgram_in_available_event(
        &mut self,
        key: ConnectionKey,
    ) -> Option<DatagramEvent<QuicheConnectionId>> {
        if !self.dgram_in_drained {
            return None;
        }

        self.dgram_in_drained = false;
        Some(DatagramEvent::InAvailable(self.id(key)))
    }

    /// Returns an [DatagramEvent::OutAvailable] event if an application waits for it.
    pub fn get_dgram_out_available_event(
        &mut self,
        key: ConnectionKey,
    ) -> Option<DatagramEvent<QuicheConnectionId>> {
        if !self.dgram_out_drained {
            return None;
        }

        self.dgram_out_drained = false;
        Some(DatagramEvent::OutAvailable(self.id(key)))
    }

    /// Returns an [DatagramEvent::OutSize] event if the maximum outgoing datagram size has changed.
    pub fn get_dgram_out_size_event(
        &mut self,
        key: ConnectionKey,
    ) -> Option<DatagramEvent<QuicheConnectionId>> {
        let max_size = usize::min(
            self.dgram_max_writable_len().unwrap_or(0),
            u16::MAX as usize,
        ) as u16;

        if self.dgram_out_max_size == max_size {
            return None;
        }

        self.dgram_out_max_size = max_size;
        Some(DatagramEvent::OutNewSize(self.id(key), max_size))
    }


    /// Returns a [ConnectionEvent::Closed] or [ConnectionEvent::Halted] event,
    /// if it wasn't created previously.
    pub fn get_close_event(
        &mut self,
        key: ConnectionKey,
    ) -> Option<ConnectionEvent<QuicheConnectionId>> {
        if self.closed_event_sent {
            return None;
        }

        self.closed_event_sent = true;

        if self.is_timed_out() {
            return Some(ConnectionEvent::Halted {
                connection_id: self.id(key),
                kind: HaltKind::Timeout,
            });
        }

        if let Some(error) = self.local_error() {
            return Some(ConnectionEvent::Closed {
                connection_id: self.id(key),
                code: error.error_code,
                reason: Bytes::copy_from_slice(&error.reason),
                is_application: error.is_app,
                is_local: true,
            });
        }

        if let Some(error) = self.peer_error() {
            return Some(ConnectionEvent::Closed {
                connection_id: self.id(key),
                code: error.error_code,
                reason: Bytes::copy_from_slice(&error.reason),
                is_application: error.is_app,
                is_local: false,
            });
        }

        Some(ConnectionEvent::Halted {
            connection_id: self.id(key),
            kind: HaltKind::StatelessReset,
        })
    }

    /// Returns a [ConnectionEvent::Halted] event,
    /// if a close event wasn't created previously.
    pub fn get_halt_event(
        &mut self,
        key: ConnectionKey,
        kind: HaltKind,
    ) -> Option<ConnectionEvent<QuicheConnectionId>> {
        if self.closed_event_sent {
            return None;
        }

        self.closed_event_sent = true;
        Some(ConnectionEvent::Halted {
            connection_id: self.id(key),
            kind,
        })
    }


    /// Recreates a [QuicheConnectionId] for the specified `key`.
    pub fn id(&self, key: ConnectionKey) -> QuicheConnectionId {
        QuicheConnectionId::new(key, self.trace)
    }
}

impl Deref for Connection {
    type Target = quiche::Connection<BytesFactory>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Connection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
