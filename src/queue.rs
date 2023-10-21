use std::{
    collections::VecDeque,
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

use crate::kcp;

pub struct QueueImpl<T: Unpin> {
    max: usize,
    queue: VecDeque<T>,
    closed: bool,
    recv_waker: Option<Waker>,
    send_waker: Option<Waker>,
}

pub struct Queue<T: Unpin>(Arc<Mutex<QueueImpl<T>>>);

pub struct QueueRead<'a, T: Unpin> {
    queue: &'a Arc<Mutex<QueueImpl<T>>>,
}

pub struct QueueSend<'a, T: Unpin> {
    data: Option<T>,
    queue: &'a Arc<Mutex<QueueImpl<T>>>,
}

pub struct ReadHalf<T: Unpin>(Queue<T>);

pub struct WriteHalf<T: Unpin>(Queue<T>);

impl<T> Queue<T>
where
    T: Unpin,
{
    pub fn new(max: usize) -> Queue<T> {
        Queue(Arc::new(Mutex::new(QueueImpl {
            max: max.min(1),
            closed: false,
            queue: Default::default(),
            recv_waker: Default::default(),
            send_waker: Default::default(),
        })))
    }

    pub fn recv<'a>(&'a self) -> QueueRead<'a, T> {
        QueueRead { queue: &self.0 }
    }

    pub fn send<'a>(&'a self, data: T) -> QueueSend<'a, T> {
        QueueSend {
            data: Some(data),
            queue: &self.0,
        }
    }

    pub fn split(self) -> (WriteHalf<T>, ReadHalf<T>) {
        (WriteHalf(self.clone()), ReadHalf(self))
    }

    pub fn block_send(&self, t: T) -> io::Result<()> {
        let mut this = self.0.lock().unwrap();

        if this.closed {
            this.wake_send();
            return Err(io::ErrorKind::Interrupted.into());
        } else {
            this.push_back(t);
            this.wake_recv();

            Ok(())
        }
    }

    pub fn close(&self) {
        self.0.lock().unwrap().close();
    }

    pub fn poll_recv(&self, cx: &mut std::task::Context<'_>) -> Poll<io::Result<T>> {
        let mut this = self.0.lock().unwrap();
        Pin::new(&mut *this).poll_read(cx)
    }

    pub fn poll_send(
        &self,
        cx: &mut std::task::Context<'_>,
        data: &mut Option<T>,
    ) -> Poll<io::Result<()>> {
        let mut this = self.0.lock().unwrap();
        Pin::new(&mut *this).poll_send(cx, data)
    }
}

impl<T> std::ops::Deref for QueueImpl<T>
where
    T: Unpin,
{
    type Target = VecDeque<T>;
    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

impl<T> std::ops::DerefMut for QueueImpl<T>
where
    T: Unpin,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.queue
    }
}

impl<T> QueueImpl<T>
where
    T: Unpin,
{
    fn filled(&self) -> bool {
        self.len() >= self.max
    }

    fn wake_recv(&mut self) {
        if let Some(waker) = self.recv_waker.take() {
            waker.wake();
        }
    }

    fn wake_send(&mut self) {
        if let Some(waker) = self.send_waker.take() {
            waker.wake();
        }
    }

    fn close(&mut self) {
        self.closed = true;
        self.wake_recv();
        self.wake_send();
    }

    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<io::Result<T>> {
        if self.closed {
            return Poll::Ready(Err(io::ErrorKind::Interrupted.into()));
        }

        match self.pop_front() {
            Some(data) => {
                let data = Poll::Ready(Ok(data));
                self.wake_send();
                data
            }
            None => {
                self.recv_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    fn poll_send(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        data: &mut Option<T>,
    ) -> Poll<io::Result<()>> {
        if self.closed {
            return Poll::Ready(Err(io::ErrorKind::Interrupted.into()));
        }

        if self.filled() {
            self.send_waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            self.push_back(data.take().unwrap());
            self.wake_recv();
            Poll::Ready(Ok(()))
        }
    }
}

impl<'a, T> Future for QueueRead<'a, T>
where
    T: Unpin,
{
    type Output = io::Result<T>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut que_impl = self.queue.lock().unwrap();
        QueueImpl::poll_read(Pin::new(&mut *que_impl), cx)
    }
}

impl<'a, T> Future for QueueSend<'a, T>
where
    T: Unpin,
{
    type Output = io::Result<()>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        let mut queue_impl = self.queue.lock().unwrap();
        QueueImpl::poll_send(Pin::new(&mut *queue_impl), cx, &mut self.data)
    }
}

impl<T: Unpin> Clone for Queue<T> {
    fn clone(&self) -> Self {
        Queue(self.0.clone())
    }
}

impl<T: Unpin> std::ops::Deref for ReadHalf<T> {
    type Target = Queue<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Unpin> std::ops::DerefMut for ReadHalf<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: Unpin> std::ops::Deref for WriteHalf<T> {
    type Target = Queue<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Unpin> std::ops::DerefMut for WriteHalf<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: Unpin> Drop for ReadHalf<T> {
    fn drop(&mut self) {
        log::debug!("close read");
        self.close();
    }
}

impl<T: Unpin> Drop for WriteHalf<T> {
    fn drop(&mut self) {
        log::debug!("close write");
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::Queue;

    #[test]
    fn test() {
        smol::block_on(async move {
            let queue = Queue::<usize>::new(1);
            let queue = Arc::new(queue);
            let (rx, ax) = (queue.clone(), queue);

            ax.send(1).await.unwrap();

            smol::spawn(async move {
                ax.send(2).await.unwrap();
            })
            .detach();

            smol::Timer::after(Duration::from_secs(3)).await;

            assert_eq!(1, rx.recv().await.unwrap());
            assert_eq!(2, rx.recv().await.unwrap());
        })
    }
}
