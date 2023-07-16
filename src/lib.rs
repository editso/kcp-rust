use std::{
    collections::{hash_map::DefaultHasher, HashMap, VecDeque},
    ffi::c_void,
    hash::{Hash, Hasher},
    net::{SocketAddr},
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::{Arc, Mutex, RwLock},
    task::{Context, Poll, Waker}, future::Future,
};

mod r#async;
mod kcp;
mod sync;

use kcp::{ConvAllocator, Kcp};
use r#async::{AsyncRead, AsyncRecvfrom, AsyncSendTo, AsyncWrite};

use sync::Sync;

pub type KcpSessions = HashMap<kcp::CONV_T, Sync<KcpCore>>;

pub type BoxedFuture<'a, O> = Pin<Box<dyn std::future::Future<Output = O> + 'a + Send>>;

#[derive(Default, Clone)]
pub struct KcpWaker(Arc<RwLock<Option<Waker>>>);

pub fn now_mills() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u32
}

#[derive(Clone)]
pub struct KcpManager<T: Runtime + Unpin, S> {
    sessions: KcpSessions,
    sender_daemon: Sync<SenderDaemon<T, S>>,
    refresh_daemon: Sync<RefreshDaemon<T>>,
}

pub struct LazyFuture<O>(Option<BoxedFuture<'static, O>>);

pub struct KcpCore {
    ikcp: kcp::Kcp<DefaultConvAllocator>,
    checkable: bool,
    reader_waker: KcpWaker,
    sender_waker: KcpWaker,
    writer_waker: KcpWaker,
    refresh_waker: KcpWaker,
    pre_check: u32,
    nex_check: u32,
}

#[derive(Debug, Clone)]
pub enum DefaultConvAllocator {
    Allocated(u32),
}

impl ConvAllocator for DefaultConvAllocator {
    fn allocate(&mut self) -> kcp::Result<kcp::CONV_T> {
        match self {
            DefaultConvAllocator::Allocated(conv) => Ok(*conv),
        }
    }

    fn deallocate(&mut self, conv: kcp::CONV_T) {
        unimplemented!()
    }
}

#[derive(Clone)]
pub struct KcpConnector<T: Runtime + Unpin, IO> {
    io: IO,
    manager: KcpManager<T, IO>,
    reactor: Sync<ReceiverDaemon<T, IO>>,
    allocator: DefaultConvAllocator,
}

pub struct KcpStream<S: AsyncSendTo> {
    core: Sync<KcpCore>,
    addr: SocketAddr,
    sender: S,
}

pub enum DaemonState {
    Checking,
    Sleeping(BoxedFuture<'static, ()>),
}

pub struct SenderDaemon<T: Runtime, S> {
    writer: S,
    task: Option<T::Output>,
    runtime: T,
    sessions: HashMap<kcp::CONV_T, Sync<KcpCore>>,
    writer_fut: LazyFuture<std::io::Result<()>>,
    sender_waker: KcpWaker,
    wait_join_que: Mutex<Option<VecDeque<Sync<KcpCore>>>>,
    wait_send_que: Option<VecDeque<(kcp::CONV_T, Vec<u8>)>>,
}

pub struct RefreshDaemon<T: Runtime> {
    task: Option<T::Output>,
    runtime: T,
    state: DaemonState,
    refresh_waker: KcpWaker,
    wait_join_que: Mutex<Option<VecDeque<Sync<KcpCore>>>>,
    wait_check_list: Vec<Sync<KcpCore>>,
}

pub struct ReceiverDaemon<T: Runtime + Unpin, R> {
    task: Option<T::Output>,
    read_fut: LazyFuture<std::io::Result<(SocketAddr, Vec<u8>)>>,
    receiver: R,
    runtime: T,
    sessions: HashMap<u64, HashMap<kcp::CONV_T, Sync<KcpCore>>>,
    manager: KcpManager<T, R>,
}

pub trait KcpIO {}

pub trait Task: Send {
    fn terminal(&self);
}

pub trait Runtime {
    type Output: Task + Unpin;
    fn spawn<F>(&self, poll: F) -> Self::Output
    where
        F: FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()> + Send + 'static;

    fn sleep(&self, mills: u32) -> BoxedFuture<'static, ()>;
}

impl KcpWaker {
    pub fn set(&self, waker: Waker) {
        drop(self.0.write().unwrap().replace(waker))
    }

    pub fn wake(&self) {
        if let Some(waker) = self.0.read().unwrap().as_ref() {
            waker.wake_by_ref();
        }
    }
}

impl<O> LazyFuture<O> {
    fn poll<F>(mut self: Pin<&mut Self>, cx: &mut Context<'_>, f: F) -> std::task::Poll<O>
    where
        F: FnOnce() -> BoxedFuture<'static, O>,
    {
        let mut fut = self.0.take().map_or_else(f, |fut| fut);

        Pin::new(&mut fut).poll(cx)
    }
}

impl<O> Default for LazyFuture<O> {
    fn default() -> Self {
        Self(None)
    }
}

impl<E, IO> KcpConnector<E, IO>
where
    E: Runtime + Unpin + Clone + Send + 'static,
    IO: AsyncSendTo + AsyncRecvfrom + Unpin + Clone + Send + 'static,
{
    pub fn new(executor: E, io: IO) -> Self {
        let manager = KcpManager::start(executor.clone(), io.clone());
        Self {
            io: io.clone(),
            manager: manager.clone(),
            reactor: ReceiverDaemon::start(executor, manager, io),
            allocator: DefaultConvAllocator::Allocated(0),
        }
    }

    async fn connect<A>(&mut self, addr: A) -> kcp::Result<KcpStream<IO>>
    where
        A: Into<SocketAddr>,
    {
        let kcp = self.manager.make_kcp(self.allocator.clone())?;
        Ok(KcpStream {
            core: kcp,
            addr: addr.into(),
            sender: self.io.clone(),
        })
    }
}

impl<E, S> KcpManager<E, S>
where
    E: Runtime + Unpin + Clone + Send + 'static,
    S: AsyncRecvfrom + AsyncSendTo + Unpin + Clone + Send + 'static,
{
    fn start(executor: E, s: S) -> Self {
        Self {
            sessions: KcpSessions::default(),
            sender_daemon: SenderDaemon::start(executor.clone(), s),
            refresh_daemon: RefreshDaemon::start(executor.clone()),
        }
    }

    fn drop_boxed_manager(ptr: *mut std::ffi::c_void) {
        println!("Clean ...");
        drop(unsafe { Box::from_raw(ptr as *mut Self) })
    }

    fn manage(&self, kcp: Sync<KcpCore>) {
        self.refresh_daemon
            .lock_mut(|refresh| refresh.join(kcp.clone()));

        self.sender_daemon.lock_mut(|sender| sender.join(kcp))
    }

    fn make_kcp(&self, allocator: DefaultConvAllocator) -> kcp::Result<Sync<KcpCore>> {
        extern "C" fn kcp_output<E, S>(
            buf: *const u8,
            len: i32,
            kcp: kcp::CB,
            user: *mut c_void,
        ) -> i32
        where
            E: Runtime + Unpin + Clone + Send + 'static,
            S: AsyncSendTo + Send + Clone + Unpin + 'static,
        {
            #[repr(C)]
            struct ffi_kcp {
                conv: kcp::CONV_T,
            }

            unsafe {
                let kcp = (kcp as *mut ffi_kcp).as_ref().unwrap_unchecked();
                let data = std::slice::from_raw_parts(buf, len as usize);
                Box::leak(Box::from_raw(user as *mut KcpManager<E, S>))
                    .sender_daemon
                    .lock_mut(|sender| sender.do_send(kcp.conv, data));
            };

            len
        }

        let manager = Box::into_raw(Box::new(self.clone()));

        let kcp = kcp::Kcp::new(allocator, Some((manager, Self::drop_boxed_manager)))?;

        kcp.set_output(kcp_output::<E, S>);

        let kcp = Sync::new(KcpCore {
            ikcp: kcp,
            checkable: false,
            pre_check: now_mills(),
            nex_check: now_mills(),
            reader_waker: Default::default(),
            writer_waker: Default::default(),
            sender_waker: self.sender_daemon.lock(|sender| sender.waker()),
            refresh_waker: self.refresh_daemon.lock(|refresh| refresh.waker()),
        });

        self.manage(kcp.clone());

        Ok(kcp)
    }
}

impl<T, S> ReceiverDaemon<T, S>
where
    T: Runtime + Unpin + Clone + Send + 'static,
    S: AsyncRecvfrom + AsyncSendTo + Clone + Unpin + Send + 'static,
{
    fn start(executor: T, manager: KcpManager<T, S>, receiver: S) -> Sync<Self> {
        Sync::new(Self {
            task: None,
            manager,
            receiver,
            read_fut: Default::default(),
            runtime: executor.clone(),
            sessions: Default::default(),
        })
        .chain_clone_mut(|receiver, this| {
            this.task = Some(executor.spawn(move |ctx| loop {
                match receiver.lock_mut(|this| Pin::new(this).poll(ctx)) {
                    std::task::Poll::Ready(_) => continue,
                    std::task::Poll::Pending => break std::task::Poll::Pending,
                }
            }))
        })
    }
}

impl<T> RefreshDaemon<T>
where
    T: Runtime + Clone + Unpin + Send + 'static,
{
    fn start(executor: T) -> Sync<Self> {
        Sync::new(Self {
            task: None,
            runtime: executor.clone(),
            state: DaemonState::Checking,
            refresh_waker: Default::default(),
            wait_join_que: Default::default(),
            wait_check_list: Default::default(),
        })
        .chain_clone_mut(|refresh, this| {
            this.task = Some(executor.spawn(move |ctx| loop {
                match refresh.lock_mut(|this| Pin::new(this).poll(ctx)) {
                    std::task::Poll::Ready(_) => continue,
                    std::task::Poll::Pending => break std::task::Poll::Pending,
                }
            }))
        })
    }

    fn join(&mut self, kcp: Sync<KcpCore>) {
        let mut wait_join_que = self.wait_join_que.lock().unwrap();

        match wait_join_que.deref_mut() {
            Some(join_que) => join_que.push_back(kcp.clone()),
            None => drop(std::mem::replace(
                wait_join_que.deref_mut(),
                Some({
                    let mut join_que = VecDeque::new();
                    join_que.push_back(kcp.clone());
                    join_que
                }),
            )),
        }

        self.refresh_waker.wake();
    }

    fn waker(&self) -> KcpWaker {
        self.refresh_waker.clone()
    }
}

impl<T, S> SenderDaemon<T, S>
where
    T: Runtime + Clone + Unpin + Send + 'static,
    S: AsyncSendTo + Clone + Unpin + Send + 'static,
{
    fn start(runtime: T, writer: S) -> Sync<Self> {
        Sync::new(Self {
            writer,
            task: None,
            runtime: runtime.clone(),
            sessions: Default::default(),
            writer_fut: Default::default(),
            sender_waker: Default::default(),
            wait_join_que: Default::default(),
            wait_send_que: Default::default(),
        })
        .chain_clone_mut(|sender, this| {
            this.task = Some(runtime.spawn(move |ctx| loop {
                match sender.lock_mut(|this| Pin::new(this).poll(ctx)) {
                    std::task::Poll::Ready(_) => continue,
                    std::task::Poll::Pending => break std::task::Poll::Pending,
                };
            }))
        })
    }

    fn do_send(&mut self, conv: kcp::CONV_T, data: &[u8]) {
        match &mut self.wait_send_que {
            Some(snd_que) => snd_que.push_back((conv, data.to_vec())),
            None => drop(std::mem::replace(
                &mut self.wait_send_que,
                Some({
                    let mut wait_snd = VecDeque::new();
                    wait_snd.push_back((conv, data.to_vec()));
                    wait_snd
                }),
            )),
        }

        self.sender_waker.wake();
    }

    fn waker(&self) -> KcpWaker {
        self.sender_waker.clone()
    }

    fn join(&mut self, kcp: Sync<KcpCore>) {}
}

impl std::ops::Deref for KcpCore {
    type Target = kcp::Kcp<DefaultConvAllocator>;

    fn deref(&self) -> &Self::Target {
        &self.ikcp
    }
}

impl KcpCore {
    fn wake_all(&self) {
        self.sender_waker.wake();
        self.reader_waker.wake();
        self.refresh_waker.wake();
        self.writer_waker.wake();
    }
}

impl<T> RefreshDaemon<T>
where
    T: Runtime,
{
    fn refresh(&mut self) {
        let join_que = self.wait_join_que.lock().unwrap().take();

        if join_que.is_none() {
            return;
        }

        let mut join_que = unsafe { join_que.unwrap_unchecked() };
        while let Some(kcp) = join_que.pop_front() {
            self.wait_check_list.push(kcp);
        }

        self.state = DaemonState::Checking;
    }
}

impl<T, S> SenderDaemon<T, S>
where
    T: Runtime,
{
    fn do_sendto(&self, data: VecDeque<(kcp::CONV_T, Vec<u8>)>) {
        for (_, data) in data {
            println!("{:?}", String::from_utf8_lossy(&data));
        }
    }
}

impl DaemonState {
    fn poll(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        match self {
            DaemonState::Checking => std::task::Poll::Ready(()),
            DaemonState::Sleeping(fut) => match Pin::new(fut).poll(cx) {
                std::task::Poll::Pending => std::task::Poll::Pending,
                _ => std::task::Poll::Ready(()),
            },
        }
    }
}

impl<T> Future for RefreshDaemon<T>
where
    T: Runtime + Unpin,
{
    type Output = ();

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        loop {
            self.refresh();

            match self.state.poll(cx) {
                std::task::Poll::Ready(()) => {}
                std::task::Poll::Pending => {
                    self.refresh_waker.set(cx.waker().clone());
                    break std::task::Poll::Pending;
                }
            }

            let mut is_first_flag = true;
            let mut check_count = 0;
            let mut next_wake_time_mss = 0;

            for kcp in &mut self.wait_check_list {
                kcp.lock_mut(|kcp| {
                    if !kcp.checkable {
                        kcp.update(now_mills());
                        return;
                    }

                    check_count += 1;

                    let now_mills = now_mills();

                    let nex_update = kcp.check(now_mills);
                    kcp.nex_check = nex_update;

                    if kcp.nex_check <= now_mills {
                        kcp.update(now_mills);
                    }

                    let nex_update = nex_update - now_mills;

                    next_wake_time_mss = if is_first_flag {
                        is_first_flag = false;
                        nex_update
                    } else {
                        nex_update.min(next_wake_time_mss)
                    };
                });
            }

            if check_count == 0 {
                self.refresh_waker.set(cx.waker().clone());
                break std::task::Poll::Pending;
            }

            if next_wake_time_mss == 0 {
                println!("checking .. {}", now_mills());
                self.state = DaemonState::Checking;
                continue;
            }

            let fut: Pin<Box<dyn Future<Output = ()> + Send>> = self.runtime.sleep(next_wake_time_mss);

            self.state = DaemonState::Sleeping(fut);
        }
    }
}

impl<T, S> Future for SenderDaemon<T, S>
where
    T: Runtime + Unpin,
    S: AsyncSendTo + Clone + Send + Unpin + 'static,
{
    type Output = ();
    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        loop {
            let poll = if self.writer_fut.0.is_some() {
                Pin::new(&mut self.writer_fut).poll(cx, || Box::pin(async move { Ok(()) }))
            } else {
                let mut writer = self.writer.clone();
                let wait_snd_que = self.wait_send_que.take();
                Pin::new(&mut self.writer_fut).poll(cx, move|| {
                    Box::pin(async move {
                        if let Some(mut queue) = wait_snd_que {
                            while let Some((conv, data)) = queue.pop_front() {
                                match writer.send_to(&([0, 0, 0, 0], 0u16).into(), data).await {
                                    Ok(e) => {}
                                    Err(_) => {}
                                }
                            }
                        }

                        Ok(())
                    })
                })
            };

            match poll {
                Poll::Ready(_) => {
                    if self.wait_send_que.is_some() {
                        continue;
                    }
                }
                _ => {}
            }


            self.sender_waker.set(cx.waker().clone());
            break std::task::Poll::Pending;
        }
    }
}

impl<T, R> Future for ReceiverDaemon<T, R>
where
    T: Runtime + Unpin + Clone + Send + 'static,
    R: AsyncRecvfrom + AsyncSendTo + Unpin + Clone + Send + 'static,
{
    type Output = ();
    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        loop {
            let mut receiver = self.receiver.clone();
            let poll = Pin::new(&mut self.read_fut).poll(cx, || {
                receiver.recv_from({
                    let mut buf = Vec::new();
                    buf.resize(1024, 0);
                    buf
                })
            });

            match poll {
                std::task::Poll::Pending => break std::task::Poll::Pending,
                std::task::Poll::Ready(Err(e)) => {
                    println!("error ...");
                    break std::task::Poll::Ready(());
                }
                std::task::Poll::Ready(Ok((addr, buf))) => {
                    let conv = Kcp::<DefaultConvAllocator>::get_conv(&buf);
                    let mut hasher = DefaultHasher::new();
                    addr.hash(&mut hasher);
                    let hash = hasher.finish();
                    let is_new = self.sessions.entry(hash).or_default().contains_key(&conv);
                    if !is_new {
                        let kcp = self.manager.make_kcp(DefaultConvAllocator::Allocated(conv));
                        let kcp = kcp.unwrap();
                        unsafe {
                            drop({
                                self.sessions
                                    .get_mut(&hash)
                                    .unwrap_unchecked()
                                    .insert(conv, kcp)
                            });
                        }
                    };

                    let kcp = unsafe {
                        self.sessions
                            .get_mut(&hash)
                            .unwrap_unchecked()
                            .get_mut(&conv)
                            .unwrap_unchecked()
                    };

                    kcp.lock(|kcp| {
                        match kcp.input(&buf) {
                            Ok(()) => kcp.update(now_mills()),
                            Err(e) => {
                                println!("{:?}", e);
                            }
                        }

                        kcp.wake_all();
                    });
                }
            };
        }
    }
}

impl<W> AsyncRead for KcpStream<W>
where
    W: AsyncSendTo + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        unimplemented!()
    }
}

impl<W> AsyncWrite for KcpStream<W>
where
    W: AsyncSendTo + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.core.lock_mut(|kcp| {
            if kcp.waitsnd() > 0 {
                kcp.writer_waker.set(cx.waker().clone());
                Poll::Pending
            } else {
                println!("{}", kcp.waitsnd());
                kcp.send(buf);
                kcp.nex_check = now_mills();
                kcp.update(kcp.nex_check);
                if !kcp.checkable {
                    kcp.checkable = true;
                    kcp.refresh_waker.wake();
                }
                std::task::Poll::Ready(Ok(buf.len()))
            }
        })
    }
}

impl<O> LazyFuture<O> {}

impl<S> Drop for KcpStream<S>
where
    S: AsyncSendTo,
{
    fn drop(&mut self) {}
}

impl<R, S> Drop for SenderDaemon<R, S>
where
    R: Runtime,
{
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.terminal();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

    use smol::future::FutureExt;

    use crate::{
        kcp, now_mills,
        r#async::{AsyncRead, AsyncReadExt, AsyncRecvfrom, AsyncSendTo, AsyncWrite, AsyncWriteExt},
        sync::Sync,
        BoxedFuture, KcpConnector, LazyFuture, Runtime, Task,
    };

    #[derive(Clone)]
    struct SmolSocket {
        sock: smol::net::UdpSocket,
        c: Sync<BoxedFuture<'static, ()>>,
        lazy: Option<Sync<BoxedFuture<'static, ()>>>, // read_fut: LazyFuture<std::io::Result<(std::net::SocketAddr, usize)>>,
                                                      // write_fut: LazyFuture<std::io::Result<usize>>,
    }

    impl AsyncRecvfrom for SmolSocket {
        fn recv_from(
            &mut self,
            mut buf: Vec<u8>,
        ) -> Pin<
            Box<
                dyn Future<Output = std::io::Result<(std::net::SocketAddr, Vec<u8>)>>
                    + Send
                    + 'static,
            >,
        > {
            let sock = self.sock.clone();
            Box::pin(async move {
                let (n, addr) = sock.recv_from(&mut buf).await?;
                buf.truncate(n);
                Ok((addr, buf))
            })
        }
    }

    impl AsyncSendTo for SmolSocket {
        fn send_to(
            &mut self,
            addr: &std::net::SocketAddr,
            buf: Vec<u8>,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + 'static>> {
            let addr = addr.clone();
            let sock = self.sock.clone();
            Box::pin(async move{
                println!("send");
                sock.send_to(&buf, &addr).await;
                Ok(())
            })
        }
    }

    #[derive(Clone)]
    pub struct SmolRuntime;

    #[derive(Clone)]
    pub struct SimplTask {}

    impl Runtime for SmolRuntime {
        type Output = SimplTask;
        fn spawn<F>(&self, f: F) -> Self::Output
        where
            F: FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()> + Send + 'static,
        {
            let task = smol::spawn(async move {
                println!("poll ...");
                smol::future::poll_fn(f).await;
                println!("poll2 ...");
            })
            .detach();

            SimplTask {}
        }

        fn sleep(&self, mills: u32) -> crate::BoxedFuture<'static, ()> {
            Box::pin(async move {
                smol::Timer::after(Duration::from_millis(mills as u64)).await;
            })
        }
    }

    impl Task for SimplTask {
        fn terminal(&self) {
            unimplemented!()
        }
    }

    #[test]
    fn test() -> kcp::Result<()> {
       
        smol::block_on(async move {
            let server = smol::net::UdpSocket::bind("0.0.0.0:9999").await.unwrap();
            let s = SmolSocket {
                sock: server,
                c: Sync::new(Box::pin(async {})),
                lazy: None,
            };

            let mut connector = KcpConnector::new(SmolRuntime, s);
            match connector.connect(([0, 0, 0, 0], 0)).await {
                Err(_) => unimplemented!(),
                Ok(mut stream) => {
                    println!("Okay ...");

                    loop {
                        let a = stream
                            .write(format!("hello {}", now_mills()).as_bytes())
                            .await;
                        println!("sending ...");

                        smol::Timer::after(Duration::from_secs(2)).await;
                    }
                }
            };
        });

        Ok(())
    }
}
