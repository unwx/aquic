use crate::net::ServerName;
use mini_moka::unsync::Cache;
use std::time::Duration;

pub(super) struct SessionCache {
    sessions: Cache<ServerName, QuicSession, ahash::RandomState>,
}

impl SessionCache {
    pub fn new(
        max_capacity: usize,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        let mut builder = Cache::builder().max_capacity(max_capacity as u64);

        if let Some(ttl) = time_to_live {
            builder = builder.time_to_live(ttl);
        }
        if let Some(tti) = time_to_idle {
            builder = builder.time_to_idle(tti);
        }

        Self {
            sessions: builder.build_with_hasher(ahash::RandomState::default()),
        }
    }

    /// Inserts a new session into the cache.
    pub fn insert(&mut self, server_name: ServerName, session: QuicSession) {
        self.sessions.insert(server_name, session);
    }

    /// Returns a session associated with the given `server_name`, if it exists.
    pub fn get(&mut self, server_name: &ServerName) -> Option<&QuicSession> {
        self.sessions.get(server_name)
    }
}


#[derive(Debug, Clone)]
pub(super) struct QuicSession {
    pub tls_ticket: Box<[u8]>,
    // TODO(feat): at this moment it is impossible to retrieve Identity (NEW_TOKEN) issued by a peer.
    // Add a `identity_token` when it gets implemented in quiche: https://github.com/cloudflare/quiche/issues/2395
}

impl QuicSession {
    pub fn new(tls_ticket: Box<[u8]>) -> Self {
        Self { tls_ticket }
    }
}
