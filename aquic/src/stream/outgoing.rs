use std::borrow::Cow;

use crate::backend::stream::OutStreamBackend;
use crate::debug_panic;
use crate::exec::{Runtime, SendOnMt};
use crate::log;
use crate::stream::{Encoder, Error, Payload};
use crate::sync::oneshot::{OneshotReceiver, OneshotSender};
use crate::sync::stream;
use crate::sync::{SmartRc, oneshot};
use crate::tracing::StreamSpan;
use crate::{Spec, backend};
use bytes::Bytes;
use futures::{FutureExt, select_biased};
use tracing::{Instrument, Level, Span};

/// An outgoing direction of QUIC stream,
/// acts like a bridge between protocol backend and application listener for a single stream.
///
/// `network_peer <-> quic_backend <-> Outgoing<S> <-> local_application`.
///
/// Its role is to receive incoming messages from app,
/// encode them, and send it to peer.
///
/// The entire flow is sequential: implementation won't write to the stream,
/// unless the network peer is ready to receive the data.
pub(crate) struct Outgoing<S, CId>
where
    S: Spec,
    CId: SendOnMt + Unpin + 'static,
{
    /// A reference to protocol specification.
    spec: SmartRc<S>,

    /// An API to communicate with QUIC implementation.
    backend: OutStreamBackend<CId>,

    /// Encoder for encoding messages into bytes.
    encoder: S::StreamEncoder,

    /// Batch of decoded items.
    encoder_batch: Option<Vec<Bytes>>,

    /// Channel for receiving messages from app.
    item_receiver: stream::Receiver<S>,

    /// Channel for sending a cancellation error: stops the stream I/O loop.
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
        let (cancel_sender, cancel_receiver) = oneshot::channel();

        let encoder = spec.new_stream_encoder();
        let encoder_batch = Some(Vec::with_capacity(4));

        Self {
            spec,
            backend,
            encoder,
            encoder_batch,
            item_receiver,
            cancel_sender,
            cancel_receiver,
            draining: false,
        }
    }

    /// Starts the stream I/O loop in a separate task.
    ///
    /// Listens for messages from application,
    /// encodes them,
    /// and sends them to the QUIC backend.
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
                        || Error::HangUp("'stream::Outgoing.cancel_receiver' is unavailable".into())
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
                        || Error::HangUp("'stream::Outgoing.item_receiver.error_receiver' is unavailable".into())
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
            self.recv().await?;
            self.write_to_stream().await?;

            if self.draining {
                return Ok(());
            }
        }
    }

    /// Receives items from the application, until one of the conditions is true:
    /// - No more items in the channel.
    /// - Total amount of bytes buffered exceeding [Spec::stream_encoder_max_batch_size].
    async fn recv(&mut self) -> Result<(), Error<S>> {
        if self.draining {
            return Ok(());
        }

        let Some(mut batch) = self.encoder_batch.take() else {
            return Err(Self::panic_on_debug(
                "'stream::Outgoing::encoder_batch' is absent at 'recv()' point".into(),
            ));
        };

        if !batch.is_empty() {
            self.encoder_batch = Some(batch);

            const MSG: &str = "'stream::Outgoing.encoder_batch' must be empty at 'recv()' point";

            if self.draining {
                return Err(Self::panic_on_debug(MSG.into()));
            }

            debug_panic!("{MSG}");
            return Ok(()); // Let the loop process the remaining...
        }

        if !self.recv_item(true, &mut batch).await? {
            self.encoder_batch = Some(batch);
            return Ok(());
        }

        let mut total_bytes = batch.iter().map(|it| it.len()).sum::<usize>();
        let mut previous_batch_len = batch.len();
        let threshold = self.spec.stream_encoder_max_batch_size();

        while total_bytes < threshold && !self.draining {
            if !self.recv_item(false, &mut batch).await? {
                break;
            }

            if !batch.is_empty() && batch.len() > previous_batch_len {
                previous_batch_len = batch.len();
                total_bytes += batch[previous_batch_len..]
                    .iter()
                    .map(|it| it.len())
                    .sum::<usize>();
            }
        }

        self.encoder_batch = Some(batch);
        Ok(())
    }

    /// Receives item from the application, and encodes it into [Self::encoder_batch].
    ///
    /// Does nothing if we've received 'FIN' before.
    ///
    /// Returns false if no item was written.
    async fn recv_item(
        &mut self,
        blocking: bool,
        buffer: &mut Vec<Bytes>,
    ) -> Result<bool, Error<S>> {
        if self.draining {
            return Ok(false);
        }

        let payload_result = if blocking {
            self.item_receiver.recv().await.map(Some)
        } else {
            self.item_receiver.try_recv()
        };

        let payload = match payload_result {
            Ok(it) => it,
            Err(e) => match &e {
                Error::Finish => {
                    // 'Error::Finish' means that stream was finished before the `recv()` call.
                    // There is an issue somewhere, but not critical I guess?
                    debug_panic!(
                        "received 'Finish' error from 'stream::Receiver' when 'self.draining' is false"
                    );

                    Some(Payload::Done)
                }
                Error::ResetSending(_) | Error::HangUp(_) => {
                    return Err(e);
                }

                Error::StopSending(_)
                | Error::Decoder(_)
                | Error::Encoder(_)
                | Error::Connection => {
                    return Err(Self::panic_on_debug(
                        format!("received unexpected error from 'stream::Receiver': {e}").into(),
                    ));
                }
            },
        };

        let Some(payload) = payload else {
            return Ok(false);
        };

        if payload.is_fin() {
            self.draining = true;
        }

        self.encoder
            .encode(payload, buffer)
            .await
            .map(|_| true)
            .map_err(Error::Encoder)
    }

    /// Writes data to stream until every byte is sent.
    async fn write_to_stream(&mut self) -> Result<(), Error<S>> {
        if let Some(priority) = self.item_receiver.priority_once() {
            self.backend.set_priority(priority);
        }

        let Some(batch) = self.encoder_batch.take() else {
            return Err(Self::panic_on_debug(
                "'stream::Outgoing::encoder_batch' is absent at 'write_to_stream()' point".into(),
            ));
        };

        let batch = match self.backend.send(batch, self.draining).await.map_err(|e| {
            Error::HangUp(format!("'stream::Outgoing.backend' is unavailable: {e}").into())
        })? {
            Ok(it) => it,
            Err(e) => {
                return match e {
                    backend::Error::StreamFinish => Err(Self::panic_on_debug(
                        "'stream::Outgoing': attempt to write bytes into a finished QUIC stream, message loss?".into(),
                    )),
                    backend::Error::StreamResetSending(e) => {
                        debug_panic!("received 'Error::StreamResetSending({e})', redundant call?");
                        Err(Error::ResetSending(e.into()))
                    }
                    backend::Error::StreamStopSending(e) => Err(Error::StopSending(e.into())),
                    other => Err(Error::HangUp(other.to_string().into())),
                };
            }
        };

        self.encoder_batch = Some(batch);
        Ok(())
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
                let proto_err = S::on_stream_encoder_error(&codec_err);
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
                let e: Cow<'static, str> = format!(
                    "Received unexpected decoder error in stream::Outgoing: \
                    {codec_err}"
                )
                .into();
                debug_panic!("{e}");

                self.item_receiver.hangup(e);
                self.backend.reset_sending(S::on_hangup().into());
                span.on_internal();
            }
        }
    }

    fn panic_on_debug(msg: Cow<'static, str>) -> Error<S> {
        debug_panic!("{msg}");
        Error::HangUp(msg)
    }
}
