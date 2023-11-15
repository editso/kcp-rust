use std::{
    any::Any,
    future::Future,
    io,
    ops::Sub,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
    time::Duration,
};

use crate::{
    kcp::{self, ConvAllocator, Kcp},
    queue::{Queue, ReadHalf, WriteHalf},
    r#async::poll_fn,
    signal::{poll_signal_or, KcpUpdateSig, Sig, SigRead},
};

pub type BoxedFuture<O> = Pin<Box<dyn Future<Output = O> + Send + 'static>>;

pub struct KcpImpl<A: ConvAllocator> {
    inner: kcp::Kcp<A>,
    last_send: u32,
    last_recv: u32,
    send_closed: bool,
    recv_closed: bool,
    last_update: u32,
    close_delay: u32,
    recv_timeout: u32,
    send_timeout: u32,
    next_update: u32,
    close_flag: u32,
    clsck_inc: u32,
    recv_waker: Option<Waker>,
    send_waker: Option<Waker>,
    close_waker: Option<Waker>,
    flush_waker: Option<Waker>,
}

pub struct SafeKcp<A: ConvAllocator>(Arc<Mutex<KcpImpl<A>>>);

enum GuardState {
    Stop(BoxedFuture<kcp::Result<()>>),
    Running(Pin<Box<dyn Future<Output = (BoxedFuture<kcp::Result<()>>, kcp::Result<()>)> + Send>>),
}

pub trait Timer {
    type Ret: Any;
    type Output: Future<Output = Self::Ret> + Send + Unpin + 'static;
    fn sleep(&self, time: std::time::Duration) -> Self::Output;
}

pub struct KcpPoller<A: ConvAllocator>(Arc<WriteHalf<SafeKcp<A>>>);

pub struct GuardedUpdate<A: ConvAllocator> {
    fut: GuardState,
    poller: KcpPoller<A>,
}

pub fn now_mills() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u32
}

impl<A: ConvAllocator> SafeKcp<A> {
    pub fn wrap(kcp: Kcp<A>) -> Self {
        let now = now_mills();

        kcp.update(now);

        let timeout = kcp.timeout();
        let close_delay = kcp.close_delay();

        Self(Arc::new(Mutex::new(KcpImpl {
            inner: kcp,
            close_delay,
            last_recv: now,
            last_send: now,
            recv_waker: None,
            send_waker: None,
            close_waker: None,
            flush_waker: None,
            last_update: now,
            send_closed: false,
            recv_closed: false,
            recv_timeout: timeout,
            send_timeout: timeout,
            next_update: now,
            close_flag: 0,
            clsck_inc: 0,
        })))
    }

    pub fn try_wake_send(&self) {
        if let Some(waker) = self.0.lock().unwrap().send_waker.take() {
            waker.wake()
        }
    }

    pub fn try_wake_recv(&self) {
        if let Some(waker) = self.0.lock().unwrap().recv_waker.take() {
            waker.wake();
        }
    }

    pub fn try_wake_flush(&self) {
        if let Some(waker) = self.0.lock().unwrap().flush_waker.take() {
            waker.wake();
        }
    }

    pub fn try_wake_close(&self) {
        if let Some(waker) = self.0.lock().unwrap().close_waker.take() {
            waker.wake();
        }
    }

    pub fn try_wake_all(&self) {
        self.try_wake_send();
        self.try_wake_recv();
        self.try_wake_flush();
        self.try_wake_close();
    }

    pub fn check(&self, now: u32) -> u32 {
        let mut this = self.0.lock().unwrap();

        this.next_update = this.inner.check(now);

        if this.close_flag == 1 {
            // close it slowly 10ms
            this.clsck_inc += 10;
        }

        this.next_update + this.clsck_inc
    }

    pub fn closed(&self) -> bool {
        let this = self.0.lock().unwrap();
        this.recv_closed && this.send_closed
    }

    pub fn updatable(&self, now: u32) -> bool {
        if self.closed() {
            true
        } else {
            let now_time = self.check(now);
            self.0.lock().unwrap().last_update <= now_time
        }
    }

    pub fn force_close(&self) {
        if self.closed() {
            return;
        }

        {
            let mut this = self.0.lock().unwrap();

            this.send_closed = true;
            this.recv_closed = true;
        }

        self.try_wake_all();
    }

    pub fn update(&mut self, now: u32) -> kcp::Result<()> {
        if self.closed() {
            return Err(kcp::KcpError::Closed);
        }

        let mut this = self.0.lock().unwrap();
        let (diff, overflow) = now.overflowing_sub(this.last_send);

        if !overflow {
            if this.close_waker.is_some() && !overflow && diff > this.close_delay {
                this.recv_closed = true;
                this.send_closed = true;
                if let Some(waker) = this.close_waker.take() {
                    drop(this);
                    waker.wake();
                }
                return Err(kcp::KcpError::Closed);
            }

            if this.recv_waker.is_some() && diff > this.recv_timeout {
                this.recv_closed = true;
                if let Some(waker) = this.recv_waker.take() {
                    drop(this);
                    waker.wake();
                }
                return Err(kcp::KcpError::ReadTimeout(diff));
            }

            if this.send_waker.is_some() && diff > this.send_timeout {
                this.recv_closed = true;
                if let Some(waker) = this.send_waker.take() {
                    drop(this);
                    waker.wake();
                }
                return Err(kcp::KcpError::WriteTimeout(diff));
            }

            if this.flush_waker.is_some() && diff > this.send_timeout {
                this.recv_closed = true;
                if let Some(waker) = this.flush_waker.take() {
                    drop(this);
                    waker.wake();
                }
                return Err(kcp::KcpError::WriteTimeout(diff));
            }
        }

        this.inner.update(now);

        Ok(())
    }
}

impl<A: ConvAllocator> SafeKcp<A> {
    pub fn poll_input<P>(&self, _: &mut std::task::Context<'_>, pkt: P) -> Poll<kcp::Result<()>>
    where
        P: AsRef<[u8]>,
    {
        let retval = {
            let mut this = self.0.lock().unwrap();
            match this.inner.input(pkt) {
                Ok(()) => {
                    this.clsck_inc = 0;
                    this.last_update = 0;
                    this.last_send = now_mills();
                    Ok(())
                }
                Err(e) => {
                    this.recv_closed = true;
                    Err(e)
                }
            }
        };

        self.try_wake_all();

        Poll::Ready(retval)
    }

    pub fn poll_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.closed() {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }

        let mut this = self.0.lock().unwrap();

        if this.send_closed {
            Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
        } else if this.inner.waitsnd() >= this.inner.sndwnd_size() as u32 {
            this.send_waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(this.inner.send(buf).map_err(Into::into))
        }
    }

    pub fn poll_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.closed() {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }

        let poll = {
            let mut this = self.0.lock().unwrap();
            if this.recv_closed {
                Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()))
            } else if this.inner.peeksize() < 0 {
                this.recv_waker = Some(cx.waker().clone());
                Poll::Pending
            } else {
                this.last_recv = now_mills();
                Poll::Ready(this.inner.recv(buf).map_err(Into::into))
            }
        };

        self.try_wake_send();

        poll
    }

    pub fn poll_flush(&self, cx: &mut std::task::Context<'_>) -> Poll<io::Result<()>> {
        if self.closed() {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }

        let mut this = self.0.lock().unwrap();

        if this.inner.waitsnd() > 0 {
            this.flush_waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    pub fn poll_close(&self, cx: &mut std::task::Context<'_>) -> Poll<io::Result<()>> {
        if self.closed() {
            return Poll::Ready(Ok(()));
        }

        let mut this = self.0.lock().unwrap();
        let (diff, overflow) = now_mills().overflowing_sub(this.last_send);

        if this.inner.waitsnd() > 0 || overflow || diff < this.close_delay {
            this.close_flag = 1;
            this.close_waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
}

impl<A: ConvAllocator> KcpPoller<A>
where
    A: Send + 'static,
{
    pub fn new<T>(timer: T) -> (Self, BoxedFuture<kcp::Result<()>>)
    where
        T: Timer + Send + Sync + 'static,
    {
        let (rx, ax) = Queue::new(10).split();
        let poller = KcpPoller(Arc::new(rx));
        (poller, Box::pin(self::poller_main(timer, ax)))
    }
}

impl<A: ConvAllocator> KcpPoller<A> {
    pub fn stop(&self) {
        self.0.close();
    }

    pub async fn register(&self, kcp: SafeKcp<A>) -> io::Result<()> {
        self.0.send(kcp).await
    }
}

async fn poller_main<A, T>(timer: T, ax: ReadHalf<SafeKcp<A>>) -> kcp::Result<()>
where
    A: ConvAllocator,
    T: Timer + Send + 'static,
{
    let mut sessions: Vec<SafeKcp<A>> = Vec::new();
    let mut sleep_fut: Option<T::Output> = None;

    let mut infallible = kcp::Result::Ok(());

    while infallible.is_ok() {
        let mut recv_fut = ax.recv();

        infallible = poll_fn(|cx| match Pin::new(&mut recv_fut).poll(cx)? {
            Poll::Ready(kcp) => {
                sessions.push(kcp);
                Poll::Ready(kcp::Result::Ok(()))
            }
            Poll::Pending => match sleep_fut.take() {
                None => {
                    if sessions.is_empty() {
                        Poll::Pending
                    } else {
                        let nex_chk = self::poll_update(cx, &mut sessions);
                        sleep_fut = Some(timer.sleep(nex_chk));
                        Poll::Ready(Ok(()))
                    }
                }
                Some(mut fut) => match Pin::new(&mut fut).poll(cx) {
                    Poll::Ready(_) => Poll::Ready(Ok(())),
                    Poll::Pending => {
                        sleep_fut = Some(fut);
                        Poll::Pending
                    }
                },
            },
        })
        .await;
    }

    for session in sessions {
        session.force_close();
    }

    infallible
}

pub fn run_async_update<A: ConvAllocator + 'static>(
    poller: KcpPoller<A>,
    signal: SigRead<KcpUpdateSig>,
    poller_fut: BoxedFuture<kcp::Result<()>>,
) -> GuardedUpdate<A> {
    GuardedUpdate {
        poller,
        fut: GuardState::Running(Box::pin(self::run_async_update_internal::<A>(
            signal, poller_fut,
        ))),
    }
}

pub async fn run_async_update_internal<A: ConvAllocator>(
    signal: SigRead<KcpUpdateSig>,
    poller_fut: BoxedFuture<kcp::Result<()>>,
) -> (BoxedFuture<kcp::Result<()>>, kcp::Result<()>) {
    let mut poller_fut = poller_fut;
    let mut pause_poller = true;

    loop {
        let poller_fn = poll_fn(|cx| {
            if pause_poller {
                Poll::Pending
            } else {
                Pin::new(&mut poller_fut).poll(cx)
            }
        });

        match poll_signal_or(poller_fn, signal.recv()).await {
            Sig::Data(result) => {
                log::trace!("poller finished");
                break (poller_fut, result);
            }
            Sig::Signal(sig) => match sig {
                Err(e) => {
                    log::debug!("kcp_update signal error {:?}", e);
                    break (poller_fut, Err(e));
                }
                Ok(KcpUpdateSig::Pause) => {
                    pause_poller = true;
                }
                Ok(KcpUpdateSig::Resume) => {
                    pause_poller = false;
                }
                Ok(KcpUpdateSig::Stop) => {
                    log::debug!("stop kcp poller");
                    break (poller_fut, Ok(()));
                }
            },
        }
    }
}

fn poll_update<A>(_cx: &mut std::task::Context<'_>, sessions: &mut Vec<SafeKcp<A>>) -> Duration
where
    A: ConvAllocator,
{
    let mut nex_chk = 0;

    sessions.retain_mut(|kcp| {
        let now = now_mills();

        if kcp.closed() {
            return false;
        }

        if kcp.updatable(now) {
            if let Err(_) = kcp.update(now) {
                kcp.force_close();
                return false;
            } else {
                kcp.try_wake_all();
            }
        }

        nex_chk = if nex_chk == 0 {
            kcp.check(now) - now
        } else {
            kcp.check(now).sub(now).min(nex_chk)
        };

        true
    });

    Duration::from_millis(nex_chk as u64)
}

impl<A: ConvAllocator> Clone for SafeKcp<A> {
    fn clone(&self) -> Self {
        SafeKcp(self.0.clone())
    }
}

impl<A: ConvAllocator> Clone for KcpPoller<A> {
    fn clone(&self) -> Self {
        KcpPoller(self.0.clone())
    }
}

impl<A: ConvAllocator> Future for GuardedUpdate<A> {
    type Output = kcp::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match &mut self.fut {
            GuardState::Stop(fut) => Pin::new(fut).poll(cx),
            GuardState::Running(fut) => match Pin::new(fut).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready((mut fut, result)) => {
                    self.poller.stop();
                    match Pin::new(&mut fut).poll(cx) {
                        Poll::Pending => {
                            drop(std::mem::replace(&mut self.fut, GuardState::Stop(fut)));
                            Poll::Pending
                        }
                        Poll::Ready(r) => {
                            if result.is_err() {
                                Poll::Ready(result)
                            } else {
                                Poll::Ready(r)
                            }
                        }
                    }
                }
            },
        }
    }
}

impl<A: ConvAllocator> Drop for GuardedUpdate<A> {
    fn drop(&mut self) {
        self.poller.stop();
        // TODO wake all future
    }
}
