mod r#async;
mod client;
mod kcp;
mod poller;
mod queue;
mod server;
mod signal;
mod sync;

use std::pin::Pin;

use r#async::{AsyncRead, AsyncSend, AsyncWrite};

pub struct KcpStream<T>(T);

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
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        unimplemented!()
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::Duration;

    use smol::future::FutureExt;
    use smol::io;

    use crate::client::Processor;
    use crate::r#async::{AsyncReadExt, AsyncRecv, AsyncSend, AsyncSendTo, AsyncWriteExt};
    use crate::{client, kcp, poller};
    use crate::{client::KcpConnector, server::KcpListener};

    #[derive(Clone)]
    pub struct UdpSocket {
        inner: smol::net::UdpSocket,
        reader_fut: Arc<std::sync::Mutex<Option<poller::BoxedFuture<io::Result<Vec<u8>>>>>>,
        writer_fut: Arc<std::sync::Mutex<Option<poller::BoxedFuture<io::Result<usize>>>>>,
    }

    impl UdpSocket {
        pub fn new(udp: smol::net::UdpSocket) -> Self {
            Self {
                inner: udp,
                reader_fut: Default::default(),
                writer_fut: Default::default(),
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
            log::debug!("send {:?}", String::from_utf8_lossy(buf));

            let mut this = self.writer_fut.lock().unwrap();

            let mut fut = this.take().unwrap_or_else(|| {
                let udp = self.inner.clone();
                let mut buf = buf.to_vec();
                Box::pin(async move {
                    let n = udp.send(&mut buf).await?;
                    std::io::Result::Ok(n)
                })
            });

            log::debug!("polling udp_send socket from smol");

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
                    let mut buf = Vec::with_capacity(buf.len());
                    buf.set_len(buf.len());
                    buf
                };
                Box::pin(async move {
                    let n = udp.recv(&mut buf).await?;
                    buf.truncate(n);
                
                    std::io::Result::Ok(buf)
                })
            });

            log::debug!("polling udp_recv socket from smol");

            match Pin::new(&mut fut).poll(cx) {
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => {
                    *this = Some(fut);
                    std::task::Poll::Pending
                }
                std::task::Poll::Ready(Ok(data)) => {
                    buf[..data.len()].copy_from_slice(&data);
                    log::debug!("receive from data {}", data.len());
                    std::task::Poll::Ready(Ok(data.len()))
                }
            }
        }
    }

    impl AsyncSendTo for UdpSocket {
        fn poll_sendto(
            &mut self,
            addr: &std::net::SocketAddr,
            buf: Vec<u8>,
        ) -> std::task::Poll<std::io::Result<usize>> {
            unimplemented!()
        }
    }

    struct KcpClientRuntime;

    struct KcpRunner;

    struct KcpTimer;

    impl poller::Timer for KcpTimer {
        fn sleep(&self, time: std::time::Duration) -> poller::BoxedFuture<()> {
            Box::pin(async move {
                // log::debug!("sleep {:?}", time);
                smol::Timer::after(time).await;
            })
        }
    }

    impl client::Runner for KcpRunner {
        type Err = io::Error;
        fn call(process: Processor) -> std::result::Result<(), Self::Err> {
            std::thread::Builder::new()
                .name("processor[kcp]".into())
                .spawn(move || {
                    smol::block_on(async move {
                        process.await.unwrap();
                        log::debug!("processor exit ..")
                    })
                })
                .unwrap();
            
            Ok(())
        }
    }

    impl client::KcpRuntime for KcpClientRuntime {
        type Err = io::Error;

        type Runner = KcpRunner;

        type Timer = KcpTimer;

        fn timer() -> Self::Timer {
            KcpTimer
        }
    }

    #[test]
    fn test_kcp_client() -> kcp::Result<()> {
        smol::block_on(async move {
            env_logger::builder()
                .filter_level(log::LevelFilter::Debug)
                .init();

            log::debug!("start kcp client");

            let udp = smol::net::UdpSocket::bind("0.0.0.0:0").await?;

            udp.connect("8.8.8.8:53").await?;

            let mut kcp_connector = KcpConnector::new::<KcpClientRuntime>(UdpSocket::new(udp))?;

            let mut kcp = kcp_connector.open().await?;

            let mut buf = [0u8; 21];

            let a = kcp.write(b"hello world").await.unwrap();

            log::debug!("send okay ...");

            // let a = kcp.read(&mut buf).await.unwrap();

            // kcp_connector.close().await;

            drop(kcp);

            smol::Timer::after(Duration::from_secs(60)).await;

            Ok(())
        })
    }

    #[test]
    fn test_kcp_server() {
        smol::block_on(async {
            let mut kcp_server = KcpListener::new();

            loop {}
        })
    }
}
