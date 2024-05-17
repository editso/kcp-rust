use std::collections::HashMap;
use std::ffi::c_void;

use std::future::Future;
use std::io;
use std::marker::PhantomData;

use std::pin::Pin;
use std::sync::Arc;

use crate::poller::{self, KcpPoller, SafeKcp};
use crate::queue::{Queue, ReadHalf, WriteHalf};
use crate::r#async::{poll_fn, AsyncRead, AsyncRecv, AsyncSend, AsyncWrite};
use crate::signal::{KcpReadSig, KcpUpdateSig, SigWrite};
use crate::{kcp, signal, Background, Config, KcpRuntime, Runner};
use std::sync::Mutex;
use std::task::Poll;

use crate::{kcp::ConvAllocator, KcpStream};

struct KcpConv {
    conv: Arc<Mutex<u32>>,
    manager: KcpManager,
}

struct ManagerImpl {
    snd_que: WriteHalf<Vec<u8>>,
    poller: poller::KcpPoller<KcpConv>,
    kcp_closer: WriteHalf<u32>,
    kcp_read_sig: Arc<SigWrite<KcpReadSig>>,
    kcp_update_sig: Arc<SigWrite<KcpUpdateSig>>,
    sessions: Arc<Mutex<HashMap<u32, KcpCore>>>,
}

#[derive(Clone)]
struct KcpCore(SafeKcp<KcpConv>);

struct KcpManager(ManagerImpl);

pub struct KcpConnector<IO: Clone> {
    config: Config,
    allocate: KcpConv,
    manager: KcpManager,
    poller: KcpPoller<KcpConv>,
    _marked: PhantomData<IO>,
}

pub struct ClientImpl {
    conv: u32,
    core: KcpCore,
    manager: KcpManager,
}

unsafe impl<IO: Clone> Send for KcpConnector<IO> {}

impl<IO> KcpConnector<IO>
where
    IO: Clone + AsyncSend + AsyncRecv + Unpin,
    IO: Send + 'static,
{
    pub fn new<Runtime>(
        io: IO,
        config: Config,
    ) -> std::result::Result<KcpConnector<IO>, Runtime::Err>
    where
        Runtime: KcpRuntime,
    {
        let mut config = config;
        let snd_que = Queue::<Vec<u8>>::new(1024).split();
        let (poller, poller_fut) = KcpPoller::new(Runtime::timer());

        let kcp_read_signal = signal::signal(config.quebuf_size);
        let kcp_update_signal = signal::signal(255);
        let kcp_closer = Queue::new(255).split();
        let kcp_read_sig_0 = Arc::new(kcp_read_signal.0);

        let manager = KcpManager(ManagerImpl {
            poller: poller.clone(),
            snd_que: snd_que.0,
            kcp_closer: kcp_closer.0,
            kcp_read_sig: kcp_read_sig_0.clone(),
            kcp_update_sig: Arc::new(kcp_update_signal.0),
            sessions: Default::default(),
        });

        config.rcvbuf_size = if config.rcvbuf_size < config.kcp_config.mtu as usize {
            config.kcp_config.mtu as usize
        } else {
            config.rcvbuf_size
        };

        let processors = vec![
            Background::new_sender(Self::run_async_write(kcp_read_sig_0, snd_que.1, io.clone())),
            Background::new_closer(Self::run_async_close(kcp_closer.1, manager.clone())),
            Background::new_reader(Self::run_async_read(
                io.clone(),
                config,
                kcp_read_signal.1,
                manager.clone(),
            )),
            Background::new_poller(poller::run_async_update(
                poller.clone(),
                kcp_update_signal.1,
                poller_fut,
            )),
        ];

        for process in processors {
            let kind = process.kind();
            Runtime::Runner::start(process)?;
            log::trace!("kcp {:?} started", kind)
        }

        let conv_allocate = KcpConv {
            conv: Arc::new(Mutex::new(1)),
            manager: manager.clone(),
        };

        log::debug!("config: {:#?}", config);

        Ok(KcpConnector {
            manager,
            poller,
            config,
            allocate: conv_allocate,
            _marked: PhantomData,
        })
    }

    pub async fn open(&self) -> kcp::Result<(u32, KcpStream<ClientImpl>)> {
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

        let kcp_config = self.config.kcp_config;

        let kcp = kcp::Kcp::new::<KcpManager>(
            self.allocate.clone(),
            kcp_config,
            Some((
                Box::into_raw(Box::new(self.manager.clone())),
                Self::kcp_cleanup,
            )),
        )?;

        let conv = kcp.conv();

        kcp.set_output(kcp_output_cb_impl);

        let kcp = SafeKcp::wrap(kcp);

        self.poller.register(kcp.clone()).await?;

        let core = KcpCore(kcp);

        self.manager.manage(conv, core.clone()).await?;

        let kcp = KcpStream(ClientImpl {
            conv,
            core,
            manager: self.manager.clone(),
        });

        Ok((conv, kcp))
    }

    fn kcp_cleanup(user: *mut c_void) {
        if !user.is_null() {
            unsafe { drop(Box::from_raw(user as *mut KcpManager)) }
        }
    }

    pub async fn close(&self) {
        drop(self.manager.kcp_read_sig.send(KcpReadSig::Quit).await);
        drop(self.manager.kcp_update_sig.send(KcpUpdateSig::Stop).await);
    }
}

impl<IO> KcpConnector<IO>
where
    IO: AsyncSend + Clone + Unpin,
{
    async fn run_async_close(
        close_receiver: ReadHalf<u32>,
        manager: KcpManager,
    ) -> kcp::Result<()> {
        let mut futures = Vec::new();

        loop {
            let mut recv_close_fut = close_receiver.recv();
            let (clean_now, conv) = poll_fn(|cx| match Pin::new(&mut recv_close_fut).poll(cx)? {
                std::task::Poll::Ready(conv) => match manager.lookup(conv) {
                    None => Poll::Ready(kcp::Result::Ok((false, conv))),
                    Some(kcp) => {
                        futures.push(poll_fn(move |cx| match kcp.0.poll_close(cx) {
                            std::task::Poll::Pending => std::task::Poll::Pending,
                            std::task::Poll::Ready(_) => std::task::Poll::Ready(conv),
                        }));
                        Poll::Ready(Ok((false, conv)))
                    }
                },
                std::task::Poll::Pending => {
                    let mut poll = Poll::Pending;

                    futures.retain_mut(|future| match Pin::new(future).poll(cx) {
                        Poll::Pending => true,
                        Poll::Ready(conv) => {
                            poll = Poll::Ready(Ok((true, conv)));
                            false
                        }
                    });

                    poll
                }
            })
            .await?;

            if clean_now {
                log::trace!("clean kcp session: conv={}", conv);
                manager.remove_kcp(conv);
            }
        }
    }

    async fn run_async_write(
        _sig: Arc<SigWrite<KcpReadSig>>,
        snd_que: ReadHalf<Vec<u8>>,
        mut io: IO,
    ) -> kcp::Result<()> {
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
        io: IO,
        config: Config,
        signal: signal::SigRead<KcpReadSig>,
        manager: KcpManager,
    ) -> kcp::Result<()> {
        let mut io = io;
        let mut pause_read = true;

        let mut buf = {
            let mut buf = Vec::with_capacity(config.rcvbuf_size);
            unsafe {
                buf.set_len(config.rcvbuf_size);
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
                        pause_read = false;
                    }
                },
                signal::Sig::Data(Err(e)) => {
                    if e.kind() == io::ErrorKind::ConnectionReset {
                        log::trace!("connection has been reset");
                    }

                    manager.close_all_session();
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
                            if let Err(_) = poll_fn(|cx| core.0.poll_input(cx, &buf[..n])).await {
                                manager.remove_kcp(conv);
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
        let sessions = self.manager.sessions.lock().unwrap();

        let last_conv = *conv;

        while sessions.contains_key(&conv) {
            let (nex_conv, overflow) = conv.overflowing_add(1);
            *conv = if overflow { 1 } else { nex_conv };
            if *conv == last_conv {
                return Err(kcp::KcpError::NoMoreConv);
            }
        }

        Ok(*conv)
    }

    fn deallocate(&mut self, _conv: kcp::CONV_T) {}
}

impl AsyncWrite for ClientImpl {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.core.0.poll_send(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.core.0.poll_flush(cx)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.core.0.poll_close(cx)
    }
}

impl AsyncRead for ClientImpl {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.core.0.poll_recv(cx, buf)
    }
}

impl std::ops::Deref for KcpManager {
    type Target = ManagerImpl;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for KcpManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl KcpManager {
    async fn manage(&self, conv: u32, kcp: KcpCore) -> kcp::Result<()> {
        self.sessions.lock().unwrap().insert(conv, kcp);

        self.kcp_read_sig.send(KcpReadSig::Resume).await?;
        self.kcp_update_sig.send(KcpUpdateSig::Resume).await?;

        Ok(())
    }
}

impl KcpManager {
    fn output(&self, data: &[u8]) -> i32 {
        if let Err(e) = self.snd_que.block_send(data.to_vec()) {
            log::error!("{}", e);
            self.kcp_read_sig.close();
            self.kcp_update_sig.close();
        }

        data.len() as i32
    }

    fn remove_kcp(&self, conv: u32) {
        if let Some(kcp) = self.sessions.lock().unwrap().remove(&conv) {
            kcp.0.force_close()
        }
    }

    fn lookup(&self, conv: u32) -> Option<KcpCore> {
        self.sessions.lock().unwrap().get(&conv).map(Clone::clone)
    }

    fn close_kcp(&self, conv: u32) {
        if let Err(e) = self.kcp_closer.block_send(conv) {
            log::error!("{:?}", e);
        };
    }

    fn close_all_session(&self) {
        let sessions = self.sessions.lock().unwrap();

        let count = sessions.len();

        for (_, kcp) in sessions.iter() {
            kcp.0.force_close();
        }

        log::trace!("{} sessions were forcibly closed", count)
    }

    fn stop_all_processor(&self) {
        self.poller.stop();
        self.kcp_closer.close();
        self.kcp_read_sig.close();
        self.kcp_update_sig.close();
    }
}

impl Clone for KcpConv {
    fn clone(&self) -> Self {
        Self {
            conv: self.conv.clone(),
            manager: self.manager.clone(),
        }
    }
}

impl Clone for KcpManager {
    fn clone(&self) -> Self {
        KcpManager(ManagerImpl {
            snd_que: self.snd_que.clone(),
            poller: self.poller.clone(),
            kcp_closer: self.kcp_closer.clone(),
            kcp_read_sig: self.kcp_read_sig.clone(),
            kcp_update_sig: self.kcp_update_sig.clone(),
            sessions: self.sessions.clone(),
        })
    }
}

impl Drop for ClientImpl {
    fn drop(&mut self) {
        self.manager.close_kcp(self.conv);
    }
}

impl<IO> Drop for KcpConnector<IO>
where
    IO: Clone,
{
    fn drop(&mut self) {
        self.manager.close_all_session();
        self.manager.stop_all_processor();
    }
}
