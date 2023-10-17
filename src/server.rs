use std::net::ToSocketAddrs;

use crate::KcpStream;

pub struct KcpListener {}

pub struct ServerImpl{

}

impl KcpListener {
    pub fn new() -> KcpListener {
        unimplemented!()
    }

    pub async fn accept(&mut self) -> std::io::Result<KcpStream<ServerImpl>>{
        unimplemented!()
    }
}

