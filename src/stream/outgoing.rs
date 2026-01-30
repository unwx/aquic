use crate::Spec;
use crate::backend::stream::{OutStreamBackend, StreamError};
use crate::exec::{Runtime, SendOnMt};
use crate::log;
use crate::stream::{Encoder, Error, Payload};
use crate::sync::oneshot::{OneshotReceiver, OneshotSender};
use crate::sync::stream;
use crate::sync::{SmartRc, oneshot};
use crate::tracing::StreamSpan;
use bytes::Bytes;
use futures::{FutureExt, select_biased};
use smallvec::SmallVec;
use tracing::{Instrument, Level, Span};

/// An outgoing direction of QUIC stream,
/// acts like a bridge between protocol backend and application listener.
///
/// Its role is to receive incoming messages from app via channel,
/// encode them, and send it to stream.
///
/// The entire flow is sequential: implementation won't write to the stream,
/// unless the network peer is ready to receive the data.
pub(crate) struct Outgoing<S, CId>
where
    S: Spec,
    CId: SendOnMt + Unpin + 'static,
{
    /// An API to communicate with QUIC implementation.
    backend: OutStreamBackend<CId>,

    /// Encoder for encoding messages into bytes.
    encoder: S::Encoder,

    /// Channel for receiving messages from app.
    item_receiver: stream::Receiver<S>,

    /// Channel for sending a cancellation error: stops the I/O loop.
    cancel_sender: oneshot::Sender<Error<S>>,

    /// Channel for receiving a cancellation error.
    cancel_receiver: oneshot::Receiver<Error<S>>,

    /// `true`, if 'FIN' was received from the `item_receiver` channel.
    draining: bool,
}

impl<S, CId> Outgoing<S, CId>
where
    S: Spec,
    CId: Clone + SendOnMt + Unpin + 'static,
{
    pub fn new(
        spec: SmartRc<S>,
        backend: OutStreamBackend<CId>,
        item_receiver: stream::Receiver<S>,
    ) -> Self {
        let encoder = spec.new_encoder();
        let (cancel_sender, cancel_receiver) = oneshot::channel();

        Self {
            backend,
            encoder,
            item_receiver,
            cancel_sender,
            cancel_receiver,
            draining: false,
        }
    }

    /// Starts the I/O loop in a separate task: sends data to the channel
    /// until 'FIN' is sent, or error is received.
    ///
    /// Returns a cancellation sender.
    ///
    /// **Note**: only few variants of [Error] are allowed to be sent via this sender:
    /// - [Error::StopSending],
    /// - [Error::Connection].
    pub fn write(mut self, span: StreamSpan) -> oneshot::Sender<Error<S>> {
        let cancel_sender = self.cancel_sender.clone();

        self.spawn_listen_for_app_error();
        self.spawn_io_loop(span);

        cancel_sender
    }


    /// Spawn a [Self::io_loop] with an additional listener for cancel signals.
    fn spawn_io_loop(mut self, span: StreamSpan) {
        let cancel_receiver = self.cancel_receiver.clone();

        let future = async move {
            select_biased! {
                e = cancel_receiver.recv().fuse() => {
                    self.close(e.unwrap_or_else(
                        || Error::HangUp("Outgoing.cancel_receiver is unavailable".into())
                    ));
                }
                e = self.io_loop().fuse() => {
                    self.close(e.map(|_| Error::Finish).unwrap_or_else(|e| e));
                }
            }
        };

        Runtime::spawn_void(async move {
            future.instrument(span.into()).await;
        })
    }

    /// Application (the one who sends messages to the stream)
    /// might send termination signals to us: [Error::ResetSending] or [Error::HangUp].
    ///
    /// Spawn a separate listener to be aware of these signals.
    fn spawn_listen_for_app_error(&mut self) {
        let cancel_sender = self.cancel_sender.clone();
        let cancel_receiver = self.cancel_receiver.clone();
        let app_error_receiver = self.item_receiver.error_receiver();

        Runtime::spawn_void(async move {
            select_biased! {
                _ = cancel_receiver.recv().fuse() => {
                    // Do nothing.
                },
                app_err = app_error_receiver.recv().fuse() => {
                    let app_err = app_err.unwrap_or_else(
                        || Error::HangUp("Outgoing.item_receiver.error_receiver is unavailable".into())
                    );

                    match &app_err {
                        Error::ResetSending(_) |
                        Error::HangUp(_) => {
                            let _ = cancel_sender.send(app_err);
                        }

                        Error::Finish |
                        Error::StopSending(_) |
                        Error::Decoder(_) |
                        Error::Encoder(_) |
                        Error::Connection => {
                            // Do nothing.
                        }
                    }
                },
            }
        });
    }


    /// Runs the primary I/O loop.
    async fn io_loop(&mut self) -> Result<(), Error<S>> {
        loop {
            self.recv_item().await?;

            if self.draining {
                let mut current = self.encode_next().await?;

                if current.is_empty() {
                    self.write_to_stream(SmallVec::new(), true).await?;
                }

                while !current.is_empty() {
                    let fin = self.encoder.is_fin();
                    self.write_to_stream(current, fin).await?;

                    current = if fin {
                        SmallVec::new()
                    } else {
                        self.encode_next().await?
                    }
                }

                return Ok(());
            } else {
                while let Some(bytes) = {
                    let result = Some(self.encode_next().await?);
                    result.filter(|it| !it.is_empty())
                } {
                    self.write_to_stream(bytes, false).await?;
                }
            }
        }
    }

    /// Receives item from the app channel, and move it into [Encoder].
    ///
    /// Does nothing if we've received 'FIN' before.
    async fn recv_item(&mut self) -> Result<(), Error<S>> {
        if self.draining {
            return Ok(());
        }

        let payload = match self.item_receiver.recv().await {
            Ok(it) => it,
            Err(e) => match &e {
                Error::Finish => {
                    if cfg!(debug_assertions) {
                        panic!("bug: received Error::Finish, but self.draining is false")
                    }

                    Payload::Done
                }
                Error::ResetSending(_) | Error::HangUp(_) => {
                    return Err(e);
                }

                Error::StopSending(_)
                | Error::Decoder(_)
                | Error::Encoder(_)
                | Error::Connection => {
                    panic!("bug: received unexpected error from stream::Receiver: {e}");
                }
            },
        };

        if payload.is_fin() {
            self.draining = true;
        }

        self.encoder.write(payload).await.map_err(Error::Encoder)
    }

    /// Returns next encoded buffer.
    async fn encode_next(&mut self) -> Result<SmallVec<[Bytes; 32]>, Error<S>> {
        self.encoder.next_buffer().await.map_err(Error::Encoder)
    }

    /// Writes data to stream until every byte is sent.
    ///
    /// Automatically repeats on partial writes.
    async fn write_to_stream(
        &mut self,
        batch: SmallVec<[Bytes; 32]>,
        fin: bool,
    ) -> Result<(), Error<S>> {
        if let Some(priority) = self.item_receiver.priority_once() {
            self.backend.set_priority(priority);
        }

        let contains_data = !batch.is_empty();

        match self
            .backend
            .send(batch, fin)
            .await
            .map_err(|e| Error::HangUp(format!("Outgoing.backend is unavailable: {e}").into()))?
        {
            Ok(_) => Ok(()),
            Err(e) => match e {
                StreamError::Finish => {
                    if contains_data {
                        // Panic, because we've lost data.
                        panic!("bug: attempt to write Bytes into a finished QUIC stream");
                    }

                    Ok(())
                }
                StreamError::ResetSending(e) => {
                    if cfg!(debug_assertions) {
                        panic!("bug: received StreamError::ResetSending({e}), redundant call");
                    }

                    Err(Error::ResetSending(e.into()))
                }
                StreamError::StopSending(e) => Err(Error::StopSending(e.into())),
                StreamError::Other(e) => Err(Error::HangUp(e)),
            },
        }
    }


    /// Close the stream direction and its resources.
    ///
    /// Notify the application and network peer when necessary.
    fn close(mut self, error: Error<S>) {
        if self.cancel_sender.send(Error::HangUp("".into())).is_err() {
            return;
        };

        let log_level = match &error {
            Error::Finish => Level::DEBUG,
            Error::StopSending(_) => Level::DEBUG,
            Error::ResetSending(_) => Level::DEBUG,
            Error::Decoder(_) => Level::ERROR,
            Error::Encoder(_) => Level::WARN,
            Error::Connection => Level::DEBUG,
            Error::HangUp(_) => Level::WARN,
        };

        log!(log_level, "closing out(write) stream, reason: {}", &error);
        let span = StreamSpan::from(Span::current());

        match error {
            Error::Finish => {
                span.on_fin();
            }

            // Peer sent 'STOP_SENDING' to us, notify the app.
            Error::StopSending(e) => {
                self.item_receiver.terminate(e);
                span.on_stop_sending(e.into());
            }

            // We need to terminate the stream without finishing it,
            // send 'RESET_STREAM' to peer.
            Error::ResetSending(e) => {
                self.backend.reset_sending(e.into());
                span.on_reset_stream(e.into());
            }

            // Occurred Encoder::Error, notify both sides.
            Error::Encoder(codec_err) => {
                let proto_err = S::on_encoder_error(&codec_err);
                self.item_receiver.terminate_due_encoder(codec_err);
                self.backend.reset_sending(proto_err.into());
                span.on_internal();
            }

            // Occurred connection error, notify the app.
            Error::Connection => {
                self.item_receiver.terminate_due_connection();
                span.on_connection_close();
            }

            // Internal error, notify both sides.
            Error::HangUp(e) => {
                self.item_receiver.hangup(e);
                self.backend.reset_sending(S::on_hangup().into());
                span.on_internal();
            }

            Error::Decoder(codec_err) => {
                span.on_internal();
                panic!(
                    "bug: received unexpected decoder error in outgoing stream: {}",
                    &codec_err
                );
            }
        }
    }
}
