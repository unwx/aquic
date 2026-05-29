use crate::AsyncRuntime;
use crate::backend::{self, StableConnectionId};
use crate::core::OutDgramBackend;
use crate::log;
use crate::{
    Spec,
    dgram::Error,
    sync::{
        SmartRc, dgram,
        mpmc::oneshot::{self, OneshotReceiver, OneshotSender},
    },
};
use crate::{debug_panic, dgram::Encoder, tracing::DgramSpan};
use futures::{FutureExt, future, select_biased};
use std::{any::type_name, borrow::Cow};
use tracing::{Instrument, Level, Span};


/// An outgoing direction of QUIC datagrams flow,
/// acts like a bridge between protocol backend and application for a single datagram direction.
///
/// `Network Peer << QUIC Backend << Outgoing<S, CId> << Application`.
///
/// Its role is to:
/// 1) Receive incoming items from `Application`.
/// 2) Encode the items.
/// 3) Send datagrams to `QUIC Backend`.
pub(crate) struct Outgoing<S, CId>
where
    S: Spec,
    CId: StableConnectionId,
{
    /// An API to communicate with QUIC implementation.
    backend: OutDgramBackend<S, CId>,

    /// Encoder for encoding messages into datagrams.
    encoder: Option<S::DgramEncoder>,

    /// Channel for receiving messages from app.
    item_receiver: dgram::Receiver<S>,

    /// Channel for sending a cancellation error: stops the stream I/O loop.
    cancel_sender: oneshot::Sender<Error>,

    /// Channel for receiving a cancellation error.
    cancel_receiver: oneshot::Receiver<Error>,
}

impl<S, CId> Outgoing<S, CId>
where
    S: Spec,
    CId: StableConnectionId,
{
    pub fn new(
        spec: SmartRc<S>,
        backend: OutDgramBackend<S, CId>,
        item_receiver: dgram::Receiver<S>,
    ) -> Self {
        let (cancel_sender, cancel_receiver) = oneshot::channel();
        let encoder = spec.new_datagram_encoder();

        Self {
            backend,
            encoder: Some(encoder),
            item_receiver,
            cancel_sender,
            cancel_receiver,
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
    /// **Note**: only a [Error::Connection] is allowed to be sent via this sender.
    pub fn write<AR: AsyncRuntime>(mut self, span: DgramSpan) -> oneshot::Sender<Error> {
        let cancel_sender = self.cancel_sender.clone();
        let cancel_receiver = self.cancel_receiver.clone();
        let app_error_receiver = self.item_receiver.error_receiver();

        let app_error_future = || async move {
            let app_err = app_error_receiver.recv().await.unwrap_or_else(|| {
                Error::HangUp(
                    format!(
                        "'{}' items '{}' error channel is unavailable",
                        type_name::<Self>(),
                        type_name::<S::DgramItem>()
                    )
                    .into(),
                )
            });

            match &app_err {
                Error::End | Error::HangUp(_) => app_err,

                // We produce this error, ignore.
                Error::Connection => future::pending().await,
            }
        };

        let future = async move {
            select_biased! {
                e = self.io_loop().fuse() => {
                    self.close(e.map(|_| Error::End).unwrap_or_else(|e| e));
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


    async fn io_loop(&mut self) -> Result<(), Error> {
        loop {
            self.recv_from_app().await?;
            self.send_to_backend().await?;
        }
    }

    async fn recv_from_app(&mut self) -> Result<(), Error> {
        if self.encoder.is_none() {
            return Ok(());
        }
        if !self.recv_item(true).await? {
            return Ok(());
        };

        if let Some(encoder) = self.encoder.as_ref()
            && encoder.should_flush()
        {
            return Ok(());
        }

        while self.recv_item(false).await? {
            if let Some(encoder) = self.encoder.as_ref()
                && encoder.should_flush()
            {
                break;
            }
        }

        Ok(())
    }

    async fn recv_item(&mut self, blocking: bool) -> Result<bool, Error> {
        let Some(encoder) = self.encoder.as_mut() else {
            return Ok(false);
        };

        let result = {
            if blocking {
                self.item_receiver.recv().await.map(Some)
            } else {
                self.item_receiver.try_recv()
            }
        };

        let item = result.map_err(|e| match &e {
            Error::End => e,
            Error::HangUp(e) => Error::HangUp(
                format!(
                    "'{}' items '{}' channel is unavailable: {}",
                    type_name::<Self>(),
                    type_name::<S::DgramItem>(),
                    e
                )
                .into(),
            ),
            other => Self::dpanic_or_hangup(
                format!(
                    "'{}' items '{}' channel returned an unexpected error: {}",
                    type_name::<Self>(),
                    type_name::<S::DgramItem>(),
                    other
                )
                .into(),
            ),
        })?;

        if let Some(item) = item {
            encoder.encode(item);
            return Ok(true);
        }

        Ok(false)
    }

    async fn send_to_backend(&mut self) -> Result<(), Error> {
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


    fn close(&mut self, error: Error) {
        if self.cancel_sender.send(error.clone()).is_err() {
            return;
        };

        let log_level = match &error {
            Error::End | Error::Connection => Level::DEBUG,
            Error::HangUp(_) => Level::WARN,
        };

        log!(
            log_level,
            "closing out(write) datagram direction, reason: {}",
            &error
        );

        let span = DgramSpan::from(Span::current());
        match error {
            Error::End => span.on_normal_end(),
            Error::Connection => {
                span.on_connection_close();
                self.item_receiver.terminate_due_connection();
            }
            Error::HangUp(e) => {
                span.on_internal();
                self.item_receiver.hangup(e);
            }
        }
    }

    fn dpanic_or_hangup(msg: Cow<'static, str>) -> Error {
        debug_panic!("{msg}");
        Error::HangUp(msg)
    }
}
