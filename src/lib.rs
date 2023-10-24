mod r#async;
mod client;
mod kcp;
mod poller;

mod queue;
mod server;
mod signal;

use std::{future::Future, pin::Pin};

pub use kcp::KcpConfig;
pub use poller::Timer;
pub use r#async::*;

pub use client::{ClientImpl, KcpConnector};
pub use server::{KcpListener, ServerImpl};

pub struct KcpStream<T>(T);

pub struct Processor(Pin<Box<dyn Future<Output = kcp::Result<()>> + Send + 'static>>);

pub trait Runner: Sized {
    type Err;
    fn call(process: Processor) -> std::result::Result<(), Self::Err>;
}

pub trait KcpRuntime: Sized {
    type Err;

    type Runner: Runner<Err = Self::Err>;

    type Timer: poller::Timer + Send + Sync + 'static;

    fn timer() -> Self::Timer;
}

impl Future for Processor {
    type Output = kcp::Result<()>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::thread;

    use smol::future::FutureExt;
    use smol::io;

    use crate::r#async::{AsyncRecv, AsyncRecvfrom, AsyncSend, AsyncSendTo};
    use crate::{client::KcpConnector, server::KcpListener};
    use crate::{kcp, poller, KcpStream, Processor};

    #[derive(Clone)]
    pub struct UdpSocket {
        inner: smol::net::UdpSocket,
        reader_fut: Arc<std::sync::Mutex<Option<poller::BoxedFuture<io::Result<Vec<u8>>>>>>,
        writer_fut: Arc<std::sync::Mutex<Option<poller::BoxedFuture<io::Result<usize>>>>>,
        sendto_fut: Arc<std::sync::Mutex<Option<poller::BoxedFuture<io::Result<usize>>>>>,
        recvfrom_fut:
            Arc<std::sync::Mutex<Option<poller::BoxedFuture<io::Result<(SocketAddr, Vec<u8>)>>>>>,
    }

    impl UdpSocket {
        pub fn new(udp: smol::net::UdpSocket) -> Self {
            Self {
                inner: udp,
                reader_fut: Default::default(),
                writer_fut: Default::default(),
                recvfrom_fut: Default::default(),
                sendto_fut: Default::default(),
            }
        }
    }

    impl AsyncSend for UdpSocket {
        fn poll_send(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            // let a = self.send(buf);

            let mut this = self.writer_fut.lock().unwrap();

            let mut fut = this.take().unwrap_or_else(|| {
                let udp = self.inner.clone();
                let mut buf = buf.to_vec();
                Box::pin(async move {
                    let n = udp.send(&mut buf).await?;
                    std::io::Result::Ok(n)
                })
            });

            match Pin::new(&mut fut).poll(cx) {
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => {
                    *this = Some(fut);
                    std::task::Poll::Pending
                }
                std::task::Poll::Ready(Ok(n)) => std::task::Poll::Ready(Ok(n)),
            }
        }
    }

    impl AsyncRecv for UdpSocket {
        fn poll_recv(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let mut this = self.reader_fut.lock().unwrap();

            let mut fut = this.take().unwrap_or_else(|| {
                let udp = self.inner.clone();
                let mut buf = unsafe {
                    let mut tmp = Vec::with_capacity(buf.len());
                    tmp.set_len(buf.len());
                    tmp
                };
                Box::pin(async move {
                    let n = udp.recv(&mut buf).await?;
                    buf.truncate(n);
                    std::io::Result::Ok(buf)
                })
            });

            match Pin::new(&mut fut).poll(cx) {
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => {
                    *this = Some(fut);
                    std::task::Poll::Pending
                }
                std::task::Poll::Ready(Ok(data)) => {
                    buf[..data.len()].copy_from_slice(&data);
                    std::task::Poll::Ready(Ok(data.len()))
                }
            }
        }
    }

    impl AsyncSendTo for UdpSocket {
        fn poll_sendto(
            &mut self,
            cx: &mut std::task::Context<'_>,
            addr: &SocketAddr,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            // let a = self.send(buf);

            let mut this = self.sendto_fut.lock().unwrap();

            let mut fut = this.take().unwrap_or_else(|| {
                let udp = self.inner.clone();
                let mut buf = buf.to_vec();
                let addr = *addr;
                Box::pin(async move {
                    let n = udp.send_to(&mut buf, addr).await?;
                    std::io::Result::Ok(n)
                })
            });

            match Pin::new(&mut fut).poll(cx) {
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => {
                    *this = Some(fut);
                    std::task::Poll::Pending
                }
                std::task::Poll::Ready(Ok(n)) => std::task::Poll::Ready(Ok(n)),
            }
        }
    }

    impl AsyncRecvfrom for UdpSocket {
        fn poll_recvfrom(
            &mut self,
            cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<(std::net::SocketAddr, usize)>> {
            let mut this = self.recvfrom_fut.lock().unwrap();

            let mut fut = this.take().unwrap_or_else(|| {
                let udp = self.inner.clone();
                let mut buf = unsafe {
                    let mut tmp = Vec::with_capacity(buf.len());
                    tmp.set_len(buf.len());
                    tmp
                };
                Box::pin(async move {
                    let (n, addr) = udp.recv_from(&mut buf).await?;

                    buf.truncate(n);

                    std::io::Result::Ok((addr, buf))
                })
            });

            match Pin::new(&mut fut).poll(cx) {
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => {
                    *this = Some(fut);
                    std::task::Poll::Pending
                }
                std::task::Poll::Ready(Ok((addr, data))) => {
                    buf[..data.len()].copy_from_slice(&data);
                    std::task::Poll::Ready(Ok((addr, data.len())))
                }
            }
        }
    }

    struct KcpClientRuntime;

    struct KcpRunner;

    struct KcpTimer;

    impl poller::Timer for KcpTimer {
        type Ret = std::time::Instant;
        type Output = smol::Timer;

        fn sleep(&self, time: std::time::Duration) -> Self::Output {
            smol::Timer::after(time)
        }
    }

    impl crate::Runner for KcpRunner {
        type Err = io::Error;
        fn call(process: Processor) -> std::result::Result<(), Self::Err> {
            std::thread::Builder::new()
                .name("processor[kcp]".into())
                .spawn(move || {
                    smol::block_on(async move {
                        if let Err(e) = process.await {
                            log::debug!("error {:?}", e);
                        };
                    });

                    log::debug!("exit thread ...");
                })
                .unwrap();

            Ok(())
        }
    }

    impl smol::io::AsyncRead for KcpStream<crate::client::ClientImpl> {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<io::Result<usize>> {
            match crate::AsyncRead::poll_read(self, cx, buf)? {
                std::task::Poll::Pending => std::task::Poll::Pending,
                std::task::Poll::Ready(n) => std::task::Poll::Ready(Ok(n)),
            }
        }
    }

    impl smol::io::AsyncWrite for KcpStream<crate::client::ClientImpl> {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            crate::AsyncWrite::poll_write(self, cx, buf)
        }

        fn poll_close(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            crate::AsyncWrite::poll_close(self, cx)
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            crate::AsyncWrite::poll_flush(self, cx)
        }
    }

    impl smol::io::AsyncRead for KcpStream<crate::server::ServerImpl> {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<io::Result<usize>> {
            crate::AsyncRead::poll_read(self, cx, buf)
        }
    }

    impl smol::io::AsyncWrite for KcpStream<crate::server::ServerImpl> {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            crate::AsyncWrite::poll_write(self, cx, buf)
        }

        fn poll_close(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            crate::AsyncWrite::poll_close(self, cx)
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            crate::AsyncWrite::poll_flush(self, cx)
        }
    }

    impl crate::KcpRuntime for KcpClientRuntime {
        type Err = io::Error;

        type Runner = KcpRunner;

        type Timer = KcpTimer;

        fn timer() -> Self::Timer {
            KcpTimer
        }
    }


    #[test]
    fn overflow_test(){
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
            let mut kcp_connector = KcpConnector::new::<KcpClientRuntime>(UdpSocket::new(udp))?;

            loop {
                let (stream, _) = tcp_server.accept().await.unwrap();
                let kcp = kcp_connector.open().await.unwrap();

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

            let udp = UdpSocket::new(udp);
            let kcp_server = KcpListener::new::<KcpClientRuntime>(udp).unwrap();

            loop {
                let stream = kcp_server.accept().await.unwrap();
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
