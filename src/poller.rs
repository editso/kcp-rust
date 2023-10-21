use std::{
    f32::consts::E,
    future::Future,
    ops::{DerefMut, Sub},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
    time::Duration,
};

use crate::kcp::{self, ConvAllocator, Kcp};

pub type BoxedFuture<O> = Pin<Box<dyn Future<Output = O> + Send + 'static>>;

pub struct KcpImpl<A: ConvAllocator> {
    inner: kcp::Kcp<A>,
    last_send: u32,
    last_recv: u32,
    next_update: u32,
}

#[derive(Clone)]
pub struct SafeKcp<A: ConvAllocator>(Arc<Mutex<KcpImpl<A>>>);

pub struct Sleep();

pub trait Timer {
    fn sleep(&self, time: std::time::Duration) -> BoxedFuture<()>;
}

pub struct PollerImpl<A: ConvAllocator> {
    timer: Box<dyn Timer + Send + 'static>,
    waker: Option<Waker>,
    session: Vec<SafeKcp<A>>,
    sleep_fut: Option<BoxedFuture<()>>,
}

#[derive(Clone)]
pub struct KcpPoller<A: ConvAllocator>(Arc<Mutex<PollerImpl<A>>>);

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

        Self(Arc::new(Mutex::new(KcpImpl {
            inner: kcp,
            next_update: now,
            last_recv: now,
            last_send: now,
        })))
    }

    pub fn waitsnd(&self) -> usize {
        let this = self.0.lock().unwrap();
        this.inner.waitsnd() as usize
    }

    pub fn send<P>(&self, pkt: P) -> kcp::Result<usize>
    where
        P: AsRef<[u8]>,
    {
        let this = self.0.lock().unwrap();
        this.inner.send(pkt)
    }

    pub fn check(&self, now: u32) -> u32 {
        let this = self.0.lock().unwrap();
        this.inner.check(now)
    }

    pub fn updatable(&self) -> bool {
        let now_time = self.check(now_mills());
        self.0.lock().unwrap().next_update <= now_time
    }

    pub fn sendable(&self) -> bool {
        self.0.lock().unwrap().inner.waitsnd() <= 0
    }

    pub fn writeable(self) -> bool {
        false
    }

    pub fn update(&mut self, now: u32) -> kcp::Result<()> {
        let mut kcp = self.0.lock().unwrap();

        if now - kcp.last_send > 10000 {
            Err(kcp::KcpError::WriteTimeout(now - kcp.last_send))
        } else {
            kcp.inner.update(now);
            Ok(())
        }
    }
}

impl<A: ConvAllocator> KcpPoller<A> {
    pub fn new<T>(timer: T) -> Self
    where
        T: Timer + Send + 'static,
    {
        KcpPoller(Arc::new(Mutex::new(PollerImpl {
            waker: None,
            timer: Box::new(timer),
            session: Default::default(),
            sleep_fut: None,
        })))
    }

    pub fn wake(&self) {
        if let Some(waker) = self.0.lock().unwrap().waker.take() {
            waker.wake();
        }
    }

    pub fn register(&self, kcp: SafeKcp<A>) {
        log::debug!("register kcp to poller");
        self.0.lock().unwrap().session.push(kcp);
        self.wake();
    }
}

impl<A: ConvAllocator> Future for KcpPoller<A> {
    type Output = kcp::Result<()>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut this = self.0.lock().unwrap();

        loop {
            if this.session.is_empty() {
                this.waker = Some(cx.waker().clone());
                break;
            }

            let mut fut = match this.sleep_fut.take() {
                Some(fut) => fut,
                None => {
                    let mut nex_chk = 0;

                    this.session.retain_mut(|kcp| {
                        let now = now_mills();

                        if kcp.updatable() {
                            if let Err(e) = kcp.update(now) {
                                // kcp.close();
                                log::debug!("update kcp error: {:?}", e);
                                return false;
                            }
                        }

                        nex_chk = if nex_chk == 0 {
                            kcp.check(now) - now
                        } else {
                            kcp.check(now).sub(now).min(nex_chk)
                        };

                        true
                    });

                    this.timer.sleep(Duration::from_millis(nex_chk as u64))
                }
            };

            match Pin::new(&mut fut).poll(cx) {
                std::task::Poll::Ready(()) => {
                    continue;
                }
                std::task::Poll::Pending => {
                    this.sleep_fut = Some(fut);
                    break;
                }
            }
        }

        std::task::Poll::Pending
    }
}
