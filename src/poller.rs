use std::{
    f32::consts::E,
    future::Future,
    ops::DerefMut,
    sync::{Arc, Mutex},
    task::Waker,
};

use crate::kcp::{self, ConvAllocator};

pub struct SafeKcp<A: ConvAllocator>(Arc<Mutex<kcp::Kcp<A>>>);

pub struct KcpPoller<A: ConvAllocator> {
    waker: Option<Waker>,
    session: Arc<Mutex<Vec<SafeKcp<A>>>>,
}

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

impl<A: ConvAllocator> KcpPoller<A> {
    pub fn new() -> Self {
        Self {
            waker: None,
            session: Default::default(),
        }
    }

    pub fn wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    pub fn register(&self, kcp: SafeKcp<A>) {
        kcp.update(|kcp| {});

        self.session.lock().unwrap().push(kcp);
    }
}

impl<A: ConvAllocator> Future for KcpPoller<A> {
    type Output = kcp::Result<()>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        loop {
            let mut sessions = self.session.lock().unwrap();

            for session in sessions.iter_mut() {
                let nex = session.update(|kcp| kcp.check(0));
                match nex {
                    Ok(_) => {}
                    Err(e) => {}
                }
            }

            // self.waker = Some(cx.waker().clone());

            break std::task::Poll::Pending;
        }
    }
}
