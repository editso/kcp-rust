use std::collections::VecDeque;
use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use std::sync::Mutex;
use std::task::{Context, Poll};

use crate::poller::KcpPoller;
use crate::queue::Queue;
use crate::r#async::{poll_fn, AsyncRead, AsyncRecv, AsyncSend, AsyncWrite, PollFn};
use crate::{kcp, server};

use crate::{
    kcp::{ConvAllocator, Kcp},
    KcpStream,
};

#[derive(Clone)]
struct KcpConv {}

struct CoreImpl {
    kcp: Kcp<KcpConv>,
}

struct ManagerImpl {}

#[derive(Clone)]
struct KcpCore(Arc<Mutex<CoreImpl>>);

#[derive(Clone)]
struct KcpManager(Arc<Mutex<ManagerImpl>>);

struct Processor(Pin<Box<dyn Future<Output = kcp::Result<()>> + 'static>>);

pub struct KcpConnector<IO: Clone> {
    io: IO,
    allocate: KcpConv,
    manager: KcpManager,
    processors: Vec<Processor>,
}

pub struct ClientImpl<IO: Clone> {
    io: IO,
    core: KcpCore,
}


impl<IO> KcpConnector<IO>
where
    IO: Clone + AsyncSend + AsyncRecv + Unpin,
{
    pub fn new(io: IO) -> KcpConnector<IO> {
        let snd_que = Queue::<Vec<u8>>::new(1024);

        let manager = KcpManager(todo!());

        let processors = vec![
            Processor(Box::pin(Self::run_async_write(snd_que, io.clone()))),
            // Processor(Box::pin(Self::run_async_read(manager))),
            Processor(Box::pin(Self::run_async_update())),
        ];

        KcpConnector {
            io,
            manager,
            allocate: KcpConv {},
            processors,
        }
    }

    pub async fn open(&mut self) -> kcp::Result<KcpStream<ClientImpl<IO>>> {
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

        let core = KcpCore(Arc::new(Mutex::new(CoreImpl { kcp })));

        self.manager.manage(core.clone());

        Ok(KcpStream(ClientImpl {
            io: self.io.clone(),
            core,
        }))
    }

    fn kcp_cleanup(user: *mut c_void) {
        if !user.is_null() {
            unsafe { drop(Box::from_raw(user as *mut KcpManager)) }
        }
    }

    async fn run_async_update() -> kcp::Result<()> {
        let sessions = Vec::<KcpCore>::new();
        let poller = KcpPoller::<KcpConv>::new();
        Ok(())
    }
}



impl<IO> KcpConnector<IO>
where
    IO: AsyncSend + Clone + Unpin,
{
    async fn run_async_write(snd_que: Queue<Vec<u8>>, mut io: IO) -> kcp::Result<()> {
        loop {
            let data = snd_que.recv().await.unwrap();
            poll_fn(|cx| Pin::new(&mut io).poll_send(cx, &data))
                .await
                .unwrap();
        }
    }
}

impl<IO> KcpConnector<IO>
where
    IO: AsyncRecv + Clone + Unpin,
{
    async fn run_async_read(mut io: IO, manager: KcpManager) -> kcp::Result<()> {
        loop {
            let mut buf = {
                let mut buf = Vec::with_capacity(1024);
                unsafe {
                    buf.set_len(1024);
                }
                buf
            };

            let size = poll_fn(|cx| Pin::new(&mut io).poll_recv(cx, &mut buf))
                .await
                .unwrap();

            let conv = kcp::Kcp::<KcpConv>::get_conv(&buf[..size]);

            match manager.lookup(conv).input(&buf[..size]).await {
                Ok(e) => {}
                Err(e) => {}
            }
        }
    }
}


impl ConvAllocator for KcpConv {
    fn allocate(&mut self) -> kcp::Result<kcp::CONV_T> {
        todo!()
    }

    fn deallocate(&mut self, conv: kcp::CONV_T) {
        todo!()
    }
}

impl Processor {
    fn stop(&mut self) {}
}

impl<IO> AsyncWrite for ClientImpl<IO>
where
    IO: Clone + AsyncSend + Unpin,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        unimplemented!()
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

impl<IO> AsyncRead for ClientImpl<IO>
where
    IO: Clone + AsyncRecv + Unpin,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        unimplemented!()
    }
}

impl<IO> Drop for KcpConnector<IO>
where
    IO: Clone,
{
    fn drop(&mut self) {
        self.processors.retain_mut(|processor| {
            processor.stop();
            false
        })
    }
}

impl KcpManager {
    fn manage(&self, kcp: KcpCore) {
        let core = kcp.0.lock().unwrap();
    }

    fn output(&self, data: &[u8]) -> i32 {
        0
    }

    fn lookup(&self, conv: u32) -> KcpCore {
        unimplemented!()
    }
}

impl KcpCore {
    async fn input(&self, data: &[u8]) -> kcp::Result<()> {
        self.0.lock().unwrap().kcp.input(data);
        Ok(())
    }
}

impl Future for Processor {
    type Output = kcp::Result<()>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        loop {
            match Pin::new(&mut self.0).poll(cx) {
                Poll::Ready(_) => {}
                Poll::Pending => break std::task::Poll::Pending,
                Poll::Ready(Err(e)) => {
                    break Poll::Ready(Ok(()));
                }
            }
        }
    }
}
