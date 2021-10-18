#[cfg(feature = "rch")]
use futures::{join, StreamExt};

#[allow(unused_imports)]
use std::{net::Ipv4Addr, sync::Once};

#[cfg(feature = "rch")]
use tokio::net::{TcpListener, TcpStream};

#[cfg(feature = "rch")]
use remoc::{rch::remote, RemoteSend};

mod chmux;

#[cfg(feature = "serde")]
mod codec;

#[cfg(feature = "rch")]
mod rch;

#[cfg(feature = "rfn")]
mod rfn;

#[cfg(feature = "robj")]
mod robj;

#[cfg(feature = "rtc")]
mod rtc;

static INIT: Once = Once::new();

pub fn init() {
    INIT.call_once(env_logger::init);
}

#[macro_export]
macro_rules! loop_transport {
    ($queue_length:expr, $a_tx:ident, $a_rx:ident, $b_tx:ident, $b_rx:ident) => {
        let ($a_tx, $b_rx) = futures::channel::mpsc::channel::<bytes::Bytes>($queue_length);
        let ($b_tx, $a_rx) = futures::channel::mpsc::channel::<bytes::Bytes>($queue_length);

        let $a_rx = $a_rx.map(Ok::<_, std::io::Error>);
        let $b_rx = $b_rx.map(Ok::<_, std::io::Error>);
    };
}

#[cfg(feature = "rch")]
pub async fn loop_channel<T>(
) -> ((remote::Sender<T>, remote::Receiver<T>), (remote::Sender<T>, remote::Receiver<T>))
where
    T: RemoteSend,
{
    let cfg = remoc::chmux::Cfg::default();
    loop_channel_with_cfg(cfg).await
}

#[cfg(feature = "rch")]
pub async fn loop_channel_with_cfg<T>(
    cfg: remoc::chmux::Cfg,
) -> ((remote::Sender<T>, remote::Receiver<T>), (remote::Sender<T>, remote::Receiver<T>))
where
    T: RemoteSend,
{
    loop_transport!(0, transport_a_tx, transport_a_rx, transport_b_tx, transport_b_rx);

    let a_cfg = cfg.clone();
    let a = async move {
        let (conn, tx, rx) = remoc::Connect::framed(a_cfg, transport_a_tx, transport_a_rx).await.unwrap();
        tokio::spawn(conn);
        (tx, rx)
    };

    let b_cfg = cfg.clone();
    let b = async move {
        let (conn, tx, rx) = remoc::Connect::framed(b_cfg, transport_b_tx, transport_b_rx).await.unwrap();
        tokio::spawn(conn);
        (tx, rx)
    };

    join!(a, b)
}

#[cfg(feature = "rch")]
pub async fn tcp_loop_channel<T>(
    tcp_port: u16,
) -> ((remote::Sender<T>, remote::Receiver<T>), (remote::Sender<T>, remote::Receiver<T>))
where
    T: RemoteSend,
{
    let server = async move {
        let listener = TcpListener::bind((Ipv4Addr::new(127, 0, 0, 1), tcp_port)).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let (socket_rx, socket_tx) = socket.into_split();
        let (conn, tx, rx) =
            remoc::Connect::io_buffered(Default::default(), socket_rx, socket_tx, 100_000).await.unwrap();
        tokio::spawn(conn);
        (tx, rx)
    };

    let client = async move {
        let socket = TcpStream::connect((Ipv4Addr::new(127, 0, 0, 1), tcp_port)).await.unwrap();
        let (socket_rx, socket_tx) = socket.into_split();
        let (conn, tx, rx) =
            remoc::Connect::io_buffered(Default::default(), socket_rx, socket_tx, 8721).await.unwrap();
        tokio::spawn(conn);
        (tx, rx)
    };

    join!(server, client)
}
