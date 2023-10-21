use std::net::ToSocketAddrs;

use crate::KcpStream;

pub struct KcpListener {}

pub struct ServerImpl{

}

impl KcpListener {
    pub fn new() -> Self{
        unimplemented!()
    }

    pub async fn accept(&mut self) -> KcpStream<ServerImpl>{
        unimplemented!()
    }
}

