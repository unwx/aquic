mod exec;
pub mod incoming;
pub mod outgoing;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum DirectionError<E> {
    Finish,
    Internal,
    StopSending(E),
    ResetStream(E),
    StreamMapper(E),
}
