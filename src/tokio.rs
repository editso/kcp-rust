use std::{future::Future, pin::Pin, sync::Arc};

use tokio::io::ReadBuf;

use crate::{
    client, server, AsyncRead, AsyncRecv, AsyncRecvfrom, AsyncSend, AsyncSendTo, AsyncWrite,
    Background, Config, KcpRuntime, KcpStream, Runner, TaskKind, Timer,
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
        let f = || {
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
        };

        #[cfg(feature = "multi_thread")]
        std::thread::spawn(f);

        #[cfg(not(feature = "multi_thread"))]
        f()?;

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
