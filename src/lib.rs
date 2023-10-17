mod r#async;
mod client;
mod kcp;
mod server;
mod sync;

use std::pin::Pin;

use r#async::{AsyncRead, AsyncWrite};

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
}

#[cfg(test)]
mod tests {
    use crate::r#async::AsyncReadExt;
    use crate::{client::KcpConnector, server::KcpListener};

    #[test]
    fn test_kcp_client() {
        smol::block_on(async move {
            let kcp_connector = KcpConnector::new();
            let stream = kcp_connector.open().await.unwrap();
        })
    }

    #[test]
    fn test_kcp_server() {
        smol::block_on(async {
            let kcp_server = KcpListener::new();

            loop {
                let mut stream = kcp_server.accept().await.unwrap();
            }
        })
    }
}
