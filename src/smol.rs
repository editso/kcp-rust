use std::{future::Future, io, pin::Pin};

use crate::{
    client, server, AsyncRecv, AsyncRecvfrom, AsyncSend, AsyncSendTo, Config, KcpRuntime,
    KcpStream, Runner, Timer,
};

type BoxedFuture<O> = Pin<Box<dyn Future<Output = std::io::Result<O>>>>;

pub struct SmolUdpSocket {
    inner: smol::net::UdpSocket,
    recv_fut: Option<BoxedFuture<usize>>,
    send_fut: Option<BoxedFuture<usize>>,
    sendto_fut: Option<BoxedFuture<usize>>,
    recvfrom_fut: Option<BoxedFuture<(usize, std::net::SocketAddr)>>,
}

pub struct WithSmolRuntime;

impl AsyncRecv for SmolUdpSocket {
    fn poll_recv(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut fut = match self.recv_fut.take() {
            Some(fut) => fut,
            None => {
                let udp = self.inner.clone();
                let buf_len = buf.len();
                let buf_ptr = buf.as_mut_ptr();
                Box::pin(async move {
                    let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, buf_len) };
                    udp.recv(buf).await
                })
            }
        };

        match Pin::new(&mut fut).poll(cx)? {
            std::task::Poll::Ready(n) => std::task::Poll::Ready(Ok(n)),
            std::task::Poll::Pending => {
                self.recv_fut = Some(fut);
                std::task::Poll::Pending
            }
        }
    }
}

impl AsyncRecvfrom for SmolUdpSocket {
    fn poll_recvfrom(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<(std::net::SocketAddr, usize)>> {
        let mut fut = match self.recvfrom_fut.take() {
            Some(fut) => fut,
            None => {
                let udp = self.inner.clone();
                let buf_len = buf.len();
                let buf_ptr = buf.as_mut_ptr();
                Box::pin(async move {
                    let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, buf_len) };
                    udp.recv_from(buf).await
                })
            }
        };

        match Pin::new(&mut fut).poll(cx)? {
            std::task::Poll::Ready((n, addr)) => std::task::Poll::Ready(Ok((addr, n))),
            std::task::Poll::Pending => {
                self.recvfrom_fut = Some(fut);
                std::task::Poll::Pending
            }
        }
    }
}

impl AsyncSend for SmolUdpSocket {
    fn poll_send(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut fut = match self.send_fut.take() {
            Some(fut) => fut,
            None => {
                let udp = self.inner.clone();
                let buf_len = buf.len();
                let buf_ptr = buf.as_ptr();
                Box::pin(async move {
                    let buf = unsafe { std::slice::from_raw_parts(buf_ptr, buf_len) };
                    udp.send(buf).await
                })
            }
        };

        match Pin::new(&mut fut).poll(cx)? {
            std::task::Poll::Ready(n) => std::task::Poll::Ready(Ok(n)),
            std::task::Poll::Pending => {
                self.send_fut = Some(fut);
                std::task::Poll::Pending
            }
        }
    }
}

impl AsyncSendTo for SmolUdpSocket {
    fn poll_sendto(
        &mut self,
        cx: &mut std::task::Context<'_>,
        addr: &std::net::SocketAddr,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut fut = match self.sendto_fut.take() {
            Some(fut) => fut,
            None => {
                let udp = self.inner.clone();
                let buf_len = buf.len();
                let buf_ptr = buf.as_ptr();
                let addr_ptr = addr as *const std::net::SocketAddr;
                Box::pin(async move {
                    let buf = unsafe { std::slice::from_raw_parts(buf_ptr, buf_len) };
                    let addr = unsafe { addr_ptr.as_ref().unwrap() };
                    udp.send_to(buf, addr).await
                })
            }
        };

        match Pin::new(&mut fut).poll(cx)? {
            std::task::Poll::Ready(n) => std::task::Poll::Ready(Ok(n)),
            std::task::Poll::Pending => {
                self.sendto_fut = Some(fut);
                std::task::Poll::Pending
            }
        }
    }
}

impl KcpRuntime for WithSmolRuntime {
    type Err = std::io::Error;

    type Runner = Self;

    type Timer = Self;

    fn timer() -> Self::Timer {
        WithSmolRuntime
    }
}

impl Runner for WithSmolRuntime {
    type Err = std::io::Error;

    fn start(process: crate::Background) -> std::result::Result<(), Self::Err> {
        let f = || {
            let executor = smol::LocalExecutor::new();
            match smol::block_on(executor.run(process)) {
                Ok(_) => {
                    log::info!("kcp[background] finished")
                }
                Err(e) => {
                    log::warn!("kcp[background] error: {:?}", e)
                }
            }
        };

        #[cfg(feature = "multi_thread")]
        std::thread::spawn(f);

        #[cfg(not(feature = "multi_thread"))]
        f();

        Ok(())
    }
}


impl Timer for WithSmolRuntime {
    type Ret = std::time::Instant;
    type Output = smol::Timer;
    fn sleep(&self, time: std::time::Duration) -> Self::Output {
        smol::Timer::after(time)
    }
}


unsafe impl Send for SmolUdpSocket {}

impl Clone for SmolUdpSocket {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            recv_fut: None,
            send_fut: None,
            sendto_fut: None,
            recvfrom_fut: None,
        }
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


impl client::KcpConnector<SmolUdpSocket> {
    pub fn new_with_smol(
        io: smol::net::UdpSocket,
        config: Config,
    ) -> std::result::Result<Self, std::io::Error> {
        client::KcpConnector::new::<WithSmolRuntime>(
            SmolUdpSocket {
                inner: io,
                recv_fut: None,
                send_fut: None,
                sendto_fut: None,
                recvfrom_fut: None,
            },
            config,
        )
    }
}

impl server::KcpListener<SmolUdpSocket> {
    pub fn new_with_smol(
        io: smol::net::UdpSocket,
        config: Config,
    ) -> std::result::Result<Self, std::io::Error> {
        server::KcpListener::new::<WithSmolRuntime>(
            SmolUdpSocket {
                inner: io,
                recv_fut: None,
                send_fut: None,
                sendto_fut: None,
                recvfrom_fut: None,
            },
            config,
        )
    }
}