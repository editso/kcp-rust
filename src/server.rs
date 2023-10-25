use std::collections::hash_map::DefaultHasher;
use std::ffi::c_void;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::io;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::task::Poll;
use std::{collections::HashMap, pin::Pin, sync::Arc};

use crate::signal::KcpUpdateSig;
use crate::{poller, Background, Config, KcpConfig, Runner};

use crate::{
    kcp::{self, ConvAllocator},
    poller::{KcpPoller, SafeKcp},
    queue::{Queue, ReadHalf, WriteHalf},
    r#async::{poll_fn, AsyncRead, AsyncRecvfrom, AsyncSendTo, AsyncWrite},
    signal::{signal, SigWrite},
    KcpRuntime, KcpStream,
};

struct RemoteConv(u32);

struct ManagerImpl {
    sessions: HashMap<u64, HashMap<u32, KcpCore>>,
}

#[derive(Clone)]
struct KcpManager {
    inner: Arc<Mutex<ManagerImpl>>,
    poller: KcpPoller<RemoteConv>,
    poller_sig: Arc<SigWrite<KcpUpdateSig>>,
    kcp_closer: Arc<WriteHalf<(u64, u32)>>,
    kcp_sender: Arc<WriteHalf<(SocketAddr, Vec<u8>)>>,
}

pub struct KcpListener<IO> {
    manager: KcpManager,
    kcp_receiver: ReadHalf<(u32, SocketAddr, KcpStream<ServerImpl>)>,
    _marked: PhantomData<IO>,
}

#[derive(Clone)]
struct KcpCore {
    kcp: SafeKcp<RemoteConv>,
}

pub struct ServerImpl {
    kcp: KcpCore,
    id: u64,
    conv: u32,
    #[allow(unused)]
    addr: SocketAddr,
    manager: KcpManager,
}

pub struct KcpOutput {
    to: SocketAddr,
    manager: KcpManager,
}

unsafe impl<IO> Send for KcpListener<IO> {}

impl<IO> KcpListener<IO>
where
    IO: AsyncRecvfrom + AsyncSendTo + Send + Unpin + Clone + 'static,
{
    pub fn new<R>(io: IO, config: Config) -> std::result::Result<Self, R::Err>
    where
        R: KcpRuntime,
    {
        let mut config = config;
        let sessions = HashMap::new();
        let (kcp_poller, poller_fut) = KcpPoller::new(R::timer());
        let acceptor = Queue::new(10).split();
        let kcp_closer = Queue::new(255).split();
        let kcp_update_sig = signal(255);
        let kcp_sender = Queue::new(config.quebuf_size).split();

        let kcp_manager = KcpManager {
            poller: kcp_poller.clone(),
            kcp_closer: Arc::new(kcp_closer.0),
            poller_sig: Arc::new(kcp_update_sig.0),
            inner: Arc::new(Mutex::new(ManagerImpl { sessions })),
            kcp_sender: Arc::new(kcp_sender.0),
        };

        config.rcvbuf_size = if config.rcvbuf_size < config.kcp_config.mtu as usize {
            config.kcp_config.mtu as usize
        } else {
            config.rcvbuf_size
        };

        let processors = vec![
            Background::new_poller(poller::run_async_update(kcp_update_sig.1, poller_fut)),
            Background::new_sender(Self::kcp_async_send(io.clone(), kcp_sender.1)),
            Background::new_closer(Self::kcp_async_close(kcp_closer.1, kcp_manager.clone())),
            Background::new_reader(Self::kcp_async_recv(
                io.clone(),
                config,
                kcp_manager.clone(),
                acceptor.0,
            )),
        ];

        for process in processors {
            let kind = process.kind();
            R::Runner::start(process)?;
            log::trace!("kcp {:?} started", kind)
        }

        log::debug!("config: {:#?}", config);

        Ok(KcpListener {
            manager: kcp_manager,
            kcp_receiver: acceptor.1,
            _marked: PhantomData,
        })
    }

    pub async fn accept(&self) -> kcp::Result<(u32, SocketAddr, KcpStream<ServerImpl>)> {
        Ok(self.kcp_receiver.recv().await?)
    }
}

impl ConvAllocator for RemoteConv {
    fn allocate(&mut self) -> kcp::Result<kcp::CONV_T> {
        Ok(self.0)
    }

    fn deallocate(&mut self, _: kcp::CONV_T) {}
}

impl<IO> KcpListener<IO>
where
    IO: AsyncRecvfrom + AsyncSendTo + Send + Unpin + 'static,
{
    async fn kcp_async_close(
        close_receiver: ReadHalf<(u64, u32)>,
        manager: KcpManager,
    ) -> kcp::Result<()> {
        let mut futures = Vec::new();

        loop {
            let mut recv_close_fut = close_receiver.recv();
            let (clean_now, id, conv) =
                poll_fn(|cx| match Pin::new(&mut recv_close_fut).poll(cx)? {
                    std::task::Poll::Ready((id, conv)) => match manager.lookup(id, conv) {
                        None => Poll::Ready(kcp::Result::Ok((false, id, conv))),
                        Some(kcp) => {
                            futures.push(poll_fn(move |cx| match kcp.poll_close(cx) {
                                std::task::Poll::Pending => std::task::Poll::Pending,
                                std::task::Poll::Ready(_) => std::task::Poll::Ready((id, conv)),
                            }));

                            Poll::Ready(Ok((false, id, conv)))
                        }
                    },
                    std::task::Poll::Pending => {
                        let mut poll = Poll::Pending;

                        futures.retain_mut(|future| match Pin::new(future).poll(cx) {
                            Poll::Pending => true,
                            Poll::Ready((id, conv)) => {
                                poll = Poll::Ready(Ok((true, id, conv)));
                                false
                            }
                        });

                        poll
                    }
                })
                .await?;

            if clean_now {
                log::trace!("clean kcp session: id={}, conv={}", id, conv);
                manager.remove(id, conv);
            }
        }
    }

    async fn kcp_async_send(
        mut io: IO,
        data_receiver: ReadHalf<(SocketAddr, Vec<u8>)>,
    ) -> kcp::Result<()> {
        loop {
            let (addr, data) = data_receiver.recv().await?;
            poll_fn(|cx| Pin::new(&mut io).poll_sendto(cx, &addr, &data)).await?;
        }
    }

    async fn kcp_async_recv(
        io: IO,
        config: Config,
        manager: KcpManager,
        acc_receiver: WriteHalf<(u32, SocketAddr, KcpStream<ServerImpl>)>,
    ) -> kcp::Result<()> {
        let mut io = io;
        let mut buf = unsafe {
            let mut buf = Vec::with_capacity(config.rcvbuf_size);
            buf.set_len(config.rcvbuf_size);
            buf
        };

        manager.poller_sig.send(KcpUpdateSig::Resume).await?;

        loop {
            let (addr, n) = match poll_fn(|cx| Pin::new(&mut io).poll_recvfrom(cx, &mut buf)).await
            {
                Ok(r) => r,
                Err(e) => {
                    if e.kind() == io::ErrorKind::ConnectionReset {
                        log::warn!("connection has been reset");
                    }
                    continue;
                }
            };

            let conv = kcp::Kcp::<RemoteConv>::get_conv(&buf);

            let hash_id = {
                let mut hasher = DefaultHasher::new();
                addr.hash(&mut hasher);
                hasher.finish()
            };

            match manager.lookup(hash_id, conv) {
                Some(kcp) => {
                    if let Err(e) = poll_fn(|cx| kcp.poll_input(cx, &buf[..n])).await {
                        log::warn!("call kcp input: {:?}", e);
                        kcp.force_close();
                    };
                }
                None => {
                    let kcp = manager.make_kcp(config.kcp_config, addr, conv).unwrap();

                    match poll_fn(|cx| kcp.poll_input(cx, &buf[..n])).await {
                        Err(e) => {
                            log::warn!("call kcp input {:?}", e);
                            kcp.force_close();
                        }
                        Ok(()) => {
                            manager.manage(hash_id, conv, kcp.clone()).await?;

                            let kcp = KcpStream(ServerImpl {
                                id: hash_id,
                                kcp,
                                addr,
                                conv,
                                manager: manager.clone(),
                            });

                            acc_receiver.send((conv, addr, kcp)).await?;
                        }
                    }
                }
            };
        }
    }
}

impl std::ops::Deref for KcpCore {
    type Target = SafeKcp<RemoteConv>;
    fn deref(&self) -> &Self::Target {
        &self.kcp
    }
}

impl std::ops::DerefMut for KcpCore {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.kcp
    }
}

impl KcpManager {
    async fn manage(&self, id: u64, conv: u32, kcp: KcpCore) -> std::io::Result<()> {
        self.poller.register(kcp.kcp.clone()).await?;

        self.inner
            .lock()
            .unwrap()
            .sessions
            .entry(id)
            .or_default()
            .insert(conv, kcp);
        Ok(())
    }

    fn remove(&self, id: u64, conv: u32) {
        if let Some(kcp) = self
            .inner
            .lock()
            .unwrap()
            .sessions
            .entry(id)
            .or_default()
            .remove(&conv)
        {
            kcp.force_close()
        }
    }

    fn lookup(&self, id: u64, conv: u32) -> Option<KcpCore> {
        self.inner
            .lock()
            .unwrap()
            .sessions
            .entry(id)
            .or_default()
            .get(&conv)
            .map(Clone::clone)
    }

    fn close_kcp(&self, id: u64, conv: u32) {
        if let Err(e) = self.kcp_closer.block_send((id, conv)) {
            log::error!("{:?}", e);
        }
    }

    fn make_kcp(&self, kcp_config: KcpConfig, addr: SocketAddr, conv: u32) -> kcp::Result<KcpCore> {
        extern "C" fn kcp_output_cb_impl(
            data: *const u8,
            len: i32,
            _: kcp::CB,
            user: *mut c_void,
        ) -> i32 {
            unsafe {
                Box::leak(Box::<KcpOutput>::from_raw(user as *mut KcpOutput)).output(
                    std::ptr::slice_from_raw_parts(data, len as usize)
                        .as_ref()
                        .unwrap(),
                )
            }
        }

        let output = KcpOutput {
            to: addr,
            manager: self.clone(),
        };

        let kcp = kcp::Kcp::new(
            RemoteConv(conv),
            kcp_config,
            Some((Box::into_raw(Box::new(output)), Self::kcp_cleanup)),
        )?;

        kcp.set_output(kcp_output_cb_impl);

        Ok(KcpCore {
            kcp: SafeKcp::wrap(kcp),
        })
    }

    fn kcp_cleanup(user: *mut c_void) {
        unsafe { drop(Box::from_raw(user as *mut KcpOutput)) }
    }

    fn close_all_session(&self) {
        let mut this = self.inner.lock().unwrap();

        for (_, sessions) in &mut this.sessions {
            for (_, kcp) in sessions {
                kcp.force_close();
            }
        }

        this.sessions.clear();
    }

    fn stop_all_processor(&self) {
        self.kcp_closer.close();
        self.kcp_sender.close();
        self.poller_sig.close();
    }
}

impl KcpOutput {
    fn output(&self, data: &[u8]) -> i32 {
        self.manager
            .kcp_sender
            .block_send((self.to, data.to_vec()))
            .unwrap();
        data.len() as i32
    }
}

impl AsyncRead for ServerImpl {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.kcp.poll_recv(cx, buf)
    }
}

impl AsyncWrite for ServerImpl {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.kcp.poll_send(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.kcp.poll_flush(cx)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.kcp.poll_close(cx)
    }
}

impl Drop for ServerImpl {
    fn drop(&mut self) {
        self.manager.close_kcp(self.id, self.conv)
    }
}

impl<IO> Drop for KcpListener<IO> {
    fn drop(&mut self) {
        self.manager.close_all_session();
        self.manager.stop_all_processor();
    }
}
