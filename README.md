# kcp-rust

# Dependencies
```
[dependencies]
tokio = { version = "*", features = ["full"]}
kcp-rust = { git = "https://github.com/editso/kcp-rust.git", features = ["runtime_tokio"]}
```

# Examples
```
use kcp_rust::{KcpConnector, KcpListener};

#[tokio::main]
async fn main(){
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            tokio::spawn(async move {
                let udp = tokio::net::UdpSocket::bind("127.0.0.1:9999").await.unwrap();

                let kcp_listener =
                    KcpListener::new_with_tokio(udp, Default::default()).unwrap();

                let (conv, addr, mut kcp_stream) = kcp_listener.accept().await.unwrap();

                let mut s = [0u8; 1024];

                println!("connected {conv} {addr}");

                let n = kcp_stream.read(&mut s).await.unwrap();

                println!("message: {}", String::from_utf8(s[..n].to_vec()).unwrap())
            });

            let udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            udp.connect("127.0.0.1:9999").await.unwrap();

            let mut kcp_connector =
                KcpConnector::new_with_tokio(udp, Default::default()).unwrap();
            let (_, mut kcp_stream) = kcp_connector.open().await.unwrap();

            kcp_stream.write(b"hello world").await.unwrap();

            tokio::time::sleep(Duration::from_secs(2)).await;
        })
}
```