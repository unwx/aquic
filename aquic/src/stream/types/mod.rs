use aquic_macros::doc_support;

mod error;
mod id;
mod payload;

pub use error::*;
pub use id::*;
pub use payload::*;


/// A stream's priority that determines the order in which stream data is sent on the wire.
///
/// **Note** different [`QuicBackend`][crate::backend::QuicBackend] implement prioritization differently,
/// or don't implement at all.
#[doc_support]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Priority {
    /// Urgency: higher values indicate higher priority.
    ///
    /// The default stream priority is `0`.
    pub urgency: i8,

    /// Controls how bandwidth is shared among streams with the same urgency.
    ///
    /// - `false`: **Sequential**. The scheduler attempts to send this stream's data
    ///   back-to-back until completion before moving to the next stream.
    ///   Use this for data that requires the whole payload to be useful (e.g., scripts, CSS).
    ///
    /// - `true`: **Incremental**. The scheduler interleaves data from this stream
    ///   with other incremental streams of the same urgency.
    ///   Use this for data that can be processed as it arrives (e.g., progressive images, video).
    ///
    #[doc_support(quiche)]
    pub incremental: bool,
}
