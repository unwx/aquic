use crate::backend::{
    ConnectionIdGenerator, PanicTokenGenerator, TokenGenerator,
    firewall::{Firewall, StrictFirewall},
};
use std::{
    any::{Any, TypeId},
    time::Duration,
};
use tracing::warn;

/// `quiche` backend configuration.
pub struct Config<CIdG: ConnectionIdGenerator, SCfg: ServerTypes = ()> {
    /// Client configuration, or `None` to reject all local connection attempts.
    pub client: Option<ClientConfig>,

    /// Server configuration, or `None` to reject all remote connection attempts.
    pub server: Option<ServerConfig<SCfg>>,

    /// Connection ID generator to use.
    ///
    /// Default is [RandomConnectionIdGenerator] with length of `16` bytes.
    pub connection_id_generator: CIdG,

    /// A fixed size of a single contiguous buffer that is used to send data from QUIC backend to network.
    ///
    /// Note: this buffer is shared for all connections.
    ///
    /// Must be >= `2048`.
    pub out_buffer_size: usize,
}

/// Remote-initiated connections configuration.
pub struct ServerConfig<T: ServerTypes> {
    /// `quiche` server configuration.
    pub quiche: quiche::Config,

    /// Firewall to use for incoming connections.
    ///
    /// See [StrictFirewall] for a simple one.
    pub firewall: T::Firewall,

    /// Token generator to use.
    ///
    /// See [PaddedTokenGenerator](crate::backend::PaddedTokenGenerator).
    pub token_generator: T::TokenGenerator,

    /// Maximum duration to accept for a connection to fully establish, before timing out the connection.
    ///
    /// Default is `15` seconds.
    ///
    /// # Security Warning
    ///
    /// A lifetime of a `Retry` token depends on this timeout,
    /// so it should be set to a reasonable value to prevent replay attacks.
    ///
    /// See [TokenGenerator::generate_retry_token].
    pub establish_timeout: Duration,
}

/// Locally-initiated connections configuration.
pub struct ClientConfig {
    /// `quiche` client configuration.
    pub quiche: quiche::Config,

    /// Maximum duration to accept for a connection to fully establish, before timing out the connection.
    ///
    /// Default is `30` seconds.
    pub establish_timeout: Option<Duration>,
}


/// Additional modules for a server configuration.
pub trait ServerTypes {
    type Firewall: Firewall + 'static;
    type TokenGenerator: TokenGenerator + 'static;
}

impl ServerTypes for () {
    type Firewall = StrictFirewall;

    type TokenGenerator = PanicTokenGenerator;
}


impl<CIdG: ConnectionIdGenerator, SCfg: ServerTypes> Config<CIdG, SCfg> {
    /// Adjusts config values if they are invalid.
    pub(super) fn fix(&mut self) {
        if self.server.is_none() && self.client.is_none() {
            warn!(
                "neither `server` nor `client` configurations were provided: Quiche backend is no-op"
            );
        }

        if self.out_buffer_size < 2048 {
            warn!(
                "specified `config.out_buffer_size` is less than '2048', this value is replaced with '2048'"
            );
            self.out_buffer_size = 2048;
        }

        if let Some(server) = self.server.as_ref() {
            if server.firewall.type_id() == TypeId::of::<PanicTokenGenerator>() {
                panic!(
                    "please specify the 'ServerTypes' inside 'Config<T: ServerTypes>' when enabling Quiche server configuration"
                );
            }
        }
    }
}

impl ClientConfig {
    /// Creates a default `ClientConfig` with the provided `quiche` client configuration.
    pub fn new(config: quiche::Config) -> Self {
        Self {
            quiche: config,
            establish_timeout: Some(Duration::from_secs(30)),
        }
    }
}

impl<T: ServerTypes> ServerConfig<T> {
    /// Creates a default `ServerConfig` with the provided `quiche` server configuration.
    pub fn new(
        config: quiche::Config,
        firewall: T::Firewall,
        token_generator: T::TokenGenerator,
    ) -> Self {
        Self {
            quiche: config,
            firewall,
            token_generator,
            establish_timeout: Duration::from_secs(15),
        }
    }
}
