use std::time::Duration;

use quinn_proto::{ClientConfig, EndpointConfig, IdleTimeout, ServerConfig, TransportConfig};
use tracing::warn;

/// `quinn-proto` provider configuration.
pub struct Config {
    /// Configuration for both: server and client connections.
    pub endpoint: EndpointConfig,

    /// Server configuration, or `None` to reject all server connection attempts.
    pub server: Option<ServerConfig>,

    /// Client configuration, or `None` to reject all client connection attempts.
    pub client: Option<ClientConfig>,

    /// Server transport configuration.
    ///
    /// - If `Some`, it will replace [ServerConfig::transport_config] with some overriden values.
    /// - If `None`, the default [TransportConfig] will replace [ServerConfig::transport_config] with some overriden values.
    pub server_transport: Option<TransportConfig>,

    /// Client transport configuration.
    ///
    /// - If `Some`, it will replace [ClientConfig::transport_config] with some overriden values.
    /// - If `None`, the default [TransportConfig] will replace [ClientConfig::transport_config] with some overriden values.
    pub client_transport: Option<TransportConfig>,

    /// Number of internal buffers that are used for output operations
    /// (to send data from QUIC backend to network).
    ///
    /// Multiple buffers are going to be used in `sendmmsg`-like syscalls.
    ///
    /// **Notes**:
    /// - It's recommended to not allocate too many,
    ///   as each buffer consumes [`MAX_PACKET_SIZE`](crate::net::MAX_PACKET_SIZE) bytes,
    ///   and it might be not efficient.
    ///   (Unfortunatelly, for `quinn-proto` I don't see ways to make it more efficient at this moment, like moving to a ring buffer).
    /// - If a current socket doesn't support `sendmmsg` this will automatically be clamped to `1`.
    pub out_buffers_count: usize,

    /// Number of reserved buffers that are used when
    /// primary buffers are yet in use, for example:
    /// - [`Socket`][crate::net::Socket] uses primary buffers, but partial write happens.
    /// - I/O loop waits a signal from socket, when it become possible to write again.
    /// - In parallel, we received more packets from peer and want to process them.
    /// - During processing, `quinn-proto` immediately writes packets to be sent to peer, but we need a buffer to write them somewhere!
    /// - If `reserved_out_buf_count > 0`, we write them in reserved buffers.
    ///   Otherwise, we wait for all outgoing packets to be sent, to make primary buffers available again.
    ///
    /// May be `zero`, and if a current socket doesn't support `sendmmsg` this will automatically be clamped to `zero`.
    pub reserved_out_buffers_count: usize,

    /// Maximum duration of inactivity to accept before timing out the connection.
    pub max_idle_timeout: Duration,

    /// Maximum acceptable duration of a timeout event to be late.
    ///
    /// Example:
    /// - `max_idle_timeout` is 30s.
    /// - `max_idle_timeout_miss` is 10ms.
    ///
    /// Connection will be timed out in 30s + up to 10ms.
    pub max_timeout_miss: Duration,
}

impl Config {
    /// Adjusts config values in case they are invalid,
    /// and prints a warning in such cases.
    ///
    /// Returns `false` if there was an invalid value.
    pub fn validate(&mut self) -> bool {
        let mut invalid = true;

        if self.server.is_none() && self.client.is_none() {
            warn!(
                "both `server` and `client` configurations were not provided: \
                QuinnProvider is no-op"
            );
            invalid = true;
        }

        if self.out_buffers_count == 0 {
            warn!("specified `config.out_buffers_count` is '0', this value is replaced with '1'");
            self.out_buffers_count = 1;
            invalid = true;
        }

        if self.max_idle_timeout.as_millis() == 0 {
            warn!(
                "`config.max_idle_timeout` is '0ms', \
                dead connections may never be removed from memory"
            );

            // Make sure it's really zero.
            self.max_idle_timeout = Duration::ZERO;
            invalid = true;
        }
        if IdleTimeout::try_from(self.max_idle_timeout).is_err() {
            warn!(
                "specified `config.max_idle_timeout` exceeds quinn_proto::IdleTimeout maximum, \
                this value is replaced with '0ms', leading to non-expiring connections"
            );

            self.max_idle_timeout = Duration::ZERO;
            invalid = true;
        }

        if self.max_timeout_miss.as_millis() == 0 {
            warn!(
                "specified `config.max_timeout_miss.as_millis()` is '0ms', which is invalid value:
                this value is replaced with '15ms'"
            );

            self.max_timeout_miss = Duration::from_millis(15);
            invalid = true;
        }
        if self.max_timeout_miss.as_millis() < 5 {
            warn!(
                "specified `config.max_timeout_miss.as_millis()` is less than '5ms': \
                it may lead to higher memory/cpu consumption"
            );

            // It's valid in some cases, but may be not in most.
        }

        invalid
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: EndpointConfig::default(),
            server: None,
            client: None,
            server_transport: None,
            client_transport: None,
            out_buffers_count: 1,
            reserved_out_buffers_count: 0,
            max_idle_timeout: Duration::from_secs(30),
            max_timeout_miss: Duration::from_millis(10),
        }
    }
}
