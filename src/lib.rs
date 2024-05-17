mod r#async;
mod client;
mod kcp;
mod poller;

mod queue;
mod server;
mod signal;

#[cfg(feature = "runtime_smol")]
mod smol;

#[cfg(feature = "runtime_tokio")]
mod tokio;

use std::{future::Future, pin::Pin};

pub use kcp::KcpConfig;
pub use poller::Timer;
pub use r#async::*;

use signal::{SigRead, SigWrite};
#[cfg(feature = "runtime_smol")]
pub use smol::*;

#[cfg(feature = "runtime_tokio")]
pub use tokio::*;

pub use kcp::KcpError;

pub use client::{ClientImpl, KcpConnector};
pub use kcp::{FAST_MODE, NORMAL_MODE};
pub use server::{KcpListener, ServerImpl};

macro_rules! background {
    ($name: ident, $kind: ident) => {
        impl Background {
            pub(crate) fn $name<F>(fut: F) -> Self
            where
                F: Future<Output = kcp::Result<()>> + Send + 'static,
            {
                Self {
                    future: Box::pin(fut),
                    kind: TaskKind::$kind,
                }
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub rcvbuf_size: usize,
    pub quebuf_size: usize,
    pub kcp_config: KcpConfig,
}

pub struct KcpStream<T>(T);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Reader,
    Sender,
    Poller,
    Closer,
}

pub struct Canceler(SigWrite<()>);

pub struct Background {
    kind: TaskKind,
    future: Pin<Box<dyn Future<Output = kcp::Result<()>> + Send + 'static>>,
}

background!(new_poller, Poller);
background!(new_closer, Closer);
background!(new_reader, Reader);
background!(new_sender, Sender);

pub trait Runner: Sized {
    type Err;
    fn start(process: Background) -> std::result::Result<Canceler, Self::Err>;
}

pub trait KcpRuntime: Sized {
    type Err;

    type Runner: Runner<Err = Self::Err>;

    type Timer: poller::Timer + Send + Sync + 'static;

    fn timer() -> Self::Timer;
}

impl Background {
    pub fn kind(&self) -> TaskKind {
        self.kind
    }
}

impl Canceler {
    pub fn new() -> (Canceler, SigRead<()>) {
        let (w, r) = signal::signal(1);
        (Canceler(w), r)
    }
}

impl Future for Background {
    type Output = kcp::Result<()>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        Pin::new(&mut self.future).poll(cx)
    }
}

impl<T> AsyncRead for KcpStream<T>
where
    T: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<T> AsyncWrite for KcpStream<T>
where
    T: AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rcvbuf_size: 1500,
            quebuf_size: 1024,
            kcp_config: kcp::FAST_MODE,
        }
    }
}

impl Canceler {
    pub fn cancel(&mut self) {
        self.0.close();
    }
}

impl Drop for Canceler {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
#[cfg(feature = "runtime_smol")]
mod tests {
    use crate::{KcpConnector, KcpListener};
    use std::sync::Arc;
    use std::thread;

    use super::kcp;

    #[test]
    fn overflow_test() {
        let a = 1u8.overflowing_add(255);
        println!("{:?}", a);
    }

    #[test]
    fn test_kcp_client() -> kcp::Result<()> {
        let executor = Arc::new(smol::Executor::<'_>::new());

        for _i in 1..10 {
            let executor = executor.clone();
            thread::spawn(move || loop {
                smol::block_on(executor.run(smol::future::pending()))
            });
        }

        smol::block_on(async move {
            env_logger::builder()
                .filter_module("kcp_rust", log::LevelFilter::Trace)
                .init();

            let udp = smol::net::UdpSocket::bind("0.0.0.0:0").await?;

            udp.connect("127.0.0.1:9999").await?;

            let tcp_server = smol::net::TcpListener::bind("0.0.0.0:7777").await.unwrap();
            let mut kcp_connector = KcpConnector::new_with_smol(udp, Default::default()).unwrap();

            loop {
                let (stream, _) = tcp_server.accept().await.unwrap();
                let (_, kcp) = kcp_connector.open().await.unwrap();

                executor
                    .spawn(async move {
                        log::debug!("thread {:?}", thread::current().id());
                        let (tcp_reader, tcp_writer) = smol::io::split(stream);
                        let (kcp_reader, kcp_writer) = smol::io::split(kcp);
                        if let Err(e) = smol::future::race(
                            smol::io::copy(tcp_reader, kcp_writer),
                            smol::io::copy(kcp_reader, tcp_writer),
                        )
                        .await
                        {
                            log::error!("{:?}", e);
                        };
                    })
                    .detach();
            }
        })
    }

    #[test]
    fn test_kcp_server() {
        env_logger::builder()
            .filter_module("kcp_rust", log::LevelFilter::Trace)
            .init();

        let executor = Arc::new(smol::Executor::<'_>::new());

        for _i in 1..10 {
            let executor = executor.clone();
            thread::spawn(move || loop {
                smol::block_on(executor.run(smol::future::pending()))
            });
        }

        smol::block_on(async {
            let udp = smol::net::UdpSocket::bind("127.0.0.1:9999").await.unwrap();

            let kcp_server = KcpListener::new_with_smol(udp, Default::default()).unwrap();

            loop {
                let (_, _, stream) = kcp_server.accept().await.unwrap();
                let kcp = smol::net::TcpStream::connect("127.0.0.1:8888")
                    .await
                    .unwrap();

                executor
                    .spawn(async move {
                        log::debug!("thread {:?}", thread::current().id());
                        let (tcp_reader, tcp_writer) = smol::io::split(stream);
                        let (kcp_reader, kcp_writer) = smol::io::split(kcp);
                        if let Err(e) = smol::future::race(
                            smol::io::copy(tcp_reader, kcp_writer),
                            smol::io::copy(kcp_reader, tcp_writer),
                        )
                        .await
                        {
                            log::error!("{:?}", e);
                        };
                    })
                    .detach();
            }
        })
    }
}
