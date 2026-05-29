use crate::AsyncRuntime;
use crate::backend::StableConnectionId;
use crate::core::OutStreamBackend;
use crate::debug_panic;
use crate::log;
use crate::stream::{Encoder, Error};
use crate::sync::SmartRc;
use crate::sync::mpmc::oneshot::{self, OneshotReceiver, OneshotSender};
use crate::sync::stream;
use crate::tracing::StreamSpan;
use crate::{Spec, backend};
use futures::{FutureExt, future, select_biased};
use std::any::type_name;
use std::borrow::Cow;
use tracing::{Instrument, Level, Span};

/// An outgoing QUIC stream direction,
/// acts like a bridge between protocol backend and application for a single stream.
///
/// `Network Peer << QUIC Backend << Outgoing<S, CId> << Application`.
///
/// Its role is to:
/// 1) Receive incoming messages from `Application`.
/// 2) Encode the messages.
/// 3) Send encoded chunks of bytes to `QUIC Backend`.
///
/// The entire flow is sequential: implementation won't write to `QUIC Backend`,
/// unless it's ready to receive the data.
pub(crate) struct Outgoing<S, CId>
where
    S: Spec,
    CId: StableConnectionId,
{
    /// An API to communicate with QUIC Backend.
    backend: OutStreamBackend<S, CId>,

    /// Encoder for encoding messages into bytes.
    encoder: Option<S::StreamEncoder>,

    /// Channel for receiving messages from app.
    item_receiver: stream::Receiver<S>,

    /// Channel for sending a cancellation error: stops the stream I/O loop.
    cancel_sender: oneshot::Sender<Error<S>>,

    /// Channel for receiving a cancellation error.
    cancel_receiver: oneshot::Receiver<Error<S>>,

    /// `true`, if 'FIN' was received from the `item_receiver` channel.
    stream_finished: bool,
}

impl<S, CId> Outgoing<S, CId>
where
    S: Spec,
    CId: StableConnectionId,
{
    pub(crate) fn new(
        spec: SmartRc<S>,
        backend: OutStreamBackend<S, CId>,
        item_receiver: stream::Receiver<S>,
    ) -> Self {
        let (cancel_sender, cancel_receiver) = oneshot::channel();
        let encoder = spec.new_stream_encoder();

        Self {
            backend,
            encoder: Some(encoder),
            item_receiver,
            cancel_sender,
            cancel_receiver,
            stream_finished: false,
        }
    }

    /// Starts the stream I/O loop in a separate task.
    ///
    /// - Receives incoming messages from `Application`.
    /// - Encodes the messages.
    /// - Sends encoded chunks of bytes to `QUIC Backend`.
    ///
    /// Returns a cancellation sender.
    ///
    /// **Note**: only few variants of [Error] are allowed to be sent via this sender:
    /// - [Error::Stop],
    /// - [Error::Connection].
    pub(crate) fn write<AR: AsyncRuntime>(mut self, span: StreamSpan) -> oneshot::Sender<Error<S>> {
        let cancel_sender = self.cancel_sender.clone();
        let cancel_receiver = self.cancel_receiver.clone();
        let app_error_receiver = self.item_receiver.error_receiver();

        let app_error_future = || async move {
            let app_err = app_error_receiver.recv().await.unwrap_or_else(|| {
                Error::HangUp(
                    format!(
                        "'{}' items '{}' error channel is unavailable",
                        type_name::<Self>(),
                        type_name::<S::StreamItem>()
                    )
                    .into(),
                )
            });

            match &app_err {
                Error::Reset(_) | Error::HangUp(_) => app_err,

                // We produce each of these errors, ignore.
                Error::Finish
                | Error::Stop(_)
                | Error::Decoder(_)
                | Error::Encoder(_)
                | Error::Connection => future::pending().await,
            }
        };

        let future = async move {
            select_biased! {
                e = self.io_loop().fuse() => {
                    self.close(e.map(|_| Error::Finish).unwrap_or_else(|e| e));
                }

                e = cancel_receiver.recv().fuse() => {
                    self.close(e.unwrap_or_else(
                        || Error::HangUp(format!(
                            "'{}' cancellation-signal channel is unavailable",
                            type_name::<Self>()
                        ).into())
                    ));
                }

                app_err = app_error_future().fuse() => {
                    self.close(app_err);
                },
            }
        };

        AR::spawn_void(async move {
            future.instrument(span.into()).await;
        });

        cancel_sender
    }


    async fn io_loop(&mut self) -> Result<(), Error<S>> {
        loop {
            self.recv_from_app().await?;
            self.write_to_stream().await?;

            if self.stream_finished {
                return Ok(());
            }
        }
    }

    async fn recv_from_app(&mut self) -> Result<(), Error<S>> {
        if self.stream_finished {
            return Ok(());
        }
        if !self.recv_item(true).await? {
            return Ok(());
        };

        while !self.stream_finished {
            if let Some(encoder) = self.encoder.as_ref()
                && encoder.should_flush()
            {
                break;
            }

            if !self.recv_item(false).await? {
                break;
            }
        }

        Ok(())
    }

    async fn recv_item(&mut self, blocking: bool) -> Result<bool, Error<S>> {
        if self.stream_finished {
            return Ok(false);
        }
        let Some(encoder) = self.encoder.as_mut() else {
            return Ok(false);
        };

        let payload_result = if blocking {
            self.item_receiver.recv().await.map(Some)
        } else {
            self.item_receiver.try_recv()
        };

        let payload = match payload_result {
            Ok(it) => it,
            Err(e) => {
                return match &e {
                    Error::HangUp(e) => Err(Error::HangUp(
                        format!(
                            "'{}' items '{}' channel is unavailable: {}",
                            type_name::<Self>(),
                            type_name::<S::StreamItem>(),
                            e
                        )
                        .into(),
                    )),
                    Error::Reset(_) => Err(e),

                    Error::Finish
                    | Error::Stop(_)
                    | Error::Decoder(_)
                    | Error::Encoder(_)
                    | Error::Connection => Err(Self::dpanic_or_hangup(
                        format!(
                            "'{}' items '{}' channel returned an unexpected error: {}",
                            type_name::<Self>(),
                            type_name::<S::StreamItem>(),
                            e
                        )
                        .into(),
                    )),
                };
            }
        };

        let Some(payload) = payload else {
            return Ok(false);
        };
        if payload.is_fin() {
            self.stream_finished = true;
        }

        encoder.encode(payload).map_err(Error::Encoder)?;
        Ok(true)
    }

    async fn write_to_stream(&mut self) -> Result<(), Error<S>> {
        if let Some(priority) = self.item_receiver.priority_once() {
            self.backend.set_priority(priority);
        }
        let Some(encoder) = self.encoder.take() else {
            return Ok(());
        };

        let call_result = self.backend.send(encoder).await;
        let backend_result = call_result.map_err(|e| {
            Error::HangUp(
                format!(
                    "'{}' QUIC backend RPC channel is unavailable: {}",
                    type_name::<Self>(),
                    e
                )
                .into(),
            )
        })?;

        match backend_result {
            Ok(enc) => {
                self.encoder = Some(enc);
                Ok(())
            }
            Err(e) => match e {
                backend::Error::StreamStop(e) => Err(Error::Stop(e.into())),
                backend::Error::Closed | backend::Error::UnknownConnection => {
                    Err(Error::Connection)
                }
                other => Err(Error::HangUp(
                    format!(
                        "'{}' QUIC backend responded with an unexpected error: {}",
                        type_name::<Self>(),
                        other
                    )
                    .into(),
                )),
            },
        }
    }


    fn close(mut self, error: Error<S>) {
        if self.cancel_sender.send(error.clone()).is_err() {
            return;
        };

        let log_level = match &error {
            Error::Finish => Level::DEBUG,
            Error::Stop(_) => Level::DEBUG,
            Error::Reset(_) => Level::DEBUG,
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
            Error::Stop(e) => {
                self.item_receiver.terminate(e);
                span.on_stop_sending(e.into());
            }

            // We need to terminate the stream without finishing it,
            // send 'RESET_STREAM' to peer.
            Error::Reset(e) => {
                self.backend.terminate(e);
                span.on_reset_stream(e.into());
            }

            // Occurred Encoder::Error, notify both sides.
            Error::Encoder(codec_err) => {
                self.item_receiver.terminate_due_encoder(codec_err.clone());
                self.backend.terminate_due_encoder(codec_err);
                span.on_internal();
            }

            // Occurred connection error, notify the app.
            Error::Connection => {
                self.item_receiver.terminate_due_connection();
                span.on_connection_close();
            }

            // Internal error, notify both sides.
            Error::HangUp(e) => {
                self.item_receiver.hangup(e.clone());
                self.backend.hangup(e);
                span.on_internal();
            }

            Error::Decoder(codec_err) => {
                let e: Cow<'static, str> = format!(
                    "'{}' received an unexpected encoder error: {}",
                    type_name::<Self>(),
                    codec_err
                )
                .into();
                debug_panic!("{e}");

                self.item_receiver.hangup(e.clone());
                self.backend.hangup(e);
                span.on_internal();
            }
        }
    }

    fn dpanic_or_hangup(msg: Cow<'static, str>) -> Error<S> {
        debug_panic!("{msg}");
        Error::HangUp(msg)
    }
}
