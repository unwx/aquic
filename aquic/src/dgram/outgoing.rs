use crate::log;
use crate::{debug_panic, dgram::Encoder, tracing::DgramSpan};
use std::{any::type_name, borrow::Cow};

use bytes::Bytes;
use futures::{FutureExt, select_biased};
use tracing::{Instrument, Level, Span};

use crate::{
    Spec,
    backend::dgram::OutDgramBackend,
    dgram::Error,
    exec::{Runtime, SendOnMt},
    sync::{
        SmartRc, dgram,
        mpmc::oneshot::{self, OneshotReceiver, OneshotSender},
    },
};


/// An outgoing direction of QUIC datagrams flow,
/// acts like a bridge between protocol backend and application for a single datagram direction.
///
/// `network_peer <-> quic_backend <-> Outgoing<S> <-> local_application`.
///
/// Its role is to receive incoming messages from app,
/// encode them, and send it to peer.
pub(crate) struct Outgoing<S, CId>
where
    S: Spec,
    CId: SendOnMt + Unpin + 'static,
{
    /// A reference to protocol specification.
    spec: SmartRc<S>,

    /// An API to communicate with QUIC implementation.
    backend: OutDgramBackend<CId>,

    /// Encoder for encoding messages into datagrams.
    encoder: S::DgramEncoder,

    /// Batch of datagrams.
    encoder_batch: Option<Vec<Bytes>>,

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
    CId: Clone + SendOnMt + Unpin + 'static,
{
    pub fn new(
        spec: SmartRc<S>,
        backend: OutDgramBackend<CId>,
        item_receiver: dgram::Receiver<S>,
    ) -> Self {
        let (cancel_sender, cancel_receiver) = oneshot::channel();

        let encoder = spec.new_dgram_encoder();
        let encoder_batch = Some(Vec::with_capacity(4));

        Self {
            spec,
            backend,
            encoder,
            encoder_batch,
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
    pub fn write(mut self, span: DgramSpan) -> oneshot::Sender<Error> {
        let cancel_sender = self.cancel_sender.clone();

        self.spawn_listen_for_app_error();
        self.spawn_io_loop(span);

        cancel_sender
    }


    /// Spawn a [Self::io_loop] with an additional listener for cancel signals.
    fn spawn_io_loop(mut self, span: DgramSpan) {
        let cancel_receiver = self.cancel_receiver.clone();

        let future = async move {
            select_biased! {
                e = cancel_receiver.recv().fuse() => {
                    self.close(e.unwrap_or_else(
                        || Error::HangUp(format!("'{}.cancel_receiver' is unavailable", type_name::<Self>()).into())
                    ));
                }
                r = self.io_loop().fuse() => {
                    if let Err(e) = r {
                        self.close(e);
                    }
                }
            }
        };

        Runtime::spawn_void(async move {
            future.instrument(span.into()).await;
        })
    }

    /// Application (the one who sends items)
    /// might drop the sender.
    ///
    /// Spawn a separate listener to be aware when it is no more interested in datagrams.
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
                        || Error::HangUp(format!("'{}.item_receiver.error_receiver' is unavailable", type_name::<Self>()).into())
                    );

                    match &app_err {
                        Error::End | Error::HangUp(_) => {
                            let _ = cancel_sender.send(app_err);
                        }
                        Error::Connection => {
                            // Do nothing.
                        }
                    }
                },
            }
        });
    }

    /// Runs the primary I/O loop.
    async fn io_loop(&mut self) -> Result<(), Error> {
        loop {
            self.recv_from_app().await?;
            self.write_to_backend().await?;
        }
    }


    /// Receives items from application,
    /// and encodes them into [Self::encoder_batch].
    async fn recv_from_app(&mut self) -> Result<(), Error> {
        let Some(batch) = self.encoder_batch.as_mut() else {
            return Err(Self::dpanic_or_hangup(
                format!(
                    "'{}.encoder_batch' is absent at 'recv_from_app()' point",
                    type_name::<Self>()
                )
                .into(),
            ));
        };

        if !batch.is_empty() {
            debug_panic!(
                "'{}.encoder_batch' was not empty at 'recv_from_app()' point",
                type_name::<Self>()
            );
            batch.clear();
        }

        {
            let item = Self::recv_item(&mut self.item_receiver, true)
                .await?
                .expect("there must be a value when 'blocking' is 'true'");

            self.encoder.encode(item, batch);
        }

        let mut total_bytes = batch.iter().map(|it| it.len()).sum::<usize>();
        let mut previous_batch_len = batch.len();
        let threshold = self.spec.dgram_encoder_max_batch_size();

        while total_bytes < threshold {
            match Self::recv_item(&mut self.item_receiver, false).await? {
                Some(item) => {
                    self.encoder.encode(item, batch);
                }
                None => {
                    break;
                }
            }

            if batch.len() > previous_batch_len {
                total_bytes += batch[previous_batch_len..]
                    .iter()
                    .map(|it| it.len())
                    .sum::<usize>();
                previous_batch_len = batch.len();
            }
        }

        Ok(())
    }

    async fn recv_item(
        receiver: &mut dgram::Receiver<S>,
        blocking: bool,
    ) -> Result<Option<S::DgramItem>, Error> {
        let result = if blocking {
            receiver.recv().await.map(Some)
        } else {
            receiver.try_recv()
        };

        let item = match result {
            Ok(it) => it,
            Err(e) => {
                return match &e {
                    Error::End => Err(e),
                    Error::HangUp(e) => Err(Error::HangUp(
                        format!(
                            "'{}.item_receiver' is unavailable: {}",
                            type_name::<Self>(),
                            e
                        )
                        .into(),
                    )),
                    Error::Connection => {
                        return Err(Self::dpanic_or_hangup(
                            format!(
                                "'{}.item_receiver' returned unexpected error: {}",
                                type_name::<Self>(),
                                e
                            )
                            .into(),
                        ));
                    }
                };
            }
        };

        Ok(item)
    }

    /// Writes encoded datagrams to QUIC backend.
    async fn write_to_backend(&mut self) -> Result<(), Error> {
        let Some(batch) = self.encoder_batch.take() else {
            return Err(Self::dpanic_or_hangup(
                format!(
                    "'{}.encoder_batch' is absent at 'write_to_backend()' point",
                    type_name::<Self>()
                )
                .into(),
            ));
        };

        let mut batch = match self.backend.send(batch).await.map_err(|e| {
            Error::HangUp(
                format!(
                    "'{}.backend' API is unavailable: {}",
                    type_name::<Self>(),
                    e
                )
                .into(),
            )
        })? {
            Ok(it) => it,
            Err(e) => {
                return Err(Error::HangUp(
                    format!(
                        "'{}.backend' API responded with '{}'",
                        type_name::<Self>(),
                        e
                    )
                    .into(),
                ));
            }
        };

        batch.clear();
        self.encoder_batch = Some(batch);
        Ok(())
    }


    fn close(&mut self, error: Error) {
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
