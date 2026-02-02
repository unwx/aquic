use crate::{
    backend::{
        quinn::{QuinnConnection, QuinnConnectionId},
        util::TimerKey,
    },
    stream::StreamId,
};
use indexmap::IndexSet;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use std::ops::{Deref, DerefMut};

pub(super) struct Connections {
    /// All active connections.
    pub all: FxHashMap<QuinnConnectionId, Connection>,

    /// Connections that were modified in some way and need to be polled.
    pub modified: IndexSet<QuinnConnectionId, FxBuildHasher>,
}

impl Connections {
    pub fn new() -> Self {
        Self {
            all: FxHashMap::with_hasher(FxBuildHasher),
            modified: IndexSet::with_hasher(FxBuildHasher),
        }
    }
}


/// Internal representation of the QUIC connection,
/// and its context.
pub(super) struct Connection {
    /// `quinn-proto` connection.
    pub inner: QuinnConnection,

    /// [TimerWheel][crate::backend::util::TimerWheel] key for a scheduled [TimeoutEvent].
    pub timeout_key: Option<TimerKey>,

    /// Streams that may receive byte-chunks immediately,
    /// not in sequential order.
    pub unordered_streams: FxHashSet<StreamId>,
}

impl Deref for Connection {
    type Target = QuinnConnection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Connection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
