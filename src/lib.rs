mod r#async;
mod client;
mod kcp;
mod server;
mod sync;
mod queue;

mod poller;

use std::pin::Pin;

use r#async::{AsyncRead, AsyncSend, AsyncWrite};

pub struct KcpStream<T>(T);

impl<T> AsyncRead for KcpStream<T>
where
    T: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<T> AsyncWrite for KcpStream<T>
where
    T: AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
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

#[cfg(test)]
mod tests {
    use std::pin::Pin;

    use crate::r#async::{AsyncReadExt, AsyncRecv, AsyncSend, AsyncSendTo};
    use crate::{client::KcpConnector, server::KcpListener};

    impl AsyncSend for smol::net::UdpSocket {
        fn poll_send(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            // let a = self.send(buf);
            unimplemented!()
        }
    }

    impl AsyncRecv for smol::net::UdpSocket {
        fn poll_recv(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            unimplemented!()
        }
    }

    impl AsyncSendTo for smol::net::UdpSocket {
        fn poll_sendto(
            &mut self,
            addr: &std::net::SocketAddr,
            buf: Vec<u8>,
        ) -> std::task::Poll<std::io::Result<usize>> {
            unimplemented!()
        }
    }

    #[test]
    fn test_kcp_client() -> std::io::Result<()> {
        smol::block_on(async move {
            let udp = smol::net::UdpSocket::bind("").await?;

            udp.connect("").await?;

            let mut kcp_connector = KcpConnector::new(udp);

            let kcp = kcp_connector.open().await.unwrap();

            Ok(())
        })
    }

    #[test]
    fn test_kcp_server() {
        smol::block_on(async {
            let mut kcp_server = KcpListener::new();

            loop {
                // let mut stream = kcp_server.accept().await.unwrap();
            }
        })
    }
}
