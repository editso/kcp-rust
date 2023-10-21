use std::{
    f32::consts::E,
    future::Future,
    ops::DerefMut,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Waker,
};

use crate::kcp::{self, ConvAllocator, Kcp};

pub type BoxedFuture<O> = Pin<Box<dyn Future<Output = O> + Send + 'static>>;

#[derive(Clone)]
pub struct SafeKcp<A: ConvAllocator>(Arc<Mutex<kcp::Kcp<A>>>);

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

impl<A: ConvAllocator> SafeKcp<A> {
    pub fn update<F, O>(&self, f: F) -> kcp::Result<O>
    where
        F: FnOnce(&'_ mut kcp::Kcp<A>) -> O,
    {
        match self.0.lock() {
            Err(e) => Err(kcp::KcpError::from(e)),
            Ok(mut kcp) => Ok(f(&mut *kcp)),
        }
    }
}

impl<A: ConvAllocator> SafeKcp<A> {
    pub fn wrap(kcp: Kcp<A>) -> Self {
        Self(Arc::new(Mutex::new(kcp)))
    }

    pub fn waitsnd(&self) -> usize {
        let this = self.0.lock().unwrap();
        this.waitsnd() as usize
    }

    pub fn send<P>(&self, pkt: P) -> kcp::Result<usize>
    where
        P: AsRef<[u8]>,
    {
        self.update(|kcp| kcp.send(pkt))?
    }

    pub fn check(&self) -> u32 {
        let this = self.0.lock().unwrap();
        // this.check(current)
        todo!()
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

    pub fn wake(&mut self) {
        // if let Some(waker) = self.waker.take() {
        //     waker.wake();
        // }
    }

    pub fn register(&mut self, kcp: SafeKcp<A>) {
        log::debug!("register kcp to poller")
        // self.session.lock().unwrap().push(kcp);
        // self.wake();
    }
}

impl<A: ConvAllocator> Future for KcpPoller<A> {
    type Output = kcp::Result<()>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        loop {
            let this = self.0.lock().unwrap();

            break std::task::Poll::Pending;
        }
    }
}
