use std::future::Future;
use std::pin::Pin;

use crate::r#async::{AsyncRead, AsyncRecv, AsyncSend, AsyncWrite};
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

struct KcpCore(CoreImpl);

struct Processor {}

#[derive(Clone)]
struct KcpManager {}

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
    IO: Clone + AsyncSend + AsyncRecv,
{
    pub fn new(io: IO) -> KcpConnector<IO> {
        let manager = KcpManager {};

        let processors = vec![
            Self::start_process_write(manager.clone()),
            Self::start_process_read(manager.clone()),
            Self::start_process_update(manager.clone()),
        ];

        KcpConnector {
            io,
            manager,
            allocate: KcpConv {},
            processors,
        }
    }

    pub async fn open(&mut self) -> std::io::Result<KcpStream<ClientImpl<IO>>> {
        let a = kcp::Kcp::new::<usize>(self.allocate.clone(), None).unwrap();

        unimplemented!()
    }

    fn start_process_write(manager: KcpManager) -> Processor {
        unimplemented!()
    }

    fn start_process_read(manager: KcpManager) -> Processor {
        unimplemented!()
    }

    fn start_process_update(manager: KcpManager) -> Processor {
        unimplemented!()
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
