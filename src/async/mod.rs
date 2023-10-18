use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

pub trait AsyncRead {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>>;
}

pub trait AsyncWrite {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>>;

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>>;

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>>;
}

pub trait AsyncSendTo {
    fn poll_sendto(
        &mut self,
        addr: &SocketAddr,
        buf: Vec<u8>,
    ) -> std::task::Poll<std::io::Result<usize>>;
}

pub trait AsyncRecvfrom {
    fn poll_recvfrom(&mut self) -> std::task::Poll<std::io::Result<(SocketAddr, Vec<u8>)>>;
}

pub trait AsyncSend {
    fn poll_send(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>>;
}

pub trait AsyncRecv {
    fn poll_recv(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>>;
}

#[pin_project::pin_project]
pub struct Read<'a, R: AsyncRead + Unpin> {
    #[pin]
    buf: &'a mut [u8],
    reader: &'a mut R,
}

#[pin_project::pin_project]
pub struct Write<'a, W: AsyncWrite + Unpin> {
    #[pin]
    buf: &'a [u8],
    writer: &'a mut W,
}

pub struct PollFn<F> {
    f: F,
}

pub trait AsyncReadExt: AsyncRead {
    fn read<'a>(&'a mut self, buf: &'a mut [u8]) -> Read<'a, Self>
    where
        Self: Unpin + Sized,
    {
        Read { buf, reader: self }
    }
}

pub trait AsyncWriteExt: AsyncWrite {
    fn write<'a>(&'a mut self, buf: &'a [u8]) -> Write<'a, Self>
    where
        Self: Unpin + Sized,
    {
        Write { buf, writer: self }
    }
}

impl<T> AsyncReadExt for T where T: AsyncRead {}
impl<T> AsyncWriteExt for T where T: AsyncWrite {}

impl<'a, R> Future for Read<'a, R>
where
    R: AsyncRead + Unpin,
{
    type Output = std::io::Result<usize>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> std::task::Poll<Self::Output> {
        let mut this = self.project();
        Pin::new(&mut **this.reader).poll_read(cx, &mut *this.buf)
    }
}

impl<'a, W> Future for Write<'a, W>
where
    W: AsyncWrite + Unpin,
{
    type Output = std::io::Result<usize>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> std::task::Poll<Self::Output> {
        let this = self.project();
        Pin::new(&mut **this.writer).poll_write(cx, &*this.buf)
    }
}

pub fn poll_fn<F, O>(f: F) -> PollFn<F>
where
    F: FnMut(&mut Context<'_>) -> Poll<O>,
{
    PollFn { f }
}

impl<F, O> Future for PollFn<F>
where
    F: FnMut(&mut Context<'_>) -> Poll<O> + Unpin,
    O: Unpin,
{
    type Output = O;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> std::task::Poll<Self::Output> {
        let f = &mut self.f;
        f(cx)
    }
}
