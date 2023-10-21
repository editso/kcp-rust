use std::task::Waker;
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
    read: Option<Waker>,
    read_closed: bool,
    send_closed: bool,
}

pub struct KcpSignal<S: Unpin> {
    que: Queue<S>,
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
        futures: vec![Box::new(signal), Box::new(fut)],
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

pub fn signal<T: Unpin + Clone>(size: usize) -> (SigWrite<T>, SigRead<T>) {
    let signal = Arc::new(Mutex::new(SignalImpl {
        que: Queue::new(size),
        read: None,
        read_closed: false,
        send_closed: false,
    }));

    (SigWrite(signal.clone()), SigRead(signal))
}

impl<S: Unpin> Drop for SigWrite<S> {
    fn drop(&mut self) {
        let mut this = self.0.lock().unwrap();
        this.send_closed = true;
        if let Some(waker) = this.read.take() {
            waker.wake();
        }
    }
}

impl<S: Unpin> Drop for SigRead<S> {
    fn drop(&mut self) {
        let mut this = self.0.lock().unwrap();
        this.read_closed = true;
    }
}

impl<T: Unpin> SigWrite<T> {
    pub async fn send(&self, data: T) -> kcp::Result<()> {
        let mut this = self.0.lock().unwrap();
        if this.read_closed {
            Err(kcp::KcpError::SignalReadClosed)
        } else {
            this.que.send(data).await?;
            if let Some(reader) = this.read.take() {
                reader.wake();
            }
            Ok(())
        }
    }
}

impl<T: Unpin> SigRead<T> {
    pub async fn recv(&self) -> kcp::Result<T> {
        let this = self.0.lock().unwrap();
        if this.send_closed {
            Err(kcp::KcpError::SignalSendClosed)
        }else{
            let data = this.que.recv().await?;
            Ok(data)
        }
    }
}


#[cfg(test)]
mod tests{
    use std::{sync::mpsc::channel, ffi::c_void};

    use super::signal;


    #[test]
    fn test_signal(){
        smol::block_on(async move{

            let (ax, rx)= smol::channel::unbounded();


            let (cx, bx) = signal(10);

            // let (sw, sr) = signal(10);

            
            std::thread::spawn(move||{
                smol::block_on(async move{
                    // let sr = sr;
                    ax.send(1);
                    cx.send(1).await;
                    let k = 0;
                    unsafe{
                        let k = k as *const u8;
                        let a = std::ptr::slice_from_raw_parts(k, 10);
                        a.as_ref().unwrap_unchecked();
                    }
                })
                
            });

            loop {

                // if let Err(e) = bx.recv().await {
                //     println!("error 2222");
                //     break;
                // }


                if let Err(e) = rx.recv().await {
                    println!("error ....");
                    break;
                }
                
            }
            
            // let a = sw.send(1).await.unwrap();
        })
    }
}