use std::collections::VecDeque;
use std::ffi::c_void;
use std::future::Future;

use std::pin::Pin;
use std::sync::{self, Arc, RwLock};

use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

use crate::poller::{self, KcpPoller, SafeKcp, Timer};
use crate::queue::Queue;
use crate::r#async::{poll_fn, AsyncRead, AsyncRecv, AsyncSend, AsyncWrite, PollFn};
use crate::{kcp, server, signal};

use crate::{
    kcp::{ConvAllocator, Kcp},
    KcpStream,
};

#[derive(Clone, Debug, Default)]
struct KcpConv {
    conv: Arc<Mutex<u32>>,
}

struct CoreImpl {
    kcp: SafeKcp<KcpConv>,
    send_waker: Option<Waker>,
    recv_waker: Option<Waker>,
    close_waker: Option<Waker>,
}

struct ManagerImpl {}

#[derive(Clone)]
struct KcpCore(Arc<Mutex<CoreImpl>>);

#[derive(Clone)]
struct KcpManager(Arc<Mutex<ManagerImpl>>);

pub struct Processor(Pin<Box<dyn Future<Output = kcp::Result<()>> + Send + 'static>>);

pub struct KcpConnector<IO: Clone> {
    io: IO,
    allocate: KcpConv,
    manager: KcpManager,
    poller: KcpPoller<KcpConv>,
}

pub struct ClientImpl {
    core: KcpCore,
}

pub trait Runner: Sized {
    type Err;
    fn call(process: Processor) -> std::result::Result<(), Self::Err>;
}

pub trait KcpRuntime: Sized {
    type Err;

    type Runner: Runner<Err = Self::Err>;

    type Timer: poller::Timer + Send + 'static;

    fn timer() -> Self::Timer;
}

impl<IO> KcpConnector<IO>
where
    IO: Clone + AsyncSend + AsyncRecv + Unpin,
    IO: Send + 'static,
{
    pub fn new<Runtime>(io: IO) -> std::result::Result<KcpConnector<IO>, Runtime::Err>
    where
        Runtime: KcpRuntime,
    {
        // let run_fn = Runtime::runner();

        let snd_que = Queue::<Vec<u8>>::new(1024);
        let poller = KcpPoller::new(Runtime::timer());

        let manager = KcpManager(Arc::new(Mutex::new(ManagerImpl {})));

        let processors = vec![
            Processor(Box::pin(Self::run_async_read(io.clone(), manager.clone()))),
            Processor(Box::pin(Self::run_async_update(poller.clone()))),
            Processor(Box::pin(Self::run_async_write(snd_que, io.clone()))),
        ];

        for process in processors {
            Runtime::Runner::call(process)?;
        }

        Ok(KcpConnector {
            io,
            manager,
            poller,
            allocate: KcpConv::default(),
        })
    }

    pub fn open(&mut self) -> kcp::Result<KcpStream<ClientImpl>> {
        extern "C" fn kcp_output_cb_impl(
            data: *const u8,
            len: i32,
            kcp: kcp::CB,
            user: *mut c_void,
        ) -> i32 {
            unimplemented!()
        }

        let kcp = kcp::Kcp::new::<KcpManager>(
            self.allocate.clone(),
            Some((
                Box::into_raw(Box::new(self.manager.clone())),
                Self::kcp_cleanup,
            )),
        )?;

        kcp.set_output(kcp_output_cb_impl);

        let kcp = SafeKcp::wrap(kcp);

        self.poller.register(kcp.clone());

        let core = KcpCore::new(kcp);

        self.manager.manage(core.clone());

        Ok(KcpStream(ClientImpl { core }))
    }

    fn kcp_cleanup(user: *mut c_void) {
        if !user.is_null() {
            unsafe { drop(Box::from_raw(user as *mut KcpManager)) }
        }
    }

    async fn run_async_update(poller: KcpPoller<KcpConv>) -> kcp::Result<()> {
        log::debug!("start kcp checker at {:?}", std::thread::current().id());
        poller.await
    }
}

impl<IO> KcpConnector<IO>
where
    IO: AsyncSend + Clone + Unpin,
{
    async fn run_async_write(snd_que: Queue<Vec<u8>>, mut io: IO) -> kcp::Result<()> {
        log::debug!("start kcp writer at {:?}", std::thread::current().name());
        loop {
            let data = snd_que.recv().await?;
            let _ = poll_fn(|cx| Pin::new(&mut io).poll_send(cx, &data)).await?;
        }
    }
}

impl<IO> KcpConnector<IO>
where
    IO: Send,
    IO: AsyncRecv + Clone + Unpin,
{
    async fn run_async_read(mut io: IO, manager: KcpManager) -> kcp::Result<()> {
        log::debug!("start kcp reader at {:?}", std::thread::current().name());

        let mut pending_read = true;

        let mut buf = {
            let mut buf = Vec::with_capacity(1024);
            unsafe {
                buf.set_len(1024);
            }
            buf
        };

        loop {
            let poll_recv = poll_fn(|cx| {
                if pending_read {
                    Poll::Pending
                } else {
                    Pin::new(&mut io).poll_recv(cx, &mut buf)
                }
            });

            let poll_signal = poll_fn(|cx| {
                if !pending_read {
                    Poll::Ready(kcp::Result::Ok(1))
                } else {
                    Poll::Pending
                }
            });

            let signal = signal::poll_signal_or(poll_recv, poll_signal).await;

            match signal {
                signal::Sig::Signal(sig) => {
                    let sig = sig?;
                    if sig == 1 {
                        pending_read = true;
                    }
                }
                signal::Sig::Data(data) => {
                    let n = data?;
                    let conv = kcp::Kcp::<KcpConv>::get_conv(&buf[..n]);

                    match manager.lookup(conv) {
                        None => {
                            println!("warn: kcp not found");
                        }
                        Some(core) => {
                            if let Err(e) = core.input(&buf[..n]).await {
                                println!("{:?}", e);
                                core.close();
                            }
                        }
                    }
                }
            }
        }
    }
}


impl ConvAllocator for KcpConv {
    fn allocate(&mut self) -> kcp::Result<kcp::CONV_T> {
        let conv = self.conv.lock().unwrap();
        Ok(*conv)
    }

    fn deallocate(&mut self, conv: kcp::CONV_T) {}
}

impl Processor {
    fn stop(&mut self) {}
}

impl AsyncWrite for ClientImpl {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.core.poll_write(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        unimplemented!()
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        unimplemented!()
    }
}

impl AsyncRead for ClientImpl {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.core.poll_read(cx, buf)
    }
}

impl<IO> Drop for KcpConnector<IO>
where
    IO: Clone,
{
    fn drop(&mut self) {}
}

impl KcpManager {
    fn manage(&self, kcp: KcpCore) {
        let core = kcp.0.lock().unwrap();
    }

    fn output(&self, data: &[u8]) -> i32 {
        0
    }

    fn lookup(&self, conv: u32) -> Option<KcpCore> {
        unimplemented!()
    }
}

impl KcpCore {
    fn new(kcp: SafeKcp<KcpConv>) -> Self {
        KcpCore(Arc::new(Mutex::new(CoreImpl {
            kcp,
            send_waker: Default::default(),
            recv_waker: Default::default(),
            close_waker: Default::default(),
        })))
    }

    fn close(&self) {}

    async fn input(&self, data: &[u8]) -> kcp::Result<()> {
        // self.0.lock().unwrap().kcp.input(data);
        Ok(())
    }

    fn poll_read(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let core = self.0.lock().unwrap();
        Poll::Pending
    }

    fn poll_write(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut this = self.0.lock().unwrap();

        if this.kcp.waitsnd() > 0 {
            this.send_waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(Ok(this.kcp.send(buf).unwrap()))
        }
    }
}

impl CoreImpl {
    fn try_wake_send(&mut self) {
        if let Some(waker) = self.send_waker.take() {
            waker.wake();
        }
    }

    fn try_wake_recv(&mut self) {
        if let Some(waker) = self.recv_waker.take() {
            waker.wake();
        }
    }

    fn try_wake_close(&mut self) {
        if let Some(waker) = self.close_waker.take() {
            waker.wake();
        }
    }
}

impl Future for Processor {
    type Output = kcp::Result<()>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        log::debug!("start kcp processor at {:?}", std::thread::current().id());
        Pin::new(&mut self.0).poll(cx)
    }
}
