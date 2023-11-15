use std::{future::Future, pin::Pin, sync::Arc};

use tokio::io::ReadBuf;

use crate::{
    client, server, AsyncRead, AsyncRecv, AsyncRecvfrom, AsyncSend, AsyncSendTo, AsyncWrite,
    Background, Config, KcpRuntime, KcpStream, Runner, Timer,
};

#[derive(Clone)]
pub struct TokioUdpSocket(pub Arc<tokio::net::UdpSocket>);

pub struct WithTokioRuntime;

type BoxedFuture<O> = Pin<Box<dyn Future<Output = O> + Send + 'static>>;

impl AsyncRecvfrom for TokioUdpSocket {
    fn poll_recvfrom(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<(std::net::SocketAddr, usize)>> {
        let mut buf = ReadBuf::new(buf);
        match self.0.poll_recv_from(cx, &mut buf)? {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(addr) => std::task::Poll::Ready(Ok((addr, buf.filled().len()))),
        }
    }
}

impl AsyncRecv for TokioUdpSocket {
    fn poll_recv(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut buf = ReadBuf::new(buf);
        match self.0.poll_recv(cx, &mut buf)? {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(()) => std::task::Poll::Ready(Ok(buf.filled().len())),
        }
    }
}

impl AsyncSendTo for TokioUdpSocket {
    fn poll_sendto(
        &mut self,
        cx: &mut std::task::Context<'_>,
        addr: &std::net::SocketAddr,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0.poll_send_to(cx, buf, addr.clone())
    }
}

impl AsyncSend for TokioUdpSocket {
    fn poll_send(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0.poll_send(cx, buf)
    }
}

impl KcpRuntime for WithTokioRuntime {
    type Err = std::io::Error;
    type Runner = Self;

    type Timer = Self;

    fn timer() -> Self::Timer {
        WithTokioRuntime
    }
}

impl Runner for WithTokioRuntime {
    type Err = std::io::Error;

    fn start(background: Background) -> std::result::Result<(), Self::Err> {
        let kind = background.kind();
        #[cfg(feature = "multi_thread")]
        std::thread::spawn(|| {
            || {
                let kind = background.kind();
                let mut runtime = tokio::runtime::Builder::new_current_thread();
                if kind == TaskKind::Closer {
                    &mut runtime
                } else {
                    runtime.event_interval(2).global_queue_interval(2)
                }
                .enable_all()
                .build()
                .unwrap()
                .block_on(background)
            }
        });

        #[cfg(not(feature = "multi_thread"))]
        tokio::spawn(async move {
            match background.await {
                Ok(_) => {
                    log::info!("kcp[background] finished")
                }
                Err(e) => {
                    log::warn!("error({:?}), stop {:?}", e, kind)
                }
            }
        });

        Ok(())
    }
}

impl Timer for WithTokioRuntime {
    type Ret = ();
    type Output = BoxedFuture<()>;

    fn sleep(&self, time: std::time::Duration) -> Self::Output {
        // log::debug!("sleep {:?}", time);
        Box::pin(tokio::time::sleep(time))
    }
}

impl<K> tokio::io::AsyncRead for KcpStream<K>
where
    K: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match AsyncRead::poll_read(Pin::new(&mut self.0), cx, buf.initialize_unfilled())? {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(n) => {
                buf.set_filled(n);
                std::task::Poll::Ready(Ok(()))
            }
        }
    }
}

impl<K> tokio::io::AsyncWrite for KcpStream<K>
where
    K: AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

impl client::KcpConnector<TokioUdpSocket> {
    pub fn new_with_tokio(
        io: tokio::net::UdpSocket,
        config: Config,
    ) -> std::result::Result<Self, std::io::Error> {
        client::KcpConnector::new::<WithTokioRuntime>(TokioUdpSocket(Arc::new(io)), config)
    }
}

impl server::KcpListener<TokioUdpSocket> {
    pub fn new_with_tokio(
        io: tokio::net::UdpSocket,
        config: Config,
    ) -> std::result::Result<Self, std::io::Error> {
        server::KcpListener::new::<WithTokioRuntime>(TokioUdpSocket(Arc::new(io)), config)
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use crate::{KcpConnector, KcpListener};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn test_tokio_runtime() {
        // env_logger::builder()
        //     .filter_level(log::LevelFilter::Trace)
        //     .init();

        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let server = tokio::net::UdpSocket::bind("127.0.0.1:9999").await;

                assert!(server.is_ok());

                let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await;

                assert!(client.is_ok());

                let client_fut = async move {
                    let client = client.unwrap();

                    tokio::time::sleep(Duration::from_secs(2)).await;

                    assert!(client.connect("127.0.0.1:9999").await.is_ok());

                    let result = KcpConnector::new_with_tokio(client, Default::default());

                    assert!(result.is_ok());

                    let mut kcp_connector = result.unwrap();

                    let result = kcp_connector.open().await;

                    assert!(result.is_ok());

                    let (_, mut kcp_stream) = result.unwrap();

                    assert!(kcp_stream.write(b"hello world").await.is_ok());

                    assert!(kcp_stream.shutdown().await.is_ok());
                };

                let server_fut = async move {
                    let server = server.unwrap();

                    let result = KcpListener::new_with_tokio(server, Default::default());

                    assert!(result.is_ok());

                    let kcp_listener = result.unwrap();

                    let result = kcp_listener.accept().await;

                    assert!(result.is_ok());

                    let (_, _, mut kcp_stream) = result.unwrap();

                    let mut s = [0u8; 1024];

                    let result = kcp_stream.read(&mut s).await;

                    assert!(result.is_ok());

                    let n = result.unwrap();

                    assert_eq!(n, 11);

                    String::from_utf8(s[..n].to_vec()).unwrap()
                };

                let (s, _) = tokio::join!(server_fut, client_fut);

                assert_eq!(s, "hello world");

                println!("message: {s}");
            })
    }
}
