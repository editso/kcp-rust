use std::io;
use std::task::Poll;
use std::{future::Future, pin::Pin, sync::Arc};

use std::sync::Mutex;

use crate::kcp;
use crate::{queue::Queue, r#async::poll_fn};

type BoxedFuture<'a, O, S> = Box<dyn Future<Output = Sig<O, S>> + Send + Unpin + 'a>;

pub struct Anyone<'a, O, S> {
    futures: Vec<BoxedFuture<'a, O, S>>,
}

pub enum Sig<T, S> {
    Data(T),
    Signal(S),
}

pub struct SignalImpl<T: Unpin> {
    que: Queue<T>,
    read_closed: bool,
    send_closed: bool,
}

#[allow(unused)]
pub enum KcpReadSig {
    Quit,
    Pause,
    Resume,
}

#[allow(unused)]
pub enum KcpUpdateSig {
    Pause,
    Stop,
    Resume,
}

pub struct SigRead<T: Unpin>(Arc<Mutex<SignalImpl<T>>>);

pub struct SigWrite<T: Unpin>(Arc<Mutex<SignalImpl<T>>>);

pub fn poll_signal_or<'a, Fut, FS, O, S>(fut: Fut, signal: FS) -> Anyone<'a, O, S>
where
    O: Unpin,
    S: Unpin,
    FS: Future<Output = S> + Send + 'a,
    Fut: Future<Output = O> + Send + 'a,
{
    let mut fut = Box::pin(fut);
    let mut signal = Box::pin(signal);

    let fut = poll_fn(move |cx| match Pin::new(&mut fut).poll(cx) {
        std::task::Poll::Ready(o) => std::task::Poll::Ready(Sig::Data(o)),
        std::task::Poll::Pending => std::task::Poll::Pending,
    });

    let signal = poll_fn(move |cx| match Pin::new(&mut signal).poll(cx) {
        std::task::Poll::Ready(o) => std::task::Poll::Ready(Sig::Signal(o)),
        std::task::Poll::Pending => std::task::Poll::Pending,
    });

    Anyone {
        futures: vec![Box::new(fut), Box::new(signal)],
    }
}

impl<'a, O, S> Future for Anyone<'a, O, S> {
    type Output = Sig<O, S>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        for future in &mut self.futures {
            match Pin::new(future).poll(cx) {
                std::task::Poll::Ready(o) => return std::task::Poll::Ready(o),
                std::task::Poll::Pending => continue,
            }
        }

        std::task::Poll::Pending
    }
}

pub fn signal<T: Unpin>(size: usize) -> (SigWrite<T>, SigRead<T>) {
    let signal = Arc::new(Mutex::new(SignalImpl {
        que: Queue::new(size),
        read_closed: false,
        send_closed: false,
    }));

    (SigWrite(signal.clone()), SigRead(signal))
}

impl<T: Unpin> SigWrite<T> {
    pub fn close(&self) {
        let mut this = self.0.lock().unwrap();
        this.send_closed = true;
        this.que.close();
    }

    pub async fn send(&self, data: T) -> kcp::Result<()> {
        let mut data = Some(data);
        poll_fn(|cx| {
            let mut this = self.0.lock().unwrap();
            if this.read_closed {
                Poll::Ready(Err(kcp::KcpError::SignalReadClosed))
            } else {
                match this.que.poll_send(cx, &mut data) {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(Ok(t)) => Poll::Ready(Ok(t)),
                    Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                        this.read_closed = true;
                        Poll::Ready(Err(kcp::KcpError::SignalReadClosed))
                    }
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),
                }
            }
        })
        .await
    }
}

impl<T: Unpin> SigRead<T> {
    pub fn close(&self) {
        let mut this = self.0.lock().unwrap();
        this.read_closed = true;
        this.que.close();
    }

    pub async fn recv(&self) -> kcp::Result<T> {
        poll_fn(|cx| {
            let mut this = self.0.lock().unwrap();
            if this.send_closed {
                Poll::Ready(Err(kcp::KcpError::SignalSendClosed))
            } else {
                match this.que.poll_recv(cx) {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(Ok(t)) => Poll::Ready(Ok(t)),
                    Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {
                        this.send_closed = true;
                        Poll::Ready(Err(kcp::KcpError::SignalReadClosed))
                    }
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),
                }
            }
        })
        .await
    }
}

impl<S: Unpin> Drop for SigWrite<S> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<S: Unpin> Drop for SigRead<S> {
    fn drop(&mut self) {
        self.close();
    }
}
