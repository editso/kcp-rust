use std::{
    collections::VecDeque,
    future::Future,
    io,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

pub struct QueueImpl<T: Unpin> {
    max: usize,
    queue: VecDeque<T>,
    recv_waker: Option<Waker>,
    send_waker: Option<Waker>,
}

#[derive(Clone)]
pub struct Queue<T: Unpin>(Arc<Mutex<QueueImpl<T>>>);

pub struct QueueRead<'a, T: Unpin> {
    queue: &'a Arc<Mutex<QueueImpl<T>>>,
}

pub struct QueueSend<'a, T: Unpin> {
    data: Option<T>,
    queue: &'a Arc<Mutex<QueueImpl<T>>>,
}

impl<T> Queue<T>
where
    T: Unpin,
{
    pub fn new(max: usize) -> Queue<T> {
        Queue(Arc::new(Mutex::new(QueueImpl {
            max: max.min(1),
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
        let mut queue_impl = self.queue.lock().unwrap();

        if let Some(waker) = queue_impl.recv_waker.take() {
            waker.wake();
        }

        match queue_impl.pop_front() {
            Some(data) => {
                let data = Poll::Ready(Ok(data));
                queue_impl.wake_send();
                data
            }
            None => {
                queue_impl.recv_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
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

        if let Some(waker) = queue_impl.send_waker.take() {
            waker.wake();
        }

        if queue_impl.filled() {
            queue_impl.send_waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            let data = self.data.take().unwrap();

            queue_impl.push_back(data);
            queue_impl.wake_recv();

            Poll::Ready(Ok(()))
        }
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
