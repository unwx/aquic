use crate::{
    backend::{ConnectionEvent, DatagramEvent, HaltKind, StableConnectionId},
    util::TimerKey,
};
use bytes::Bytes;
use hashlink::LinkedHashSet;
use rand::{Rng, rng};
use std::{
    fmt::{self, Display, Formatter},
    hash::Hash,
    hash::Hasher,
    ops::{Deref, DerefMut},
};
use uuid::Uuid;


pub(super) type ConnectionKey = quinn_proto::ConnectionHandle;

/// A Quinn stable connection ID.
#[derive(Debug, Copy, Clone)]
pub struct QuinnConnectionId {
    /// A `quinn-proto` generated key.
    pub(super) key: ConnectionKey,

    /// A unique trace for logging and debugging.
    pub(super) trace: Uuid,

    /// `key` version to avoid ABA bugs.
    version: u32,
}

impl QuinnConnectionId {
    pub(super) fn new(key: ConnectionKey, trace: Uuid, version: u32) -> Self {
        Self {
            key,
            trace,
            version,
        }
    }
}

impl PartialEq for QuinnConnectionId {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.version == other.version
    }
}

impl Eq for QuinnConnectionId {}

impl Hash for QuinnConnectionId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
        self.version.hash(state);
    }
}

impl Display for QuinnConnectionId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.trace)
    }
}

impl StableConnectionId for QuinnConnectionId {}


pub(super) struct Connections {
    /// All active connections.
    ///
    /// This is a map, where key is a `QuinnConnectionId.key.0`,
    /// and value is a connection context.
    ///
    /// **Note**: To avoid ABA bugs, compare `QuinnConnectionId.version` with a `Connection.version`.
    all: ConnectionsMap,

    /// Modified `all` connections that wait to be polled.
    ///
    /// LinkedHashSet is used for effective, fair (FIFO) polling.
    active: LinkedHashSet<ConnectionKey, foldhash::fast::RandomState>,

    /// Version sequence for `QuinnConnectionId.version`.
    version_sequence: u32,
}

impl Connections {
    pub fn new() -> Self {
        Self {
            all: ConnectionsMap::new(),
            active: LinkedHashSet::with_hasher(foldhash::fast::RandomState::default()),
            version_sequence: 0,
        }
    }

    /// Inserts a new connection and marks it as active.
    ///
    /// **Warning**: This is a low-level method, that doesn't check for ABA bugs.
    #[rustfmt::skip]
    pub fn insert_raw(
        &mut self,
        connection_key: ConnectionKey,
        connection: quinn_proto::Connection,
    ) -> QuinnConnectionId {
        let trace = {
            let mut id = [0u8; 16];
            rng().fill_bytes(id.as_mut());

            uuid::Builder::from_random_bytes(id).into_uuid()
        };
        let version = self.version_sequence;
        self.version_sequence = self.version_sequence.wrapping_add(1);

        self.remove_raw(connection_key);
        self.all.insert(connection_key, Connection::new(connection, trace, version));
        self.active.replace(connection_key);

        QuinnConnectionId {
            key: connection_key,
            trace,
            version,
        }
    }


    /// Returns an immutable connection.
    pub fn get(&self, id: QuinnConnectionId) -> Option<&Connection> {
        self.all.get(id.key).filter(|it| it.version == id.version)
    }

    /// Returns a mutable connection.
    pub fn get_mut(&mut self, id: QuinnConnectionId) -> Option<&mut Connection> {
        self.all
            .get_mut(id.key)
            .filter(|it| it.version == id.version)
    }

    /// Returns a mutable connection and marks it as active, to be polled later.
    ///
    /// If connection is already marked as active, it's no-op.
    /// Otherwise, it will be put in the back of the awaiting queue.
    pub fn get_mark_active(&mut self, id: QuinnConnectionId) -> Option<&mut Connection> {
        self.get_mark_active_if(id, |_| true)
    }

    /// Returns a mutable connection and marks it as active, to be polled later.
    ///
    /// If connection is already marked as active, it's no-op.
    /// Otherwise, it will be put in the back of the awaiting queue.
    ///
    /// **Warning**: This is a low-level method, that doesn't check for ABA bugs.
    pub fn get_mark_active_raw(&mut self, key: ConnectionKey) -> Option<&mut Connection> {
        self.all.get_mut(key).inspect(|_| {
            self.active.replace(key);
        })
    }

    /// Returns a mutable connection if it satisfies the specified `filter`, and marks it as active.
    ///
    /// If connection is already marked as active, it's no-op.
    /// Otherwise, it will be put in the back of the awaiting queue.
    pub fn get_mark_active_if<F>(
        &mut self,
        id: QuinnConnectionId,
        filter: F,
    ) -> Option<&mut Connection>
    where
        F: FnOnce(&Connection) -> bool,
    {
        self.all
            .get_mut(id.key)
            .filter(|it| it.version == id.version)
            .filter(|it| filter(it))
            .inspect(|_| {
                self.active.replace(id.key);
            })
    }

    /// Returns a mutable iterator for all present connections,
    /// marking each active.
    pub fn all_iter_mark_active(
        &mut self,
    ) -> impl Iterator<Item = (QuinnConnectionId, &mut Connection)> + '_ {
        self.all
            .iter_mut()
            .inspect(|(key, _)| {
                self.active.replace(*key);
            })
            .map(|(key, connection)| {
                let id = connection.id(key);
                (id, connection)
            })
    }


    /// Returns an active connection.
    ///
    /// This method will return the same connection, until [`next_active`](Self::next_active) is called.
    ///
    /// Connections are iterated in fair, FIFO order.
    pub fn peek_active(&mut self) -> Option<(QuinnConnectionId, &mut Connection)> {
        loop {
            let connection_key = self.active.front().copied()?;

            // Should be always `true`.
            if self.all.contains_key(connection_key) {
                let connection = self.all.get_mut(connection_key).unwrap();
                return Some((connection.id(connection_key), connection));
            }

            self.next_active();
        }
    }

    /// Removes an `active` mark from the last [`peeked`](Self::peek_active) connection
    /// by removing it from the queue, and moves the cursor to the next one.
    pub fn next_active(&mut self) {
        self.active.pop_front();
    }


    /// Forgets a connection associated with the specified `id`.
    pub fn remove(&mut self, id: QuinnConnectionId) -> Option<Connection> {
        self.all.get(id.key).filter(|it| it.version == id.version)?;
        self.remove_raw(id.key)
    }

    /// Forgets a connection associated with the specified `key`.
    ///
    /// **Warning**: This is a low-level method, that doesn't check for ABA bugs.
    pub fn remove_raw(&mut self, connection_key: ConnectionKey) -> Option<Connection> {
        let connection = self.all.remove(connection_key)?;
        self.active.remove(&connection_key);
        Some(connection)
    }
}


/// QUIC connection and its context.
pub(super) struct Connection {
    /// `quinn-proto` connection.
    inner: quinn_proto::Connection,

    /// [WheelTimer][crate::util::WheelTimer]
    /// key for a scheduled Quinn timeout event.
    timer_quinn_timeout_key: Option<TimerKey>,

    /// It's unique trace ID: the same as [QuicheConnectionId::trace].
    trace: Uuid,

    /// Connection unique version for ABA bug avoidance.
    version: u32,

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
    /// Creates a new connection context, wrapping orignal quinn connection.
    pub fn new(quinn_connection: quinn_proto::Connection, trace: Uuid, version: u32) -> Self {
        Self {
            inner: quinn_connection,
            timer_quinn_timeout_key: None,
            dgram_out_max_size: 0,
            closed_event_sent: false,
            trace,
            version,
        }
    }


    /// Returns a Quinn timer key.
    ///
    /// The original key is removed afterwards.
    pub fn pop_timer_key(&mut self) -> Option<TimerKey> {
        self.timer_quinn_timeout_key.take()
    }

    /// Sets the specified timer `key`, may be `None` to reset.
    pub fn set_timer_key(&mut self, key: Option<TimerKey>) {
        self.timer_quinn_timeout_key = key;
    }


    /// Returns an [DatagramEvent::OutSize] event if the maximum outgoing datagram size has changed.
    pub fn get_dgram_out_size_event(
        &mut self,
        key: ConnectionKey,
    ) -> Option<DatagramEvent<QuinnConnectionId>> {
        let max_size =
            usize::min(self.datagrams().max_size().unwrap_or(0), u16::MAX as usize) as u16;

        if self.dgram_out_max_size == max_size {
            return None;
        }

        self.dgram_out_max_size = max_size;
        Some(DatagramEvent::OutNewSize(self.id(key), max_size))
    }


    /// Returns a [ConnectionEvent::Closed] event,
    /// if a close event wasn't created previously.
    pub fn get_close_event(
        &mut self,
        key: ConnectionKey,
        code: u64,
        reason: Bytes,
        is_application: bool,
        is_local: bool,
    ) -> Option<ConnectionEvent<QuinnConnectionId>> {
        if self.closed_event_sent {
            return None;
        }

        self.closed_event_sent = true;
        Some(ConnectionEvent::Closed {
            connection_id: self.id(key),
            code,
            reason,
            is_application,
            is_local,
        })
    }

    /// Returns a [ConnectionEvent::Halted] event,
    /// if a close event wasn't created previously.
    pub fn get_halt_event(
        &mut self,
        key: ConnectionKey,
        kind: HaltKind,
    ) -> Option<ConnectionEvent<QuinnConnectionId>> {
        if self.closed_event_sent {
            return None;
        }

        self.closed_event_sent = true;
        Some(ConnectionEvent::Halted {
            connection_id: self.id(key),
            kind,
        })
    }


    /// Recreates a [QuinnConnectionId] for the specified `key`.
    pub fn id(&self, key: ConnectionKey) -> QuinnConnectionId {
        QuinnConnectionId::new(key, self.trace, self.version)
    }
}

impl Deref for Connection {
    type Target = quinn_proto::Connection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Connection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}


struct ConnectionsMap(Vec<Option<Connection>>);

impl ConnectionsMap {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn insert(&mut self, key: ConnectionKey, connection: Connection) {
        if key.0 >= self.0.len() {
            self.0.resize_with(key.0 + 1, || None);
        }

        self.0[key.0] = Some(connection);
    }

    pub fn get(&self, key: ConnectionKey) -> Option<&Connection> {
        self.0.get(key.0).and_then(Option::as_ref)
    }

    pub fn get_mut(&mut self, key: ConnectionKey) -> Option<&mut Connection> {
        self.0.get_mut(key.0).and_then(Option::as_mut)
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (ConnectionKey, &mut Connection)> {
        self.0
            .iter_mut()
            .enumerate()
            .filter_map(|(idx, connection)| {
                connection
                    .as_mut()
                    .map(|conn| (quinn_proto::ConnectionHandle(idx), conn))
            })
    }

    pub fn contains_key(&self, key: ConnectionKey) -> bool {
        self.0.get(key.0).and_then(Option::as_ref).is_some()
    }

    pub fn remove(&mut self, key: ConnectionKey) -> Option<Connection> {
        if key.0 >= self.0.len() {
            return None;
        }

        self.0[key.0].take()
    }
}
