use quinn_proto::{EndpointConfig, TransportConfig};
use tracing::warn;

/// `quinn-proto` backend configuration.
#[derive(Debug)]
pub struct Config {
    /// Shared endpoint configuration that applies for both: server and client connections.
    pub endpoint: EndpointConfig,

    /// Client configuration, or `None` to reject all local connection attempts.
    pub client: Option<ClientConfig>,

    /// Server configuration, or `None` to reject all remote connection attempts.
    pub server: Option<ServerConfig>,

    /// Number of primary buffers that are used to send data from QUIC backend to network.
    ///
    /// Multiple buffers are going to be used in `sendmmsg`-like syscalls.
    /// Number of buffers are going to be scaled down to `1` automatically
    /// if a socket doesn't support `sendmmsg`-like feature.
    ///
    /// Must be > 0.
    /// Default is `3`.
    pub out_buffers_count: usize,

    /// Number of reserved small buffers (1500 bytes long each) that are only used for the first packets
    /// like `Initial`, `Retry`, `Version Negotiation`, even when the primary `out_buffers` are busy.
    ///
    /// May be `zero`, and if a current socket doesn't support `sendmmsg` this will automatically be clamped to `zero`.
    ///
    /// Default is `3`.
    pub reserved_out_buffers_count: usize,

    /// Size of a single primary `out_buffer`.
    ///
    /// Must be >= `1500`.
    /// Default is `1500`.
    pub out_buffer_size: usize,

    /// Size of a single primary `out_buffer`, if a socket supports GSO.
    ///
    /// Must be >= `1500`.
    /// Default is `8192`.
    pub out_buffer_gso_size: usize,
}

/// Locally-initiated connections configuration.
#[derive(Debug)]
pub struct ClientConfig {
    /// `quinn-proto` client configuration.
    pub config: quinn_proto::ClientConfig,

    /// Transport configuration.
    ///
    /// - If `Some`, it will replace [ClientConfig::transport_config] with some overriden values, depending on a socket features.
    /// - If `None`, the default [TransportConfig] will replace [ClientConfig::transport_config] with some overriden values.
    ///
    /// Default is `None`.
    pub transport: Option<TransportConfig>,
}

/// Remote-initiated connections configuration.
#[derive(Debug)]
pub struct ServerConfig {
    /// `quinn-proto` server configuration.
    pub config: quinn_proto::ServerConfig,

    /// Transport configuration.
    ///
    /// - If `Some`, it will replace [ServerConfig::transport_config] with some overriden values, depending on a socket features.
    /// - If `None`, the default [TransportConfig] will replace [ServerConfig::transport_config] with some overriden values.
    ///
    /// Default is `None`.
    pub transport: Option<TransportConfig>,
}


impl Config {
    /// Adjusts config values if they are invalid.
    pub(super) fn fix(&mut self) {
        if self.server.is_none() && self.client.is_none() {
            warn!(
                "neither `server` nor `client` configurations were provided: Quinn backend is no-op"
            );
        }

        if self.out_buffers_count == 0 {
            warn!("specified `config.out_buffers_count` is '0', this value is replaced with '1'");
            self.out_buffers_count = 1;
        }
        if self.out_buffer_size < 1500 {
            warn!(
                "specified `config.out_buffer_size` is less than '1500', this value is replaced with '1500'"
            );
            self.out_buffer_size = 1500;
        }
        if self.out_buffer_gso_size < self.out_buffer_size {
            self.out_buffer_gso_size = self.out_buffer_size;
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: EndpointConfig::default(),
            server: None,
            client: None,
            out_buffers_count: 3,
            reserved_out_buffers_count: 3,
            out_buffer_size: 1500,
            out_buffer_gso_size: 8192,
        }
    }
}


impl ClientConfig {
    /// Creates a default `ClientConfig` with the provided `quinn-proto` client configuration.
    pub fn new(config: quinn_proto::ClientConfig) -> Self {
        Self {
            config,
            transport: None,
        }
    }
}

impl ServerConfig {
    /// Creates a default `ServerConfig` with the provided `quinn-proto` server configuration.
    pub fn new(config: quinn_proto::ServerConfig) -> Self {
        Self {
            config,
            transport: None,
        }
    }
}
