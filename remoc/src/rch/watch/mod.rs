//! A single-producer, multi-consumer remote channel that only retains the last sent value.
//!
//! The sender and receiver can both be sent to remote endpoints.
//! The channel also works if both halves are local.
//! Forwarding over multiple connections is supported.
//!
//! This has similar functionality as [tokio::sync::watch] with the additional
//! ability to work over remote connections.
//!
//! # Alternatives
//!
//! If your endpoints need the ability to change the value and synchronize the changes
//! with other endpoints, consider using an [read/write lock](crate::robj::rw_lock)
//! instead.
//!
//! # Example
//!
//! In the following example the client sends a number and a watch channel sender to the server.
//! The server counts to the number and sends each value to the client over the watch channel.
//!
//! ```
//! use remoc::prelude::*;
//!
//! #[derive(Debug, serde::Serialize, serde::Deserialize)]
//! struct CountReq {
//!     up_to: u32,
//!     watch_tx: rch::watch::Sender<u32>,
//! }
//!
//! // This would be run on the client.
//! async fn client(mut tx: rch::base::Sender<CountReq>) {
//!     let (watch_tx, mut watch_rx) = rch::watch::channel(0);
//!     tx.send(CountReq { up_to: 4, watch_tx }).await.unwrap();
//!
//!     // Intermediate values may be missed.
//!     while *watch_rx.borrow_and_update().unwrap() != 3 {
//!         watch_rx.changed().await;
//!     }
//! }
//!
//! // This would be run on the server.
//! async fn server(mut rx: rch::base::Receiver<CountReq>) {
//!     while let Some(CountReq { up_to, watch_tx }) = rx.recv().await.unwrap() {
//!         for i in 0..up_to {
//!             watch_tx.send(i).unwrap();
//!         }
//!     }
//! }
//! # tokio_test::block_on(remoc::doctest::client_server(client, server));
//! ```
//!

use bytes::Buf;
use serde::{de::DeserializeOwned, Serialize};
use std::{fmt, ops::Deref};

use super::{base, RemoteSendError, DEFAULT_MAX_ITEM_SIZE};
use crate::{chmux, codec, rch::BACKCHANNEL_MSG_ERROR, RemoteSend};

mod receiver;
mod sender;

pub use receiver::{ChangedError, Receiver, ReceiverStream, RecvError};
pub use sender::{SendError, Sender};

/// Length of queuing for storing errors that occurred during remote send.
const ERROR_QUEUE: usize = 16;

/// Returns a reference to the inner value.
pub struct Ref<'a, T>(tokio::sync::watch::Ref<'a, Result<T, RecvError>>);

impl<'a, T> Deref for Ref<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}

impl<'a, T> fmt::Debug for Ref<'a, T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", &**self)
    }
}

/// Creates a new watch channel, returning the sender and receiver.
///
/// The sender and receiver may be sent to remote endpoints via channels.
pub fn channel<T, Codec>(init: T) -> (Sender<T, Codec>, Receiver<T, Codec>)
where
    T: RemoteSend,
{
    let (tx, rx) = tokio::sync::watch::channel(Ok(init));
    let (remote_send_err_tx, remote_send_err_rx) = tokio::sync::mpsc::channel(ERROR_QUEUE);

    let sender = Sender::new(tx, remote_send_err_tx.clone(), remote_send_err_rx, DEFAULT_MAX_ITEM_SIZE);
    let receiver = Receiver::new(rx, remote_send_err_tx, None);
    (sender, receiver)
}

/// Send implementation for deserializer of Sender and serializer of Receiver.
async fn send_impl<T, Codec>(
    mut rx: tokio::sync::watch::Receiver<Result<T, RecvError>>, raw_tx: chmux::Sender,
    mut raw_rx: chmux::Receiver, remote_send_err_tx: tokio::sync::mpsc::Sender<RemoteSendError>,
    max_item_size: usize,
) where
    T: Serialize + Send + Clone + 'static,
    Codec: codec::Codec,
{
    // Encode data using remote sender for sending.
    let mut remote_tx = base::Sender::<Result<T, RecvError>, Codec>::new(raw_tx);
    remote_tx.set_max_item_size(max_item_size);

    // Process events.
    loop {
        tokio::select! {
            biased;

            // Back channel message from remote endpoint.
            backchannel_msg = raw_rx.recv() => {
                match backchannel_msg {
                    Ok(Some(mut msg)) if msg.remaining() >= 1 => {
                        if msg.get_u8() == BACKCHANNEL_MSG_ERROR {
                            let _ = remote_send_err_tx.try_send(RemoteSendError::Forward);
                        }
                    }
                    _ => break,
                }
            }

            // Data to send to remote endpoint.
            changed = rx.changed() => {
                match changed {
                    Ok(()) => {
                        let value = rx.borrow_and_update().clone();
                        if let Err(err) = remote_tx.send(value).await {
                            let _ = remote_send_err_tx.try_send(RemoteSendError::Send(err.kind.clone()));
                            if err.is_item_specific() {
                                break
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// Receive implementation for serializer of Sender and deserializer of Receiver.
async fn recv_impl<T, Codec>(
    tx: tokio::sync::watch::Sender<Result<T, RecvError>>, mut raw_tx: chmux::Sender, raw_rx: chmux::Receiver,
    mut remote_send_err_rx: tokio::sync::mpsc::Receiver<RemoteSendError>,
    mut current_err: Option<RemoteSendError>, max_item_size: usize,
) where
    T: DeserializeOwned + Send + 'static,
    Codec: codec::Codec,
{
    // Decode raw received data using remote receiver.
    let mut remote_rx = base::Receiver::<Result<T, RecvError>, Codec>::new(raw_rx);
    remote_rx.set_max_item_size(max_item_size);

    // Process events.
    loop {
        tokio::select! {
            biased;

            // Channel closure requested locally.
            () = tx.closed() => break,

            // Notify remote endpoint of error.
            Some(_) = remote_send_err_rx.recv() => {
                let _ = raw_tx.send(vec![BACKCHANNEL_MSG_ERROR].into()).await;
            }
            () = futures::future::ready(()), if current_err.is_some() => {
                let _ = raw_tx.send(vec![BACKCHANNEL_MSG_ERROR].into()).await;
                current_err = None;
            }

            // Data received from remote endpoint.
            res = remote_rx.recv() => {
                let mut is_final_err = false;
                let value = match res {
                    Ok(Some(value)) => value,
                    Ok(None) => break,
                    Err(err) => {
                        is_final_err = err.is_final();
                        Err(RecvError::RemoteReceive(err))
                    },
                };
                if tx.send(value).is_err() {
                    break;
                }
                if is_final_err {
                    break;
                }
            }
        }
    }
}
