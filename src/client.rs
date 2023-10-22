use std::collections::HashMap;
use std::ffi::c_void;

use std::io;
use std::ops::Add;
use std::pin::Pin;
use std::sync::Arc;

use crate::poller::{self, KcpPoller, SafeKcp};
use crate::queue::{Queue, ReadHalf, WriteHalf};
use crate::r#async::{poll_fn, AsyncRead, AsyncRecv, AsyncSend, AsyncWrite};
use crate::signal::{KcpReadSig, KcpUpdateSig, SigWrite};
use crate::{kcp, signal, KcpRuntime, Processor, Runner};
use std::sync::Mutex;
use std::task::Poll;

use crate::{kcp::ConvAllocator, KcpStream};

#[derive(Clone, Debug, Default)]
struct KcpConv {
    conv: Arc<Mutex<u32>>,
}

struct ManagerImpl {
    snd_que: WriteHalf<Vec<u8>>,
    poller: poller::KcpPoller<KcpConv>,
    kcp_read_sig: Arc<SigWrite<KcpReadSig>>,
    kcp_update_sig: SigWrite<KcpUpdateSig>,
    sessions: HashMap<u32, KcpCore>,
}

#[derive(Clone)]
struct KcpCore(SafeKcp<KcpConv>);

#[derive(Clone)]
struct KcpManager(Arc<Mutex<ManagerImpl>>);

pub struct KcpConnector<IO: Clone> {
    io: IO,
    allocate: KcpConv,
    manager: KcpManager,
    poller: KcpPoller<KcpConv>,
}

pub struct ClientImpl {
    conv: u32,
    core: KcpCore,
    manager: KcpManager,
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
        let snd_que = Queue::<Vec<u8>>::new(1024).split();
        let poller = KcpPoller::new(Runtime::timer());

        let kcp_read_signal = signal::signal(10);
        let kcp_update_signal = signal::signal(10);

        let kcp_read_sig_0 = Arc::new(kcp_read_signal.0);

        let manager = KcpManager(Arc::new(Mutex::new(ManagerImpl {
            poller: poller.clone(),
            snd_que: snd_que.0,
            kcp_read_sig: kcp_read_sig_0.clone(),
            kcp_update_sig: kcp_update_signal.0,
            sessions: Default::default(),
        })));

        let processors = vec![
            Processor(Box::pin(Self::run_async_read(
                io.clone(),
                kcp_read_signal.1,
                manager.clone(),
            ))),
            Processor(Box::pin(poller::run_async_update(
                kcp_update_signal.1,
                poller.clone(),
            ))),
            Processor(Box::pin(Self::run_async_write(
                kcp_read_sig_0,
                snd_que.1,
                io.clone(),
            ))),
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

    pub async fn open(&mut self) -> kcp::Result<KcpStream<ClientImpl>> {
        extern "C" fn kcp_output_cb_impl(
            data: *const u8,
            len: i32,
            _: kcp::CB,
            user: *mut c_void,
        ) -> i32 {
            unsafe {
                Box::leak(Box::from_raw(user as *mut KcpManager)).output(
                    std::ptr::slice_from_raw_parts(data, len as usize)
                        .as_ref()
                        .unwrap(),
                )
            }
        }

        let kcp = kcp::Kcp::new::<KcpManager>(
            self.allocate.clone(),
            Some((
                Box::into_raw(Box::new(self.manager.clone())),
                Self::kcp_cleanup,
            )),
        )?;

        let conv = kcp.conv();

        kcp.set_output(kcp_output_cb_impl);

        let kcp = SafeKcp::wrap(kcp);

        self.poller.register(kcp.clone());

        let core = KcpCore::new(kcp);

        self.manager.manage(conv, core.clone()).await?;

        Ok(KcpStream(ClientImpl {
            conv,
            core,
            manager: self.manager.clone(),
        }))
    }

    fn kcp_cleanup(user: *mut c_void) {
        if !user.is_null() {
            unsafe { drop(Box::from_raw(user as *mut KcpManager)) }
        }
    }

    pub async fn close(&self) {
        let this = self.manager.0.lock().unwrap();
        drop(this.kcp_read_sig.send(KcpReadSig::Quit).await);
        drop(this.kcp_update_sig.send(KcpUpdateSig::Stop).await);
    }
}

impl<IO> KcpConnector<IO>
where
    IO: AsyncSend + Clone + Unpin,
{
    async fn run_async_write(
        sig: Arc<SigWrite<KcpReadSig>>,
        snd_que: ReadHalf<Vec<u8>>,
        mut io: IO,
    ) -> kcp::Result<()> {
        log::trace!("start kcp writer at {:?}", std::thread::current().name());
        loop {
            let data = snd_que.recv().await?;
            poll_fn(|cx| Pin::new(&mut io).poll_send(cx, &data)).await?;
        }
    }
}

impl<IO> KcpConnector<IO>
where
    IO: Send,
    IO: AsyncRecv + Clone + Unpin,
{
    async fn run_async_read(
        mut io: IO,
        signal: signal::SigRead<KcpReadSig>,
        manager: KcpManager,
    ) -> kcp::Result<()> {
        log::trace!("start kcp reader at {:?}", std::thread::current().name());

        let mut pause_read = true;

        let mut buf = {
            let mut buf = Vec::with_capacity(1500);
            unsafe {
                buf.set_len(1500);
            }
            buf
        };

        loop {
            let poll_recv = poll_fn(|cx| {
                if pause_read {
                    Poll::Pending
                } else {
                    Pin::new(&mut io).poll_recv(cx, &mut buf)
                }
            });

            let signal = signal::poll_signal_or(poll_recv, signal.recv()).await;

            match signal {
                signal::Sig::Signal(sig) => match sig? {
                    KcpReadSig::Quit => {
                        break Ok(());
                    }
                    KcpReadSig::Pause => {
                        pause_read = true;
                    }
                    KcpReadSig::Resume => {
                        log::trace!("resume kcp_read");
                        pause_read = false;
                    }
                },
                signal::Sig::Data(Err(e)) => {
                    if e.kind() == io::ErrorKind::ConnectionReset {
                        log::trace!("connection has been reset");
                    }

                    manager.close_all();
                    manager.stop_all_processor();
                    break Err(e.into());
                }
                signal::Sig::Data(Ok(n)) => {
                    let conv = kcp::Kcp::<KcpConv>::get_conv(&buf[..n]);
                    match manager.lookup(conv) {
                        None => {
                            log::trace!("kcp session not found. discard it conv: {}", conv);
                        }
                        Some(core) => {
                            if let Err(e) = core.input(&buf[..n]).await {
                                println!("{:?}", e);
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
        let mut conv = self.conv.lock().unwrap();
        *conv = conv.add(1);
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
    fn drop(&mut self) {
        let mgr = self.manager.0.lock().unwrap();

        mgr.snd_que.close();
        mgr.kcp_read_sig.close();
        mgr.kcp_update_sig.close();
    }
}

impl KcpManager {
    async fn manage(&self, conv: u32, kcp: KcpCore) -> kcp::Result<()> {
        let mut mgr = self.0.lock().unwrap();

        mgr.sessions.insert(conv, kcp);

        mgr.kcp_update_sig.send(KcpUpdateSig::Resume).await?;
        mgr.kcp_read_sig.send(KcpReadSig::Resume).await?;

        mgr.poller.wake();

        Ok(())
    }

    fn output(&self, data: &[u8]) -> i32 {
        let this = self.0.lock().unwrap();

        if let Err(e) = this.snd_que.block_send(data.to_vec()) {
            log::error!("{}", e);
            this.kcp_read_sig.close();
            this.kcp_update_sig.close();
        }

        data.len() as i32
    }

    fn remove(&self, conv: u32) {
        self.0
            .lock()
            .unwrap()
            .sessions
            .remove(&conv)
            .map(|kcp| kcp.0.force_close());
    }

    fn lookup(&self, conv: u32) -> Option<KcpCore> {
        self.0.lock().unwrap().sessions.get(&conv).map(Clone::clone)
    }

    fn close_all(&self) {
        let this = self.0.lock().unwrap();

        let count = this.sessions.len();

        for (_, kcp) in this.sessions.iter() {
            kcp.0.force_close();
        }

        log::trace!("{} sessions were forcibly closed", count)
    }

    fn stop_all_processor(&self) {
        let this = self.0.lock().unwrap();
        this.kcp_read_sig.close();
        this.kcp_update_sig.close();
    }
}

impl KcpCore {
    fn new(kcp: SafeKcp<KcpConv>) -> Self {
        KcpCore(kcp)
    }

    async fn input(&self, data: &[u8]) -> kcp::Result<()> {
        poll_fn(|cx| self.0.poll_input(cx, data)).await
    }

    fn poll_read(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.poll_recv(cx, buf)
    }

    fn poll_write(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.poll_send(cx, buf)
    }
}

impl Drop for ClientImpl {
    fn drop(&mut self) {
        log::trace!("clean session in KcpManager: conv = {}", self.conv);

        self.manager.remove(self.conv);
    }
}
