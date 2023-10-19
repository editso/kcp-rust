use std::net::ToSocketAddrs;

use crate::KcpStream;

pub struct KcpListener {}

pub struct ServerImpl{

}

impl KcpListener {
    pub async fn new() {

    }

    pub async fn accept(&mut self) -> KcpStream<ServerImpl>{
        unimplemented!()
    }
}

